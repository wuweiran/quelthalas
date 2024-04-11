use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::{
    ERROR_INVALID_WINDOW_HANDLE, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::QT;

pub enum MenuInfo {
    MenuItem {
        sub_menu_list: Vec<MenuInfo>,
        text: PCWSTR,
    },
    MenuDivider,
}

struct MenuItem {
    info: MenuInfo,
}

pub struct Menu {
    qt: QT,
    items: Vec<MenuItem>,
    owning_window: HWND,
}

impl QT {
    pub unsafe fn open_menu(
        &self,
        parent_window: &HWND,
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
        if IsWindow(parent_window).into() == false {
            return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
        }
        let items = menu_list
            .into_iter()
            .map(|menu_info| MenuItem { info: menu_info })
            .collect();
        let boxed = Box::new(Menu {
            qt: self.clone(),
            items,
            owning_window: *parent_window,
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
            Some(Box::<Menu>::into_raw(boxed) as _),
        );
        init_tracking(*parent_window)?;
        track_menu(window, 0, 0, *parent_window)?;
        exit_tracking(*parent_window)?;
        Ok(())
    }
}

unsafe fn init_tracking(owning_window: HWND) -> Result<()> {
    HideCaret(None)?;
    SendMessageW(
        SendMessageW,
        WM_ENTERMENULOOP,
        WPARAM(TRUE.0 as usize),
        LPARAM(0),
    );
    SendMessageW(
        SendMessageW,
        WM_SETCURSOR,
        WPARAM(owning_window.0 as usize),
        LPARAM(HTCAPTION as isize),
    );
    Ok(())
}

unsafe fn track_menu(window: HWND, x: i32, y: i32, owning_window: HWND) -> Result<()> {
    Ok(())
}

unsafe fn exit_tracking(owning_window: HWND) -> Result<()> {
    SendMessageW(
        SendMessageW,
        WM_EXITMENULOOP,
        WPARAM(TRUE.0 as usize),
        LPARAM(0),
    );
    ShowCaret(None)?;
    Ok(())
}

unsafe fn show_popup(
    owning_window: HWND,
    menu_list: &Vec<MenuItem>,
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
            let raw = (*cs).lpCreateParams as *mut Menu;
            let menu = Box::<Menu>::from_raw(raw);
            match show_popup(menu.owning_window, &menu.items, 0, (*cs).x, (*cs).y, 0, 0) {
                Ok(_) => {
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Menu>::into_raw(menu) as _);
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
            EndPaint(window, &ps);
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
