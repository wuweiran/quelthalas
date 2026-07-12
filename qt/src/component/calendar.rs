//! A month calendar — Win32 `SysMonthCal32`, Fluent-styled (Fluent React's
//! `Calendar`). A self-painting child HWND with two views:
//!   * **Day view** — a title + up/down nav arrows, a weekday header, and a 6×7 grid
//!     of day cells. Today is a brand-filled circle; the selected day a subtle
//!     rounded square; adjacent-month days are muted.
//!   * **Month view** — reached by clicking the title; a 4×3 grid of months with
//!     up/down year nav. Picking a month drops back to the day view.
//! A "Go to today" link sits in the footer.
//!
//! Structurally this clones `data_grid`'s host wiring (window class, State/Context
//! split, `ID2D1HwndRenderTarget`, DPI, rounded-frame `SetWindowRgn`) and swaps the
//! row painter for the calendar grid. Cells are internal hit-tested regions, not
//! child HWNDs. Date math is computed locally (leap year, days-in-month, Sakamoto's
//! day-of-week); "today" comes from `GetLocalTime`.

use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR, ID2D1DeviceContext5, ID2D1HwndRenderTarget,
    ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_LINE_SPACING_METHOD_DEFAULT,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, RDW_INVALIDATE, RedrawWindow,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VIRTUAL_KEY, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

use crate::icon::Icon;
use crate::{QT, get_scaling_factor};

/// A calendar day. `month` is 1-12, `day` 1-31.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Date {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

// --- layout constants (DIPs) ---
const HEADER_H: f32 = 40.0;
const WEEKDAY_H: f32 = 28.0;
const FOOTER_H: f32 = 36.0;
const DAY_ROWS: usize = 6;
const MONTH_ROWS: usize = 3;
const MONTH_COLS: usize = 4;
/// Header nav-arrow button (square, DIPs).
const ARROW_BOX: f32 = 28.0;
/// Title hover-box height (DIPs) — same line height as the arrow boxes.
const TITLE_H: f32 = 28.0;
/// Left padding of the title text inside its box (DIPs).
const TITLE_PAD: f32 = 10.0;
/// Nav-arrow glyph draw size (DIPs) — 20px art rescaled to 12, like data_grid's
/// header sort arrows.
const ARROW_ICON: f32 = 12.0;
/// Day marker box (selected / hover square), 2px padding inside its cell.
const DAY_BOX: f32 = 24.0;
const DAY_PAD: f32 = 2.0;
/// Full day cell = box + padding on both sides (28×28).
const DAY_CELL: f32 = DAY_BOX + DAY_PAD * 2.0;
/// Today circle diameter (DIPs).
const TODAY_CIRCLE: f32 = 20.0;
/// Month marker box and the horizontal gap between month boxes.
const MONTH_BOX: f32 = 40.0;
const MONTH_GAP: f32 = 12.0;
/// Padding around the whole component (DIPs).
const PAD: f32 = 12.0;

// --- English labels (localization deferred; the demo shows the picked date) ---
const WEEKDAYS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];

#[derive(Copy, Clone, PartialEq, Eq)]
enum View {
    Day,
    Month,
}

/// What the pointer is over (drives hover fills + the click action).
#[derive(Copy, Clone, PartialEq, Eq)]
enum Hot {
    None,
    Prev,
    Next,
    Title,
    Today,
    Day(usize),   // 0..42 grid cell
    Month(usize), // 0..12
}

// --- date math (no external crate) ---
fn is_leap(y: i32) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn days_in_month(y: i32, m: i32) -> i32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Day of week of `y-m-d` (0 = Sunday), via Sakamoto's algorithm.
fn day_of_week(y: i32, m: i32, d: i32) -> i32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d).rem_euclid(7)
}

/// Step `(year, month)` (1-based month) by `delta` months, wrapping years.
fn add_months(year: i32, month: i32, delta: i32) -> (i32, i32) {
    let zero = (year * 12 + (month - 1)) + delta;
    (zero.div_euclid(12), zero.rem_euclid(12) + 1)
}

/// Add `delta` days to a date, normalizing across month/year boundaries.
fn add_days(mut y: i32, mut m: i32, mut d: i32, delta: i32) -> (i32, i32, i32) {
    d += delta;
    while d < 1 {
        let (py, pm) = add_months(y, m, -1);
        y = py;
        m = pm;
        d += days_in_month(y, m);
    }
    loop {
        let dim = days_in_month(y, m);
        if d <= dim {
            break;
        }
        d -= dim;
        let (ny, nm) = add_months(y, m, 1);
        y = ny;
        m = nm;
    }
    (y, m, d)
}

pub struct MouseEvent {
    /// Fired when a day is picked (click or Enter).
    pub on_select_date: Box<dyn Fn(&HWND, Date)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_select_date: Box::new(|_, _| {}),
        }
    }
}

pub struct Props {
    pub selected: Option<Date>,
    /// Fixed width (DIPs). `0` = natural.
    pub width: i32,
    /// Fixed height (DIPs). `0` = natural.
    pub height: i32,
    pub background: Option<D2D1_COLOR_F>,
    pub mouse_event: MouseEvent,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            selected: None,
            width: 0,
            height: 0,
            background: None,
            mouse_event: MouseEvent::default(),
        }
    }
}

struct State {
    qt: QT,
    width: f32,
    height: f32,
    background: Option<D2D1_COLOR_F>,
    on_select_date: Box<dyn Fn(&HWND, Date)>,
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    day_format: IDWriteTextFormat,     // centered
    weekday_format: IDWriteTextFormat, // centered, secondary
    title_format: IDWriteTextFormat,   // leading, semibold-ish larger
    footer_format: IDWriteTextFormat,  // trailing (right-aligned link)
    arrow_up_svg: Option<ID2D1SvgDocument>,
    arrow_down_svg: Option<ID2D1SvgDocument>,
    today: Date,
    selected: Option<Date>,
    view: View,
    visible_year: i32,
    visible_month: i32, // 1-12
    /// Keyboard focus: a full date in Day view; only `.month` matters in Month view.
    focus: Date,
    hot: Hot,
    pressed: Hot,
    is_hovered: bool,
}

impl Context {
    fn content_rect(&self) -> D2D_RECT_F {
        // 12px padding around the whole component.
        D2D_RECT_F {
            left: PAD,
            top: PAD,
            right: self.state.width - PAD,
            bottom: self.state.height - PAD,
        }
    }

    fn header_rect(&self) -> D2D_RECT_F {
        let c = self.content_rect();
        D2D_RECT_F { bottom: c.top + HEADER_H, ..c }
    }

    /// The two nav-arrow boxes (prev = up, next = down), right-aligned in the header.
    fn arrow_rects(&self) -> (D2D_RECT_F, D2D_RECT_F) {
        let h = self.header_rect();
        let cy = (h.top + h.bottom) / 2.0;
        let top = cy - ARROW_BOX / 2.0;
        let bottom = cy + ARROW_BOX / 2.0;
        let next_right = h.right;
        let next = D2D_RECT_F { left: next_right - ARROW_BOX, top, right: next_right, bottom };
        // The two arrows abut (no gap between them).
        let prev = D2D_RECT_F { left: next.left - ARROW_BOX, top, right: next.left, bottom };
        (prev, next)
    }

    /// "Go to today" is a no-op (disabled) when the Day view already shows today's
    /// month — there's nowhere to jump to.
    fn today_disabled(&self) -> bool {
        self.view == View::Day
            && self.visible_year == self.today.year as i32
            && self.visible_month == self.today.month as i32
    }

    /// The title string for the current view ("July 2026" / "2026").
    fn title_text(&self) -> String {        match self.view {
            View::Day => format!("{} {}", MONTHS_LONG[(self.visible_month - 1) as usize], self.visible_year),
            View::Month => format!("{}", self.visible_year),
        }
    }

    /// The title hover box (left of the arrows): a 28-tall rounded box spanning from
    /// the content left edge to 4px before the arrows, vertically centered.
    fn title_rect(&self) -> D2D_RECT_F {
        let h = self.header_rect();
        let cy = (h.top + h.bottom) / 2.0;
        let (prev, _) = self.arrow_rects();
        D2D_RECT_F {
            left: h.left,
            top: cy - TITLE_H / 2.0,
            right: prev.left - self.state.qt.theme.tokens.spacing_horizontal_xs,
            bottom: cy + TITLE_H / 2.0,
        }
    }

    fn footer_rect(&self) -> D2D_RECT_F {
        let c = self.content_rect();
        D2D_RECT_F { top: c.bottom - FOOTER_H, ..c }
    }

    fn weekday_rect(&self) -> D2D_RECT_F {
        let c = self.content_rect();
        let top = c.top + HEADER_H;
        D2D_RECT_F { top, bottom: top + WEEKDAY_H, ..c }
    }

    /// Rect of day cell `i` (0..42) — a fixed 28×28 cell (24px box + 2px padding),
    /// left-aligned under the weekday header.
    fn day_cell_rect(&self, i: usize) -> D2D_RECT_F {
        let c = self.content_rect();
        let (row, col) = (i / 7, i % 7);
        let left = c.left + col as f32 * DAY_CELL;
        let top = c.top + HEADER_H + WEEKDAY_H + row as f32 * DAY_CELL;
        D2D_RECT_F { left, top, right: left + DAY_CELL, bottom: top + DAY_CELL }
    }

    /// Rect of month box `i` (0..12) — a fixed 40×40 box with 12px gaps, in a 4×3
    /// grid vertically centered in the area below the header.
    fn month_cell_rect(&self, i: usize) -> D2D_RECT_F {
        let c = self.content_rect();
        let (row, col) = (i / MONTH_COLS, i % MONTH_COLS);
        let stride = MONTH_BOX + MONTH_GAP;
        let grid_h = MONTH_ROWS as f32 * MONTH_BOX + (MONTH_ROWS as f32 - 1.0) * MONTH_GAP;
        let area_top = c.top + HEADER_H;
        let area_bottom = c.bottom - FOOTER_H;
        let top0 = area_top + ((area_bottom - area_top) - grid_h) / 2.0;
        let left = c.left + col as f32 * stride;
        let top = top0 + row as f32 * stride;
        D2D_RECT_F { left, top, right: left + MONTH_BOX, bottom: top + MONTH_BOX }
    }

    /// The date shown in day-grid cell `i` (0..42) and whether it's in the visible month.
    fn cell_date(&self, i: usize) -> (Date, bool) {
        let first_dow = day_of_week(self.visible_year, self.visible_month, 1);
        let offset = i as i32 - first_dow; // day-of-month - 1
        let (y, m, d) = add_days(self.visible_year, self.visible_month, 1, offset);
        let in_month = y == self.visible_year && m == self.visible_month;
        (
            Date { year: y as u16, month: m as u8, day: d as u8 },
            in_month,
        )
    }

    fn hit_test(&self, x: f32, y: f32) -> Hot {
        let inside = |r: &D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        let (prev, next) = self.arrow_rects();
        if inside(&prev) {
            return Hot::Prev;
        }
        if inside(&next) {
            return Hot::Next;
        }
        if inside(&self.title_rect()) {
            return Hot::Title;
        }
        // Footer "Go to today" occupies the right portion; whole footer row is fine.
        if inside(&self.footer_rect()) {
            return Hot::Today;
        }
        match self.view {
            View::Day => {
                for i in 0..DAY_ROWS * 7 {
                    if inside(&self.day_cell_rect(i)) {
                        return Hot::Day(i);
                    }
                }
            }
            View::Month => {
                for i in 0..12 {
                    if inside(&self.month_cell_rect(i)) {
                        return Hot::Month(i);
                    }
                }
            }
        }
        Hot::None
    }
}

impl QT {
    pub fn create_calendar(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_CALENDAR");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_CLASSDC,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let scaling_factor = get_scaling_factor(parent_window);
            // Natural size: 12px padding + 7 fixed day cells; header + weekday + 6 rows + footer.
            let natural_w = PAD * 2.0 + 7.0 * DAY_CELL;
            let natural_h = PAD * 2.0 + HEADER_H + WEEKDAY_H + DAY_ROWS as f32 * DAY_CELL + FOOTER_H;
            let width = if props.width > 0 { props.width as f32 } else { natural_w };
            let height = if props.height > 0 { props.height as f32 } else { natural_h };
            let boxed = Box::new(State {
                qt: self.clone(),
                width,
                height,
                background: props.background,
                on_select_date: props.mouse_event.on_select_date,
            });
            // The initial selection rides alongside State in the create tuple.
            let selected = props.selected;
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
                Some(Box::<(State, Option<Date>)>::into_raw(Box::new((*boxed, selected))) as _),
            )?;
            Ok(hwnd)
        }
    }

    /// The currently selected date, if any.
    pub fn calendar_selection(&self, calendar: HWND) -> Option<Date> {
        unsafe {
            let raw = GetWindowLongPtrW(calendar, GWLP_USERDATA) as *const Context;
            if raw.is_null() { None } else { (*raw).selected }
        }
    }
}

fn set_svg_color(svg: &ID2D1SvgDocument, color: &D2D1_COLOR_F) {
    unsafe {
        if let Ok(paint) = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(color), w!("")) {
            if let (Ok(root), Ok(attr)) = (svg.GetRoot(), paint.cast::<ID2D1SvgAttribute>()) {
                if let Ok(child) = root.GetFirstChild() {
                    _ = child.SetAttributeValue(w!("fill"), &attr);
                }
            }
        }
    }
}

fn make_svg(dc5: &ID2D1DeviceContext5, icon: &Icon, color: &D2D1_COLOR_F) -> Option<ID2D1SvgDocument> {
    unsafe {
        let stream = SHCreateMemStream(Some(icon.svg.as_bytes()))?;
        let svg = dc5
            .CreateSvgDocument(
                &stream,
                D2D_SIZE_F { width: icon.size as f32, height: icon.size as f32 },
            )
            .ok()?;
        set_svg_color(&svg, color);
        Some(svg)
    }
}

fn create_format(
    qt: &QT,
    size: f32,
    line_height: Option<f32>,
    align: DWRITE_TEXT_ALIGNMENT,
    semibold: bool,
) -> Result<IDWriteTextFormat> {
    let tokens = &qt.theme.tokens;
    let weight = if semibold { tokens.font_weight_semibold } else { tokens.font_weight_regular };
    unsafe {
        let format = qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetTextAlignment(align)?;
        if let Some(lh) = line_height {
            format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_DEFAULT, lh - size, size)?;
        }
        Ok(format)
    }
}

fn on_create(window: HWND, state: State, selected: Option<Date>) -> Result<Context> {
    unsafe {
        let (size_200, size_300, arrow_color) = {
            let tokens = &state.qt.theme.tokens;
            (
                tokens.font_size_base200,
                tokens.font_size_base300,
                tokens.color_neutral_foreground2,
            )
        };
        // Day numbers: caption size (base200) with a 24px line box (Fluent day cell).
        let day_format = create_format(&state.qt, size_200, Some(24.0), DWRITE_TEXT_ALIGNMENT_CENTER, false)?;
        let weekday_format = create_format(&state.qt, size_200, None, DWRITE_TEXT_ALIGNMENT_CENTER, false)?;
        // Title "July 2026": semibold base300 (matches the mockup).
        let title_format = create_format(&state.qt, size_300, None, DWRITE_TEXT_ALIGNMENT_LEADING, true)?;
        let footer_format = create_format(&state.qt, size_200, None, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?;

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

        let (arrow_up_svg, arrow_down_svg) = match render_target.cast::<ID2D1DeviceContext5>() {
            Ok(dc5) => (
                make_svg(&dc5, &Icon::arrow_up_20_regular(), &arrow_color),
                make_svg(&dc5, &Icon::arrow_down_20_regular(), &arrow_color),
            ),
            Err(_) => (None, None),
        };

        let st = GetLocalTime();
        let today = Date { year: st.wYear, month: st.wMonth as u8, day: st.wDay as u8 };

        // Open on the selected month if given, else today's month.
        let anchor = selected.unwrap_or(today);
        Ok(Context {
            state,
            render_target,
            day_format,
            weekday_format,
            title_format,
            footer_format,
            arrow_up_svg,
            arrow_down_svg,
            today,
            selected,
            view: View::Day,
            visible_year: anchor.year as i32,
            visible_month: anchor.month as i32,
            focus: anchor,
            hot: Hot::None,
            pressed: Hot::None,
            is_hovered: false,
        })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (state.width * scaling_factor).ceil() as i32;
    let scaled_height = (state.height * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(window, None, 0, 0, scaled_width, scaled_height, SWP_NOMOVE | SWP_NOZORDER)?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;
        // No outer frame → no rounded window region (full rectangle).
    }
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

/// Draw an SVG glyph of logical size `size` centered in `slot`, re-tinted to `color`.
fn draw_glyph(context: &Context, svg: &ID2D1SvgDocument, slot: &D2D_RECT_F, size: f32, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let dc5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        set_svg_color(svg, color);
        let vp = svg.GetViewportSize();
        let scale = size / vp.width;
        let left = slot.left + ((slot.right - slot.left) - size) / 2.0;
        let top = slot.top + ((slot.bottom - slot.top) - size) / 2.0;
        dc5.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: left, M32: top });
        dc5.DrawSvgDocument(svg);
        dc5.SetTransform(&Matrix3x2::identity());
    }
    Ok(())
}

fn draw_text(context: &Context, text: &[u16], format: &IDWriteTextFormat, rect: &D2D_RECT_F, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let brush = context.render_target.CreateSolidColorBrush(color, None)?;
        context.render_target.DrawText(text, format, rect, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
    }
    Ok(())
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        // No outer frame: paint straight onto the page background (canvas).
        let background = state.background.unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;

        // --- header: title + nav arrows ---
        // Shared hover/pressed styling for the title and arrow buttons.
        let state_colors = |target: Hot, rest_fg: D2D1_COLOR_F| -> (Option<D2D1_COLOR_F>, D2D1_COLOR_F) {
            if context.pressed == target && context.hot == target {
                (Some(tokens.color_brand_background_inverted_pressed), tokens.color_brand_foreground_on_light_pressed)
            } else if context.hot == target {
                (Some(tokens.color_brand_background_inverted_hover), tokens.color_brand_foreground_on_light_hover)
            } else {
                (None, rest_fg)
            }
        };
        let radius = tokens.border_radius_medium;

        let title = context.title_text();
        let title_rect = context.title_rect();
        let (title_bg, title_fg) = state_colors(Hot::Title, tokens.color_neutral_foreground1);
        if let Some(bg) = title_bg {
            let brush = context.render_target.CreateSolidColorBrush(&bg, None)?;
            let rr = D2D1_ROUNDED_RECT { rect: title_rect, radiusX: radius, radiusY: radius };
            context.render_target.FillRoundedRectangle(&rr, &brush);
        }
        // 10px left padding inside the box.
        draw_text(context, &wide(&title), &context.title_format, &D2D_RECT_F { left: title_rect.left + TITLE_PAD, ..title_rect }, &title_fg)?;

        let (prev, next) = context.arrow_rects();
        for (r, svg, target) in [
            (prev, &context.arrow_up_svg, Hot::Prev),
            (next, &context.arrow_down_svg, Hot::Next),
        ] {
            let (bg, fg) = state_colors(target, tokens.color_neutral_foreground2);
            if let Some(bg) = bg {
                let brush = context.render_target.CreateSolidColorBrush(&bg, None)?;
                let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
                context.render_target.FillRoundedRectangle(&rr, &brush);
            }
            if let Some(svg) = svg {
                draw_glyph(context, svg, &r, ARROW_ICON, &fg)?;
            }
        }

        match context.view {
            View::Day => paint_day_view(context, tokens)?,
            View::Month => paint_month_view(context, tokens)?,
        }

        // --- footer: "Go to today" link ---
        let footer = context.footer_rect();
        let today_color = if context.today_disabled() {
            &tokens.color_neutral_foreground_disabled
        } else if context.hot == Hot::Today {
            &tokens.color_brand_foreground1
        } else {
            &tokens.color_neutral_foreground1
        };
        draw_text(context, &wide("Go to today"), &context.footer_format, &footer, today_color)?;
    }
    Ok(())
}

fn paint_day_view(context: &Context, tokens: &crate::theme::Tokens) -> Result<()> {
    unsafe {
        // Weekday header row — aligned to the fixed day-cell columns.
        let wr = context.weekday_rect();
        for (col, label) in WEEKDAYS.iter().enumerate() {
            let left = wr.left + col as f32 * DAY_CELL;
            let cell = D2D_RECT_F { left, top: wr.top, right: left + DAY_CELL, bottom: wr.bottom };
            draw_text(context, &wide(label), &context.weekday_format, &cell, &tokens.color_neutral_foreground2)?;
        }

        // Day cells.
        for i in 0..DAY_ROWS * 7 {
            let (date, in_month) = context.cell_date(i);
            let rect = context.day_cell_rect(i);
            let is_today = date == context.today;
            let is_selected = context.selected == Some(date);
            let is_hover = context.hot == Hot::Day(i);

            // Marker box (24×24 centered in the 28×28 cell — 2px padding).
            let cx = (rect.left + rect.right) / 2.0;
            let cy = (rect.top + rect.bottom) / 2.0;
            let marker = D2D_RECT_F { left: cx - DAY_BOX / 2.0, top: cy - DAY_BOX / 2.0, right: cx + DAY_BOX / 2.0, bottom: cy + DAY_BOX / 2.0 };
            let radius = tokens.border_radius_medium;

            // Selected: inverted-selected fill + 1px brand stroke. Hover (unselected):
            // inverted-hover fill. Selected + hovered keeps the selected fill.
            if is_selected {
                let brush = context.render_target.CreateSolidColorBrush(&tokens.color_brand_background_inverted_selected, None)?;
                let rr = D2D1_ROUNDED_RECT { rect: marker, radiusX: radius, radiusY: radius };
                context.render_target.FillRoundedRectangle(&rr, &brush);
                let stroke = tokens.stroke_width_thin;
                let border = context.render_target.CreateSolidColorBrush(&tokens.color_brand_stroke1, None)?;
                let inset = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: marker.left + stroke * 0.5,
                        top: marker.top + stroke * 0.5,
                        right: marker.right - stroke * 0.5,
                        bottom: marker.bottom - stroke * 0.5,
                    },
                    radiusX: radius,
                    radiusY: radius,
                };
                context.render_target.DrawRoundedRectangle(&inset, &border, stroke, &context.state.qt.stroke_style);
            } else if is_hover {
                let brush = context.render_target.CreateSolidColorBrush(&tokens.color_brand_background_inverted_hover, None)?;
                let rr = D2D1_ROUNDED_RECT { rect: marker, radiusX: radius, radiusY: radius };
                context.render_target.FillRoundedRectangle(&rr, &brush);
            }
            // Today: brand-filled 20px circle (over any selected/hover fill).
            if is_today {
                let circle = D2D_RECT_F { left: cx - TODAY_CIRCLE / 2.0, top: cy - TODAY_CIRCLE / 2.0, right: cx + TODAY_CIRCLE / 2.0, bottom: cy + TODAY_CIRCLE / 2.0 };
                let brush = context.render_target.CreateSolidColorBrush(&tokens.color_compound_brand_background, None)?;
                let rr = D2D1_ROUNDED_RECT { rect: circle, radiusX: TODAY_CIRCLE / 2.0, radiusY: TODAY_CIRCLE / 2.0 };
                context.render_target.FillRoundedRectangle(&rr, &brush);
            }

            let text_color = if is_today {
                // Today circle: on-brand text.
                tokens.color_neutral_foreground_on_brand
            } else if is_selected || is_hover {
                // Over an inverted-blue fill, text is the static dark foreground so it
                // stays readable in both themes.
                tokens.color_neutral_foreground1_static
            } else if !in_month {
                tokens.color_neutral_foreground3
            } else {
                tokens.color_neutral_foreground1
            };
            draw_text(context, &wide(&date.day.to_string()), &context.day_format, &rect, &text_color)?;
        }
    }
    Ok(())
}

fn paint_month_view(context: &Context, tokens: &crate::theme::Tokens) -> Result<()> {
    unsafe {
        for i in 0..12 {
            let rect = context.month_cell_rect(i);
            let is_hover = context.hot == Hot::Month(i);
            if is_hover {
                // Fill the whole 40×40 month box with the inverted-hover brand color.
                let brush = context.render_target.CreateSolidColorBrush(&tokens.color_brand_background_inverted_hover, None)?;
                let rr = D2D1_ROUNDED_RECT { rect, radiusX: tokens.border_radius_medium, radiusY: tokens.border_radius_medium };
                context.render_target.FillRoundedRectangle(&rr, &brush);
            }
            let color = if is_hover {
                tokens.color_neutral_foreground1_static
            } else {
                tokens.color_neutral_foreground3
            };
            draw_text(context, &wide(MONTHS_SHORT[i]), &context.day_format, &rect, &color)?;
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

fn select_date(window: HWND, context: &mut Context, date: Date) {
    context.selected = Some(date);
    context.focus = date;
    context.visible_year = date.year as i32;
    context.visible_month = date.month as i32;
    (context.state.on_select_date)(&window, date);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

/// Handle a click on whatever the pointer is over.
fn on_click(window: HWND, context: &mut Context, hot: Hot) {
    match hot {
        Hot::Prev | Hot::Next => {
            let delta = if hot == Hot::Prev { -1 } else { 1 };
            match context.view {
                View::Day => {
                    let (y, m) = add_months(context.visible_year, context.visible_month, delta);
                    context.visible_year = y;
                    context.visible_month = m;
                }
                View::Month => context.visible_year += delta,
            }
        }
        Hot::Title => {
            context.view = match context.view {
                View::Day => View::Month,
                View::Month => View::Day,
            };
        }
        Hot::Today => {
            if context.today_disabled() {
                return;
            }
            context.visible_year = context.today.year as i32;
            context.visible_month = context.today.month as i32;
            context.view = View::Day;
        }
        Hot::Day(i) => {
            let (date, _) = context.cell_date(i);
            select_date(window, context, date);
            return;
        }
        Hot::Month(i) => {
            context.visible_month = i as i32 + 1;
            context.view = View::Day;
        }
        Hot::None => return,
    }
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

fn key_down(window: HWND, context: &mut Context, vk: VIRTUAL_KEY) -> bool {
    match context.view {
        View::Day => {
            let f = context.focus;
            let (y, m, d) = (f.year as i32, f.month as i32, f.day as i32);
            let moved = match vk {
                VK_LEFT => Some(add_days(y, m, d, -1)),
                VK_RIGHT => Some(add_days(y, m, d, 1)),
                VK_UP => Some(add_days(y, m, d, -7)),
                VK_DOWN => Some(add_days(y, m, d, 7)),
                VK_PRIOR => {
                    let (ny, nm) = add_months(y, m, -1);
                    let nd = d.min(days_in_month(ny, nm));
                    Some((ny, nm, nd))
                }
                VK_NEXT => {
                    let (ny, nm) = add_months(y, m, 1);
                    let nd = d.min(days_in_month(ny, nm));
                    Some((ny, nm, nd))
                }
                VK_HOME => Some((y, m, 1)),
                VK_END => Some((y, m, days_in_month(y, m))),
                VK_RETURN => {
                    select_date(window, context, context.focus);
                    return true;
                }
                _ => None,
            };
            if let Some((ny, nm, nd)) = moved {
                context.focus = Date { year: ny as u16, month: nm as u8, day: nd as u8 };
                context.visible_year = ny;
                context.visible_month = nm;
                unsafe {
                    _ = InvalidateRect(Some(window), None, false);
                }
                return true;
            }
            false
        }
        View::Month => {
            let mut m = context.focus.month as i32;
            let delta = match vk {
                VK_LEFT => -1,
                VK_RIGHT => 1,
                VK_UP => -(MONTH_COLS as i32),
                VK_DOWN => MONTH_COLS as i32,
                VK_RETURN => {
                    context.visible_month = context.focus.month as i32;
                    context.view = View::Day;
                    unsafe {
                        _ = InvalidateRect(Some(window), None, false);
                    }
                    return true;
                }
                _ => 0,
            };
            if delta != 0 {
                m = (m - 1 + delta).rem_euclid(12) + 1;
                context.focus.month = m as u8;
                unsafe {
                    _ = InvalidateRect(Some(window), None, false);
                }
                return true;
            }
            false
        }
    }
}

extern "system" fn window_proc(window: HWND, message: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    match message {
        WM_CREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut (State, Option<Date>);
            let (state, selected) = *Box::<(State, Option<Date>)>::from_raw(raw);
            match on_create(window, state, selected) {
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
        WM_GETDLGCODE => LRESULT(DLGC_WANTARROWS as isize),
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if !raw.is_null() {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
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
            }
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let hot = context.hit_test(px, py);
            if hot != context.hot {
                context.hot = hot;
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_hovered = false;
            if context.hot != Hot::None {
                context.hot = Hot::None;
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let hot = context.hit_test(px, py);
            // Press-then-release: show the pressed visual, act on button-up.
            context.pressed = hot;
            context.hot = hot;
            SetCapture(window);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            let pressed = context.pressed;
            context.pressed = Hot::None;
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let hot = context.hit_test(px, py);
            context.hot = hot;
            // Only act if released over the same target it was pressed on.
            if pressed != Hot::None && hot == pressed {
                on_click(window, context, pressed);
            } else {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if key_down(window, context, VIRTUAL_KEY(w_param.0 as u16)) {
                LRESULT(0)
            } else {
                DefWindowProcW(window, message, w_param, l_param)
            }
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
