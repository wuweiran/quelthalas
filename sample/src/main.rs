//#![windows_subsystem = "windows"]
use std::mem::size_of;

use windows::Win32::Foundation::{
    COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
    PtInRect, RDW_ALLCHILDREN, RDW_INVALIDATE, RDW_UPDATENOW, RedrawWindow, ScreenToClient,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use quelthalas::component::button::IconPosition;
use quelthalas::component::dialog::DialogResult;
use quelthalas::component::menu::MenuInfo;
use quelthalas::component::{
    button, calendar, checkbox, combobox, data_grid, date_picker, dialog, divider, dropdown, input,
    link, list_box, menu, menu_bar, message_bar, option, progress_bar, radio, search_box, slider,
    spin_button, spinner, split_button, switch, tab_list, task_dialog, text, textarea, toolbar,
    tree_view,
};
use quelthalas::icon::Icon;
use quelthalas::layout::Stack;
use quelthalas::{MouseEvent, QT, Theme};

#[derive(Copy, Clone, PartialEq, Eq)]
enum AppTheme {
    WebLight,
    WebDark,
}

impl AppTheme {
    fn theme(self) -> Theme {
        match self {
            AppTheme::WebLight => Theme::web_light(),
            AppTheme::WebDark => Theme::web_dark(),
        }
    }
}

/// One tab's page: a laid-out Stack + the flat list of its controls (so we can
/// show/hide the whole page at once). `controls` excludes the always-visible
/// chrome (tab strip + Close), which live in every page's Stack.
struct Page {
    stack: Stack,
    controls: Vec<HWND>,
}

struct AppState {
    qt: QT,
    pages: Vec<Page>,
    active: usize,
    menu_target: HWND,
    // The tab strip child — WM_PAINT reads its height to split the window into the
    // chrome band (behind/around the strip) and the CANVAS page below it.
    tab_list: HWND,
    // The menu bar child (above the tab strip). WM_PAINT reads its geometry too so
    // the chrome band covers it; Alt/F10 forwards here to enter menu mode.
    menu_bar: HWND,
    theme: AppTheme,
    palette: Palette,
}

// The TabList's on_change posts this to the app window (like SysTabControl32 →
// TCN_SELCHANGE); the app owns the page swap.
const WM_APP_TAB: u32 = WM_APP + 1;

// Menu-bar command ids (arrive as WM_COMMAND wParam). The menu component posts
// these to the app window when a dropdown item is chosen. Only actions the sample
// can honestly perform are enabled; the rest are shown disabled.
const CMD_VIEW_THEME_LIGHT: u32 = 200;
const CMD_VIEW_THEME_DARK: u32 = 201;
const CMD_HELP_ABOUT: u32 = 300;
// SplitButton dropdown items.
const CMD_ITEM_A: u32 = 400;
const CMD_ITEM_B: u32 = 401;
// Toolbar (rich-text editor) command ids.
const CMD_TB_FONT: u32 = 500;
const CMD_TB_BOLD: u32 = 501;
const CMD_TB_ITALIC: u32 = 502;
const CMD_TB_UNDERLINE: u32 = 503;
const CMD_TB_FONT_INCREASE: u32 = 504;
const CMD_TB_FONT_DECREASE: u32 = 505;
const CMD_TB_BULLET_LIST: u32 = 506;
const CMD_TB_NUMBER_LIST: u32 = 507;

/// Re-arrange the tab strip + the active page. The tab strip and Close button are
/// members of every page's Stack, so arranging the active page's Stack lays out
/// everything currently visible.
unsafe fn arrange_all(state: &AppState, window: HWND) {
    unsafe {
        let mut rc = RECT::default();
        if GetClientRect(window, &mut rc).is_ok() {
            _ = state.pages[state.active].stack.arrange(window, rc);
        }
    }
}

/// Switch to page `idx`: hide the other pages' controls, show this one's, arrange.
unsafe fn show_page(state: &mut AppState, window: HWND, idx: usize) {
    unsafe {
        for (i, page) in state.pages.iter().enumerate() {
            let cmd = if i == idx { SW_SHOW } else { SW_HIDE };
            for &hwnd in &page.controls {
                _ = ShowWindow(hwnd, cmd);
            }
        }
        state.active = idx;
        arrange_all(state, window);
        // Just invalidate the parent to repaint the canvas gaps left by the hidden
        // page. WS_CLIPCHILDREN keeps that fill off the children, and each shown
        // child repaints itself — no synchronous full-tree redraw (which flashed).
        _ = InvalidateRect(Some(window), None, true);
    }
}

/// EnumChildWindows callback: collect each child HWND into the `Vec<HWND>` passed
/// as `lparam`. (Collect first, then destroy — mutating the tree mid-enumeration is
/// unsafe.)
unsafe extern "system" fn collect_child(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);
        list.push(hwnd);
    }
    TRUE
}

/// Rebuild the whole UI under `window` with `target` theme, preserving the active
/// tab. Every control clones the theme at creation, so a theme change means
/// destroy-all + recreate (see the plan): we can't re-theme frozen controls.
unsafe fn retheme(window: HWND, target: AppTheme) {
    unsafe {
        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut AppState;
        if raw.is_null() || (*raw).theme == target {
            return;
        }
        let active = (*raw).active;

        // 1. Destroy every existing child control (collect then destroy).
        let mut children: Vec<HWND> = Vec::new();
        _ = EnumChildWindows(
            Some(window),
            Some(collect_child),
            LPARAM(&mut children as *mut _ as isize),
        );
        for &child in &children {
            _ = DestroyWindow(child);
        }

        // 2. Reclaim + drop the old AppState (releases the old QT + closures).
        drop(Box::from_raw(raw));
        SetWindowLongPtrW(window, GWLP_USERDATA, 0);

        // 3. Rebuild with a fresh QT carrying the new theme.
        let Ok(qt) = QT::new_with(target.theme()) else {
            return;
        };
        let mut state = Box::new(build_ui(qt, window, target, active));
        show_page(&mut state, window, active);
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as _);
        // 4. Repaint the window chrome (the two-tone band) in the new palette.
        _ = InvalidateRect(Some(window), None, true);
    }
}

// Window canvas background (#fafafa). Labels use it so they blend seamlessly.
const CANVAS: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 250.0 / 255.0,
    g: 250.0 / 255.0,
    b: 250.0 / 255.0,
    a: 1.0,
};

// Conventional Win32 app chrome background (#f0f0f0, i.e. COLOR_BTNFACE in the
// classic/light scheme). Used for the window fill and the TabList strip so the
// strip blends into the window and the (lighter) selected card reads as the
// content surface lifting out of the chrome.
const WIN32_GRAY: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 240.0 / 255.0,
    g: 240.0 / 255.0,
    b: 240.0 / 255.0,
    a: 1.0,
};

// Distinct fill (#e6e6e6) for the right-click target so the area is visible.
const MENU_AREA: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 230.0 / 255.0,
    g: 230.0 / 255.0,
    b: 230.0 / 255.0,
    a: 1.0,
};

/// A `#rrggbb`-style opaque D2D color from one gray byte (r==g==b).
const fn gray(v: u8) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: v as f32 / 255.0,
        g: v as f32 / 255.0,
        b: v as f32 / 255.0,
        a: 1.0,
    }
}

/// The sample's own chrome surfaces (distinct from the library's control theme).
/// These feed every control's `background:` prop and the two-tone `WM_PAINT`, so
/// they must flip with the theme to avoid light halos on a dark window.
#[derive(Copy, Clone)]
struct Palette {
    /// Page/content background (behind controls) — the D2D `background:` value.
    canvas: D2D1_COLOR_F,
    /// Chrome band behind the menu bar + tab strip.
    chrome: D2D1_COLOR_F,
    /// Right-click target fill (so the area is visible).
    menu_area: D2D1_COLOR_F,
    /// GDI COLORREF (0x00BBGGRR) equivalents for the two-tone `WM_PAINT`.
    chrome_ref: COLORREF,
    page_ref: COLORREF,
}

impl Palette {
    fn light() -> Self {
        Palette {
            canvas: CANVAS,
            chrome: WIN32_GRAY,
            menu_area: MENU_AREA,
            chrome_ref: COLORREF(0xf0f0f0),
            page_ref: COLORREF(0xfafafa),
        }
    }
    // Dark: all sample surfaces flatten to #1f1f1f (the two-tone band and the
    // right-click target lose their distinct fills in dark; the selected tab still
    // reads via its border stroke).
    fn dark() -> Self {
        Palette {
            canvas: gray(0x1f),
            chrome: gray(0x1f),
            menu_area: gray(0x1f),
            chrome_ref: COLORREF(0x1f1f1f),
            page_ref: COLORREF(0x1f1f1f),
        }
    }
    fn for_theme(theme: AppTheme) -> Self {
        match theme {
            AppTheme::WebLight => Palette::light(),
            AppTheme::WebDark => Palette::dark(),
        }
    }
}

/// Build every control + the page/stack layout for the given theme, returning a
/// fresh `AppState`. Called at startup and again on a theme toggle (the whole UI
/// is recreated because each control clones the theme at creation). `active` is
/// the tab to select, so a toggle keeps the current page.
fn build_ui(qt: QT, window: HWND, theme: AppTheme, active: usize) -> AppState {
    let palette = Palette::for_theme(theme);
    unsafe {
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
                let search = qt
                    .create_search_box(
                        window,
                        0,
                        0,
                        search_box::Props {
                            width: 360,
                            placeholder: Some(w!("Search")),
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
                                            dialog::Actions::OkCancel,
                                        );
                                    }
                                }),
                            },
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let open_task_dialog = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            text: w!("Open task dialog"),
                            mouse_event: MouseEvent {
                                on_click: Box::new({
                                    let qt = qt.clone();
                                    move |_| {
                                        let result = qt.open_task_dialog(
                                            window,
                                            task_dialog::Props {
                                                title: w!("Unsaved changes"),
                                                instruction: w!("Do you want to save your changes?"),
                                                content: w!("Your changes will be lost if you don't save them."),
                                                intent: task_dialog::Intent::Warning,
                                                buttons: vec![
                                                    task_dialog::Button::Yes,
                                                    task_dialog::Button::No,
                                                ],
                                                command_links: vec![
                                                    task_dialog::CommandLink {
                                                        id: 100,
                                                        text: w!("Save and continue"),
                                                        note: Some(w!("Keep your work and proceed.")),
                                                    },
                                                    task_dialog::CommandLink {
                                                        id: 101,
                                                        text: w!("Discard changes"),
                                                        note: Some(w!("Permanently lose your edits.")),
                                                    },
                                                ],
                                                verification: Some(w!("Don't ask again")),
                                                verification_checked: false,
                                            },
                                        );
                                        if let Ok(r) = result {
                                            let msg = match r.button {
                                                100 => w!("Save and continue (checked: see title)"),
                                                101 => w!("Discard changes"),
                                                6 => w!("Yes"),
                                                7 => w!("No"),
                                                _ => w!("Closed"),
                                            };
                                            _ = qt.open_dialog(
                                                window,
                                                if r.verified {
                                                    w!("Result (verified)")
                                                } else {
                                                    w!("Result")
                                                },
                                                msg,
                                                &dialog::ModelType::Alert,
                                                dialog::Actions::Ok,
                                            );
                                        }
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
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default()
                };
                let buttons_label = section(w!("Buttons"));
                let inputs_label = section(w!("Inputs"));
                let search_box_label = section(w!("Search box"));
                let progress_label = section(w!("Progress bar"));
                let dialog_label = section(w!("Dialog"));
                let task_dialog_label = section(w!("Task dialog"));
                let menu_label = section(w!("Menu"));
                let text_label = section(w!("Text"));

                let menu_hint = qt
                    .create_body1(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("Right-click here for a context menu."),
                            background: Some(palette.menu_area),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let split_button_label = section(w!("Split button"));
                let split_menu = || {
                    vec![
                        menu::MenuInfo::MenuItem {
                            text: w!("Item a"),
                            command_id: CMD_ITEM_A,
                            disabled: false,
                            secondary_text: None,
                            icon: None,
                        },
                        menu::MenuInfo::MenuItem {
                            text: w!("Item b"),
                            command_id: CMD_ITEM_B,
                            disabled: false,
                            secondary_text: None,
                            icon: None,
                        },
                    ]
                };
                let split_secondary = qt
                    .create_split_button(
                        window,
                        0,
                        0,
                        split_button::Props {
                            text: w!("Default"),
                            menu_list: split_menu(),
                            mouse_event: MouseEvent {
                                on_click: Box::new(move |_| {
                                    _ = PostMessageW(
                                        Some(window),
                                        WM_COMMAND,
                                        WPARAM(CMD_ITEM_A as usize),
                                        LPARAM(0),
                                    );
                                }),
                            },
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let split_primary = qt
                    .create_split_button(
                        window,
                        0,
                        0,
                        split_button::Props {
                            text: w!("Primary"),
                            appearance: button::Appearance::Primary,
                            menu_list: split_menu(),
                            mouse_event: MouseEvent {
                                on_click: Box::new(move |_| {
                                    _ = PostMessageW(
                                        Some(window),
                                        WM_COMMAND,
                                        WPARAM(CMD_ITEM_A as usize),
                                        LPARAM(0),
                                    );
                                }),
                            },
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
                            background: Some(palette.canvas),
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
                            background: Some(palette.canvas),
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
                            background: Some(palette.canvas),
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
                            background: Some(palette.canvas),
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
                            background: Some(palette.canvas),
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
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let animals = vec![
                    option::Item::new(w!("Cat")),
                    option::Item::new(w!("Dog")),
                    option::Item::disabled(w!("Ferret")),
                    option::Item::new(w!("Fish")),
                    option::Item::new(w!("Hamster")),
                    option::Item::new(w!("Snake")),
                ];
                let dropdown_label = section(w!("Dropdown"));
                let dropdown = qt
                    .create_dropdown(
                        window,
                        0,
                        0,
                        dropdown::Props {
                            options: animals.clone(),
                            placeholder: w!("Select an animal"),
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let combobox_label = section(w!("Combobox"));
                let combobox = qt
                    .create_combobox(
                        window,
                        0,
                        0,
                        combobox::Props {
                            options: animals.clone(),
                            placeholder: w!("Type or pick an animal"),
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let spin_button_label = section(w!("Spin button"));
                let spin_button = qt
                    .create_spin_button(
                        window,
                        0,
                        0,
                        spin_button::Props {
                            value: 10.0,
                            min: 0.0,
                            max: 20.0,
                            step: 1.0,
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let list_box_label = section(w!("List box"));
                let list_box = qt
                    .create_list_box(
                        window,
                        0,
                        0,
                        list_box::Props {
                            items: vec![
                                option::Item::new(w!("Mercury")),
                                option::Item::new(w!("Venus")),
                                option::Item::new(w!("Earth")),
                                option::Item::disabled(w!("Mars")),
                                option::Item::new(w!("Jupiter")),
                                option::Item::new(w!("Saturn")),
                                option::Item::new(w!("Uranus")),
                                option::Item::new(w!("Neptune")),
                            ],
                            width: 240,
                            height: 160,
                            selected: Some(2),
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let tree_view_label = section(w!("Tree view"));
                let tree_view = qt
                    .create_tree_view(
                        window,
                        0,
                        0,
                        tree_view::Props {
                            roots: vec![
                                tree_view::Node::branch(w!("Documents")),
                                tree_view::Node::branch(w!("Pictures")),
                                tree_view::Node::branch(w!("Music")),
                                tree_view::Node::leaf(w!("readme.txt")),
                            ],
                            // Lazy children: synthesize from the node's path so the
                            // TVN_ITEMEXPANDING-style callback is exercised. Depth < 2
                            // folders get subfolders; deeper levels get leaf files.
                            on_expand: Box::new(|path| {
                                if path.len() < 2 {
                                    vec![
                                        tree_view::Node::branch(w!("Subfolder A")),
                                        tree_view::Node::branch(w!("Subfolder B")),
                                        tree_view::Node::leaf(w!("notes.txt")),
                                    ]
                                } else {
                                    vec![
                                        tree_view::Node::leaf(w!("file1.dat")),
                                        tree_view::Node::leaf(w!("file2.dat")),
                                        tree_view::Node::leaf(w!("file3.dat")),
                                    ]
                                }
                            }),
                            width: 240,
                            height: 160,
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Data grid: a files table (Win32 ListView report mode / Fluent
                // DataGrid) with a select-all + per-row checkbox column and per-cell
                // icons. Multi-select via click / Ctrl / Shift / Space / Ctrl+A.
                let data_grid_label = section(w!("Data grid"));
                let data_grid = qt
                    .create_data_grid(
                        window,
                        0,
                        0,
                        data_grid::Props {
                            columns: vec![
                                data_grid::Column { header: w!("File"), width: 200, sortable: true },
                                data_grid::Column { header: w!("Author"), width: 140, sortable: true },
                                data_grid::Column { header: w!("Last updated"), width: 150, sortable: true },
                            ],
                            rows: vec![
                                data_grid::Row {
                                    cells: vec![
                                        data_grid::Cell::new(
                                            Icon::text_font_24_regular(),
                                            w!("Meeting notes"),
                                        ),
                                        data_grid::Cell::text(w!("Max Mustermann")),
                                        data_grid::Cell::new(
                                            Icon::calendar_month_20_regular(),
                                            w!("2026-07-12 09:15"),
                                        ),
                                    ],
                                },
                                data_grid::Row {
                                    cells: vec![
                                        data_grid::Cell::new(
                                            Icon::slide_text_20_regular(),
                                            w!("Thursday presentation"),
                                        ),
                                        data_grid::Cell::text(w!("Erika Mustermann")),
                                        data_grid::Cell::new(
                                            Icon::calendar_month_20_regular(),
                                            w!("2026-07-11 16:40"),
                                        ),
                                    ],
                                },
                                data_grid::Row {
                                    cells: vec![
                                        data_grid::Cell::new(
                                            Icon::text_bullet_list_24_regular(),
                                            w!("Training recording"),
                                        ),
                                        data_grid::Cell::text(w!("John Doe")),
                                        data_grid::Cell::new(
                                            Icon::calendar_month_20_regular(),
                                            w!("2026-07-11 08:05"),
                                        ),
                                    ],
                                },
                                data_grid::Row {
                                    cells: vec![
                                        data_grid::Cell::new(
                                            Icon::text_number_list_24_regular(),
                                            w!("Purchase order"),
                                        ),
                                        data_grid::Cell::text(w!("Jane Doe")),
                                        data_grid::Cell::new(
                                            Icon::calendar_month_20_regular(),
                                            w!("2026-07-07 09:30"),
                                        ),
                                    ],
                                },
                                data_grid::Row {
                                    cells: vec![
                                        data_grid::Cell::new(
                                            Icon::text_bold_24_regular(),
                                            w!("Design spec"),
                                        ),
                                        data_grid::Cell::text(w!("Sam Rivera")),
                                        data_grid::Cell::new(
                                            Icon::calendar_month_20_regular(),
                                            w!("2026-07-06 16:12"),
                                        ),
                                    ],
                                },
                                data_grid::Row {
                                    cells: vec![
                                        data_grid::Cell::new(
                                            Icon::text_italic_24_regular(),
                                            w!("Budget draft"),
                                        ),
                                        data_grid::Cell::text(w!("Alex Kim")),
                                        data_grid::Cell::new(
                                            Icon::calendar_month_20_regular(),
                                            w!("2026-07-03 11:20"),
                                        ),
                                    ],
                                },
                            ],
                            width: 540,
                            height: 240,
                            selection_mode: data_grid::SelectionMode::Multiselect,
                            mouse_event: data_grid::MouseEvent {
                                on_activate: Box::new({
                                    let qt = qt.clone();
                                    move |_, i| {
                                        // Win32 ListView activate (double-click) —
                                        // "open" the file. Name it in a dialog.
                                        const FILES: [PCWSTR; 6] = [
                                            w!("Meeting notes"),
                                            w!("Thursday presentation"),
                                            w!("Training recording"),
                                            w!("Purchase order"),
                                            w!("Design spec"),
                                            w!("Budget draft"),
                                        ];
                                        let name = FILES.get(i).copied().unwrap_or(w!(""));
                                        _ = qt.open_dialog(
                                            window,
                                            w!("Open"),
                                            name,
                                            &dialog::ModelType::Alert,
                                            dialog::Actions::Ok,
                                        );
                                    }
                                }),
                                ..Default::default()
                            },
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let textarea_label = section(w!("Textarea"));
                let textarea = qt
                    .create_textarea(
                        window,
                        0,
                        0,
                        textarea::Props {
                            width: 280,
                            height: 96,
                            placeholder: Some(w!("Type here\u{2026}")),
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                let divider_label = section(w!("Divider"));
                let make_divider = |label: Option<PCWSTR>,
                                    alignment: divider::Alignment,
                                    appearance: divider::Appearance| {
                    qt.create_divider(
                        window,
                        0,
                        0,
                        divider::Props {
                            label,
                            alignment,
                            appearance,
                            width: 280,
                            background: Some(palette.canvas),
                        },
                    )
                    .unwrap_or_default()
                };
                let divider_start = make_divider(
                    Some(w!("Details")),
                    divider::Alignment::Start,
                    divider::Appearance::Default,
                );
                let divider_center = make_divider(
                    Some(w!("Center")),
                    divider::Alignment::Center,
                    divider::Appearance::Default,
                );
                let divider_plain =
                    make_divider(None, divider::Alignment::Center, divider::Appearance::Default);
                let divider_subtle = make_divider(
                    Some(w!("Subtle")),
                    divider::Alignment::Start,
                    divider::Appearance::Subtle,
                );
                let divider_brand = make_divider(
                    Some(w!("Brand")),
                    divider::Alignment::Start,
                    divider::Appearance::Brand,
                );
                let divider_strong = make_divider(
                    Some(w!("Strong")),
                    divider::Alignment::Start,
                    divider::Appearance::Strong,
                );

                // Toolbar: a rich-text editor strip of icon-only buttons in three
                // groups, at a fixed width so trailing groups collapse into the
                // "More" (⋯) overflow flyout.
                let toolbar_label = section(w!("Toolbar"));
                let toolbar = qt
                    .create_toolbar(
                        window,
                        0,
                        0,
                        toolbar::Props {
                            width: 260,
                            items: vec![
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_FONT,
                                    icon: Some(Icon::text_font_24_regular()),
                                    text: Some(w!("Font")),
                                },
                                toolbar::ToolbarItem::Divider,
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_BOLD,
                                    icon: Some(Icon::text_bold_24_regular()),
                                    text: Some(w!("Bold")),
                                },
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_ITALIC,
                                    icon: Some(Icon::text_italic_24_regular()),
                                    text: Some(w!("Italic")),
                                },
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_UNDERLINE,
                                    icon: Some(Icon::text_underline_24_regular()),
                                    text: Some(w!("Underline")),
                                },
                                toolbar::ToolbarItem::Divider,
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_FONT_INCREASE,
                                    icon: Some(Icon::font_increase_24_regular()),
                                    text: Some(w!("Increase font size")),
                                },
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_FONT_DECREASE,
                                    icon: Some(Icon::font_decrease_24_regular()),
                                    text: Some(w!("Decrease font size")),
                                },
                                toolbar::ToolbarItem::Divider,
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_BULLET_LIST,
                                    icon: Some(Icon::text_bullet_list_24_regular()),
                                    text: Some(w!("Bulleted list")),
                                },
                                toolbar::ToolbarItem::Button {
                                    id: CMD_TB_NUMBER_LIST,
                                    icon: Some(Icon::text_number_list_24_regular()),
                                    text: Some(w!("Numbered list")),
                                },
                            ],
                            background: Some(palette.canvas),
                        },
                    )
                    .unwrap_or_default();


                let slider_label = section(w!("Slider"));
                let slider = qt
                    .create_slider(
                        window,
                        0,
                        0,
                        slider::Props {
                            min: 0.0,
                            max: 100.0,
                            value: 40.0,
                            width: 200,
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // Spinner: an indeterminate rotating busy indicator.
                let spinner_label = section(w!("Spinner"));
                let spinner = qt
                    .create_spinner(
                        window,
                        0,
                        0,
                        spinner::Props {
                            size: spinner::Size::Medium,
                            background: Some(palette.canvas),
                        },
                    )
                    .unwrap_or_default();

                // Link: a Fluent-styled hyperlink; click / Enter / Space fires on_click.
                let link_label = section(w!("Link"));
                let link = qt
                    .create_link(
                        window,
                        0,
                        0,
                        link::Props {
                            text: w!("This is a link"),
                            mouse_event: MouseEvent {
                                on_click: Box::new(move |_| {
                                    // Open the URL in the default browser — the
                                    // "open" verb, exactly what SysLink does.
                                    ShellExecuteW(
                                        Some(window),
                                        w!("open"),
                                        w!("https://www.bing.com"),
                                        PCWSTR::null(),
                                        PCWSTR::null(),
                                        SW_SHOWNORMAL,
                                    );
                                }),
                            },
                            background: Some(palette.canvas),
                        },
                    )
                    .unwrap_or_default();

                // Tooltip: an icon-only button that shows "Example tooltip" on hover
                let tooltip_label = section(w!("Tooltip"));
                let tooltip_button = qt
                    .create_button(
                        window,
                        0,
                        0,
                        button::Props {
                            icon: Some(Icon::slide_text_20_regular()),
                            size: button::Size::Large,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                _ = qt.add_tooltip(tooltip_button, w!("Example tooltip"));

                // Message bars: one per intent, plus an info bar with an action button.
                let message_bar_label = section(w!("Message bar"));
                let make_bar = |intent: message_bar::Intent, title: PCWSTR, msg: PCWSTR, actions: Vec<message_bar::Action>| {
                    qt.create_message_bar(
                        window,
                        0,
                        0,
                        message_bar::Props {
                            intent,
                            title,
                            message: msg,
                            actions,
                            width: 380,
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default()
                };
                let bar_info = make_bar(
                    message_bar::Intent::Info,
                    w!("Update available"),
                    w!("A new version is ready to install."),
                    vec![message_bar::Action {
                        text: w!("Update"),
                        mouse_event: MouseEvent {
                            on_click: Box::new({
                                let qt = qt.clone();
                                move |_| {
                                    _ = qt.open_dialog(
                                        window,
                                        w!("Update"),
                                        w!("Updating to the latest version\u{2026}"),
                                        &dialog::ModelType::Alert,
                                        dialog::Actions::Ok,
                                    );
                                }
                            }),
                        },
                    }],
                );
                let bar_success = make_bar(
                    message_bar::Intent::Success,
                    w!("Saved"),
                    w!("Your changes were saved."),
                    Vec::new(),
                );
                let bar_warning = make_bar(
                    message_bar::Intent::Warning,
                    w!("Heads up"),
                    w!("Your session expires soon."),
                    Vec::new(),
                );
                let bar_error = make_bar(
                    message_bar::Intent::Error,
                    w!("Connection failed"),
                    w!("Couldn't connect to the server."),
                    Vec::new(),
                );

                // Text section: an intro line, then every preset labelled by name.
                let text_intro = qt
                    .create_body1(
                        window,
                        0,
                        0,
                        text::PresetProps {
                            text: w!("This is an example of the Text component's usage."),
                            background: Some(palette.canvas),
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
                                background: Some(palette.canvas),
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

                // A tab strip organizes the controls into pages. Each page is a
                // vertical Stack of sections; switching tabs hides the other pages'
                // controls (the TabList reports the index; the app owns the swap).

                // The menu bar (View / Help) — classic Win32 chrome above the tab
                // strip. Each label opens a dropdown (reusing the flyout menu); picks
                // post WM_COMMAND to this window (see below). Only actions the sample
                // can truly perform are enabled; the rest are shown disabled so the
                // menu is honest about what works. (No File menu — closing is already
                // covered by the footer Close button, the window X, and Alt+F4.)
                let menu_bar = qt
                    .create_menu_bar(
                        window,
                        0,
                        0,
                        menu_bar::Props {
                            items: vec![
                                menu_bar::MenuBarItem {
                                    text: w!("View"),
                                    menu_list: vec![MenuInfo::SubMenu {
                                        text: w!("Theme"),
                                        // A working radio group: pick one, the checkmark
                                        // follows. (The palette swap itself is a
                                        // follow-up; the selection is live today.)
                                        menu_list: vec![
                                            MenuInfo::MenuItemRadio {
                                                text: w!("Web Light"),
                                                command_id: CMD_VIEW_THEME_LIGHT,
                                                checked: theme == AppTheme::WebLight,
                                                disabled: false,
                                                secondary_text: None,
                                                icon: None,
                                            },
                                            MenuInfo::MenuItemRadio {
                                                text: w!("Web Dark"),
                                                command_id: CMD_VIEW_THEME_DARK,
                                                checked: theme == AppTheme::WebDark,
                                                disabled: false,
                                                secondary_text: None,
                                                icon: None,
                                            },
                                        ],
                                    }],
                                },
                                menu_bar::MenuBarItem {
                                    text: w!("Help"),
                                    menu_list: vec![MenuInfo::MenuItem {
                                        text: w!("About…"),
                                        command_id: CMD_HELP_ABOUT,
                                        disabled: false,
                                        secondary_text: None,
                                        icon: None,
                                    }],
                                },
                            ],
                            background: Some(palette.chrome),
                        },
                    )
                    .unwrap_or_default();

                let tabs = qt
                    .create_tab_list(
                        window,
                        0,
                        0,
                        tab_list::Props {
                            tabs: vec![
                                w!("Basic Input"),
                                w!("Collections"),
                                w!("Text"),
                                w!("Status & info"),
                                w!("Menus & toolbars"),
                                w!("Dialogs & flyouts"),
                                w!("Other"),
                            ],
                            selected_index: active,
                            mouse_event: tab_list::MouseEvent {
                                on_change: Box::new(move |_, idx| {
                                    _ = PostMessageW(
                                        Some(window),
                                        WM_APP_TAB,
                                        WPARAM(idx),
                                        LPARAM(0),
                                    );
                                }),
                            },
                            background: Some(palette.chrome),
                            // Selected card matches the page canvas so it reads as
                            // connected to the content below.
                            selected_background: Some(palette.canvas),
                            // Header: stretch to the window width so the bottom line
                            // spans edge to edge.
                            width_behavior: tab_list::WidthBehavior::Fill,
                        },
                    )
                    .unwrap_or_default();

                // A standalone demo TabList for the "Other" page (no-op handler —
                // it just showcases the control; it doesn't drive the app's pages).
                let tablist_label = section(w!("Tab list"));
                let demo_tabs = qt
                    .create_tab_list(
                        window,
                        0,
                        0,
                        tab_list::Props {
                            tabs: vec![w!("First"), w!("Second"), w!("Third")],
                            selected_index: 0,
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();

                // --- Basic Input ---
                let gap_s = qt.theme().tokens.spacing_vertical_s;
                let gap_m = qt.theme().tokens.spacing_horizontal_m;
                let gap_section = qt.theme().tokens.spacing_vertical_xxl;
                let pad_page = qt.theme().tokens.spacing_horizontal_xxl;
                let gap_gutter = qt.theme().tokens.spacing_horizontal_xxxl;

                // Two columns (this page only): the other tabs stay single-column.
                // Left: buttons, checkbox, radio, slider, switch, link. Right: the
                // selection controls (dropdown, combobox), with room to grow.
                let basic_left = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(buttons_label)
                            .add_stack(
                                Stack::horizontal()
                                    .gap(gap_s)
                                    .add(rounded)
                                    .add(circular)
                                    .add(square)
                                    .add(primary),
                            )
                            .add_stack(
                                Stack::vertical()
                                    .gap(gap_s)
                                    .add(small_icon)
                                    .add(icon_after)
                                    .add(large_icon),
                            ),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(split_button_label)
                            .add_stack(
                                Stack::horizontal()
                                    .gap(gap_m)
                                    .add(split_secondary)
                                    .add(split_primary),
                            ),
                    )
                    .add_stack(Stack::vertical().gap(gap_s).add(checkbox_label).add(checkbox))
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(radio_label)
                            .add_stack(
                                Stack::horizontal()
                                    .add(radio_apple)
                                    .add(radio_pear)
                                    .add(radio_banana)
                                    .add(radio_orange),
                            ),
                    )
                    .add_stack(Stack::vertical().gap(gap_s).add(link_label).add(link));
                let basic_right = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(Stack::vertical().gap(gap_s).add(dropdown_label).add(dropdown))
                    .add_stack(Stack::vertical().gap(gap_s).add(combobox_label).add(combobox))
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(spin_button_label)
                            .add(spin_button),
                    )
                    .add_stack(Stack::vertical().gap(gap_s).add(slider_label).add(slider))
                    .add_stack(Stack::vertical().gap(gap_s).add(switch_label).add(switch));
                let basic_input = Stack::horizontal()
                    .gap(gap_gutter)
                    .add_stack(basic_left)
                    .add_stack(basic_right);

                // --- Collections ---
                // (list_box | tree_view) / data_grid
                let collections = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::horizontal()
                            .gap(gap_gutter)
                            .add_stack(
                                Stack::vertical()
                                    .gap(gap_s)
                                    .add(list_box_label)
                                    .add(list_box),
                            )
                            .add_stack(
                                Stack::vertical()
                                    .gap(gap_s)
                                    .add(tree_view_label)
                                    .add(tree_view),
                            ),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(data_grid_label)
                            .add(data_grid),
                    );

                // --- Text ---
                // input, text | textarea
                let text_left = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(inputs_label)
                            .add_stack(
                                Stack::horizontal()
                                    .gap(gap_m)
                                    .add(input_default)
                                    .add(input_filled)
                                    .add(input_filled_darker),
                            )
                            .add(input_password),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
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
                    );
                let text_right = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(textarea_label)
                            .add(textarea),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(search_box_label)
                            .add(search),
                    );
                let text_page = Stack::horizontal()
                    .gap(gap_gutter)
                    .add_stack(text_left)
                    .add_stack(text_right);

                // --- Status & info ---
                // progress_bar, spinner, tooltip
                let status_info = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(progress_label)
                            .add(progress_medium)
                            .add(progress_large),
                    )
                    .add_stack(Stack::vertical().gap(gap_s).add(spinner_label).add(spinner))
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(tooltip_label)
                            .add(tooltip_button),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(message_bar_label)
                            .add(bar_info)
                            .add(bar_success)
                            .add(bar_warning)
                            .add(bar_error),
                    );

                // --- Menus & toolbars ---
                // menu (context-menu hint), tab_list, toolbar
                let menus_toolbars = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(Stack::vertical().gap(gap_s).add(menu_label).add(menu_hint))
                    .add_stack(Stack::vertical().gap(gap_s).add(tablist_label).add(demo_tabs))
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(toolbar_label)
                            .add(toolbar),
                    );

                // --- Dialogs & flyouts ---
                // dialog, task_dialog
                let dialogs_flyouts = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(dialog_label)
                            .add(open_dialog),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(task_dialog_label)
                            .add(open_task_dialog),
                    );

                // --- Other ---
                // divider, calendar, date picker
                let calendar_label = section(w!("Calendar"));
                let calendar = qt
                    .create_calendar(
                        window,
                        0,
                        0,
                        calendar::Props {
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let date_picker_label = section(w!("Date picker"));
                let date_picker = qt
                    .create_date_picker(
                        window,
                        0,
                        0,
                        date_picker::Props {
                            placeholder: w!("Select a date\u{2026}"),
                            width: 260,
                            background: Some(palette.canvas),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                let other = Stack::vertical()
                    .gap(gap_section)
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(divider_label)
                            .add(divider_start)
                            .add(divider_center)
                            .add(divider_plain)
                            .add(divider_subtle)
                            .add(divider_brand)
                            .add(divider_strong),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(date_picker_label)
                            .add(date_picker),
                    )
                    .add_stack(
                        Stack::vertical()
                            .gap(gap_s)
                            .add(calendar_label)
                            .add(calendar),
                    );

                // Each page's own controls (for show/hide) — the strip + Close are
                // always visible, so they're not in these lists.
                let page_contents = [
                    basic_input,
                    collections,
                    text_page,
                    status_info,
                    menus_toolbars,
                    dialogs_flyouts,
                    other,
                ];
                let pages: Vec<Page> = page_contents
                    .into_iter()
                    .map(|content| {
                        let controls = content.controls();
                        // Master Stack per page (arranged against the full window):
                        // flush strip, then padded content, then a spring that pushes
                        // the Close footer to the window's bottom-right. The spring
                        // must live here (not inside the padded inner stack) so it has
                        // the window's leftover height to expand into.
                        let stack = Stack::vertical()
                            .add_fill(menu_bar)
                            .add_fill(tabs)
                            .add_stack(
                                Stack::vertical()
                                    .padding(pad_page)
                                    .gap(gap_section)
                                    .add_stack(content),
                            )
                            .spring()
                            .add_stack(
                                Stack::horizontal().padding(pad_page).spring().add(close),
                            );
                        Page { stack, controls }
                    })
                    .collect();
        AppState {
            qt,
            pages,
            active,
            menu_target: menu_hint,
            tab_list: tabs,
            menu_bar,
            theme,
            palette,
        }
    }
}

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
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let window = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Use Quel'Thalas"),
            // WS_CLIPCHILDREN so the parent never paints under its child controls
            // (kills the tab-switch flash — the #fafafa fill can't touch children).
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
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
                let Ok(qt) = QT::new_with(Theme::web_light()) else {
                    return LRESULT(-1);
                };
                let mut state = Box::new(build_ui(qt, window, AppTheme::WebLight, 0));
                // Show the initial page, hide the rest, and do the initial arrange.
                show_page(&mut state, window, 0);
                SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as _);
                DefWindowProcW(window, message, w_param, l_param)
            }
            WM_APP_TAB => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut AppState;
                if !raw.is_null() {
                    show_page(&mut *raw, window, w_param.0);
                }
                LRESULT(0)
            }
            WM_SIZE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                if !raw.is_null() {
                    arrange_all(&*raw, window);
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                if !raw.is_null() {
                    arrange_all(&*raw, window);
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
                    &dialog::ModelType::Alert,
                    dialog::Actions::OkCancel,
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
                // Two-tone: a chrome band (#f0f0f0) behind the menu bar + tab strip,
                // and the CANVAS page (#fafafa) below it — the selected tab's CANVAS
                // card flares down into the matching page, so it reads as one surface.
                let mut client = RECT::default();
                _ = GetClientRect(window, &mut client);
                // Pull the band edge + the palette's GDI refs from AppState (default
                // to the light palette before it exists / if null).
                let (band, chrome_ref, page_ref) = {
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                    if raw.is_null() {
                        let p = Palette::light();
                        (0, p.chrome_ref, p.page_ref)
                    } else {
                        // The tab strip sits below the menu bar now, so its bottom in
                        // window-client coords (GetWindowRect → ScreenToClient) is the
                        // band edge and covers the menu bar above it automatically.
                        let mut wr = RECT::default();
                        let band = if GetWindowRect((*raw).tab_list, &mut wr).is_ok() {
                            let mut bl = POINT {
                                x: wr.left,
                                y: wr.bottom,
                            };
                            _ = ScreenToClient(window, &mut bl);
                            bl.y
                        } else {
                            0
                        };
                        (band, (*raw).palette.chrome_ref, (*raw).palette.page_ref)
                    }
                };
                let chrome = CreateSolidBrush(chrome_ref);
                let page = CreateSolidBrush(page_ref);
                FillRect(
                    hdc,
                    &RECT {
                        bottom: band,
                        ..client
                    },
                    chrome,
                );
                FillRect(
                    hdc,
                    &RECT {
                        top: band,
                        ..client
                    },
                    page,
                );
                _ = DeleteObject(chrome.into());
                _ = DeleteObject(page.into());
                _ = EndPaint(window, &ps);
                LRESULT(0)
            }
            WM_SYSCOMMAND => {
                // A clean Alt tap / F10 arrives as SC_KEYMENU with no mnemonic char
                // in lParam. Windows never sends this for Alt+Tab (Alt held + another
                // key), so it's the correct trigger — no lingering menu. Forward it
                // to the bar to enter keyboard menu mode.
                if (w_param.0 & 0xfff0) == SC_KEYMENU as usize && l_param.0 == 0 {
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                    if !raw.is_null() {
                        _ = PostMessageW(
                            Some((*raw).menu_bar),
                            menu_bar::WM_ENTER_MENU_MODE,
                            WPARAM(0),
                            LPARAM(0),
                        );
                        return LRESULT(0);
                    }
                }
                DefWindowProcW(window, message, w_param, l_param)
            }
            WM_COMMAND => {
                // Menu-bar picks arrive here (the menu posts WM_COMMAND with the
                // item's command_id in wParam). Theme radios rebuild the whole UI in
                // the picked theme; About shows a dialog.
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const AppState;
                if raw.is_null() {
                    return DefWindowProcW(window, message, w_param, l_param);
                }
                let id = w_param.0 as u32;
                match id {
                    CMD_VIEW_THEME_LIGHT => retheme(window, AppTheme::WebLight),
                    CMD_VIEW_THEME_DARK => retheme(window, AppTheme::WebDark),
                    CMD_HELP_ABOUT => {
                        let qt = &(*raw).qt;
                        _ = qt.open_dialog(
                            window,
                            w!("About"),
                            w!("Quel'Thalas — Fluent-styled Win32 controls."),
                            &dialog::ModelType::Alert,
                            dialog::Actions::Ok,
                        );
                    }
                    CMD_ITEM_A | CMD_ITEM_B => {
                        let qt = &(*raw).qt;
                        let content = match id {
                            CMD_ITEM_A => w!("Item a"),
                            _ => w!("Item b"),
                        };
                        _ = qt.open_dialog(
                            window,
                            w!("Split button"),
                            content,
                            &dialog::ModelType::Alert,
                            dialog::Actions::Ok,
                        );
                    }
                    // Only the Font button demonstrates the action (opens a dialog);
                    // the other formatting buttons are inert in this demo.
                    CMD_TB_FONT => {
                        let qt = &(*raw).qt;
                        _ = qt.open_dialog(
                            window,
                            w!("Toolbar"),
                            w!("Font"),
                            &dialog::ModelType::Alert,
                            dialog::Actions::Ok,
                        );
                    }
                    _ => {}
                }
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
                        icon: None,
                    },
                    MenuInfo::MenuItem {
                        text: w!("New window"),
                        command_id: 1,
                        disabled: false,
                        secondary_text: None,
                        icon: None,
                    },
                    MenuInfo::MenuItem {
                        text: w!("Open file"),
                        command_id: 2,
                        disabled: true,
                        secondary_text: None,
                        icon: None,
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
                                icon: None,
                            },
                            MenuInfo::MenuItem {
                                text: w!("Online services settings"),
                                command_id: 31,
                                disabled: false,
                                secondary_text: None,
                                icon: None,
                            },
                            MenuInfo::MenuDivider,
                            MenuInfo::MenuItem {
                                text: w!("Extensions"),
                                command_id: 32,
                                disabled: false,
                                secondary_text: None,
                                icon: None,
                            },
                            MenuInfo::SubMenu {
                                text: w!("Appearance"),
                                menu_list: vec![
                                    MenuInfo::MenuItem {
                                        text: w!("Centered layout"),
                                        command_id: 30,
                                        disabled: false,
                                        secondary_text: None,
                                        icon: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zen"),
                                        command_id: 31,
                                        disabled: false,
                                        secondary_text: None,
                                        icon: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zoom in"),
                                        command_id: 32,
                                        disabled: true,
                                        secondary_text: None,
                                        icon: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Zoom out"),
                                        command_id: 33,
                                        disabled: false,
                                        secondary_text: None,
                                        icon: None,
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
                                        icon: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Split Down"),
                                        command_id: 41,
                                        disabled: false,
                                        secondary_text: None,
                                        icon: None,
                                    },
                                    MenuInfo::MenuItem {
                                        text: w!("Single"),
                                        command_id: 42,
                                        disabled: false,
                                        secondary_text: None,
                                        icon: None,
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
