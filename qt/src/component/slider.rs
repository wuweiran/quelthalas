use std::mem::size_of;
use std::sync::Once;

use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ELLIPSE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VIRTUAL_KEY, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

pub struct MouseEvent {
    pub on_change: Box<dyn Fn(&HWND, f32)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_change: Box::new(|_window, _value| {}),
        }
    }
}

pub struct Props {
    pub min: f32,
    pub max: f32,
    /// Initial value (clamped to `[min, max]`).
    pub value: f32,
    /// Snap increment for both drag/click and arrow-key steps.
    pub step: f32,
    /// Track width in DIPs.
    pub width: i32,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            min: 0.0,
            max: 100.0,
            value: 0.0,
            step: 1.0,
            width: 120,
            mouse_event: MouseEvent::default(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    /// Fluent medium: 20px thumb, 4px rail, 32px min height.
    fn thumb_diameter(&self) -> f32 {
        20.0
    }
    fn thumb_radius(&self) -> f32 {
        self.thumb_diameter() / 2.0
    }
    fn rail_height(&self) -> f32 {
        4.0
    }
    fn control_height(&self) -> f32 {
        32.0
    }
    /// Inner brand disc radius (Fluent innerThumbRadius).
    fn inner_thumb_radius(&self) -> f32 {
        6.0
    }
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    value: f32,
    hovered: bool,
    pressed: bool,
    is_captured: bool,
}

impl QT {
    pub fn create_slider(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_SLIDER");
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
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
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

    /// Current value of a slider created by `create_slider`.
    pub fn slider_value(&self, slider: HWND) -> f32 {
        unsafe {
            let raw = GetWindowLongPtrW(slider, GWLP_USERDATA) as *const Context;
            if raw.is_null() { 0.0 } else { (*raw).value }
        }
    }
}

fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi)
}

/// Snap a raw value to the step grid and clamp to `[min, max]`.
fn snap(state: &State, raw: f32) -> f32 {
    let (min, max, step) = (state.props.min, state.props.max, state.props.step);
    let snapped = if step > 0.0 {
        min + ((raw - min) / step).round() * step
    } else {
        raw
    };
    clamp(snapped, min, max)
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let value = clamp(state.props.value, state.props.min, state.props.max);
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
                pixelSize: D2D_SIZE_U {
                    width: 0,
                    height: 0,
                },
                presentOptions: Default::default(),
            },
        )?;
        Ok(Context {
            state,
            render_target,
            value,
            hovered: false,
            pressed: false,
            is_captured: false,
        })
    }
}

/// Auto-size to `width` × 32 DIPs and resize the render target.
fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let width = state.props.width as f32;
    let height = state.control_height();
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (width * scaling_factor).ceil() as i32;
    let scaled_height = (height * scaling_factor).ceil() as i32;
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
    }
    Ok(())
}

/// Progress in [0,1] for the current value.
fn progress(context: &Context) -> f32 {
    let (min, max) = (context.state.props.min, context.state.props.max);
    if max > min {
        clamp((context.value - min) / (max - min), 0.0, 1.0)
    } else {
        0.0
    }
}

/// Map a client x (device px) to a snapped value.
fn value_from_x(window: HWND, context: &Context, x_px: i32) -> f32 {
    let scaling_factor = get_scaling_factor(window);
    let x = x_px as f32 / scaling_factor;
    let r = context.state.thumb_radius();
    let width = context.state.props.width as f32;
    let span = width - 2.0 * r;
    let p = if span > 0.0 {
        clamp((x - r) / span, 0.0, 1.0)
    } else {
        0.0
    };
    let raw = context.state.props.min + p * (context.state.props.max - context.state.props.min);
    snap(&context.state, raw)
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
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;

        let r = state.thumb_radius();
        let rail_h = state.rail_height();
        let rail_top = (height - rail_h) / 2.0;
        let rail_radius = rail_h / 2.0;
        let center_y = height / 2.0;
        let p = progress(context);
        let thumb_cx = r + p * (width - 2.0 * r);

        // Brand colour by state (progress fill + thumb disc share it).
        let brand = if context.pressed {
            &tokens.color_compound_brand_background_pressed
        } else if context.hovered {
            &tokens.color_compound_brand_background_hover
        } else {
            &tokens.color_compound_brand_background
        };

        // Unfilled rail [thumb_cx, width-r] — neutral.
        let rail_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_stroke_accessible, None)?;
        context.render_target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: r,
                    top: rail_top,
                    right: width - r,
                    bottom: rail_top + rail_h,
                },
                radiusX: rail_radius,
                radiusY: rail_radius,
            },
            &rail_brush,
        );
        // Filled rail [r, thumb_cx] — brand.
        if thumb_cx > r {
            let fill_brush = context.render_target.CreateSolidColorBrush(brand, None)?;
            context.render_target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: r,
                        top: rail_top,
                        right: thumb_cx,
                        bottom: rail_top + rail_h,
                    },
                    radiusX: rail_radius,
                    radiusY: rail_radius,
                },
                &fill_brush,
            );
        }

        // Thumb: white outer disc + 1px grey outline, then the brand inner disc —
        // matches Fluent's white inset ring over a brand fill.
        let center = Vector2 {
            X: thumb_cx,
            Y: center_y,
        };
        let white_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_background1, None)?;
        context.render_target.FillEllipse(
            &D2D1_ELLIPSE {
                point: center,
                radiusX: r,
                radiusY: r,
            },
            &white_brush,
        );
        let outline_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_stroke1, None)?;
        context.render_target.DrawEllipse(
            &D2D1_ELLIPSE {
                point: center,
                radiusX: r - tokens.stroke_width_thin * 0.5,
                radiusY: r - tokens.stroke_width_thin * 0.5,
            },
            &outline_brush,
            tokens.stroke_width_thin,
            &state.qt.stroke_style,
        );
        let inner_brush = context.render_target.CreateSolidColorBrush(brand, None)?;
        context.render_target.FillEllipse(
            &D2D1_ELLIPSE {
                point: center,
                radiusX: state.inner_thumb_radius(),
                radiusY: state.inner_thumb_radius(),
            },
            &inner_brush,
        );
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

/// Set the value, and if it changed, repaint + fire on_change.
fn set_value(window: HWND, context: &mut Context, new_value: f32) {
    if new_value != context.value {
        context.value = new_value;
        unsafe {
            _ = InvalidateRect(Some(window), None, false);
        }
        (context.state.props.mouse_event.on_change)(&window, new_value);
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
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            SetCapture(window);
            _ = SetFocus(Some(window));
            context.is_captured = true;
            context.pressed = true;
            let x = l_param.0 as i16 as i32;
            let new_value = value_from_x(window, context, x);
            _ = InvalidateRect(Some(window), None, false);
            set_value(window, context, new_value);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if context.is_captured && GetCapture() == window {
                // Dragging: track the cursor 1:1.
                let x = l_param.0 as i16 as i32;
                let new_value = value_from_x(window, context, x);
                set_value(window, context, new_value);
            } else if !context.hovered {
                context.hovered = true;
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
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if context.is_captured {
                if GetCapture() == window {
                    _ = ReleaseCapture();
                }
                context.is_captured = false;
            }
            context.pressed = false;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.hovered = false;
            if !context.is_captured {
                context.pressed = false;
            }
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT(DLGC_WANTARROWS as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let step = context.state.props.step.max(0.0);
            let (min, max) = (context.state.props.min, context.state.props.max);
            let new_value = match VIRTUAL_KEY(w_param.0 as u16) {
                VK_LEFT | VK_DOWN => Some(clamp(context.value - step, min, max)),
                VK_RIGHT | VK_UP => Some(clamp(context.value + step, min, max)),
                VK_HOME => Some(min),
                VK_END => Some(max),
                _ => None,
            };
            match new_value {
                Some(v) => {
                    set_value(window, context, v);
                    LRESULT(0)
                }
                None => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
