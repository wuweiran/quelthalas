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
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
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
    pub(crate) wic_factory: IWICImagingFactory,
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
        let wic_factory = unsafe {
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?
        };
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
            wic_factory,
            transition_library,
            stroke_style,
        })
    }
}

pub(crate) fn get_scaling_factor(window: HWND) -> f32 {
    unsafe { GetDpiForWindow(window) as f32 / USER_DEFAULT_SCREEN_DPI as f32 }
}

/// Strip the Alt-mnemonic from a Win32 label. We draw our own menus, so the `&`
/// marker must go; on CJK it's a trailing `(&X)` group (`剪切(&T)`), which we drop
/// whole so no stray `(T)` remains.
fn strip_mnemonic(label: &[u16]) -> Vec<u16> {
    const AMP: u16 = b'&' as u16;
    const LPAREN: u16 = b'(' as u16;
    const RPAREN: u16 = b')' as u16;
    let mut out = Vec::with_capacity(label.len() + 1);
    let mut i = 0;
    while i < label.len() {
        let c = label[i];
        if c == LPAREN
            && label.get(i + 1) == Some(&AMP)
            && label.get(i + 3) == Some(&RPAREN)
        {
            i += 4;
            continue;
        }
        if c == AMP {
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out.push(0);
    out
}

/// A localized Edit-control menu label by command id (Cut = 768, Copy = 769,
/// Paste = 770, Select All = 177). These live in a MENU resource (user32 menu #1),
/// not a string table. Returns an owned NUL-terminated buffer; the caller keeps it
/// alive while the menu is on screen.
pub(crate) fn edit_menu_label(command_id: u32, fallback: &str) -> Vec<u16> {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{DestroyMenu, GetMenuStringW, LoadMenuW, MF_BYCOMMAND};
    use windows::core::{PCWSTR, w};

    let owned = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let menu = unsafe { GetModuleHandleW(w!("user32.dll")) }
        .ok()
        .and_then(|m| unsafe { LoadMenuW(Some(m.into()), PCWSTR(1 as *const u16)) }.ok());
    let Some(menu) = menu else {
        return owned(fallback);
    };
    let mut buf = [0u16; 128];
    let len = unsafe { GetMenuStringW(menu, command_id, Some(&mut buf), MF_BYCOMMAND) };
    let label = if len > 0 { strip_mnemonic(&buf[..len as usize]) } else { owned(fallback) };
    let _ = unsafe { DestroyMenu(menu) };
    label
}

/// A localized string from user32's string table by id (e.g. the standard dialog
/// buttons: OK = 800, Cancel = 801, Yes = 805, …). Returns an owned NUL-terminated
/// buffer; the caller owns it and controls its lifetime.
pub(crate) fn system_string(id: u32, fallback: &str) -> Vec<u16> {
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::LoadStringW;
    use windows::core::{PWSTR, w};

    let owned = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let Ok(module) = (unsafe { GetModuleHandleW(w!("user32.dll")) }) else {
        return owned(fallback);
    };
    let mut buf = [0u16; 128];
    let len = unsafe {
        LoadStringW(Some(HINSTANCE(module.0)), id, PWSTR(buf.as_mut_ptr()), buf.len() as i32)
    };
    if len > 0 { strip_mnemonic(&buf[..len as usize]) } else { owned(fallback) }
}

pub mod component;
pub mod icon;
pub mod layout;
mod theme;
