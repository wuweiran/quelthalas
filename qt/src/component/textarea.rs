//! A multiline text field — Win32 `ES_MULTILINE` EDIT, Fluent-styled. Word-wrapped
//! editable text with a 2D caret, multi-line selection, and vertical scrolling
//! (wheel + draggable thumb + auto-scroll-to-caret). The chrome (rounded border,
//! bottom accent, brand focus underline) is cloned from `input`/`combobox`; the
//! editor core is a fresh multiline implementation over a single word-wrapped
//! `IDWriteTextLayout` (input's editor is single-line + module-private). Scrolling
//! is delegated to the shared `scroll::VScroll` helper.

use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1Factory1, ID2D1HwndRenderTarget,
    ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_HIT_TEST_METRICS,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS,
    DWRITE_WORD_WRAPPING_WRAP, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, CreateRoundRectRgn, EndPaint, GetSysColor,
    InvalidateRect, PAINTSTRUCT, RDW_INVALIDATE, RedrawWindow, SYS_COLOR_INDEX, ScreenToClient,
    SetWindowRgn,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Animation::{
    IUIAnimationManager2, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionLibrary2, IUIAnimationVariable2, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE,
    UIAnimationManager2, UIAnimationTimer,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VIRTUAL_KEY, VK_A, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

use crate::component::input;
use crate::component::menu::MenuInfo;
use crate::component::scroll::{SCROLLBAR_W, ScrollHit, VScroll};
use crate::{QT, get_scaling_factor};

const CARET_TIMER_ID: usize = 1;
const REPEAT_TIMER_ID: usize = 2;
const REPEAT_INITIAL_MS: u32 = 250;
const REPEAT_INTERVAL_MS: u32 = 40;

// Context-menu command ids.
const CMD_CUT: u32 = 1;
const CMD_COPY: u32 = 2;
const CMD_PASTE: u32 = 3;
const CMD_SELECT_ALL: u32 = 4;

// user32.dll string-table ids for the edit context menu (MUI-localized).
const IDS_CUT: u32 = 769;
const IDS_COPY: u32 = 770;
const IDS_PASTE: u32 = 771;
const IDS_SELECT_ALL: u32 = 773;

/// Load a localized string from user32's string table, falling back to `fallback`
/// (and stripping the `&` accelerator marker). Leaked so the `PCWSTR` stays valid
/// for the menu's lifetime.
fn system_string(id: u32, fallback: PCWSTR) -> PCWSTR {
    unsafe {
        let mut buf = [0u16; 128];
        let module = GetModuleHandleW(w!("user32.dll")).unwrap_or_default();
        let len = LoadStringW(
            Some(HINSTANCE(module.0)),
            id,
            PWSTR(buf.as_mut_ptr()),
            buf.len() as i32,
        );
        let text: Vec<u16> = if len > 0 {
            buf[..len as usize]
                .iter()
                .copied()
                .filter(|&c| c != '&' as u16)
                .chain(std::iter::once(0))
                .collect()
        } else {
            fallback.as_wide().iter().copied().chain(std::iter::once(0)).collect()
        };
        PCWSTR::from_raw(Box::leak(text.into_boxed_slice()).as_ptr())
    }
}
/// Gap (DIPs) between the scrollbar's inner edge and where wrapped text stops.
const SCROLLBAR_GAP: f32 = 4.0;
/// Gap (DIPs) between the scrollbar's outer edge and the field outline.
const SCROLLBAR_MARGIN: f32 = 2.0;

pub struct Props {
    pub width: i32,
    pub height: i32,
    pub size: input::Size,
    pub appearance: input::Appearance,
    pub default_value: Option<PCWSTR>,
    pub placeholder: Option<PCWSTR>,
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            width: 0,
            height: 0,
            size: input::Size::Medium,
            appearance: input::Appearance::Outline,
            default_value: None,
            placeholder: None,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    width: f32,
    height: f32,
    size: input::Size,
    appearance: input::Appearance,
    placeholder: Option<PCWSTR>,
    background: Option<D2D1_COLOR_F>,
}

impl State {
    fn horizontal_padding(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        tokens.spacing_horizontal_m_nudge + tokens.spacing_horizontal_xxs
    }
    fn vertical_padding(&self) -> f32 {
        self.qt.theme.tokens.spacing_vertical_s_nudge
    }
    fn font_size(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            input::Size::Small => tokens.font_size_base200,
            input::Size::Medium => tokens.font_size_base300,
            input::Size::Large => tokens.font_size_base400,
        }
    }
    fn line_height(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            input::Size::Small => tokens.line_height_base200,
            input::Size::Medium => tokens.line_height_base300,
            input::Size::Large => tokens.line_height_base400,
        }
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    bottom_focus_border: IUIAnimationVariable2,
    is_focused: bool,
    is_hovered: bool,
    // --- editor ---
    buffer: Vec<u16>,
    caret: usize,
    sel_anchor: usize,
    caret_visible: bool,
    dragging_text: bool,
    // --- scroll ---
    scroll: VScroll,
}

impl Context {
    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.sel_anchor), self.caret.max(self.sel_anchor))
    }

    /// The content rectangle (inside padding), in DIPs. The scrollbar gutter, when
    /// present, is carved from the right of the text column.
    fn content_rect(&self) -> D2D_RECT_F {
        let pad = self.state.horizontal_padding();
        let vpad = self.state.vertical_padding();
        D2D_RECT_F {
            left: pad,
            top: vpad,
            right: self.state.width - pad,
            bottom: self.state.height - vpad,
        }
    }

    /// The width text wraps to (content width minus the scrollbar gutter when the
    /// scrollbar is showing).
    fn wrap_width(&self) -> f32 {
        let c = self.content_rect();
        let mut w = c.right - c.left;
        if self.scroll.visible() {
            // Text stops at the scrollbar's left edge (which sits out near the
            // outline, past the content padding).
            w = self.track_rect().left - SCROLLBAR_GAP - c.left;
        }
        w.max(1.0)
    }

    /// The scrollbar track rect. It sits in the right margin near the outline —
    /// outside the text content padding — like a native desktop scrollbar.
    fn track_rect(&self) -> D2D_RECT_F {
        let stroke = self.state.qt.theme.tokens.stroke_width_thin;
        let right = self.state.width - stroke - SCROLLBAR_MARGIN;
        D2D_RECT_F {
            left: right - SCROLLBAR_W,
            top: self.content_rect().top,
            right,
            bottom: self.content_rect().bottom,
        }
    }

    /// A fresh word-wrapped layout over the whole buffer at the current wrap width.
    fn make_layout(&self) -> Option<IDWriteTextLayout> {
        unsafe {
            let layout = self
                .state
                .qt
                .dwrite_factory
                .CreateTextLayout(&self.buffer, &self.text_format, self.wrap_width(), f32::MAX)
                .ok()?;
            _ = layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            Some(layout)
        }
    }

    /// Total laid-out content height (DIPs) for the current buffer/wrap width.
    fn content_height(&self) -> f32 {
        match self.make_layout() {
            Some(layout) => {
                let mut m = DWRITE_TEXT_METRICS::default();
                if unsafe { layout.GetMetrics(&mut m) }.is_ok() {
                    (m.height).max(self.state.line_height())
                } else {
                    self.state.line_height()
                }
            }
            None => self.state.line_height(),
        }
    }
}

impl QT {
    pub fn create_textarea(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_TEXTAREA");
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
            let width = if props.width > 0 { props.width as f32 } else { 260.0 };
            let height = if props.height > 0 { props.height as f32 } else { 96.0 };
            let boxed = Box::new(State {
                qt: self.clone(),
                width,
                height,
                size: props.size,
                appearance: props.appearance,
                placeholder: props.placeholder,
                background: props.background,
            });
            let default_value = props.default_value;
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (width * scaling_factor) as i32,
                (height * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )?;
            if let Some(text) = default_value {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Context;
                if !raw.is_null() {
                    let context = &mut *raw;
                    context.buffer = text.as_wide().to_vec();
                    context.caret = 0;
                    context.sel_anchor = 0;
                    update_metrics(context);
                    _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            Ok(hwnd)
        }
    }

    /// The textarea's current text.
    pub fn textarea_value(&self, textarea: HWND) -> String {
        unsafe {
            let raw = GetWindowLongPtrW(textarea, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                String::new()
            } else {
                String::from_utf16_lossy(&(*raw).buffer)
            }
        }
    }
}

fn create_text_format(qt: &QT, font_size: f32, line_height: f32) -> Result<IDWriteTextFormat> {
    let tokens = &qt.theme.tokens;
    unsafe {
        let format = qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            font_size,
            w!(""),
        )?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
        // Fluent lineHeightBase* — pin the line box to the token (not the font's
        // default metric spacing). Baseline at 80% keeps text vertically centered.
        format.SetLineSpacing(
            DWRITE_LINE_SPACING_METHOD_UNIFORM,
            line_height,
            line_height * 0.8,
        )?;
        Ok(format)
    }
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

fn on_create(window: HWND, state: State) -> Result<Context> {
    let font_size = state.font_size();
    let line_height = state.line_height();
    unsafe {
        let text_format = create_text_format(&state.qt, font_size, line_height)?;
        let dpi = GetDpiForWindow(window);
        let render_target = state.qt.d2d_factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U { width: 0, height: 0 },
                presentOptions: Default::default(),
            },
        )?;

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
            text_format,
            render_target,
            animation_manager,
            animation_timer,
            transition_library,
            bottom_focus_border,
            is_focused: false,
            is_hovered: false,
            buffer: Vec::new(),
            caret: 0,
            sel_anchor: 0,
            caret_visible: false,
            dragging_text: false,
            scroll: VScroll::new(),
        })
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
            _ = InvalidateRect(Some(self.window), None, false);
        }
        Ok(())
    }
    fn OnRenderingTooSlow(&self, _fps: u32) -> Result<()> {
        Ok(())
    }
}

fn start_focus_animation(context: &mut Context) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    unsafe {
        let transition = context.transition_library.CreateCubicBezierLinearTransition(
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

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (state.width * scaling_factor).ceil() as i32;
    let scaled_height = (state.height * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(
            window,
            None,
            0,
            0,
            scaled_width,
            scaled_height,
            SWP_NOMOVE | SWP_NOZORDER,
        )?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;
        let corner_diameter =
            (state.qt.theme.tokens.border_radius_medium * scaling_factor * 2.0) as i32;
        let region = CreateRoundRectRgn(
            0,
            0,
            scaled_width + 1,
            scaled_height + 1,
            corner_diameter,
            corner_diameter,
        );
        SetWindowRgn(window, Some(region), true);
    }
    Ok(())
}

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
            Vector2 { X: left_cx - d, Y: corner_cy + d },
            D2D1_FIGURE_BEGIN_HOLLOW,
        );
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: left_cx, Y: cy },
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.AddLine(Vector2 { X: right_cx, Y: cy });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: right_cx + d, Y: corner_cy + d },
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        Ok(geometry)
    }
}

/// Caret's content-space (x, y-top, line-height) via HitTestTextPosition.
fn caret_point(context: &Context, cp: usize) -> (f32, f32, f32) {
    if let Some(layout) = context.make_layout() {
        let mut x = 0f32;
        let mut y = 0f32;
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if unsafe { layout.HitTestTextPosition(cp as u32, false, &mut x, &mut y, &mut m) }.is_ok() {
            return (x, y, m.height.max(context.state.line_height()));
        }
    }
    (0.0, 0.0, context.state.line_height())
}

/// Caret nearest a content-space point (x, y already offset into content coords).
fn point_to_caret(context: &Context, x: f32, y: f32) -> usize {
    if let Some(layout) = context.make_layout() {
        let mut is_trailing = FALSE;
        let mut is_inside = FALSE;
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if unsafe { layout.HitTestPoint(x, y, &mut is_trailing, &mut is_inside, &mut m) }.is_ok() {
            return (m.textPosition + if is_trailing.as_bool() { 1 } else { 0 }) as usize;
        }
    }
    0
}

/// Recompute scroll metrics from the current buffer/layout.
fn update_metrics(context: &mut Context) {
    let content_h = context.content_height();
    let c = context.content_rect();
    let viewport_h = c.bottom - c.top;
    context
        .scroll
        .set_metrics(content_h, viewport_h, context.state.line_height());
}

/// Scroll the caret line into view.
fn scroll_caret_into_view(context: &mut Context) {
    let (_, y, h) = caret_point(context, context.caret);
    context.scroll.ensure_visible(y, y + h);
}

fn reset_blink(window: HWND) {
    unsafe {
        _ = SetTimer(Some(window), CARET_TIMER_ID, GetCaretBlinkTime(), None);
    }
}

fn after_edit(window: HWND, context: &mut Context) {
    update_metrics(context);
    scroll_caret_into_view(context);
    context.caret_visible = true;
    reset_blink(window);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

fn insert_text(window: HWND, context: &mut Context, chars: &[u16]) {
    let (s, e) = context.selection();
    context.buffer.splice(s..e, chars.iter().copied());
    context.caret = s + chars.len();
    context.sel_anchor = context.caret;
    after_edit(window, context);
}

fn delete_selection_or(window: HWND, context: &mut Context, forward: bool) {
    let (s, e) = context.selection();
    if s != e {
        context.buffer.drain(s..e);
        context.caret = s;
    } else if forward {
        if context.caret < context.buffer.len() {
            context.buffer.remove(context.caret);
        }
    } else if context.caret > 0 {
        context.caret -= 1;
        context.buffer.remove(context.caret);
    }
    context.sel_anchor = context.caret;
    after_edit(window, context);
}

fn move_caret(window: HWND, context: &mut Context, to: usize, extend: bool) {
    context.caret = to.min(context.buffer.len());
    if !extend {
        context.sel_anchor = context.caret;
    }
    scroll_caret_into_view(context);
    context.caret_visible = true;
    reset_blink(window);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

/// Word class for double-click selection: alphanumeric runs, whitespace runs, and
/// single punctuation each form a unit.
#[derive(PartialEq)]
enum CharClass {
    Word,
    Space,
    Other,
}

fn classify(c: u16) -> CharClass {
    match char::from_u32(c as u32) {
        Some(ch) if ch.is_alphanumeric() || ch == '_' => CharClass::Word,
        Some(ch) if ch == ' ' || ch == '\t' => CharClass::Space,
        _ => CharClass::Other,
    }
}

/// Select the word (or whitespace run / single punctuation) around `pos`.
fn select_word_at(window: HWND, context: &mut Context, pos: usize) {
    let len = context.buffer.len();
    if len == 0 {
        return;
    }
    // Anchor the class on the char to the left of the caret when at a boundary.
    let idx = pos.min(len - 1);
    let class = classify(context.buffer[idx]);
    if class == CharClass::Other {
        // Single punctuation char.
        context.sel_anchor = idx;
        context.caret = (idx + 1).min(len);
    } else {
        let mut start = idx;
        while start > 0 && classify(context.buffer[start - 1]) == class {
            start -= 1;
        }
        let mut end = idx;
        while end < len && classify(context.buffer[end]) == class {
            end += 1;
        }
        context.sel_anchor = start;
        context.caret = end;
    }
    context.caret_visible = true;
    reset_blink(window);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

/// Caret index at the start of the visual line containing `cp`.
fn line_home(context: &Context, cp: usize) -> usize {
    let (_, y, _) = caret_point(context, cp);
    point_to_caret(context, 0.0, y + 1.0)
}

/// Caret index at the end of the visual line containing `cp`.
fn line_end(context: &Context, cp: usize) -> usize {
    let (_, y, h) = caret_point(context, cp);
    // A large x lands past the last glyph on the line → trailing → line end.
    let end = point_to_caret(context, f32::MAX, y + h * 0.5);
    end.min(context.buffer.len())
}

/// Vertical navigation: from the caret, move one line up/down keeping the x column.
fn move_vertical(window: HWND, context: &mut Context, dir: f32, extend: bool) {
    let (x, y, h) = caret_point(context, context.caret);
    let target_y = y + dir * h + h * 0.5;
    let to = point_to_caret(context, x, target_y.max(0.0));
    move_caret(window, context, to, extend);
}

fn page_lines(context: &Context) -> f32 {
    let c = context.content_rect();
    ((c.bottom - c.top) / context.state.line_height()).floor().max(1.0)
}

fn copy_selection(window: HWND, context: &Context) -> Result<()> {
    let (s, e) = context.selection();
    if s == e {
        return Ok(());
    }
    let slice = &context.buffer[s..e];
    unsafe {
        let hdst = GlobalAlloc(GMEM_MOVEABLE, (slice.len() + 1) * size_of::<u16>())?;
        let dst = GlobalLock(hdst) as *mut u16;
        std::ptr::copy_nonoverlapping(slice.as_ptr(), dst, slice.len());
        *dst.add(slice.len()) = 0;
        _ = GlobalUnlock(hdst);
        OpenClipboard(Some(window))?;
        EmptyClipboard()?;
        SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(hdst.0 as _)))?;
        CloseClipboard()?;
    }
    Ok(())
}

fn paste_clipboard(window: HWND, context: &mut Context) -> Result<()> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_err() {
            return Ok(());
        }
        OpenClipboard(Some(window))?;
        let hsrc = GetClipboardData(CF_UNICODETEXT.0 as u32)?;
        let mut chars: Vec<u16> = Vec::new();
        if !hsrc.0.is_null() {
            let src = GlobalLock(HGLOBAL(hsrc.0 as _)) as *const u16;
            if !src.is_null() {
                let mut i = 0;
                loop {
                    let ch = *src.add(i);
                    if ch == 0 {
                        break;
                    }
                    // Keep newlines (multiline), drop carriage returns and other
                    // control chars.
                    if ch >= b' ' as u16 || ch == b'\n' as u16 {
                        chars.push(ch);
                    }
                    i += 1;
                }
                _ = GlobalUnlock(HGLOBAL(hsrc.0 as _));
            }
        }
        CloseClipboard()?;
        if !chars.is_empty() {
            insert_text(window, context, &chars);
        }
    }
    Ok(())
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let background = state
            .background
            .unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let stroke = tokens.stroke_width_thin;
        let radius = tokens.border_radius_medium;

        // Field box.
        let field_rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: stroke * 0.5,
                top: stroke * 0.5,
                right: width - stroke * 0.5,
                bottom: height - stroke * 0.5,
            },
            radiusX: radius,
            radiusY: radius,
        };
        let bg = match state.appearance {
            input::Appearance::FilledDarker => tokens.color_neutral_background3,
            _ => tokens.color_neutral_background1,
        };
        let fill_brush = context.render_target.CreateSolidColorBrush(&bg, None)?;
        context.render_target.FillRoundedRectangle(&field_rect, &fill_brush);

        // --- content: text, selection, caret, scrollbar (clipped) ---
        // Text is laid out inside the padding, but it scrolls through the padding
        // and clips at the *padding box* (just inside the border) — like a web
        // textarea — so the top/bottom padding stays filled while scrolling. The
        // bottom stops 1px above the brand focus bar.
        let content = context.content_rect();
        let offset = context.scroll.offset();
        context.render_target.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: content.left,
                top: stroke,
                right: content.right,
                bottom: height - 3.0,
            },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );

        if context.buffer.is_empty() {
            if let Some(placeholder) = state.placeholder {
                if !placeholder.is_null() && !placeholder.as_wide().is_empty() {
                    let ph_brush = context
                        .render_target
                        .CreateSolidColorBrush(&tokens.color_neutral_foreground3, None)?;
                    context.render_target.DrawText(
                        placeholder.as_wide(),
                        &context.text_format,
                        &D2D_RECT_F {
                            left: content.left,
                            top: content.top,
                            right: content.left + context.wrap_width(),
                            bottom: content.bottom,
                        },
                        &ph_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
        } else if let Some(text_layout) = context.make_layout() {
            let origin = Vector2 {
                X: content.left,
                Y: content.top - offset,
            };

            // Selection: one filled rect per visual line via HitTestTextRange.
            let (s, e) = context.selection();
            if s != e {
                let hl_brush = context
                    .render_target
                    .CreateSolidColorBrush(&sys_color_to_d2d(COLOR_HIGHLIGHT), None)?;
                let mut count: u32 = 0;
                _ = text_layout.HitTestTextRange(
                    s as u32,
                    (e - s) as u32,
                    origin.X,
                    origin.Y,
                    None,
                    &mut count,
                );
                if count > 0 {
                    let mut hits = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
                    let mut actual: u32 = 0;
                    if text_layout
                        .HitTestTextRange(
                            s as u32,
                            (e - s) as u32,
                            origin.X,
                            origin.Y,
                            Some(&mut hits),
                            &mut actual,
                        )
                        .is_ok()
                    {
                        for h in &hits[..actual as usize] {
                            context.render_target.FillRectangle(
                                &D2D_RECT_F {
                                    left: h.left,
                                    top: h.top,
                                    right: h.left + h.width,
                                    bottom: h.top + h.height,
                                },
                                &hl_brush,
                            );
                        }
                    }
                }
            }

            let text_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
            context.render_target.DrawTextLayout(
                origin,
                &text_layout,
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );

            // Selected glyphs re-drawn in highlight-text color, clipped per line.
            if s != e {
                let mut count: u32 = 0;
                _ = text_layout.HitTestTextRange(
                    s as u32,
                    (e - s) as u32,
                    origin.X,
                    origin.Y,
                    None,
                    &mut count,
                );
                if count > 0 {
                    let mut hits = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
                    let mut actual: u32 = 0;
                    if text_layout
                        .HitTestTextRange(
                            s as u32,
                            (e - s) as u32,
                            origin.X,
                            origin.Y,
                            Some(&mut hits),
                            &mut actual,
                        )
                        .is_ok()
                    {
                        let hlt_brush = context
                            .render_target
                            .CreateSolidColorBrush(&sys_color_to_d2d(COLOR_HIGHLIGHTTEXT), None)?;
                        for h in &hits[..actual as usize] {
                            context.render_target.PushAxisAlignedClip(
                                &D2D_RECT_F {
                                    left: h.left,
                                    top: h.top,
                                    right: h.left + h.width,
                                    bottom: h.top + h.height,
                                },
                                D2D1_ANTIALIAS_MODE_ALIASED,
                            );
                            context.render_target.DrawTextLayout(
                                origin,
                                &text_layout,
                                &hlt_brush,
                                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                            );
                            context.render_target.PopAxisAlignedClip();
                        }
                    }
                }
            }
        }

        // Caret (hidden while a selection is active, like Input).
        if context.is_focused && context.caret_visible && context.caret == context.sel_anchor {
            let (cx, cy, ch) = caret_point(context, context.caret);
            let caret_top = content.top + cy - offset;
            let caret_bottom = caret_top + ch;
            let x = content.left + cx;
            let caret_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
            context.render_target.FillRectangle(
                &D2D_RECT_F {
                    left: x,
                    top: caret_top,
                    right: x + 1.0,
                    bottom: caret_bottom,
                },
                &caret_brush,
            );
        }

        context.render_target.PopAxisAlignedClip();

        // Scrollbar (rail at rest, expanded bar on hover).
        context.scroll.paint(
            &context.render_target,
            context.track_rect(),
            tokens,
        )?;

        // --- chrome on top (border, accent, focus underline) ---
        if let input::Appearance::Outline = state.appearance {
            let border_color = if context.is_focused {
                &tokens.color_neutral_stroke1_pressed
            } else if context.is_hovered {
                &tokens.color_neutral_stroke1_hover
            } else {
                &tokens.color_neutral_stroke1
            };
            let border_brush = context.render_target.CreateSolidColorBrush(border_color, None)?;
            context.render_target.DrawRoundedRectangle(
                &field_rect,
                &border_brush,
                stroke,
                &state.qt.stroke_style,
            );

            let accent_color = if context.is_hovered && !context.is_focused {
                &tokens.color_neutral_stroke_accessible_hover
            } else {
                &tokens.color_neutral_stroke_accessible
            };
            let accent_brush = context.render_target.CreateSolidColorBrush(accent_color, None)?;
            let accent_geometry =
                bottom_accent_geometry(&state.qt.d2d_factory, width, radius, height - stroke * 0.5)?;
            context.render_target.DrawGeometry(
                &accent_geometry,
                &accent_brush,
                stroke,
                &state.qt.stroke_style,
            );
        }

        if context.is_focused {
            let percentage = context.bottom_focus_border.GetValue()? as f32;
            let left = width * (1.0 - percentage) / 2.0;
            let underline_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_compound_brand_stroke, None)?;
            context.render_target.FillRectangle(
                &D2D_RECT_F {
                    left,
                    top: height - 2.0,
                    right: left + width * percentage,
                    bottom: height,
                },
                &underline_brush,
            );
        }
    }
    Ok(())
}

fn on_paint(window: HWND, context: &Context) -> Result<()> {
    unsafe {
        context.render_target.BeginDraw();
        let result = paint(window, context);
        match result {
            Ok(_) => context.render_target.EndDraw(None, None),
            Err(_) => {
                context.render_target.EndDraw(None, None)?;
                result
            }
        }
    }
}

/// Convert a client-pixel mouse point to content-space DIP coordinates.
fn mouse_to_content(context: &Context, window: HWND, lx: i32, ly: i32) -> (f32, f32) {
    let scaling_factor = get_scaling_factor(window);
    let x = lx as f32 / scaling_factor;
    let y = ly as f32 / scaling_factor;
    let content = context.content_rect();
    let cx = (x - content.left).max(0.0);
    let cy = y - content.top + context.scroll.offset();
    (cx, cy)
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
                Ok(mut context) => {
                    _ = layout(window, &context);
                    update_metrics(&mut context);
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if !raw.is_null() {
                drop(Box::<Context>::from_raw(raw));
            }
            LRESULT(0)
        },
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = on_paint(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = on_paint(window, &*raw);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_TIMER if w_param.0 == CARET_TIMER_ID => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.caret_visible = !context.caret_visible;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_TIMER if w_param.0 == REPEAT_TIMER_ID => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if context.scroll.is_repeating() {
                SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INTERVAL_MS, None);
                if context.scroll.repeat_step() {
                    _ = InvalidateRect(Some(window), None, false);
                }
            } else {
                _ = KillTimer(Some(window), REPEAT_TIMER_ID);
            }
            LRESULT(0)
        },
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_focused = true;
            context.caret_visible = true;
            _ = start_focus_animation(context);
            reset_blink(window);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_focused = false;
            context.caret_visible = false;
            _ = KillTimer(Some(window), CARET_TIMER_ID);
            context.bottom_focus_border =
                match context.animation_manager.CreateAnimationVariable(0.0) {
                    Ok(v) => v,
                    Err(_) => context.bottom_focus_border.clone(),
                };
            _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE);
            LRESULT(0)
        },
        WM_SETCURSOR => unsafe {
            // Arrow cursor over the scrollbar region; the class default (I-beam)
            // everywhere else. Only when there's actually a scrollbar.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context;
            if !raw.is_null() && (*raw).scroll.visible() {
                let mut pt = POINT::default();
                _ = GetCursorPos(&mut pt);
                _ = ScreenToClient(window, &mut pt);
                let scaling_factor = get_scaling_factor(window);
                let x = pt.x as f32 / scaling_factor;
                let y = pt.y as f32 / scaling_factor;
                let t = (*raw).track_rect();
                if x >= t.left && x <= t.right && y >= t.top && y <= t.bottom {
                    SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                    return LRESULT(1);
                }
            }
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if !context.is_hovered {
                context.is_hovered = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                _ = TrackMouseEvent(&mut tme);
                _ = InvalidateRect(Some(window), None, false);
            }
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            // Expand the rail into the full bar only while over the scrollbar region
            // (not the whole component). It stays expanded during drag/arrow-press
            // via VScroll's own state.
            let t = context.track_rect();
            let over_track = px >= t.left && px <= t.right && py >= t.top && py <= t.bottom;
            if context.scroll.set_expanded(over_track) {
                _ = InvalidateRect(Some(window), None, false);
            }
            if context.scroll.is_dragging() {
                let (_, redraw) = context.scroll.on_mouse_move(px, py, context.track_rect());
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
            } else if context.dragging_text {
                let (cx, cy) = mouse_to_content(context, window, l_param.0 as i16 as i32, (l_param.0 >> 16) as i16 as i32);
                let cp = point_to_caret(context, cx, cy);
                move_caret(window, context, cp, true);
            } else {
                let (_, redraw) = context.scroll.on_mouse_move(px, py, context.track_rect());
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_hovered = false;
            _ = context.scroll.clear_hover();
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let (handled, redraw) = match context
                .scroll
                .on_l_button_down(px, py, context.track_rect())
            {
                ScrollHit::Miss => (false, false),
                ScrollHit::Thumb => (true, true),
                ScrollHit::Track | ScrollHit::Up | ScrollHit::Down => {
                    // Auto-repeat while the arrow / track is held (like SpinButton).
                    SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INITIAL_MS, None);
                    (true, true)
                }
            };
            if handled {
                SetCapture(window);
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
            } else {
                let (cx, cy) =
                    mouse_to_content(context, window, l_param.0 as i16 as i32, (l_param.0 >> 16) as i16 as i32);
                let cp = point_to_caret(context, cx, cy);
                let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                context.dragging_text = true;
                SetCapture(window);
                move_caret(window, context, cp, shift);
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let redraw = context.scroll.on_l_button_up();
            context.dragging_text = false;
            _ = KillTimer(Some(window), REPEAT_TIMER_ID);
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            if redraw {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONDBLCLK => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            // Ignore double-clicks on the scrollbar; only word-select over text.
            if context.scroll.on_l_button_down(px, py, context.track_rect()) == ScrollHit::Miss {
                let (cx, cy) = mouse_to_content(
                    context,
                    window,
                    l_param.0 as i16 as i32,
                    (l_param.0 >> 16) as i16 as i32,
                );
                let cp = point_to_caret(context, cx, cy);
                select_word_at(window, context, cp);
            }
            LRESULT(0)
        },
        WM_CONTEXTMENU => unsafe {
            _ = SetFocus(Some(window));
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let (s, e) = context.selection();
            let has_selection = s != e;
            let has_text = !context.buffer.is_empty();
            let qt = context.state.qt.clone();
            let can_paste = IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).is_ok();

            let mut x = l_param.0 as i16 as i32;
            let mut y = (l_param.0 >> 16) as i16 as i32;
            if x == -1 && y == -1 {
                // Keyboard-invoked (Shift+F10): anchor at the control's bottom-left.
                let mut rc = RECT::default();
                if GetWindowRect(window, &mut rc).is_ok() {
                    x = rc.left;
                    y = rc.bottom;
                }
            }

            let menu_list = vec![
                MenuInfo::MenuItem {
                    text: system_string(IDS_CUT, w!("Cut")),
                    command_id: CMD_CUT,
                    disabled: !has_selection,
                    secondary_text: Some(w!("Ctrl+X")),
                },
                MenuInfo::MenuItem {
                    text: system_string(IDS_COPY, w!("Copy")),
                    command_id: CMD_COPY,
                    disabled: !has_selection,
                    secondary_text: Some(w!("Ctrl+C")),
                },
                MenuInfo::MenuItem {
                    text: system_string(IDS_PASTE, w!("Paste")),
                    command_id: CMD_PASTE,
                    disabled: !can_paste,
                    secondary_text: Some(w!("Ctrl+V")),
                },
                MenuInfo::MenuItem {
                    text: system_string(IDS_SELECT_ALL, w!("Select All")),
                    command_id: CMD_SELECT_ALL,
                    disabled: !has_text,
                    secondary_text: Some(w!("Ctrl+A")),
                },
            ];
            _ = qt.open_menu(window, x, y, crate::component::menu::Props { menu_list });
            LRESULT::default()
        },
        WM_COMMAND => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            match (w_param.0 & 0xffff) as u32 {
                CMD_CUT => {
                    _ = copy_selection(window, context);
                    delete_selection_or(window, context, false);
                }
                CMD_COPY => _ = copy_selection(window, context),
                CMD_PASTE => _ = paste_clipboard(window, context),
                CMD_SELECT_ALL => {
                    context.sel_anchor = 0;
                    context.caret = context.buffer.len();
                    _ = InvalidateRect(Some(window), None, false);
                }
                _ => {}
            }
            LRESULT::default()
        },
        WM_MOUSEWHEEL => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let delta = (w_param.0 >> 16) as i16 as i32;
            if context.scroll.on_wheel(delta) {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS | DLGC_WANTALLKEYS) as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
            let control = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_LEFT => {
                    let to = context.caret.saturating_sub(1);
                    move_caret(window, context, to, shift);
                }
                VK_RIGHT => {
                    let to = context.caret + 1;
                    move_caret(window, context, to, shift);
                }
                VK_UP => move_vertical(window, context, -1.0, shift),
                VK_DOWN => move_vertical(window, context, 1.0, shift),
                VK_HOME => {
                    let to = if control { 0 } else { line_home(context, context.caret) };
                    move_caret(window, context, to, shift);
                }
                VK_END => {
                    let to = if control {
                        context.buffer.len()
                    } else {
                        line_end(context, context.caret)
                    };
                    move_caret(window, context, to, shift);
                }
                VK_PRIOR => {
                    let lines = page_lines(context);
                    move_vertical_by(window, context, -lines, shift);
                }
                VK_NEXT => {
                    let lines = page_lines(context);
                    move_vertical_by(window, context, lines, shift);
                }
                VK_DELETE => delete_selection_or(window, context, true),
                VK_A if control => {
                    context.sel_anchor = 0;
                    context.caret = context.buffer.len();
                    _ = InvalidateRect(Some(window), None, false);
                }
                _ => return DefWindowProcW(window, message, w_param, l_param),
            }
            LRESULT(0)
        },
        WM_CHAR => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let ch = w_param.0 as u16;
            match ch {
                0x08 => delete_selection_or(window, context, false), // backspace
                0x03 => _ = copy_selection(window, context),         // ^C
                0x16 => _ = paste_clipboard(window, context),        // ^V
                0x18 => {
                    // ^X
                    _ = copy_selection(window, context);
                    delete_selection_or(window, context, false);
                }
                0x01 => {} // ^A handled in WM_KEYDOWN
                0x0d | 0x0a => insert_text(window, context, &[b'\n' as u16]), // Enter → newline
                _ => {
                    if ch >= b' ' as u16 && ch != 127 {
                        insert_text(window, context, &[ch]);
                    }
                }
            }
            LRESULT(0)
        },
        WM_SETTEXT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let text = PCWSTR(l_param.0 as *const u16);
            context.buffer = text.as_wide().to_vec();
            context.caret = 0;
            context.sel_anchor = 0;
            after_edit(window, context);
            LRESULT(TRUE.0 as isize)
        },
        WM_GETTEXTLENGTH => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            LRESULT((*raw).buffer.len() as isize)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            update_metrics(context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}

/// Move by N visual lines (fractional page), keeping the x column.
fn move_vertical_by(window: HWND, context: &mut Context, lines: f32, extend: bool) {
    let (x, y, h) = caret_point(context, context.caret);
    let target_y = (y + lines * h + h * 0.5).max(0.0);
    let to = point_to_caret(context, x, target_y);
    move_caret(window, context, to, extend);
}
