extern crate self as qt;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::USER_DEFAULT_SCREEN_DPI;

use crate::theme::TypographyStyles;
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
    typography_styles: TypographyStyles,
}

impl QT {
    pub fn new() -> Self {
        Self::from(Tokens::web_light())
    }

    pub fn from(tokens: Tokens) -> Self {
        let typography_styles = TypographyStyles::from(&tokens);
        QT {
            tokens,
            typography_styles,
        }
    }
}

pub(crate) fn get_scaling_factor(window: &HWND) -> f32 {
    unsafe { GetDpiForWindow(*window) as f32 / USER_DEFAULT_SCREEN_DPI as f32 }
}

pub mod component;
pub mod icon;
mod theme;
