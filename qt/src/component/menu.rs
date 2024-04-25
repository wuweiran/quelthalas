use std::cell::RefCell;
use std::mem::size_of;
use std::ops::Deref;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::{
    ERROR_INVALID_WINDOW_HANDLE, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromPoint, RedrawWindow, SetRectEmpty, HDC,
    MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, RDW_ALLCHILDREN, RDW_UPDATENOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_DOWN, VK_END, VK_ESCAPE, VK_F10, VK_HOME, VK_LEFT,
    VK_MENU, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::QT;

pub enum MenuInfo {
    MenuItem {
        text: PCWSTR,
    },
    SubMenu {
        menu_list: Vec<MenuInfo>,
        text: PCWSTR,
    },
    MenuDivider,
}

pub enum MenuItem {
    MenuItem {
        text: PCWSTR,
    },
    SubMenu {
        sub_menu: Rc<RefCell<Menu>>,
        text: PCWSTR,
    },
    MenuDivider,
}

pub struct Menu {
    items: Vec<MenuItem>,
    window: Option<HWND>,
    focused_item_index: Option<usize>,
    menu_list_rect: RECT,
}

pub struct Context {
    qt: QT,
    menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
}

fn convert_menu_info_list_to_menu(menu_info_list: Vec<MenuInfo>) -> Menu {
    let items = menu_info_list
        .into_iter()
        .map(|menu_info| match menu_info {
            MenuInfo::MenuItem { text } => MenuItem::MenuItem { text },
            MenuInfo::SubMenu { menu_list, text } => {
                let sub_menu = convert_menu_info_list_to_menu(menu_list);
                MenuItem::SubMenu {
                    sub_menu: Rc::new(RefCell::new(sub_menu)),
                    text,
                }
            }
            MenuInfo::MenuDivider => MenuItem::MenuDivider,
        })
        .collect();
    Menu {
        items,
        window: None,
        focused_item_index: None,
        menu_list_rect: RECT::default(),
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
        init_popup(self.clone(), parent_window, menu.clone(), x, y);
        init_tracking(parent_window)?;
        track_menu(menu.clone(), 0, 0, parent_window).and(exit_tracking(parent_window))?;
        Ok(())
    }
}

unsafe fn init_popup(qt: QT, owning_window: HWND, menu: Rc<RefCell<Menu>>, x: i32, y: i32) {
    let boxed = Box::new(Context {
        qt,
        menu: menu.clone(),
        owning_window,
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
        HINSTANCE(GetWindowLongPtrW(owning_window, GWLP_HINSTANCE)),
        Some(Box::<Context>::into_raw(boxed) as _),
    );
    menu.borrow_mut().window = Some(window)
}

unsafe fn init_tracking(owning_window: HWND) -> Result<()> {
    HideCaret(None)?;
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
    None
}

fn menu_button_down(mt: &mut Tracker, message: u32, menu: Rc<RefCell<Menu>>) -> bool {
    false
}

fn menu_button_up(mt: &mut Tracker, menu: Rc<RefCell<Menu>>) -> ExecutionResult {
    ExecutionResult::NoExecuted
}

fn menu_mouse_move(mt: &mut Tracker, menu: Rc<RefCell<Menu>>) -> bool {
    false
}

fn select_item(menu: &mut Menu, index: Option<i32>) {}

fn select_previous(menu: &Menu) {}

fn select_next(menu: &Menu) {}

fn select_first(menu: &Menu) {}

fn select_last(menu: &Menu) {}

fn menu_key_left(mt: &mut Tracker, message: u32) {}

fn menu_key_right(mt: &mut Tracker, message: u32) {}

fn menu_key_escape(mt: &mut Tracker) {}

#[derive(PartialEq)]
enum ExecutionResult {
    Executed = 0,
    NoExecuted = -1,
    ShownPopup = -2,
}
fn execute_focused_item(mt: &mut Tracker, menu: &Menu) -> ExecutionResult {
    ExecutionResult::NoExecuted
}

fn hide_sub_popups(menu: &mut Menu) {
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        if let MenuItem::SubMenu { sub_menu, text } = item {
            let mut submenu = sub_menu.borrow_mut();
            hide_sub_popups(&mut submenu);
            select_item(&mut submenu, None);
        }
    }
}

unsafe fn track_menu(menu: Rc<RefCell<Menu>>, x: i32, y: i32, owning_window: HWND) -> Result<bool> {
    let mut menu_mut = menu.borrow_mut();
    if menu_mut.window.is_none() {
        return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
    }
    let window = menu_mut.window.unwrap();

    SetCapture(window);
    let mut remove_message = false;
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
                if CallMsgFilterW(&msg, MSGF_MENU as i32).into() {
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
                        LPARAM(window.0),
                    );
                    MsgWaitForMultipleObjectsEx(
                        None,
                        0xffffffff,
                        QS_ALLINPUT,
                        MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS::default(),
                    );
                }
            }
        }

        if msg.message == WM_CANCELMODE {
            exit_menu = true;
            _ = PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
            break;
        }

        mt.point = msg.pt;
        if msg.hwnd == window || msg.message != WM_TIMER {
            enter_idle_sent = false;
        }

        remove_message = false;
        if msg.message >= WM_MOUSEFIRST && msg.message <= WM_MOUSELAST {
            mt.point.x = msg.lParam.0 as i16 as i32;
            mt.point.y = (msg.lParam.0 >> 16) as i16 as i32;

            let menu_from_point_result = menu_from_point(menu.clone(), &mt.point);

            match msg.message {
                WM_RBUTTONDBLCLK | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONDOWN => {
                    remove_message = match menu_from_point_result {
                        None => false,
                        Some(menu_from_point) => {
                            menu_button_down(&mut mt, msg.message, menu_from_point)
                        }
                    };
                    exit_menu = !remove_message;
                }
                WM_RBUTTONUP | WM_LBUTTONUP => match menu_from_point_result {
                    Some(menu_from_point) => {
                        execution_result = menu_button_up(&mut mt, menu_from_point);
                        remove_message = execution_result != ExecutionResult::NoExecuted;
                        exit_menu = remove_message;
                    }
                    None => exit_menu = false,
                },
                WM_MOUSEMOVE => {
                    if let Some(menu_from_point) = menu_from_point_result {
                        exit_menu = exit_menu | !menu_mouse_move(&mut mt, menu_from_point)
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
                    VK_HOME => select_first(menu.borrow().deref()),
                    VK_END => select_last(menu.borrow().deref()),
                    VK_UP => select_previous(menu.borrow().deref()),
                    VK_DOWN => select_next(menu.borrow().deref()),
                    VK_LEFT => menu_key_left(&mut mt, msg.message),
                    VK_RIGHT => menu_key_right(&mut mt, msg.message),
                    VK_ESCAPE => menu_key_escape(&mut mt),
                    _ => {
                        let _ = TranslateMessage(&mut msg);
                    }
                },
                _ => {}
            }
        } else {
            PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
            DispatchMessageW(&msg);
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
        let mut top_menu = mt.top_menu.borrow_mut();
        hide_sub_popups(&mut top_menu);
        DestroyWindow(window)?;
        menu_mut.window = None;
        select_item(&mut top_menu, None);
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
    ShowCaret(None)?;
    Ok(())
}

unsafe fn calc_popup_menu_size(menu: &mut Menu, max_height: i32) -> (i32, i32) {
    SetRectEmpty(&mut menu.menu_list_rect);
    (0, 0)
}

unsafe fn show_popup(menu: &mut Menu, x: i32, y: i32, x_anchor: i32, y_anchor: i32) -> Result<()> {
    if menu.window.is_none() {
        return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
    }
    let window = menu.window.unwrap();
    menu.focused_item_index = None;
    let pt = POINT { x, y };
    let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    GetMonitorInfoW(monitor, &mut info);
    let max_height = info.rcWork.bottom - info.rcWork.top;
    let (width, height) = calc_popup_menu_size(menu, max_height);
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
    SetWindowPos(
        window,
        HWND_TOPMOST,
        x,
        y,
        width,
        height,
        SWP_SHOWWINDOW | SWP_NOACTIVATE,
    )?;
    RedrawWindow(window, None, None, RDW_UPDATENOW | RDW_ALLCHILDREN);
    Ok(())
}

unsafe fn draw_popup_menu(window: HWND, dc: HDC, menu: &Menu) -> Result<()> {
    Ok(())
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
            let raw = (*cs).lpCreateParams as *mut Context;
            let context = Box::<Context>::from_raw(raw);
            let result = {
                let mut menu = context.menu.borrow_mut();
                show_popup(&mut menu, (*cs).x, (*cs).y, 0, 0)
            };
            match result {
                Ok(_) => {
                    SetWindowLongPtrW(
                        window,
                        GWLP_USERDATA,
                        Box::<Context>::into_raw(context) as _,
                    );
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
            let dc = BeginPaint(window, &mut ps);
            _ = draw_popup_menu(window, dc, context.menu.borrow().deref());
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = draw_popup_menu(
                window,
                HDC(w_param.0 as isize),
                context.menu.borrow().deref(),
            );
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
