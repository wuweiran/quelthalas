use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::component::button;
use crate::{get_scaling_factor, MouseEvent, QT};

#[derive(Copy, Clone)]
pub enum DialogResult {
    OK,
    Cancel,
    Close,
}

struct State {
    qt_ptr: *const QT,
    body: PCWSTR,
    content: PCWSTR,
}

struct Context {
    state: State,
    result: DialogResult,
}
impl QT {
    pub fn open_dialog(
        &self,
        parent_window: &HWND,
        instance: &HINSTANCE,
        window_title: PCWSTR,
        body: PCWSTR,
        content: PCWSTR,
    ) -> Result<DialogResult> {
        let class_name: PCWSTR = w!("QT_DIALOG");
        unsafe {
            let window_class: WNDCLASSEXW = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpszClassName: class_name,
                style: CS_OWNDC,
                lpfnWndProc: Some(window_proc),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
            let scaling_factor = get_scaling_factor(parent_window);
            EnableWindow(*parent_window, FALSE);
            let boxed = Box::new(State {
                qt_ptr: self as *const Self,
                body,
                content,
            });
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                window_title,
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (600f32 * scaling_factor) as i32,
                400,
                *parent_window,
                None,
                *instance,
                Some(Box::<State>::into_raw(boxed) as _),
            );

            self.creat_button(
                &window,
                &instance,
                20,
                30,
                w!("OK"),
                &button::Appearance::Primary,
                None,
                None,
                &button::Shape::Rounded,
                &button::Size::Medium,
                MouseEvent {
                    on_click: Box::new(move |_| {
                        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                        (*raw).result = DialogResult::OK;
                        _ = PostMessageW(window, WM_USER, WPARAM(0), LPARAM(0));
                    }),
                },
            )?;
            self.creat_button(
                &window,
                &instance,
                20 + 110 * scaling_factor as i32,
                30,
                w!("Cancel"),
                &button::Appearance::Secondary,
                None,
                None,
                &button::Shape::Rounded,
                &button::Size::Medium,
                MouseEvent {
                    on_click: Box::new(move |_| {
                        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                        (*raw).result = DialogResult::Cancel;
                        _ = PostMessageW(window, WM_USER, WPARAM(0), LPARAM(0));
                    }),
                },
            )?;
            ShowWindow(window, SW_SHOWDEFAULT);

            let mut message = MSG::default();
            let mut result = DialogResult::Cancel;
            while GetMessageW(&mut message, None, 0, 0).into() {
                if message.message == WM_USER {
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &*raw;
                    result = context.result;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
                let window_exists: bool = IsWindow(window).into();
                if !window_exists {
                    break;
                }
            }
            EnableWindow(*parent_window, TRUE);
            SetActiveWindow(*parent_window);
            Ok(result)
        }
    }
}

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    Ok(Context {
        state,
        result: DialogResult::Close,
    })
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut State;
            let state = Box::<State>::from_raw(raw);
            match on_create(window, *state) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    DefWindowProcW(window, message, w_param, l_param)
                }
                Err(_) => {
                    _ = DestroyWindow(window);
                    LRESULT(-1)
                }
            }
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
