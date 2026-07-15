//! A classic Win32 **toolbar** (`ToolbarWindow32`), Fluent-restyled.
//!
//! A horizontal strip of command buttons + separators. Like the real Win32
//! toolbar (and unlike our dialogs), the buttons are **internal, hit-tested
//! regions** — not child `HWND`s — so the strip owns all hover/press/paint and
//! can move any items that don't fit into an **overflow flyout**. Buttons default
//! to Fluent's *subtle* chrome (transparent at rest, subtle fill on hover/press).
//! Clicking a button posts `WM_COMMAND(id)` to the parent, the same convention the
//! menu / menu bar / split button use.
//!
//! Structurally this clones `menu_bar`'s self-painting-strip scaffold and reuses
//! `button`'s SVG-icon pipeline and `menu`'s flyout for overflow. v1 ships the
//! Button + Divider items and the overflow menu; ToggleButton / RadioButton are
//! defined in [`ToolbarItem`] and painted as buttons for now.

use std::mem::size_of;
use std::sync::Once;

use crate::component::menu;
use crate::icon::Icon;
use crate::icon::path::build_geometry;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget, ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, InvalidateRect, PAINTSTRUCT,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use crate::sys::dpi_for_window;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

/// Toolbar strip height (DIPs).
const TOOLBAR_HEIGHT: f32 = 32.0;
/// Height of a button's subtle highlight, inset from the strip edges (DIPs). The
/// button fills the full strip height (32×32), so there's no inset.
const BUTTON_INSET_Y: f32 = 0.0;
/// Icon box inside a button (DIPs) — reserves the layout square. Icon buttons are
/// square: 2·pad + box = 4·2 + 24 = 32.
const ICON_SIZE: f32 = 24.0;
/// The glyph is drawn at this size, centered in the 24px box (so a 24-viewBox icon
/// renders at 20×20 with a little breathing room).
const ICON_DRAW_SIZE: f32 = 20.0;
/// Horizontal padding inside a button, each side (DIPs).
const BUTTON_PAD_X: f32 = 4.0;
/// A divider item's total horizontal footprint (DIPs): 12px padding each side, the
/// line centered between.
const DIVIDER_WIDTH: f32 = 24.0;
/// The divider line's height (DIPs) — a fixed 24px rule, vertically centered in the
/// strip regardless of the strip's own height.
const DIVIDER_HEIGHT: f32 = 24.0;
/// Left inset before the first item (DIPs).
const INSET_X: f32 = 4.0;

/// One toolbar entry. v1 fully renders `Button` and `Divider`; `ToggleButton` and
/// `RadioButton` are accepted and drawn as buttons (their pressed/checked state
/// will render as a persistent subtle fill in a follow-up).
pub enum ToolbarItem {
    Button {
        id: u32,
        icon: Option<Icon>,
        text: Option<PCWSTR>,
    },
    Divider,
    ToggleButton {
        id: u32,
        icon: Option<Icon>,
        text: Option<PCWSTR>,
        pressed: bool,
    },
    RadioButton {
        id: u32,
        group: u32,
        icon: Option<Icon>,
        text: Option<PCWSTR>,
        checked: bool,
    },
}

impl ToolbarItem {
    fn is_divider(&self) -> bool {
        matches!(self, ToolbarItem::Divider)
    }
    fn id(&self) -> Option<u32> {
        match self {
            ToolbarItem::Button { id, .. }
            | ToolbarItem::ToggleButton { id, .. }
            | ToolbarItem::RadioButton { id, .. } => Some(*id),
            ToolbarItem::Divider => None,
        }
    }
    fn icon(&self) -> Option<Icon> {
        match self {
            ToolbarItem::Button { icon, .. }
            | ToolbarItem::ToggleButton { icon, .. }
            | ToolbarItem::RadioButton { icon, .. } => *icon,
            ToolbarItem::Divider => None,
        }
    }
    fn text(&self) -> Option<PCWSTR> {
        match self {
            ToolbarItem::Button { text, .. }
            | ToolbarItem::ToggleButton { text, .. }
            | ToolbarItem::RadioButton { text, .. } => *text,
            ToolbarItem::Divider => None,
        }
    }
    /// Persistent "on" fill for a checked toggle / radio (Fluent ToggleButton look).
    fn is_on(&self) -> bool {
        matches!(
            self,
            ToolbarItem::ToggleButton { pressed: true, .. }
                | ToolbarItem::RadioButton { checked: true, .. }
        )
    }
}

pub struct Props {
    pub items: Vec<ToolbarItem>,
    /// Fixed strip width (DIPs). `0` fills the parent container (header style);
    /// a positive value pins the width, which is what makes items overflow.
    pub width: i32,
    /// Strip background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            items: Vec::new(),
            width: 0,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

/// A laid-out clickable slot on the strip: its item index and screen-independent
/// client rect (DIPs). Dividers get no slot.
#[derive(Clone, Copy)]
struct Slot {
    item_index: usize,
    left: f32,
    width: f32,
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    /// Per-item icon geometry + native pixel size (None for dividers / icon-less
    /// items). Indexed by item; the tint is applied per paint via a brush.
    icon_geometries: Vec<Option<(ID2D1PathGeometry1, f32)>>,
    /// The "More" overflow glyph geometry + native pixel size, built once.
    more_geometry: Option<(ID2D1PathGeometry1, f32)>,
    /// Visible button slots after overflow measurement (DIPs).
    slots: Vec<Slot>,
    /// Item indices pushed into the overflow flyout (in original order).
    overflow: Vec<usize>,
    /// Left edge (DIPs) of the separator divider drawn just before the More button,
    /// present only when `overflow` is non-empty. Marks the visible/overflow split.
    overflow_sep_left: Option<f32>,
    /// The "More" button's rect (DIPs), present only when `overflow` is non-empty.
    more_rect: Option<(f32, f32)>, // (left, width)
    /// Hover / press targets. `usize::MAX` marks the More button.
    hovered: Option<usize>,
    pressed: Option<usize>,
}

const MORE_TARGET: usize = usize::MAX;

impl QT {
    pub fn create_toolbar(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_TOOLBAR");
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
            let boxed = Box::new(State {
                qt: self.clone(),
                props,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_VISIBLE | WS_CHILD,
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
}

fn measure_text_width(qt: &QT, format: &IDWriteTextFormat, text: PCWSTR) -> f32 {
    unsafe {
        let Ok(layout) = qt
            .dwrite_factory
            .CreateTextLayout(text.as_wide(), format, f32::MAX, f32::MAX)
        else {
            return 0.0;
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut metrics).is_ok() {
            metrics.width.ceil()
        } else {
            0.0
        }
    }
}

/// Natural width of an item's button (DIPs). An item with an icon is rendered
/// icon-only as a square (its `text` is just the overflow-menu / tooltip label);
/// a text-only item sizes to its label.
fn button_width(context: &Context, item: &ToolbarItem) -> f32 {
    if item.icon().is_some() {
        return BUTTON_PAD_X * 2.0 + ICON_SIZE;
    }
    let text_w = item
        .text()
        .filter(|t| unsafe { !t.is_null() && !t.as_wide().is_empty() })
        .map(|t| measure_text_width(&context.state.qt, &context.text_format, t))
        .unwrap_or(0.0);
    BUTTON_PAD_X * 2.0 + text_w
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base300,
            w!(""),
        )?;
        text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;

        let dpi = dpi_for_window(window);
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

        // Build each item's icon (and the More glyph) as a fillable geometry paired
        // with its native pixel size; the tint is chosen per paint (rest/hover) via a
        // brush.
        let icon_geometries = state
            .props
            .items
            .iter()
            .map(|item| {
                item.icon().and_then(|ic| {
                    build_geometry(&state.qt.d2d_factory, &ic)
                        .ok()
                        .map(|g| (g, ic.size as f32))
                })
            })
            .collect();
        let more_icon = Icon::more_horizontal_24_regular();
        let more_geometry = build_geometry(&state.qt.d2d_factory, &more_icon)
            .ok()
            .map(|g| (g, more_icon.size as f32));

        Ok(Context {
            state,
            text_format,
            render_target,
            icon_geometries,
            more_geometry,
            slots: Vec::new(),
            overflow: Vec::new(),
            overflow_sep_left: None,
            more_rect: None,
            hovered: None,
            pressed: None,
        })
    }
}

/// Measure items left-to-right; anything that doesn't fit goes to the overflow
/// flyout behind a trailing "More" button. Fills the container width (the parent
/// owns width via `Stack::add_fill`); owns only the height.
fn layout(window: HWND, context: &mut Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    let scaled_height = (TOOLBAR_HEIGHT * scaling_factor).ceil() as i32;

    let avail = unsafe {
        // A fixed `width` pins the strip (so items overflow at a known size);
        // otherwise fill whatever width the parent stretched us to.
        let fixed = context.state.props.width;
        let scaled_width = if fixed > 0 {
            fixed.max(1)
        } else {
            let mut rc = RECT::default();
            GetClientRect(window, &mut rc)?;
            rc.right.max(1)
        };
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
        scaled_width as f32 / scaling_factor
    };

    // First pass: natural width of every item.
    let widths: Vec<f32> = context
        .state
        .props
        .items
        .iter()
        .map(|item| {
            if item.is_divider() {
                DIVIDER_WIDTH
            } else {
                button_width(context, item)
            }
        })
        .collect();

    // The "More" button reserves a fixed square whenever it's shown.
    let more_w = ICON_SIZE + BUTTON_PAD_X * 2.0;
    let n = context.state.props.items.len();
    let is_div = |i: usize| context.state.props.items[i].is_divider();

    // Prefix sums of item widths: prefix[k] = sum(widths[0..k]).
    let mut prefix = vec![0.0f32; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + widths[i];
    }

    // Place the visible prefix items[0..=upto] as slots, left-to-right.
    let place = |upto: Option<usize>| -> Vec<Slot> {
        let mut slots = Vec::new();
        let mut cursor = INSET_X;
        if let Some(upto) = upto {
            for i in 0..=upto {
                slots.push(Slot { item_index: i, left: cursor, width: widths[i] });
                cursor += widths[i];
            }
        }
        slots
    };

    // The last button (non-divider) on the strip; if there are none, nothing to show.
    let Some(last_btn) = (0..n).rev().find(|&i| !is_div(i)) else {
        context.slots = Vec::new();
        context.overflow = Vec::new();
        context.overflow_sep_left = None;
        context.more_rect = None;
        return Ok(());
    };

    // Everything through the last button (trailing dividers dropped) fits → no overflow.
    if INSET_X + prefix[last_btn + 1] + INSET_X <= avail {
        context.slots = place(Some(last_btn));
        context.overflow = Vec::new();
        context.overflow_sep_left = None;
        context.more_rect = None;
        return Ok(());
    }

    // Overflow. Choose the largest button `lb` (< last_btn) whose visible prefix
    // [0..=lb] + optional separator + More button fits. The separator is shown
    // ONLY when the item right after `lb` is a divider — i.e. the last visible
    // group is whole. If the fold splits the last group (next item is a button),
    // there's no separator (Fluent's rule).
    let sep_after = |lb: usize| lb + 1 < n && is_div(lb + 1);
    let mut chosen = None;
    for lb in 0..last_btn {
        if is_div(lb) {
            continue;
        }
        let sep_w = if sep_after(lb) { DIVIDER_WIDTH } else { 0.0 };
        if INSET_X + prefix[lb + 1] + sep_w + more_w + INSET_X <= avail {
            chosen = Some(lb);
        }
    }
    // Fall back to the first button if even it (plus More) can't fit.
    let lb = chosen.unwrap_or_else(|| (0..n).find(|&i| !is_div(i)).unwrap_or(0));

    let slots = place(Some(lb));
    let cursor = INSET_X + prefix[lb + 1];

    // Overflow = items after `lb`. A divider immediately after `lb` becomes the
    // separator before ⋯ (consumed, not repeated in the flyout).
    let has_sep = sep_after(lb);
    let mut start = lb + 1;
    if has_sep {
        start += 1;
    }
    let mut overflow: Vec<usize> = (start..n).collect();
    // Guard: never let the flyout open on a separator.
    while matches!(overflow.first().map(|&i| is_div(i)), Some(true)) {
        overflow.remove(0);
    }

    if overflow.is_empty() {
        // Nothing actually overflowed (only trailing dividers) — no ⋯.
        context.slots = slots;
        context.overflow = Vec::new();
        context.overflow_sep_left = None;
        context.more_rect = None;
        return Ok(());
    }

    let (sep_left, more_left) = if has_sep {
        (Some(cursor), cursor + DIVIDER_WIDTH)
    } else {
        (None, cursor)
    };

    context.slots = slots;
    context.overflow = overflow;
    context.overflow_sep_left = sep_left;
    context.more_rect = Some((more_left, more_w));
    Ok(())
}

/// Hit-test a client x (device px) to a hover/press target (item index or
/// `MORE_TARGET`). Dividers aren't targets.
fn hit_test(context: &Context, x_px: i32, scaling_factor: f32) -> Option<usize> {
    let x = x_px as f32 / scaling_factor;
    for slot in &context.slots {
        if context.state.props.items[slot.item_index].is_divider() {
            continue;
        }
        if x >= slot.left && x < slot.left + slot.width {
            return Some(slot.item_index);
        }
    }
    if let Some((left, width)) = context.more_rect {
        if x >= left && x < left + width {
            return Some(MORE_TARGET);
        }
    }
    None
}

fn draw_button_bg(
    context: &Context,
    left: f32,
    width: f32,
    height: f32,
    on: bool,
    hovered: bool,
    pressed: bool,
) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    let fill = if pressed {
        Some(tokens.color_subtle_background_pressed)
    } else if hovered {
        Some(tokens.color_subtle_background_hover)
    } else if on {
        Some(tokens.color_subtle_background_pressed)
    } else {
        None
    };
    if let Some(color) = fill {
        unsafe {
            let brush = context.render_target.CreateSolidColorBrush(&color, None)?;
            let rect = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left,
                    top: BUTTON_INSET_Y,
                    right: left + width,
                    bottom: height - BUTTON_INSET_Y,
                },
                radiusX: tokens.border_radius_medium,
                radiusY: tokens.border_radius_medium,
            };
            context.render_target.FillRoundedRectangle(&rect, &brush);
        }
    }
    Ok(())
}

/// Draw a glyph geometry at `ICON_DRAW_SIZE`, centered on `(cx, cy)`, tinted
/// `color`. The glyph is scaled from its native size (`native`) to exactly
/// `ICON_DRAW_SIZE` (20px), regardless of whether the source is a 20- or 24-px icon.
fn draw_icon_centered(
    render_target: &ID2D1HwndRenderTarget,
    geometry: &ID2D1PathGeometry1,
    native: f32,
    color: &D2D1_COLOR_F,
    cx: f32,
    cy: f32,
) -> Result<()> {
    unsafe {
        let brush = render_target.CreateSolidColorBrush(color, None)?;
        let scale = ICON_DRAW_SIZE / native;
        let drawn = native * scale;
        render_target.SetTransform(&Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: cx - drawn / 2.0,
            M32: cy - drawn / 2.0,
        });
        render_target.FillGeometry(geometry, &brush, None);
        render_target.SetTransform(&Matrix3x2::identity());
    }
    Ok(())
}

/// Draw a vertical divider rule at client x `cx` — a fixed `DIVIDER_HEIGHT` line,
/// vertically centered in the `height`-tall strip.
fn draw_divider(context: &Context, cx: f32, height: f32) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    let top = (height - DIVIDER_HEIGHT) / 2.0;
    unsafe {
        let brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_stroke2, None)?;
        context.render_target.DrawLine(
            windows_numerics::Vector2 { X: cx, Y: top },
            windows_numerics::Vector2 {
                X: cx,
                Y: top + DIVIDER_HEIGHT,
            },
            &brush,
            1.0,
            None,
        );
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
        let height = rc.bottom as f32 / scaling_factor;

        let text_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;

        for slot in &context.slots {
            let item = &state.props.items[slot.item_index];
            if item.is_divider() {
                draw_divider(context, slot.left + DIVIDER_WIDTH / 2.0, height)?;
                continue;
            }

            let hovered = context.hovered == Some(slot.item_index);
            let pressed = context.pressed == Some(slot.item_index);
            draw_button_bg(context, slot.left, slot.width, height, item.is_on(), hovered, pressed)?;

            if item.icon().is_some() {
                // Icon-only square: native-size glyph, centered. colorNeutralForeground2
                // at rest, colorNeutralForeground2BrandHover on hover.
                if let Some(Some((geometry, native))) = context.icon_geometries.get(slot.item_index) {
                    let color = if hovered {
                        &tokens.color_neutral_foreground2_brand_hover
                    } else {
                        &tokens.color_neutral_foreground2
                    };
                    draw_icon_centered(
                        &context.render_target,
                        geometry,
                        *native,
                        color,
                        slot.left + slot.width / 2.0,
                        height / 2.0,
                    )?;
                }
            } else if let Some(text) = item
                .text()
                .filter(|t| !t.is_null() && !t.as_wide().is_empty())
            {
                context.render_target.DrawText(
                    text.as_wide(),
                    &context.text_format,
                    &D2D_RECT_F {
                        left: slot.left + BUTTON_PAD_X,
                        top: 0.0,
                        right: slot.left + slot.width - BUTTON_PAD_X,
                        bottom: height,
                    },
                    &text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }

        // Separator divider marking the visible / overflow split, then the "More"
        // button: `… L C R │ ⋯`.
        if let Some(sep_left) = context.overflow_sep_left {
            draw_divider(context, sep_left + DIVIDER_WIDTH / 2.0, height)?;
        }

        // Trailing "More" button.
        if let Some((left, width)) = context.more_rect {
            let hovered = context.hovered == Some(MORE_TARGET);
            let pressed = context.pressed == Some(MORE_TARGET);
            draw_button_bg(context, left, width, height, false, hovered, pressed)?;
            if let Some((geometry, native)) = &context.more_geometry {
                let color = if hovered {
                    &tokens.color_neutral_foreground2_brand_hover
                } else {
                    &tokens.color_neutral_foreground2
                };
                draw_icon_centered(
                    &context.render_target,
                    geometry,
                    *native,
                    color,
                    left + width / 2.0,
                    height / 2.0,
                )?;
            }
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

/// The client rect (DIPs) of the More button's bottom-left, in screen px — the
/// anchor for the overflow flyout.
fn more_screen_anchor(window: HWND, context: &Context) -> Option<(i32, i32)> {
    let (left, width) = context.more_rect?;
    let scale = get_scaling_factor(window);
    let mut rc = RECT::default();
    unsafe {
        _ = GetClientRect(window, &mut rc);
    }
    let mut pt = POINT {
        x: ((left + width) * scale).round() as i32,
        y: rc.bottom,
    };
    unsafe {
        _ = ClientToScreen(window, &mut pt);
    }
    Some((pt.x, pt.y))
}

/// Open the overflow flyout: the overflowed items as menu entries, right-aligned
/// under the More button, posting the same `WM_COMMAND(id)` on pick.
fn open_overflow(window: HWND, context: &Context) {
    let Some((x, y)) = more_screen_anchor(window, context) else {
        return;
    };
    let mut menu_list = Vec::new();
    for &i in &context.overflow {
        let item = &context.state.props.items[i];
        if item.is_divider() {
            menu_list.push(menu::MenuInfo::MenuDivider);
        } else if let (Some(id), Some(text)) = (item.id(), item.text()) {
            menu_list.push(menu::MenuInfo::MenuItem {
                text,
                command_id: id,
                disabled: false,
                secondary_text: None,
                icon: item.icon(),
            });
        } else if let Some(id) = item.id() {
            // Icon-only overflow item: no label text, just the icon (the strip
            // buttons are icon-only, so the flyout shows the same glyph).
            menu_list.push(menu::MenuInfo::MenuItem {
                text: w!(""),
                command_id: id,
                disabled: false,
                secondary_text: None,
                icon: item.icon(),
            });
        }
    }
    if menu_list.is_empty() {
        return;
    }
    let parent = unsafe { GetParent(window).unwrap_or(window) };
    _ = context
        .state
        .qt
        .open_menu_right_aligned(parent, x, y, menu::Props { menu_list });
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
                    _ = layout(window, &mut context);
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
        WM_SIZE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if raw.is_null() {
                return DefWindowProcW(window, message, w_param, l_param);
            }
            let context = &mut *raw;
            let width = (l_param.0 & 0xffff) as u32;
            let height = (l_param.0 >> 16) as u32;
            _ = context.render_target.Resize(&D2D_SIZE_U {
                width: width.max(1),
                height: height.max(1),
            });
            // Re-run overflow measurement at the new width.
            _ = layout(window, context);
            _ = InvalidateRect(Some(window), None, false);
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
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let x = l_param.0 as i16 as i32;
            let hit = hit_test(context, x, get_scaling_factor(window));
            if context.hovered != hit {
                context.hovered = hit;
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
            let context = &mut *raw;
            context.hovered = None;
            context.pressed = None;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let x = l_param.0 as i16 as i32;
            context.pressed = hit_test(context, x, get_scaling_factor(window));
            if context.pressed.is_some() {
                _ = SetCapture(window);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            let x = l_param.0 as i16 as i32;
            let released = hit_test(context, x, get_scaling_factor(window));
            let pressed = context.pressed.take();
            _ = InvalidateRect(Some(window), None, false);
            // Fire only on a full press+release in the same target.
            if pressed.is_some() && pressed == released {
                let target = released.unwrap();
                if target == MORE_TARGET {
                    open_overflow(window, context);
                } else if let Some(id) = context.state.props.items[target].id() {
                    if let Ok(parent) = GetParent(window) {
                        _ = PostMessageW(
                            Some(parent),
                            WM_COMMAND,
                            WPARAM(id as usize),
                            LPARAM(0),
                        );
                    }
                }
            }
            LRESULT(0)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
