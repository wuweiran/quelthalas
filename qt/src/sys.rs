//! Load-safe wrappers for per-monitor DPI APIs.
//!
//! `GetDpiForWindow` and `AdjustWindowRectExForDpi` are Windows 10 (1607) exports.
//! Calling them *directly* bakes their names into the executable's import table, so
//! the loader resolves them **before `main` runs** — on Windows 7 that is a hard
//! "entry point not found" launch failure, no matter how carefully we guard the
//! call site with a runtime version check.
//!
//! To stay launchable on Windows 7 we never name these symbols in an import: we look
//! them up at runtime with `GetProcAddress` (cached, resolved once). When present
//! (Windows 10+) we call the native API — the modern path is byte-for-byte what it
//! was before. When absent (Windows 7/8) we fall back to system-wide DPI via
//! `GetDeviceCaps(LOGPIXELSX)` and the non-DPI `AdjustWindowRectEx`.
//!
//! This is capability probing (does this machine *have* the API), the load-safe form
//! of an OS-version check — and it only ever changes behavior on the old OS.

use std::sync::OnceLock;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSX, ReleaseDC};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, USER_DEFAULT_SCREEN_DPI, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows::core::{Error, Result, s, w};

type GetDpiForWindowFn = unsafe extern "system" fn(HWND) -> u32;
// The native export takes/returns `BOOL`, which is ABI-identical to `i32`. We own the
// transmuted signature, so we use `i32` directly and avoid depending on the crate's
// `BOOL` path (which moves between windows-rs versions).
type AdjustForDpiFn =
    unsafe extern "system" fn(*mut RECT, u32, i32, u32, u32) -> i32;

/// `user32!GetDpiForWindow`, resolved once at runtime. `None` on Windows 7/8.
fn get_dpi_for_window_fn() -> Option<GetDpiForWindowFn> {
    static CACHE: OnceLock<Option<GetDpiForWindowFn>> = OnceLock::new();
    *CACHE.get_or_init(|| unsafe {
        let module = GetModuleHandleW(w!("user32.dll")).ok()?;
        let proc = GetProcAddress(module, s!("GetDpiForWindow"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, GetDpiForWindowFn>(proc))
    })
}

/// `user32!AdjustWindowRectExForDpi`, resolved once at runtime. `None` on Windows 7/8.
fn adjust_for_dpi_fn() -> Option<AdjustForDpiFn> {
    static CACHE: OnceLock<Option<AdjustForDpiFn>> = OnceLock::new();
    *CACHE.get_or_init(|| unsafe {
        let module = GetModuleHandleW(w!("user32.dll")).ok()?;
        let proc = GetProcAddress(module, s!("AdjustWindowRectExForDpi"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, AdjustForDpiFn>(proc))
    })
}

/// System-wide DPI for `window`'s device, the Windows 7 fallback. Returns the 96-dpi
/// default if the device context can't be queried.
fn system_dpi(window: HWND) -> u32 {
    unsafe {
        let hdc = GetDC(Some(window));
        if hdc.is_invalid() {
            return USER_DEFAULT_SCREEN_DPI;
        }
        let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSX);
        ReleaseDC(Some(window), hdc);
        if dpi > 0 { dpi as u32 } else { USER_DEFAULT_SCREEN_DPI }
    }
}

/// Effective DPI for `window`. Per-monitor on Windows 10+, system-wide on Windows 7/8.
/// Drop-in for `GetDpiForWindow`.
pub(crate) fn dpi_for_window(window: HWND) -> u32 {
    if let Some(f) = get_dpi_for_window_fn() {
        let dpi = unsafe { f(window) };
        if dpi != 0 {
            return dpi;
        }
    }
    system_dpi(window)
}

/// DPI-aware non-client frame adjustment. Uses `AdjustWindowRectExForDpi` on Windows
/// 10+, and the DPI-agnostic `AdjustWindowRectEx` on Windows 7/8. Drop-in for
/// `AdjustWindowRectExForDpi`.
pub(crate) fn adjust_window_rect_ex_for_dpi(
    rect: &mut RECT,
    style: WINDOW_STYLE,
    menu: bool,
    ex_style: WINDOW_EX_STYLE,
    dpi: u32,
) -> Result<()> {
    if let Some(f) = adjust_for_dpi_fn() {
        unsafe {
            if f(rect, style.0, menu as i32, ex_style.0, dpi) != 0 {
                Ok(())
            } else {
                Err(Error::from_thread())
            }
        }
    } else {
        unsafe { AdjustWindowRectEx(rect, style, menu, ex_style) }
    }
}
