//#![windows_subsystem = "windows"]
use std::mem::size_of;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, PAINTSTRUCT,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use quelthalas::component::button::IconPosition;
use quelthalas::component::dialog::DialogResult;
use quelthalas::component::menu::MenuInfo;
use quelthalas::component::{button, dialog, input, menu, progress_bar, text};
use quelthalas::icon::Icon;
use quelthalas::layout::Stack;
use quelthalas::{MouseEvent, QT};

struct AppState {
    qt: QT,
    layout: Stack,
}

// Window canvas background (#fafafa). Labels use it so they blend seamlessly.
const CANVAS: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 250.0 / 255.0,
    g: 250.0 / 255.0,
    b: 250.0 / 255.0,
    a: 1.0,
};

fn main() -> Result<()> {
    unsafe {
        let instance = HINSTANCE::from(GetModuleHandleW(None)?);
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;

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
                let Ok(qt) = QT::new() else {
                    return LRESULT(-1);
                };
                let icon = Icon::calendar_month_regular();

                // Controls are created at (0,0); the Stack owns their positions.
                let rounded = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Rounded"),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let circular = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Circular"),
                            shape: button::Shape::Circular,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let square = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Square"),
                            shape: button::Shape::Square,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let primary = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Primary"),
                            appearance: button::Appearance::Primary,
                            icon: Some(icon),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let small_icon = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Small with calender icon"),
                            icon: Some(icon),
                            size: button::Size::Small,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let icon_after = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("With calendar icon after contents"),
                            icon: Some(icon),
                            icon_position: Some(IconPosition::After),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let large_icon = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Large with calender icon"),
                            icon: Some(icon),
                            size: button::Size::Large,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let input_default = qt
                    .create_input(
                        window,
                        0,
                        0,
                        input::Props {
                            width: 280,
                            default_value: Some(w!("Default text")),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let input_filled = qt
                    .create_input(
                        window,
                        0,
                        0,
                        input::Props {
                            width: 280,
                            appearance: input::Appearance::FilledLighter,
                            default_value: Some(w!("Filled lighter")),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let input_password = qt
                    .create_input(
                        window,
                        0,
                        0,
                        input::Props {
                            width: 380,
                            size: input::Size::Small,
                            input_type: input::Type::Password,
                            placeholder: Some(w!("Small with placeholder")),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let progress_medium = qt
                    .create_progress_bar(
                        window,
                        0,
                        0,
                        progress_bar::Props {
                            width: 400,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let progress_large = qt
                    .create_progress_bar(
                        window,
                        0,
                        0,
                        progress_bar::Props {
                            width: 400,
                            value: Some(0.4),
                            thickness: progress_bar::Thickness::Large,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let close = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Close"),
                            mouse_event: MouseEvent {
                                on_click: Box::new(move |_| {
                                    _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
                                }),
                            },
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let buttons_label = qt
                    .create_subtitle2(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("Buttons"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let inputs_label = qt
                    .create_subtitle2(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("Inputs"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let progress_label = qt
                    .create_subtitle2(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("Progress bar"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Gallery grouped by component type (top-anchored); a spring pins
                // the Close footer to the bottom-right.
                let section_gap = 24.0;
                let layout = Stack::vertical()
                    .padding(24.0)
                    .gap(section_gap)
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(buttons_label)
                            .add_stack(
                                Stack::horizontal()
                                    .gap(8.0)
                                    .add(rounded)
                                    .add(circular)
                                    .add(square)
                                    .add(primary),
                            )
                            .add_stack(
                                Stack::horizontal()
                                    .gap(8.0)
                                    .add(small_icon)
                                    .add(icon_after)
                                    .add(large_icon),
                            ),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(inputs_label)
                            .add_stack(
                                Stack::horizontal()
                                    .gap(12.0)
                                    .add(input_default)
                                    .add(input_filled),
                            )
                            .add(input_password),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(progress_label)
                            .add(progress_medium)
                            .add(progress_large),
                    )
                    .spring()
                    .add_stack(Stack::horizontal().spring().add(close));

                let mut rc = RECT::default();
                if GetClientRect(window, &mut rc).is_ok() {
                    _ = layout.arrange(window, rc);
                }

                let state = Box::new(AppState { qt, layout });
                SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as _);
                DefWindowProcW(window, message, w_param, l_param)
            }
            WM_SIZE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                if !raw.is_null() {
                    let mut rc = RECT::default();
                    if GetClientRect(window, &mut rc).is_ok() {
                        _ = (*raw).layout.arrange(window, rc);
                    }
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                let qt = &(*raw).qt;
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
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut AppState;
                if !raw.is_null() {
                    _ = Box::from_raw(raw);
                }
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

                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                let qt = &(*raw).qt;
                let menu_list = vec![
                    MenuInfo::MenuItem {
                        text: w!("New"),
                        command_id: 0,
                        disabled: false,
                        secondary_text: None,
                    },
                    MenuInfo::MenuItem {
                        text: w!("New window"),
                        command_id: 1,
                        disabled: false,
                        secondary_text: None,
                    },
                    MenuInfo::MenuItem {
                        text: w!("Open file"),
                        command_id: 2,
                        disabled: true,
                        secondary_text: None,
                    },
                    MenuInfo::MenuDivider,
                    MenuInfo::SubMenu {
                        text: w!("Preferences"),
                        menu_list: vec![
                            MenuInfo::MenuItem {
                                text: w!("Settings"),
                                command_id: 30,
                                disabled: false,
                                secondary_text: None,
                            },
                            MenuInfo::MenuItem {
                                text: w!("Online services settings"),
                                command_id: 31,
                                disabled: false,
                                secondary_text: None,
                            },
                            MenuInfo::MenuDivider,
                            MenuInfo::MenuItem {
                                text: w!("Extensions"),
                                command_id: 32,
                                disabled: false,
                                secondary_text: None,
                            },
                            MenuInfo::SubMenu {
                                text: w!("Appearance"),
                                menu_list: vec![
                                    MenuInfo::MenuItem {
                                        text: w!("Centered layout"),
                                        command_id: 30,
                                        disabled: false,
                                        secondary_text: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zen"),
                                        command_id: 31,
                                        disabled: false,
                                        secondary_text: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zoom in"),
                                        command_id: 32,
                                        disabled: true,
                                        secondary_text: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zoom out"),
                                        command_id: 33,
                                        disabled: false,
                                        secondary_text: None,
                                    },
                                ],
                            },
                            MenuInfo::SubMenu {
                                text: w!("Editor Layout"),
                                menu_list: vec![
                                    MenuInfo::MenuItem {
                                        text: w!("Split Up"),
                                        command_id: 40,
                                        disabled: false,
                                        secondary_text: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Split Down"),
                                        command_id: 41,
                                        disabled: false,
                                        secondary_text: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Single"),
                                        command_id: 42,
                                        disabled: false,
                                        secondary_text: None,
                                    },
                                ],
                            },
                        ],
                    },
                ];
                _ = qt.open_menu(window, x, y, menu::Props { menu_list });
                LRESULT::default()
            }
            _ => DefWindowProcW(window, message, w_param, l_param),
        }
    }
}
