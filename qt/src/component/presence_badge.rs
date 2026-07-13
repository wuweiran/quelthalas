//! A **PresenceBadge** — Fluent UI 2's status dot. A small status-colored glyph
//! (Available=check, Busy/DND=filled/minus, Away=clock, Offline/Blocked/OOF/Unknown=
//! outline) on a surface-colored circle. Fluent's presence icons are each a single
//! filled path: a disc with the glyph knocked out, so we just **tint the whole glyph
//! with the status color** over a `colorNeutralBackground1` circle — the outer edge +
//! glyph holes read as the surface (matching Fluent's `padding:1px; backgroundClip`).
//!
//! The draw is a pure function (`draw_presence`) shared by this standalone control and
//! by `avatar` (which hosts the badge in its own paint pass — no child window).

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ELLIPSE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_SVG_PAINT_TYPE_COLOR, ID2D1DeviceContext5, ID2D1HwndRenderTarget, ID2D1SvgAttribute,
    ID2D1SvgDocument,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

use crate::icon::Icon;
use crate::theme::Tokens;
use crate::{QT, get_scaling_factor};

/// Presence status — sets the glyph and color.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
pub enum Status {
    Available,
    Away,
    Busy,
    DoNotDisturb,
    Blocked,
    Offline,
    OutOfOffice,
    Unknown,
}

impl Status {
    /// All statuses, in demo order.
    pub fn all() -> [Status; 8] {
        [
            Status::Available,
            Status::Away,
            Status::Busy,
            Status::DoNotDisturb,
            Status::Blocked,
            Status::Offline,
            Status::OutOfOffice,
            Status::Unknown,
        ]
    }

    /// The status glyph at the given native art size (10/12/16/20 px).
    fn icon(self, art: u32) -> Icon {
        match (self, art) {
            (Status::Available, 10) => Icon::presence_available_10_filled(),
            (Status::Available, 16) => Icon::presence_available_16_filled(),
            (Status::Available, 20) => Icon::presence_available_20_filled(),
            (Status::Available, _) => Icon::presence_available_12_filled(),
            (Status::Away, 10) => Icon::presence_away_10_filled(),
            (Status::Away, 16) => Icon::presence_away_16_filled(),
            (Status::Away, 20) => Icon::presence_away_20_filled(),
            (Status::Away, _) => Icon::presence_away_12_filled(),
            (Status::Busy, 10) => Icon::presence_busy_10_filled(),
            (Status::Busy, 16) => Icon::presence_busy_16_filled(),
            (Status::Busy, 20) => Icon::presence_busy_20_filled(),
            (Status::Busy, _) => Icon::presence_busy_12_filled(),
            (Status::DoNotDisturb, 10) => Icon::presence_dnd_10_filled(),
            (Status::DoNotDisturb, 16) => Icon::presence_dnd_16_filled(),
            (Status::DoNotDisturb, 20) => Icon::presence_dnd_20_filled(),
            (Status::DoNotDisturb, _) => Icon::presence_dnd_12_filled(),
            (Status::Blocked, 10) => Icon::presence_blocked_10_regular(),
            (Status::Blocked, 16) => Icon::presence_blocked_16_regular(),
            (Status::Blocked, 20) => Icon::presence_blocked_20_regular(),
            (Status::Blocked, _) => Icon::presence_blocked_12_regular(),
            (Status::Offline, 10) => Icon::presence_offline_10_regular(),
            (Status::Offline, 16) => Icon::presence_offline_16_regular(),
            (Status::Offline, 20) => Icon::presence_offline_20_regular(),
            (Status::Offline, _) => Icon::presence_offline_12_regular(),
            (Status::OutOfOffice, 10) => Icon::presence_oof_10_regular(),
            (Status::OutOfOffice, 16) => Icon::presence_oof_16_regular(),
            (Status::OutOfOffice, 20) => Icon::presence_oof_20_regular(),
            (Status::OutOfOffice, _) => Icon::presence_oof_12_regular(),
            (Status::Unknown, 10) => Icon::presence_unknown_10_regular(),
            (Status::Unknown, 16) => Icon::presence_unknown_16_regular(),
            (Status::Unknown, 20) => Icon::presence_unknown_20_regular(),
            (Status::Unknown, _) => Icon::presence_unknown_12_regular(),
        }
    }
}

/// Badge diameter (DIPs) — Fluent's presence size ramp.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Size {
    Tiny,
    ExtraSmall,
    Small,
    Medium,
    Large,
    ExtraLarge,
}

impl Size {
    pub(crate) fn dim(self) -> f32 {
        match self {
            Size::Tiny => 6.0,
            Size::ExtraSmall => 10.0,
            Size::Small => 12.0,
            Size::Medium => 16.0,
            Size::Large => 20.0,
            Size::ExtraLarge => 28.0,
        }
    }

    /// The native icon art size to use (Fluent maps tiny/xs→10, small→12, medium→16,
    /// large/xl→20 — the outliers 6 and 28 scale the nearest shipped asset).
    pub(crate) fn art_px(self) -> u32 {
        match self {
            Size::Tiny | Size::ExtraSmall => 10,
            Size::Small => 12,
            Size::Medium => 16,
            Size::Large | Size::ExtraLarge => 20,
        }
    }
}

/// The status color (Fluent `colorPalette*Background3/Foreground3`, neutral for
/// offline/unknown).
pub(crate) fn status_color(status: Status, tokens: &Tokens) -> D2D1_COLOR_F {
    match status {
        Status::Available => tokens.color_palette_light_green_foreground3,
        Status::Busy | Status::DoNotDisturb | Status::Blocked => tokens.color_palette_red_background3,
        Status::Away => tokens.color_palette_marigold_background3,
        Status::OutOfOffice => tokens.color_palette_berry_foreground3,
        Status::Offline | Status::Unknown => tokens.color_neutral_foreground3,
    }
}

/// Per-status glyph SVGs, pre-tinted to their status color. Built once per render
/// target (glyph color is baked in, so rebuild on theme change).
pub(crate) struct PresenceResources {
    svgs: HashMap<Status, Option<ID2D1SvgDocument>>,
}

impl PresenceResources {
    /// Build the glyph set at a single native art size (10/12/16/20 px). The badge
    /// draws its native asset scaled 1:1 (or near it), so strokes match Fluent.
    pub(crate) fn new(qt: &QT, render_target: &ID2D1HwndRenderTarget, art_px: u32) -> Self {
        let mut svgs = HashMap::new();
        if let Ok(dc5) = render_target.cast::<ID2D1DeviceContext5>() {
            for status in Status::all() {
                let color = status_color(status, &qt.theme.tokens);
                svgs.insert(status, make_svg(&dc5, &status.icon(art_px), &color));
            }
        }
        PresenceResources { svgs }
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
            .CreateSvgDocument(&stream, D2D_SIZE_F { width: icon.size as f32, height: icon.size as f32 })
            .ok()?;
        set_svg_color(&svg, color);
        Some(svg)
    }
}

/// Draw the badge for `status` — a surface circle of `badge_px + 2` behind the tinted
/// glyph, both centered at `center` (DIPs). The glyph's own holes/edge show the
/// surface, forming the ring.
pub(crate) fn draw_presence(
    rt: &ID2D1HwndRenderTarget,
    center: Vector2,
    badge_px: f32,
    status: Status,
    surface: D2D1_COLOR_F,
    res: &PresenceResources,
) -> Result<()> {
    unsafe {
        // Surface halo so the glyph's transparent border reads as the ring on any backdrop.
        let ring = badge_px + 2.0;
        let ring_brush = rt.CreateSolidColorBrush(&surface, None)?;
        rt.FillEllipse(
            &D2D1_ELLIPSE { point: center, radiusX: ring / 2.0, radiusY: ring / 2.0 },
            &ring_brush,
        );
        // The tinted glyph (12px art scaled to badge_px), centered.
        if let Some(Some(svg)) = res.svgs.get(&status) {
            let dc5 = rt.cast::<ID2D1DeviceContext5>()?;
            let vp = svg.GetViewportSize();
            let scale = badge_px / vp.width;
            let left = center.X - badge_px / 2.0;
            let top = center.Y - badge_px / 2.0;
            dc5.SetTransform(&Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: left, M32: top });
            dc5.DrawSvgDocument(svg);
            dc5.SetTransform(&Matrix3x2::identity());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Standalone control
// ---------------------------------------------------------------------------

pub struct Props {
    pub status: Status,
    pub size: Size,
    /// Surface behind the badge (the ring blends into it). `None` = theme surface.
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props { status: Status::Available, size: Size::Medium, background: None }
    }
}

struct State {
    qt: QT,
    status: Status,
    size: Size,
    background: Option<D2D1_COLOR_F>,
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    res: PresenceResources,
}

impl QT {
    pub fn create_presence_badge(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_PRESENCE_BADGE");
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
            // Window is the ring square (badge + 2).
            let dim = props.size.dim() + 2.0;
            let boxed = Box::new(State {
                qt: self.clone(),
                status: props.status,
                size: props.size,
                background: props.background,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_VISIBLE | WS_CHILD,
                x,
                y,
                (dim * scaling_factor) as i32,
                (dim * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    unsafe {
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
        let res = PresenceResources::new(&state.qt, &render_target, state.size.art_px());
        Ok(Context { state, render_target, res })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    let dim = ((context.state.size.dim() + 2.0) * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(window, None, 0, 0, dim, dim, SWP_NOMOVE | SWP_NOZORDER)?;
        context.render_target.Resize(&D2D_SIZE_U { width: dim as u32, height: dim as u32 })?;
    }
    Ok(())
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let dim = rc.right as f32 / scaling_factor;
        let surface = state.background.unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&surface));
        draw_presence(
            &context.render_target,
            Vector2 { X: dim / 2.0, Y: dim / 2.0 },
            state.size.dim(),
            state.status,
            surface,
            &context.res,
        )?;
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

extern "system" fn window_proc(window: HWND, message: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
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
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = layout(window, context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
