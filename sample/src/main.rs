use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, PAINTSTRUCT,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use quelthalas::component::button;
use quelthalas::{MouseEvent, QT};

fn main() -> Result<()> {
    unsafe {
        let instance = HINSTANCE::from(GetModuleHandleW(None)?);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;

        //Register the window class
        let class_name = w!("Sample windows class");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: Default::default(),
            lpfnWndProc: Some(window_process),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Use Quel'Thalas"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            instance,
            None,
        );

        ShowWindow(window, SW_SHOWDEFAULT);

        let scaling_factor = GetDpiForWindow(window) / USER_DEFAULT_SCREEN_DPI;

        let qt = QT::new()?;
        qt.creat_button(
            &window,
            &instance,
            20,
            30,
            w!("Rounded"),
            &button::Appearance::Secondary,
            None,
            &button::Shape::Rounded,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;
        qt.creat_button(
            &window,
            &instance,
            20 + 110 * scaling_factor as i32,
            30,
            w!("Circular"),
            &button::Appearance::Secondary,
            None,
            &button::Shape::Circular,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;
        qt.creat_button(
            &window,
            &instance,
            20 + 220 * scaling_factor as i32,
            30,
            w!("Square"),
            &button::Appearance::Secondary,
            None,
            &button::Shape::Square,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;
        qt.creat_button(
            &window,
            &instance,
             20 + 110 * scaling_factor as i32,
            30 + 50 * scaling_factor as i32,
            w!("Primary"),
            &button::Appearance::Primary,
            None,
            &button::Shape::Rounded,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;
        qt.creat_button(
            &window,
            &instance,
            20 + 220 * scaling_factor as i32,
            30 + 50 * scaling_factor as i32,
            w!("Outline"),
            &button::Appearance::Outline,
            None,
            &button::Shape::Rounded,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).into() {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        Ok(())
    }
}

extern "system" fn window_process(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(window, &mut ps);
                FillRect(hdc, &ps.rcPaint, CreateSolidBrush(COLORREF(0xfafafa)));
                EndPaint(window, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, w_param, l_param),
        }
    }
}
