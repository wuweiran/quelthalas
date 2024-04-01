use std::ffi::c_void;
use std::mem::{size_of, swap};
use std::ptr;
use std::ptr::null_mut;
use std::slice::from_raw_parts_mut;

use windows::core::*;
use windows::Win32::Foundation::{
    FALSE, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Globalization::{
    lstrcpynW, lstrlenW, u_memcpy, ScriptBreak, ScriptStringAnalyse, ScriptStringCPtoX,
    ScriptStringFree, ScriptStringXtoCP, ScriptString_pSize, SCRIPT_ANALYSIS, SCRIPT_LOGATTR,
    SCRIPT_UNDEFINED, SSA_FALLBACK, SSA_GLYPHS, SSA_LINK, SSA_PASSWORD,
};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, GetDC, GetTextMetricsW, IntersectRect, InvalidateRect, MapWindowPoints,
    RedrawWindow, ReleaseDC, SelectObject, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET,
    FW_BOLD, HDC, OUT_OUTLINE_PRECIS, RDW_INVALIDATE, TEXTMETRICW, VARIABLE_PITCH,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::SystemServices::MK_SHIFT;
use windows::Win32::UI::Controls::{SetScrollInfo, WORD_BREAK_ACTION};
use windows::Win32::UI::Controls::{WB_ISDELIMITER, WB_LEFT, WB_RIGHT};
use windows::Win32::UI::Input::Ime::{IMECHARPOSITION, IMR_QUERYCHARPOSITION};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DELETE,
    VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_RIGHT, VK_SHIFT,
};
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

struct ScriptStringAnalysis(*mut c_void);

impl Default for ScriptStringAnalysis {
    fn default() -> Self {
        ScriptStringAnalysis(null_mut())
    }
}

pub struct StringBuffer(Vec<u16>);

impl StringBuffer {
    fn new() -> Self {
        StringBuffer(vec![0])
    }

    fn with_capacity(capacity: usize) -> Self {
        let mut vec = Vec::<u16>::with_capacity(capacity + 1);
        vec[capacity] = 0;
        StringBuffer(vec)
    }

    fn make_fit(&mut self, size: usize) {
        if size + 1 < self.0.len() {
            return;
        }

        self.0.resize(size + 1, 0);
    }

    fn empty(&mut self) {
        if self.0.len() > 32 {
            self.0 = vec![0];
        } else {
            self.0[0] = 0;
        }
    }

    fn insert_at(&mut self, at: usize, to_insert: &[u16]) {
        self.0.splice(at..at, to_insert.iter().cloned());
    }

    fn is_empty(&self) -> bool {
        self.0[0] == 0
    }
    fn as_wcs(&self) -> PCWSTR {
        PCWSTR::from_raw(self.0.as_slice().as_ptr())
    }

    fn as_ptr(&self) -> *const u16 {
        self.0.as_ptr()
    }

    fn as_mut_ptr(&mut self) -> *mut u16 {
        self.0.as_mut_ptr()
    }
}

pub struct Context {
    state: State,
    cached_text_length: Option<usize>,
    buffer: StringBuffer,
    x_offset: usize,
    undo_insert_count: usize,
    undo_position: usize,
    undo_buffer: StringBuffer,
    selection_start: usize,
    selection_end: usize,
    is_captured: bool,
    is_focused: bool,
    ime_status: usize,
    format_rect: RECT,
    line_height: i32,
    char_width: i32,
    text_width: i32,
    log_attribute: Vec<SCRIPT_LOGATTR>,
    ssa: Option<ScriptStringAnalysis>,
}

impl Context {
    unsafe fn get_text_length(&mut self) -> usize {
        match self.cached_text_length {
            None => {
                let length = lstrlenW(self.buffer.as_wcs()) as usize;
                self.cached_text_length = Some(length);
                length
            }
            Some(text_length) => text_length,
        }
    }
    unsafe fn invalidate_uniscribe_data(&mut self) -> Result<()> {
        match &mut self.ssa {
            None => {}
            Some(ssa) => {
                ScriptStringFree(&mut ssa.0)?;
                self.ssa = None;
            }
        }
        Ok(())
    }

    unsafe fn text_buffer_changed(&mut self) -> Result<()> {
        self.cached_text_length = None;
        self.log_attribute.clear();
        self.invalidate_uniscribe_data()
    }

    unsafe fn empty_undo_buffer(&mut self) {
        self.undo_insert_count = 0;
        self.undo_buffer.empty();
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
            let mut pt3 = ScriptStringCPtoX(ssa.0, start_col as i32, FALSE)?;
            pt3 = pt3 + context.format_rect.left;
            pt3
        }
    };

    Ok(RECT {
        left: pt1.min(pt2).min(pt3),
        top: context.format_rect.top,
        right: pt1.max(pt2).max(pt3),
        bottom: context.format_rect.top + context.line_height,
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
) -> Result<()> {
    let mut start = context.selection_start;
    let mut end = context.selection_end;
    context.invalidate_uniscribe_data()?;
    if start == end && replace.is_empty() {
        return Ok(());
    }
    order_usize!(&mut start, &mut end);
    let mut text_length = context.get_text_length();
    let size = text_length - (end - start) + replace.len();
    context.buffer.make_fit(size);
    let mut buf = StringBuffer::new();
    if end != start {
        let buf_length = end - start;
        buf = StringBuffer::with_capacity(buf_length);
        u_memcpy(
            buf.as_mut_ptr(),
            context.buffer.as_ptr().offset(start as isize),
            buf_length as i32,
        );
        lstrcpynW(
            from_raw_parts_mut(
                context.buffer.as_mut_ptr().offset(start as isize),
                size - start,
            ),
            PCWSTR::from_raw(context.buffer.as_ptr().offset(end as isize)),
        );
        context.text_buffer_changed()?;
    }
    if !replace.is_empty() {
        context.buffer.insert_at(start, replace.as_slice());
        context.text_buffer_changed()?;
    }

    context.invalidate_uniscribe_data()?;
    calculate_line_width(window, context)?;

    if end != start {
        if can_undo {
            let undo_text_length = lstrlenW(context.undo_buffer.as_wcs()) as usize;
            if context.undo_insert_count == 0
                && !context.undo_buffer.is_empty()
                && start == context.undo_position
            {
                context.undo_buffer.make_fit(undo_text_length + end - start);
                u_memcpy(
                    context
                        .undo_buffer
                        .as_mut_ptr()
                        .offset(undo_text_length as isize),
                    context.buffer.as_ptr(),
                    (end - start) as i32,
                );
            } else if context.undo_insert_count == 0
                && !context.undo_buffer.is_empty()
                && end == context.undo_position
            {
                context.undo_buffer.make_fit(undo_text_length + end - start);
                context.undo_buffer.insert_at(0, buf.as_wcs().as_wide());
                context.undo_position = start;
            } else {
                context.undo_buffer.make_fit(end - start);
                u_memcpy(
                    context.undo_buffer.as_mut_ptr(),
                    context.buffer.as_ptr(),
                    (end - start) as i32,
                );
                context.undo_position = start;
            }
            context.undo_insert_count = 0;
        } else {
            context.empty_undo_buffer();
        }
    }
    if !replace.is_empty() {
        if can_undo {
            if start == context.undo_position
                || (context.undo_insert_count != 0
                    && start == context.undo_position + context.undo_insert_count)
            {
                context.undo_insert_count = context.undo_insert_count + replace.len()
            } else {
                context.undo_insert_count = start;
                context.undo_insert_count = replace.len();
                context.undo_buffer.empty();
            }
        } else {
            context.empty_undo_buffer();
        }
    }

    start = start + replace.len();
    set_selection(window, context, Some(start), Some(start))?;
    InvalidateRect(window, None, true);

    scroll_caret(window, context)?;
    update_scroll_info(window, context);

    context.invalidate_uniscribe_data()?;

    Ok(())
}

unsafe fn update_uniscribe_data(
    window: HWND,
    context: &mut Context,
    dc: Option<HDC>,
) -> Result<()> {
    if context.ssa.is_none() {
        let length = context.get_text_length();
        let udc = dc.unwrap_or(GetDC(window));
        let mut ssa = ScriptStringAnalysis::default();
        match context.state.input_type {
            Type::Password => {
                ScriptStringAnalyse(
                    udc,
                    w!("*").as_ptr() as _,
                    (1.5 * length as f32 + 16f32) as i32,
                    -1,
                    SSA_LINK | SSA_FALLBACK | SSA_GLYPHS | SSA_PASSWORD,
                    -1,
                    None,
                    None,
                    None,
                    None,
                    ptr::null(),
                    &mut ssa.0,
                )?;
            }
            _ => {
                ScriptStringAnalyse(
                    udc,
                    context.buffer.as_ptr() as _,
                    (1.5 * length as f32 + 16f32) as i32,
                    -1,
                    SSA_LINK | SSA_FALLBACK | SSA_GLYPHS,
                    -1,
                    None,
                    None,
                    None,
                    None,
                    ptr::null(),
                    &mut ssa.0,
                )?;
            }
        }
        if dc.map(|x| x == udc).unwrap_or(false) {
            ReleaseDC(window, udc);
        }
        context.ssa = Some(ssa);
    }
    Ok(())
}

unsafe fn set_caret_position(window: HWND, context: &mut Context, position: usize) -> Result<()> {
    if context.is_focused {
        let res = position_from_char(window, context, position)?;
        SetCaretPos(res.x, res.y)?;
        update_imm_composition_window(window, res.x, res.y)?;
    }
    Ok(())
}

unsafe fn scroll_caret(window: HWND, context: &mut Context) -> Result<()> {
    let mut x = position_from_char(window, context, context.selection_end)?.x;
    let format_width = context.format_rect.right - context.format_rect.left;
    if x < context.format_rect.left {
        let goal = context.format_rect.left + format_width / 3;
        loop {
            context.x_offset = context.x_offset - 1;
            x = position_from_char(window, context, context.selection_end)?.x;
            if x >= goal || context.x_offset == 0 {
                break;
            }
        }
    } else if x > context.format_rect.right {
        let len = context.get_text_length();
        let goal = context.format_rect.right - format_width / 3;
        loop {
            context.x_offset = context.x_offset + 1;
            x = position_from_char(window, context, context.selection_end)?.x;
            let x_last = position_from_char(window, context, len)?.x;
            if x <= goal || x_last <= context.format_rect.right {
                break;
            }
        }
        InvalidateRect(window, None, true);
    }

    set_caret_position(window, context, context.selection_end)?;
    Ok(())
}

unsafe fn update_scroll_info(window: HWND, context: &mut Context) {
    let si = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_PAGE | SIF_POS | SIF_RANGE | SIF_DISABLENOSCROLL,
        nMin: 0,
        nMax: context.text_width - 1,
        nPage: (context.format_rect.right - context.format_rect.left) as u32,
        nPos: context.x_offset as i32,
        nTrackPos: context.x_offset as i32,
    };
    SetScrollInfo(window, SB_HORZ, &si, true);
}

unsafe fn set_text(window: HWND, context: &mut Context, text: PCWSTR) -> Result<()> {
    set_selection(window, context, Some(0), None)?;
    replace_selection(window, context, false, text.as_wide().to_vec())?;
    set_selection(window, context, Some(0), Some(0))?;
    scroll_caret(window, context)?;
    update_scroll_info(window, context);
    context.invalidate_uniscribe_data()?;
    Ok(())
}

unsafe fn calculate_line_width(window: HWND, context: &mut Context) -> Result<()> {
    update_uniscribe_data(window, context, None)?;
    context.char_width = match &context.ssa {
        None => 0,
        Some(ssa) => {
            let size = ScriptString_pSize(ssa.0);
            (*size).cx
        }
    };
    Ok(())
}

unsafe fn position_from_char(window: HWND, context: &mut Context, index: usize) -> Result<POINT> {
    let length = context.get_text_length();
    update_uniscribe_data(window, context, None)?;
    let mut x_off: usize = 0;
    if context.x_offset != 0 {
        match &context.ssa {
            None => {
                x_off = 0;
            }
            Some(ssa) => {
                if context.x_offset >= length {
                    let leftover = context.x_offset - length;
                    let size = ScriptString_pSize(ssa.0);
                    x_off = (*size).cx as usize;
                    x_off += context.char_width as usize * leftover;
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
                Some(ssa) => {
                    let size = ScriptString_pSize(ssa.0);
                    (*size).cx as usize
                }
            }
        } else {
            match &context.ssa {
                None => 0,
                Some(ssa) => ScriptStringCPtoX(ssa.0, context.x_offset as i32, FALSE)? as usize,
            }
        }
    } else {
        0
    };
    Ok(POINT {
        x: xi as i32 - x_off as i32 + context.format_rect.left,
        y: context.format_rect.top,
    })
}

unsafe fn char_from_position(window: HWND, context: &mut Context, point: POINT) -> Result<usize> {
    let x = point.x - context.format_rect.left;
    if x == 0 {
        return Ok(context.x_offset);
    }

    update_uniscribe_data(window, context, None)?;
    let x_off = if context.x_offset != 0 {
        let length = context.get_text_length();
        match &context.ssa {
            None => 0,
            Some(ssa) => {
                if context.x_offset >= length {
                    let size = ScriptString_pSize(ssa.0);
                    (*size).cx
                } else {
                    ScriptStringCPtoX(ssa.0, context.x_offset as i32, FALSE)?
                }
            }
        }
    } else {
        0
    };
    let mut index = 0;
    if x < 0 {
        if x + x_off > 0 || context.ssa.is_none() {
            let ssa = ScriptStringAnalysis::default();
            let mut trailing = 0;
            ScriptStringXtoCP(ssa.0, x + x_off, &mut index, &mut trailing)?;
            if trailing != 0 {
                index = index + 1;
            }
        }
    } else {
        if x != 0 {
            let length = context.get_text_length();
            match &context.ssa {
                None => {
                    index = 0;
                }
                Some(ssa) => {
                    let size = ScriptString_pSize(ssa.0);
                    if x > (*size).cx {
                        index = length as i32;
                    }
                    let mut trailing = 0;
                    ScriptStringXtoCP(ssa.0, x + x_off, &mut index, &mut trailing)?;
                    if trailing != 0 {
                        index = index + 1;
                    }
                }
            }
        } else {
            index = context.x_offset as i32;
        }
    }
    Ok(index as usize)
}

unsafe fn clear(window: HWND, context: &mut Context) -> Result<()> {
    replace_selection(window, context, true, Vec::new())
}

unsafe fn move_end(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let end = context.get_text_length();
    let start = if extend { context.selection_start } else { end };
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn move_home(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let end = 0;
    let start = if extend { context.selection_start } else { end };
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn move_forward(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let mut e = context.selection_end;

    if context.get_text_length() > e {
        e = e + 1;
    }
    let start = if extend { context.selection_start } else { e };
    set_selection(window, context, Some(start), Some(e))?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn move_backward(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let mut e = context.selection_end;
    if e > 0 {
        e = e - 1;
    }
    let start = if extend { context.selection_start } else { e };
    set_selection(window, context, Some(start), Some(e))?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    let font = CreateFontW(
        32,                           // Height of the font
        0,                            // Width of the font (0 for default)
        0,                            // Angle of escapement (0 for default)
        0,                            // Orientation angle (0 for default)
        FW_BOLD.0 as i32,             // Font weight (bold)
        0,                            // Italic (not italic)
        0,                            // Underline (not underlined)
        0,                            // Strikeout (not struck out)
        DEFAULT_CHARSET.0 as u32,     // Character set (default)
        OUT_OUTLINE_PRECIS.0 as u32,  // Output precision (outline)
        CLIP_DEFAULT_PRECIS.0 as u32, // Clipping precision (default)
        CLEARTYPE_QUALITY.0 as u32,   // Font quality (ClearType)
        VARIABLE_PITCH.0 as u32,      // Pitch and family (variable pitch)
        state.qt.theme.tokens.font_family_name,
    );
    let dc = GetDC(window);
    let old_font = SelectObject(dc, font);
    let mut tm = TEXTMETRICW::default();
    GetTextMetricsW(dc, &mut tm);
    SelectObject(dc, old_font);
    ReleaseDC(window, dc);
    Ok(Context {
        state,
        cached_text_length: None,
        buffer: StringBuffer::new(),
        x_offset: 0,
        undo_insert_count: 0,
        undo_position: 0,
        undo_buffer: StringBuffer::new(),
        selection_start: 0,
        selection_end: 0,
        is_captured: false,
        is_focused: false,
        ime_status: 0,
        format_rect: RECT::default(),
        line_height: tm.tmHeight,
        char_width: tm.tmAveCharWidth,
        text_width: 0,
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
                    replace_selection(window, context, true, Vec::<u16>::from([char]))?;
                }
            }
        },
    }
    Ok(())
}

unsafe fn on_copy(window: HWND, context: &mut Context) -> Result<()> {
    let start = context.selection_start.min(context.selection_end);
    let end = context.selection_start.max(context.selection_end);
    if end == start {
        return Ok(());
    }
    let length = end - start;
    let hdst = GlobalAlloc(GMEM_MOVEABLE, (length + 1) * size_of::<u16>())?;
    let dst = GlobalLock(hdst);
    u_memcpy(
        dst as _,
        context.buffer.as_ptr().offset(start as isize),
        length as i32,
    );
    *(dst.offset(length as isize) as *mut u16) = 0;
    GlobalUnlock(hdst)?;
    OpenClipboard(window)?;
    EmptyClipboard()?;
    SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(hdst.0 as _))?;
    CloseClipboard()?;
    Ok(())
}

unsafe fn on_cut(window: HWND, context: &mut Context) -> Result<()> {
    on_copy(window, context)?;
    clear(window, context)?;
    Ok(())
}

unsafe fn on_paste(window: HWND, context: &mut Context) -> Result<()> {
    OpenClipboard(window)?;
    let hsrc = GetClipboardData(CF_UNICODETEXT.0 as u32)?;
    if !hsrc.is_invalid() {
        let src = GlobalLock(HGLOBAL(hsrc.0 as _));
        let string = PCWSTR::from_raw(src as _);
        let mut len = lstrlenW(string) as usize;
        match string.as_wide().iter().position(|a| *a == '\n' as u16) {
            None => {}
            Some(position) => {
                len = position;
                if len > 0 && string.as_wide()[len - 1] == '\r' as u16 {
                    len = len - 1;
                }
            }
        }
        replace_selection(window, context, true, string.as_wide()[..len].to_vec())?;
        GlobalUnlock(HGLOBAL(hsrc.0 as _))?;
    } else {
        match context.state.input_type {
            Type::Password => {
                replace_selection(window, context, true, Vec::new())?;
            }
            _ => {}
        }
    }
    CloseClipboard()?;
    Ok(())
}

unsafe fn on_key_down(window: HWND, context: &mut Context, key: i32) -> Result<()> {
    if GetKeyState(VK_MENU.0 as i32) != 0 {
        return Ok(());
    }

    let shift = GetKeyState(VK_SHIFT.0 as i32) != 0;
    let control = GetKeyState(VK_CONTROL.0 as i32) != 0;

    const LEFT: i32 = VK_LEFT.0 as i32;
    const RIGHT: i32 = VK_RIGHT.0 as i32;
    const HOME: i32 = VK_HOME.0 as i32;
    const END: i32 = VK_END.0 as i32;
    const DELETE: i32 = VK_DELETE.0 as i32;
    const INSERT: i32 = VK_INSERT.0 as i32;
    const A: i32 = 'A' as i32;
    match key {
        LEFT => {
            move_backward(window, context, shift)?;
        }
        RIGHT => {
            move_backward(window, context, shift)?;
        }
        HOME => move_home(window, context, shift)?,
        END => move_end(window, context, shift)?,
        DELETE => {
            if !(shift && control) {
                if context.selection_start != context.selection_end {
                    if shift {
                        on_cut(window, context)?;
                    } else {
                        clear(window, context)?;
                    }
                } else {
                    set_selection(window, context, None, Some(0))?;
                    if shift {
                        move_backward(window, context, true)?;
                    } else if control {
                        move_end(window, context, false)?;
                    } else {
                        move_forward(window, context, true)?;
                    }
                    clear(window, context)?;
                }
            }
        }
        INSERT => {
            if shift {
                on_paste(window, context)?;
            } else if control {
                on_copy(window, context)?;
            }
        }
        A => {
            if control {
                let length = context.get_text_length();
                set_selection(window, context, Some(0), Some(length))?;
            }
        }
        _ => {}
    }
    Ok(())
}

unsafe fn on_kill_focus(window: HWND, context: &mut Context) -> Result<()> {
    context.is_focused = false;
    DestroyCaret()?;
    invalidate_text(
        window,
        context,
        context.selection_start,
        context.selection_end,
    )?;
    Ok(())
}

unsafe fn word_break_proc(
    context: &mut Context,
    mut index: usize,
    count: usize,
    action: WORD_BREAK_ACTION,
) -> Result<usize> {
    let length = context.get_text_length();
    if length == 0 {
        return Ok(0);
    }

    if context.log_attribute.is_empty() {
        let psa = SCRIPT_ANALYSIS {
            _bitfield: SCRIPT_UNDEFINED as u16,
            s: Default::default(),
        };
        context
            .log_attribute
            .resize(length, SCRIPT_LOGATTR::default());
        ScriptBreak(
            context.buffer.as_wcs(),
            length as i32,
            &psa,
            context.log_attribute.as_mut_ptr(),
        )?;
    }

    let ret = match action {
        WB_LEFT => {
            if index != 0 {
                index = index - 1;
            }
            while index != 0 && (context.log_attribute[index]._bitfield & 0x0001) == 0 {
                index = index - 1;
            }
            index
        }
        WB_RIGHT => {
            if count == 0 {
                0
            } else {
                while index < count && (context.log_attribute[index]._bitfield & 0x0001) == 0 {
                    index = index + 1;
                }
                index
            }
        }
        WB_ISDELIMITER => {
            if context.log_attribute[index]._bitfield & 0x0002 != 0 {
                1
            } else {
                0
            }
        }
        _ => 0,
    };
    Ok(ret)
}

unsafe fn call_word_break_proc(
    context: &mut Context,
    start: usize,
    index: usize,
    count: usize,
    action: WORD_BREAK_ACTION,
) -> Result<usize> {
    Ok(word_break_proc(context, index + start, count + start, action)? - start)
}

unsafe fn on_double_click(window: HWND, context: &mut Context) -> Result<()> {
    context.is_captured = true;
    SetCapture(window);
    let length = context.get_text_length();
    let start = call_word_break_proc(context, 0, context.selection_end, length, WB_LEFT)?;
    let end = call_word_break_proc(context, 0, context.selection_end, length, WB_RIGHT)?;
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn on_left_button_down(
    window: HWND,
    context: &mut Context,
    keys: u32,
    mut x: i32,
    mut y: i32,
) -> Result<()> {
    context.is_captured = true;
    SetCapture(window);
    x = x
        .max(context.format_rect.left)
        .min(context.format_rect.right - 1);
    y = y
        .max(context.format_rect.top)
        .min(context.format_rect.bottom - 1);
    let end = char_from_position(window, context, POINT { x, y })?;
    let start = if (keys & MK_SHIFT.0) != 0 {
        context.selection_start
    } else {
        end
    };
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    if !context.is_focused {
        SetFocus(window);
    }
    Ok(())
}

unsafe fn on_left_button_up(window: HWND, context: &mut Context) -> Result<()> {
    if context.is_captured {
        if GetCapture() == window {
            ReleaseCapture()?;
        }
        context.is_captured = false;
    }
    Ok(())
}

unsafe fn on_mouse_move(window: HWND, context: &mut Context, x: i32, y: i32) -> Result<()> {
    if !context.is_captured || GetCapture() != window {
        return Ok(());
    }

    let end = char_from_position(window, context, POINT { x, y })?;
    set_selection(window, context, Some(context.selection_start), Some(end))?;
    set_caret_position(window, context, context.selection_end)?;
    scroll_caret(window, context)?;
    Ok(())
}

unsafe fn on_paint(window: HWND, context: &Context) -> Result<()> {
    Ok(())
}

unsafe fn set_focus(window: HWND, context: &mut Context) -> Result<()> {
    context.is_focused = true;
    invalidate_text(
        window,
        context,
        context.selection_start,
        context.selection_end,
    )?;
    CreateCaret(window, None, 1, context.line_height)?;
    set_caret_position(window, context, context.selection_end)?;
    ShowCaret(window)?;
    RedrawWindow(window, None, None, RDW_INVALIDATE);
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
            match on_copy(window, context) {
                Ok(_) => LRESULT(1),
                Err(_) => LRESULT(0),
            }
        },
        WM_CUT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_cut(window, context);
            LRESULT::default()
        },
        WM_GETTEXT => unsafe {
            let max_length = w_param.0;
            let dest = l_param.0 as *mut u16;
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let source = context.buffer.as_wcs();
            lstrcpynW(from_raw_parts_mut(dest, max_length), source);
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
            _ = on_key_down(window, context, w_param.0 as i32);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_kill_focus(window, context);
            LRESULT(0)
        },
        WM_LBUTTONDBLCLK => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_double_click(window, context);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let mouse_x = l_param.0 as i16 as i32;
            let mouse_y = (l_param.0 >> 16) as i16 as i32;
            _ = on_left_button_down(window, context, w_param.0 as u32, mouse_x, mouse_y);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_left_button_up(window, context);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let mouse_x = l_param.0 as i16 as i32;
            let mouse_y = (l_param.0 >> 16) as i16 as i32;
            _ = on_mouse_move(window, context, mouse_x, mouse_y);
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
            _ = set_focus(window, context);
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
            _ = replace_selection(window, context, true, Vec::new());
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_IME_SELECT => LRESULT::default(),
        WM_IME_REQUEST => unsafe {
            match w_param.0 as u32 {
                IMR_QUERYCHARPOSITION => {
                    let char_pos = &mut (*(l_param.0 as *mut IMECHARPOSITION));
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &mut *raw;
                    match position_from_char(
                        window,
                        context,
                        context.selection_start + char_pos.dwCharPos as usize,
                    ) {
                        Ok(point) => {
                            char_pos.pt.x = point.x;
                            char_pos.pt.y = point.y;
                            MapWindowPoints(window, HWND_DESKTOP, &mut [char_pos.pt]);
                            char_pos.cLineHeight = context.line_height as u32;
                            let mut doc_points = [
                                POINT {
                                    x: context.format_rect.left,
                                    y: context.format_rect.top,
                                },
                                POINT {
                                    x: context.format_rect.right,
                                    y: context.format_rect.bottom,
                                },
                            ];
                            MapWindowPoints(window, HWND_DESKTOP, &mut doc_points);
                            char_pos.rcDocument = RECT {
                                left: doc_points[0].x,
                                top: doc_points[0].y,
                                right: doc_points[1].x,
                                bottom: doc_points[1].y,
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
