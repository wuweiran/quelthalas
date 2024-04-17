//#![windows_subsystem = "windows"]
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

use quelthalas::component::button::IconPosition;
use quelthalas::component::dialog::DialogResult;
use quelthalas::component::{button, dialog, input, progress_bar};
use quelthalas::icon::Icon;
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

        ShowWindow(window, SW_SHOW);

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
            WM_CREATE => {
                let qt = QT::default();
                let instance = HINSTANCE(GetWindowLongPtrW(window, GWLP_HINSTANCE));
                let scaling_factor = GetDpiForWindow(window) / USER_DEFAULT_SCREEN_DPI;
                let icon = Icon::calendar_month_regular();

                _ = qt.creat_button(
                    &window,
                    &instance,
                    20,
                    30,
                    w!("Rounded"),
                    &button::Appearance::Secondary,
                    None,
                    None,
                    &button::Shape::Rounded,
                    &button::Size::Medium,
                    MouseEvent::default(),
                );
                _ = qt.creat_button(
                    &window,
                    &instance,
                    20 + 110 * scaling_factor as i32,
                    30,
                    w!("Circular"),
                    &button::Appearance::Secondary,
                    None,
                    None,
                    &button::Shape::Circular,
                    &button::Size::Medium,
                    MouseEvent::default(),
                );
                _ = qt.creat_button(
                    &window,
                    &instance,
                    20 + 220 * scaling_factor as i32,
                    30,
                    w!("Square"),
                    &button::Appearance::Secondary,
                    None,
                    None,
                    &button::Shape::Square,
                    &button::Size::Medium,
                    MouseEvent::default(),
                );
                _ = qt.creat_button(
                    &window,
                    &instance,
                    20 + 330 * scaling_factor as i32,
                    30,
                    w!("Primary"),
                    &button::Appearance::Primary,
                    Some(&icon),
                    None,
                    &button::Shape::Rounded,
                    &button::Size::Medium,
                    MouseEvent::default(),
                );
                _ = qt.creat_button(
                    &window,
                    &instance,
                    20,
                    30 + 50 * scaling_factor as i32,
                    w!("Small with calender icon"),
                    &button::Appearance::Secondary,
                    Some(&icon),
                    None,
                    &button::Shape::Rounded,
                    &button::Size::Small,
                    MouseEvent::default(),
                );
                _ = qt.creat_button(
                    &window,
                    &instance,
                    20,
                    30 + 100 * scaling_factor as i32,
                    w!("With calendar icon after contents"),
                    &button::Appearance::Secondary,
                    Some(&icon),
                    Some(&IconPosition::After),
                    &button::Shape::Rounded,
                    &button::Size::Medium,
                    MouseEvent::default(),
                );
                _ = qt.creat_button(
                    &window,
                    &instance,
                    20,
                    30 + 150 * scaling_factor as i32,
                    w!("Large with calender icon"),
                    &button::Appearance::Secondary,
                    Some(&icon),
                    None,
                    &button::Shape::Rounded,
                    &button::Size::Large,
                    MouseEvent::default(),
                );
                _ = qt.create_input(
                    &window,
                    &instance,
                    20,
                    30 + 200 * scaling_factor as i32,
                    200 * scaling_factor as i32,
                    &input::Size::Medium,
                    &input::Appearance::Outline,
                    Some(w!("Default text")),
                    &input::Type::Text,
                    None,
                );
                _ = qt.create_input(
                    &window,
                    &instance,
                    20 + 220 * scaling_factor as i32,
                    30 + 200 * scaling_factor as i32,
                    200 * scaling_factor as i32,
                    &input::Size::Medium,
                    &input::Appearance::FilledLighter,
                    Some(w!("Filled lighter")),
                    &input::Type::Text,
                    None,
                );
                _ = qt.create_input(
                    &window,
                    &instance,
                    20,
                    30 + 250 * scaling_factor as i32,
                    380 * scaling_factor as i32,
                    &input::Size::Small,
                    &input::Appearance::Outline,
                    None,
                    &input::Type::Text,
                    Some(w!("Small with placeholder")),
                );
                _ = qt.create_progress_bar(
                    &window,
                    &instance,
                    20,
                    30 + 300 * scaling_factor as i32,
                    400 * scaling_factor as i32,
                    &progress_bar::Shape::Rounded,
                    None,
                    None,
                    &progress_bar::Thickness::Medium,
                );
                _ = qt.create_progress_bar(
                    &window,
                    &instance,
                    20,
                    30 + 325 * scaling_factor as i32,
                    400 * scaling_factor as i32,
                    &progress_bar::Shape::Rounded,
                    Some(0.4),
                    None,
                    &progress_bar::Thickness::Large,
                );
                SetWindowLongPtrW(
                    window,
                    GWLP_USERDATA,
                    Box::<QT>::into_raw(Box::from(qt)) as _,
                );
                DefWindowProcW(window, message, w_param, l_param)
            }
            WM_CLOSE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const QT;
                let qt = &*raw;
                let instance = HINSTANCE(GetWindowLongPtrW(window, GWLP_HINSTANCE));
                match qt.open_dialog(
                    &window,
                    &instance,
                    w!("Dialog title"),
                    w!("Lorem ipsum dolor sit amet consectetur adipisicing elit. Quisquam exercitationem cumque repellendus eaque est dolor eius expedita nulla ullam? Tenetur reprehenderit aut voluptatum impedit voluptates in natus iure cumque eaque?"),
                    &dialog::ModelType::Alert
                ) {
                    Ok(result) => {
                        if let DialogResult::OK = result {
                            _ = DestroyWindow(window);
                        }
                        LRESULT(0)
                    }
                    Err(_) => LRESULT(-1),
                }
            }
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
