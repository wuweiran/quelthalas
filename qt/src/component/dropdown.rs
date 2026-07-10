use std::mem::size_of;
use std::sync::Once;

use crate::component::input;
use crate::icon::Icon;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_SVG_PAINT_TYPE_COLOR, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1DeviceContext5,
    ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1PathGeometry1, ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromWindow, PAINTSTRUCT, RDW_INVALIDATE, RedrawWindow, SetWindowRgn,
    UpdateWindow,
};
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
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY,
    VK_DOWN, VK_END, VK_ESCAPE, VK_F4, VK_HOME, VK_RETURN, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

const FIELD_CLASS: PCWSTR = w!("QT_DROPDOWN");
const POPUP_CLASS: PCWSTR = w!("QT_DROPDOWN_LISTBOX");
/// Height of each option row (Fluent Option: lineHeightBase300 20 + 2×6 padding).
const ITEM_HEIGHT: f32 = 32.0;
/// Checkmark glyph size (Fluent Option checkIcon: fontSizeBase400).
const CHECK_SIZE: f32 = 16.0;
/// Padding the Listbox puts around all options — the margin around the rows.
const LIST_PADDING: f32 = 4.0;

pub struct MouseEvent {
    pub on_select: Box<dyn Fn(&HWND, usize)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_select: Box::new(|_window, _index| {}),
        }
    }
}

use crate::component::option::Item;

pub struct Props {
    /// Options shown in the list. The caller keeps the strings alive (same
    /// contract as checkbox/radio labels).
    pub options: Vec<Item>,
    /// Initially selected option, or `None` to show the placeholder.
    pub selected_index: Option<usize>,
    pub placeholder: PCWSTR,
    pub size: input::Size,
    pub appearance: input::Appearance,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            options: Vec::new(),
            selected_index: None,
            placeholder: w!(""),
            size: input::Size::Medium,
            appearance: input::Appearance::Outline,
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
    fn field_height(&self) -> f32 {
        match self.size() {
            input::Size::Small => 24.0,
            input::Size::Medium => 32.0,
            input::Size::Large => 40.0,
        }
    }
    fn size(&self) -> input::Size {
        self.props.size
    }
    fn horizontal_padding(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.props.size {
            input::Size::Small => tokens.spacing_horizontal_s,
            input::Size::Medium => tokens.spacing_horizontal_m,
            input::Size::Large => tokens.spacing_horizontal_m + tokens.spacing_horizontal_s_nudge,
        }
    }
    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base300
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    chevron_svg: ID2D1SvgDocument,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    /// Brand underline growth 0..1, animated on focus (like input's).
    bottom_focus_border: IUIAnimationVariable2,
    selected_index: Option<usize>,
    is_focused: bool,
    is_hovered: bool,
}

impl QT {
    pub fn create_dropdown(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let field_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: FIELD_CLASS,
                    style: CS_CLASSDC,
                    lpfnWndProc: Some(field_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&field_class);
                let popup_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: POPUP_CLASS,
                    style: CS_DROPSHADOW | CS_SAVEBITS,
                    lpfnWndProc: Some(popup_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&popup_class);
            });
            let boxed = Box::new(State {
                qt: self.clone(),
                props,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                FIELD_CLASS,
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

    /// Currently selected option index of a dropdown created by `create_dropdown`.
    pub fn dropdown_selected(&self, dropdown: HWND) -> Option<usize> {
        unsafe {
            let raw = GetWindowLongPtrW(dropdown, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                None
            } else {
                (*raw).selected_index
            }
        }
    }
}

fn set_svg_color(svg: &ID2D1SvgDocument, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let svg_paint = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(color), w!(""))?;
        svg.GetRoot()?
            .GetFirstChild()?
            .SetAttributeValue(w!("fill"), &svg_paint.cast::<ID2D1SvgAttribute>()?)?;
    }
    Ok(())
}

fn create_text_format(qt: &QT, font_size: f32) -> Result<IDWriteTextFormat> {
    let tokens = &qt.theme.tokens;
    unsafe {
        let format = qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            font_size,
            w!(""),
        )?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }
}

fn measure_text_width(qt: &QT, format: &IDWriteTextFormat, text: PCWSTR) -> f32 {
    unsafe {
        let Ok(layout) =
            qt.dwrite_factory
                .CreateTextLayout(text.as_wide(), format, f32::MAX, f32::MAX)
        else {
            return 0.0;
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut metrics).is_ok() {
            metrics.width.ceil()
        } else {
            0.0
        }
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    let selected_index = state.props.selected_index;
    unsafe {
        let text_format = create_text_format(&state.qt, state.font_size())?;

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

        let icon = Icon::chevron_down_20_regular();
        let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
        let svg_stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
        let chevron_svg = device_context5.CreateSvgDocument(
            svg_stream.as_ref(),
            D2D_SIZE_F {
                width: icon.size as f32,
                height: icon.size as f32,
            },
        )?;
        _ = set_svg_color(&chevron_svg, &tokens.color_neutral_stroke_accessible);

        // Windows Animation Manager for the focus underline (input's pattern).
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
        let bottom_focus_border = animation_manager.CreateAnimationVariable(0.0)?;

        Ok(Context {
            state,
            text_format,
            render_target,
            chevron_svg,
            animation_manager,
            animation_timer,
            transition_library,
            bottom_focus_border,
            selected_index,
            is_focused: false,
            is_hovered: false,
        })
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

/// Schedule the focus underline to grow from the centre (input's `set_focus`).
fn start_focus_animation(context: &mut Context) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    unsafe {
        let transition = context.transition_library.CreateCubicBezierLinearTransition(
            tokens.duration_normal,
            1.0,
            tokens.curve_decelerate_mid[0],
            tokens.curve_decelerate_mid[1],
            tokens.curve_decelerate_mid[2],
            tokens.curve_decelerate_mid[3],
        )?;
        let seconds_now = context.animation_timer.GetTime()?;
        context.bottom_focus_border = context.animation_manager.CreateAnimationVariable(0.0)?;
        context.animation_manager.ScheduleTransition(
            &context.bottom_focus_border,
            &transition,
            seconds_now,
        )?;
    }
    Ok(())
}

/// Auto-size to the widest option + chevron + padding, and resize the render target.
fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let mut widest = 0.0f32;
    for option in &state.props.options {
        widest = widest.max(measure_text_width(&state.qt, &context.text_format, option.text));
    }
    widest = widest.max(measure_text_width(
        &state.qt,
        &context.text_format,
        state.props.placeholder,
    ));
    let pad = state.horizontal_padding();
    let width = pad + widest + state.qt.theme.tokens.spacing_horizontal_s + 20.0 + pad;
    let height = state.field_height();

    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (width * scaling_factor).ceil() as i32;
    let scaled_height = (height * scaling_factor).ceil() as i32;
    unsafe {
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
        // Rounded window region (like input) so the focus underline's corners
        // follow the field's rounded shape instead of showing square edges.
        let corner_diameter =
            (state.qt.theme.tokens.border_radius_medium * scaling_factor * 2.0) as i32;
        let region = CreateRoundRectRgn(
            0,
            0,
            scaled_width + 1,
            scaled_height + 1,
            corner_diameter,
            corner_diameter,
        );
        SetWindowRgn(window, Some(region), true);
    }
    Ok(())
}

/// Bottom edge plus the lower half of each rounded corner — the resting accent
/// line that follows the field's rounded bottom (matches input's underline).
/// All values in DIPs (the render target's DPI does the scaling).
fn bottom_accent_geometry(
    factory: &ID2D1Factory1,
    width: f32,
    r: f32,
    cy: f32,
) -> Result<ID2D1PathGeometry1> {
    let left_cx = r;
    let right_cx = width - r;
    let corner_cy = cy - r;
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.BeginFigure(
            Vector2 {
                X: left_cx - d,
                Y: corner_cy + d,
            },
            D2D1_FIGURE_BEGIN_HOLLOW,
        );
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: left_cx, Y: cy },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(Vector2 {
            X: right_cx,
            Y: cy,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: right_cx + d,
                Y: corner_cy + d,
            },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        Ok(geometry)
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
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let stroke = tokens.stroke_width_thin;
        let radius = tokens.border_radius_medium;

        // The field box: white rounded rect on the (canvas) background.
        let field_rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: stroke * 0.5,
                top: stroke * 0.5,
                right: width - stroke * 0.5,
                bottom: height - stroke * 0.5,
            },
            radiusX: radius,
            radiusY: radius,
        };
        let fill_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_background1, None)?;
        context
            .render_target
            .FillRoundedRectangle(&field_rect, &fill_brush);

        // Outline border (Outline appearance): darker when focused, or on hover
        // when not focused (Fluent Input :hover → colorNeutralStroke1Hover).
        if let input::Appearance::Outline = state.props.appearance {
            let border_color = if context.is_focused {
                &tokens.color_neutral_stroke1_pressed
            } else if context.is_hovered {
                &tokens.color_neutral_stroke1_hover
            } else {
                &tokens.color_neutral_stroke1
            };
            let border_brush = context.render_target.CreateSolidColorBrush(border_color, None)?;
            context.render_target.DrawRoundedRectangle(
                &field_rect,
                &border_brush,
                stroke,
                &state.qt.stroke_style,
            );
        }

        // Resting bottom accent line (darkens on hover, per Fluent), then the brand
        // underline growing from the centre on focus — driven by the WAM variable.
        let accent_color = if context.is_hovered && !context.is_focused {
            &tokens.color_neutral_stroke_accessible_hover
        } else {
            &tokens.color_neutral_stroke_accessible
        };
        let accent_brush = context
            .render_target
            .CreateSolidColorBrush(accent_color, None)?;
        let accent_geometry =
            bottom_accent_geometry(&state.qt.d2d_factory, width, radius, height - stroke * 0.5)?;
        context.render_target.DrawGeometry(
            &accent_geometry,
            &accent_brush,
            stroke,
            &state.qt.stroke_style,
        );
        if context.is_focused {
            let percentage = context.bottom_focus_border.GetValue()? as f32;
            let left = width * (1.0 - percentage) / 2.0;
            let underline_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_compound_brand_stroke, None)?;
            context.render_target.FillRectangle(
                &D2D_RECT_F {
                    left,
                    top: height - 2.0,
                    right: left + width * percentage,
                    bottom: height,
                },
                &underline_brush,
            );
        }

        // Selected option text (or placeholder), left-aligned, vertically centred.
        let pad = state.horizontal_padding();
        let (text, text_color) = match context.selected_index {
            Some(i) if i < state.props.options.len() => {
                (state.props.options[i].text, &tokens.color_neutral_foreground1)
            }
            _ => (state.props.placeholder, &tokens.color_neutral_foreground3),
        };
        if !text.is_null() && !text.as_wide().is_empty() {
            let text_brush = context.render_target.CreateSolidColorBrush(text_color, None)?;
            context.render_target.DrawText(
                text.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: pad,
                    top: 0.0,
                    right: width - pad - 20.0,
                    bottom: height,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        // Chevron-down at the right edge, vertically centred.
        let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        let chevron_x = width - pad - 20.0;
        let chevron_y = (height - 20.0) / 2.0;
        device_context5.SetTransform(&Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: chevron_x,
            M32: chevron_y,
        });
        device_context5.DrawSvgDocument(&context.chevron_svg);
        device_context5.SetTransform(&Matrix3x2::identity());
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

/// Open the popup, apply the pick. Uses raw-pointer field access (never holding a
/// `&mut Context` across `run_popup`, whose modal loop re-enters this window_proc).
fn open(window: HWND, raw: *mut Context) {
    unsafe {
        let qt = (*raw).state.qt.clone();
        let options = (*raw).state.props.options.clone();
        let selected = (*raw).selected_index;

        let picked = run_popup(&qt, window, &options, selected);

        if let Some(i) = picked {
            (*raw).selected_index = Some(i);
            (*(*raw).state.props.mouse_event.on_select)(&window, i);
        }
        _ = InvalidateRect(Some(window), None, false);
    }
}

extern "system" fn field_proc(
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
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = true;
            _ = start_focus_animation(&mut *raw);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = false;
            // Reset the underline variable so no stale value can paint blue, and
            // force the update region (bare InvalidateRect can be coalesced/lost —
            // input uses RedrawWindow here for the same reason).
            (*raw).bottom_focus_border =
                match (*raw).animation_manager.CreateAnimationVariable(0.0) {
                    Ok(v) => v,
                    Err(_) => (*raw).bottom_focus_border.clone(),
                };
            _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if !context.is_hovered {
                context.is_hovered = true;
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
            (*raw).is_hovered = false;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            _ = SetFocus(Some(window));
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            open(window, raw);
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_RETURN | VK_SPACE | VK_F4 | VK_DOWN => open(window, raw),
                _ => return DefWindowProcW(window, message, w_param, l_param),
            }
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

// ---------------------------------------------------------------------------
// Popup listbox
// ---------------------------------------------------------------------------

struct PopupParams {
    qt: QT,
    options: Vec<Item>,
    selected: Option<usize>,
    width_dip: f32,
}

struct PopupContext {
    qt: QT,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    checkmark_svg: ID2D1SvgDocument,
    options: Vec<Item>,
    selected: Option<usize>,
    hovered: Option<usize>,
    width_dip: f32,
}

fn popup_on_create(window: HWND, params: PopupParams) -> Result<PopupContext> {
    let tokens = &params.qt.theme.tokens;
    unsafe {
        let text_format = create_text_format(&params.qt, tokens.font_size_base300)?;

        let dpi = GetDpiForWindow(window);
        let render_target = params.qt.d2d_factory.CreateHwndRenderTarget(
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

        let icon = Icon::checkmark_20_filled();
        let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
        let svg_stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
        let checkmark_svg = device_context5.CreateSvgDocument(
            svg_stream.as_ref(),
            D2D_SIZE_F {
                width: icon.size as f32,
                height: icon.size as f32,
            },
        )?;
        // Fluent's Option checkIcon is `fill: currentColor` — the option's text
        // colour (neutral foreground1), NOT the brand colour.
        _ = set_svg_color(&checkmark_svg, &tokens.color_neutral_foreground1);

        let hovered = match params.selected {
            Some(i) if !params.options[i].disabled => Some(i),
            _ => first_enabled(&params.options),
        };
        Ok(PopupContext {
            qt: params.qt,
            render_target,
            text_format,
            checkmark_svg,
            options: params.options,
            selected: params.selected,
            hovered,
            width_dip: params.width_dip,
        })
    }
}

/// First enabled option index, if any.
fn first_enabled(options: &[Item]) -> Option<usize> {
    options.iter().position(|o| !o.disabled)
}

/// Last enabled option index, if any.
fn last_enabled(options: &[Item]) -> Option<usize> {
    options.iter().rposition(|o| !o.disabled)
}

/// Next enabled option after `from` (or the first enabled if `from` is None).
fn next_enabled(options: &[Item], from: Option<usize>) -> Option<usize> {
    let start = match from {
        Some(i) => i + 1,
        None => 0,
    };
    (start..options.len())
        .find(|&i| !options[i].disabled)
        .or(from)
}

/// Previous enabled option before `from` (or the last enabled if `from` is None).
fn prev_enabled(options: &[Item], from: Option<usize>) -> Option<usize> {
    match from {
        Some(i) => (0..i).rev().find(|&j| !options[j].disabled).or(from),
        None => last_enabled(options),
    }
}

fn popup_paint(window: HWND, context: &PopupContext) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    unsafe {
        context.render_target.Clear(Some(&tokens.color_neutral_background1));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let width = context.width_dip;

        // Fluent Listbox pads all options by `LIST_PADDING`; rows are contiguous
        // 32px bands inside that padding.
        let item_left = LIST_PADDING;
        let item_right = width - LIST_PADDING;
        let item_pad = tokens.spacing_horizontal_s; // Option's own paddingX (8)
        let gap = tokens.spacing_horizontal_xs; // Option's columnGap (4)

        for (i, option) in context.options.iter().enumerate() {
            let top = LIST_PADDING + i as f32 * ITEM_HEIGHT;
            // Hover highlight fills the full option row (Fluent has no per-row
            // margin; the margin is the Listbox padding around all rows).
            // Disabled options never highlight.
            if context.hovered == Some(i) && !option.disabled {
                let hover_brush = context
                    .render_target
                    .CreateSolidColorBrush(&tokens.color_neutral_background1_hover, None)?;
                let rounded = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: item_left,
                        top,
                        right: item_right,
                        bottom: top + ITEM_HEIGHT,
                    },
                    radiusX: tokens.border_radius_medium,
                    radiusY: tokens.border_radius_medium,
                };
                context
                    .render_target
                    .FillRoundedRectangle(&rounded, &hover_brush);
            }
            // Checkmark for the selected option (16px, neutral foreground).
            if context.selected == Some(i) {
                let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
                let scale = CHECK_SIZE / 20.0;
                device_context5.SetTransform(&Matrix3x2 {
                    M11: scale,
                    M12: 0.0,
                    M21: 0.0,
                    M22: scale,
                    M31: item_left + item_pad,
                    M32: top + (ITEM_HEIGHT - CHECK_SIZE) / 2.0,
                });
                device_context5.DrawSvgDocument(&context.checkmark_svg);
                device_context5.SetTransform(&Matrix3x2::identity());
            }
            // Option text (after the check column + column gap). Disabled → greyed.
            let text_color = if option.disabled {
                &tokens.color_neutral_foreground_disabled
            } else {
                &tokens.color_neutral_foreground1
            };
            let text_brush = context.render_target.CreateSolidColorBrush(text_color, None)?;
            context.render_target.DrawText(
                option.text.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: item_left + item_pad + CHECK_SIZE + gap,
                    top,
                    right: item_right - item_pad,
                    bottom: top + ITEM_HEIGHT,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
    Ok(())
}

fn popup_on_paint(window: HWND, context: &PopupContext) -> Result<()> {
    unsafe {
        context.render_target.BeginDraw();
        let result = popup_paint(window, context);
        match result {
            Ok(_) => context.render_target.EndDraw(None, None),
            Err(_) => {
                context.render_target.EndDraw(None, None)?;
                result
            }
        }
    }
}

extern "system" fn popup_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut PopupParams;
            let params = Box::<PopupParams>::from_raw(raw);
            match popup_on_create(window, *params) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<PopupContext>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            if !raw.is_null() {
                _ = Box::<PopupContext>::from_raw(raw);
            }
            LRESULT(0)
        },
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            let context = &*raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = popup_on_paint(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}

/// Hit-test a popup client point (device px) to an *enabled* option index.
/// Disabled rows (and gaps) return None — they never hover or select.
fn hit_test(context: &PopupContext, x: i32, y: i32, scaling_factor: f32) -> Option<usize> {
    let x_dip = x as f32 / scaling_factor;
    let y_dip = y as f32 / scaling_factor - LIST_PADDING;
    if x_dip < 0.0 || x_dip > context.width_dip || y_dip < 0.0 {
        return None;
    }
    let i = (y_dip / ITEM_HEIGHT) as usize;
    match context.options.get(i) {
        Some(o) if !o.disabled => Some(i),
        _ => None,
    }
}

/// True if a popup client point (device px) is inside the list area at all
/// (used to tell a disabled-row click from a click-outside).
fn in_popup_bounds(context: &PopupContext, x: i32, y: i32, scaling_factor: f32) -> bool {
    let x_dip = x as f32 / scaling_factor;
    let y_dip = y as f32 / scaling_factor;
    let total_h = LIST_PADDING * 2.0 + context.options.len() as f32 * ITEM_HEIGHT;
    x_dip >= 0.0 && x_dip <= context.width_dip && y_dip >= 0.0 && y_dip <= total_h
}

/// Create the popup below `field`, run a synchronous modal loop, return the pick.
fn run_popup(qt: &QT, field: HWND, options: &[Item], selected: Option<usize>) -> Option<usize> {
    if options.is_empty() {
        return None;
    }
    unsafe {
        let mut field_rect = RECT::default();
        if GetWindowRect(field, &mut field_rect).is_err() {
            return None;
        }
        let scaling_factor = get_scaling_factor(field);
        let width_px = field_rect.right - field_rect.left;
        let width_dip = width_px as f32 / scaling_factor;
        let height_px = ((ITEM_HEIGHT * options.len() as f32 + LIST_PADDING * 2.0)
            * scaling_factor)
            .ceil() as i32;

        // Position directly below the field; flip above if it would overflow.
        let monitor = MonitorFromWindow(field, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        _ = GetMonitorInfoW(monitor, &mut info);
        let x = field_rect.left;
        let mut y = field_rect.bottom;
        if y + height_px > info.rcWork.bottom {
            y = (field_rect.top - height_px).max(info.rcWork.top);
        }

        let params = Box::new(PopupParams {
            qt: qt.clone(),
            options: options.to_vec(),
            selected,
            width_dip,
        });
        let Ok(popup) = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            POPUP_CLASS,
            w!(""),
            WS_POPUP,
            x,
            y,
            width_px,
            height_px,
            Some(field),
            None,
            Some(HINSTANCE(GetWindowLongPtrW(field, GWLP_HINSTANCE) as _)),
            Some(Box::<PopupParams>::into_raw(params) as _),
        ) else {
            return None;
        };

        // Rounded corners like the menu popup.
        let corner = (qt.theme.tokens.border_radius_medium * 2.0 * scaling_factor) as i32;
        let region = CreateRoundRectRgn(0, 0, width_px + 1, height_px + 1, corner, corner);
        SetWindowRgn(popup, Some(region), false);
        _ = SetWindowPos(
            popup,
            Some(HWND_TOPMOST),
            x,
            y,
            width_px,
            height_px,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        );

        let raw = GetWindowLongPtrW(popup, GWLP_USERDATA) as *mut PopupContext;
        // The render target was created at WM_CREATE before the window had a size,
        // so its auto-detected pixel size was 0×0 (nothing painted → only the
        // dropshadow showed). Resize it to the real client size now.
        let popup_dpi = GetDpiForWindow(popup);
        (*raw).render_target.SetDpi(popup_dpi as f32, popup_dpi as f32);
        _ = (*raw).render_target.Resize(&D2D_SIZE_U {
            width: width_px as u32,
            height: height_px as u32,
        });
        _ = UpdateWindow(popup);

        let mut result: Option<usize> = None;
        // Screen point of a click-outside dismissal, so we can move focus to the
        // window the user actually clicked (the popup's capture swallows it).
        let mut dismiss_point: Option<POINT> = None;

        SetCapture(popup);
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }
            let mut done = false;
            match msg.message {
                WM_MOUSEMOVE => {
                    let x = msg.lParam.0 as i16 as i32;
                    let y = (msg.lParam.0 >> 16) as i16 as i32;
                    let hit = hit_test(&*raw, x, y, scaling_factor);
                    if (*raw).hovered != hit {
                        (*raw).hovered = hit;
                        _ = InvalidateRect(Some(popup), None, false);
                    }
                }
                WM_LBUTTONUP => {
                    let x = msg.lParam.0 as i16 as i32;
                    let y = (msg.lParam.0 >> 16) as i16 as i32;
                    if let Some(i) = hit_test(&*raw, x, y, scaling_factor) {
                        // Released over an enabled option → select and close.
                        result = Some(i);
                        done = true;
                    } else if !in_popup_bounds(&*raw, x, y, scaling_factor) {
                        // Released outside the list → dismiss.
                        let mut pt = POINT::default();
                        _ = GetCursorPos(&mut pt);
                        dismiss_point = Some(pt);
                        done = true;
                    }
                    // Else: released on a disabled row → stay open, no-op.
                }
                WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                    // A press outside the list cancels (click-outside). A press on
                    // a disabled row is ignored (stays open).
                    let x = msg.lParam.0 as i16 as i32;
                    let y = (msg.lParam.0 >> 16) as i16 as i32;
                    if !in_popup_bounds(&*raw, x, y, scaling_factor) {
                        let mut pt = POINT::default();
                        _ = GetCursorPos(&mut pt);
                        dismiss_point = Some(pt);
                        done = true;
                    }
                }
                WM_KEYDOWN => {
                    match VIRTUAL_KEY(msg.wParam.0 as u16) {
                        VK_DOWN => {
                            (*raw).hovered = next_enabled(&(*raw).options, (*raw).hovered);
                            _ = InvalidateRect(Some(popup), None, false);
                        }
                        VK_UP => {
                            (*raw).hovered = prev_enabled(&(*raw).options, (*raw).hovered);
                            _ = InvalidateRect(Some(popup), None, false);
                        }
                        VK_HOME => {
                            (*raw).hovered = first_enabled(&(*raw).options);
                            _ = InvalidateRect(Some(popup), None, false);
                        }
                        VK_END => {
                            (*raw).hovered = last_enabled(&(*raw).options);
                            _ = InvalidateRect(Some(popup), None, false);
                        }
                        VK_RETURN => {
                            result = (*raw).hovered;
                            done = true;
                        }
                        VK_ESCAPE => {
                            done = true;
                        }
                        _ => {}
                    }
                }
                _ => {
                    _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if done {
                break;
            }
        }
        _ = ReleaseCapture();
        _ = DestroyWindow(popup);
        // The modal loop drops the field's real keyboard focus (it goes to NONE),
        // while its `is_focused` flag stays true — so it can never get a normal
        // WM_KILLFOCUS and the underline sticks. Restore genuine focus to the field
        // first so its state matches reality.
        _ = SetFocus(Some(field));
        // Click-outside: then move focus to the clicked window, so the field now
        // *loses* focus properly (WM_KILLFOCUS fires → underline clears), de-focusing
        // like a real combo. `field` holds focus at this point, so the transfer works.
        if let Some(pt) = dismiss_point {
            let target = WindowFromPoint(pt);
            if !target.is_invalid() && target != field {
                _ = SetFocus(Some(target));
            }
        }
        result
    }
}
