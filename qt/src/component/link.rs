use std::mem::size_of;
use std::sync::Once;

use crate::{MouseEvent, QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE,
    IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use crate::sys::dpi_for_window;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

pub struct Props {
    pub text: PCWSTR,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            text: w!(""),
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
    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base300
    }
    fn line_height(&self) -> f32 {
        self.qt.theme.tokens.line_height_base300
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    /// Cached layout, so the underline uses the font's own metrics (sits just
    /// under the baseline, like the browser) rather than a hand-drawn line.
    text_layout: Option<IDWriteTextLayout>,
    hovered: bool,
    pressed: bool,
    is_focused: bool,
}

impl QT {
    pub fn create_link(&self, parent_window: HWND, x: i32, y: i32, props: Props) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_LINK");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_CLASSDC,
                    lpfnWndProc: Some(window_proc),
                    // The one control where the hand cursor is the correct Win32
                    // choice (SysLink shows it too).
                    hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
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
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
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

        let dpi = dpi_for_window(window);
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
        Ok(Context {
            state,
            text_format,
            render_target,
            text_layout: None,
            hovered: false,
            pressed: false,
            is_focused: false,
        })
    }
}

/// Auto-size the window to the measured text (single line); record the glyph width.
fn layout(window: HWND, context: &mut Context) -> Result<()> {
    let state = &context.state;
    unsafe {
        let text_layout = state.qt.dwrite_factory.CreateTextLayout(
            state.props.text.as_wide(),
            &context.text_format,
            f32::MAX,
            f32::MAX,
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        text_layout.GetMetrics(&mut metrics)?;

        let scaling_factor = get_scaling_factor(window);
        let width = metrics.width.ceil().max(1.0);
        let height = state.line_height().max(metrics.height.ceil());
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
        context.text_layout = Some(text_layout);
    }
    Ok(())
}

fn paint(_window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let background = state
            .props
            .background
            .unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let Some(text_layout) = &context.text_layout else {
            return Ok(());
        };

        // Fluent link colour by state.
        let color = if context.pressed {
            &tokens.color_brand_foreground_link_pressed
        } else if context.hovered {
            &tokens.color_brand_foreground_link_hover
        } else {
            &tokens.color_brand_foreground_link
        };
        let brush = context.render_target.CreateSolidColorBrush(color, None)?;

        // Underline on hover / press / focus — via the layout so it uses the
        // font's own underline metrics (just under the baseline, like the web),
        // not a hand-drawn line at the bottom of the line box.
        let underline = context.hovered || context.pressed || context.is_focused;
        let range = DWRITE_TEXT_RANGE {
            startPosition: 0,
            length: state.props.text.as_wide().len() as u32,
        };
        text_layout.SetUnderline(underline, range)?;
        context.render_target.DrawTextLayout(
            Vector2 { X: 0.0, Y: 0.0 },
            text_layout,
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
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
                Ok(mut context) => {
                    _ = layout(window, &mut context);
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
            _ = SetFocus(Some(window));
            context.pressed = true;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if context.pressed {
                context.pressed = false;
                _ = InvalidateRect(Some(window), None, false);
                (context.state.props.mouse_event.on_click)(&window);
            }
            LRESULT(0)
        },
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = true;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = false;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT(DLGC_WANTCHARS as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_RETURN | VK_SPACE => {
                    (context.state.props.mouse_event.on_click)(&window);
                    LRESULT(0)
                }
                _ => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
