use std::cell::Cell;
use std::mem::{size_of, transmute};
use std::sync::Once;

use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, PAINTSTRUCT, SetWindowRgn,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

const POPUP_CLASS: PCWSTR = w!("QT_TOOLTIP");
/// Per-owner state pointer, attached to the subclassed window.
const TOOLTIP_PROP: PCWSTR = w!("QT_TOOLTIP_STATE");

/// Timer on the owner: fires once after the hover delay to show the tooltip.
const HOVER_TIMER_ID: usize = 0x71001;
/// Timer on the popup: drives the fade-in.
const FADE_TIMER_ID: usize = 1;
const FADE_INTERVAL_MS: u32 = 8;
/// Native tooltip default initial delay.
const HOVER_DELAY_MS: u32 = 500;
/// Fluent tooltip maxWidth (240) minus horizontal padding (2×12).
const MAX_TEXT_WIDTH: f32 = 240.0 - 24.0;

/// CSS cubic-bezier easing (copied from menu — kept module-local to avoid coupling).
fn cubic_bezier(t: f64, c: [f64; 4]) -> f64 {
    let axis = |s: f64, a: f64, b: f64| {
        3.0 * (1.0 - s).powi(2) * s * a + 3.0 * (1.0 - s) * s * s * b + s.powi(3)
    };
    let (mut lo, mut hi, mut s) = (0.0, 1.0, t);
    for _ in 0..20 {
        s = 0.5 * (lo + hi);
        if axis(s, c[0], c[2]) < t {
            lo = s;
        } else {
            hi = s;
        }
    }
    axis(s, c[1], c[3])
}

/// Tooltip text format: base200 (12/16), regular. Shared by measure + paint so
/// the wrapped layout matches.
fn create_text_format(qt: &QT) -> Result<IDWriteTextFormat> {
    let tokens = &qt.theme.tokens;
    unsafe {
        qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base200,
            w!(""),
        )
    }
}

// ---------------------------------------------------------------------------
// A. Owner subclass (the TTF_SUBCLASS half)
// ---------------------------------------------------------------------------

struct ToolInfo {
    qt: QT,
    text: PCWSTR,
    old_proc: WNDPROC,
    hover_pending: bool,
    popup: Option<HWND>,
}

impl QT {
    /// Attach a Fluent tooltip to `owner`. Subclasses the owner (like native
    /// `TTF_SUBCLASS`): after ~500ms of hover it shows a popup with `text`, hiding
    /// on leave/click. The caller keeps `text` alive (same contract as labels).
    pub fn add_tooltip(&self, owner: HWND, text: PCWSTR) -> Result<()> {
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: POPUP_CLASS,
                    style: CS_DROPSHADOW | CS_SAVEBITS,
                    lpfnWndProc: Some(popup_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });

            let boxed = Box::new(ToolInfo {
                qt: self.clone(),
                text,
                old_proc: None,
                hover_pending: false,
                popup: None,
            });
            let ptr = Box::into_raw(boxed);
            // Swap the owner's wndproc (user32 subclassing — comctl32-free).
            let old = SetWindowLongPtrW(owner, GWLP_WNDPROC, subclass_proc as *const () as isize);
            (*ptr).old_proc = transmute::<isize, WNDPROC>(old);
            SetPropW(owner, TOOLTIP_PROP, Some(HANDLE(ptr as _)))?;
        }
        Ok(())
    }
}

fn hide(info: &mut ToolInfo, owner: HWND) {
    unsafe {
        if info.hover_pending {
            _ = KillTimer(Some(owner), HOVER_TIMER_ID);
            info.hover_pending = false;
        }
        if let Some(popup) = info.popup.take() {
            _ = DestroyWindow(popup);
        }
    }
}

extern "system" fn subclass_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    unsafe {
        let raw = GetPropW(window, TOOLTIP_PROP).0 as *mut ToolInfo;
        if raw.is_null() {
            return DefWindowProcW(window, message, w_param, l_param);
        }
        let info = &mut *raw;
        let old = info.old_proc;

        match message {
            WM_MOUSEMOVE => {
                if info.popup.is_none() && !info.hover_pending {
                    info.hover_pending = true;
                    _ = SetTimer(Some(window), HOVER_TIMER_ID, HOVER_DELAY_MS, None);
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: window,
                        dwHoverTime: 0,
                    };
                    _ = TrackMouseEvent(&mut tme);
                }
                CallWindowProcW(old, window, message, w_param, l_param)
            }
            WM_TIMER if w_param.0 == HOVER_TIMER_ID => {
                _ = KillTimer(Some(window), HOVER_TIMER_ID);
                info.hover_pending = false;
                if info.popup.is_none() {
                    info.popup = show_tooltip(&info.qt, window, info.text).ok();
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_KILLFOCUS => {
                hide(info, window);
                CallWindowProcW(old, window, message, w_param, l_param)
            }
            WM_NCDESTROY => {
                hide(info, window);
                // Restore the original proc, drop our prop + state before the
                // window fully dies.
                SetWindowLongPtrW(window, GWLP_WNDPROC, transmute::<WNDPROC, isize>(old));
                _ = RemovePropW(window, TOOLTIP_PROP);
                let result = CallWindowProcW(old, window, message, w_param, l_param);
                drop(Box::from_raw(raw));
                result
            }
            _ => CallWindowProcW(old, window, message, w_param, l_param),
        }
    }
}

// ---------------------------------------------------------------------------
// B. The popup window (menu's shell + fade, but non-modal/passive)
// ---------------------------------------------------------------------------

struct CreateParams {
    qt: QT,
    text: PCWSTR,
}

struct PopupContext {
    qt: QT,
    text: PCWSTR,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    fade_elapsed_ms: Cell<u32>,
}

/// Measure the text, size + position the popup near `owner` (clamped to the
/// monitor), create it, and start the fade. Returns the popup HWND.
fn show_tooltip(qt: &QT, owner: HWND, text: PCWSTR) -> Result<HWND> {
    unsafe {
        let tokens = &qt.theme.tokens;
        let pad_x = tokens.spacing_horizontal_m; // 12
        let pad_y = tokens.spacing_vertical_s_nudge; // 6

        // Measure the wrapped text (maxWidth 216 DIP).
        let text_format = create_text_format(qt)?;
        let layout =
            qt.dwrite_factory
                .CreateTextLayout(text.as_wide(), &text_format, MAX_TEXT_WIDTH, f32::MAX)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;

        let scaling = get_scaling_factor(owner);
        let w_dip = metrics.width.ceil() + 2.0 * pad_x;
        let h_dip = metrics.height.ceil() + 2.0 * pad_y;
        let w = (w_dip * scaling).ceil() as i32;
        let h = (h_dip * scaling).ceil() as i32;

        // Anchor below the owner, centred; flip above / clamp to the work area.
        let mut orc = RECT::default();
        GetWindowRect(owner, &mut orc)?;
        let gap = (4.0 * scaling) as i32;
        let mut x = orc.left + (orc.right - orc.left - w) / 2;
        let mut y = orc.bottom + gap;

        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        _ = GetMonitorInfoW(monitor, &mut info);
        if y + h > info.rcWork.bottom {
            y = orc.top - h - gap; // flip above
        }
        if y < info.rcWork.top {
            y = info.rcWork.top;
        }
        if x + w > info.rcWork.right {
            x = info.rcWork.right - w;
        }
        if x < info.rcWork.left {
            x = info.rcWork.left;
        }

        let boxed = Box::new(CreateParams {
            qt: qt.clone(),
            text,
        });
        let popup = CreateWindowExW(
            WS_EX_LAYERED,
            POPUP_CLASS,
            w!(""),
            WS_POPUP,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(HINSTANCE(GetWindowLongPtrW(owner, GWLP_HINSTANCE) as _)),
            Some(Box::<CreateParams>::into_raw(boxed) as _),
        )?;
        Ok(popup)
    }
}

fn popup_on_create(window: HWND, params: CreateParams) -> Result<PopupContext> {
    unsafe {
        // The window is already sized (show_tooltip created it at final w×h), so
        // the render target gets a real pixel size — no 0×0 bug.
        let mut client_rect = RECT::default();
        GetClientRect(window, &mut client_rect)?;
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
                    width: (client_rect.right - client_rect.left) as u32,
                    height: (client_rect.bottom - client_rect.top) as u32,
                },
                presentOptions: Default::default(),
            },
        )?;
        let text_format = create_text_format(&params.qt)?;
        Ok(PopupContext {
            qt: params.qt,
            text: params.text,
            render_target,
            text_format,
            fade_elapsed_ms: Cell::new(0),
        })
    }
}

/// Show topmost (non-activating), round the corners, start the fade — copies
/// menu's show sequence, minus any capture/modal loop.
fn start_show(window: HWND, context: &PopupContext) {
    unsafe {
        let mut rc = RECT::default();
        if GetClientRect(window, &mut rc).is_err() {
            return;
        }
        let scaling = get_scaling_factor(window);
        let corner = (context.qt.theme.tokens.border_radius_medium * 2.0 * scaling) as i32;
        // Fully transparent before it's shown (no flash), then fade in via WM_TIMER.
        _ = SetLayeredWindowAttributes(window, COLORREF(0), 0, LWA_ALPHA);
        _ = SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_SHOWWINDOW | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
        );
        let region = CreateRoundRectRgn(0, 0, rc.right + 1, rc.bottom + 1, corner, corner);
        SetWindowRgn(window, Some(region), false);
        _ = SetTimer(Some(window), FADE_TIMER_ID, FADE_INTERVAL_MS, None);
    }
}

fn popup_paint(window: HWND, context: &PopupContext) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    unsafe {
        context
            .render_target
            .Clear(Some(&tokens.color_neutral_background1));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling = get_scaling_factor(window);
        let width = rc.right as f32 / scaling;
        let height = rc.bottom as f32 / scaling;
        let pad_x = tokens.spacing_horizontal_m;
        let pad_y = tokens.spacing_vertical_s_nudge;

        let brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
        context.render_target.DrawText(
            context.text.as_wide(),
            &context.text_format,
            &D2D_RECT_F {
                left: pad_x,
                top: pad_y,
                right: width - pad_x,
                bottom: height - pad_y,
            },
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
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
            let raw = (*cs).lpCreateParams as *mut CreateParams;
            let params = Box::<CreateParams>::from_raw(raw);
            match popup_on_create(window, *params) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(
                        window,
                        GWLP_USERDATA,
                        Box::<PopupContext>::into_raw(boxed) as _,
                    );
                    let context = &*(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext);
                    start_show(window, context);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            _ = KillTimer(Some(window), FADE_TIMER_ID);
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            if !raw.is_null() {
                _ = Box::<PopupContext>::from_raw(raw);
            }
            LRESULT(0)
        },
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if w_param.0 == FADE_TIMER_ID => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            let context = &*raw;
            let tokens = &context.qt.theme.tokens;
            let duration_ms = (tokens.duration_normal * 1000.0) as u32;
            let elapsed = (context.fade_elapsed_ms.get() + FADE_INTERVAL_MS).min(duration_ms);
            context.fade_elapsed_ms.set(elapsed);
            let t = elapsed as f64 / duration_ms as f64;
            let eased = cubic_bezier(t, tokens.curve_decelerate_mid);
            let alpha = (eased * 255.0).round() as u8;
            _ = SetLayeredWindowAttributes(window, COLORREF(0), alpha, LWA_ALPHA);
            if elapsed >= duration_ms {
                _ = KillTimer(Some(window), FADE_TIMER_ID);
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
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            let context = &*raw;
            _ = popup_on_paint(window, context);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_ERASEBKGND => LRESULT(1),
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
