use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::component::button;
use crate::sys::{adjust_window_rect_ex_for_dpi, dpi_for_window};
use crate::{MouseEvent, QT, get_scaling_factor};

#[derive(Copy, Clone)]
pub enum DialogResult {
    OK,
    Cancel,
    Close,
}

pub enum ModelType {
    Modal,
    Alert,
}

// user32 string-table ids for the standard buttons (same ids MB_GetString uses).
const IDS_OK: u32 = 800;
const IDS_CANCEL: u32 = 801;

/// Which action buttons a dialog shows.
#[derive(Copy, Clone)]
pub enum Actions {
    /// A primary "OK" and a secondary "Cancel".
    OkCancel,
    /// A single primary "OK" that just dismisses the dialog.
    Ok,
}

struct State {
    qt: QT,
    title: PCWSTR,
    content: PCWSTR,
    actions: Actions,
}

struct Context {
    state: State,
    result: DialogResult,
    title_text_format: IDWriteTextFormat,
    content_text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    ok_button: HWND,
    cancel_button: Option<HWND>,
    // Owned button-label buffers the child buttons' `PCWSTR`s point into; kept
    // alive for the dialog's lifetime (the buttons read them live, never copy).
    _button_labels: Vec<Vec<u16>>,
}
impl QT {
    pub fn open_dialog(
        &self,
        parent_window: HWND,
        title: PCWSTR,
        content: PCWSTR,
        modal_type: &ModelType,
        actions: Actions,
    ) -> Result<DialogResult> {
        let class_name: PCWSTR = w!("QT_DIALOG");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_OWNDC,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let scaling_factor = get_scaling_factor(parent_window);
            _ = EnableWindow(parent_window, false);
            let boxed = Box::new(State {
                qt: self.clone(),
                title,
                content,
                actions,
            });
            let window_style = match modal_type {
                ModelType::Modal => WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                ModelType::Alert => WS_OVERLAPPED | WS_DLGFRAME,
            };
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                title,
                window_style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (600f32 * scaling_factor) as i32,
                (400f32 * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(
                    GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _
                )),
                Some(Box::<State>::into_raw(boxed) as _),
            )?;

            _ = ShowWindow(window, SW_SHOW);

            let mut message = MSG::default();
            let mut result = DialogResult::Cancel;
            while GetMessageW(&mut message, None, 0, 0).into() {
                if message.message == WM_USER {
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &*raw;
                    result = context.result;
                    // Re-enable + reactivate the parent BEFORE WM_USER dispatches
                    // to DestroyWindow. If the parent is still disabled when the
                    // dialog is destroyed, the system transfers activation to some
                    // other window and bounces it back — the close flicker.
                    _ = EnableWindow(parent_window, true);
                    _ = SetActiveWindow(parent_window);
                }
                _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                if !IsWindow(Some(window)).as_bool() {
                    break;
                }
            }
            _ = EnableWindow(parent_window, true);
            _ = SetActiveWindow(parent_window);
            Ok(result)
        }
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let qt = &state.qt;
    unsafe {
        let direct_write_factory = &qt.dwrite_factory;
        let title_typo = &qt.theme.typography_styles.subtitle1;
        let title_text_format = title_typo.create_text_format(&direct_write_factory)?;
        let content_typo = &qt.theme.typography_styles.body1;
        let content_text_format = content_typo.create_text_format(&direct_write_factory)?;

        let factory = &qt.d2d_factory;
        let dpi = dpi_for_window(window);
        let render_target = factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U {
                    width: 600u32,
                    height: 400u32,
                },
                presentOptions: Default::default(),
            },
        )?;

        // Owned label buffers; moved into Context below to outlive the buttons.
        let ok_label = crate::system_string(IDS_OK, "OK");
        let cancel_label = crate::system_string(IDS_CANCEL, "Cancel");

        // OkCancel adds a "Cancel"; both use "OK" as the primary.
        let ok_button = qt.create_button(
            window,
            0,
            0,
            button::Props {
                text: PCWSTR::from_raw(ok_label.as_ptr()),
                appearance: button::Appearance::Primary,
                mouse_event: MouseEvent {
                    on_click: Box::new(move |_| {
                        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                        (*raw).result = DialogResult::OK;
                        _ = PostMessageW(Some(window), WM_USER, WPARAM(0), LPARAM(0));
                    }),
                },
                ..Default::default()
            },
        )?;
        let cancel_button = match state.actions {
            Actions::OkCancel => Some(qt.create_button(
                window,
                0,
                0,
                button::Props {
                    text: PCWSTR::from_raw(cancel_label.as_ptr()),
                    mouse_event: MouseEvent {
                        on_click: Box::new(move |_| {
                            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                            (*raw).result = DialogResult::Cancel;
                            _ = PostMessageW(Some(window), WM_USER, WPARAM(0), LPARAM(0));
                        }),
                    },
                    ..Default::default()
                },
            )?),
            Actions::Ok => None,
        };
        Ok(Context {
            state,
            title_text_format,
            content_text_format,
            render_target,
            result: DialogResult::Close,
            ok_button,
            cancel_button,
            _button_labels: vec![ok_label, cancel_label],
        })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);

    unsafe {
        let mut button_rect = RECT::default();
        // The secondary (Cancel) button is only present in OkCancel mode.
        let (cancel_button_width, cancel_button_height) = match context.cancel_button {
            Some(cancel) => {
                GetClientRect(cancel, &mut button_rect)?;
                (
                    button_rect.right - button_rect.left,
                    button_rect.bottom - button_rect.top,
                )
            }
            None => (0, 0),
        };
        GetClientRect(context.ok_button, &mut button_rect)?;
        let ok_button_width = button_rect.right - button_rect.left;
        let ok_button_height = button_rect.bottom - button_rect.top;

        let surface_padding = 24f32;
        let gap = 8f32;

        let state = &context.state;
        let direct_write_factory = &state.qt.dwrite_factory;
        let title_text_layout = direct_write_factory.CreateTextLayout(
            state.title.as_wide(),
            &context.title_text_format,
            600f32 - 24f32 - 24f32,
            1000f32,
        )?;
        let mut title_metrics = DWRITE_TEXT_METRICS::default();
        title_text_layout.GetMetrics(&mut title_metrics)?;
        let content_text_layout = direct_write_factory.CreateTextLayout(
            state.content.as_wide(),
            &context.content_text_format,
            600f32 - 24f32 - 24f32,
            1000f32,
        )?;
        let mut content_metrics = DWRITE_TEXT_METRICS::default();
        content_text_layout.GetMetrics(&mut content_metrics)?;

        // Width fits the title/content text (capped at 600), but must also be
        // wide enough for the two buttons + their gaps, or they'd overflow the
        // left edge when the text is short (button widths are already device px).
        let text_width = (((surface_padding * 2f32 + title_metrics.width)
            .max(surface_padding * 2f32 + content_metrics.width)
            .min(600f32))
            * scaling_factor)
            .ceil() as i32;
        // Buttons sit right-aligned to the 24px padding; when a Cancel button is
        // present they're separated by `gap` and Cancel takes the rightmost slot.
        // In Close mode cancel_button_width is 0 and there's no inter-button gap.
        let inter_button_gap = if context.cancel_button.is_some() {
            gap
        } else {
            0f32
        };
        let buttons_min_width = cancel_button_width
            + ok_button_width
            + ((surface_padding * 2f32 + inter_button_gap) * scaling_factor).ceil() as i32;
        let scaled_width = text_width.max(buttons_min_width);
        let buttons_top =
            surface_padding + title_metrics.height + gap + content_metrics.height + gap;
        let scaled_height = ((buttons_top + surface_padding) * scaling_factor).ceil() as i32
            + ok_button_height.max(cancel_button_height);

        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scaled_width,
            bottom: scaled_height,
        };
        adjust_window_rect_ex_for_dpi(
            &mut rect,
            WINDOW_STYLE(GetWindowLongPtrW(window, GWL_STYLE) as u32),
            false,
            WINDOW_EX_STYLE(GetWindowLongPtrW(window, GWL_EXSTYLE) as u32),
            dpi_for_window(window),
        )?;
        let window_width = rect.right - rect.left;
        let window_height = rect.bottom - rect.top;
        // Center over the owner window (the app), falling back to the screen. For an
        // owned top-level window GA_PARENT returns the desktop, so use GW_OWNER.
        let owner = GetWindow(window, GW_OWNER).unwrap_or_else(|_| GetDesktopWindow());
        GetWindowRect(owner, &mut rect)?;
        SetWindowPos(
            window,
            None,
            rect.left / 2 + rect.right / 2 - window_width / 2,
            rect.top / 2 + rect.bottom / 2 - window_height / 2,
            window_width,
            window_height,
            SWP_NOZORDER,
        )?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;
        // Right-align the button row to the 24px padding. Cancel (when present)
        // takes the rightmost slot; the primary button sits to its left. In Close
        // mode there's no Cancel, so the primary button takes the rightmost slot.
        let ok_right_offset = match context.cancel_button {
            Some(cancel) => {
                MoveWindow(
                    cancel,
                    scaled_width - (cancel_button_width + (24f32 * scaling_factor) as i32),
                    (buttons_top * scaling_factor) as i32,
                    cancel_button_width,
                    cancel_button_height,
                    false,
                )?;
                cancel_button_width + ok_button_width + (32f32 * scaling_factor) as i32
            }
            None => ok_button_width + (24f32 * scaling_factor) as i32,
        };
        MoveWindow(
            context.ok_button,
            scaled_width - ok_right_offset,
            (buttons_top * scaling_factor) as i32,
            ok_button_width,
            ok_button_height,
            false,
        )?;
    }

    Ok(())
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let mut window_rect = RECT::default();
        GetClientRect(window, &mut window_rect)?;
        let scaling_factor = get_scaling_factor(window);
        let width = (window_rect.right - window_rect.left) as f32 / scaling_factor;
        let height = (window_rect.bottom - window_rect.top) as f32 / scaling_factor;
        let text_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
        context.render_target.DrawText(
            state.title.as_wide(),
            &context.title_text_format,
            &D2D_RECT_F {
                left: 24f32,
                top: 24f32,
                right: width - 24f32,
                bottom: height - 24f32,
            },
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );

        let direct_write_factory = &state.qt.dwrite_factory;
        let title_text_layout = direct_write_factory.CreateTextLayout(
            state.title.as_wide(),
            &context.title_text_format,
            width - 24f32 - 24f32,
            height - 24f32 - 24f32,
        )?;
        let mut title_metrics = DWRITE_TEXT_METRICS::default();
        title_text_layout.GetMetrics(&mut title_metrics)?;
        context.render_target.DrawText(
            state.content.as_wide(),
            &context.content_text_format,
            &D2D_RECT_F {
                left: 24f32,
                top: 24f32 + title_metrics.height + 8f32,
                right: width - 24f32,
                bottom: height - 24f32,
            },
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
    Ok(())
}

fn on_paint(window: HWND, context: &Context) -> Result<()> {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        BeginPaint(window, &mut ps);
        context.render_target.BeginDraw();
        context.render_target.Clear(Some(
            &context.state.qt.theme.tokens.color_neutral_background1,
        ));

        let result = paint(window, context).and(context.render_target.EndDraw(None, None));
        _ = EndPaint(window, &ps);
        result
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
                Ok(context) => {
                    _ = layout(window, &context);
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    DefWindowProcW(window, message, w_param, l_param)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = on_paint(window, context);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_GETDPISCALEDSIZE => LRESULT(TRUE.0 as isize),
        WM_DPICHANGED => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let new_dpi_x = w_param.0 as i16 as f32;
            let new_dpi_y = (w_param.0 >> 16) as i16 as f32;
            context.render_target.SetDpi(new_dpi_x, new_dpi_y);
            _ = layout(window, &context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(TRUE.0 as isize)
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        WM_USER => unsafe {
            _ = DestroyWindow(window);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
