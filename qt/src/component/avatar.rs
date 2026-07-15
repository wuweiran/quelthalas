//! An **Avatar** — Fluent UI 2's circular identity surface. v1 draws **initials on a
//! deterministic colored circle**: the person's name both produces the initials and,
//! via Fluent's exact name-hash, picks a color from a curated palette. An optional
//! **PresenceBadge** dot sits at the bottom-right corner.
//!
//! A static, self-painting `WS_CHILD` — no mouse/keyboard/interaction. Photo loading,
//! the full 30-color palette, and square shape are deferred follow-ups.

use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use crate::sys::dpi_for_window;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

use crate::component::presence_badge::{self, PresenceResources, Status};
use crate::{QT, get_scaling_factor};

/// Avatar diameter (DIPs) — a curated slice of Fluent's size ramp.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Size {
    Size24,
    Size32,
    Size48,
    Size72,
}

impl Size {
    fn dim(self) -> f32 {
        match self {
            Size::Size24 => 24.0,
            Size::Size32 => 32.0,
            Size::Size48 => 48.0,
            Size::Size72 => 72.0,
        }
    }
    /// Initials font size (DIPs), per Fluent's per-size thresholds.
    fn font_size(self) -> f32 {
        match self {
            Size::Size24 => 10.0, // base100
            Size::Size32 => 14.0, // base300
            Size::Size48 => 16.0, // base400
            Size::Size72 => 20.0, // base500
        }
    }
    /// Presence-dot diameter (DIPs) — Fluent's `getBadgeSize(size)` mapping.
    fn badge_size(self) -> presence_badge::Size {
        match self {
            Size::Size24 => presence_badge::Size::Tiny,       // 6
            Size::Size32 => presence_badge::Size::ExtraSmall, // 10
            Size::Size48 => presence_badge::Size::Small,      // 12
            Size::Size72 => presence_badge::Size::Large,      // 20
        }
    }
}

pub struct Props {
    /// The person's name — produces the initials AND the deterministic color. The
    /// caller keeps it alive (same contract as button/label text).
    pub name: PCWSTR,
    pub size: Size,
    /// Corner presence badge — `None` hides it.
    pub presence: Option<Status>,
    /// Canvas fill behind the circle (the corner ring blends into it). `None` uses the
    /// theme surface.
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            name: w!(""),
            size: Size::Size32,
            presence: None,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    size: Size,
    presence: Option<Status>,
    background: Option<D2D1_COLOR_F>,
    /// Uppercased initials (0–2 chars), computed from `name` at create.
    initials: Vec<u16>,
    /// Hashed `(foreground, background)` from `name`.
    fg: D2D1_COLOR_F,
    bg: D2D1_COLOR_F,
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    /// Presence glyphs (built only when a badge is shown).
    presence_res: Option<PresenceResources>,
}

/// The curated 8-color avatar palette `(foreground, background)`, in Fluent's order.
fn palette(qt: &QT) -> [(D2D1_COLOR_F, D2D1_COLOR_F); 8] {
    let t = &qt.theme.tokens;
    [
        (t.color_palette_red_foreground2, t.color_palette_red_background2),
        (t.color_palette_pumpkin_foreground2, t.color_palette_pumpkin_background2),
        (t.color_palette_peach_foreground2, t.color_palette_peach_background2),
        (t.color_palette_forest_foreground2, t.color_palette_forest_background2),
        (t.color_palette_teal_foreground2, t.color_palette_teal_background2),
        (t.color_palette_blue_foreground2, t.color_palette_blue_background2),
        (t.color_palette_purple_foreground2, t.color_palette_purple_background2),
        (t.color_palette_magenta_foreground2, t.color_palette_magenta_background2),
    ]
}

/// Fluent's exact avatar name-hash (`useAvatar.tsx`).
fn hash_code(name: &[u16]) -> u32 {
    let mut hash: u32 = 0;
    for i in (0..name.len()).rev() {
        let ch = name[i] as u32;
        let shift = (i % 8) as u32;
        hash ^= (ch << shift).wrapping_add(ch >> (8 - shift));
    }
    hash
}

/// First letter of the first + last whitespace-separated token, ASCII-uppercased.
fn initials(name: &[u16]) -> Vec<u16> {
    let words: Vec<&[u16]> = name
        .split(|&c| c == b' ' as u16 || c == b'\t' as u16)
        .filter(|w| !w.is_empty())
        .collect();
    let up = |c: u16| -> u16 {
        if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }
    };
    match words.as_slice() {
        [] => Vec::new(),
        [only] => vec![up(only[0])],
        [first, .., last] => vec![up(first[0]), up(last[0])],
    }
}

impl QT {
    pub fn create_avatar(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_AVATAR");
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
            let name = props.name.as_wide();

            // Initials + hashed color (empty name → neutral fallback).
            let initials = initials(name);
            let (fg, bg) = if name.is_empty() {
                let t = &self.theme.tokens;
                (t.color_neutral_foreground3, t.color_neutral_background3)
            } else {
                let pal = palette(self);
                pal[(hash_code(name) % pal.len() as u32) as usize]
            };

            let dim = props.size.dim();
            let boxed = Box::new(State {
                qt: self.clone(),
                size: props.size,
                presence: props.presence,
                background: props.background,
                initials,
                fg,
                bg,
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
        let tokens = &state.qt.theme.tokens;
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_semibold,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            state.size.font_size(),
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
                pixelSize: D2D_SIZE_U { width: 0, height: 0 },
                presentOptions: Default::default(),
            },
        )?;
        let presence_res = state
            .presence
            .map(|_| PresenceResources::new(&state.qt, &render_target, state.size.badge_size().art_px()));
        Ok(Context { state, render_target, text_format, presence_res })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    let dim = (context.state.size.dim() * scaling_factor).ceil() as i32;
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
        let r = dim / 2.0;

        // Canvas behind the circle (the corner ring blends into it).
        let canvas = state.background.unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&canvas));

        // The avatar circle.
        let bg_brush = context.render_target.CreateSolidColorBrush(&state.bg, None)?;
        context.render_target.FillEllipse(
            &D2D1_ELLIPSE { point: Vector2 { X: r, Y: r }, radiusX: r, radiusY: r },
            &bg_brush,
        );

        // Centered initials.
        if !state.initials.is_empty() {
            let fg_brush = context.render_target.CreateSolidColorBrush(&state.fg, None)?;
            context.render_target.DrawText(
                &state.initials,
                &context.text_format,
                &D2D_RECT_F { left: 0.0, top: 0.0, right: dim, bottom: dim },
                &fg_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        // Presence badge (shared draw): the corner dot with its status glyph, on a
        // surface ring, at the bottom-right. Fluent centers the badge `badgeRadius`
        // (= badge_px/2) from each edge, so the dot is tangent to the corner; the
        // surface ring (drawn +1px larger) forms the gap and clips flush at the edge.
        if let (Some(status), Some(res)) = (state.presence, &context.presence_res) {
            let badge_px = state.size.badge_size().dim();
            let cx = dim - badge_px / 2.0;
            let cy = dim - badge_px / 2.0;
            presence_badge::draw_presence(
                &context.render_target,
                Vector2 { X: cx, Y: cy },
                badge_px,
                status,
                canvas,
                res,
            )?;
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
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = layout(window, context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
