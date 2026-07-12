//! A multi-column grid — Win32 `SysListView32` in **Report/Details** mode, Fluent-
//! styled (Fluent React's `DataGrid`/`Table`). A fixed header row of columns over a
//! scrolling body of rows; each cell is an optional 20px icon + text. An optional
//! leading checkbox column drives selection (click / Shift-range / Space / Ctrl+A);
//! double-click activates a row (Win32 `NM_DBLCLK`).
//!
//! Structurally this clones `list_box`'s host wiring — the shared `scroll::VScroll`
//! embedding, keyboard nav, DPI — and swaps the single-column row painter for a
//! header band + multi-column cells. Cells are internal hit-tested regions (not
//! child HWNDs), like `menu_bar`/`toolbar`.

use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR,
    ID2D1DeviceContext5, ID2D1HwndRenderTarget, ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, PAINTSTRUCT, RDW_INVALIDATE,
    RedrawWindow, SetWindowRgn,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent, VIRTUAL_KEY, VK_A, VK_CONTROL, VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR,
    VK_SHIFT, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

use crate::icon::Icon;
use crate::component::scroll::{SCROLLBAR_W, ScrollHit, VScroll};
use crate::{QT, get_scaling_factor};

const REPEAT_TIMER_ID: usize = 1;
const REPEAT_INITIAL_MS: u32 = 250;
const REPEAT_INTERVAL_MS: u32 = 40;
const SCROLLBAR_MARGIN: f32 = 2.0;
/// Height of the fixed header band (DIPs).
const HEADER_H: f32 = 32.0;
/// Height of a body row's content (DIPs). Each row also has a 1px bottom divider,
/// so the per-row slot is `ROW_H + stroke_width_thin` (≈ 45).
const ROW_H: f32 = 44.0;
/// Width of the leading checkbox column (DIPs), shown in any selecting mode.
const CHECKBOX_COL_W: f32 = 40.0;
/// Checkbox box size (DIPs) — Fluent medium.
const CHECK_BOX: f32 = 16.0;
/// Checkmark glyph size inside the box (DIPs).
const CHECK_GLYPH: f32 = 12.0;
/// Leading icon draw size inside a cell (DIPs).
const CELL_ICON: f32 = 20.0;

/// One column: a header label and a fixed width (DIPs). (Resizable/sortable deferred.)
pub struct Column {
    pub header: PCWSTR,
    pub width: i32,
}

/// One cell: an optional leading 20px icon and text (either may be empty).
#[derive(Copy, Clone)]
pub struct Cell {
    pub icon: Option<Icon>,
    pub text: PCWSTR,
}

impl Cell {
    pub fn text(text: PCWSTR) -> Self {
        Cell { icon: None, text }
    }
    pub fn new(icon: Icon, text: PCWSTR) -> Self {
        Cell { icon: Some(icon), text }
    }
}

/// One row: cells parallel to the grid's columns.
pub struct Row {
    pub cells: Vec<Cell>,
}

/// How rows can be selected (Fluent DataGrid `selectionMode`).
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum SelectionMode {
    /// No selection column, no selection interaction.
    None,
    /// One row at a time (a leading radio-like checkbox column, no select-all).
    Single,
    /// Many rows (leading select-all + per-row checkbox column; Ctrl / Shift / Space).
    Multiselect,
}

/// How a selected row is emphasized (Fluent DataGrid `selectionAppearance`).
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum SelectionAppearance {
    /// No row fill — the checkbox alone shows selection.
    None,
    /// `colorSubtleBackgroundSelected` fill (hover = `colorSubtleBackgroundHover`).
    Neutral,
    /// `colorBrandBackground2` fill (hover = `colorSubtleBackgroundHover`).
    Brand,
}

pub struct MouseEvent {
    /// Fired whenever the selection changes, with the sorted selected row indices.
    pub on_selection_change: Box<dyn Fn(&HWND, &[usize])>,
    /// Fired when a row is double-clicked (Win32 ListView's `NM_DBLCLK` — "activate
    /// / open"), with that row's index.
    pub on_activate: Box<dyn Fn(&HWND, usize)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_selection_change: Box::new(|_, _| {}),
            on_activate: Box::new(|_, _| {}),
        }
    }
}

pub struct Props {
    pub columns: Vec<Column>,
    pub rows: Vec<Row>,
    /// Fixed width (DIPs). `0` = sum of the columns (+ checkbox column + padding).
    pub width: i32,
    /// Fixed height (DIPs). `0` = a default.
    pub height: i32,
    /// Selection behavior. Default `None`.
    pub selection_mode: SelectionMode,
    /// How a selected row is emphasized. Default `Brand`.
    pub selection_appearance: SelectionAppearance,
    pub mouse_event: MouseEvent,
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            columns: Vec::new(),
            rows: Vec::new(),
            width: 0,
            height: 0,
            selection_mode: SelectionMode::None,
            selection_appearance: SelectionAppearance::Brand,
            mouse_event: MouseEvent::default(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    columns: Vec<Column>,
    rows: Vec<Row>,
    width: f32,
    height: f32,
    selection_mode: SelectionMode,
    selection_appearance: SelectionAppearance,
    background: Option<D2D1_COLOR_F>,
    on_selection_change: Box<dyn Fn(&HWND, &[usize])>,
    on_activate: Box<dyn Fn(&HWND, usize)>,
}

impl State {
    /// Whether a leading checkbox column is shown (any selecting mode).
    fn has_checkbox_col(&self) -> bool {
        self.selection_mode != SelectionMode::None
    }
    /// Whether the select-all header checkbox is shown (Multiselect only).
    fn has_select_all(&self) -> bool {
        self.selection_mode == SelectionMode::Multiselect
    }

    /// The 1px divider under each body row (its slot = row + divider, no gap).
    fn row_divider(&self) -> f32 {
        self.qt.theme.tokens.stroke_width_thin
    }
    fn row_slot(&self) -> f32 {
        ROW_H + self.row_divider()
    }
    /// Horizontal padding of a cell's content (both sides).
    fn cell_pad(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_s
    }
}

struct Context {
    state: State,
    cell_format: IDWriteTextFormat,
    header_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    checkmark_svg: ID2D1SvgDocument,
    /// The mixed-state inner square (Fluent `Square12Filled`), re-tinted per draw.
    square_svg: ID2D1SvgDocument,
    /// Per-cell icon SVGs (rows × columns), built once in `on_create`.
    icon_svgs: Vec<Vec<Option<ID2D1SvgDocument>>>,
    /// Per-row selection flag (parallel to rows).
    selected: Vec<bool>,
    /// Keyboard focus row (drawn with a hover fill when not selected).
    focused: Option<usize>,
    /// Anchor for Shift-range selection.
    anchor: Option<usize>,
    /// Body row the pointer is over (subtle hover fill).
    hovered_row: Option<usize>,
    /// Header column the pointer is over (subtle hover fill behind the label).
    hovered_col: Option<usize>,
    /// Row currently held down by the pointer (pressed background/text).
    pressed: Option<usize>,
    is_focused: bool,
    is_hovered: bool,
    scroll: VScroll,
}

impl Context {
    /// The body area (below the header band), edge-to-edge inside the border, DIPs.
    /// No frame padding — rows fill the full width.
    fn body_rect(&self) -> D2D_RECT_F {
        let stroke = self.state.qt.theme.tokens.stroke_width_thin;
        D2D_RECT_F {
            left: stroke,
            top: HEADER_H,
            right: self.state.width - stroke,
            bottom: self.state.height - stroke,
        }
    }

    fn track_rect(&self) -> D2D_RECT_F {
        let stroke = self.state.qt.theme.tokens.stroke_width_thin;
        let right = self.state.width - stroke - SCROLLBAR_MARGIN;
        let body = self.body_rect();
        D2D_RECT_F {
            left: right - SCROLLBAR_W,
            top: body.top,
            right,
            bottom: body.bottom,
        }
    }

    /// The left content edge (the border stroke — no frame padding).
    fn content_left(&self) -> f32 {
        self.state.qt.theme.tokens.stroke_width_thin
    }

    /// Left x (DIPs) of data column `c` (past the checkbox column when present).
    fn col_left(&self, c: usize) -> f32 {
        let mut x = self.content_left();
        if self.state.has_checkbox_col() {
            x += CHECKBOX_COL_W;
        }
        for col in &self.state.columns[..c] {
            x += col.width as f32;
        }
        x
    }

    /// Data column index at a client-DIP x (ignores the checkbox column), or None.
    fn col_at_x(&self, x: f32) -> Option<usize> {
        for c in 0..self.state.columns.len() {
            let cl = self.col_left(c);
            let cr = cl + self.state.columns[c].width as f32;
            if x >= cl && x < cr {
                return Some(c);
            }
        }
        None
    }

    /// The checkbox column's box rect at a given row `top` (DIPs), or for the header
    /// when `header` is true (centered in the header band).
    fn checkbox_box(&self, top: f32, header: bool) -> D2D_RECT_F {
        let col_left = self.content_left();
        let box_left = col_left + (CHECKBOX_COL_W - CHECK_BOX) / 2.0;
        let band_h = if header { HEADER_H } else { ROW_H };
        let box_top = top + (band_h - CHECK_BOX) / 2.0;
        D2D_RECT_F {
            left: box_left,
            top: box_top,
            right: box_left + CHECK_BOX,
            bottom: box_top + CHECK_BOX,
        }
    }

    /// Body row index at a client-DIP y, or None if in the header / past the end.
    fn row_at(&self, y: f32) -> Option<usize> {
        let body = self.body_rect();
        if y < body.top {
            return None;
        }
        let rel = y - body.top + self.scroll.offset();
        if rel < 0.0 {
            return None;
        }
        let slot = self.state.row_slot();
        let i = (rel / slot) as usize;
        // No inter-row gap now (rows abut, separated only by a 1px divider), so the
        // whole slot belongs to the row.
        if i < self.state.rows.len() { Some(i) } else { None }
    }

    fn selected_indices(&self) -> Vec<usize> {
        (0..self.selected.len()).filter(|&i| self.selected[i]).collect()
    }
    fn any_selected(&self) -> bool {
        self.selected.iter().any(|&b| b)
    }
    fn all_selected(&self) -> bool {
        !self.selected.is_empty() && self.selected.iter().all(|&b| b)
    }
}

impl QT {
    pub fn create_data_grid(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_DATA_GRID");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_CLASSDC | CS_DBLCLKS,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let scaling_factor = get_scaling_factor(parent_window);
            // Default width = padding + checkbox column + sum of column widths.
            let has_checkbox_col = props.selection_mode != SelectionMode::None;
            let cols_w: f32 = props.columns.iter().map(|c| c.width as f32).sum();
            let stroke = self.theme.tokens.stroke_width_thin;
            let natural_w =
                stroke * 2.0 + if has_checkbox_col { CHECKBOX_COL_W } else { 0.0 } + cols_w;
            let width = if props.width > 0 { props.width as f32 } else { natural_w };
            let height = if props.height > 0 { props.height as f32 } else { 260.0 };
            let boxed = Box::new(State {
                qt: self.clone(),
                columns: props.columns,
                rows: props.rows,
                width,
                height,
                selection_mode: props.selection_mode,
                selection_appearance: props.selection_appearance,
                background: props.background,
                on_selection_change: props.mouse_event.on_selection_change,
                on_activate: props.mouse_event.on_activate,
            });
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
            Ok(hwnd)
        }
    }

    /// The current selection (sorted row indices).
    pub fn data_grid_selection(&self, grid: HWND) -> Vec<usize> {
        unsafe {
            let raw = GetWindowLongPtrW(grid, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                Vec::new()
            } else {
                (*raw).selected_indices()
            }
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

fn make_svg(
    dc5: &ID2D1DeviceContext5,
    icon: &Icon,
    color: &D2D1_COLOR_F,
) -> Option<ID2D1SvgDocument> {
    unsafe {
        let stream = SHCreateMemStream(Some(icon.svg.as_bytes()))?;
        let svg = dc5
            .CreateSvgDocument(
                &stream,
                D2D_SIZE_F {
                    width: icon.size as f32,
                    height: icon.size as f32,
                },
            )
            .ok()?;
        set_svg_color(&svg, color);
        Some(svg)
    }
}

fn create_format(qt: &QT) -> Result<IDWriteTextFormat> {
    let tokens = &qt.theme.tokens;
    unsafe {
        let format = qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base300,
            w!(""),
        )?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    unsafe {
        let cell_format = create_format(&state.qt)?;
        let header_format = create_format(&state.qt)?;
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

        let tokens = &state.qt.theme.tokens;
        let dc5 = render_target.cast::<ID2D1DeviceContext5>()?;

        // Checkmark for the checkbox column (inverted, like checkbox.rs).
        let checkmark_svg = make_svg(
            &dc5,
            &Icon::checkmark_12_filled(),
            &tokens.color_neutral_foreground_inverted,
        )
        .ok_or(Error::from(E_FAIL))?;
        // Mixed (select-all partial) inner square — re-tinted per draw.
        let square_svg = make_svg(
            &dc5,
            &Icon::square_12_filled(),
            &tokens.color_compound_brand_foreground1,
        )
        .ok_or(Error::from(E_FAIL))?;

        // Per-cell icon SVGs (rows × columns), tinted foreground2.
        let icon_color = tokens.color_neutral_foreground2;
        let icon_svgs = state
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| cell.icon.and_then(|ic| make_svg(&dc5, &ic, &icon_color)))
                    .collect()
            })
            .collect();

        let n = state.rows.len();
        Ok(Context {
            state,
            cell_format,
            header_format,
            render_target,
            checkmark_svg,
            square_svg,
            icon_svgs,
            selected: vec![false; n],
            focused: None,
            anchor: None,
            hovered_row: None,
            hovered_col: None,
            pressed: None,
            is_focused: false,
            is_hovered: false,
            scroll: VScroll::new(),
        })
    }
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
        // Clip the window to the rounded field so row fills / dividers don't poke
        // past the corners.
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

fn update_metrics(context: &mut Context) {
    let slot = context.state.row_slot();
    let n = context.state.rows.len() as f32;
    // Each row's slot already includes its 1px divider; no trailing gap to subtract.
    let content_h = (n * slot).max(0.0);
    let body = context.body_rect();
    let viewport_h = body.bottom - body.top;
    context.scroll.set_metrics(content_h, viewport_h, slot);
}

fn ensure_row_visible(context: &mut Context, i: usize) {
    let slot = context.state.row_slot();
    context.scroll.ensure_visible(i as f32 * slot, i as f32 * slot + ROW_H);
}

fn fire_selection(window: HWND, context: &Context) {
    let indices = context.selected_indices();
    (context.state.on_selection_change)(&window, &indices);
}

// --- selection mutations ---

fn select_single(window: HWND, context: &mut Context, i: usize) {
    context.selected.iter_mut().for_each(|b| *b = false);
    context.selected[i] = true;
    context.anchor = Some(i);
    context.focused = Some(i);
    ensure_row_visible(context, i);
    fire_selection(window, context);
}

/// Single-select toggle: select `i` alone, or clear if it was already the one.
fn toggle_single(window: HWND, context: &mut Context, i: usize) {
    let was_only = context.selected[i];
    context.selected.iter_mut().for_each(|b| *b = false);
    context.selected[i] = !was_only;
    context.anchor = Some(i);
    context.focused = Some(i);
    ensure_row_visible(context, i);
    fire_selection(window, context);
}

fn toggle_row(window: HWND, context: &mut Context, i: usize) {
    context.selected[i] = !context.selected[i];
    context.anchor = Some(i);
    context.focused = Some(i);
    ensure_row_visible(context, i);
    fire_selection(window, context);
}

fn select_range(window: HWND, context: &mut Context, to: usize) {
    let from = context.anchor.unwrap_or(to);
    let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
    context.selected.iter_mut().for_each(|b| *b = false);
    for b in &mut context.selected[lo..=hi] {
        *b = true;
    }
    context.focused = Some(to);
    ensure_row_visible(context, to);
    fire_selection(window, context);
}

fn set_all(window: HWND, context: &mut Context, value: bool) {
    context.selected.iter_mut().for_each(|b| *b = value);
    fire_selection(window, context);
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let background = state.background.unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let stroke = tokens.stroke_width_thin;
        let radius = tokens.border_radius_medium;

        // Field box — rounded frame (Fluent surface).
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
        context.render_target.FillRoundedRectangle(&field_rect, &fill_brush);

        // Body rows — below the header band, clipped to the body area.
        let body = context.body_rect();
        let offset = context.scroll.offset();
        let slot = state.row_slot();
        context.render_target.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: body.left,
                top: body.top,
                right: body.right,
                bottom: height - stroke,
            },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );

        for (i, row) in state.rows.iter().enumerate() {
            let top = body.top + i as f32 * slot - offset;
            let bottom = top + ROW_H;
            if bottom < body.top || top > height {
                continue;
            }
            let is_selected = context.selected[i];
            let is_hovered = context.hovered_row == Some(i);
            let is_pressed = context.pressed == Some(i);
            let cell_pad = state.cell_pad();

            // Row fill (no corner radius). Precedence: pressed → hover → selected —
            // hovering a selected row shows the hover colour, not the selected one.
            let selected_fill = match state.selection_appearance {
                SelectionAppearance::Brand => Some(tokens.color_brand_background2),
                SelectionAppearance::Neutral => Some(tokens.color_subtle_background_selected),
                SelectionAppearance::None => None,
            };
            let fill = if is_pressed {
                Some(tokens.color_subtle_background_pressed)
            } else if is_hovered {
                Some(tokens.color_subtle_background_hover)
            } else if is_selected {
                selected_fill
            } else {
                None
            };
            if let Some(color) = fill {
                let brush = context.render_target.CreateSolidColorBrush(&color, None)?;
                context.render_target.FillRectangle(
                    &D2D_RECT_F { left: body.left, top, right: body.right, bottom },
                    &brush,
                );
            }

            // Bottom divider (no gap between rows).
            let divider = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_stroke2, None)?;
            context.render_target.FillRectangle(
                &D2D_RECT_F {
                    left: body.left,
                    top: bottom,
                    right: body.right,
                    bottom: bottom + state.row_divider(),
                },
                &divider,
            );

            if state.has_checkbox_col() {
                draw_checkbox(context, context.checkbox_box(top, false), is_selected, false)?;
            }

            // Text colour follows the row interaction state.
            let text_color = if is_pressed {
                &tokens.color_neutral_foreground1_pressed
            } else if is_hovered {
                &tokens.color_neutral_foreground1_hover
            } else {
                &tokens.color_neutral_foreground1
            };
            for (c, _col) in state.columns.iter().enumerate() {
                let cell = row.cells.get(c);
                let Some(cell) = cell else { continue };
                let cl = context.col_left(c);
                let cr = cl + state.columns[c].width as f32;
                context.render_target.PushAxisAlignedClip(
                    &D2D_RECT_F { left: cl, top, right: cr, bottom },
                    D2D1_ANTIALIAS_MODE_ALIASED,
                );
                let mut text_left = cl + cell_pad;
                if cell.icon.is_some() {
                    if let Some(Some(svg)) = context.icon_svgs.get(i).and_then(|r| r.get(c)) {
                        draw_cell_icon(context, svg, text_left, top, bottom)?;
                    }
                    text_left += CELL_ICON + cell_pad;
                }
                if !cell.text.is_null() && !cell.text.as_wide().is_empty() {
                    let text_brush = context
                        .render_target
                        .CreateSolidColorBrush(text_color, None)?;
                    context.render_target.DrawText(
                        cell.text.as_wide(),
                        &context.cell_format,
                        &D2D_RECT_F { left: text_left, top, right: cr - cell_pad, bottom },
                        &text_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                context.render_target.PopAxisAlignedClip();
            }
        }

        context.render_target.PopAxisAlignedClip();

        // Header band (fixed) — painted over the field, above the body clip.
        let header_fill = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_background1, None)?;
        context.render_target.FillRectangle(
            &D2D_RECT_F { left: stroke, top: stroke, right: width - stroke, bottom: HEADER_H },
            &header_fill,
        );

        if let Some(hc) = context.hovered_col {
            let cl = context.col_left(hc);
            // The last column's highlight extends to the field edge (its trailing
            // padding) rather than stopping at the column width.
            let cr = if hc + 1 == state.columns.len() {
                width - stroke
            } else {
                cl + state.columns[hc].width as f32
            };
            let hover = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_subtle_background_hover, None)?;
            context.render_target.FillRectangle(
                &D2D_RECT_F { left: cl, top: stroke, right: cr, bottom: HEADER_H },
                &hover,
            );
        }

        if state.has_select_all() {
            let all = context.all_selected();
            let some = context.any_selected() && !all;
            draw_checkbox(context, context.checkbox_box(0.0, true), all, some)?;
        }

        let header_pad = state.cell_pad();
        let header_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_foreground2, None)?;
        for (c, col) in state.columns.iter().enumerate() {
            let cl = context.col_left(c);
            let cr = cl + col.width as f32;
            context.render_target.DrawText(
                col.header.as_wide(),
                &context.header_format,
                &D2D_RECT_F {
                    left: cl + header_pad,
                    top: 0.0,
                    right: cr - header_pad,
                    bottom: HEADER_H,
                },
                &header_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        // Divider under the header.
        let divider = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_stroke2, None)?;
        context.render_target.FillRectangle(
            &D2D_RECT_F { left: stroke, top: HEADER_H, right: width - stroke, bottom: HEADER_H + stroke },
            &divider,
        );

        context
            .scroll
            .paint(&context.render_target, context.track_rect(), tokens)?;

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
    Ok(())
}

/// Draw a checkbox (Fluent DataGrid selection checkbox states):
/// - **checked**: filled `compoundBrandBackground` + inverted checkmark.
/// - **mixed** (indeterminate): stroked `compoundBrandStroke` box + a `Square12`
///   glyph in `compoundBrandForeground1`.
/// - **unchecked**: stroked `neutralStrokeAccessible` box.
///
/// No hover state — selection is driven by clicking the row, so the checkbox is a
/// pure state indicator (matching how the row press/hover reads).
fn draw_checkbox(
    context: &Context,
    box_rect: D2D_RECT_F,
    checked: bool,
    indeterminate: bool,
) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    let radius = tokens.border_radius_small;
    let s = tokens.stroke_width_thin;
    unsafe {
        if checked {
            let rounded = D2D1_ROUNDED_RECT { rect: box_rect, radiusX: radius, radiusY: radius };
            let fill = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_compound_brand_background, None)?;
            context.render_target.FillRoundedRectangle(&rounded, &fill);
            let dc5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
            let viewport = context.checkmark_svg.GetViewportSize();
            let scale = CHECK_GLYPH / viewport.width;
            let inset = (CHECK_BOX - CHECK_GLYPH) / 2.0;
            dc5.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: box_rect.left + inset,
                M32: box_rect.top + inset,
            });
            dc5.DrawSvgDocument(&context.checkmark_svg);
            dc5.SetTransform(&Matrix3x2::identity());
        } else if indeterminate {
            // Stroked box in the compound-brand stroke colour…
            let border = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_compound_brand_stroke, None)?;
            context.render_target.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: box_rect.left + s * 0.5,
                        top: box_rect.top + s * 0.5,
                        right: box_rect.right - s * 0.5,
                        bottom: box_rect.bottom - s * 0.5,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &border,
                s,
                &context.state.qt.stroke_style,
            );
            // …with the inner Square glyph in compound-brand foreground1.
            set_svg_color(&context.square_svg, &tokens.color_compound_brand_foreground1);
            let dc5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
            let viewport = context.square_svg.GetViewportSize();
            let scale = CHECK_GLYPH / viewport.width;
            let inset = (CHECK_BOX - CHECK_GLYPH) / 2.0;
            dc5.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: box_rect.left + inset,
                M32: box_rect.top + inset,
            });
            dc5.DrawSvgDocument(&context.square_svg);
            dc5.SetTransform(&Matrix3x2::identity());
        } else {
            let border = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_stroke_accessible, None)?;
            context.render_target.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: box_rect.left + s * 0.5,
                        top: box_rect.top + s * 0.5,
                        right: box_rect.right - s * 0.5,
                        bottom: box_rect.bottom - s * 0.5,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &border,
                s,
                &context.state.qt.stroke_style,
            );
        }
    }
    Ok(())
}

/// Draw a cell's leading icon (20px, vertically centered) at `icon_left`.
fn draw_cell_icon(
    context: &Context,
    svg: &ID2D1SvgDocument,
    icon_left: f32,
    top: f32,
    bottom: f32,
) -> Result<()> {
    unsafe {
        let dc5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        let vp = svg.GetViewportSize();
        let scale = CELL_ICON / vp.width;
        let icon_top = top + ((bottom - top) - CELL_ICON) / 2.0;
        dc5.SetTransform(&Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: icon_left,
            M32: icon_top,
        });
        dc5.DrawSvgDocument(svg);
        dc5.SetTransform(&Matrix3x2::identity());
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

fn page_rows(context: &Context) -> usize {
    let body = context.body_rect();
    (((body.bottom - body.top) / context.state.row_slot()).floor() as usize).max(1)
}

fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { (GetKeyState(vk.0 as i32) as u16 & 0x8000) != 0 }
}

/// Column index the checkbox column occupies a client-DIP x, true if over it.
fn over_checkbox_col(context: &Context, x: f32) -> bool {
    if !context.state.has_checkbox_col() {
        return false;
    }
    let left = context.content_left();
    x >= left && x < left + CHECKBOX_COL_W
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
            (*raw).is_focused = true;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = false;
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
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
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
            } else {
                let (over, redraw) = context.scroll.on_mouse_move(px, py, context.track_rect());
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
                let new_hover = if over { None } else { context.row_at(py) };
                if new_hover != context.hovered_row {
                    context.hovered_row = new_hover;
                    _ = InvalidateRect(Some(window), None, false);
                }
                // Header column hover — within the header band, off the scrollbar.
                let new_col = if !over && py < HEADER_H {
                    context.col_at_x(px)
                } else {
                    None
                };
                if new_col != context.hovered_col {
                    context.hovered_col = new_col;
                    _ = InvalidateRect(Some(window), None, false);
                }
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_hovered = false;
            context.hovered_row = None;
            context.hovered_col = None;
            context.pressed = None;
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

            // Scrollbar first (it overlays the body).
            let (handled, redraw) = match context
                .scroll
                .on_l_button_down(px, py, context.track_rect())
            {
                ScrollHit::Miss => (false, false),
                ScrollHit::Thumb => (true, true),
                ScrollHit::Track | ScrollHit::Up | ScrollHit::Down => {
                    SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INITIAL_MS, None);
                    (true, true)
                }
            };
            if handled {
                SetCapture(window);
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
                return LRESULT(0);
            }

            // Header: only the select-all checkbox is interactive.
            if py < HEADER_H {
                if context.state.has_select_all() && over_checkbox_col(context, px) {
                    let value = !context.all_selected();
                    set_all(window, context, value);
                    _ = InvalidateRect(Some(window), None, false);
                }
                return LRESULT(0);
            }

            if context.state.selection_mode == SelectionMode::None {
                return LRESULT(0);
            }
            if let Some(i) = context.row_at(py) {
                context.pressed = Some(i);
                SetCapture(window);
                // A click anywhere on the row toggles it (checkbox and body alike);
                // Shift extends a range in Multiselect.
                match context.state.selection_mode {
                    SelectionMode::Single => toggle_single(window, context, i),
                    SelectionMode::Multiselect => {
                        if key_down(VK_SHIFT) {
                            select_range(window, context, i);
                        } else {
                            toggle_row(window, context, i);
                        }
                    }
                    SelectionMode::None => {}
                }
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let redraw = context.scroll.on_l_button_up();
            _ = KillTimer(Some(window), REPEAT_TIMER_ID);
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            let was_pressed = context.pressed.take().is_some();
            if redraw || was_pressed {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONDBLCLK => unsafe {
            // Win32 ListView's activate gesture (NM_DBLCLK). The 1st click of the pair
            // already selected the row (via WM_LBUTTONDOWN); the double-click only
            // *activates* it — it does NOT re-toggle the selection.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            // Ignore the scrollbar and the header band.
            if context.scroll.on_l_button_down(px, py, context.track_rect()) != ScrollHit::Miss {
                context.scroll.on_l_button_up();
                return LRESULT(0);
            }
            if py < HEADER_H {
                return LRESULT(0);
            }
            if context.state.selection_mode != SelectionMode::None {
                if let Some(i) = context.row_at(py) {
                    // Keep the pressed affordance until button-up; fire activate.
                    context.pressed = Some(i);
                    SetCapture(window);
                    (context.state.on_activate)(&window, i);
                    _ = InvalidateRect(Some(window), None, false);
                }
            }
            LRESULT(0)
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
        WM_GETDLGCODE => LRESULT(DLGC_WANTARROWS as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let n = context.state.rows.len();
            if n == 0 {
                return DefWindowProcW(window, message, w_param, l_param);
            }
            let move_focus = |context: &mut Context, i: usize| {
                context.focused = Some(i);
                ensure_row_visible(context, i);
                _ = InvalidateRect(Some(window), None, false);
            };
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_UP => {
                    let i = context.focused.map(|f| f.saturating_sub(1)).unwrap_or(0);
                    move_focus(context, i);
                }
                VK_DOWN => {
                    let i = context.focused.map(|f| (f + 1).min(n - 1)).unwrap_or(0);
                    move_focus(context, i);
                }
                VK_HOME => move_focus(context, 0),
                VK_END => move_focus(context, n - 1),
                VK_PRIOR => {
                    let page = page_rows(context);
                    let i = context.focused.unwrap_or(0).saturating_sub(page);
                    move_focus(context, i);
                }
                VK_NEXT => {
                    let page = page_rows(context);
                    let i = (context.focused.unwrap_or(0) + page).min(n - 1);
                    move_focus(context, i);
                }
                VK_SPACE => {
                    if let Some(i) = context.focused {
                        match context.state.selection_mode {
                            SelectionMode::Single => select_single(window, context, i),
                            SelectionMode::Multiselect => toggle_row(window, context, i),
                            SelectionMode::None => {}
                        }
                        _ = InvalidateRect(Some(window), None, false);
                    }
                }
                VK_A if key_down(VK_CONTROL)
                    && context.state.selection_mode == SelectionMode::Multiselect =>
                {
                    set_all(window, context, true);
                    _ = InvalidateRect(Some(window), None, false);
                }
                _ => return DefWindowProcW(window, message, w_param, l_param),
            }
            LRESULT(0)
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
