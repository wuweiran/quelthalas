use std::cell::RefCell;
use std::mem::size_of;
use std::rc::Rc;

use windows::core::*;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{
    ERROR_INVALID_WINDOW_HANDLE, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DeviceContext5, ID2D1Factory1, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush, ID2D1SvgDocument, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateRoundRectRgn, EndPaint, GetMonitorInfoW, MonitorFromPoint,
    OffsetRect, PtInRect, RedrawWindow, SetRect, SetRectEmpty, SetWindowRgn, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RDW_INVALIDATE, RDW_NOCHILDREN,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_DOWN, VK_END, VK_ESCAPE, VK_F10, VK_HOME, VK_LEFT,
    VK_MENU, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::icon::Icon;
use crate::{get_scaling_factor, QT};

pub enum MenuInfo {
    MenuItem {
        text: PCWSTR,
        command_id: u32,
        disabled: bool,
    },
    SubMenu {
        menu_list: Vec<MenuInfo>,
        text: PCWSTR,
    },
    MenuDivider,
}

enum MenuItem {
    MenuItem {
        text: PCWSTR,
        id: u32,
        rect: RECT,
        disabled: bool,
    },
    SubMenu {
        sub_menu: Rc<RefCell<Menu>>,
        text: PCWSTR,
        rect: RECT,
    },
    MenuDivider {
        rect: RECT,
    },
}

struct Menu {
    items: Vec<MenuItem>,
    window: Option<HWND>,
    focused_item_index: Option<usize>,
    menu_list_rect: RECT,
    is_scrolling: bool,
    scroll_position: i32,
}

pub struct Context {
    qt: QT,
    menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    text_brush: ID2D1SolidColorBrush,
    text_focused_brush: ID2D1SolidColorBrush,
    text_disabled_brush: ID2D1SolidColorBrush,
    sub_menu_indicator_svg: ID2D1SvgDocument,
    sub_menu_indicator_focused_svg: ID2D1SvgDocument,
}

fn convert_menu_info_list_to_menu(menu_info_list: Vec<MenuInfo>) -> Menu {
    let items = menu_info_list
        .into_iter()
        .map(|menu_info| match menu_info {
            MenuInfo::MenuItem {
                text,
                command_id,
                disabled,
            } => MenuItem::MenuItem {
                text,
                id: command_id,
                rect: RECT::default(),
                disabled,
            },
            MenuInfo::SubMenu { menu_list, text } => {
                let sub_menu = convert_menu_info_list_to_menu(menu_list);
                MenuItem::SubMenu {
                    sub_menu: Rc::new(RefCell::new(sub_menu)),
                    text,
                    rect: RECT::default(),
                }
            }
            MenuInfo::MenuDivider => MenuItem::MenuDivider {
                rect: RECT::default(),
            },
        })
        .collect();
    Menu {
        items,
        window: None,
        focused_item_index: None,
        menu_list_rect: RECT::default(),
        is_scrolling: false,
        scroll_position: 0,
    }
}

const CLASS_NAME: PCWSTR = w!("QT_MENU");

impl QT {
    pub unsafe fn open_menu(
        &self,
        parent_window: HWND,
        menu_list: Vec<MenuInfo>,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let window_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpszClassName: CLASS_NAME,
            style: CS_DROPSHADOW | CS_SAVEBITS | CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&window_class);
        if !IsWindow(parent_window).as_bool() {
            return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
        }
        let menu = Rc::new(RefCell::new(convert_menu_info_list_to_menu(menu_list)));
        init_popup(self.clone(), parent_window, menu.clone(), x, y, 0, 0)?;
        init_tracking(parent_window)?;
        track_menu(menu.clone(), 0, 0, parent_window).and(exit_tracking(parent_window))?;
        Ok(())
    }
}

pub struct CreateParams {
    qt: QT,
    menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
    x_anchor: i32,
    y_anchor: i32,
}

unsafe fn init_popup(
    qt: QT,
    owning_window: HWND,
    menu: Rc<RefCell<Menu>>,
    x: i32,
    y: i32,
    x_anchor: i32,
    y_anchor: i32,
) -> Result<()> {
    let boxed = Box::new(CreateParams {
        qt,
        menu: menu.clone(),
        owning_window,
        x_anchor,
        y_anchor,
    });
    let window = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        CLASS_NAME,
        w!(""),
        WS_POPUP,
        x,
        y,
        0,
        0,
        owning_window,
        None,
        HINSTANCE(GetWindowLongPtrW(owning_window, GWLP_HINSTANCE) as _),
        Some(Box::<CreateParams>::into_raw(boxed) as _),
    )?;
    menu.borrow_mut().window = Some(window);
    Ok(())
}

unsafe fn init_tracking(owning_window: HWND) -> Result<()> {
    _ = HideCaret(None);
    SendMessageW(
        owning_window,
        WM_ENTERMENULOOP,
        WPARAM(TRUE.0 as usize),
        LPARAM(0),
    );
    SendMessageW(
        owning_window,
        WM_SETCURSOR,
        WPARAM(owning_window.0 as usize),
        LPARAM(HTCAPTION as isize),
    );
    Ok(())
}

struct Tracker {
    current_menu: Rc<RefCell<Menu>>,
    top_menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
    point: POINT,
}

fn menu_from_point(root: Rc<RefCell<Menu>>, point: &POINT) -> Option<Rc<RefCell<Menu>>> {
    let menu = root.borrow();
    let mut result: Option<Rc<RefCell<Menu>>> = None;
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        if let MenuItem::SubMenu { sub_menu, .. } = item {
            result = menu_from_point(sub_menu.clone(), point)
        }
    }
    if let None = result {
        let hit_window = unsafe { WindowFromPoint(*point) };
        if menu.window == Some(hit_window) {
            result = Some(root.clone())
        }
    };
    result
}

fn adjust_menu_item_rect(menu: &Menu, rect: &RECT) -> RECT {
    let scroll_offset = if menu.is_scrolling {
        menu.scroll_position
    } else {
        0
    };
    let mut rect = rect.clone();
    unsafe {
        let _ = OffsetRect(
            &mut rect,
            menu.menu_list_rect.left,
            menu.menu_list_rect.top - scroll_offset,
        );
    }
    rect
}

#[derive(PartialEq)]
enum HitTest {
    Nowhere,
    Border,
    Item(usize),
    ScrollUp,
    ScrollDown,
}

fn find_item_by_coordinates(menu: &Menu, point: &mut POINT) -> HitTest {
    let mut rect = RECT::default();
    if let Some(window) = menu.window {
        unsafe {
            if GetWindowRect(window, &mut rect).is_err() {
                return HitTest::Nowhere;
            }
            if !PtInRect(&rect, *point).as_bool() {
                return HitTest::Nowhere;
            }

            point.x -= rect.left;
            point.y -= rect.top;

            let scaling_factor = get_scaling_factor(window);
            point.x = (point.x as f32 / scaling_factor) as i32;
            point.y = (point.y as f32 / scaling_factor) as i32;

            if !PtInRect(&menu.menu_list_rect, *point).as_bool() {
                if !menu.is_scrolling
                    || point.x < menu.menu_list_rect.left
                    || point.x >= menu.menu_list_rect.right
                {
                    return HitTest::Border;
                }

                // On a scroll arrow. Update point so that it points to the item just outside menu_list_rect
                if point.y < menu.menu_list_rect.top {
                    point.y = menu.menu_list_rect.top - 1;
                    return HitTest::ScrollUp;
                } else {
                    point.y = menu.menu_list_rect.bottom;
                    return HitTest::ScrollDown;
                }
            }

            for (index, item) in menu.items.iter().enumerate() {
                match item {
                    MenuItem::MenuItem {
                        rect: item_rect, ..
                    }
                    | MenuItem::SubMenu {
                        rect: item_rect, ..
                    }
                    | MenuItem::MenuDivider {
                        rect: item_rect, ..
                    } => {
                        let rect = adjust_menu_item_rect(menu, item_rect);
                        if PtInRect(&rect, *point).as_bool() {
                            return HitTest::Item(index);
                        }
                    }
                }
            }
        }
    }
    return HitTest::Nowhere;
}

fn switch_tracking(menu: &mut Menu, new_index: usize) -> Result<()> {
    hide_sub_popups(menu)?;
    select_item(menu, Some(new_index));
    Ok(())
}

unsafe fn menu_button_down(context: &Context, mt: &mut Tracker, menu: &mut Menu) -> Result<bool> {
    let ht = find_item_by_coordinates(menu, &mut mt.point);
    if let HitTest::Item(item_index) = ht {
        if menu.focused_item_index != Some(item_index) {
            switch_tracking(menu, item_index)?;
        }
        let item = &menu.items[item_index];
        if let MenuItem::SubMenu { sub_menu, .. } = item {
            if sub_menu.borrow().window.is_none() {
                mt.current_menu =
                    show_sub_popup(&context.qt, context.owning_window, sub_menu.clone())?;
            }
        }
    }

    match ht {
        HitTest::Nowhere => Ok(false),
        HitTest::Item(_) => Ok(true),
        _ => Ok(true),
    }
}

unsafe fn menu_button_up(
    context: &Context,
    mt: &mut Tracker,
    menu: &mut Menu,
) -> Result<ExecutionResult> {
    if let HitTest::Item(item_index) = find_item_by_coordinates(menu, &mut mt.point) {
        if menu.focused_item_index == Some(item_index) {
            if let MenuItem::SubMenu { .. } = menu.items[item_index] {
            } else {
                let execution_result = execute_focused_item(context, mt, &menu)?;
                return if execution_result == ExecutionResult::NoExecuted
                    || execution_result == ExecutionResult::ShownPopup
                {
                    Ok(ExecutionResult::NoExecuted)
                } else {
                    Ok(execution_result)
                };
            }
        }
    }
    Ok(ExecutionResult::NoExecuted)
}

unsafe fn menu_mouse_move(
    context: &Context,
    mt: &mut Tracker,
    menu: Rc<RefCell<Menu>>,
) -> Result<bool> {
    let item_index_option = {
        let menu_borrow = menu.borrow_mut();
        find_item_by_coordinates(&menu_borrow, &mut mt.point)
    };

    if let HitTest::Item(item_index) = item_index_option {
        let focused_item_index = {
            let menu_borrow = menu.borrow();
            menu_borrow.focused_item_index
        };

        if focused_item_index != Some(item_index) {
            {
                let mut menu_borrow = menu.borrow_mut();
                switch_tracking(&mut menu_borrow, item_index)?;
            }
            mt.current_menu = show_sub_popup(&context.qt, context.owning_window, menu)?;
        }
    } else {
        let mut menu_borrow = menu.borrow_mut();
        hide_sub_popups(&mut menu_borrow)?;
        select_item(&mut menu_borrow, None);
    }

    Ok(true)
}

fn select_item(menu: &mut Menu, index: Option<usize>) {
    if menu.focused_item_index == index {
        return;
    }
    menu.focused_item_index = index;
    unsafe {
        if let Some(window) = menu.window {
            _ = RedrawWindow(window, None, None, RDW_INVALIDATE | RDW_NOCHILDREN);
        }
    }
}

fn select_previous(menu: &mut Menu) {
    if let Some(mut item_index) = menu.focused_item_index {
        while item_index > 0 {
            item_index = item_index - 1;
            if let MenuItem::MenuDivider { .. } = menu.items[item_index] {
                continue;
            }
            select_item(menu, Some(item_index));
            break;
        }
    }
}

fn select_next(menu: &mut Menu) {
    if let Some(mut item_index) = menu.focused_item_index {
        while item_index + 1 < menu.items.len() {
            item_index = item_index + 1;
            if let MenuItem::MenuDivider { .. } = menu.items[item_index] {
                continue;
            }
            select_item(menu, Some(item_index));
            break;
        }
    }
}

fn select_first(menu: &mut Menu) {
    let mut item_index = 0;
    while item_index < menu.items.len() {
        if let MenuItem::MenuDivider { .. } = menu.items[item_index] {
            item_index = item_index + 1;
            continue;
        }
        select_item(menu, Some(item_index));
        break;
    }
}

fn select_last(menu: &mut Menu) {
    let mut item_index = menu.items.len() as isize - 1;
    while item_index >= 0 {
        if let MenuItem::MenuDivider { .. } = menu.items[item_index as usize] {
            item_index = item_index - 1;
            continue;
        }
        select_item(menu, Some(item_index as usize));
        break;
    }
}

fn menu_key_left(mt: &mut Tracker) -> Result<()> {
    let mut tmp_menu = mt.top_menu.clone();
    let mut prev_menu = mt.top_menu.clone();

    // close topmost popup
    while !Rc::ptr_eq(&tmp_menu, &mt.current_menu) {
        prev_menu = tmp_menu.clone();
        let prev_menu_borrowed = prev_menu.borrow();
        tmp_menu = get_sub_popup(&prev_menu_borrowed).unwrap();
    }

    {
        let mut prev_menu_borrowed = prev_menu.borrow_mut();
        hide_sub_popups(&mut prev_menu_borrowed)?;
    }
    mt.current_menu = prev_menu.clone();

    Ok(())
}

unsafe fn menu_key_right(context: &Context, mt: &mut Tracker) -> Result<()> {
    mt.current_menu = show_sub_popup(&context.qt, mt.owning_window, mt.current_menu.clone())?;
    Ok(())
}

fn get_sub_popup(menu: &Menu) -> Option<Rc<RefCell<Menu>>> {
    if let Some(item_index) = menu.focused_item_index {
        if let MenuItem::SubMenu { sub_menu, .. } = &menu.items[item_index] {
            let sub_menu_borrowed = sub_menu.borrow();
            if sub_menu_borrowed.window.is_some() {
                return Some(sub_menu.clone());
            }
        }
    }
    None
}

fn menu_key_escape(mt: &mut Tracker) -> Result<bool> {
    if !Rc::ptr_eq(&mt.current_menu, &mt.top_menu) {
        let mut top = mt.top_menu.clone();
        let mut prev_menu = top.clone();
        while !Rc::ptr_eq(&top, &mt.current_menu) {
            prev_menu = top;
            let prev_menu_borrowed = prev_menu.borrow();
            match get_sub_popup(&prev_menu_borrowed) {
                None => {
                    break;
                }
                Some(sub_popup) => {
                    top = sub_popup;
                }
            }
        }
        let mut prev_menu_borrowed = prev_menu.borrow_mut();
        hide_sub_popups(&mut prev_menu_borrowed)?;
        mt.current_menu = prev_menu.clone();
        return Ok(false);
    }
    return Ok(true);
}

const MENU_MARGIN: i32 = 4;
const MENU_BORDER_WIDTH: i32 = 1;
const MENU_LIST_GAP: i32 = 2;

#[derive(PartialEq)]
enum ExecutionResult {
    Executed = 0,
    NoExecuted = -1,
    ShownPopup = -2,
}

unsafe fn show_sub_popup(
    qt: &QT,
    owning_window: HWND,
    menu: Rc<RefCell<Menu>>,
) -> Result<Rc<RefCell<Menu>>> {
    {
        let menu = menu.borrow_mut();
        if let Some(focused_item_index) = menu.focused_item_index {
            let item = &menu.items[focused_item_index];
            if let MenuItem::SubMenu {
                sub_menu,
                rect: item_rect,
                ..
            } = item
            {
                if let Some(window) = menu.window {
                    let item_rect = adjust_menu_item_rect(&menu, &item_rect);
                    let mut rect = RECT::default();
                    GetWindowRect(window, &mut rect)?;
                    let scaling_factor = get_scaling_factor(window);
                    rect.left +=
                        ((item_rect.right - MENU_BORDER_WIDTH) as f32 * scaling_factor) as i32;
                    rect.top += (item_rect.top as f32 * scaling_factor) as i32;
                    rect.right = ((item_rect.left - item_rect.right + MENU_BORDER_WIDTH) as f32
                        * scaling_factor) as i32;
                    rect.bottom = ((item_rect.top - item_rect.bottom - 2 * MENU_MARGIN) as f32
                        * scaling_factor) as i32;
                    init_popup(
                        qt.clone(),
                        owning_window,
                        sub_menu.clone(),
                        rect.left,
                        rect.top,
                        rect.right,
                        rect.bottom,
                    )?;
                    return Ok(sub_menu.clone());
                }
            }
        }
    }
    Ok(menu)
}

fn hide_sub_popups(menu: &mut Menu) -> Result<()> {
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        if let MenuItem::SubMenu { sub_menu, .. } = item {
            let mut sub_menu = sub_menu.borrow_mut();
            hide_sub_popups(&mut sub_menu)?;
            select_item(&mut sub_menu, None);
            if let Some(sub_menu_window) = sub_menu.window {
                unsafe { DestroyWindow(sub_menu_window)? };
                sub_menu.window = None;
            }
        }
    }
    Ok(())
}
unsafe fn execute_focused_item(
    context: &Context,
    mt: &mut Tracker,
    menu: &Menu,
) -> Result<ExecutionResult> {
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        match item {
            MenuItem::MenuItem { id, disabled, .. } => unsafe {
                if *disabled {
                    Ok(ExecutionResult::NoExecuted)
                } else {
                    PostMessageW(
                        mt.owning_window,
                        WM_COMMAND,
                        WPARAM(*id as usize),
                        LPARAM(0),
                    )?;
                    Ok(ExecutionResult::Executed)
                }
            },
            MenuItem::SubMenu { sub_menu, .. } => {
                mt.current_menu =
                    show_sub_popup(&context.qt, context.owning_window, sub_menu.clone())?;
                Ok(ExecutionResult::ShownPopup)
            }
            MenuItem::MenuDivider { .. } => Ok(ExecutionResult::NoExecuted),
        }
    } else {
        Ok(ExecutionResult::NoExecuted)
    }
}

unsafe fn track_menu(menu: Rc<RefCell<Menu>>, x: i32, y: i32, owning_window: HWND) -> Result<bool> {
    let window = {
        let menu = menu.borrow();
        if menu.window.is_none() {
            return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
        }
        menu.window.unwrap()
    };

    SetCapture(window);
    let mut mt = Tracker {
        current_menu: menu.clone(),
        top_menu: menu.clone(),
        owning_window,
        point: POINT { x, y },
    };
    let mut exit_menu = false;
    let mut enter_idle_sent = false;
    let mut execution_result = ExecutionResult::NoExecuted;
    while !exit_menu {
        let mut msg = MSG::default();
        loop {
            if PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE).into() {
                if !CallMsgFilterW(&msg, MSGF_MENU as i32).as_bool() {
                    break;
                }
                _ = PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
            } else {
                if !enter_idle_sent {
                    enter_idle_sent = true;
                    SendMessageW(
                        owning_window,
                        WM_ENTERIDLE,
                        WPARAM(MSGF_MENU as usize),
                        LPARAM(window.0 as _),
                    );
                }
                MsgWaitForMultipleObjectsEx(
                    None,
                    0xffffffff,
                    QS_ALLINPUT,
                    MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS::default(),
                );
            }
        }

        if msg.message == WM_CANCELMODE {
            _ = PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
            break;
        }

        mt.point = msg.pt;
        if msg.hwnd == window || msg.message != WM_TIMER {
            enter_idle_sent = false;
        }

        let mut remove_message = false;
        if msg.message >= WM_MOUSEFIRST && msg.message <= WM_MOUSELAST {
            mt.point.x = msg.lParam.0 as i16 as i32;
            mt.point.y = (msg.lParam.0 >> 16) as i16 as i32;
            _ = ClientToScreen(window, &mut mt.point);

            let menu_from_point_result = menu_from_point(menu.clone(), &mt.point);

            match msg.message {
                WM_RBUTTONDBLCLK | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONDOWN => {
                    remove_message = match menu_from_point_result {
                        None => false,
                        Some(menu_from_point) => {
                            let mut menu_from_point_borrowed = menu_from_point.borrow_mut();
                            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                            let context = &*raw;
                            menu_button_down(context, &mut mt, &mut menu_from_point_borrowed)?
                        }
                    };
                    exit_menu = !remove_message;
                }
                WM_RBUTTONUP | WM_LBUTTONUP => match menu_from_point_result {
                    Some(menu_from_point) => {
                        let mut menu_from_point_borrowed = menu_from_point.borrow_mut();
                        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                        let context = &*raw;
                        execution_result =
                            menu_button_up(context, &mut mt, &mut menu_from_point_borrowed)?;
                        remove_message = execution_result != ExecutionResult::NoExecuted;
                        exit_menu = remove_message;
                    }
                    None => exit_menu = false,
                },
                WM_MOUSEMOVE => {
                    if let Some(menu_from_point) = menu_from_point_result {
                        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                        let context = &*raw;
                        exit_menu = exit_menu | !menu_mouse_move(context, &mut mt, menu_from_point)?
                    }
                }
                _ => {}
            }
        } else if msg.message >= WM_KEYFIRST && msg.message <= WM_KEYLAST {
            remove_message = true;
            match msg.message {
                WM_KEYDOWN | WM_SYSKEYDOWN => match VIRTUAL_KEY(msg.wParam.0 as u16) {
                    VK_MENU | VK_F10 => {
                        exit_menu = true;
                    }
                    VK_HOME => {
                        let mut menu = mt.current_menu.borrow_mut();
                        select_first(&mut menu)
                    }
                    VK_END => {
                        let mut menu = mt.current_menu.borrow_mut();
                        select_last(&mut menu)
                    }
                    VK_UP => {
                        let mut menu = mt.current_menu.borrow_mut();
                        select_previous(&mut menu)
                    }
                    VK_DOWN => {
                        let mut menu = mt.current_menu.borrow_mut();
                        select_next(&mut menu)
                    }
                    VK_LEFT => menu_key_left(&mut mt)?,
                    VK_RIGHT => {
                        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                        let context = &*raw;
                        menu_key_right(context, &mut mt)?
                    }
                    VK_ESCAPE => exit_menu = menu_key_escape(&mut mt)?,
                    _ => {
                        let _ = TranslateMessage(&mut msg);
                    }
                },
                _ => {}
            }
        } else {
            if PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE).as_bool() {
                DispatchMessageW(&msg);
            }
            continue;
        }

        if !exit_menu {
            remove_message = true;
        }

        if remove_message {
            PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
        }
    }

    ReleaseCapture()?;
    if IsWindow(mt.owning_window).into() {
        {
            let mut top_menu = mt.top_menu.borrow_mut();
            hide_sub_popups(&mut top_menu)?;
        }
        {
            DestroyWindow(window)?;
            let mut menu_mut = menu.borrow_mut();
            menu_mut.window = None;
        }
        {
            let mut top_menu = mt.top_menu.borrow_mut();
            select_item(&mut top_menu, None);
        }
    }
    Ok(execution_result != ExecutionResult::ShownPopup)
}

unsafe fn exit_tracking(owning_window: HWND) -> Result<()> {
    SendMessageW(
        owning_window,
        WM_EXITMENULOOP,
        WPARAM(TRUE.0 as usize),
        LPARAM(0),
    );
    _ = ShowCaret(None);
    Ok(())
}

unsafe fn calc_menu_item_size(
    qt: &QT,
    menu_item: &mut MenuItem,
    org_x: i32,
    org_y: i32,
    text_format: &IDWriteTextFormat,
) -> Result<()> {
    let tokens = &qt.theme.tokens;
    match menu_item {
        MenuItem::MenuItem { rect, text, .. } | MenuItem::SubMenu { rect, text, .. } => {
            SetRect(rect, org_x, org_y, org_x, org_y);
            let direct_write_factory =
                DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
            let text_layout = direct_write_factory.CreateTextLayout(
                text.as_wide(),
                text_format,
                290f32,
                500f32,
            )?;
            let mut metrics = DWRITE_TEXT_METRICS::default();
            text_layout.GetMetrics(&mut metrics)?;
            rect.right += metrics.width.ceil() as i32 + 2 * tokens.spacing_vertical_s_nudge as i32;
            rect.bottom +=
                (metrics.height.ceil() as i32 + 2 * tokens.spacing_vertical_s_nudge as i32).max(32);
        }
        MenuItem::MenuDivider { rect } => {
            SetRect(rect, org_x, org_y, org_x, org_y);
            rect.bottom += 4 + tokens.stroke_width_thin as i32;
        }
    }
    if let MenuItem::SubMenu { rect, .. } = menu_item {
        rect.right = rect.right + 4 + 20;
    }
    Ok(())
}

unsafe fn get_text_format(qt: &QT) -> Result<IDWriteTextFormat> {
    let direct_write_factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
    let tokens = &qt.theme.tokens;
    direct_write_factory.CreateTextFormat(
        tokens.font_family_base,
        None,
        tokens.font_weight_regular,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        tokens.font_size_base300,
        w!(""),
    )
}

unsafe fn calc_popup_menu_size(qt: &QT, menu: &mut Menu, max_height: i32) -> Result<(i32, i32)> {
    SetRectEmpty(&mut menu.menu_list_rect);
    let mut start = 0;
    let text_format = get_text_format(qt)?;
    while start < menu.items.len() {
        let org_x = menu.menu_list_rect.right;
        let mut org_y = menu.menu_list_rect.top;

        let mut i = start;
        while i < menu.items.len() {
            let item = &mut menu.items[i];
            calc_menu_item_size(qt, item, org_x, org_y, &text_format)?;
            let desired_width = match item {
                MenuItem::MenuItem { rect, .. }
                | MenuItem::SubMenu { rect, .. }
                | MenuItem::MenuDivider { rect } => rect.right,
            };
            let desired_height = match item {
                MenuItem::MenuItem { rect, .. }
                | MenuItem::SubMenu { rect, .. }
                | MenuItem::MenuDivider { rect } => rect.bottom,
            };

            menu.menu_list_rect.right = menu.menu_list_rect.right.max(desired_width);
            org_y = desired_height + MENU_LIST_GAP;

            i = i + 1;
        }
        org_y -= MENU_LIST_GAP;
        menu.menu_list_rect.right = menu.menu_list_rect.right.max(138);
        while start < i {
            let item = &mut menu.items[start];
            match item {
                MenuItem::MenuItem { rect, .. }
                | MenuItem::SubMenu { rect, .. }
                | MenuItem::MenuDivider { rect } => rect.right = menu.menu_list_rect.right,
            }
            start = start + 1;
        }
        menu.menu_list_rect.bottom = menu.menu_list_rect.bottom.max(org_y);
    }

    OffsetRect(
        &mut menu.menu_list_rect,
        MENU_BORDER_WIDTH + MENU_MARGIN,
        MENU_BORDER_WIDTH + MENU_MARGIN,
    );
    let mut height = menu.menu_list_rect.bottom + MENU_BORDER_WIDTH + MENU_MARGIN;
    let width = menu.menu_list_rect.right + MENU_BORDER_WIDTH + MENU_MARGIN;
    if height >= max_height {
        height = max_height;
        menu.is_scrolling = true;
        menu.menu_list_rect.top = MENU_MARGIN;
        menu.menu_list_rect.bottom = height - MENU_MARGIN;
    }

    Ok((width, height))
}

unsafe fn show_popup(
    qt: &QT,
    window: HWND,
    menu: &mut Menu,
    x: i32,
    y: i32,
    x_anchor: i32,
    y_anchor: i32,
) -> Result<()> {
    menu.focused_item_index = None;
    let pt = POINT { x, y };
    let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    GetMonitorInfoW(monitor, &mut info);
    let max_height = info.rcWork.bottom - info.rcWork.top;
    let (width, height) = calc_popup_menu_size(qt, menu, max_height)?;
    let mut x = x;
    if x + width > info.rcWork.right {
        if x_anchor != 0 && x >= width - x_anchor {
            x = x - width - x_anchor;
        }
        if x + width > info.rcWork.right {
            x = info.rcWork.right - width;
        }
    }
    if x < info.rcWork.left {
        x = info.rcWork.left;
    }
    let mut y = y;
    if y + height > info.rcWork.bottom {
        if y_anchor != 0 && y >= height + y_anchor {
            y -= height + y_anchor;
        }
        if y + height > info.rcWork.bottom {
            y = info.rcWork.bottom - height;
        }
    }
    if y < info.rcWork.top {
        y = info.rcWork.top;
    }
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (width as f32 * scaling_factor) as i32;
    let scaled_height = (height as f32 * scaling_factor) as i32;
    SetWindowPos(
        window,
        HWND_TOPMOST,
        x,
        y,
        scaled_width,
        scaled_height,
        SWP_SHOWWINDOW | SWP_NOACTIVATE,
    )?;
    let corner_diameter = (qt.theme.tokens.border_radius_medium * 2f32 * scaling_factor) as i32;
    let region = CreateRoundRectRgn(
        0,
        0,
        scaled_width + 1,
        scaled_height + 1,
        corner_diameter,
        corner_diameter,
    );
    SetWindowRgn(window, region, FALSE);
    Ok(())
}

unsafe fn draw_menu_item(
    menu: &Menu,
    menu_item: &MenuItem,
    context: &Context,
    focused: bool,
) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    let rect = match menu_item {
        MenuItem::MenuItem {
            rect: item_rect, ..
        }
        | MenuItem::SubMenu {
            rect: item_rect, ..
        }
        | MenuItem::MenuDivider { rect: item_rect } => adjust_menu_item_rect(menu, item_rect),
    };
    if focused {
        let show_focused = match menu_item {
            MenuItem::MenuItem { disabled, .. } => !*disabled,
            MenuItem::SubMenu { .. } => true,
            MenuItem::MenuDivider { .. } => false,
        };
        if show_focused {
            let focused_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_background1_hover, None)?;
            let rounded_rect = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: rect.left as f32,
                    top: rect.top as f32,
                    right: rect.right as f32,
                    bottom: rect.bottom as f32,
                },
                radiusX: tokens.border_radius_medium,
                radiusY: tokens.border_radius_medium,
            };
            context
                .render_target
                .FillRoundedRectangle(&rounded_rect, &focused_brush);
        }
    }
    match menu_item {
        MenuItem::MenuItem { text, disabled, .. } => {
            let text_rect = D2D_RECT_F {
                left: rect.left as f32 + tokens.spacing_vertical_s_nudge,
                top: rect.top as f32 + tokens.spacing_vertical_s_nudge,
                right: rect.right as f32 - tokens.spacing_vertical_s_nudge,
                bottom: rect.bottom as f32 - tokens.spacing_vertical_s_nudge,
            };
            let text_brush = if *disabled {
                &context.text_disabled_brush
            } else if focused {
                &context.text_focused_brush
            } else {
                &context.text_brush
            };
            context.render_target.DrawText(
                text.as_wide(),
                &context.text_format,
                &text_rect,
                text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        MenuItem::SubMenu { text, .. } => {
            let text_rect = D2D_RECT_F {
                left: rect.left as f32 + tokens.spacing_vertical_s_nudge,
                top: rect.top as f32 + tokens.spacing_vertical_s_nudge,
                right: (rect.right - 4 - 20) as f32 - tokens.spacing_vertical_s_nudge,
                bottom: rect.bottom as f32 - tokens.spacing_vertical_s_nudge,
            };
            context.render_target.DrawText(
                text.as_wide(),
                &context.text_format,
                &text_rect,
                &context.text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
            device_context5.SetTransform(&Matrix3x2::translation(
                rect.right as f32 - tokens.spacing_vertical_s_nudge - 4f32 - 20f32,
                rect.top as f32 + tokens.spacing_vertical_s_nudge,
            ));
            let svg = if focused {
                &context.sub_menu_indicator_focused_svg
            } else {
                &context.sub_menu_indicator_svg
            };
            device_context5.DrawSvgDocument(svg);
            device_context5.SetTransform(&Matrix3x2::identity());
        }
        MenuItem::MenuDivider { .. } => {
            let start = D2D_POINT_2F {
                x: (rect.left - MENU_MARGIN) as f32,
                y: rect.top as f32 + 2.0,
            };
            let end = D2D_POINT_2F {
                x: (rect.right + MENU_MARGIN) as f32,
                y: rect.top as f32 + 2.0,
            };
            let divider_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_stroke2, None)?;
            context.render_target.DrawLine(
                start,
                end,
                &divider_brush,
                tokens.stroke_width_thin,
                None,
            );
        }
    }
    Ok(())
}

unsafe fn draw_scroll_arrows(window: HWND, context: &Context) -> Result<()> {
    // TODO
    Ok(())
}

unsafe fn draw_popup_menu(window: HWND, context: &Context) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    context.render_target.BeginDraw();
    context
        .render_target
        .Clear(Some(&tokens.color_neutral_background1));
    let menu = context.menu.borrow();
    for (index, item) in menu.items.iter().enumerate() {
        draw_menu_item(&menu, item, context, Some(index) == menu.focused_item_index)?;
    }
    if menu.is_scrolling {
        draw_scroll_arrows(window, context)?;
    }
    context.render_target.EndDraw(None, None)?;
    Ok(())
}

unsafe fn on_create(window: HWND, params: CreateParams, x: i32, y: i32) -> Result<Context> {
    {
        let mut menu = params.menu.borrow_mut();
        show_popup(
            &params.qt,
            window,
            &mut menu,
            x,
            y,
            params.x_anchor,
            params.y_anchor,
        )?;
    }

    let mut client_rect = RECT::default();
    GetClientRect(window, &mut client_rect)?;
    let dpi = GetDpiForWindow(window);
    let factory = D2D1CreateFactory::<ID2D1Factory1>(
        D2D1_FACTORY_TYPE_SINGLE_THREADED,
        Some(&D2D1_FACTORY_OPTIONS::default()),
    )?;
    let render_target = factory.CreateHwndRenderTarget(
        &D2D1_RENDER_TARGET_PROPERTIES {
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            ..Default::default()
        },
        &D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: window,
            pixelSize: D2D_SIZE_U {
                width: (client_rect.right - client_rect.left) as u32,
                height: (client_rect.bottom - client_rect.top) as u32,
            },
            presentOptions: Default::default(),
        },
    )?;
    let text_format = get_text_format(&params.qt)?;
    let tokens = &params.qt.theme.tokens;
    let text_brush =
        render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground2, None)?;
    let text_focused_brush =
        render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground1_hover, None)?;
    let text_disabled_brush =
        render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground_disabled, None)?;
    let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
    let sub_menu_indicator_icon = Icon::chevron_right_regular();
    let sub_menu_indicator_svg =
        match SHCreateMemStream(Some(sub_menu_indicator_icon.svg.as_bytes())) {
            None => device_context5.CreateSvgDocument(
                None,
                D2D_SIZE_F {
                    width: sub_menu_indicator_icon.size as f32,
                    height: sub_menu_indicator_icon.size as f32,
                },
            )?,
            Some(svg_stream) => device_context5.CreateSvgDocument(
                &svg_stream,
                D2D_SIZE_F {
                    width: sub_menu_indicator_icon.size as f32,
                    height: sub_menu_indicator_icon.size as f32,
                },
            )?,
        };
    let sub_menu_indicator_focused_icon = Icon::chevron_right_filled();
    let sub_menu_indicator_focused_svg =
        match SHCreateMemStream(Some(sub_menu_indicator_focused_icon.svg.as_bytes())) {
            None => device_context5.CreateSvgDocument(
                None,
                D2D_SIZE_F {
                    width: sub_menu_indicator_focused_icon.size as f32,
                    height: sub_menu_indicator_focused_icon.size as f32,
                },
            )?,
            Some(svg_stream) => device_context5.CreateSvgDocument(
                &svg_stream,
                D2D_SIZE_F {
                    width: sub_menu_indicator_focused_icon.size as f32,
                    height: sub_menu_indicator_focused_icon.size as f32,
                },
            )?,
        };
    Ok(Context {
        qt: params.qt,
        menu: params.menu,
        owning_window: params.owning_window,
        render_target,
        text_format,
        text_brush,
        text_focused_brush,
        text_disabled_brush,
        sub_menu_indicator_svg,
        sub_menu_indicator_focused_svg,
    })
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut CreateParams;
            let params = Box::<CreateParams>::from_raw(raw);
            match on_create(window, *params, (*cs).x, (*cs).y) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = draw_popup_menu(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = draw_popup_menu(window, context);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
