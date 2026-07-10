extern crate self as qt;

use std::rc::Rc;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_STROKE_STYLE_PROPERTIES1, ID2D1Factory1, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWriteCreateFactory, IDWriteFactory,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Animation::{IUIAnimationTransitionLibrary2, UIAnimationTransitionLibrary2};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::USER_DEFAULT_SCREEN_DPI;
use windows::core::{Interface, Result};

pub use crate::theme::Theme;
pub use crate::theme::Tokens;

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
    pub(crate) d2d_factory: ID2D1Factory1,
    pub(crate) dwrite_factory: IDWriteFactory,
    pub(crate) transition_library: IUIAnimationTransitionLibrary2,
    pub(crate) stroke_style: ID2D1StrokeStyle,
}

impl QT {
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn new() -> Result<Self> {
        Self::new_with(Theme::web_light())
    }

    pub fn new_with(theme: Theme) -> Result<Self> {
        let d2d_factory = unsafe {
            D2D1CreateFactory::<ID2D1Factory1>(
                D2D1_FACTORY_TYPE_SINGLE_THREADED,
                Some(&D2D1_FACTORY_OPTIONS::default()),
            )?
        };
        let dwrite_factory =
            unsafe { DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)? };
        let transition_library = unsafe {
            CoCreateInstance(&UIAnimationTransitionLibrary2, None, CLSCTX_INPROC_SERVER)?
        };
        let stroke_style = unsafe {
            d2d_factory
                .CreateStrokeStyle(&D2D1_STROKE_STYLE_PROPERTIES1::default(), None)?
                .cast::<ID2D1StrokeStyle>()?
        };
        Ok(QT {
            theme: Rc::new(theme),
            d2d_factory,
            dwrite_factory,
            transition_library,
            stroke_style,
        })
    }
}

pub(crate) fn get_scaling_factor(window: HWND) -> f32 {
    unsafe { GetDpiForWindow(window) as f32 / USER_DEFAULT_SCREEN_DPI as f32 }
}

pub mod component;
pub mod icon;
pub mod layout;
mod theme;
