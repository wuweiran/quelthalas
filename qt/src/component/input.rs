use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, CS_CLASSDC, CS_DBLCLKS, DefWindowProcW, IDC_ARROW, IDC_IBEAM, LoadCursorW, RegisterClassExW, WINDOW_EX_STYLE, WNDCLASSEXW, WS_CHILD, WS_TABSTOP, WS_VISIBLE};
use crate::{get_scaling_factor, QT};

#[derive(Copy, Clone)]
pub enum Size {
    Small,
    Medium,
    Large,
}

#[derive(Copy, Clone)]
pub enum Appearance {
    Outline,
    FilledLighter,
    FilledDarker,
}

#[derive(Copy, Clone)]
pub enum Type {
    Number,
    Text,
    Password,
}

pub struct State {
    qt: QT,
    size: Size,
    appearance: Appearance,
    default_value: Option<PCWSTR>,
    input_type: Type,
    placeholder: Option<PCWSTR>
}

impl QT {
    pub fn creat_input(
        &self,
        parent_window: &HWND,
        instance: &HINSTANCE,
        x: i32,
        y: i32,
        size: &Size,
        appearance: &Appearance,
        default_value: Option<PCWSTR>,
        input_type: &Type,
        placeholder: Option<PCWSTR>
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_INPUT");
        unsafe {
            let window_class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpszClassName: class_name,
                style: CS_CLASSDC | CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                hCursor: LoadCursorW(None, IDC_IBEAM)?,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
            let boxed = Box::new(State {
                qt: self.clone(),
                size: *size,
                appearance: *appearance,
                default_value,
                input_type: *input_type,
                placeholder
            });
            let scaling_factor = get_scaling_factor(parent_window);
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (380f32 * scaling_factor) as i32,
                (32f32 * scaling_factor) as i32,
                *parent_window,
                None,
                *instance,
                Some(Box::<State>::into_raw(boxed) as _),
            );
            Ok(window)
        }
    }
}
extern "system" fn window_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(window, message, w_param, l_param) }
}
