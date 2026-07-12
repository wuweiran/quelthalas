//! A date picker — Win32 `SysDateTimePick32`, Fluent-styled (Fluent React's
//! `DatePicker`). A read-styled field (like `dropdown`'s) with a trailing calendar
//! glyph; clicking it (or Enter/Space/Down/F4) opens the **Calendar** in a flyout
//! popup below. Picking a day closes the flyout and writes the date into the field,
//! formatted with the user's locale short-date pattern (`GetDateFormatEx`) — exactly
//! what the real `SysDateTimePick32` shows.
//!
//! Structurally the field clones `dropdown`'s field wiring (chrome, WAM focus
//! underline, hover/focus tracking). The flyout differs: instead of drawing its own
//! rows and holding `SetCapture`, it **hosts a real `create_calendar` child HWND** and
//! runs a modal loop WITHOUT capture, so the Calendar gets its own mouse/keys.

use std::cell::Cell;
use std::mem::size_of;
use std::rc::Rc;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{DATE_SHORTDATE, GetDateFormatEx};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_SVG_PAINT_TYPE_COLOR, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1DeviceContext5,
    ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1PathGeometry1, ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, GetMonitorInfoW, InvalidateRect,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, RDW_INVALIDATE,
    RedrawWindow, SetWindowRgn, UpdateWindow,
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
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_F4,
    VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

use crate::component::calendar;
use crate::component::input;
use crate::icon::Icon;
use crate::{QT, get_scaling_factor};

pub use crate::component::calendar::Date;

const FIELD_CLASS: PCWSTR = w!("QT_DATE_PICKER");
const POPUP_CLASS: PCWSTR = w!("QT_DATE_PICKER_POPUP");
/// Trailing calendar glyph draw size (DIPs).
const GLYPH: f32 = 20.0;
/// Posted to the popup by the Calendar's `on_select_date` to end the modal loop.
const WM_APP_PICKED: u32 = WM_APP + 1;
/// Posted to the popup to cancel the modal loop (e.g. the field lost focus because the
/// user switched window/app).
const WM_APP_CANCEL: u32 = WM_APP + 2;

pub struct MouseEvent {
    pub on_select_date: Box<dyn Fn(&HWND, Date)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_select_date: Box::new(|_, _| {}),
        }
    }
}

pub struct Props {
    /// Initial date, or `None` to show the placeholder.
    pub value: Option<Date>,
    pub placeholder: PCWSTR,
    pub size: input::Size,
    pub appearance: input::Appearance,
    /// Fixed width (DIPs). `0` = a default.
    pub width: i32,
    pub background: Option<D2D1_COLOR_F>,
    pub mouse_event: MouseEvent,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            value: None,
            placeholder: w!("Select a date\u{2026}"),
            size: input::Size::Medium,
            appearance: input::Appearance::Outline,
            width: 0,
            background: None,
            mouse_event: MouseEvent::default(),
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    fn field_height(&self) -> f32 {
        match self.props.size {
            input::Size::Small => 24.0,
            input::Size::Medium => 32.0,
            input::Size::Large => 40.0,
        }
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
    glyph_svg: ID2D1SvgDocument,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    bottom_focus_border: IUIAnimationVariable2,
    value: Option<Date>,
    is_focused: bool,
    is_hovered: bool,
    /// The open flyout popup, if any — so `WM_KILLFOCUS` (app/window switch) can
    /// dismiss it. `HWND::default()` when closed.
    flyout_popup: HWND,
}

impl QT {
    pub fn create_date_picker(
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
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }

    /// The currently selected date of a date picker created by `create_date_picker`.
    pub fn date_picker_value(&self, date_picker: HWND) -> Option<Date> {
        unsafe {
            let raw = GetWindowLongPtrW(date_picker, GWLP_USERDATA) as *const Context;
            if raw.is_null() { None } else { (*raw).value }
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

/// Format `date` with the user's locale short-date pattern (`GetDateFormatEx`).
fn format_date(date: Date) -> Vec<u16> {
    let st = SYSTEMTIME {
        wYear: date.year,
        wMonth: date.month as u16,
        wDay: date.day as u16,
        ..Default::default()
    };
    let mut buf = [0u16; 128];
    // Null locale name = LOCALE_NAME_USER_DEFAULT; null format = the locale's
    // short-date pattern (DATE_SHORTDATE).
    let len = unsafe {
        GetDateFormatEx(
            PCWSTR::null(),
            DATE_SHORTDATE,
            Some(&st),
            PCWSTR::null(),
            Some(&mut buf),
            PCWSTR::null(),
        )
    };
    if len > 1 {
        buf[..(len - 1) as usize].to_vec()
    } else {
        Vec::new()
    }
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

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    let value = state.props.value;
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
                pixelSize: D2D_SIZE_U { width: 0, height: 0 },
                presentOptions: Default::default(),
            },
        )?;

        let icon = Icon::calendar_month_20_regular();
        let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
        let svg_stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
        let glyph_svg = device_context5.CreateSvgDocument(
            svg_stream.as_ref(),
            D2D_SIZE_F { width: icon.size as f32, height: icon.size as f32 },
        )?;
        _ = set_svg_color(&glyph_svg, &tokens.color_neutral_stroke_accessible);

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
            glyph_svg,
            animation_manager,
            animation_timer,
            transition_library,
            bottom_focus_border,
            value,
            is_focused: false,
            is_hovered: false,
            flyout_popup: HWND::default(),
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

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let pad = state.horizontal_padding();
    let width = if state.props.width > 0 {
        state.props.width as f32
    } else {
        // A sensible default (fits a long localized short-date + the glyph).
        pad + 140.0 + state.qt.theme.tokens.spacing_horizontal_s + GLYPH + pad
    };
    let height = state.field_height();

    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (width * scaling_factor).ceil() as i32;
    let scaled_height = (height * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(window, None, 0, 0, scaled_width, scaled_height, SWP_NOMOVE | SWP_NOZORDER)?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;
        let corner_diameter =
            (state.qt.theme.tokens.border_radius_medium * scaling_factor * 2.0) as i32;
        let region = CreateRoundRectRgn(0, 0, scaled_width + 1, scaled_height + 1, corner_diameter, corner_diameter);
        SetWindowRgn(window, Some(region), true);
    }
    Ok(())
}

/// Bottom edge plus the lower half of each rounded corner (the resting underline).
fn bottom_accent_geometry(factory: &ID2D1Factory1, width: f32, r: f32, cy: f32) -> Result<ID2D1PathGeometry1> {
    let left_cx = r;
    let right_cx = width - r;
    let corner_cy = cy - r;
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.BeginFigure(Vector2 { X: left_cx - d, Y: corner_cy + d }, D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: left_cx, Y: cy },
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(Vector2 { X: right_cx, Y: cy });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: right_cx + d, Y: corner_cy + d },
            size: D2D_SIZE_F { width: r, height: r },
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
        let background = state.props.background.unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let stroke = tokens.stroke_width_thin;
        let radius = tokens.border_radius_medium;

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
        let fill_brush = context.render_target.CreateSolidColorBrush(&tokens.color_neutral_background1, None)?;
        context.render_target.FillRoundedRectangle(&field_rect, &fill_brush);

        if let input::Appearance::Outline = state.props.appearance {
            let border_color = if context.is_focused {
                &tokens.color_neutral_stroke1_pressed
            } else if context.is_hovered {
                &tokens.color_neutral_stroke1_hover
            } else {
                &tokens.color_neutral_stroke1
            };
            let border_brush = context.render_target.CreateSolidColorBrush(border_color, None)?;
            context.render_target.DrawRoundedRectangle(&field_rect, &border_brush, stroke, &state.qt.stroke_style);
        }

        let accent_color = if context.is_hovered && !context.is_focused {
            &tokens.color_neutral_stroke_accessible_hover
        } else {
            &tokens.color_neutral_stroke_accessible
        };
        let accent_brush = context.render_target.CreateSolidColorBrush(accent_color, None)?;
        let accent_geometry = bottom_accent_geometry(&state.qt.d2d_factory, width, radius, height - stroke * 0.5)?;
        context.render_target.DrawGeometry(&accent_geometry, &accent_brush, stroke, &state.qt.stroke_style);
        if context.is_focused {
            let percentage = context.bottom_focus_border.GetValue()? as f32;
            let left = width * (1.0 - percentage) / 2.0;
            let underline_brush = context.render_target.CreateSolidColorBrush(&tokens.color_compound_brand_stroke, None)?;
            context.render_target.FillRectangle(
                &D2D_RECT_F { left, top: height - 2.0, right: left + width * percentage, bottom: height },
                &underline_brush,
            );
        }

        // Value text (or placeholder), left-aligned, vertically centred.
        let pad = state.horizontal_padding();
        let value_text: Vec<u16> = context.value.map(format_date).unwrap_or_default();
        let (text, text_color): (&[u16], &D2D1_COLOR_F) = if !value_text.is_empty() {
            (&value_text, &tokens.color_neutral_foreground1)
        } else {
            (state.props.placeholder.as_wide(), &tokens.color_neutral_foreground3)
        };
        if !text.is_empty() {
            let text_brush = context.render_target.CreateSolidColorBrush(text_color, None)?;
            context.render_target.DrawText(
                text,
                &context.text_format,
                &D2D_RECT_F { left: pad, top: 0.0, right: width - pad - GLYPH, bottom: height },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        // Calendar glyph at the right edge, vertically centred.
        let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        let glyph_x = width - pad - GLYPH;
        let glyph_y = (height - GLYPH) / 2.0;
        device_context5.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: glyph_x, M32: glyph_y });
        device_context5.DrawSvgDocument(&context.glyph_svg);
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

/// Open the flyout, apply the pick. Clones state out of the raw pointer BEFORE the
/// re-entrant modal loop (never holds `&mut Context` across it).
fn open(window: HWND, raw: *mut Context) {
    unsafe {
        let qt = (*raw).state.qt.clone();
        let selected = (*raw).value;

        let picked = run_calendar_popup(&qt, window, raw, selected);

        if let Some(date) = picked {
            (*raw).value = Some(date);
            ((*raw).state.props.mouse_event.on_select_date)(&window, date);
        }
        _ = InvalidateRect(Some(window), None, false);
    }
}

extern "system" fn field_proc(window: HWND, message: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
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
            if !raw.is_null() {
                _ = Box::<Context>::from_raw(raw);
            }
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
            _ = on_paint(window, &*raw);
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
            // If a flyout is open, the field losing focus means the user switched
            // window/app (the flyout never takes focus itself) → dismiss it.
            let popup = (*raw).flyout_popup;
            if !popup.is_invalid() {
                _ = PostMessageW(Some(popup), WM_APP_CANCEL, WPARAM(0), LPARAM(0));
            }
            (*raw).bottom_focus_border = match (*raw).animation_manager.CreateAnimationVariable(0.0) {
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
        WM_GETDLGCODE => LRESULT(DLGC_WANTARROWS as isize),
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
// Flyout popup — hosts a real Calendar child HWND
// ---------------------------------------------------------------------------

struct PopupParams {
    qt: QT,
    selected: Option<Date>,
    /// The Calendar's `on_select_date` writes the pick here and posts `WM_APP_PICKED`.
    picked: Rc<Cell<Option<Date>>>,
    /// Receives the created Calendar child HWND (so the loop can forward keys to it).
    child: Rc<Cell<HWND>>,
}

extern "system" fn popup_proc(window: HWND, message: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut PopupParams;
            let params = Box::<PopupParams>::from_raw(raw);
            let picked = params.picked.clone();
            // Embedded Calendar: never grabs focus. Menu surface background
            // (theme background1) — NOT the field's canvas fill.
            let cal = params.qt.create_calendar_embedded(
                window,
                0,
                0,
                calendar::Props {
                    selected: params.selected,
                    background: None,
                    mouse_event: calendar::MouseEvent {
                        on_select_date: Box::new(move |_, date| {
                            picked.set(Some(date));
                            _ = PostMessageW(Some(window), WM_APP_PICKED, WPARAM(0), LPARAM(0));
                        }),
                    },
                    ..Default::default()
                },
            );
            if let Ok(cal) = cal {
                params.child.set(cal);
            }
            LRESULT(0)
        },
        // A menu-style flyout must not take activation from the owner window.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}

/// Show the Calendar in a flyout under `field`, run a modal loop, return the picked
/// date (or `None` if dismissed). No `SetCapture` — the hosted Calendar child needs
/// its own mouse/keyboard. `field_ctx` is the field's Context so its `WM_KILLFOCUS`
/// (window/app switch) can dismiss the flyout.
fn run_calendar_popup(
    qt: &QT,
    field: HWND,
    field_ctx: *mut Context,
    selected: Option<Date>,
) -> Option<Date> {
    unsafe {
        let mut field_rect = RECT::default();
        if GetWindowRect(field, &mut field_rect).is_err() {
            return None;
        }
        let scaling_factor = get_scaling_factor(field);
        let (nat_w, nat_h) = calendar::natural_size();
        let width_px = (nat_w * scaling_factor).ceil() as i32;
        let height_px = (nat_h * scaling_factor).ceil() as i32;

        // Position below the field; flip above if it would overflow the work area.
        let monitor = MonitorFromWindow(field, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        _ = GetMonitorInfoW(monitor, &mut info);
        let x = field_rect.left;
        let mut y = field_rect.bottom;
        if y + height_px > info.rcWork.bottom {
            y = (field_rect.top - height_px).max(info.rcWork.top);
        }

        let picked: Rc<Cell<Option<Date>>> = Rc::new(Cell::new(None));
        let child: Rc<Cell<HWND>> = Rc::new(Cell::new(HWND::default()));
        let params = Box::new(PopupParams {
            qt: qt.clone(),
            selected,
            picked: picked.clone(),
            child: child.clone(),
        });
        let Ok(popup) = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
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

        let corner = (qt.theme.tokens.border_radius_medium * 2.0 * scaling_factor) as i32;
        let region = CreateRoundRectRgn(0, 0, width_px + 1, height_px + 1, corner, corner);
        SetWindowRgn(popup, Some(region), false);
        _ = SetWindowPos(popup, Some(HWND_TOPMOST), x, y, width_px, height_px, SWP_SHOWWINDOW | SWP_NOACTIVATE);
        _ = UpdateWindow(popup);

        // Record the open popup so the field's WM_KILLFOCUS can cancel it when the
        // user switches window/app (the field keeps focus while the flyout is up).
        (*field_ctx).flyout_popup = popup;

        let cal_child = child.get();

        // Modal loop WITHOUT capture and WITHOUT stealing focus: the field keeps
        // keyboard focus (owner stays active, like a normal menu), so we forward keys
        // to the Calendar child ourselves. Dismiss on a pick, an outside click, or Esc.
        let mut msg = MSG::default();
        loop {
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                break;
            }
            let mut done = false;
            match msg.message {
                WM_APP_PICKED => {
                    done = true;
                }
                WM_APP_CANCEL => {
                    // The field lost focus (window/app switch) → dismiss.
                    done = true;
                }
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN => {
                    // A press on any window that is neither the popup nor its Calendar
                    // child dismisses (click-outside).
                    if msg.hwnd != popup && !IsChild(popup, msg.hwnd).as_bool() {
                        done = true;
                    } else {
                        _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                WM_KEYDOWN | WM_KEYUP | WM_CHAR => {
                    // Focus stayed on the field; route navigation keys to the Calendar.
                    if msg.message == WM_KEYDOWN
                        && VIRTUAL_KEY(msg.wParam.0 as u16) == VK_ESCAPE
                    {
                        done = true;
                    } else if !cal_child.is_invalid() {
                        SendMessageW(cal_child, msg.message, Some(msg.wParam), Some(msg.lParam));
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

        (*field_ctx).flyout_popup = HWND::default();
        _ = DestroyWindow(popup);
        // Drain a stray button-up left in the queue (e.g. the click that dismissed us
        // landed on the field) so it can't immediately re-open the flyout.
        let mut stray = MSG::default();
        _ = PeekMessageW(&mut stray, Some(field), WM_LBUTTONUP, WM_LBUTTONUP, PM_REMOVE);
        picked.get()
    }
}
