use std::mem::size_of;
use std::sync::Once;

use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Animation::{
    IUIAnimationManager2, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionLibrary2, IUIAnimationVariable2, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE,
    UIAnimationManager2, UIAnimationTimer,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

pub struct MouseEvent {
    pub on_change: Box<dyn Fn(&HWND, bool)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_change: Box::new(|_window, _checked| {}),
        }
    }
}

pub struct Props {
    pub label: PCWSTR,
    pub checked: bool,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            label: w!(""),
            checked: false,
            mouse_event: MouseEvent::default(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    /// Fluent Switch track: 40 x 20, pill-shaped.
    fn track_width(&self) -> f32 {
        40.0
    }
    fn track_height(&self) -> f32 {
        20.0
    }
    /// Gap between thumb and track edge — Fluent's `spaceBetweenThumbAndTrack`.
    fn thumb_inset(&self) -> f32 {
        2.0
    }
    fn thumb_radius(&self) -> f32 {
        // Fluent's track is border-box, so its 1px border eats into the 20px height
        // (18px interior), and the thumb is a CircleFilled glyph whose visible
        // circle is ~14px, not the full 18px em-box. We fill an ellipse geometrically
        // (no glyph padding), so to match the visible result the radius is the track
        // radius minus the border and the thumb-to-track gap: 10 − 1 − 2 = 7 (⌀14).
        self.track_height() / 2.0 - self.qt.theme.tokens.stroke_width_thin - self.thumb_inset()
    }

    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base300
    }
    fn line_height(&self) -> f32 {
        self.qt.theme.tokens.line_height_base300
    }

    /// Padding around the track and trailing the label — `spacingHorizontalS`.
    fn pad(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_s
    }
    /// Space between the track's padding and the label text — `spacingHorizontalXS`.
    fn gap(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_xs
    }
    /// Vertical padding above and below the track — `spacingVerticalS`.
    fn vpad(&self) -> f32 {
        self.qt.theme.tokens.spacing_vertical_s
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    /// Eased thumb position in [0,1]: 0 = off (left), 1 = on (right). Also drives
    /// the track/thumb colour cross-fade in paint.
    thumb_position: IUIAnimationVariable2,
    checked: bool,
    hovered: bool,
    pressed: bool,
}

impl QT {
    pub fn create_switch(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_SWITCH");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_CLASSDC,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let boxed = Box::new(State {
                qt: self.clone(),
                props,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                0,
                0,
                Some(parent_window),
                None,
                Some(HINSTANCE(
                    GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _
                )),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }

    /// Current checked state of a switch created by `create_switch`.
    pub fn switch_checked(&self, switch: HWND) -> bool {
        unsafe {
            let raw = GetWindowLongPtrW(switch, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                false
            } else {
                (*raw).checked
            }
        }
    }
}

#[implement(IUIAnimationTimerEventHandler)]
struct AnimationTimerEventHandler {
    window: HWND,
}

impl IUIAnimationTimerEventHandler_Impl for AnimationTimerEventHandler_Impl {
    fn OnPreUpdate(&self) -> Result<()> {
        Ok(())
    }

    fn OnPostUpdate(&self) -> Result<()> {
        unsafe {
            _ = InvalidateRect(Some(self.window), None, false);
        }
        Ok(())
    }

    fn OnRenderingTooSlow(&self, _frames_per_second: u32) -> Result<()> {
        Ok(())
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    let checked = state.props.checked;
    unsafe {
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            state.font_size(),
            w!(""),
        )?;
        text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;

        let dpi = GetDpiForWindow(window);
        let render_target = state.qt.d2d_factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U {
                    width: 0,
                    height: 0,
                },
                presentOptions: Default::default(),
            },
        )?;

        // Windows Animation Manager: let the system interpolate the thumb slide.
        // Same wiring as button.rs.
        let animation_timer: IUIAnimationTimer =
            CoCreateInstance(&UIAnimationTimer, None, CLSCTX_INPROC_SERVER)?;
        let transition_library = state.qt.transition_library.clone();
        let animation_manager: IUIAnimationManager2 =
            CoCreateInstance(&UIAnimationManager2, None, CLSCTX_INPROC_SERVER)?;
        let timer_update_handler = animation_manager.cast::<IUIAnimationTimerUpdateHandler>()?;
        animation_timer
            .SetTimerUpdateHandler(&timer_update_handler, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE)?;
        let timer_event_handler: IUIAnimationTimerEventHandler =
            AnimationTimerEventHandler { window }.into();
        animation_timer.SetTimerEventHandler(&timer_event_handler)?;
        let thumb_position =
            animation_manager.CreateAnimationVariable(if checked { 1.0 } else { 0.0 })?;

        Ok(Context {
            state,
            text_format,
            render_target,
            animation_manager,
            animation_timer,
            transition_library,
            thumb_position,
            checked,
            hovered: false,
            pressed: false,
        })
    }
}

/// Auto-size to the track + gap + label and resize the render target.
fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    unsafe {
        let text_layout = state.qt.dwrite_factory.CreateTextLayout(
            state.props.label.as_wide(),
            &context.text_format,
            f32::MAX,
            f32::MAX,
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        text_layout.GetMetrics(&mut metrics)?;

        let scaling_factor = get_scaling_factor(window);
        let has_label = !state.props.label.is_null() && !state.props.label.as_wide().is_empty();
        // `spacingHorizontalS` padding around the track on every side; the label
        // adds `spacingHorizontalXS` before its text and `spacingHorizontalS` after.
        let width = if has_label {
            state.pad() + state.track_width() + state.pad() + state.gap() + metrics.width.ceil()
                + state.pad()
        } else {
            state.pad() + state.track_width() + state.pad()
        };
        // Fluent 36px row: `spacingVerticalS` above and below the 20px track.
        let height = (state.track_height() + state.vpad() * 2.0).max(state.line_height());
        let scaled_width = (width * scaling_factor).ceil() as i32;
        let scaled_height = (height * scaling_factor).ceil() as i32;

        SetWindowPos(
            window,
            None,
            0,
            0,
            scaled_width,
            scaled_height,
            SWP_NOMOVE | SWP_NOZORDER,
        )?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;
    }
    Ok(())
}

/// Same colour with a replaced alpha (for cross-fading the track fill in/out).
fn with_alpha(color: &D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: color.r,
        g: color.g,
        b: color.b,
        a,
    }
}

/// Linear blend between two colours (channel-wise).
fn lerp_color(from: &D2D1_COLOR_F, to: &D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: from.a + (to.a - from.a) * t,
    }
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let background = state
            .props
            .background
            .unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let height = rc.bottom as f32 / scaling_factor;

        let p = context.thumb_position.GetValue()? as f32;

        let track_w = state.track_width();
        let track_h = state.track_height();
        let track_left = state.pad();
        let track_top = (height - track_h) / 2.0;
        let radius = track_h / 2.0;

        // Endpoint colours by hover/press. Unchecked → neutral stroke; checked →
        // compound-brand fill + white thumb. Cross-faded by `p`.
        let unchecked_stroke = if context.pressed {
            &tokens.color_neutral_stroke_accessible_pressed
        } else if context.hovered {
            &tokens.color_neutral_stroke_accessible_hover
        } else {
            &tokens.color_neutral_stroke_accessible
        };
        let checked_fill = if context.pressed {
            &tokens.color_compound_brand_background_pressed
        } else if context.hovered {
            &tokens.color_compound_brand_background_hover
        } else {
            &tokens.color_compound_brand_background
        };
        let checked_thumb = &tokens.color_neutral_foreground_on_brand;

        let track_rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: track_left,
                top: track_top,
                right: track_left + track_w,
                bottom: track_top + track_h,
            },
            radiusX: radius,
            radiusY: radius,
        };

        // Fill: brand colour fading in with `p` (unchecked track has no fill).
        let fill_brush = context
            .render_target
            .CreateSolidColorBrush(&with_alpha(checked_fill, p), None)?;
        context
            .render_target
            .FillRoundedRectangle(&track_rect, &fill_brush);

        // Border: neutral stroke fading out as the fill takes over.
        let border_brush = context
            .render_target
            .CreateSolidColorBrush(&with_alpha(unchecked_stroke, 1.0 - p), None)?;
        let inset = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: track_left + tokens.stroke_width_thin * 0.5,
                top: track_top + tokens.stroke_width_thin * 0.5,
                right: track_left + track_w - tokens.stroke_width_thin * 0.5,
                bottom: track_top + track_h - tokens.stroke_width_thin * 0.5,
            },
            radiusX: radius,
            radiusY: radius,
        };
        context.render_target.DrawRoundedRectangle(
            &inset,
            &border_brush,
            tokens.stroke_width_thin,
            &state.qt.stroke_style,
        );

        // Thumb: slides from the left cap centre to the right cap centre, colour
        // lerping neutral-stroke → white.
        let off_x = track_left + radius;
        let on_x = track_left + track_w - radius;
        let thumb_x = off_x + (on_x - off_x) * p;
        let thumb_color = lerp_color(unchecked_stroke, checked_thumb, p);
        let thumb_brush = context
            .render_target
            .CreateSolidColorBrush(&thumb_color, None)?;
        let thumb = D2D1_ELLIPSE {
            point: Vector2 {
                X: thumb_x,
                Y: track_top + track_h / 2.0,
            },
            radiusX: state.thumb_radius(),
            radiusY: state.thumb_radius(),
        };
        context.render_target.FillEllipse(&thumb, &thumb_brush);

        // Label to the right of the track, vertically centred.
        let has_label = !state.props.label.is_null() && !state.props.label.as_wide().is_empty();
        if has_label {
            let text_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
            context.render_target.DrawText(
                state.props.label.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: track_left + track_w + state.pad() + state.gap(),
                    top: 0.0,
                    right: rc.right as f32 / scaling_factor,
                    bottom: height,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
    Ok(())
}

fn on_paint(window: HWND, context: &Context) -> Result<()> {
    unsafe {
        context.render_target.BeginDraw();
        let result = paint(window, context);
        match result {
            Ok(_) => context.render_target.EndDraw(None, None),
            Err(_) => {
                context.render_target.EndDraw(None, None)?;
                result
            }
        }
    }
}

/// Schedule the thumb slide to `target` (0 or 1), letting the Windows Animation
/// Manager interpolate the eased curve — `durationNormal` + `curveEasyEase`, per
/// Fluent. Retargets smoothly if a slide is already in flight.
fn animate_to(context: &Context, target: f64) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    unsafe {
        let storyboard = context.animation_manager.CreateStoryboard()?;
        let transition = context.transition_library.CreateCubicBezierLinearTransition(
            tokens.duration_normal,
            target,
            tokens.curve_easy_ease[0],
            tokens.curve_easy_ease[1],
            tokens.curve_easy_ease[2],
            tokens.curve_easy_ease[3],
        )?;
        storyboard.AddTransition(&context.thumb_position, &transition)?;
        let seconds_now = context.animation_timer.GetTime()?;
        storyboard.Schedule(seconds_now, None)?;
    }
    Ok(())
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut State;
            let state = Box::<State>::from_raw(raw);
            match on_create(window, *state) {
                Ok(context) => {
                    _ = layout(window, &context);
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = on_paint(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = on_paint(window, context);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if !context.hovered {
                context.hovered = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                _ = TrackMouseEvent(&mut tme);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.hovered = false;
            context.pressed = false;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            // Take focus like a native toggle so the previously-focused control
            // (input/dropdown) loses focus.
            _ = SetFocus(Some(window));
            context.pressed = true;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.pressed = false;
            context.checked = !context.checked;
            _ = animate_to(context, if context.checked { 1.0 } else { 0.0 });
            (context.state.props.mouse_event.on_change)(&window, context.checked);
            LRESULT(0)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
