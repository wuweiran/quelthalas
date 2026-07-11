//! Shared vertical-scroll helper (WinUI 3 desktop style). Not a window or control —
//! a plain struct a self-painting control owns and drives from its own paint +
//! message handling. The host feeds content/viewport metrics, decides the track
//! rect (a right gutter inside its content area), forwards mouse/wheel events, and
//! calls `paint` during its render pass.
//!
//! Look: a thin rail at rest; when the host is hovered (or the bar is being used)
//! it expands into a full bar with a track, a wider thumb, and up/down repeat-arrow
//! buttons — the native WinUI 3 desktop scrollbar. Reusable by a future TreeView /
//! ListView without a container abstraction.

use std::cell::RefCell;

use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D_SIZE_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR, ID2D1DeviceContext5, ID2D1HwndRenderTarget,
    ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::{Interface, Result, w};
use windows_numerics::Matrix3x2;

use crate::icon::Icon;
use crate::theme::Tokens;

/// Scrollbar gutter width (DIPs) — WinUI ScrollBarSize. Reserved on the right of
/// the content area.
pub const SCROLLBAR_W: f32 = 12.0;
/// Height of each arrow button (DIPs) in the expanded bar (square = gutter width).
const ARROW_H: f32 = 12.0;
/// Thin thumb width at rest (DIPs) — the overlay rail.
const RAIL_W: f32 = 2.0;
/// Thumb width in the expanded bar (DIPs) — a slim pill, not a fat block.
const EXPANDED_THUMB_W: f32 = 6.0;
/// Shortest the thumb is allowed to get (DIPs) — WinUI ScrollBarVerticalThumbMinHeight.
const MIN_THUMB_LEN: f32 = 30.0;
/// Arrow glyph box (DIPs); the 20-viewBox triangle is scaled (non-uniformly) into
/// it — wider than tall for a flatter Fluent triangle.
const GLYPH_W: f32 = 10.0;
const GLYPH_H: f32 = 7.5;
/// Gap (DIPs) between an arrow button and the thumb travel channel — WinUI's
/// ScrollBarVerticalDecrease/IncreaseMargin (4).
const THUMB_GAP: f32 = 1.5;
/// How far the pill background extends past the arrows at each end (DIPs), so the
/// rounded ends aren't flush with the arrow buttons.
const BG_END_PAD: f32 = 3.0;
/// Wheel notch scroll distance = this many lines × line height.
const WHEEL_LINES: f32 = 3.0;

/// What a press/hover landed on.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ScrollPart {
    None,
    Up,
    Down,
    Thumb,
}

/// The result of a button-down on the scrollbar, so the host can react (capture,
/// start an auto-repeat timer, etc.).
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ScrollHit {
    /// Not on the scrollbar — the host should treat the click as its own (caret).
    Miss,
    /// Grabbed the thumb (host should SetCapture; drag handled via on_mouse_move).
    Thumb,
    /// Pressed the up arrow (host should SetCapture + start the repeat timer).
    Up,
    /// Pressed the down arrow (host should SetCapture + start the repeat timer).
    Down,
    /// Clicked the track above/below the thumb — already paged; host just redraws.
    Track,
}

pub struct VScroll {
    offset: f32,
    content_height: f32,
    viewport_height: f32,
    line_height: f32,
    dragging: bool,
    drag_mouse_y0: f32,
    drag_offset0: f32,
    /// Host is hovered → show the expanded bar.
    expanded: bool,
    hovered_part: ScrollPart,
    pressed_part: ScrollPart,
    /// Track hold-to-repeat paging: direction (-1 up / +1 down), the target y
    /// (the held cursor position in the track), and the track it was pressed in.
    track_paging: bool,
    page_dir: f32,
    page_target_y: f32,
    page_track: D2D_RECT_F,
    /// Arrow glyph SVGs, lazily created on first paint from the render target.
    up_svg: RefCell<Option<ID2D1SvgDocument>>,
    down_svg: RefCell<Option<ID2D1SvgDocument>>,
}

impl VScroll {
    pub fn new() -> Self {
        VScroll {
            offset: 0.0,
            content_height: 0.0,
            viewport_height: 0.0,
            line_height: 16.0,
            dragging: false,
            drag_mouse_y0: 0.0,
            drag_offset0: 0.0,
            expanded: false,
            hovered_part: ScrollPart::None,
            pressed_part: ScrollPart::None,
            track_paging: false,
            page_dir: 0.0,
            page_target_y: 0.0,
            page_track: D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
            },
            up_svg: RefCell::new(None),
            down_svg: RefCell::new(None),
        }
    }

    pub fn set_metrics(&mut self, content_height: f32, viewport_height: f32, line_height: f32) {
        self.content_height = content_height;
        self.viewport_height = viewport_height;
        self.line_height = line_height.max(1.0);
        self.offset = self.offset.clamp(0.0, self.max_offset());
    }

    pub fn offset(&self) -> f32 {
        self.offset
    }

    pub fn max_offset(&self) -> f32 {
        (self.content_height - self.viewport_height).max(0.0)
    }

    pub fn visible(&self) -> bool {
        self.max_offset() > 0.0
    }

    /// The bar is drawn expanded while hovered, dragging, or a part is pressed.
    fn is_expanded(&self) -> bool {
        self.expanded || self.dragging || self.pressed_part != ScrollPart::None
    }

    /// Host hover state (drives rail→bar expansion). Returns true if it changed.
    pub fn set_expanded(&mut self, expanded: bool) -> bool {
        if self.expanded != expanded {
            self.expanded = expanded;
            true
        } else {
            false
        }
    }

    pub fn scroll_to(&mut self, y: f32) -> bool {
        let new = y.clamp(0.0, self.max_offset());
        if (new - self.offset).abs() < f32::EPSILON {
            return false;
        }
        self.offset = new;
        true
    }

    pub fn scroll_by(&mut self, dy: f32) -> bool {
        self.scroll_to(self.offset + dy)
    }

    pub fn ensure_visible(&mut self, top: f32, bottom: f32) -> bool {
        if top < self.offset {
            self.scroll_to(top)
        } else if bottom > self.offset + self.viewport_height {
            self.scroll_to(bottom - self.viewport_height)
        } else {
            false
        }
    }

    // --- geometry (all in DIPs, relative to the track the host passes in) ---

    fn up_arrow_rect(&self, track: D2D_RECT_F) -> D2D_RECT_F {
        D2D_RECT_F {
            left: track.left,
            top: track.top,
            right: track.right,
            bottom: track.top + ARROW_H,
        }
    }

    fn down_arrow_rect(&self, track: D2D_RECT_F) -> D2D_RECT_F {
        D2D_RECT_F {
            left: track.left,
            top: track.bottom - ARROW_H,
            right: track.right,
            bottom: track.bottom,
        }
    }

    /// The thumb travel channel (between the two arrow buttons, with a gap so the
    /// thumb never touches an arrow). Reserved even at rest so the thumb doesn't
    /// jump position when the bar expands.
    fn thumb_channel(&self, track: D2D_RECT_F) -> D2D_RECT_F {
        D2D_RECT_F {
            left: track.left,
            top: track.top + ARROW_H + THUMB_GAP,
            right: track.right,
            bottom: track.bottom - ARROW_H - THUMB_GAP,
        }
    }

    /// The thumb rect within `track`, or None when there's nothing to scroll.
    pub fn thumb_rect(&self, track: D2D_RECT_F) -> Option<D2D_RECT_F> {
        if !self.visible() {
            return None;
        }
        let channel = self.thumb_channel(track);
        let channel_len = channel.bottom - channel.top;
        if channel_len <= 0.0 || self.content_height <= 0.0 {
            return None;
        }
        let ratio = (self.viewport_height / self.content_height).clamp(0.0, 1.0);
        let thumb_len = (channel_len * ratio).max(MIN_THUMB_LEN).min(channel_len);
        let travel = channel_len - thumb_len;
        let progress = if self.max_offset() > 0.0 {
            self.offset / self.max_offset()
        } else {
            0.0
        };
        let top = channel.top + travel * progress;
        // Slim pill: a thin rail at rest, a bit wider when the bar is expanded
        // (WinUI's thumb grows on hover). Both share the same right edge, so the
        // rail's right aligns with the expanded thumb's right — it only grows left.
        let w = if self.is_expanded() { EXPANDED_THUMB_W } else { RAIL_W };
        let cx = (track.left + track.right) / 2.0;
        let right = cx + EXPANDED_THUMB_W / 2.0;
        Some(D2D_RECT_F {
            left: right - w,
            top,
            right,
            bottom: top + thumb_len,
        })
    }

    fn in_rect(x: f32, y: f32, r: D2D_RECT_F) -> bool {
        x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
    }

    fn part_at(&self, x: f32, y: f32, track: D2D_RECT_F) -> ScrollPart {
        if !self.visible() {
            return ScrollPart::None;
        }
        if let Some(t) = self.thumb_rect(track) {
            if Self::in_rect(x, y, t) {
                return ScrollPart::Thumb;
            }
        }
        if self.is_expanded() {
            if Self::in_rect(x, y, self.up_arrow_rect(track)) {
                return ScrollPart::Up;
            }
            if Self::in_rect(x, y, self.down_arrow_rect(track)) {
                return ScrollPart::Down;
            }
        }
        ScrollPart::None
    }

    // --- paint ---

    pub fn paint(
        &self,
        rt: &ID2D1HwndRenderTarget,
        track: D2D_RECT_F,
        tokens: &Tokens,
    ) -> Result<()> {
        if !self.visible() {
            return Ok(());
        }
        unsafe {
            if self.is_expanded() {
                // Semi-transparent pill background (circular ends) behind the whole
                // bar — WinUI's colorNeutralBackgroundAlpha overlay. Extended a bit
                // past the arrows at each end so the rounded ends aren't flush.
                let bg_brush =
                    rt.CreateSolidColorBrush(&tokens.color_neutral_background_alpha, None)?;
                let bg_radius = (track.right - track.left) / 2.0;
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: track.left,
                            top: track.top - BG_END_PAD,
                            right: track.right,
                            bottom: track.bottom + BG_END_PAD,
                        },
                        radiusX: bg_radius,
                        radiusY: bg_radius,
                    },
                    &bg_brush,
                );

                // Arrow glyphs (Fluent Triangle Up/Down Filled SVGs). Hover/press
                // is a colour change on the glyph itself (like the thumb).
                self.paint_arrow(rt, self.up_arrow_rect(track), true, tokens)?;
                self.paint_arrow(rt, self.down_arrow_rect(track), false, tokens)?;
            }

            // Thumb.
            if let Some(thumb) = self.thumb_rect(track) {
                let color: D2D1_COLOR_F = if self.dragging {
                    tokens.color_neutral_foreground3_pressed
                } else if self.hovered_part == ScrollPart::Thumb {
                    tokens.color_neutral_foreground3_hover
                } else {
                    tokens.color_neutral_foreground3
                };
                let radius = (thumb.right - thumb.left) / 2.0;
                let brush = rt.CreateSolidColorBrush(&color, None)?;
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: thumb,
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &brush,
                );
            }
        }
        Ok(())
    }

    fn paint_arrow(
        &self,
        rt: &ID2D1HwndRenderTarget,
        rect: D2D_RECT_F,
        up: bool,
        tokens: &Tokens,
    ) -> Result<()> {
        let part = if up { ScrollPart::Up } else { ScrollPart::Down };
        // Colour change on the glyph itself (like the thumb) — no button background.
        let glyph_color = if self.pressed_part == part {
            tokens.color_neutral_foreground3_pressed
        } else if self.hovered_part == part {
            tokens.color_neutral_foreground3_hover
        } else {
            tokens.color_neutral_foreground3
        };
        unsafe {
            let device_context5 = rt.cast::<ID2D1DeviceContext5>()?;
            // Lazily create the SVG doc (once) from the render target.
            let cell = if up { &self.up_svg } else { &self.down_svg };
            if cell.borrow().is_none() {
                let icon = if up {
                    Icon::triangle_up_20_filled()
                } else {
                    Icon::triangle_down_20_filled()
                };
                let stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
                let doc = device_context5.CreateSvgDocument(
                    stream.as_ref(),
                    D2D_SIZE_F {
                        width: icon.size as f32,
                        height: icon.size as f32,
                    },
                )?;
                *cell.borrow_mut() = Some(doc);
            }
            let svg = cell.borrow();
            let svg = svg.as_ref().unwrap();

            // Tint to the current state colour.
            let paint = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(&glyph_color), w!(""))?;
            svg.GetRoot()?
                .GetFirstChild()?
                .SetAttributeValue(w!("fill"), &paint.cast::<ID2D1SvgAttribute>()?)?;

            // Scale the 20-viewBox glyph into GLYPH_W × GLYPH_H, centred in the button.
            let scale_x = GLYPH_W / 20.0;
            let scale_y = GLYPH_H / 20.0;
            let gx = (rect.left + rect.right) / 2.0 - GLYPH_W / 2.0;
            let gy = (rect.top + rect.bottom) / 2.0 - GLYPH_H / 2.0;
            device_context5.SetTransform(&Matrix3x2 {
                M11: scale_x,
                M12: 0.0,
                M21: 0.0,
                M22: scale_y,
                M31: gx,
                M32: gy,
            });
            device_context5.DrawSvgDocument(svg);
            device_context5.SetTransform(&Matrix3x2::identity());
        }
        Ok(())
    }

    // --- input ---

    /// Press. Returns what was hit; on Track it has already paged.
    pub fn on_l_button_down(&mut self, x: f32, y: f32, track: D2D_RECT_F) -> ScrollHit {
        if !self.visible() || !Self::in_rect(x, y, track) {
            return ScrollHit::Miss;
        }
        let part = self.part_at(x, y, track);
        match part {
            ScrollPart::Thumb => {
                self.dragging = true;
                self.drag_mouse_y0 = y;
                self.drag_offset0 = self.offset;
                ScrollHit::Thumb
            }
            ScrollPart::Up => {
                self.pressed_part = ScrollPart::Up;
                self.step_line(-1.0);
                ScrollHit::Up
            }
            ScrollPart::Down => {
                self.pressed_part = ScrollPart::Down;
                self.step_line(1.0);
                ScrollHit::Down
            }
            ScrollPart::None => {
                // Track press → page toward the pointer once, then keep paging
                // (hold-to-repeat) until the thumb reaches the pointer.
                let dir = {
                    let thumb_mid = self
                        .thumb_rect(track)
                        .map(|t| (t.top + t.bottom) / 2.0)
                        .unwrap_or(y);
                    if y < thumb_mid { -1.0 } else { 1.0 }
                };
                self.track_paging = true;
                self.page_dir = dir;
                self.page_target_y = y;
                self.page_track = track;
                self.page_step();
                ScrollHit::Track
            }
        }
    }

    /// One line step (for arrow buttons / auto-repeat). `dir` = -1 up, +1 down.
    pub fn step_line(&mut self, dir: f32) -> bool {
        self.scroll_by(dir * self.line_height)
    }

    /// One page step toward the held track position. Stops (clears `track_paging`)
    /// once the thumb reaches the pointer or the end is hit. Returns redraw.
    fn page_step(&mut self) -> bool {
        // Stop if the thumb now covers the target pointer y.
        if let Some(t) = self.thumb_rect(self.page_track) {
            if self.page_target_y >= t.top && self.page_target_y <= t.bottom {
                self.track_paging = false;
                return false;
            }
        }
        let page = self.viewport_height * 0.9;
        let changed = self.scroll_by(self.page_dir * page);
        if !changed {
            // Hit the end — nothing more to page.
            self.track_paging = false;
        }
        changed
    }

    /// Called by the host's repeat timer while an arrow or the track is held.
    /// Returns redraw.
    pub fn repeat_step(&mut self) -> bool {
        if self.track_paging {
            return self.page_step();
        }
        match self.pressed_part {
            ScrollPart::Up => self.step_line(-1.0),
            ScrollPart::Down => self.step_line(1.0),
            _ => false,
        }
    }

    /// Track hover + drag. Returns (over_scrollbar, needs_redraw).
    pub fn on_mouse_move(&mut self, x: f32, y: f32, track: D2D_RECT_F) -> (bool, bool) {
        if self.dragging {
            let channel = self.thumb_channel(track);
            let channel_len = channel.bottom - channel.top;
            let ratio = if self.content_height > 0.0 {
                (self.viewport_height / self.content_height).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let thumb_len = (channel_len * ratio).max(MIN_THUMB_LEN).min(channel_len);
            let travel = channel_len - thumb_len;
            let dy = y - self.drag_mouse_y0;
            let new_offset = if travel > 0.0 {
                self.drag_offset0 + dy / travel * self.max_offset()
            } else {
                self.drag_offset0
            };
            return (true, self.scroll_to(new_offset));
        }
        let part = self.part_at(x, y, track);
        let over = Self::in_rect(x, y, track);
        if part != self.hovered_part {
            self.hovered_part = part;
            return (over, true);
        }
        (over, false)
    }

    /// Release. Returns needs_redraw.
    pub fn on_l_button_up(&mut self) -> bool {
        let was_active =
            self.dragging || self.pressed_part != ScrollPart::None || self.track_paging;
        self.dragging = false;
        self.pressed_part = ScrollPart::None;
        self.track_paging = false;
        was_active
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    /// True while an arrow or the track is held (host keeps the repeat timer alive).
    pub fn is_repeating(&self) -> bool {
        self.track_paging || matches!(self.pressed_part, ScrollPart::Up | ScrollPart::Down)
    }

    /// Clear hover state (host WM_MOUSELEAVE). Returns redraw.
    pub fn clear_hover(&mut self) -> bool {
        let mut changed = false;
        if self.hovered_part != ScrollPart::None {
            self.hovered_part = ScrollPart::None;
            changed = true;
        }
        if self.set_expanded(false) {
            changed = true;
        }
        changed
    }

    pub fn on_wheel(&mut self, delta: i32) -> bool {
        if !self.visible() {
            return false;
        }
        let notches = delta as f32 / 120.0;
        self.scroll_by(-notches * WHEEL_LINES * self.line_height)
    }
}
