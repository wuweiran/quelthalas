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
use quelthalas::component::menu::MenuInfo;
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
            Some(instance),
            None,
        )?;

        let _ = ShowWindow(window, SW_SHOW);

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).into() {
            let _ = TranslateMessage(&message);
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
                let scaling_factor = GetDpiForWindow(window) / USER_DEFAULT_SCREEN_DPI;
                let icon = Icon::calendar_month_regular();

                _ = qt.create_button(
                    window,
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
                _ = qt.create_button(
                    window,
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
                _ = qt.create_button(
                    window,
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
                _ = qt.create_button(
                    window,
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
                _ = qt.create_button(
                    window,
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
                _ = qt.create_button(
                    window,
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
                _ = qt.create_button(
                    window,
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
                    window,
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
                    window,
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
                    window,
                    20,
                    30 + 250 * scaling_factor as i32,
                    380 * scaling_factor as i32,
                    &input::Size::Small,
                    &input::Appearance::Outline,
                    None,
                    &input::Type::Password,
                    Some(w!("Small with placeholder")),
                );
                _ = qt.create_progress_bar(
                    window,
                    20,
                    30 + 300 * scaling_factor as i32,
                    400 * scaling_factor as i32,
                    &progress_bar::Shape::Rounded,
                    None,
                    None,
                    &progress_bar::Thickness::Medium,
                );
                _ = qt.create_progress_bar(
                    window,
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
                match qt.open_dialog(
                    window,
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
                _ = EndPaint(window, &ps);
                LRESULT(0)
            }
            WM_CONTEXTMENU => {
                let x = l_param.0 as i16 as i32;
                let y = (l_param.0 >> 16) as i16 as i32;

                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const QT;
                let qt = &*raw;
                let menu_list = vec![
                    MenuInfo::MenuItem {
                        text: w!("New"),
                        command_id: 0,
                        disabled: false,
                    },
                    MenuInfo::MenuItem {
                        text: w!("New window"),
                        command_id: 1,
                        disabled: false,
                    },
                    MenuInfo::MenuItem {
                        text: w!("Open file"),
                        command_id: 2,
                        disabled: true,
                    },
                    MenuInfo::MenuDivider,
                    MenuInfo::SubMenu {
                        text: w!("Preferences"),
                        menu_list: vec![
                            MenuInfo::MenuItem {
                                text: w!("Settings"),
                                command_id: 30,
                                disabled: false,
                            },
                            MenuInfo::MenuItem {
                                text: w!("Online services settings"),
                                command_id: 31,
                                disabled: false,
                            },
                            MenuInfo::MenuDivider,
                            MenuInfo::MenuItem {
                                text: w!("Extensions"),
                                command_id: 32,
                                disabled: false,
                            },
                            MenuInfo::SubMenu {
                                text: w!("Appearance"),
                                menu_list: vec![
                                    MenuInfo::MenuItem {
                                        text: w!("Centered layout"),
                                        command_id: 30,
                                        disabled: false,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zen"),
                                        command_id: 31,
                                        disabled: false,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zoom in"),
                                        command_id: 32,
                                        disabled: true,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zoom out"),
                                        command_id: 33,
                                        disabled: false,
                                    },
                                ],
                            },
                        ],
                    },
                ];
                _ = qt.open_menu(window, menu_list, x, y);
                LRESULT::default()
            }
            _ => DefWindowProcW(window, message, w_param, l_param),
        }
    }
}
