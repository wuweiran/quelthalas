//! An **Image** — Fluent UI 2's framed picture surface. The Win32 ancestor is the
//! `STATIC` control with `SS_BITMAP`: a window that just displays a picture. Fluent
//! restyles it with **fit** modes (how the picture fills the frame), a **shape**
//! (square / rounded / circular clip), an optional 1px **border**, and an optional
//! **shadow**.
//!
//! A static, self-painting `WS_CHILD` — no mouse/keyboard/interaction, like Avatar.
//! The pixels come through **WIC** (decode the source bytes → `ID2D1Bitmap`), and the
//! draw is pure Direct2D: a fit rect + a shape clip layer + a stroke.
//!
//! ### Fit ↔ Win32 `SS_*` styles
//! | `Fit`     | Win32 static style      | behavior                          |
//! |-----------|-------------------------|-----------------------------------|
//! | `None`    | `SS_REALSIZEIMAGE`      | natural size, centered, clipped   |
//! | `Center`  | `SS_CENTERIMAGE`        | natural size, centered, clipped   |
//! | `Default` | `SS_REALSIZECONTROL`    | stretch to fill (aspect ignored)  |
//! | `Contain` | *(no native style)*     | aspect-fit inside the frame       |
//! | `Cover`   | *(no native style)*     | aspect-fill, overflow clipped     |
//!
//! `Shape` and `Shadow` are pure Fluent additions — a `STATIC` is always a
//! rectangle with no shadow.

use std::mem::{ManuallyDrop, size_of};
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_COLOR_F, D2D1_COMPOSITE_MODE_SOURCE_OVER, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    CLSID_D2D1Shadow, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_ELLIPSE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_LAYER_OPTIONS_NONE, D2D1_LAYER_PARAMETERS, D2D1_PROPERTY_TYPE_FLOAT,
    D2D1_PROPERTY_TYPE_VECTOR4, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION, D2D1_SHADOW_PROP_COLOR, ID2D1Bitmap,
    ID2D1DeviceContext, ID2D1Geometry, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::Graphics::Imaging::{
    GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapDitherTypeNone,
    WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnLoad,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

use crate::{QT, get_scaling_factor};

/// How the picture fills its frame.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Fit {
    /// Stretch to fill the frame, ignoring aspect ratio (Fluent's default).
    Default,
    /// Natural pixel size, centered and clipped.
    None,
    /// Natural pixel size, centered and clipped (same as `None`).
    Center,
    /// Scale (preserving aspect) to fit entirely inside the frame — may letterbox.
    Contain,
    /// Scale (preserving aspect) to fill the frame — overflow is clipped.
    Cover,
}

/// The frame's clip shape.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Shape {
    /// Sharp corners.
    Square,
    /// `borderRadiusMedium` (4px) corners.
    Rounded,
    /// A full circle/ellipse.
    Circular,
}

pub struct Props<'a> {
    /// Encoded image bytes (PNG/JPEG/BMP/GIF/…) — anything WIC can decode. The
    /// component copies them at create, so the caller needn't keep them alive.
    pub src: &'a [u8],
    /// Frame width (DIPs). `0` uses the image's natural width.
    pub width: f32,
    /// Frame height (DIPs). `0` uses the image's natural height.
    pub height: f32,
    pub fit: Fit,
    pub shape: Shape,
    /// A 1px `colorNeutralStroke1` frame around the picture.
    pub bordered: bool,
    /// A soft drop shadow beneath the frame (Fluent's `shadow4`).
    pub shadow: bool,
    /// Surface behind/around the picture (letterbox + rounded-corner gaps). `None`
    /// uses the theme surface.
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props<'_> {
    fn default() -> Self {
        Props {
            src: &[],
            width: 0.0,
            height: 0.0,
            fit: Fit::Default,
            shape: Shape::Square,
            bordered: false,
            shadow: false,
            background: None,
        }
    }
}

/// Extra window padding (DIPs) reserved on each side when `shadow` is on, so the
/// blur has room to render instead of being clipped at the window edge.
const SHADOW_MARGIN: f32 = 8.0;

struct State {
    qt: QT,
    /// Owned copy of the encoded bytes (decoded once in `on_create`).
    src: Vec<u8>,
    width: f32,
    height: f32,
    fit: Fit,
    shape: Shape,
    bordered: bool,
    shadow: bool,
    background: Option<D2D1_COLOR_F>,
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    bitmap: Option<ID2D1Bitmap>,
    /// Resolved frame size (DIPs) — the picture box, excluding any shadow margin.
    frame: (f32, f32),
}

/// Decode encoded bytes into a device bitmap via WIC (→ 32bpp premultiplied BGRA).
fn decode(
    wic: &IWICImagingFactory,
    rt: &ID2D1HwndRenderTarget,
    bytes: &[u8],
) -> Result<ID2D1Bitmap> {
    unsafe {
        let stream = wic.CreateStream()?;
        stream.InitializeFromMemory(bytes)?;
        let decoder =
            wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnLoad)?;
        let frame = decoder.GetFrame(0)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        rt.CreateBitmapFromWicBitmap(&converter, None)
    }
}

impl QT {
    pub fn create_image(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_IMAGE");
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
            let boxed = Box::new(State {
                qt: self.clone(),
                src: props.src.to_vec(),
                width: props.width,
                height: props.height,
                fit: props.fit,
                shape: props.shape,
                bordered: props.bordered,
                shadow: props.shadow,
                background: props.background,
            });
            // Initial size is a placeholder; `layout` fixes it once the natural size
            // is known (a 0-dim frame falls back to the decoded bitmap size).
            let margin = if props.shadow { 2.0 * SHADOW_MARGIN } else { 0.0 };
            let w = ((props.width.max(1.0) + margin) * scaling_factor) as i32;
            let h = ((props.height.max(1.0) + margin) * scaling_factor) as i32;
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_VISIBLE | WS_CHILD,
                x,
                y,
                w,
                h,
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
        let bitmap = decode(&state.qt.wic_factory, &render_target, &state.src).ok();
        // Resolve the frame: explicit dims win; a 0 dim falls back to the natural
        // (decoded) size; with no bitmap at all, a small neutral placeholder.
        let (nat_w, nat_h) = match &bitmap {
            Some(b) => {
                let s = b.GetSize();
                (s.width, s.height)
            }
            None => (100.0, 100.0),
        };
        let fw = if state.width > 0.0 { state.width } else { nat_w };
        let fh = if state.height > 0.0 { state.height } else { nat_h };
        Ok(Context { state, render_target, bitmap, frame: (fw, fh) })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    let margin = if context.state.shadow { 2.0 * SHADOW_MARGIN } else { 0.0 };
    let (fw, fh) = context.frame;
    let w = ((fw + margin) * scaling_factor).ceil() as i32;
    let h = ((fh + margin) * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(window, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER)?;
        context
            .render_target
            .Resize(&D2D_SIZE_U { width: w as u32, height: h as u32 })?;
    }
    Ok(())
}

/// The destination rect for the picture inside `frame`, per the fit mode.
fn fit_rect(fit: Fit, frame: D2D_RECT_F, img_w: f32, img_h: f32) -> D2D_RECT_F {
    let fw = frame.right - frame.left;
    let fh = frame.bottom - frame.top;
    if img_w <= 0.0 || img_h <= 0.0 {
        return frame;
    }
    let (dw, dh) = match fit {
        Fit::Default => (fw, fh),
        Fit::None | Fit::Center => (img_w, img_h),
        Fit::Contain => {
            let s = (fw / img_w).min(fh / img_h);
            (img_w * s, img_h * s)
        }
        Fit::Cover => {
            let s = (fw / img_w).max(fh / img_h);
            (img_w * s, img_h * s)
        }
    };
    // Center the scaled picture within the frame.
    let cx = (frame.left + frame.right) / 2.0;
    let cy = (frame.top + frame.bottom) / 2.0;
    D2D_RECT_F {
        left: cx - dw / 2.0,
        top: cy - dh / 2.0,
        right: cx + dw / 2.0,
        bottom: cy + dh / 2.0,
    }
}

/// Build the clip/stroke geometry for the frame shape (`None` for a plain square,
/// which uses cheaper axis-aligned ops).
fn shape_geometry(qt: &QT, shape: Shape, frame: D2D_RECT_F, radius: f32) -> Option<ID2D1Geometry> {
    unsafe {
        match shape {
            Shape::Square => None,
            Shape::Rounded => qt
                .d2d_factory
                .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect: frame,
                    radiusX: radius,
                    radiusY: radius,
                })
                .ok()
                .and_then(|g| g.cast::<ID2D1Geometry>().ok()),
            Shape::Circular => qt
                .d2d_factory
                .CreateEllipseGeometry(&D2D1_ELLIPSE {
                    point: Vector2 {
                        X: (frame.left + frame.right) / 2.0,
                        Y: (frame.top + frame.bottom) / 2.0,
                    },
                    radiusX: (frame.right - frame.left) / 2.0,
                    radiusY: (frame.bottom - frame.top) / 2.0,
                })
                .ok()
                .and_then(|g| g.cast::<ID2D1Geometry>().ok()),
        }
    }
}

/// Draw a soft `shadow4` beneath the frame shape. Renders the opaque shape into a
/// command list, blurs it with the D2D Shadow effect, and composites it 2px down.
/// Best-effort: any failure just skips the shadow.
fn draw_shadow(
    rt: &ID2D1HwndRenderTarget,
    frame: D2D_RECT_F,
    shape: Shape,
    geo: &Option<ID2D1Geometry>,
) -> Result<()> {
    unsafe {
        let dc = rt.cast::<ID2D1DeviceContext>()?;
        let list = dc.CreateCommandList()?;
        let black = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
        let opaque = dc.CreateSolidColorBrush(&black, None)?;

        // Record the opaque shape into the command list. We're already inside the
        // caller's BeginDraw; SetTarget just redirects draws to the list (no nested
        // BeginDraw — that would end the outer frame).
        let previous = dc.GetTarget()?;
        dc.SetTarget(&list);
        match (shape, geo) {
            (Shape::Square, _) | (_, None) => rt.FillRectangle(&frame, &opaque),
            (_, Some(g)) => rt.FillGeometry(g, &opaque, None),
        }
        list.Close()?;
        dc.SetTarget(&previous);

        let shadow = dc.CreateEffect(&CLSID_D2D1Shadow)?;
        shadow.SetInput(0, &list, true);
        // ~4px CSS blur ≈ σ 2.0; Fluent shadow4 is a soft ambient black.
        let sigma: f32 = 2.0;
        shadow.SetValue(
            D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32,
            D2D1_PROPERTY_TYPE_FLOAT,
            &sigma.to_le_bytes(),
        )?;
        let color = [0.0f32, 0.0, 0.0, 0.22];
        let mut color_bytes = [0u8; 16];
        for (i, c) in color.iter().enumerate() {
            color_bytes[i * 4..i * 4 + 4].copy_from_slice(&c.to_le_bytes());
        }
        shadow.SetValue(
            D2D1_SHADOW_PROP_COLOR.0 as u32,
            D2D1_PROPERTY_TYPE_VECTOR4,
            &color_bytes,
        )?;
        let output = shadow.GetOutput()?;
        dc.DrawImage(
            &output,
            Some(&Vector2 { X: 0.0, Y: 2.0 }),
            None,
            D2D1_INTERPOLATION_MODE_LINEAR,
            D2D1_COMPOSITE_MODE_SOURCE_OVER,
        );
        Ok(())
    }
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let rt = &context.render_target;
    let tokens = &state.qt.theme.tokens;
    let canvas = state.background.unwrap_or(tokens.color_neutral_background1);
    let scaling_factor = get_scaling_factor(window);
    unsafe {
        rt.Clear(Some(&canvas));

        // The picture box, inset by the shadow margin when a shadow is drawn.
        let margin = if state.shadow { SHADOW_MARGIN } else { 0.0 };
        let (fw, fh) = context.frame;
        let frame = D2D_RECT_F {
            left: margin,
            top: margin,
            right: margin + fw,
            bottom: margin + fh,
        };
        let radius = tokens.border_radius_medium;
        let geo = shape_geometry(&state.qt, state.shape, frame, radius);

        // Shadow first, beneath everything.
        if state.shadow {
            let _ = draw_shadow(rt, frame, state.shape, &geo);
        }

        // Fill the shape with the surface so letterbox areas / rounded gaps read as
        // the background (not the shadow) before the picture lands on top.
        let canvas_brush = rt.CreateSolidColorBrush(&canvas, None)?;
        match (state.shape, &geo) {
            (Shape::Square, _) | (_, None) => rt.FillRectangle(&frame, &canvas_brush),
            (_, Some(g)) => rt.FillGeometry(g, &canvas_brush, None),
        }

        // Clip to the shape, draw the picture at its fit rect, unclip.
        let mut pushed_layer = false;
        match &geo {
            None => rt.PushAxisAlignedClip(&frame, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE),
            Some(g) => {
                let params = D2D1_LAYER_PARAMETERS {
                    contentBounds: D2D_RECT_F {
                        left: f32::MIN,
                        top: f32::MIN,
                        right: f32::MAX,
                        bottom: f32::MAX,
                    },
                    geometricMask: ManuallyDrop::new(Some(g.clone())),
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    opacityBrush: ManuallyDrop::new(None),
                    layerOptions: D2D1_LAYER_OPTIONS_NONE,
                };
                rt.PushLayer(&params, None);
                // Release the cloned mask ref we handed to the layer.
                drop(ManuallyDrop::into_inner(params.geometricMask));
                pushed_layer = true;
            }
        }
        if let Some(bitmap) = &context.bitmap {
            let s = bitmap.GetSize();
            let dest = fit_rect(state.fit, frame, s.width, s.height);
            rt.DrawBitmap(
                bitmap,
                Some(&dest),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        if pushed_layer {
            rt.PopLayer();
        } else {
            rt.PopAxisAlignedClip();
        }

        // 1px neutral border, drawn on top of the shape edge.
        if state.bordered {
            let stroke = rt.CreateSolidColorBrush(&tokens.color_neutral_stroke1, None)?;
            let sw = tokens.stroke_width_thin;
            match &geo {
                Some(g) => rt.DrawGeometry(g, &stroke, sw, None),
                None => {
                    // Inset by half the stroke so the 1px line sits inside the frame.
                    let h = sw / 2.0;
                    rt.DrawRectangle(
                        &D2D_RECT_F {
                            left: frame.left + h,
                            top: frame.top + h,
                            right: frame.right - h,
                            bottom: frame.bottom - h,
                        },
                        &stroke,
                        sw,
                        None,
                    );
                }
            }
        }
    }
    let _ = scaling_factor;
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
