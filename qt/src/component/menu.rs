use std::mem::size_of;
use std::rc::Rc;

use windows::core::*;
use windows::Win32::Foundation::{
    ERROR_INVALID_WINDOW_HANDLE, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::SetCapture;
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

impl QT {
    pub unsafe fn open_menu(
        &self,
        parent_window: HWND,
        menu_list: Vec<MenuInfo>,
        x: i32,
        y: i32,
    ) -> Result<()> {
        const CLASS_NAME: PCWSTR = w!("QT_MENU");
        let window_class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpszClassName: CLASS_NAME,
            style: CS_DROPSHADOW | CS_SAVEBITS | CS_DBLCLKS,
            lpfnWndProc: Some(window_proc),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        };
        RegisterClassExW(&window_class);
        let is_parent_window_valid: bool = IsWindow(parent_window).into();
        if !is_parent_window_valid {
            return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
        }
        let menu = Rc::new(convert_menu_info_list_to_menu(menu_list));
        let boxed = Box::new(Context {
            qt: self.clone(),
            menu: menu.clone(),
            owning_window: parent_window,
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
            parent_window,
            None,
            HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE)),
            Some(Box::<Context>::into_raw(boxed) as _),
        );
        init_tracking(parent_window)?;
        track_menu(window, menu.clone(), 0, 0, parent_window).and(exit_tracking(parent_window))?;
        Ok(())
    }
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

unsafe fn track_menu(
    window: HWND,
    menu: Rc<Menu>,
    x: i32,
    y: i32,
    owning_window: HWND,
) -> Result<()> {
    SetCapture(window);
    let mut mt = Tracker {
        current_menu: menu.clone(),
        top_menu: menu.clone(),
        owning_window,
        point: Default::default(),
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
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Menu;
            let menu = &*raw;
            let mut ps = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut ps);
            _ = draw_popup_menu(window, dc, menu);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Menu;
            let menu = &*raw;
            _ = draw_popup_menu(window, HDC(w_param.0 as isize), menu);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Menu;
            _ = Box::<Menu>::from_raw(raw);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
