use std::mem::size_of;
use std::sync::Once;

use crate::component::input;
use crate::component::option::Item;
use crate::icon::Icon;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    ID2D1DeviceContext5, ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1PathGeometry1,
    ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_HIT_TEST_METRICS,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, CreateRoundRectRgn, EndPaint, GetMonitorInfoW,
    GetSysColor, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    PAINTSTRUCT, RDW_INVALIDATE, RedrawWindow, SYS_COLOR_INDEX, SetWindowRgn, UpdateWindow,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
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
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VIRTUAL_KEY, VK_A, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F4, VK_HOME, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

const FIELD_CLASS: PCWSTR = w!("QT_COMBOBOX");
const POPUP_CLASS: PCWSTR = w!("QT_COMBOBOX_POPUP");
const ITEM_HEIGHT: f32 = 32.0;
const LIST_PADDING: f32 = 4.0;
const CARET_TIMER_ID: usize = 1;
/// Popup posts this to the field when a row is clicked (wParam = original index).
const WM_APP_COMBO_COMMIT: u32 = WM_APP + 7;
/// Popup posts this to the field to request dismissal (click-outside / capture lost).
const WM_APP_COMBO_CLOSE: u32 = WM_APP + 8;

pub struct MouseEvent {
    /// Fired when a suggestion is committed (click or Enter on a highlight),
    /// carrying the item's *original* index. Typed-only commits keep the text and
    /// don't fire — read it via `combobox_value`.
    pub on_select: Box<dyn Fn(&HWND, usize)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_select: Box::new(|_window, _index| {}),
        }
    }
}

pub struct Props {
    /// Suggestions. The caller keeps the strings alive (label contract).
    pub options: Vec<Item>,
    pub placeholder: PCWSTR,
    /// Initial field text.
    pub default_value: Option<PCWSTR>,
    pub size: input::Size,
    pub appearance: input::Appearance,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            options: Vec::new(),
            placeholder: w!(""),
            default_value: None,
            size: input::Size::Medium,
            appearance: input::Appearance::Outline,
            mouse_event: MouseEvent::default(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    fn field_height(&self) -> f32 {
        match self.props.size {
            input::Size::Small => 24.0,
            input::Size::Medium => 32.0,
            input::Size::Large => 40.0,
        }
    }
    fn horizontal_padding(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.props.size {
            input::Size::Small => tokens.spacing_horizontal_s,
            input::Size::Medium => tokens.spacing_horizontal_m,
            input::Size::Large => tokens.spacing_horizontal_m + tokens.spacing_horizontal_s_nudge,
        }
    }
    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base300
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    chevron_svg: ID2D1SvgDocument,
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
    x_offset: f32,
    caret_visible: bool,
    // --- popup coordination ---
    popup: Option<HWND>,
    /// Original indices of the options currently shown (substring-filtered).
    filtered: Vec<usize>,
    /// Highlighted row *within* `filtered`, or None.
    hovered: Option<usize>,
}

impl Context {
    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.sel_anchor), self.caret.max(self.sel_anchor))
    }
    /// Visible text-column width in DIPs (field width minus paddings + chevron).
    fn text_width_dip(&self) -> f32 {
        let pad = self.state.horizontal_padding();
        field_width(&self.state) - pad - pad - 20.0
    }
}

impl QT {
    pub fn create_combobox(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let field_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: FIELD_CLASS,
                    style: CS_CLASSDC,
                    lpfnWndProc: Some(field_proc),
                    hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&field_class);
                let popup_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: POPUP_CLASS,
                    style: CS_DROPSHADOW | CS_SAVEBITS,
                    lpfnWndProc: Some(popup_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&popup_class);
            });
            let boxed = Box::new(State {
                qt: self.clone(),
                props,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                FIELD_CLASS,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                0,
                0,
                Some(parent_window),
                None,
                Some(HINSTANCE(
                    GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _
                )),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }

    /// The combobox's current text (the value — it's what the user typed / picked).
    pub fn combobox_value(&self, combobox: HWND) -> String {
        unsafe {
            let raw = GetWindowLongPtrW(combobox, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                String::new()
            } else {
                String::from_utf16_lossy(&(*raw).buffer)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (cloned from dropdown)
// ---------------------------------------------------------------------------

fn set_svg_color(svg: &ID2D1SvgDocument, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let svg_paint = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(color), w!(""))?;
        svg.GetRoot()?
            .GetFirstChild()?
            .SetAttributeValue(w!("fill"), &svg_paint.cast::<ID2D1SvgAttribute>()?)?;
    }
    Ok(())
}

fn create_text_format(qt: &QT, font_size: f32) -> Result<IDWriteTextFormat> {
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
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
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

/// Field auto-size width (DIPs): widest option/placeholder + paddings + chevron.
fn field_width(state: &State) -> f32 {
    let mut widest = 0.0f32;
    unsafe {
        let measure = |text: PCWSTR| -> f32 {
            let Ok(layout) = state.qt.dwrite_factory.CreateTextLayout(
                text.as_wide(),
                // recreate a transient format at the field font size
                &create_text_format(&state.qt, state.font_size()).unwrap(),
                f32::MAX,
                f32::MAX,
            ) else {
                return 0.0;
            };
            let mut m = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() {
                m.width.ceil()
            } else {
                0.0
            }
        };
        for option in &state.props.options {
            widest = widest.max(measure(option.text));
        }
        widest = widest.max(measure(state.props.placeholder));
    }
    let pad = state.horizontal_padding();
    pad + widest + state.qt.theme.tokens.spacing_horizontal_s + 20.0 + pad
}

// ---------------------------------------------------------------------------
// Field: chrome + editor
// ---------------------------------------------------------------------------

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    let default_value = state.props.default_value;
    unsafe {
        let text_format = create_text_format(&state.qt, state.font_size())?;
        let dpi = GetDpiForWindow(window);
        let render_target = state.qt.d2d_factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U {
                    width: 0,
                    height: 0,
                },
                presentOptions: Default::default(),
            },
        )?;

        let icon = Icon::chevron_down_20_regular();
        let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
        let svg_stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
        let chevron_svg = device_context5.CreateSvgDocument(
            svg_stream.as_ref(),
            D2D_SIZE_F {
                width: icon.size as f32,
                height: icon.size as f32,
            },
        )?;
        _ = set_svg_color(&chevron_svg, &tokens.color_neutral_stroke_accessible);

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

        let buffer: Vec<u16> = match default_value {
            Some(v) => v.as_wide().to_vec(),
            None => Vec::new(),
        };
        let caret = buffer.len();

        Ok(Context {
            state,
            text_format,
            render_target,
            chevron_svg,
            animation_manager,
            animation_timer,
            transition_library,
            bottom_focus_border,
            is_focused: false,
            is_hovered: false,
            buffer,
            caret,
            sel_anchor: caret,
            x_offset: 0.0,
            caret_visible: false,
            popup: None,
            filtered: Vec::new(),
            hovered: None,
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
    let width = field_width(state);
    let height = state.field_height();
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (width * scaling_factor).ceil() as i32;
    let scaled_height = (height * scaling_factor).ceil() as i32;
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
            Vector2 {
                X: left_cx - d,
                Y: corner_cy + d,
            },
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
            point: Vector2 {
                X: right_cx + d,
                Y: corner_cy + d,
            },
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

/// Pixel-x (DIP, relative to text start, before scroll) of a caret position.
fn caret_to_x(context: &Context, cp: usize) -> f32 {
    unsafe {
        let Ok(layout) = context.state.qt.dwrite_factory.CreateTextLayout(
            &context.buffer,
            &context.text_format,
            f32::MAX,
            f32::MAX,
        ) else {
            return 0.0;
        };
        let mut x = 0f32;
        let mut y = 0f32;
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if layout
            .HitTestTextPosition(cp as u32, false, &mut x, &mut y, &mut m)
            .is_ok()
        {
            x
        } else {
            0.0
        }
    }
}

/// Caret position nearest a text-relative x (DIP, before scroll).
fn x_to_caret(context: &Context, x: f32) -> usize {
    unsafe {
        let Ok(layout) = context.state.qt.dwrite_factory.CreateTextLayout(
            &context.buffer,
            &context.text_format,
            f32::MAX,
            f32::MAX,
        ) else {
            return 0;
        };
        let mut is_trailing = FALSE;
        let mut is_inside = FALSE;
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        if layout
            .HitTestPoint(x, 0.0, &mut is_trailing, &mut is_inside, &mut m)
            .is_ok()
        {
            (m.textPosition + if is_trailing.as_bool() { 1 } else { 0 }) as usize
        } else {
            0
        }
    }
}

/// Keep the caret within the visible text column by adjusting `x_offset`.
fn scroll_caret(context: &mut Context) {
    let view = context.text_width_dip().max(1.0);
    let cx = caret_to_x(context, context.caret);
    if cx - context.x_offset > view {
        context.x_offset = cx - view;
    }
    if cx < context.x_offset {
        context.x_offset = cx;
    }
    if context.x_offset < 0.0 {
        context.x_offset = 0.0;
    }
}

/// After any buffer mutation: refilter, update the popup, rescroll, repaint.
fn after_edit(window: HWND, context: &mut Context) {
    refilter(context);
    open_or_update_popup(window, context);
    scroll_caret(context);
    context.caret_visible = true;
    reset_blink(window);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

fn reset_blink(window: HWND) {
    unsafe {
        _ = SetTimer(Some(window), CARET_TIMER_ID, GetCaretBlinkTime(), None);
    }
}

fn refilter(context: &mut Context) {
    let text = String::from_utf16_lossy(&context.buffer).to_lowercase();
    context.filtered = context
        .state
        .props
        .options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            text.is_empty()
                || String::from_utf16_lossy(unsafe { o.text.as_wide() })
                    .to_lowercase()
                    .contains(&text)
        })
        .map(|(i, _)| i)
        .collect();
    context.hovered = None;
}

// Enabled-row navigation over the *filtered* list.
fn opt(context: &Context, row: usize) -> &Item {
    &context.state.props.options[context.filtered[row]]
}
fn last_enabled_row(context: &Context) -> Option<usize> {
    (0..context.filtered.len()).rev().find(|&r| !opt(context, r).disabled)
}
fn next_enabled_row(context: &Context, from: Option<usize>) -> Option<usize> {
    let start = match from {
        Some(r) => r + 1,
        None => 0,
    };
    (start..context.filtered.len())
        .find(|&r| !opt(context, r).disabled)
        .or(from)
}
fn prev_enabled_row(context: &Context, from: Option<usize>) -> Option<usize> {
    match from {
        Some(r) => (0..r).rev().find(|&j| !opt(context, j).disabled).or(from),
        None => last_enabled_row(context),
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
    scroll_caret(context);
    context.caret_visible = true;
    reset_blink(window);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
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
                    // Skip control chars (newlines etc.) — single-line field.
                    if ch >= b' ' as u16 {
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
            .props
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
        let pad = state.horizontal_padding();

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
        let fill_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_background1, None)?;
        context
            .render_target
            .FillRoundedRectangle(&field_rect, &fill_brush);

        if let input::Appearance::Outline = state.props.appearance {
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
        }

        // Bottom accent + focus underline.
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

        // Text column: [pad, width - pad - 20]. Text drawn at base_x - x_offset,
        // clipped to the column so scrolled-off text doesn't spill.
        let col_left = pad;
        let col_right = width - pad - 20.0;
        let text_rect = D2D_RECT_F {
            left: col_left,
            top: 0.0,
            right: col_right,
            bottom: height,
        };

        if context.buffer.is_empty() {
            // Placeholder.
            if !state.props.placeholder.is_null() && !state.props.placeholder.as_wide().is_empty() {
                let ph_brush = context
                    .render_target
                    .CreateSolidColorBrush(&tokens.color_neutral_foreground3, None)?;
                context.render_target.DrawText(
                    state.props.placeholder.as_wide(),
                    &context.text_format,
                    &text_rect,
                    &ph_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        } else {
            let base_x = col_left - context.x_offset;
            let draw_rect = D2D_RECT_F {
                left: base_x,
                top: 0.0,
                right: base_x + 100000.0,
                bottom: height,
            };
            context.render_target.PushAxisAlignedClip(&text_rect, D2D1_ANTIALIAS_MODE_ALIASED);

            // Selection highlight.
            let (s, e) = context.selection();
            if s != e {
                let sx = caret_to_x(context, s);
                let ex = caret_to_x(context, e);
                let hl_brush = context
                    .render_target
                    .CreateSolidColorBrush(&sys_color_to_d2d(COLOR_HIGHLIGHT), None)?;
                context.render_target.FillRectangle(
                    &D2D_RECT_F {
                        left: base_x + sx,
                        top: 2.0,
                        right: base_x + ex,
                        bottom: height - 2.0,
                    },
                    &hl_brush,
                );
            }

            // All text in foreground1.
            let text_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
            context.render_target.DrawText(
                &context.buffer,
                &context.text_format,
                &draw_rect,
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );

            // Selected text in highlight-text color (clipped to the selection band).
            if s != e {
                let sx = caret_to_x(context, s);
                let ex = caret_to_x(context, e);
                context.render_target.PushAxisAlignedClip(
                    &D2D_RECT_F {
                        left: base_x + sx,
                        top: 0.0,
                        right: base_x + ex,
                        bottom: height,
                    },
                    D2D1_ANTIALIAS_MODE_ALIASED,
                );
                let hlt_brush = context
                    .render_target
                    .CreateSolidColorBrush(&sys_color_to_d2d(COLOR_HIGHLIGHTTEXT), None)?;
                context.render_target.DrawText(
                    &context.buffer,
                    &context.text_format,
                    &draw_rect,
                    &hlt_brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                context.render_target.PopAxisAlignedClip();
            }

            context.render_target.PopAxisAlignedClip();
        }

        // Caret.
        if context.is_focused && context.caret_visible {
            let cx = col_left + caret_to_x(context, context.caret) - context.x_offset;
            if cx >= col_left - 0.5 && cx <= col_right + 0.5 {
                let caret_brush = context
                    .render_target
                    .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
                context.render_target.FillRectangle(
                    &D2D_RECT_F {
                        left: cx,
                        top: 4.0,
                        right: cx + 1.0,
                        bottom: height - 4.0,
                    },
                    &caret_brush,
                );
            }
        }

        // Chevron.
        let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        let chevron_x = width - pad - 20.0;
        let chevron_y = (height - 20.0) / 2.0;
        device_context5.SetTransform(&Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: chevron_x,
            M32: chevron_y,
        });
        device_context5.DrawSvgDocument(&context.chevron_svg);
        device_context5.SetTransform(&Matrix3x2::identity());
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

/// Commit a *filtered row*: set the field to the option text, close, fire on_select.
fn commit_row(window: HWND, context: &mut Context, row: usize) {
    if row >= context.filtered.len() {
        return;
    }
    let original = context.filtered[row];
    let text = unsafe { context.state.props.options[original].text.as_wide().to_vec() };
    context.buffer = text;
    context.caret = context.buffer.len();
    context.sel_anchor = context.caret;
    context.x_offset = 0.0;
    close_popup(context);
    (context.state.props.mouse_event.on_select)(&window, original);
    scroll_caret(context);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

fn close_popup(context: &mut Context) {
    if let Some(p) = context.popup.take() {
        unsafe {
            _ = ReleaseCapture();
            _ = DestroyWindow(p);
        }
    }
    context.hovered = None;
}

/// Open the popup for the current `filtered` list, or update/close it.
fn open_or_update_popup(window: HWND, context: &mut Context) {
    if context.filtered.is_empty() {
        close_popup(context);
        return;
    }
    unsafe {
        let mut field_rect = RECT::default();
        if GetWindowRect(window, &mut field_rect).is_err() {
            return;
        }
        let scaling_factor = get_scaling_factor(window);
        let width_px = field_rect.right - field_rect.left;
        let height_px = ((ITEM_HEIGHT * context.filtered.len() as f32 + LIST_PADDING * 2.0)
            * scaling_factor)
            .ceil() as i32;
        let monitor = MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        _ = GetMonitorInfoW(monitor, &mut info);
        let x = field_rect.left;
        let mut y = field_rect.bottom;
        if y + height_px > info.rcWork.bottom {
            y = (field_rect.top - height_px).max(info.rcWork.top);
        }

        match context.popup {
            Some(popup) => {
                // Resize/reposition + repaint.
                let corner =
                    (context.state.qt.theme.tokens.border_radius_medium * 2.0 * scaling_factor)
                        as i32;
                let region = CreateRoundRectRgn(0, 0, width_px + 1, height_px + 1, corner, corner);
                SetWindowRgn(popup, Some(region), false);
                _ = SetWindowPos(
                    popup,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    width_px,
                    height_px,
                    SWP_NOACTIVATE,
                );
                let raw = GetWindowLongPtrW(popup, GWLP_USERDATA) as *mut PopupContext;
                if !raw.is_null() {
                    _ = (*raw).render_target.Resize(&D2D_SIZE_U {
                        width: width_px as u32,
                        height: height_px as u32,
                    });
                    _ = InvalidateRect(Some(popup), None, false);
                }
            }
            None => {
                let width_dip = width_px as f32 / scaling_factor;
                let params = Box::new(PopupParams {
                    qt: context.state.qt.clone(),
                    field: window,
                    width_dip,
                });
                let Ok(popup) = CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                    POPUP_CLASS,
                    w!(""),
                    WS_POPUP,
                    x,
                    y,
                    width_px,
                    height_px,
                    Some(window),
                    None,
                    Some(HINSTANCE(GetWindowLongPtrW(window, GWLP_HINSTANCE) as _)),
                    Some(Box::<PopupParams>::into_raw(params) as _),
                ) else {
                    return;
                };
                let corner =
                    (context.state.qt.theme.tokens.border_radius_medium * 2.0 * scaling_factor)
                        as i32;
                let region = CreateRoundRectRgn(0, 0, width_px + 1, height_px + 1, corner, corner);
                SetWindowRgn(popup, Some(region), false);
                _ = SetWindowPos(
                    popup,
                    Some(HWND_TOPMOST),
                    x,
                    y,
                    width_px,
                    height_px,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                );
                let raw = GetWindowLongPtrW(popup, GWLP_USERDATA) as *mut PopupContext;
                if !raw.is_null() {
                    let popup_dpi = GetDpiForWindow(popup);
                    (*raw).render_target.SetDpi(popup_dpi as f32, popup_dpi as f32);
                    _ = (*raw).render_target.Resize(&D2D_SIZE_U {
                        width: width_px as u32,
                        height: height_px as u32,
                    });
                    _ = UpdateWindow(popup);
                }
                // Mouse-only capture (keyboard still goes to the focused field, so
                // live typing/filtering is unaffected) so a click anywhere —
                // including the same window's canvas — is seen by the popup and can
                // dismiss it. This is dropdown's click-outside mechanism minus the
                // blocking modal loop.
                SetCapture(popup);
                context.popup = Some(popup);
            }
        }
    }
}

extern "system" fn field_proc(
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
                    _ = layout(window, &context);
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let mut ctx = Box::<Context>::from_raw(raw);
            close_popup(&mut ctx);
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
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = true;
            (*raw).caret_visible = true;
            _ = start_focus_animation(&mut *raw);
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
            close_popup(context);
            context.bottom_focus_border =
                match context.animation_manager.CreateAnimationVariable(0.0) {
                    Ok(v) => v,
                    Err(_) => context.bottom_focus_border.clone(),
                };
            _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE);
            LRESULT(0)
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
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_hovered = false;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let width = field_width(&context.state);
            let pad = context.state.horizontal_padding();
            if x >= width - pad - 20.0 {
                // Chevron zone → toggle the full list.
                if context.popup.is_some() {
                    close_popup(context);
                } else {
                    refilter(context);
                    open_or_update_popup(window, context);
                }
                _ = InvalidateRect(Some(window), None, false);
            } else {
                // Text zone → place caret.
                let rel = x - pad + context.x_offset;
                let cp = x_to_caret(context, rel.max(0.0));
                move_caret(window, context, cp, false);
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
            let control = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_DOWN => {
                    if context.popup.is_none() {
                        refilter(context);
                        open_or_update_popup(window, context);
                    }
                    context.hovered = next_enabled_row(context, context.hovered);
                    if let Some(p) = context.popup {
                        _ = InvalidateRect(Some(p), None, false);
                    }
                }
                VK_UP => {
                    if context.popup.is_some() {
                        context.hovered = prev_enabled_row(context, context.hovered);
                        if let Some(p) = context.popup {
                            _ = InvalidateRect(Some(p), None, false);
                        }
                    }
                }
                VK_RETURN => {
                    if let Some(row) = context.hovered {
                        commit_row(window, context, row);
                    } else {
                        close_popup(context);
                        _ = InvalidateRect(Some(window), None, false);
                    }
                }
                VK_ESCAPE => {
                    close_popup(context);
                    _ = InvalidateRect(Some(window), None, false);
                }
                VK_F4 => {
                    if context.popup.is_some() {
                        close_popup(context);
                    } else {
                        refilter(context);
                        open_or_update_popup(window, context);
                    }
                    _ = InvalidateRect(Some(window), None, false);
                }
                VK_LEFT => {
                    let to = context.caret.saturating_sub(1);
                    move_caret(window, context, to, shift);
                }
                VK_RIGHT => {
                    let to = context.caret + 1;
                    move_caret(window, context, to, shift);
                }
                VK_HOME => move_caret(window, context, 0, shift),
                VK_END => {
                    let to = context.buffer.len();
                    move_caret(window, context, to, shift);
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
                _ => {
                    if ch >= b' ' as u16 && ch != 127 {
                        insert_text(window, context, &[ch]);
                    }
                }
            }
            LRESULT(0)
        },
        WM_APP_COMBO_COMMIT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            commit_row(window, &mut *raw, w_param.0);
            LRESULT(0)
        },
        WM_APP_COMBO_CLOSE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if !raw.is_null() {
                close_popup(&mut *raw);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}

// ---------------------------------------------------------------------------
// Popup (modeless): reads the field's Context for filtered/hovered state
// ---------------------------------------------------------------------------

struct PopupParams {
    qt: QT,
    field: HWND,
    width_dip: f32,
}

struct PopupContext {
    qt: QT,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    field: HWND,
    width_dip: f32,
}

fn popup_on_create(window: HWND, params: PopupParams) -> Result<PopupContext> {
    let tokens = &params.qt.theme.tokens;
    unsafe {
        let text_format = create_text_format(&params.qt, tokens.font_size_base300)?;
        let dpi = GetDpiForWindow(window);
        let render_target = params.qt.d2d_factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U {
                    width: 0,
                    height: 0,
                },
                presentOptions: Default::default(),
            },
        )?;
        Ok(PopupContext {
            qt: params.qt,
            render_target,
            text_format,
            field: params.field,
            width_dip: params.width_dip,
        })
    }
}

fn popup_paint(window: HWND, context: &PopupContext) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    unsafe {
        context
            .render_target
            .Clear(Some(&tokens.color_neutral_background1));
        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let width = context.width_dip;

        // Read the field's filtered list + hovered row.
        let field_raw = GetWindowLongPtrW(context.field, GWLP_USERDATA) as *const Context;
        if field_raw.is_null() {
            return Ok(());
        }
        let field = &*field_raw;

        let item_left = LIST_PADDING;
        let item_right = width - LIST_PADDING;
        let item_pad = tokens.spacing_horizontal_s;

        for (row, &original) in field.filtered.iter().enumerate() {
            let option = &field.state.props.options[original];
            let top = LIST_PADDING + row as f32 * ITEM_HEIGHT;
            if field.hovered == Some(row) && !option.disabled {
                let hover_brush = context
                    .render_target
                    .CreateSolidColorBrush(&tokens.color_neutral_background1_hover, None)?;
                let rounded = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: item_left,
                        top,
                        right: item_right,
                        bottom: top + ITEM_HEIGHT,
                    },
                    radiusX: tokens.border_radius_medium,
                    radiusY: tokens.border_radius_medium,
                };
                context
                    .render_target
                    .FillRoundedRectangle(&rounded, &hover_brush);
            }
            let text_color = if option.disabled {
                &tokens.color_neutral_foreground_disabled
            } else {
                &tokens.color_neutral_foreground1
            };
            let text_brush = context.render_target.CreateSolidColorBrush(text_color, None)?;
            context.render_target.DrawText(
                option.text.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: item_left + item_pad,
                    top,
                    right: item_right - item_pad,
                    bottom: top + ITEM_HEIGHT,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
    Ok(())
}

fn popup_on_paint(window: HWND, context: &PopupContext) -> Result<()> {
    unsafe {
        context.render_target.BeginDraw();
        let result = popup_paint(window, context);
        match result {
            Ok(_) => context.render_target.EndDraw(None, None),
            Err(_) => {
                context.render_target.EndDraw(None, None)?;
                result
            }
        }
    }
}

/// Hit-test popup client px → filtered-row index (enabled rows only).
fn popup_hit_test(context: &PopupContext, x: i32, y: i32, scaling_factor: f32) -> Option<usize> {
    let x_dip = x as f32 / scaling_factor;
    let y_dip = y as f32 / scaling_factor - LIST_PADDING;
    if x_dip < 0.0 || x_dip > context.width_dip || y_dip < 0.0 {
        return None;
    }
    let row = (y_dip / ITEM_HEIGHT) as usize;
    unsafe {
        let field_raw = GetWindowLongPtrW(context.field, GWLP_USERDATA) as *const Context;
        if field_raw.is_null() {
            return None;
        }
        let field = &*field_raw;
        match field.filtered.get(row) {
            Some(&original) if !field.state.props.options[original].disabled => Some(row),
            _ => None,
        }
    }
}

/// True if a captured mouse point (popup-relative device px) is inside the list.
fn in_popup_client(window: HWND, x: i32, y: i32) -> bool {
    let mut rc = RECT::default();
    unsafe {
        if GetClientRect(window, &mut rc).is_err() {
            return false;
        }
    }
    x >= 0 && x < rc.right && y >= 0 && y < rc.bottom
}

extern "system" fn popup_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut PopupParams;
            let params = Box::<PopupParams>::from_raw(raw);
            match popup_on_create(window, *params) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(
                        window,
                        GWLP_USERDATA,
                        Box::<PopupContext>::into_raw(boxed) as _,
                    );
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            if !raw.is_null() {
                _ = Box::<PopupContext>::from_raw(raw);
            }
            LRESULT(0)
        },
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            let context = &*raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = popup_on_paint(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            let context = &*raw;
            let x = l_param.0 as i16 as i32;
            let y = (l_param.0 >> 16) as i16 as i32;
            let hit = popup_hit_test(context, x, y, get_scaling_factor(window));
            let field_raw = GetWindowLongPtrW(context.field, GWLP_USERDATA) as *mut Context;
            if !field_raw.is_null() && (*field_raw).hovered != hit {
                (*field_raw).hovered = hit;
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            let context = &*raw;
            let x = l_param.0 as i16 as i32;
            let y = (l_param.0 >> 16) as i16 as i32;
            if let Some(row) = popup_hit_test(context, x, y, get_scaling_factor(window)) {
                _ = PostMessageW(
                    Some(context.field),
                    WM_APP_COMBO_COMMIT,
                    WPARAM(row),
                    LPARAM(0),
                );
            }
            LRESULT(0)
        },
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => unsafe {
            // With capture, a press outside the list (canvas, another control, the
            // field's own chevron) lands here as an out-of-client point → dismiss.
            let x = l_param.0 as i16 as i32;
            let y = (l_param.0 >> 16) as i16 as i32;
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            if !in_popup_client(window, x, y) && !raw.is_null() {
                _ = PostMessageW(Some((*raw).field), WM_APP_COMBO_CLOSE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        },
        WM_CAPTURECHANGED => unsafe {
            // Capture was stolen (Alt-Tab, another SetCapture) → dismiss too.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut PopupContext;
            if !raw.is_null() {
                _ = PostMessageW(Some((*raw).field), WM_APP_COMBO_CLOSE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
