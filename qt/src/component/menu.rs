use std::mem::size_of;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::{
    ERROR_INVALID_WINDOW_HANDLE, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetCapture, VIRTUAL_KEY, VK_DOWN, VK_END, VK_F10, VK_HOME, VK_MENU, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::QT;

pub enum MenuInfo {
    MenuItem {
        sub_menu_list: Vec<MenuInfo>,
        text: PCWSTR,
    },
    MenuDivider,
}

pub enum MenuItem {
    MenuItem { sub_menu: Rc<Menu>, text: PCWSTR },
    MenuDivider,
}

pub struct Menu {
    items: Vec<MenuItem>,
}

pub struct Context {
    qt: QT,
    menu: Rc<Menu>,
    owning_window: HWND,
}

fn convert_menu_info_list_to_menu(menu_info_list: Vec<MenuInfo>) -> Menu {
    let items = menu_info_list
        .into_iter()
        .map(|menu_info| match menu_info {
            MenuInfo::MenuItem {
                sub_menu_list,
                text,
            } => {
                let sub_menu = convert_menu_info_list_to_menu(sub_menu_list);
                MenuItem::MenuItem {
                    sub_menu: Rc::new(sub_menu),
                    text,
                }
            }
            MenuInfo::MenuDivider => MenuItem::MenuDivider,
        })
        .collect();
    Menu { items }
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
        let menu = Rc::new(convert_menu_info_list_to_menu(menu_list));
        let window = init_popup(self.clone(), parent_window, menu.clone(), x, y);
        init_tracking(parent_window)?;
        track_menu(window, menu.clone(), 0, 0, parent_window).and(exit_tracking(parent_window))?;
        Ok(())
    }
}

unsafe fn init_popup(qt: QT, owning_window: HWND, menu: Rc<Menu>, x: i32, y: i32) -> HWND {
    let boxed = Box::new(Context {
        qt,
        menu,
        owning_window,
    });
    CreateWindowExW(
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
    )
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
    current_menu: Rc<Menu>,
    top_menu: Rc<Menu>,
    owning_window: HWND,
    point: POINT,
}

fn menu_from_point(root: Rc<Menu>, point: &POINT) -> Option<Rc<Menu>> {
    None
}

fn menu_button_down(mt: &mut Tracker, message: u32, menu: Rc<Menu>) -> bool {
    false
}

fn menu_button_up(mt: &mut Tracker, menu: Rc<Menu>) -> i32 {
    0
}

fn menu_mouse_move(mt: &mut Tracker, menu: Rc<Menu>) -> bool {
    false
}

fn select_item(window: HWND, menu: &Menu, index: Option<i32>) {}

fn select_previous(window: HWND, menu: &Menu) {}

fn select_next(window: HWND, menu: &Menu) {}

fn select_first(window: HWND, menu: &Menu) {}

fn select_last(window: HWND, menu: &Menu) {}

unsafe fn track_menu(
    window: HWND,
    menu: Rc<Menu>,
    x: i32,
    y: i32,
    owning_window: HWND,
) -> Result<()> {
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
                        let executed_menu_id = menu_button_up(&mut mt, menu_from_point);
                        remove_message = executed_menu_id != -1;
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
                    VK_HOME => select_first(window, menu.as_ref()),
                    VK_END => select_last(window, menu.as_ref()),
                    VK_UP => select_previous(window, menu.as_ref()),
                    VK_DOWN => select_next(window, menu.as_ref()),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    Ok(())
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

unsafe fn show_popup(
    owning_window: HWND,
    menu_list: &Menu,
    id: usize,
    x: i32,
    y: i32,
    x_anchor: i32,
    y_anchor: i32,
) -> Result<()> {
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
            match show_popup(
                context.owning_window,
                context.menu.as_ref(),
                0,
                (*cs).x,
                (*cs).y,
                0,
                0,
            ) {
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
            _ = draw_popup_menu(window, dc, context.menu.as_ref());
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = draw_popup_menu(window, HDC(w_param.0 as isize), context.menu.as_ref());
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
