extern crate self as qt;

use std::rc::Rc;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::USER_DEFAULT_SCREEN_DPI;

use crate::theme::Theme;

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

#[derive(Clone)]
pub struct QT {
    theme: Rc<Theme>,
}

impl QT {
    pub fn default() -> Self {
        QT {
            theme: Rc::new(Theme::web_light()),
        }
    }
}

pub(crate) fn get_scaling_factor(window: &HWND) -> f32 {
    unsafe { GetDpiForWindow(*window) as f32 / USER_DEFAULT_SCREEN_DPI as f32 }
}

pub mod component;
pub mod icon;
mod theme;
