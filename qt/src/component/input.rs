use std::ffi::c_void;
use std::mem::{size_of, swap, zeroed};
use std::ptr;

use windows::core::*;
use windows::Win32::Foundation::{
    FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Globalization::{lstrcpynW, lstrlenW, ScriptStringCPtoX, SCRIPT_ANALYSIS, SCRIPT_LOGATTR, SCRIPT_UNDEFINED, ScriptStringAnalyse, SSA_LINK, SSA_FALLBACK, SSA_GLYPHS, SSA_PASSWORD, ScriptString_pSize};
use windows::Win32::Graphics::Gdi::{GetDC, HDC, IntersectRect, InvalidateRect, MapWindowPoints, ReleaseDC};
use windows::Win32::UI::Input::Ime::{IMECHARPOSITION, IMR_QUERYCHARPOSITION};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_BACK, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{get_scaling_factor, QT};
macro_rules! order_usize {
    ($x:expr, $y:expr) => {{
        if $y < $x {
            swap($x, $y);
        }
    }};
}
#[derive(Copy, Clone)]
pub enum Size {
    Small,
    Medium,
    Large,
}

#[derive(Copy, Clone)]
pub enum Appearance {
    Outline,
    FilledLighter,
    FilledDarker,
}

#[derive(Copy, Clone)]
pub enum Type {
    Number,
    Text,
    Password,
}

pub struct State {
    qt: QT,
    size: Size,
    appearance: Appearance,
    default_value: Option<PCWSTR>,
    input_type: Type,
    placeholder: Option<PCWSTR>,
}

impl State {
    fn get_line_height(&self) -> i32 {
        14
    }
}

struct ScriptStringAnalysis(*mut c_void);

impl Default for ScriptStringAnalysis {
    fn default() -> Self {
        unsafe { zeroed() }
    }
}

pub struct Context {
    state: State,
    cached_text_length: Option<usize>,
    buffer: Vec<u16>,
    x_offset: usize,
    undo_insert_count: isize,
    undo_position: usize,
    undo_buffer: Vec<u16>,
    selection_start: usize,
    selection_end: usize,
    is_captured: bool,
    ime_status: usize,
    format_rect: RECT,
    log_attribute: Vec<SCRIPT_LOGATTR>,
    ssa: Option<ScriptStringAnalysis>,
}

impl Context {
    unsafe fn get_text_length(&mut self) -> usize {
        match self.cached_text_length {
            None => {
                let length = lstrlenW(PCWSTR::from_raw(self.buffer.as_slice().as_ptr())) as usize;
                self.cached_text_length = Some(length);
                length
            }
            Some(text_length) => text_length,
        }
    }
}

impl QT {
    pub fn creat_input(
        &self,
        parent_window: &HWND,
        instance: &HINSTANCE,
        x: i32,
        y: i32,
        size: &Size,
        appearance: &Appearance,
        default_value: Option<PCWSTR>,
        input_type: &Type,
        placeholder: Option<PCWSTR>,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_INPUT");
        unsafe {
            let window_class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpszClassName: class_name,
                style: CS_CLASSDC | CS_DBLCLKS,
                lpfnWndProc: Some(window_proc),
                hCursor: LoadCursorW(None, IDC_IBEAM)?,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
            let boxed = Box::new(State {
                qt: self.clone(),
                size: *size,
                appearance: *appearance,
                default_value,
                input_type: *input_type,
                placeholder,
            });
            let scaling_factor = get_scaling_factor(parent_window);
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (380f32 * scaling_factor) as i32,
                (32f32 * scaling_factor) as i32,
                *parent_window,
                None,
                *instance,
                Some(Box::<State>::into_raw(boxed) as _),
            );
            Ok(window)
        }
    }
}

unsafe fn get_single_line_rect(
    window: HWND,
    context: &mut Context,
    start_col: usize,
    end_col: Option<usize>,
) -> Result<RECT> {
    let pt1 = if start_col == 0 {
        context.format_rect.left
    } else {
        position_from_char(window, context, start_col)?.x
    };
    let pt2 = match end_col {
        None => context.format_rect.right,
        Some(col) => position_from_char(window, context, col)?.x,
    };
    let pt3 = match &context.ssa {
        None => pt1,
        Some(ssa) => {
            let mut pt3 = ScriptStringCPtoX(
                ssa.0,
                start_col as i32,
                FALSE,
            )?;
            pt3 = pt3 + context.format_rect.left;
            pt3
        }
    };

    Ok(RECT {
        left: pt1.min(pt2).min(pt3),
        top: context.format_rect.top,
        right: pt1.max(pt2).max(pt3),
        bottom: context.format_rect.top + context.state.get_line_height(),
    })
}

unsafe fn invalidate_text(
    window: HWND,
    context: &mut Context,
    start: usize,
    end: usize,
) -> Result<()> {
    if start == end {
        return Ok(());
    }

    let actual_start = start.min(end);
    let actual_end = start.max(end);
    let line_rect = get_single_line_rect(window, context, actual_start, Some(actual_end))?;
    let mut rc = RECT::default();
    if IntersectRect(&mut rc, &line_rect, &context.format_rect).into() {
        InvalidateRect(window, Some(&rc), true);
    }
    Ok(())
}

unsafe fn set_selection(
    window: HWND,
    context: &mut Context,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<bool> {
    let mut old_start = context.selection_start;
    let mut old_end = context.selection_end;
    let length = context.get_text_length();

    let mut new_start = match start {
        None => context.selection_end,
        Some(s) => length.min(s),
    };
    let mut new_end = match start {
        None => context.selection_end,
        Some(_) => match end {
            None => length,
            Some(e) => length.min(e),
        },
    };
    if old_start == new_start && old_end == new_end {
        return Ok(false);
    }

    context.selection_start = new_start;
    context.selection_end = new_end;

    /* Compute the necessary invalidation region.
    Let's assume that we sort them in this order: new_start <= new_end <= old_start <= old_end */
    order_usize!(&mut new_end, &mut old_end);
    order_usize!(&mut new_start, &mut old_start);
    order_usize!(&mut old_start, &mut old_end);
    order_usize!(&mut new_start, &mut new_end);
    /* Note that at this point 'end' and 'old_start' are not in order, but start is definitely the min. and old_end is definitely the max. */
    if new_end != old_start {
        if old_start > new_end {
            invalidate_text(window, context, new_start, new_end)?;
            invalidate_text(window, context, old_start, old_end)?;
        } else {
            invalidate_text(window, context, new_start, old_start)?;
            invalidate_text(window, context, new_end, old_end)?;
        }
    } else {
        invalidate_text(window, context, new_start, old_start)?;
    }
    Ok(true)
}

unsafe fn replace_selection(
    window: HWND,
    context: &mut Context,
    can_undo: bool,
    replace: Vec<u16>,
    send_update: bool,
    honor_limit: bool,
) -> Result<()> {
    Ok(())
}

unsafe fn update_uniscribe_data(window: HWND, context: &mut Context, dc: Option<HDC>) -> Result<()> {
    if context.ssa.is_none() {
        let length = context.get_text_length();
        let udc = dc.unwrap_or(GetDC(window));
        let mut ssa = ScriptStringAnalysis::default();
        match context.state.input_type {
            Type::Password => {
                ScriptStringAnalyse(udc, w!("*").as_ptr() as _, (1.5 * length as f32 + 16f32) as i32, -1, SSA_LINK|SSA_FALLBACK|SSA_GLYPHS|SSA_PASSWORD, -1, None, None, None, None, ptr::null(), &mut ssa.0)?;
            },
            _ => {ScriptStringAnalyse(udc, context.buffer.as_slice().as_ptr() as _, (1.5 * length as f32 + 16f32) as i32, -1, SSA_LINK|SSA_FALLBACK|SSA_GLYPHS, -1, None, None, None, None, ptr::null(), &mut ssa.0)?;}
        }
        if dc.map(|x| x == udc).unwrap_or(false) {
            ReleaseDC(window, udc);
        }
        context.ssa = Some(ssa);
    }
    Ok(())
}
unsafe fn invalidate_uniscribe_data(window: HWND, context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn scroll_caret(window: HWND, context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn update_scroll_info(window: HWND, context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn set_text(window: HWND, context: &mut Context, text: PCWSTR) -> Result<()> {
    set_selection(window, context, Some(0), None)?;
    replace_selection(
        window,
        context,
        false,
        text.as_wide().to_vec(),
        false,
        false,
    )?;
    set_selection(window, context, Some(0), Some(0))?;
    scroll_caret(window, context)?;
    update_scroll_info(window, context)?;
    invalidate_uniscribe_data(window, context)?;
    Ok(())
}

unsafe fn position_from_char(window: HWND, context: &mut Context, index: usize) -> Result<POINT> {
    let length = context.get_text_length();
    update_uniscribe_data(window, context, None)?;
    let mut x_off: usize = 0;
    if context.x_offset != 0 {
        match &context.ssa {
            None => {x_off = 0;}
            Some(ssa) => {
                if context.x_offset >= length {
                    let leftover = context.x_offset - length;
                    let size = ScriptString_pSize(ssa.0);
                    x_off = (*size).cx as usize;
                    x_off += 8 * leftover;
                } else {
                    x_off = ScriptStringCPtoX(ssa.0, context.x_offset as i32, FALSE)? as usize;
                }
            }
        }
    }
    let index = index.min(length);
    let xi = if index != 0 {
        if index >= length {
            match &context.ssa {
                None => 0,
                Some(ssa) =>  {let size = ScriptString_pSize(ssa.0);
                    (*size).cx as usize}
            }
        } else {
            match &context.ssa {
                None => 0,
                Some(ssa) =>  ScriptStringCPtoX(ssa.0, context.x_offset as i32, FALSE)? as usize
            }
        }
    } else {0};
    Ok(POINT {
        x: xi as i32 - x_off as i32 + context.format_rect.left,
        y: context.format_rect.top,
    })
}

unsafe fn clear(window: HWND, context: &mut Context) -> Result<()> {
    replace_selection(window, context, true, Vec::new(), true, true)
}

unsafe fn move_backward(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let mut e = context.selection_end;
    if e > 0 {
        e = e - 1;
    }
    let start = if extend {
        context.selection_start
    } else {
        e
    };
    set_selection(window, context, Some(start), Some(e))?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    Ok(Context {
        state,
        cached_text_length: None,
        buffer: Vec::new(),
        x_offset: 0,
        undo_insert_count: 0,
        undo_position: 0,
        undo_buffer: Vec::new(),
        selection_start: 0,
        selection_end: 0,
        is_captured: false,
        ime_status: 0,
        format_rect: RECT::default(),
        log_attribute: Vec::new(),
        ssa: Default::default(),
    })
}

unsafe fn on_char(window: HWND, context: &mut Context, char: u16) -> Result<()> {
    let control = GetKeyState(VK_CONTROL.0 as i32) != 0;
    const BACK: u16 = VK_BACK.0;
    match char {
        BACK => {
            if !control {
                if context.selection_start != context.selection_end {
                    clear(window, context)?;
                } else {
                    set_selection(window, context, None, None)?;
                    move_backward(window, context, true)?;
                    clear(window, context)?;
                }
            }
        }
        0x03 => {
            // ^C
            match context.state.input_type {
                Type::Password => {}
                _ => {
                    SendMessageW(window, WM_COPY, WPARAM(0), LPARAM(0));
                }
            }
        }
        0x16 => {
            // ^V
            SendMessageW(window, WM_PASTE, WPARAM(0), LPARAM(0));
        }
        0x18 => {
            // ^X
            match context.state.input_type {
                Type::Password => {}
                _ => {
                    SendMessageW(window, WM_CUT, WPARAM(0), LPARAM(0));
                }
            }
        }
        0x1A => {
            // ^Z
            SendMessageW(window, WM_UNDO, WPARAM(0), LPARAM(0));
        }
        _ => match context.state.input_type {
            Type::Number => {}
            _ => {
                if char >= '_' as u16 && char != 127 {
                    replace_selection(window, context, true, Vec::<u16>::from([char]), true, true)?;
                }
            }
        },
    }
    Ok(())
}

unsafe fn on_copy(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn on_cut(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn on_key_down(context: &mut Context, key: i32) -> Result<()> {
    Ok(())
}

unsafe fn on_kill_focus(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn on_double_click(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn on_left_button_down(context: &mut Context, keys: usize, x: i32, y: i32) -> Result<()> {
    Ok(())
}

unsafe fn on_left_button_up(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn on_mouse_move(context: &mut Context, x: i32, y: i32) -> Result<()> {
    Ok(())
}

unsafe fn on_paint(window: HWND, context: &Context) -> Result<()> {
    Ok(())
}

unsafe fn on_paste(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn set_focus(context: &mut Context) -> Result<()> {
    Ok(())
}

unsafe fn update_imm_composition_window(window: HWND, x: i32, y: i32) -> Result<()> {
    Ok(())
}

unsafe fn update_imm_composition_font(context: &Context) -> Result<()> {
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
            let raw = (*cs).lpCreateParams as *mut State;
            let state = Box::<State>::from_raw(raw);
            match on_create(window, *state) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        WM_CHAR => unsafe {
            let char = w_param.0 as u16;
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = on_char(window, &mut *raw, char);
            LRESULT(0)
        },
        WM_UNICHAR => unsafe {
            if w_param.0 as u32 == UNICODE_NOCHAR {
                LRESULT(TRUE.0 as isize)
            } else {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                let context = &mut *raw;
                if w_param.0 < 0x000fffff {
                    if w_param.0 > 0xffff {
                        // convert to surrogates
                        let param = w_param.0 - 0x10000;
                        _ = on_char(window, context, ((param >> 10) + 0xd800) as u16).and(on_char(
                            window,
                            context,
                            ((param & 0x03ff) + 0xdc00) as u16,
                        ));
                    }
                } else {
                    _ = on_char(window, context, w_param.0 as u16);
                }
                LRESULT(0)
            }
        },
        WM_CLEAR => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = clear(window, context);
            LRESULT::default()
        },
        WM_CONTEXTMENU => LRESULT::default(),
        WM_COPY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            match on_copy(context) {
                Ok(_) => LRESULT(1),
                Err(_) => LRESULT(0),
            }
        },
        WM_CUT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_cut(context);
            LRESULT::default()
        },
        WM_GETTEXT => unsafe {
            let max_length = w_param.0;
            let dest = l_param.0 as *mut u16;
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let source = PCWSTR::from_raw(context.buffer.as_slice().as_ptr());
            lstrcpynW(std::slice::from_raw_parts_mut(dest, max_length), source);
            LRESULT(lstrlenW(PCWSTR(dest)) as isize)
        },
        WM_GETTEXTLENGTH => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            LRESULT(context.get_text_length() as isize)
        },
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_key_down(context, w_param.0 as i32);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_kill_focus(context);
            LRESULT(0)
        },
        WM_LBUTTONDBLCLK => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_double_click(context);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let mouse_x = l_param.0 as i16 as i32;
            let mouse_y = (l_param.0 >> 16) as i16 as i32;
            _ = on_left_button_down(context, w_param.0, mouse_x, mouse_y);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_left_button_up(context);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let mouse_x = l_param.0 as i16 as i32;
            let mouse_y = (l_param.0 >> 16) as i16 as i32;
            _ = on_mouse_move(context, mouse_x, mouse_y);
            LRESULT(0)
        },
        WM_PRINTCLIENT | WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            match on_paint(window, context) {
                Ok(_) => LRESULT(0),
                Err(_) => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        WM_PASTE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = on_paint(window, context);
            LRESULT::default()
        },
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_left_button_up(context);
            LRESULT(0)
        },
        WM_SETTEXT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = set_text(window, context, PCWSTR(l_param.0 as *const u16));
            LRESULT(TRUE.0 as isize)
        },
        WM_IME_SETCONTEXT => unsafe {
            let mut point = POINT::default();
            match GetCaretPos(&mut point) {
                Ok(_) => {
                    _ = update_imm_composition_window(window, point.x, point.y);
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &*raw;
                    _ = update_imm_composition_font(context);
                }
                Err(_) => {}
            }
            LRESULT::default()
        },
        WM_IME_COMPOSITION => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = replace_selection(window, context, true, Vec::new(), true, true);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_IME_SELECT => LRESULT::default(),
        WM_IME_REQUEST => unsafe {
            match w_param.0 as u32 {
                IMR_QUERYCHARPOSITION => {
                    let char_pos = &mut (*(l_param.0 as *mut IMECHARPOSITION));
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &mut *raw;
                    match position_from_char(window, context, context.selection_start + char_pos.dwCharPos as usize)
                    {
                        Ok(point) => {
                            char_pos.pt.x = point.x;
                            char_pos.pt.y = point.y;
                            MapWindowPoints(window, HWND_DESKTOP, &mut [char_pos.pt]);
                            char_pos.cLineHeight = context.state.get_line_height() as u32;
                            let mut doc_tl = POINT {
                                x: context.format_rect.left,
                                y: context.format_rect.top,
                            };
                            let mut doc_br = POINT {
                                x: context.format_rect.right,
                                y: context.format_rect.bottom,
                            };
                            MapWindowPoints(window, HWND_DESKTOP, &mut [doc_tl, doc_br]);
                            char_pos.rcDocument = RECT {
                                left: doc_tl.x,
                                top: doc_tl.y,
                                right: doc_br.x,
                                bottom: doc_br.y,
                            };
                            LRESULT(1)
                        }
                        Err(_) => LRESULT(0),
                    }
                }
                _ => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
