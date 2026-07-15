//! A **MessageBar** — Fluent UI 2's inline, non-modal notification strip. A tinted
//! rounded bar with an intent icon, a message, and optional trailing action buttons.
//! This is the modern replacement for the (dropped) Win32 status-bar pattern.
//!
//! Single-line layout: `[intent icon]  message  ……  [action buttons]`. Four intents
//! (info / success / warning / error) each carry their own icon + background tint +
//! border (mirroring `task_dialog`'s intent mapping). The bar is a passive
//! self-painting `WS_CHILD` window that **hosts real `create_button` children** for
//! the actions (no capture, no popup — each button handles its own clicks). There is
//! no built-in dismiss; an app that wants one passes a "Dismiss" action.

use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget, ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, MapWindowPoints, PAINTSTRUCT,
    SetWindowRgn,
};
use crate::sys::dpi_for_window;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

use crate::component::button;
use crate::icon::Icon;
use crate::icon::path::build_geometry;
use crate::{MouseEvent, QT, get_scaling_factor};

/// Bar height (DIPs) — fits a 24px Small action button with vertical padding.
const BAR_HEIGHT: f32 = 36.0;
/// Intent icon draw size (DIPs).
const ICON: f32 = 20.0;

/// The notification's severity — sets the icon, background tint, and border.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Intent {
    Info,
    Success,
    Warning,
    Error,
}

/// A trailing action button (reuses the shared `MouseEvent`).
pub struct Action {
    pub text: PCWSTR,
    pub mouse_event: MouseEvent,
}

pub struct Props {
    pub intent: Intent,
    /// A short semibold title shown before the message (empty = none).
    pub title: PCWSTR,
    /// The message text. The caller keeps it alive (same contract as button/label text).
    pub message: PCWSTR,
    /// Trailing action buttons (0+). Empty = none.
    pub actions: Vec<Action>,
    /// Fixed width (DIPs). `0` = a default.
    pub width: i32,
    /// Override the intent background tint if needed.
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            intent: Intent::Info,
            title: w!(""),
            message: w!(""),
            actions: Vec::new(),
            width: 0,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    intent: Intent,
    title: PCWSTR,
    message: PCWSTR,
    width: f32,
    background: Option<D2D1_COLOR_F>,
    /// The action button child HWNDs (created in `on_create`, destroyed with the bar).
    buttons: Vec<HWND>,
}

impl State {
    /// (background tint, border) for the intent — or the props override for the tint.
    fn colors(&self) -> (D2D1_COLOR_F, D2D1_COLOR_F) {
        let t = &self.qt.theme.tokens;
        let (bg, border) = match self.intent {
            Intent::Info => (t.color_neutral_background3, t.color_neutral_stroke2),
            Intent::Success => (t.color_status_success_background1, t.color_status_success_border1),
            Intent::Warning => (t.color_status_warning_background1, t.color_status_warning_border1),
            Intent::Error => (t.color_status_danger_background1, t.color_status_danger_border1),
        };
        (self.background.unwrap_or(bg), border)
    }

    /// The intent icon color.
    fn icon_color(&self) -> D2D1_COLOR_F {
        let t = &self.qt.theme.tokens;
        match self.intent {
            Intent::Info => t.color_brand_foreground1,
            Intent::Success => t.color_status_success_foreground1,
            Intent::Warning => t.color_status_warning_foreground3,
            Intent::Error => t.color_status_danger_foreground1,
        }
    }

    fn icon(&self) -> Icon {
        match self.intent {
            Intent::Info => Icon::info_20_filled(),
            Intent::Success => Icon::checkmark_circle_20_filled(),
            Intent::Warning => Icon::warning_20_filled(),
            Intent::Error => Icon::diamond_dismiss_20_filled(),
        }
    }
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    title_format: IDWriteTextFormat,
    icon_geometry: Option<ID2D1PathGeometry1>,
}

impl QT {
    pub fn create_message_bar(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_MESSAGE_BAR");
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
            let scaling_factor = get_scaling_factor(parent_window);
            let width = if props.width > 0 { props.width as f32 } else { 360.0 };
            let boxed = Box::new(State {
                qt: self.clone(),
                intent: props.intent,
                title: props.title,
                message: props.message,
                width,
                background: props.background,
                buttons: Vec::new(),
            });
            // The action specs ride alongside State in the create tuple (the child
            // Buttons are created in on_create, once the bar HWND exists to parent them).
            let actions = props.actions;
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_VISIBLE | WS_CHILD,
                x,
                y,
                (width * scaling_factor) as i32,
                (BAR_HEIGHT * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<(State, Vec<Action>)>::into_raw(Box::new((*boxed, actions))) as _),
            )
        }
    }
}

/// Natural width (DIPs) of `text` in `format`.
fn measure_width(qt: &QT, format: &IDWriteTextFormat, text: PCWSTR) -> f32 {
    unsafe {
        let Ok(layout) = qt.dwrite_factory.CreateTextLayout(text.as_wide(), format, f32::MAX, f32::MAX) else {
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

fn on_create(window: HWND, mut state: State, actions: Vec<Action>) -> Result<Context> {
    unsafe {
        let tokens = &state.qt.theme.tokens;
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base300,
            w!(""),
        )?;
        text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;

        // Title: same size, semibold (Fluent body-strong).
        let title_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_semibold,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base300,
            w!(""),
        )?;
        title_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        title_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;

        let dpi = dpi_for_window(window);
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

        // The intent icon is a filled geometry (Direct2D 1.0); its tint is applied at
        // draw time via a brush (the intent color), so we only build the shape here.
        let icon_geometry = build_geometry(&state.qt.d2d_factory, &state.icon()).ok();

        // Create the real action Buttons, parented to the bar (Secondary, Small).
        for action in actions {
            if let Ok(btn) = state.qt.create_button(
                window,
                0,
                0,
                button::Props {
                    text: action.text,
                    appearance: button::Appearance::Secondary,
                    size: button::Size::Small,
                    mouse_event: action.mouse_event,
                    ..Default::default()
                },
            ) {
                state.buttons.push(btn);
            }
        }

        Ok(Context { state, render_target, text_format, title_format, icon_geometry })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (state.width * scaling_factor).ceil() as i32;
    let scaled_height = (BAR_HEIGHT * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(window, None, 0, 0, scaled_width, scaled_height, SWP_NOMOVE | SWP_NOZORDER)?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;
        let corner = (state.qt.theme.tokens.border_radius_medium * scaling_factor * 2.0) as i32;
        let region = CreateRoundRectRgn(0, 0, scaled_width + 1, scaled_height + 1, corner, corner);
        SetWindowRgn(window, Some(region), true);

        // Position action buttons right→left, vertically centered. Buttons self-size on
        // WM_CREATE, so read their device-px size back with GetWindowRect.
        let pad = (state.qt.theme.tokens.spacing_horizontal_m * scaling_factor) as i32;
        let gap = (state.qt.theme.tokens.spacing_horizontal_s * scaling_factor) as i32;
        let mut right = scaled_width - pad;
        for &btn in state.buttons.iter().rev() {
            let mut rc = RECT::default();
            _ = GetWindowRect(btn, &mut rc);
            let bw = rc.right - rc.left;
            let bh = rc.bottom - rc.top;
            let top = (scaled_height - bh) / 2;
            _ = SetWindowPos(btn, None, right - bw, top, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
            right -= bw + gap;
        }
    }
    Ok(())
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let stroke = tokens.stroke_width_thin;
        let radius = tokens.border_radius_medium;
        let (bg, border) = state.colors();

        context.render_target.Clear(Some(&bg));

        // Tinted rounded bar + 1px border.
        let bar = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: stroke * 0.5,
                top: stroke * 0.5,
                right: width - stroke * 0.5,
                bottom: height - stroke * 0.5,
            },
            radiusX: radius,
            radiusY: radius,
        };
        let fill = context.render_target.CreateSolidColorBrush(&bg, None)?;
        context.render_target.FillRoundedRectangle(&bar, &fill);
        let border_brush = context.render_target.CreateSolidColorBrush(&border, None)?;
        context.render_target.DrawRoundedRectangle(&bar, &border_brush, stroke, &state.qt.stroke_style);

        // Intent icon at the left, vertically centered.
        let pad = tokens.spacing_horizontal_m;
        let icon_top = (height - ICON) / 2.0;
        if let Some(geometry) = &context.icon_geometry {
            // Native art size of the intent icon (all intent icons are 20px).
            let native = state.icon().size as f32;
            let scale = ICON / native;
            // Same intent-based tint the SVG path baked in (info/success/warning/error).
            let icon_color = state.icon_color();
            let icon_brush = context.render_target.CreateSolidColorBrush(&icon_color, None)?;
            context.render_target.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: pad, M32: icon_top });
            context.render_target.FillGeometry(geometry, &icon_brush, None);
            context.render_target.SetTransform(&Matrix3x2::identity());
        }

        // Content row: [title (semibold)] [message], after the icon and clipped to end
        // before the first action button.
        let content_left = pad + ICON + tokens.spacing_horizontal_s;
        // Left edge of the leftmost button (DIPs), or the right padding if no buttons.
        let content_right = if let Some(&first) = state.buttons.first() {
            let mut brc = RECT::default();
            _ = GetWindowRect(first, &mut brc);
            let mut origin = RECT { left: brc.left, top: brc.top, right: brc.right, bottom: brc.bottom };
            _ = MapWindowPoints(Some(HWND_DESKTOP), Some(window), std::slice::from_raw_parts_mut(&mut origin as *mut RECT as *mut POINT, 2));
            origin.left as f32 / scaling_factor - tokens.spacing_horizontal_s
        } else {
            width - pad
        };

        let fg = context.render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
        let mut x = content_left;
        // Semibold title first.
        let has_title = !state.title.is_null() && !state.title.as_wide().is_empty();
        if has_title && content_right > x {
            context.render_target.DrawText(
                state.title.as_wide(),
                &context.title_format,
                &D2D_RECT_F { left: x, top: 0.0, right: content_right, bottom: height },
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            x += measure_width(&state.qt, &context.title_format, state.title)
                + tokens.spacing_horizontal_s;
        }
        // Message after the title.
        if !state.message.is_null() && !state.message.as_wide().is_empty() && content_right > x {
            context.render_target.DrawText(
                state.message.as_wide(),
                &context.text_format,
                &D2D_RECT_F { left: x, top: 0.0, right: content_right, bottom: height },
                &fg,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
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

extern "system" fn window_proc(window: HWND, message: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut (State, Vec<Action>);
            let (state, actions) = *Box::<(State, Vec<Action>)>::from_raw(raw);
            match on_create(window, state, actions) {
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
                drop(Box::<Context>::from_raw(raw));
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
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = layout(window, context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
