use std::mem::{size_of, swap};
use std::slice::from_raw_parts_mut;
use std::sync::Once;

use windows::Win32::Foundation::{
    FALSE, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, TRUE, WPARAM,
};
use windows::Win32::Globalization::{
    SCRIPT_ANALYSIS, SCRIPT_LOGATTR, SCRIPT_UNDEFINED, ScriptBreak, lstrcpynW, lstrlenW, u_memcpy,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1Factory1, ID2D1HwndRenderTarget,
    ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_HIT_TEST_METRICS,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS, DWRITE_WORD_WRAPPING_NO_WRAP,
    IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_GRAYTEXT, COLOR_HIGHLIGHT,
    COLOR_HIGHLIGHTTEXT, CreateFontW, CreateRoundRectRgn, DEFAULT_CHARSET, DeleteObject, EndPaint,
    FF_SWISS, GetDC, GetObjectW, GetSysColor, GetTextMetricsW, HFONT, InflateRect, IntersectRect,
    InvalidateRect, LOGFONTW, MapWindowPoints, OUT_OUTLINE_PRECIS, PAINTSTRUCT, RDW_INVALIDATE,
    RedrawWindow, ReleaseDC, SelectObject, SetWindowRgn, SYS_COLOR_INDEX, TEXTMETRICW,
    VARIABLE_PITCH,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::System::SystemServices::MK_SHIFT;
use windows::Win32::UI::Animation::{
    IUIAnimationManager2, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionLibrary2, IUIAnimationVariable2, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE,
    UIAnimationManager2, UIAnimationTimer,
};
use windows::Win32::UI::Controls::{SetScrollInfo, WORD_BREAK_ACTION};
use windows::Win32::UI::Controls::{WB_ISDELIMITER, WB_LEFT, WB_RIGHT};
use windows::Win32::UI::Input::Ime::{
    CFS_RECT, COMPOSITIONFORM, IMECHARPOSITION, IMR_QUERYCHARPOSITION, ImmGetContext,
    ImmReleaseContext, ImmSetCompositionFontW, ImmSetCompositionWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_BACK, VK_CONTROL, VK_DELETE,
    VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_MENU, VK_RIGHT, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

use crate::theme::TypographyStyle;
use crate::{QT, get_scaling_factor};

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

pub struct Props {
    pub width: i32,
    pub size: Size,
    pub appearance: Appearance,
    pub default_value: Option<PCWSTR>,
    pub input_type: Type,
    pub placeholder: Option<PCWSTR>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            width: 0,
            size: Size::Medium,
            appearance: Appearance::Outline,
            default_value: None,
            input_type: Type::Text,
            placeholder: None,
        }
    }
}

pub struct State {
    qt: QT,
    width: f32,
    size: Size,
    appearance: Appearance,
    default_value: Option<PCWSTR>,
    input_type: Type,
    placeholder: Option<PCWSTR>,
}

impl State {
    fn get_field_height(&self) -> f32 {
        match self.size {
            Size::Small => 24f32,
            Size::Medium => 32f32,
            Size::Large => 40f32,
        }
    }

    fn get_horizontal_padding(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            Size::Small => tokens.spacing_horizontal_s,
            Size::Medium => tokens.spacing_horizontal_m,
            Size::Large => tokens.spacing_horizontal_m + tokens.spacing_horizontal_s_nudge,
        }
    }

    fn get_typography_style(&self) -> &TypographyStyle {
        let typography_styles = &self.qt.theme.typography_styles;
        match self.size {
            Size::Small => &typography_styles.caption1,
            Size::Medium => &typography_styles.body1,
            Size::Large => &typography_styles.body2,
        }
    }
}

#[derive(Clone)]
pub struct StringBuffer(Vec<u16>);

impl StringBuffer {
    fn new() -> Self {
        StringBuffer(vec![0])
    }

    fn with_capacity(capacity: usize) -> Self {
        let mut vec = Vec::<u16>::with_capacity(capacity + 1);
        vec.resize(capacity + 1, 0);
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

    fn remove_at(&mut self, at: usize) {
        self.0.splice(at..at + 1, []);
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
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    bottom_focus_border: IUIAnimationVariable2,
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
    format_rect: RECT,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    text_layout: Option<IDWriteTextLayout>,
    font: HFONT,
    line_height: i32,
    char_width: i32,
    text_width: i32,
    log_attribute: Vec<SCRIPT_LOGATTR>,
}

impl Context {
    fn get_text_length(&mut self) -> usize {
        match self.cached_text_length {
            None => unsafe {
                let length = lstrlenW(self.buffer.as_wcs()) as usize;
                self.cached_text_length = Some(length);
                length
            },
            Some(text_length) => text_length,
        }
    }
    fn invalidate_text_layout(&mut self) {
        self.text_layout = None;
    }

    /// Cached text layout over the display string (bullets for passwords).
    fn update_text_layout(&mut self) -> Result<Option<IDWriteTextLayout>> {
        if self.text_layout.is_none() {
            let length = self.get_text_length();
            if length == 0 {
                return Ok(None);
            }
            let display: Vec<u16> = match self.state.input_type {
                Type::Password => vec!['\u{2022}' as u16; length],
                _ => unsafe { self.buffer.as_wcs().as_wide()[..length].to_vec() },
            };
            let layout = unsafe {
                self.state.qt.dwrite_factory.CreateTextLayout(
                    &display,
                    &self.text_format,
                    f32::MAX,
                    f32::MAX,
                )?
            };
            self.text_layout = Some(layout);
        }
        Ok(self.text_layout.clone())
    }

    fn text_buffer_changed(&mut self) -> Result<()> {
        self.cached_text_length = None;
        self.log_attribute.clear();
        self.invalidate_text_layout();
        Ok(())
    }

    fn layout_text_width(&mut self) -> Result<i32> {
        match self.update_text_layout()? {
            None => Ok(0),
            Some(layout) => {
                let mut metrics = DWRITE_TEXT_METRICS::default();
                unsafe { layout.GetMetrics(&mut metrics)? };
                Ok(metrics.widthIncludingTrailingWhitespace.round() as i32)
            }
        }
    }

    fn layout_cp_to_x(&mut self, cp: usize) -> Result<i32> {
        match self.update_text_layout()? {
            None => Ok(0),
            Some(layout) => {
                let mut x = 0f32;
                let mut y = 0f32;
                let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                unsafe {
                    layout.HitTestTextPosition(cp as u32, false, &mut x, &mut y, &mut metrics)?
                };
                Ok(x.round() as i32)
            }
        }
    }

    fn layout_x_to_cp(&mut self, x: i32) -> Result<(i32, bool)> {
        match self.update_text_layout()? {
            None => Ok((0, false)),
            Some(layout) => {
                let mut is_trailing = FALSE;
                let mut is_inside = FALSE;
                let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                unsafe {
                    layout.HitTestPoint(
                        x as f32,
                        0f32,
                        &mut is_trailing,
                        &mut is_inside,
                        &mut metrics,
                    )?
                };
                Ok((metrics.textPosition as i32, is_trailing.as_bool()))
            }
        }
    }

    fn empty_undo_buffer(&mut self) {
        self.undo_insert_count = 0;
        self.undo_buffer.empty();
    }
}

impl QT {
    pub fn create_input(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_INPUT");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_CLASSDC | CS_DBLCLKS,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let scaling_factor = get_scaling_factor(parent_window);
            let boxed = Box::new(State {
                qt: self.clone(),
                width: props.width as f32 / scaling_factor,
                size: props.size,
                appearance: props.appearance,
                default_value: props.default_value,
                input_type: props.input_type,
                placeholder: props.placeholder,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (boxed.width * scaling_factor) as i32,
                (boxed.get_field_height() * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(
                    GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _
                )),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }
}

fn get_single_line_rect(
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
    Ok(RECT {
        left: pt1.min(pt2),
        top: context.format_rect.top,
        right: pt1.max(pt2),
        bottom: context.format_rect.top + context.line_height,
    })
}

fn invalidate_text(window: HWND, context: &mut Context, start: usize, end: usize) -> Result<()> {
    if start == end {
        return Ok(());
    }

    let actual_start = start.min(end);
    let actual_end = start.max(end);
    let line_rect = get_single_line_rect(window, context, actual_start, Some(actual_end))?;
    unsafe {
        let mut rc = RECT::default();
        if IntersectRect(&mut rc, &line_rect, &context.format_rect).into() {
            _ = InvalidateRect(Some(window), Some(&rc), true);
        }
    }
    Ok(())
}

fn set_selection(
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
    /* Note that at this point 'new_end' and 'old_start' are not in order, but start is definitely the min. and old_end is definitely the max. */
    if new_end != old_start {
        if old_start > new_end {
            invalidate_text(window, context, new_start, new_end)?;
            invalidate_text(window, context, old_start, old_end)?;
        } else {
            invalidate_text(window, context, new_start, old_start)?;
            invalidate_text(window, context, new_end, old_end)?;
        }
    } else {
        invalidate_text(window, context, new_start, old_end)?;
    }
    Ok(true)
}

fn replace_selection(
    window: HWND,
    context: &mut Context,
    can_undo: bool,
    replace: &[u16],
    honor_limit: bool,
) -> Result<()> {
    let mut start = context.selection_start;
    let mut end = context.selection_end;
    context.invalidate_text_layout();
    let mut replace_length = replace.len();
    if start == end && replace_length == 0 {
        return Ok(());
    }
    order_usize!(&mut start, &mut end);
    let text_length = context.get_text_length();
    let size = text_length - (end - start) + replace_length;
    if size == 0 {
        context.text_width = 0;
    }
    context.buffer.make_fit(size);
    let mut buf = StringBuffer::new();
    if end != start {
        let buf_length = end - start;
        buf = StringBuffer::with_capacity(buf_length);
        unsafe {
            u_memcpy(
                buf.as_mut_ptr(),
                context.buffer.as_ptr().offset(start as isize),
                buf_length as i32,
            );
            lstrcpynW(
                from_raw_parts_mut(
                    context.buffer.as_mut_ptr().offset(start as isize),
                    size - start + 1,
                ),
                PCWSTR::from_raw(context.buffer.as_ptr().offset(end as isize)),
            );
        }
        context.text_buffer_changed()?;
    }
    if replace_length != 0 {
        context.buffer.insert_at(start, replace);
        context.text_buffer_changed()?;
    }

    let fw = context.format_rect.right - context.format_rect.left;
    context.invalidate_text_layout();
    calculate_line_width(window, context)?;
    if honor_limit && context.text_width > fw {
        while (context.text_width > fw) && start + replace_length >= start {
            context.buffer.remove_at(start + replace_length - 1);
            replace_length = replace_length - 1;
            context.cached_text_length = None;
            context.invalidate_text_layout();
            calculate_line_width(window, context)?;
        }
        context.text_buffer_changed()?;
    }

    if end != start {
        if can_undo {
            unsafe {
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
                        buf.as_ptr(),
                        (end - start) as i32,
                    );
                    context.undo_position = start;
                }
                context.undo_insert_count = 0;
            }
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
    unsafe {
        _ = InvalidateRect(Some(window), Some(&context.format_rect), false);
    }

    scroll_caret(window, context)?;
    update_scroll_info(window, context);

    context.invalidate_text_layout();

    Ok(())
}

fn set_caret_position(window: HWND, context: &mut Context, position: usize) -> Result<()> {
    if context.is_focused {
        let res = position_from_char(window, context, position)?;
        unsafe {
            SetCaretPos(res.x, res.y)?;
            update_imm_composition_window(window, context, res.x, res.y);
        }
    }
    Ok(())
}

fn scroll_caret(window: HWND, context: &mut Context) -> Result<()> {
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
        unsafe {
            _ = InvalidateRect(Some(window), Some(&context.format_rect), true);
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
        unsafe {
            _ = InvalidateRect(Some(window), Some(&context.format_rect), true);
        }
    }

    set_caret_position(window, context, context.selection_end)?;
    Ok(())
}

fn update_scroll_info(window: HWND, context: &mut Context) {
    let si = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_PAGE | SIF_POS | SIF_RANGE | SIF_DISABLENOSCROLL,
        nMin: 0,
        nMax: context.text_width - 1,
        nPage: (context.format_rect.right - context.format_rect.left) as u32,
        nPos: context.x_offset as i32,
        nTrackPos: context.x_offset as i32,
    };
    unsafe {
        SetScrollInfo(window, SB_HORZ, &si, true);
    }
}

fn set_text(window: HWND, context: &mut Context, text: PCWSTR) -> Result<()> {
    set_selection(window, context, Some(0), None)?;
    unsafe {
        replace_selection(window, context, false, text.as_wide(), false)?;
    }
    context.x_offset = 0;
    set_selection(window, context, Some(0), Some(0))?;
    scroll_caret(window, context)?;
    update_scroll_info(window, context);
    context.invalidate_text_layout();
    Ok(())
}

fn adjust_format_rect(window: HWND, context: &mut Context) -> Result<()> {
    context.format_rect.right = context
        .format_rect
        .right
        .max(context.format_rect.left + context.char_width);
    let y_offset = (context.format_rect.bottom - context.format_rect.top - context.line_height) / 2;
    if y_offset > 0 {
        context.format_rect.top = context.format_rect.top + y_offset;
    }
    context.format_rect.bottom = context.format_rect.top + context.line_height;
    let mut client_rect = RECT::default();
    unsafe {
        GetClientRect(window, &mut client_rect)?;
    }
    let scaling_factor = get_scaling_factor(window);
    let border_bottom_width = (2.0 * scaling_factor) as i32;
    context.format_rect.bottom = context
        .format_rect
        .bottom
        .min(client_rect.bottom - border_bottom_width);
    set_caret_position(window, context, context.selection_end)
}

fn set_rect_np(window: HWND, context: &mut Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    unsafe {
        GetClientRect(window, &mut context.format_rect)?;
        let corner_diameter =
            (context.state.qt.theme.tokens.border_radius_medium * scaling_factor * 2f32) as i32;
        let region = CreateRoundRectRgn(
            0,
            0,
            context.format_rect.right + 1,
            context.format_rect.bottom + 1,
            corner_diameter,
            corner_diameter,
        );
        SetWindowRgn(window, Some(region), true);
        let border_width = (1.0 * scaling_factor) as i32;
        _ = InflateRect(&mut context.format_rect, -border_width, 0);
        if context.format_rect.bottom - context.format_rect.top
            > context.line_height + 2 * border_width
        {
            _ = InflateRect(&mut context.format_rect, 0, -border_width);
        }
    }
    let horizontal_padding = (context.state.get_horizontal_padding() * scaling_factor) as i32;
    context.format_rect.left = context.format_rect.left + horizontal_padding;
    context.format_rect.right = context.format_rect.right - horizontal_padding;
    adjust_format_rect(window, context)
}

fn calculate_line_width(_window: HWND, context: &mut Context) -> Result<()> {
    context.char_width = context.layout_text_width()?;
    Ok(())
}

fn position_from_char(_window: HWND, context: &mut Context, index: usize) -> Result<POINT> {
    let length = context.get_text_length();
    let mut x_off: usize = 0;
    if context.x_offset != 0 {
        if context.x_offset >= length {
            let leftover = context.x_offset - length;
            x_off = context.layout_text_width()? as usize;
            x_off += context.char_width as usize * leftover;
        } else {
            x_off = context.layout_cp_to_x(context.x_offset)? as usize;
        }
    }
    let index = index.min(length);
    let xi = if index != 0 {
        if index >= length {
            context.layout_text_width()? as usize
        } else {
            context.layout_cp_to_x(index)? as usize
        }
    } else {
        0
    };
    Ok(POINT {
        x: xi as i32 - x_off as i32 + context.format_rect.left,
        y: context.format_rect.top,
    })
}

fn char_from_position(_window: HWND, context: &mut Context, point: POINT) -> Result<usize> {
    let x = point.x - context.format_rect.left;
    if x == 0 {
        return Ok(context.x_offset);
    }

    let x_off = if context.x_offset != 0 {
        let length = context.get_text_length();
        if context.x_offset >= length {
            context.layout_text_width()?
        } else {
            context.layout_cp_to_x(context.x_offset)?
        }
    } else {
        0
    };
    let mut index;
    if x < 0 {
        let (cp, trailing) = context.layout_x_to_cp(x + x_off)?;
        index = cp;
        if trailing {
            index = index + 1;
        }
    } else {
        if x != 0 {
            let length = context.get_text_length();
            let text_width = context.layout_text_width()?;
            let (cp, trailing) = context.layout_x_to_cp(x + x_off)?;
            index = cp;
            if x > text_width {
                index = length as i32;
            } else if trailing {
                index = index + 1;
            }
        } else {
            index = context.x_offset as i32;
        }
    }
    Ok(index as usize)
}

fn clear(window: HWND, context: &mut Context) -> Result<()> {
    replace_selection(window, context, true, &[], true)
}

fn move_end(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let end = context.get_text_length();
    let start = if extend { context.selection_start } else { end };
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    Ok(())
}

fn move_home(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let end = 0;
    let start = if extend { context.selection_start } else { end };
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    Ok(())
}

fn move_forward(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let mut e = context.selection_end;

    if context.get_text_length() > e {
        e = e + 1;
    }
    let start = if extend { context.selection_start } else { e };
    set_selection(window, context, Some(start), Some(e))?;
    scroll_caret(window, context)?;
    Ok(())
}

fn move_backward(window: HWND, context: &mut Context, extend: bool) -> Result<()> {
    let mut e = context.selection_end;
    if e > 0 {
        e = e - 1;
    }
    let start = if extend { context.selection_start } else { e };
    set_selection(window, context, Some(start), Some(e))?;
    scroll_caret(window, context)?;
    Ok(())
}

fn create_font_from_typography_style(
    typography_style: &TypographyStyle,
    scaling_factor: f32,
) -> HFONT {
    unsafe {
        CreateFontW(
            (typography_style.line_height * scaling_factor) as i32,
            0,                                      // Width of the font (0 for default)
            0,                                      // Angle of escapement
            0,                                      // Orientation angle
            typography_style.font_weight.0,         // Font weight
            0,                                      // Italic (not italic)
            0,                                      // Underline (not underlined)
            0,                                      // Strikeout (not struck out)
            DEFAULT_CHARSET,                        // Character set (default)
            OUT_OUTLINE_PRECIS,                     // Output precision (outline)
            CLIP_DEFAULT_PRECIS,                    // Clipping precision (default)
            CLEARTYPE_QUALITY,                      // Font quality (ClearType)
            (FF_SWISS.0 | VARIABLE_PITCH.0) as u32, // Pitch and family (variable pitch)
            typography_style.font_family,
        )
    }
}

#[implement(IUIAnimationTimerEventHandler)]
struct AnimationTimerEventHandler {
    window: HWND,
}

impl IUIAnimationTimerEventHandler_Impl for AnimationTimerEventHandler_Impl {
    fn OnPreUpdate(&self) -> Result<()> {
        Ok(())
    }

    fn OnPostUpdate(&self) -> Result<()> {
        unsafe {
            let mut rc = RECT::default();
            GetClientRect(self.window, &mut rc)?;
            let scaling_factor = get_scaling_factor(self.window);
            let border_width = (1.0 * scaling_factor) as i32;
            let border_bottom_width = (2.0 * scaling_factor) as i32;
            _ = InvalidateRect(
                Some(self.window),
                Some(&RECT {
                    left: rc.left,
                    top: (rc.bottom - border_bottom_width).max(rc.top + border_width),
                    right: rc.right,
                    bottom: rc.bottom,
                }),
                false,
            );
        }
        Ok(())
    }

    fn OnRenderingTooSlow(&self, _frames_per_second: u32) -> Result<()> {
        Ok(())
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let scaling_factor = get_scaling_factor(window);
    let typography_style = state.get_typography_style();
    let font = create_font_from_typography_style(typography_style, scaling_factor);
    unsafe {
        let dc = GetDC(Some(window));
        let old_font = SelectObject(dc, font.into());
        let mut tm = TEXTMETRICW::default();
        if !GetTextMetricsW(dc, &mut tm).as_bool() {
            return Err(Error::empty());
        }
        SelectObject(dc, old_font);
        ReleaseDC(Some(window), dc);

        // Identity-DPI render target; the font is pre-scaled, so DIPs == device pixels.
        let mut client_rect = RECT::default();
        GetClientRect(window, &mut client_rect)?;
        let render_target = state.qt.d2d_factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: USER_DEFAULT_SCREEN_DPI as f32,
                dpiY: USER_DEFAULT_SCREEN_DPI as f32,
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
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            typography_style.font_family,
            None,
            typography_style.font_weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            typography_style.font_size * scaling_factor,
            w!(""),
        )?;
        text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;

        let animation_timer: IUIAnimationTimer =
            CoCreateInstance(&UIAnimationTimer, None, CLSCTX_INPROC_SERVER)?;
        let transition_library = state.qt.transition_library.clone();
        let animation_manager: IUIAnimationManager2 =
            CoCreateInstance(&UIAnimationManager2, None, CLSCTX_INPROC_SERVER)?;
        let timer_update_handler = animation_manager.cast::<IUIAnimationTimerUpdateHandler>()?;
        animation_timer
            .SetTimerUpdateHandler(&timer_update_handler, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE)?;
        let timer_event_handler: IUIAnimationTimerEventHandler =
            AnimationTimerEventHandler { window }.into();
        animation_timer.SetTimerEventHandler(&timer_event_handler)?;
        let bottom_focus_border = animation_manager.CreateAnimationVariable(0.0)?;
        Ok(Context {
            state,
            animation_manager,
            animation_timer,
            transition_library,
            bottom_focus_border,
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
            format_rect: RECT::default(),
            render_target,
            text_format,
            text_layout: None,
            font,
            line_height: tm.tmHeight,
            char_width: tm.tmAveCharWidth,
            text_width: 0,
            log_attribute: Vec::new(),
        })
    }
}

fn on_char(window: HWND, context: &mut Context, char: u16) -> Result<()> {
    unsafe {
        let control = GetKeyState(VK_CONTROL.0 as i32) < 0;
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
                if let Type::Password = context.state.input_type {
                } else {
                    SendMessageW(window, WM_COPY, None, None);
                }
            }
            0x16 => {
                // ^V
                SendMessageW(window, WM_PASTE, None, None);
            }
            0x18 => {
                // ^X
                if let Type::Password = context.state.input_type {
                } else {
                    SendMessageW(window, WM_CUT, None, None);
                }
            }
            0x1A => {
                // ^Z
                SendMessageW(window, WM_UNDO, None, None);
            }
            _ => {
                if let Type::Number = context.state.input_type {
                } else {
                    if char >= ' ' as u16 && char != 127 {
                        replace_selection(window, context, true, &[char], true)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn on_copy(window: HWND, context: &mut Context) -> Result<()> {
    let start = context.selection_start.min(context.selection_end);
    let end = context.selection_start.max(context.selection_end);
    if end == start {
        return Ok(());
    }
    unsafe {
        let length = end - start;
        let hdst = GlobalAlloc(GMEM_MOVEABLE, (length + 1) * size_of::<u16>())?;
        let dst = GlobalLock(hdst);
        u_memcpy(
            dst as _,
            context.buffer.as_ptr().offset(start as isize),
            length as i32,
        );
        *(dst as *mut u16).offset(length as isize) = 0;
        GlobalUnlock(hdst).or_else(|error| error.code().ok())?;
        OpenClipboard(Some(window))?;
        EmptyClipboard()?;
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hdst.0 as _)))?;
        CloseClipboard()?;
    }
    Ok(())
}

fn on_cut(window: HWND, context: &mut Context) -> Result<()> {
    on_copy(window, context)?;
    clear(window, context)?;
    Ok(())
}

fn on_paste(window: HWND, context: &mut Context) -> Result<()> {
    unsafe {
        OpenClipboard(Some(window))?;
        let hsrc = GetClipboardData(CF_UNICODETEXT.0 as u32)?;
        if !hsrc.is_invalid() {
            let src = GlobalLock(HGLOBAL(hsrc.0 as _));
            let string = PCWSTR::from_raw(src as _);
            let mut len = lstrlenW(string) as usize;
            if let Some(position) = string.as_wide().iter().position(|a| *a == '\n' as u16) {
                len = position;
                if len > 0 && string.as_wide()[len - 1] == '\r' as u16 {
                    len = len - 1;
                }
            }
            replace_selection(window, context, true, &string.as_wide()[..len], true)?;
            GlobalUnlock(HGLOBAL(hsrc.0 as _)).or_else(|error| error.code().ok())?;
        } else {
            if let Type::Password = context.state.input_type {
                replace_selection(window, context, true, &[], true)?;
            }
        }
        CloseClipboard()?;
    }
    Ok(())
}

fn on_undo(window: HWND, context: &mut Context) -> Result<()> {
    let text = context.undo_buffer.clone();
    set_selection(
        window,
        context,
        Some(context.undo_position),
        Some(context.undo_position + context.undo_insert_count),
    )?;
    context.undo_buffer.empty();
    unsafe {
        replace_selection(window, context, true, text.as_wcs().as_wide(), true)?;
    }
    set_selection(
        window,
        context,
        Some(context.undo_position),
        Some(context.undo_position + context.undo_insert_count),
    )?;
    scroll_caret(window, context)?;
    Ok(())
}

fn on_key_down(window: HWND, context: &mut Context, key: i32) -> Result<()> {
    unsafe {
        if GetKeyState(VK_MENU.0 as i32) < 0 {
            return Ok(());
        }
    }

    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let control = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;

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
            move_forward(window, context, shift)?;
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

fn on_kill_focus(window: HWND, context: &mut Context) -> Result<()> {
    context.is_focused = false;
    unsafe {
        DestroyCaret()?;
    }
    invalidate_text(
        window,
        context,
        context.selection_start,
        context.selection_end,
    )?;
    unsafe {
        _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE);
    }
    Ok(())
}

fn word_break_proc(
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
        unsafe {
            ScriptBreak(
                context.buffer.as_wcs(),
                length as i32,
                &psa,
                context.log_attribute.as_mut_ptr(),
            )?
        };
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

fn call_word_break_proc(
    context: &mut Context,
    start: usize,
    index: usize,
    count: usize,
    action: WORD_BREAK_ACTION,
) -> Result<usize> {
    Ok(word_break_proc(context, index + start, count + start, action)? - start)
}

fn on_double_click(window: HWND, context: &mut Context) -> Result<()> {
    context.is_captured = true;
    unsafe {
        SetCapture(window);
    }
    let length = context.get_text_length();
    let start = call_word_break_proc(context, 0, context.selection_end, length, WB_LEFT)?;
    let end = call_word_break_proc(context, 0, context.selection_end, length, WB_RIGHT)?;
    set_selection(window, context, Some(start), Some(end))?;
    scroll_caret(window, context)?;
    Ok(())
}

fn on_left_button_down(
    window: HWND,
    context: &mut Context,
    keys: u32,
    mut x: i32,
    mut y: i32,
) -> Result<()> {
    context.is_captured = true;
    unsafe {
        SetCapture(window);
    }
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
        unsafe {
            SetFocus(Some(window))?;
        }
    }
    Ok(())
}

fn on_left_button_up(window: HWND, context: &mut Context) -> Result<()> {
    if context.is_captured {
        unsafe {
            if GetCapture() == window {
                ReleaseCapture()?;
            }
        }
        context.is_captured = false;
    }
    Ok(())
}

fn on_mouse_move(window: HWND, context: &mut Context, x: i32, y: i32) -> Result<()> {
    unsafe {
        if !context.is_captured || GetCapture() != window {
            return Ok(());
        }
    }

    let end = char_from_position(window, context, POINT { x, y })?;
    set_selection(window, context, Some(context.selection_start), Some(end))?;
    set_caret_position(window, context, context.selection_end)?;
    scroll_caret(window, context)?;
    Ok(())
}

fn sys_color_to_d2d(index: SYS_COLOR_INDEX) -> D2D1_COLOR_F {
    let c = unsafe { GetSysColor(index) };
    D2D1_COLOR_F {
        r: (c & 0xff) as f32 / 255.0,
        g: ((c >> 8) & 0xff) as f32 / 255.0,
        b: ((c >> 16) & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// Content pass: text, selection, placeholder. Chrome is drawn by the caller.
fn paint(window: HWND, context: &mut Context) -> Result<()> {
    // Owned snapshots so the `tokens` borrow doesn't outlive the mutable calls below.
    let background_color = context.state.qt.theme.tokens.color_neutral_background1;
    let foreground_color = context.state.qt.theme.tokens.color_neutral_foreground1;
    let rt = context.render_target.clone();
    unsafe {
        rt.Clear(Some(&background_color));
    }

    let format_rect = context.format_rect;
    let clip = D2D_RECT_F {
        left: format_rect.left as f32,
        top: format_rect.top as f32,
        right: format_rect.right as f32,
        bottom: (format_rect.top + context.line_height) as f32,
    };

    if context.get_text_length() == 0 {
        if let Some(placeholder) = context.state.placeholder {
            let brush = unsafe {
                rt.CreateSolidColorBrush(&sys_color_to_d2d(COLOR_GRAYTEXT), None)?
            };
            let rect = D2D_RECT_F {
                left: format_rect.left as f32,
                top: format_rect.top as f32,
                right: format_rect.right as f32,
                bottom: (format_rect.top + context.line_height) as f32,
            };
            unsafe {
                rt.DrawText(
                    placeholder.as_wide(),
                    &context.text_format,
                    &rect,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
        return Ok(());
    }

    let origin_x = position_from_char(window, context, 0)?.x as f32;
    let origin = Vector2 {
        X: origin_x,
        Y: format_rect.top as f32,
    };
    let text_brush = unsafe {
        rt.CreateSolidColorBrush(&foreground_color, None)?
    };

    let layout = match context.update_text_layout()? {
        None => return Ok(()),
        Some(layout) => layout,
    };

    unsafe {
        rt.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
        rt.DrawTextLayout(origin, &layout, &text_brush, D2D1_DRAW_TEXT_OPTIONS_NONE);

        // Selection is shown only while focused (matches classic EDIT).
        let sel_start = context.selection_start.min(context.selection_end);
        let sel_end = context.selection_start.max(context.selection_end);
        if context.is_focused && sel_start != sel_end {
            let x0 = position_from_char(window, context, sel_start)?.x as f32;
            let x1 = position_from_char(window, context, sel_end)?.x as f32;
            let sel_rect = D2D_RECT_F {
                left: x0,
                top: format_rect.top as f32,
                right: x1,
                bottom: (format_rect.top + context.line_height) as f32,
            };
            let hl_brush = rt.CreateSolidColorBrush(&sys_color_to_d2d(COLOR_HIGHLIGHT), None)?;
            rt.FillRectangle(&sel_rect, &hl_brush);
            let hl_text_brush =
                rt.CreateSolidColorBrush(&sys_color_to_d2d(COLOR_HIGHLIGHTTEXT), None)?;
            rt.PushAxisAlignedClip(&sel_rect, D2D1_ANTIALIAS_MODE_ALIASED);
            rt.DrawTextLayout(origin, &layout, &hl_text_brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
            rt.PopAxisAlignedClip();
        }

        rt.PopAxisAlignedClip();
    }
    Ok(())
}

fn on_paint(window: HWND, context: &mut Context) -> Result<()> {
    let rt = context.render_target.clone();
    unsafe {
        HideCaret(Some(window)).ok();
        rt.BeginDraw();
        let result = paint_content_and_chrome(window, context);
        let end = rt.EndDraw(None, None);
        ShowCaret(Some(window)).ok();
        result.and(end)
    }
}

/// Bottom edge plus the lower half of each rounded corner (the resting underline).
fn bottom_accent_geometry(
    factory: &ID2D1Factory1,
    width: f32,
    r: f32,
    cy: f32,
) -> Result<ID2D1PathGeometry1> {
    let left_cx = r;
    let right_cx = width - r;
    let corner_cy = cy - r;
    let d = r * std::f32::consts::FRAC_1_SQRT_2;
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.BeginFigure(
            Vector2 {
                X: left_cx - d,
                Y: corner_cy + d,
            },
            D2D1_FIGURE_BEGIN_HOLLOW,
        );
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: left_cx,
                Y: cy,
            },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(Vector2 {
            X: right_cx,
            Y: cy,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: right_cx + d,
                Y: corner_cy + d,
            },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        Ok(geometry)
    }
}

fn paint_content_and_chrome(window: HWND, context: &mut Context) -> Result<()> {
    paint(window, context)?;

    let rt = context.render_target.clone();
    let tokens = &context.state.qt.theme.tokens;
    let scaling_factor = get_scaling_factor(window);
    let mut rc = RECT::default();
    unsafe {
        GetClientRect(window, &mut rc)?;
    }
    let width = rc.right as f32;
    let height = rc.bottom as f32;
    let stroke = tokens.stroke_width_thin * scaling_factor;
    let radius = tokens.border_radius_medium * scaling_factor;
    let border_bottom_width = 2.0 * scaling_factor;

    unsafe {
        // Filled variants skip the full border, keeping only the bottom accent.
        if let Appearance::Outline = context.state.appearance {
            let border_color = if context.is_focused {
                &tokens.color_neutral_stroke1_pressed
            } else {
                &tokens.color_neutral_stroke1
            };
            let border_brush = rt.CreateSolidColorBrush(border_color, None)?;
            let rounded = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: stroke * 0.5,
                    top: stroke * 0.5,
                    right: width - stroke * 0.5,
                    bottom: height - stroke * 0.5,
                },
                radiusX: radius,
                radiusY: radius,
            };
            rt.DrawRoundedRectangle(&rounded, &border_brush, stroke, &context.state.qt.stroke_style);
        }

        let accent_brush = rt.CreateSolidColorBrush(&tokens.color_neutral_stroke_accessible, None)?;
        let accent_geometry =
            bottom_accent_geometry(&context.state.qt.d2d_factory, width, radius, height - stroke * 0.5)?;
        rt.DrawGeometry(&accent_geometry, &accent_brush, stroke, &context.state.qt.stroke_style);

        // Brand underline grows from the centre on focus, over the resting line.
        if context.is_focused {
            let percentage = context.bottom_focus_border.GetValue()?;
            let left = width as f64 * (1.0 - percentage) / 2.0;
            let underline_brush =
                rt.CreateSolidColorBrush(&tokens.color_compound_brand_stroke, None)?;
            rt.FillRectangle(
                &D2D_RECT_F {
                    left: left as f32,
                    top: height - border_bottom_width,
                    right: (left + width as f64 * percentage) as f32,
                    bottom: height,
                },
                &underline_brush,
            );
        }
    }
    Ok(())
}

fn set_focus(window: HWND, context: &mut Context) -> Result<()> {
    context.is_focused = true;
    invalidate_text(
        window,
        context,
        context.selection_start,
        context.selection_end,
    )?;
    let scaling_factor = get_scaling_factor(window);
    unsafe {
        CreateCaret(
            window,
            None,
            (1.0 * scaling_factor) as i32,
            context.line_height,
        )?;
        set_caret_position(window, context, context.selection_end)?;
        ShowCaret(Some(window))?;
        _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE);
        let tokens = &context.state.qt.theme.tokens;
        let transition = context
            .transition_library
            .CreateCubicBezierLinearTransition(
                tokens.duration_normal,
                1.0,
                tokens.curve_decelerate_mid[0],
                tokens.curve_decelerate_mid[1],
                tokens.curve_decelerate_mid[2],
                tokens.curve_decelerate_mid[3],
            )?;
        let seconds_now = context.animation_timer.GetTime()?;
        context.bottom_focus_border = context.animation_manager.CreateAnimationVariable(0.0)?;
        context.animation_manager.ScheduleTransition(
            &context.bottom_focus_border,
            &transition,
            seconds_now,
        )?;
    }
    Ok(())
}

fn update_imm_composition_window(window: HWND, context: &Context, x: i32, y: i32) {
    let form = COMPOSITIONFORM {
        dwStyle: CFS_RECT,
        ptCurrentPos: POINT { x, y },
        rcArea: context.format_rect,
    };
    unsafe {
        let himc = ImmGetContext(window);
        _ = ImmSetCompositionWindow(himc, &form);
        _ = ImmReleaseContext(window, himc);
    }
}

fn update_imm_composition_font(window: HWND, context: &Context) {
    unsafe {
        let himc = ImmGetContext(window);
        let mut composition_font = LOGFONTW::default();
        GetObjectW(
            context.font.into(),
            size_of::<LOGFONTW>() as i32,
            Some(&mut composition_font as *mut LOGFONTW as _),
        );
        _ = ImmSetCompositionFontW(himc, &composition_font);
        _ = ImmReleaseContext(window, himc);
    }
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
            match on_create(window, *state).and_then(|mut context| {
                set_rect_np(window, &mut context)?;
                if let Some(default_text) = context.state.default_value {
                    replace_selection(window, &mut context, false, default_text.as_wide(), false)?;
                }
                Ok(context)
            }) {
                Ok(mut context) => {
                    update_scroll_info(window, &mut context);
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = Box::<Context>::from_raw(raw);
            _ = DeleteObject(context.font.into());
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
        WM_UNDO => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            match on_undo(window, context) {
                Ok(_) => LRESULT(TRUE.0 as isize),
                Err(_) => LRESULT(FALSE.0 as isize),
            }
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
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = on_paint(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_paint(window, context);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_PASTE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = on_paste(window, context);
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
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let mut point = POINT::default();
            if GetCaretPos(&mut point).is_ok() {
                _ = update_imm_composition_window(window, context, point.x, point.y);
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                let context = &*raw;
                _ = update_imm_composition_font(window, context);
            }
            LRESULT::default()
        },
        WM_IME_COMPOSITION => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = replace_selection(window, context, true, &[], true);
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
                            MapWindowPoints(Some(window), Some(HWND_DESKTOP), &mut [char_pos.pt]);
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
                            MapWindowPoints(Some(window), Some(HWND_DESKTOP), &mut doc_points);
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
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let scaling_factor = get_scaling_factor(window);
            if SetWindowPos(
                window,
                None,
                0,
                0,
                (context.state.width * scaling_factor) as i32,
                (context.state.get_field_height() * scaling_factor) as i32,
                SWP_NOMOVE | SWP_NOZORDER,
            )
            .is_ok()
            {
                let typography_style = context.state.get_typography_style();
                // GDI font is still used for text metrics and IME composition.
                let font = create_font_from_typography_style(typography_style, scaling_factor);
                let dc = GetDC(Some(window));
                let old_font = SelectObject(dc, font.into());
                let mut tm = TEXTMETRICW::default();
                if GetTextMetricsW(dc, &mut tm).into() {
                    context.line_height = tm.tmHeight;
                    context.char_width = tm.tmAveCharWidth;
                }
                SelectObject(dc, old_font);
                ReleaseDC(Some(window), dc);
                _ = DeleteObject(context.font.into());
                context.font = font;

                // Rebuild text format at the new scale; resize the target; drop the layout.
                if let Ok(text_format) = context.state.qt.dwrite_factory.CreateTextFormat(
                    typography_style.font_family,
                    None,
                    typography_style.font_weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    typography_style.font_size * scaling_factor,
                    w!(""),
                ) {
                    _ = text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                    context.text_format = text_format;
                }
                context.invalidate_text_layout();
                let mut client_rect = RECT::default();
                if GetClientRect(window, &mut client_rect).is_ok() {
                    _ = context.render_target.Resize(&D2D_SIZE_U {
                        width: (client_rect.right - client_rect.left) as u32,
                        height: (client_rect.bottom - client_rect.top) as u32,
                    });
                }

                if set_rect_np(window, context).is_ok() {
                    _ = InvalidateRect(Some(window), None, true);
                }
            }
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
