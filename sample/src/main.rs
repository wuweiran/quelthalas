//#![windows_subsystem = "windows"]
use std::mem::size_of;

use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, FillRect, PAINTSTRUCT, PtInRect, RDW_ALLCHILDREN,
    RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use quelthalas::component::button::IconPosition;
use quelthalas::component::dialog::DialogResult;
use quelthalas::component::menu::MenuInfo;
use quelthalas::component::{
    button, checkbox, dialog, dropdown, input, menu, progress_bar, radio, switch, text,
};
use quelthalas::icon::Icon;
use quelthalas::layout::Stack;
use quelthalas::{MouseEvent, QT};

struct AppState {
    qt: QT,
    layout: Stack,
    menu_target: HWND,
}

// Window canvas background (#fafafa). Labels use it so they blend seamlessly.
const CANVAS: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 250.0 / 255.0,
    g: 250.0 / 255.0,
    b: 250.0 / 255.0,
    a: 1.0,
};

// Distinct fill (#e6e6e6) for the right-click target so the area is visible.
const MENU_AREA: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 230.0 / 255.0,
    g: 230.0 / 255.0,
    b: 230.0 / 255.0,
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
                let icon = Icon::calendar_month_20_regular();

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
                let input_filled_darker = qt
                    .create_input(
                        window,
                        0,
                        0,
                        input::Props {
                            width: 280,
                            appearance: input::Appearance::FilledDarker,
                            default_value: Some(w!("Filled darker")),
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
                let open_dialog = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Open dialog"),
                            mouse_event: MouseEvent {
                                on_click: Box::new({
                                    let qt = qt.clone();
                                    move |_| {
                                        _ = qt.open_dialog(
                                            window,
                                            w!("Dialog title"),
                                            w!("Lorem ipsum dolor sit amet consectetur adipisicing elit. Quisquam exercitationem cumque repellendus eaque est dolor eius expedita nulla ullam? Tenetur reprehenderit aut voluptatum impedit voluptates in natus iure cumque eaque?"),
                                            &dialog::ModelType::Alert,
                                        );
                                    }
                                }),
                            },
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

                // Section header (subtitle2) blended onto the canvas.
                let section = |text: PCWSTR| {
                    qt.create_subtitle2(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text,
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default()
                };
                let buttons_label = section(w!("Buttons"));
                let inputs_label = section(w!("Inputs"));
                let progress_label = section(w!("Progress bar"));
                let dialog_label = section(w!("Dialog"));
                let menu_label = section(w!("Menu"));
                let text_label = section(w!("Text"));

                let menu_hint = qt
                    .create_body1(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("Right-click here for a context menu."),
                            background: Some(MENU_AREA),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let checkbox_label = section(w!("Checkbox"));
                let checkbox = qt
                    .create_checkbox(
                        window,
                        0,
                        0,
                        checkbox::Props {
                            label: w!("Checked"),
                            checked: true,
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Radio group: `group_start` on the first marks the WS_GROUP
                // boundary; selecting one auto-clears the others.
                let radio_label = section(w!("Radio"));
                let radio_apple = qt
                    .create_radio(
                        window,
                        0,
                        0,
                        radio::Props {
                            label: w!("Apple"),
                            group_start: true,
                            checked: true,
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let radio_pear = qt
                    .create_radio(
                        window,
                        0,
                        0,
                        radio::Props {
                            label: w!("Pear"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let radio_banana = qt
                    .create_radio(
                        window,
                        0,
                        0,
                        radio::Props {
                            label: w!("Banana"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let radio_orange = qt
                    .create_radio(
                        window,
                        0,
                        0,
                        radio::Props {
                            label: w!("Orange"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Switch: click to toggle; the thumb slides (WAM-eased).
                let switch_label = section(w!("Switch"));
                let switch = qt
                    .create_switch(
                        window,
                        0,
                        0,
                        switch::Props {
                            label: w!("This is a switch"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Dropdown: click the field to open a flat popup list; pick one.
                // Ferret is disabled (greyed, unclickable, skipped by keyboard).
                let dropdown_label = section(w!("Dropdown"));
                let dropdown = qt
                    .create_dropdown(
                        window,
                        0,
                        0,
                        dropdown::Props {
                            options: vec![
                                dropdown::Item::new(w!("Cat")),
                                dropdown::Item::new(w!("Caterpillar")),
                                dropdown::Item::new(w!("Corgi")),
                                dropdown::Item::new(w!("Chupacabra")),
                                dropdown::Item::new(w!("Dog")),
                                dropdown::Item::disabled(w!("Ferret")),
                                dropdown::Item::new(w!("Fish")),
                                dropdown::Item::new(w!("Fox")),
                                dropdown::Item::new(w!("Hamster")),
                                dropdown::Item::new(w!("Snake")),
                            ],
                            placeholder: w!("Select an animal"),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Text section: an intro line, then every preset labelled by name.
                let text_intro = qt
                    .create_body1(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("This is an example of the Text component's usage."),
                            background: Some(CANVAS),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                macro_rules! preset {
                    ($method:ident, $label:expr) => {
                        qt.$method(
                            window,
                            0,
                            0,
                            text::PresetProps {
                                text: $label,
                                background: Some(CANVAS),
                                ..Default::default()
                            },
                        )
                        .unwrap_or_default()
                    };
                }
                let p_caption1 = preset!(create_caption1, w!("Caption1"));
                let p_caption1_strong = preset!(create_caption1_strong, w!("Caption1Strong"));
                let p_body1 = preset!(create_body1, w!("Body1"));
                let p_body1_strong = preset!(create_body1_strong, w!("Body1Strong"));
                let p_body2 = preset!(create_body2, w!("Body2"));
                let p_subtitle2 = preset!(create_subtitle2, w!("Subtitle2"));
                let p_subtitle1 = preset!(create_subtitle1, w!("Subtitle1"));
                let p_title3 = preset!(create_title3, w!("Title3"));
                let p_title2 = preset!(create_title2, w!("Title2"));
                let p_title1 = preset!(create_title1, w!("Title1"));

                // Two columns: components on the left, Text presets on the right;
                // a spring pins the Close footer to the bottom-right.
                let left_column = Stack::vertical()
                    .gap(24.0)
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
                                    .add(input_filled)
                                    .add(input_filled_darker),
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
                    .add_stack(Stack::vertical().gap(8.0).add(dialog_label).add(open_dialog))
                    .add_stack(Stack::vertical().gap(8.0).add(menu_label).add(menu_hint))
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(checkbox_label)
                            .add(checkbox),
                    );

                let right_column = Stack::vertical()
                    .gap(24.0)
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(text_label)
                            .add(text_intro)
                            .add(p_caption1)
                            .add(p_caption1_strong)
                            .add(p_body1)
                            .add(p_body1_strong)
                            .add(p_body2)
                            .add(p_subtitle2)
                            .add(p_subtitle1)
                            .add(p_title3)
                            .add(p_title2)
                            .add(p_title1),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(radio_label)
                            // Horizontal group. Each radio already carries its own
                            // `pad()` on both sides, so no extra gap is needed.
                            .add_stack(
                                Stack::horizontal()
                                    .add(radio_apple)
                                    .add(radio_pear)
                                    .add(radio_banana)
                                    .add(radio_orange),
                            ),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(switch_label)
                            .add(switch),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(8.0)
                            .add(dropdown_label)
                            .add(dropdown),
                    );

                let layout = Stack::vertical()
                    .padding(24.0)
                    .gap(24.0)
                    .add_stack(
                        Stack::horizontal()
                            .gap(48.0)
                            .add_stack(left_column)
                            .add_stack(right_column),
                    )
                    .spring()
                    .add_stack(Stack::horizontal().spring().add(close));

                let mut rc = RECT::default();
                if GetClientRect(window, &mut rc).is_ok() {
                    _ = layout.arrange(window, rc);
                }

                let state = Box::new(AppState {
                    qt,
                    layout,
                    menu_target: menu_hint,
                });
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
            WM_DISPLAYCHANGE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                if !raw.is_null() {
                    let mut rc = RECT::default();
                    if GetClientRect(window, &mut rc).is_ok() {
                        _ = (*raw).layout.arrange(window, rc);
                    }
                }
                _ = RedrawWindow(
                    Some(window),
                    None,
                    None,
                    RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
                );
                LRESULT(0)
            }
            WM_CLOSE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                let qt = &(*raw).qt;
                match qt.open_dialog(
                    window,
                    w!("Close"),
                    w!("Are you sure you want to close this window?"),
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
                // Only pop the menu when the right-click lands on the designated
                // target label; otherwise let default handling proceed.
                let mut target_rect = RECT::default();
                if GetWindowRect((*raw).menu_target, &mut target_rect).is_err()
                    || !PtInRect(&target_rect, POINT { x, y }).as_bool()
                {
                    return DefWindowProcW(window, message, w_param, l_param);
                }
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
