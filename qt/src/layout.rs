//! Lightweight computed layout — a `Stack` you build once and `arrange` from
//! `WM_CREATE` / `WM_SIZE`. It only repositions existing child controls
//! (`SWP_NOSIZE`); it never creates windows or resizes controls. Spacing is in
//! DIPs, scaled by the parent window's DPI at arrange time.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
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
    Stack(Stack),
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

    pub fn add_stack(mut self, stack: Stack) -> Self {
        self.items.push(Item::Stack(stack));
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
            Item::Control(hwnd) => window_size(*hwnd),
            Item::Stack(s) => s.measure(scale),
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
        let scale = unsafe { GetDpiForWindow(parent) } as f32 / USER_DEFAULT_SCREEN_DPI as f32;
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
                Item::Spacer(_) | Item::Spring => {}
            }
            cursor += this_main + gap;
        }
        Ok(())
    }
}
