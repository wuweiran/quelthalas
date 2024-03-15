extern crate self as qt;

use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;

use qt::theme::Tokens;

pub struct MouseEvent {
    on_click: fn(),
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent { on_click: || {} }
    }
}

pub struct QT {
    tokens: Tokens,
}

impl QT {
    pub fn new() -> Result<Self> {
        Ok(QT {
            tokens: Tokens::web_light(),
        })
    }
}

pub(crate) fn get_scaling_factor(window: &HWND) -> f32 {
    unsafe { GetDpiForWindow(*window) as f32 / 96.0f32 }
}

pub mod component;
mod theme;
