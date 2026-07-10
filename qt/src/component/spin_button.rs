//! An editable numeric field with up/down steppers — Win32 UpDown + buddy edit,
//! Fluent-styled. Type a number (validated) or nudge it with the steppers / arrows;
//! min/max/step with optional wrap. The editable field is modeled on `combobox`'s
//! self-contained editor (minus the popup); the steppers are drawn in a right gutter.

use std::mem::size_of;
use std::sync::Once;

use crate::component::input;
use crate::icon::Icon;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_SVG_PAINT_TYPE_COLOR, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1DeviceContext5,
    ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1PathGeometry1, ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_HIT_TEST_METRICS,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
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
    TrackMouseEvent, VIRTUAL_KEY, VK_A, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

const FIELD_CLASS: PCWSTR = w!("QT_SPIN_BUTTON");
const CARET_TIMER_ID: usize = 1;
const REPEAT_TIMER_ID: usize = 2;
/// Right-gutter width (DIPs): the 24px-wide stepper button column.
const STEPPER_W: f32 = 24.0;
/// The 16×16 chevron renders at native size, centered horizontally in the 24px
/// button, and nudged toward the center line (matching Fluent's asymmetric
/// paddingTop/Bottom of 4/1 and 1/4 on the increment/decrement buttons).
const GLYPH_W: f32 = 16.0;
const GLYPH_H: f32 = 16.0;
const GLYPH_NUDGE: f32 = 1.5;
const REPEAT_INITIAL_MS: u32 = 500;
const REPEAT_INTERVAL_MS: u32 = 60;

#[derive(Copy, Clone, PartialEq, Eq)]
enum Stepper {
    None,
    Up,
    Down,
}

pub struct MouseEvent {
    /// Fired on each committed value change (step, arrow, or field commit).
    pub on_change: Box<dyn Fn(&HWND, f64)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_change: Box::new(|_window, _value| {}),
        }
    }
}

pub struct Props {
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// Decimal places shown / accepted (0 = integer).
    pub precision: u32,
    /// UDS_WRAP: stepping past an end wraps to the other end (else clamps).
    pub wrap: bool,
    pub size: input::Size,
    pub appearance: input::Appearance,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            value: 0.0,
            min: 0.0,
            max: 100.0,
            step: 1.0,
            precision: 0,
            wrap: false,
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
    up_svg: ID2D1SvgDocument,
    down_svg: ID2D1SvgDocument,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    bottom_focus_border: IUIAnimationVariable2,
    is_focused: bool,
    is_hovered: bool,
    // editor
    buffer: Vec<u16>,
    caret: usize,
    sel_anchor: usize,
    x_offset: f32,
    caret_visible: bool,
    // steppers
    value: f64,
    hovered_stepper: Stepper,
    pressed_stepper: Stepper,
}

impl Context {
    fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.sel_anchor), self.caret.max(self.sel_anchor))
    }
    fn text_width_dip(&self) -> f32 {
        let pad = self.state.horizontal_padding();
        field_width(&self.state) - pad - STEPPER_W
    }
}

impl QT {
    pub fn create_spin_button(
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
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_IBEAM).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&field_class);
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

    /// The last committed numeric value.
    pub fn spin_button_value(&self, spin_button: HWND) -> f64 {
        unsafe {
            let raw = GetWindowLongPtrW(spin_button, GWLP_USERDATA) as *const Context;
            if raw.is_null() { 0.0 } else { (*raw).value }
        }
    }
}

// --- shared helpers (cloned from combobox) ---

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

fn make_svg(rt: &ID2D1HwndRenderTarget, icon: &Icon, color: &D2D1_COLOR_F) -> Result<ID2D1SvgDocument> {
    unsafe {
        let device_context5 = rt.cast::<ID2D1DeviceContext5>()?;
        let svg_stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
        let svg = device_context5.CreateSvgDocument(
            svg_stream.as_ref(),
            D2D_SIZE_F {
                width: icon.size as f32,
                height: icon.size as f32,
            },
        )?;
        _ = set_svg_color(&svg, color);
        Ok(svg)
    }
}

// --- numeric value logic ---

fn format_value(v: f64, precision: u32) -> Vec<u16> {
    format!("{:.*}", precision as usize, v).encode_utf16().collect()
}

/// A string that could grow into a valid number: optional leading `-`, digits, at
/// most one `.` (only when precision allows). Empty / "-" / "1." are valid partials.
fn is_valid_partial(s: &str, precision: u32) -> bool {
    let mut chars = s.chars().peekable();
    if chars.peek() == Some(&'-') {
        chars.next();
    }
    let mut seen_dot = false;
    for c in chars {
        match c {
            '0'..='9' => {}
            '.' if precision > 0 && !seen_dot => seen_dot = true,
            _ => return false,
        }
    }
    true
}

fn prospective(context: &Context, chars: &[u16]) -> Vec<u16> {
    let (s, e) = context.selection();
    let mut v = context.buffer[..s].to_vec();
    v.extend_from_slice(chars);
    v.extend_from_slice(&context.buffer[e..]);
    v
}

fn parse_buffer(context: &Context) -> f64 {
    String::from_utf16_lossy(&context.buffer)
        .trim()
        .parse::<f64>()
        .unwrap_or(context.value)
}

fn clamp(state: &State, v: f64) -> f64 {
    v.max(state.props.min).min(state.props.max)
}

/// Reset the buffer to display `value`, caret at end.
fn set_buffer_to_value(context: &mut Context) {
    context.buffer = format_value(context.value, context.state.props.precision);
    context.caret = context.buffer.len();
    context.sel_anchor = context.caret;
    context.x_offset = 0.0;
}

/// Parse the typed text, clamp, reformat, fire on_change if the value changed.
fn commit(window: HWND, context: &mut Context) {
    let clamped = clamp(&context.state, parse_buffer(context));
    let changed = clamped != context.value;
    context.value = clamped;
    set_buffer_to_value(context);
    scroll_caret(context);
    if changed {
        (context.state.props.mouse_event.on_change)(&window, context.value);
    }
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

/// Step by `delta` from the current (typed) value; wrap or clamp per props.
fn step_by(window: HWND, context: &mut Context, delta: f64) {
    let props = &context.state.props;
    let base = clamp(&context.state, parse_buffer(context));
    let mut v = base + delta;
    if props.wrap {
        if v > props.max {
            v = props.min;
        } else if v < props.min {
            v = props.max;
        }
    } else {
        v = clamp(&context.state, v);
    }
    let changed = v != context.value;
    context.value = v;
    set_buffer_to_value(context);
    scroll_caret(context);
    if changed {
        (context.state.props.mouse_event.on_change)(&window, context.value);
    }
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

// --- editor (cloned from combobox, popup stripped) ---

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

fn reset_blink(window: HWND) {
    unsafe {
        _ = SetTimer(Some(window), CARET_TIMER_ID, GetCaretBlinkTime(), None);
    }
}

fn after_edit(window: HWND, context: &mut Context) {
    scroll_caret(context);
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
                    if ch >= b' ' as u16 {
                        chars.push(ch);
                    }
                    i += 1;
                }
                _ = GlobalUnlock(HGLOBAL(hsrc.0 as _));
            }
        }
        CloseClipboard()?;
        // Only paste if the result stays a valid partial number.
        if !chars.is_empty() {
            let candidate = prospective(context, &chars);
            if is_valid_partial(
                &String::from_utf16_lossy(&candidate),
                context.state.props.precision,
            ) {
                insert_text(window, context, &chars);
            }
        }
    }
    Ok(())
}

// --- layout / geometry ---

fn field_width(state: &State) -> f32 {
    let widest = unsafe {
        let Ok(format) = create_text_format(&state.qt, state.font_size()) else {
            return 120.0;
        };
        let measure = |s: &[u16]| -> f32 {
            let Ok(layout) = state
                .qt
                .dwrite_factory
                .CreateTextLayout(s, &format, f32::MAX, f32::MAX)
            else {
                return 0.0;
            };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() {
                m.width.ceil()
            } else {
                0.0
            }
        };
        let min_s = format_value(state.props.min, state.props.precision);
        let max_s = format_value(state.props.max, state.props.precision);
        measure(&min_s).max(measure(&max_s)).max(40.0)
    };
    let pad = state.horizontal_padding();
    // No right padding: the stepper gutter sits flush to the right edge.
    pad + widest + state.qt.theme.tokens.spacing_horizontal_s + STEPPER_W
}

/// Gutter x-range (DIPs) for the stepper column — flush to the right edge, inset by
/// the border stroke so the fill doesn't overpaint the border.
fn gutter_x(state: &State, width: f32) -> (f32, f32) {
    let stroke = state.qt.theme.tokens.stroke_width_thin;
    (width - STEPPER_W, width - stroke)
}

/// Which stepper (if any) a client point (DIP) is over.
fn stepper_at(state: &State, x: f32, y: f32, width: f32, height: f32) -> Stepper {
    let (gl, gr) = gutter_x(state, width);
    if x < gl || x > gr {
        return Stepper::None;
    }
    if y < height / 2.0 {
        Stepper::Up
    } else {
        Stepper::Down
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
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

        let up_svg = make_svg(
            &render_target,
            &Icon::chevron_up_16_regular(),
            &tokens.color_neutral_stroke_accessible,
        )?;
        let down_svg = make_svg(
            &render_target,
            &Icon::chevron_down_16_regular(),
            &tokens.color_neutral_stroke_accessible,
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

        let value = clamp(&state, state.props.value);
        let buffer = format_value(value, state.props.precision);
        let caret = buffer.len();

        Ok(Context {
            state,
            text_format,
            render_target,
            up_svg,
            down_svg,
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
            value,
            hovered_stepper: Stepper::None,
            pressed_stepper: Stepper::None,
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

        {
            let (gl, gr) = gutter_x(state, width);
            let mid = height / 2.0;
            let up_disabled = !state.props.wrap && context.value >= state.props.max;
            let down_disabled = !state.props.wrap && context.value <= state.props.min;
            for (stepper, top, bot, disabled) in [
                (Stepper::Up, 0.0f32, mid, up_disabled),
                (Stepper::Down, mid, height, down_disabled),
            ] {
                let hl = if !disabled && context.pressed_stepper == stepper {
                    Some(tokens.color_subtle_background_pressed)
                } else if !disabled && context.hovered_stepper == stepper {
                    Some(tokens.color_subtle_background_hover)
                } else {
                    None
                };
                if let Some(color) = hl {
                    let brush = context.render_target.CreateSolidColorBrush(&color, None)?;
                    context.render_target.FillRectangle(
                        &D2D_RECT_F {
                            left: gl,
                            top,
                            right: gr,
                            bottom: bot,
                        },
                        &brush,
                    );
                }
            }
        }

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

        // Text column: [pad, width - pad - STEPPER_W].
        let col_left = pad;
        let col_right = width - pad - STEPPER_W;
        let text_rect = D2D_RECT_F {
            left: col_left,
            top: 0.0,
            right: col_right,
            bottom: height,
        };

        let base_x = col_left - context.x_offset;
        let draw_rect = D2D_RECT_F {
            left: base_x,
            top: 0.0,
            right: base_x + 100000.0,
            bottom: height,
        };
        context.render_target.PushAxisAlignedClip(&text_rect, D2D1_ANTIALIAS_MODE_ALIASED);

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

        // Steppers.
        let (gl, _gr) = gutter_x(state, width);
        let mid = height / 2.0;
        let up_disabled = !state.props.wrap && context.value >= state.props.max;
        let down_disabled = !state.props.wrap && context.value <= state.props.min;
        let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        let scale_x = GLYPH_W / 16.0;
        let scale_y = GLYPH_H / 16.0;
        let gx = gl + (STEPPER_W - GLYPH_W) / 2.0;

        for (stepper, svg, disabled, gy) in [
            (Stepper::Up, &context.up_svg, up_disabled, GLYPH_NUDGE),
            (Stepper::Down, &context.down_svg, down_disabled, mid - GLYPH_NUDGE),
        ] {
            let color = if disabled {
                &tokens.color_neutral_foreground_disabled
            } else if context.pressed_stepper == stepper {
                &tokens.color_neutral_foreground3_pressed
            } else if context.hovered_stepper == stepper {
                &tokens.color_neutral_foreground3_hover
            } else {
                &tokens.color_neutral_foreground3
            };
            _ = set_svg_color(svg, color);
            device_context5.SetTransform(&Matrix3x2 {
                M11: scale_x,
                M12: 0.0,
                M21: 0.0,
                M22: scale_y,
                M31: gx,
                M32: gy,
            });
            device_context5.DrawSvgDocument(svg);
        }
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

/// True if `stepper` is at its bound with wrap off (can't step further).
fn stepper_disabled(context: &Context, stepper: Stepper) -> bool {
    let props = &context.state.props;
    if props.wrap {
        return false;
    }
    match stepper {
        Stepper::Up => context.value >= props.max,
        Stepper::Down => context.value <= props.min,
        Stepper::None => true,
    }
}

/// Begin a stepper hold: step once, capture the mouse, arm the auto-repeat.
fn begin_step(window: HWND, context: &mut Context, stepper: Stepper) {
    if stepper == Stepper::None || stepper_disabled(context, stepper) {
        return;
    }
    let step = context.state.props.step;
    context.pressed_stepper = stepper;
    let delta = match stepper {
        Stepper::Up => step,
        Stepper::Down => -step,
        Stepper::None => return,
    };
    unsafe {
        SetCapture(window);
        step_by(window, context, delta);
        _ = SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INITIAL_MS, None);
    }
}

fn end_step(window: HWND, context: &mut Context) {
    if context.pressed_stepper != Stepper::None {
        context.pressed_stepper = Stepper::None;
        unsafe {
            _ = KillTimer(Some(window), REPEAT_TIMER_ID);
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            _ = InvalidateRect(Some(window), None, false);
        }
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
            _ = Box::<Context>::from_raw(raw);
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
        WM_SETCURSOR => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context;
            if !raw.is_null() {
                let mut pt = POINT::default();
                _ = GetCursorPos(&mut pt);
                _ = ScreenToClient(window, &mut pt);
                let scaling_factor = get_scaling_factor(window);
                let x = pt.x as f32 / scaling_factor;
                let y = pt.y as f32 / scaling_factor;
                let state = &(*raw).state;
                if stepper_at(state, x, y, field_width(state), state.field_height())
                    != Stepper::None
                {
                    SetCursor(LoadCursorW(None, IDC_ARROW).ok());
                    return LRESULT(1);
                }
            }
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
            let step = context.state.props.step;
            let delta = match context.pressed_stepper {
                Stepper::Up => step,
                Stepper::Down => -step,
                Stepper::None => {
                    _ = KillTimer(Some(window), REPEAT_TIMER_ID);
                    return LRESULT(0);
                }
            };
            // Re-arm at the faster repeat cadence after the initial delay.
            _ = SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INTERVAL_MS, None);
            step_by(window, context, delta);
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
            commit(window, context);
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
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let y = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let width = field_width(&context.state);
            let height = context.state.field_height();
            let new_stepper = stepper_at(&context.state, x, y, width, height);
            let mut changed = false;
            if !context.is_hovered {
                context.is_hovered = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                _ = TrackMouseEvent(&mut tme);
                changed = true;
            }
            if context.hovered_stepper != new_stepper {
                context.hovered_stepper = new_stepper;
                changed = true;
            }
            if changed {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_hovered = false;
            context.hovered_stepper = Stepper::None;
            end_step(window, context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let y = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let width = field_width(&context.state);
            let height = context.state.field_height();
            match stepper_at(&context.state, x, y, width, height) {
                Stepper::None => {
                    let pad = context.state.horizontal_padding();
                    let rel = x - pad + context.x_offset;
                    let cp = x_to_caret(context, rel.max(0.0));
                    move_caret(window, context, cp, false);
                }
                s => begin_step(window, context, s),
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            end_step(window, &mut *raw);
            LRESULT(0)
        },
        WM_CAPTURECHANGED => unsafe {
            // Capture yanked (focus stolen, etc.) without a button-up → clear the
            // pressed stepper so it can't get stuck.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if !raw.is_null() {
                (*raw).pressed_stepper = Stepper::None;
                _ = KillTimer(Some(window), REPEAT_TIMER_ID);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
            let control = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
            let step = context.state.props.step;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_UP => step_by(window, context, step),
                VK_DOWN => step_by(window, context, -step),
                VK_PRIOR => step_by(window, context, step * 10.0),
                VK_NEXT => step_by(window, context, -step * 10.0),
                VK_RETURN => commit(window, context),
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
                        // Numeric filter: only accept if the result stays a valid
                        // partial number.
                        let candidate = prospective(context, &[ch]);
                        if is_valid_partial(
                            &String::from_utf16_lossy(&candidate),
                            context.state.props.precision,
                        ) {
                            insert_text(window, context, &[ch]);
                        }
                    }
                }
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
