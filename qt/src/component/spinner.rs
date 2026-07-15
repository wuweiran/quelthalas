use std::mem::size_of;
use std::sync::Once;

use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES, ID2D1Bitmap,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Animation::{
    IUIAnimationManager, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionLibrary, IUIAnimationVariable, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE,
    UI_ANIMATION_MANAGER_IDLE, UIAnimationManager, UIAnimationTimer,
};
use crate::sys::dpi_for_window;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

/// Degrees of the visible arc tail (Fluent's conic-gradient tail is 240°).
const TAIL_DEG: f32 = 240.0;
/// Dot samples along the tail; ≤2° spacing overlaps into a smooth stroked arc.
const ARC_DOTS: usize = 120;
/// One revolution duration (Fluent: 1.5s linear infinite).
const SPIN_SECONDS: f64 = 1.5;

#[derive(Copy, Clone)]
pub enum Size {
    ExtraSmall,
    Small,
    Medium,
    Large,
}

pub struct Props {
    pub size: Size,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            size: Size::Medium,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    /// Control diameter in DIPs (Fluent square sizes).
    fn size(&self) -> f32 {
        match self.props.size {
            Size::ExtraSmall => 24.0,
            Size::Small => 28.0,
            Size::Medium => 32.0,
            Size::Large => 36.0,
        }
    }
    /// Ring stroke width (Fluent: strokeWidthThick 2 for ≤small, thicker 3 for ≥medium).
    fn stroke(&self) -> f32 {
        match self.props.size {
            Size::ExtraSmall | Size::Small => 2.0,
            Size::Medium | Size::Large => 3.0,
        }
    }
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    animation_manager: IUIAnimationManager,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary,
    /// Rotation angle in degrees, ramped 0→360 forever.
    angle: IUIAnimationVariable,
    /// The track ring + gradient-tail arc, baked once (rebuilt on DPI change).
    /// Each frame just rotates and blits this — one draw call instead of 121.
    arc_bitmap: Option<ID2D1Bitmap>,
}

impl QT {
    pub fn create_spinner(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_SPINNER");
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

/// Opaque channel-wise blend `from`→`to` at `t`. Used to pre-composite the arc so
/// overlapping dots overwrite (no alpha accumulation) — matching a true gradient.
fn lerp_color(from: &D2D1_COLOR_F, to: &D2D1_COLOR_F, t: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: from.r + (to.r - from.r) * t,
        g: from.g + (to.g - from.g) * t,
        b: from.b + (to.b - from.b) * t,
        a: 1.0,
    }
}

/// Bake the track ring + gradient-tail arc (unrotated) into a bitmap the size of
/// the control. Paint then just rotates and blits this — moving the 121-dot cost
/// off the per-frame path onto create/DPI-change only.
fn build_arc_bitmap(context: &Context) -> Result<ID2D1Bitmap> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    let side = state.size();
    let stroke = state.stroke();
    let cx = side / 2.0;
    let cy = side / 2.0;
    let r = (side - stroke) / 2.0;
    let center = Vector2 { X: cx, Y: cy };
    unsafe {
        // Compatible RT: transparent-backed, DPI inherited from the parent.
        let bitmap_rt = context.render_target.CreateCompatibleRenderTarget(
            Some(&D2D_SIZE_F {
                width: side,
                height: side,
            }),
            None,
            None,
            D2D1_COMPATIBLE_RENDER_TARGET_OPTIONS_NONE,
        )?;
        bitmap_rt.BeginDraw();
        bitmap_rt.Clear(Some(&D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        }));

        // Faint full track ring.
        let track_brush =
            bitmap_rt.CreateSolidColorBrush(&tokens.color_brand_stroke2_contrast, None)?;
        bitmap_rt.DrawEllipse(
            &D2D1_ELLIPSE {
                point: center,
                radiusX: r,
                radiusY: r,
            },
            &track_brush,
            stroke,
            &state.qt.stroke_style,
        );

        // Gradient-tail arc, drawn OPAQUE in pre-blended track→brand colour so
        // overlapping dots overwrite instead of accumulating alpha (translucent
        // dots compound: ~6 stacked at alpha t give 1-(1-t)^6, which made the
        // mid-arc read almost fully dark). Opaque lerp = one colour per pixel,
        // exactly like a real conic gradient. One brush reused via SetColor.
        let dot_radius = stroke / 2.0;
        let track = &tokens.color_brand_stroke2_contrast;
        let brand = &tokens.color_compound_brand_stroke;
        let dot_brush = bitmap_rt.CreateSolidColorBrush(track, None)?;
        for i in 0..=ARC_DOTS {
            let t = i as f32 / ARC_DOTS as f32;
            let theta = (TAIL_DEG * t).to_radians();
            dot_brush.SetColor(&lerp_color(track, brand, t));
            bitmap_rt.FillEllipse(
                &D2D1_ELLIPSE {
                    point: Vector2 {
                        X: cx + r * theta.cos(),
                        Y: cy + r * theta.sin(),
                    },
                    radiusX: dot_radius,
                    radiusY: dot_radius,
                },
                &dot_brush,
            );
        }

        bitmap_rt.EndDraw(None, None)?;
        bitmap_rt.GetBitmap()
    }
}

/// Schedule the angle to ramp 0→360 over SPIN_SECONDS. Re-called on idle to loop.
fn schedule_spin(context: &Context) -> Result<()> {
    unsafe {
        let transition = context
            .transition_library
            .CreateLinearTransition(SPIN_SECONDS, 360.0)?;
        let seconds_now = context.animation_timer.GetTime()?;
        context
            .animation_manager
            .ScheduleTransition(&context.angle, &transition, seconds_now)?;
    }
    Ok(())
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

            // Loop forever: when the ramp finishes, reset the angle to 0 and
            // re-schedule — same pattern as progress_bar's indeterminate bar.
            let raw = GetWindowLongPtrW(self.window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let status = context.animation_manager.GetStatus()?;
            if status == UI_ANIMATION_MANAGER_IDLE {
                context.angle = context.animation_manager.CreateAnimationVariable(0.0)?;
                schedule_spin(context)?;
            }
        }
        Ok(())
    }

    fn OnRenderingTooSlow(&self, _frames_per_second: u32) -> Result<()> {
        Ok(())
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    unsafe {
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

        // WAM wiring (mirrors progress_bar/switch).
        let animation_timer: IUIAnimationTimer =
            CoCreateInstance(&UIAnimationTimer, None, CLSCTX_INPROC_SERVER)?;
        let transition_library = state.qt.transition_library.clone();
        let animation_manager: IUIAnimationManager =
            CoCreateInstance(&UIAnimationManager, None, CLSCTX_INPROC_SERVER)?;
        let timer_update_handler = animation_manager.cast::<IUIAnimationTimerUpdateHandler>()?;
        animation_timer
            .SetTimerUpdateHandler(&timer_update_handler, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE)?;
        let timer_event_handler: IUIAnimationTimerEventHandler =
            AnimationTimerEventHandler { window }.into();
        animation_timer.SetTimerEventHandler(&timer_event_handler)?;
        let angle = animation_manager.CreateAnimationVariable(0.0)?;

        let mut context = Context {
            state,
            render_target,
            animation_manager,
            animation_timer,
            transition_library,
            angle,
            arc_bitmap: None,
        };
        context.arc_bitmap = build_arc_bitmap(&context).ok();
        schedule_spin(&context)?;
        Ok(context)
    }
}

/// Size the control to `size × size` and resize the render target.
fn layout(window: HWND, context: &Context) -> Result<()> {
    let side = context.state.size();
    let scaling_factor = get_scaling_factor(window);
    let scaled = (side * scaling_factor).ceil() as i32;
    unsafe {
        SetWindowPos(window, None, 0, 0, scaled, scaled, SWP_NOMOVE | SWP_NOZORDER)?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled as u32,
            height: scaled as u32,
        })?;
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

        let Some(bitmap) = &context.arc_bitmap else {
            return Ok(());
        };
        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let side = rc.right as f32 / scaling_factor;
        let cx = side / 2.0;
        let cy = side / 2.0;

        // Rotate the pre-baked ring+arc about the centre and blit it — one draw
        // call per frame. Rotation θ (deg) about (cx,cy) as a Matrix3x2.
        let rot = (context.angle.GetValue()? as f32).to_radians();
        let (c, s) = (rot.cos(), rot.sin());
        context.render_target.SetTransform(&Matrix3x2 {
            M11: c,
            M12: s,
            M21: -s,
            M22: c,
            M31: cx - cx * c + cy * s,
            M32: cy - cx * s - cy * c,
        });
        context.render_target.DrawBitmap(
            bitmap,
            Some(&D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: side,
                bottom: side,
            }),
            1.0,
            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            None,
        );
        context.render_target.SetTransform(&Matrix3x2::identity());
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
            let context = &*raw;
            _ = on_paint(window, context);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            // Rebake the arc at the new backing resolution so it stays crisp.
            context.arc_bitmap = build_arc_bitmap(context).ok();
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
