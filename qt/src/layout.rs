//! Lightweight computed layout — a `Stack` you build once and `arrange` from
//! `WM_CREATE` / `WM_SIZE`. It only repositions existing child controls
//! (`SWP_NOSIZE`); it never creates windows or resizes controls. Spacing is in
//! DIPs, scaled by the parent window's DPI at arrange time.

use windows::Win32::Foundation::{HWND, RECT};
use crate::sys::dpi_for_window;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, SWP_NOCOPYBITS, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, USER_DEFAULT_SCREEN_DPI,
};
use windows::core::Result;

/// Cross-axis placement of leaf controls. Nested stacks always fill the cross axis.
#[derive(Copy, Clone)]
pub enum Align {
    Start,
    Center,
    End,
}

#[derive(Copy, Clone)]
enum Orientation {
    Vertical,
    Horizontal,
}

enum Item {
    Control(HWND),
    /// Like `Control`, but resized to fill the cross axis (full container width in a
    /// vertical stack). Used for a header-style control (e.g. a Fill-width TabList).
    FillControl(HWND),
    Stack(Stack),
    /// A nested table — columns line up across rows (see `Grid`).
    Grid(Grid),
    Spacer(f32),
    Spring,
}

pub struct Stack {
    orientation: Orientation,
    padding: f32,
    gap: f32,
    align: Align,
    items: Vec<Item>,
}

fn window_size(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

impl Stack {
    pub fn vertical() -> Self {
        Stack {
            orientation: Orientation::Vertical,
            padding: 0.0,
            gap: 0.0,
            align: Align::Start,
            items: Vec::new(),
        }
    }

    pub fn horizontal() -> Self {
        Stack {
            orientation: Orientation::Horizontal,
            ..Stack::vertical()
        }
    }

    pub fn padding(mut self, dips: f32) -> Self {
        self.padding = dips;
        self
    }

    pub fn gap(mut self, dips: f32) -> Self {
        self.gap = dips;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    pub fn add(mut self, control: HWND) -> Self {
        self.items.push(Item::Control(control));
        self
    }

    /// Add a control that fills the cross axis (its full container width in a
    /// vertical stack), re-stretched on every arrange so it tracks window resize.
    /// Its main-axis size (height in a vertical stack) stays whatever the control
    /// reports. Use for a header-style control such as a Fill-width TabList.
    pub fn add_fill(mut self, control: HWND) -> Self {
        self.items.push(Item::FillControl(control));
        self
    }

    pub fn add_stack(mut self, stack: Stack) -> Self {
        self.items.push(Item::Stack(stack));
        self
    }

    pub fn add_grid(mut self, grid: Grid) -> Self {
        self.items.push(Item::Grid(grid));
        self
    }

    pub fn spacer(mut self, dips: f32) -> Self {
        self.items.push(Item::Spacer(dips));
        self
    }

    pub fn spring(mut self) -> Self {
        self.items.push(Item::Spring);
        self
    }

    /// Collect every leaf control HWND in this stack (recursing into nested
    /// stacks) — used to show/hide a whole page's controls at once.
    pub fn controls(&self) -> Vec<HWND> {
        let mut out = Vec::new();
        self.collect_controls(&mut out);
        out
    }

    fn collect_controls(&self, out: &mut Vec<HWND>) {
        for item in &self.items {
            match item {
                Item::Control(hwnd) | Item::FillControl(hwnd) => out.push(*hwnd),
                Item::Stack(s) => s.collect_controls(out),
                Item::Grid(g) => g.collect_controls(out),
                Item::Spacer(_) | Item::Spring => {}
            }
        }
    }

    /// Natural (width, height) in px, ignoring available space (springs count as 0).
    fn measure(&self, scale: f32) -> (i32, i32) {
        let pad = (self.padding * scale) as i32;
        let gap = (self.gap * scale) as i32;
        let mut main = 0i32;
        let mut cross = 0i32;
        let mut n = 0i32;
        for item in &self.items {
            let (im, ic) = self.item_main_cross(item, scale);
            main += im;
            cross = cross.max(ic);
            n += 1;
        }
        if n > 1 {
            main += gap * (n - 1);
        }
        main += 2 * pad;
        cross += 2 * pad;
        self.to_wh(main, cross)
    }

    /// Project an item's natural size onto this stack's (main, cross) axes.
    fn item_main_cross(&self, item: &Item, scale: f32) -> (i32, i32) {
        let (w, h) = match item {
            Item::Control(hwnd) | Item::FillControl(hwnd) => window_size(*hwnd),
            Item::Stack(s) => s.measure(scale),
            Item::Grid(g) => g.measure(scale),
            Item::Spacer(dips) => {
                let s = (dips * scale) as i32;
                match self.orientation {
                    Orientation::Vertical => (0, s),
                    Orientation::Horizontal => (s, 0),
                }
            }
            Item::Spring => (0, 0),
        };
        match self.orientation {
            Orientation::Vertical => (h, w),
            Orientation::Horizontal => (w, h),
        }
    }

    fn to_wh(&self, main: i32, cross: i32) -> (i32, i32) {
        match self.orientation {
            Orientation::Vertical => (cross, main),
            Orientation::Horizontal => (main, cross),
        }
    }

    /// Position every control within `rect` (parent client-area px). `parent`
    /// supplies the DPI. Call from `WM_CREATE` and `WM_SIZE`.
    pub fn arrange(&self, parent: HWND, rect: RECT) -> Result<()> {
        let scale = dpi_for_window(parent) as f32 / USER_DEFAULT_SCREEN_DPI as f32;
        self.arrange_scaled(rect, scale)
    }

    fn arrange_scaled(&self, rect: RECT, scale: f32) -> Result<()> {
        let pad = (self.padding * scale) as i32;
        let gap = (self.gap * scale) as i32;
        let inner = RECT {
            left: rect.left + pad,
            top: rect.top + pad,
            right: rect.right - pad,
            bottom: rect.bottom - pad,
        };
        let (inner_main, inner_cross) = match self.orientation {
            Orientation::Vertical => (inner.bottom - inner.top, inner.right - inner.left),
            Orientation::Horizontal => (inner.right - inner.left, inner.bottom - inner.top),
        };

        let sizes: Vec<(i32, i32)> = self
            .items
            .iter()
            .map(|item| self.item_main_cross(item, scale))
            .collect();
        let mut total = 0i32;
        let mut springs = 0i32;
        for (item, (im, _)) in self.items.iter().zip(sizes.iter()) {
            total += im;
            if let Item::Spring = item {
                springs += 1;
            }
        }
        let n = self.items.len() as i32;
        if n > 1 {
            total += gap * (n - 1);
        }
        let leftover = (inner_main - total).max(0);
        let spring_each = if springs > 0 { leftover / springs } else { 0 };

        let mut cursor = match self.orientation {
            Orientation::Vertical => inner.top,
            Orientation::Horizontal => inner.left,
        };
        for (item, (im, ic)) in self.items.iter().zip(sizes.iter()) {
            let (im, ic) = (*im, *ic);
            let this_main = if let Item::Spring = item { spring_each } else { im };
            match item {
                Item::Control(hwnd) => {
                    let cross_off = match self.align {
                        Align::Start => 0,
                        Align::Center => (inner_cross - ic) / 2,
                        Align::End => inner_cross - ic,
                    };
                    let (x, y) = match self.orientation {
                        Orientation::Vertical => (inner.left + cross_off, cursor),
                        Orientation::Horizontal => (cursor, inner.top + cross_off),
                    };
                    unsafe {
                        SetWindowPos(
                            *hwnd,
                            None,
                            x,
                            y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOCOPYBITS,
                        )?;
                    }
                }
                Item::FillControl(hwnd) => {
                    // Fill the cross axis (full width in a vertical stack); keep the
                    // control's own main-axis size. Positioned at the cross start.
                    let (x, y) = match self.orientation {
                        Orientation::Vertical => (inner.left, cursor),
                        Orientation::Horizontal => (cursor, inner.top),
                    };
                    let (w, h) = match self.orientation {
                        Orientation::Vertical => (inner_cross, this_main),
                        Orientation::Horizontal => (this_main, inner_cross),
                    };
                    unsafe {
                        SetWindowPos(
                            *hwnd,
                            None,
                            x,
                            y,
                            w,
                            h,
                            SWP_NOZORDER | SWP_NOCOPYBITS,
                        )?;
                    }
                }
                Item::Stack(s) => {
                    // Nested stacks fill the cross axis, so their inner springs work.
                    let child_rect = match self.orientation {
                        Orientation::Vertical => RECT {
                            left: inner.left,
                            top: cursor,
                            right: inner.right,
                            bottom: cursor + this_main,
                        },
                        Orientation::Horizontal => RECT {
                            left: cursor,
                            top: inner.top,
                            right: cursor + this_main,
                            bottom: inner.bottom,
                        },
                    };
                    s.arrange_scaled(child_rect, scale)?;
                }
                Item::Grid(g) => {
                    let (x, y) = match self.orientation {
                        Orientation::Vertical => (inner.left, cursor),
                        Orientation::Horizontal => (cursor, inner.top),
                    };
                    g.arrange_scaled(x, y, scale)?;
                }
                Item::Spacer(_) | Item::Spring => {}
            }
            cursor += this_main + gap;
        }
        Ok(())
    }
}

/// A **reposition-only table**: cells line up into columns across rows. A column's
/// width is the widest natural cell in it; a row's height is the tallest cell in it.
/// Like `Stack`, it never resizes controls — it only places each cell at its column
/// x / row y (`SWP_NOSIZE`). Add it to a `Stack` with `add_grid`.
pub struct Grid {
    col_gap: f32,
    row_gap: f32,
    /// Vertical placement of each cell within its (possibly taller) row.
    align: Align,
    rows: Vec<Vec<HWND>>,
}

impl Default for Grid {
    fn default() -> Self {
        Grid::new()
    }
}

impl Grid {
    pub fn new() -> Self {
        Grid { col_gap: 0.0, row_gap: 0.0, align: Align::Center, rows: Vec::new() }
    }

    pub fn col_gap(mut self, dips: f32) -> Self {
        self.col_gap = dips;
        self
    }

    pub fn row_gap(mut self, dips: f32) -> Self {
        self.row_gap = dips;
        self
    }

    pub fn align(mut self, align: Align) -> Self {
        self.align = align;
        self
    }

    /// Append a row of cells (left to right). Rows may differ in length; a column's
    /// width is the max over the rows that reach it.
    pub fn row(mut self, cells: Vec<HWND>) -> Self {
        self.rows.push(cells);
        self
    }

    fn collect_controls(&self, out: &mut Vec<HWND>) {
        for row in &self.rows {
            out.extend(row.iter().copied());
        }
    }

    /// Per-column widths (px) — the max natural cell width in each column.
    fn col_widths(&self) -> Vec<i32> {
        let mut widths: Vec<i32> = Vec::new();
        for row in &self.rows {
            for (c, &hwnd) in row.iter().enumerate() {
                let w = window_size(hwnd).0;
                if c >= widths.len() {
                    widths.push(w);
                } else {
                    widths[c] = widths[c].max(w);
                }
            }
        }
        widths
    }

    /// Per-row heights (px) — the max natural cell height in each row.
    fn row_heights(&self) -> Vec<i32> {
        self.rows
            .iter()
            .map(|row| row.iter().map(|&h| window_size(h).1).max().unwrap_or(0))
            .collect()
    }

    fn measure(&self, scale: f32) -> (i32, i32) {
        let cols = self.col_widths();
        let rows = self.row_heights();
        let col_gap = (self.col_gap * scale) as i32;
        let row_gap = (self.row_gap * scale) as i32;
        let mut w: i32 = cols.iter().sum();
        if cols.len() > 1 {
            w += col_gap * (cols.len() as i32 - 1);
        }
        let mut h: i32 = rows.iter().sum();
        if rows.len() > 1 {
            h += row_gap * (rows.len() as i32 - 1);
        }
        (w, h)
    }

    /// Position every cell relative to the top-left (`ox`, `oy`) in parent px. Cells
    /// keep their natural size; only their origin moves.
    fn arrange_scaled(&self, ox: i32, oy: i32, scale: f32) -> Result<()> {
        let cols = self.col_widths();
        let row_h = self.row_heights();
        let col_gap = (self.col_gap * scale) as i32;
        let row_gap = (self.row_gap * scale) as i32;

        let mut y = oy;
        for (r, row) in self.rows.iter().enumerate() {
            let rh = row_h[r];
            let mut x = ox;
            for (c, &hwnd) in row.iter().enumerate() {
                let ch = window_size(hwnd).1;
                let cross_off = match self.align {
                    Align::Start => 0,
                    Align::Center => (rh - ch) / 2,
                    Align::End => rh - ch,
                };
                unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        x,
                        y + cross_off,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOCOPYBITS,
                    )?;
                }
                x += cols.get(c).copied().unwrap_or(0) + col_gap;
            }
            y += rh + row_gap;
        }
        Ok(())
    }
}
