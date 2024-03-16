use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, PAINTSTRUCT,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use quelthalas::component::button;
use quelthalas::{MouseEvent, QT};

fn main() -> Result<()> {
    unsafe {
        let instance = HINSTANCE::from(GetModuleHandleW(None)?);
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        SetProcessDPIAware();

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

        let qt = QT::new()?;
        let appearance = button::Appearance::Secondary;
        qt.creat_button(
            &window,
            &instance,
            20,
            30,
            w!("Example"),
            &appearance,
            None,
            &button::Shape::Square,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;
        qt.creat_button(
            &window,
            &instance,
            50,
            90,
            w!("Hello"),
            &appearance,
            None,
            &button::Shape::Rounded,
            &button::Size::Medium,
            MouseEvent::default(),
        )?;
        qt.creat_button(
            &window,
            &instance,
            80,
            130,
            w!("Hello"),
            &appearance,
            None,
            &button::Shape::Circular,
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
