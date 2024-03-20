extern crate self as qt;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::USER_DEFAULT_SCREEN_DPI;

use qt::theme::Tokens;

pub struct MouseEvent {
    pub on_click: Box<dyn Fn(&HWND)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_click: Box::new(|_window| {}),
        }
    }
}

pub struct QT {
    tokens: Tokens,
}

impl QT {
    pub fn new() -> Self {
        QT {
            tokens: Tokens::web_light(),
        }
    }
}

pub(crate) fn get_scaling_factor(window: &HWND) -> f32 {
    unsafe { GetDpiForWindow(*window) as f32 / USER_DEFAULT_SCREEN_DPI as f32 }
}

pub mod component;
pub mod icon;
mod theme;
