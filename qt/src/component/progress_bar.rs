use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::{FALSE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_GRADIENT_STOP, D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory1, ID2D1GradientStopCollection, ID2D1HwndRenderTarget,
    D2D1_EXTEND_MODE_WRAP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_GAMMA_2_2,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, SetWindowRgn, PAINTSTRUCT,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Animation::{
    IUIAnimationManager2, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionLibrary2, IUIAnimationVariable2, UIAnimationManager2, UIAnimationTimer,
    UIAnimationTransitionLibrary2, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE, UI_ANIMATION_MANAGER_IDLE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::{get_scaling_factor, QT};

#[derive(Copy, Clone)]
pub enum Shape {
    Rounded,
    Square,
}

#[derive(Copy, Clone)]
pub enum Thickness {
    Medium,
    Large,
}
pub struct State {
    qt: QT,
    shape: Shape,
    value: Option<f32>,
    max: f32,
    thickness: Thickness,
    width: f32,
}

impl State {
    fn get_height(&self) -> f32 {
        match self.thickness {
            Thickness::Medium => 2f32,
            Thickness::Large => 4f32,
        }
    }
}

pub struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    indeterminate_stop_collection: ID2D1GradientStopCollection,
    indeterminate_left: IUIAnimationVariable2,
}

impl QT {
    pub fn create_progress_bar(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        width: i32,
        shape: &Shape,
        value: Option<f32>,
        max: Option<f32>,
        thickness: &Thickness,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_PROGRESS_BAR");
        unsafe {
            let window_class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpszClassName: class_name,
                style: CS_CLASSDC,
                lpfnWndProc: Some(window_proc),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
            let scaling_factor = get_scaling_factor(parent_window);
            let boxed = Box::new(State {
                qt: self.clone(),
                value,
                max: max.unwrap_or(1f32),
                shape: *shape,
                thickness: *thickness,
                width: width as f32 / scaling_factor,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_VISIBLE | WS_CHILD,
                x,
                y,
                width,
                (boxed.as_ref().get_height() * scaling_factor) as i32,
                parent_window,
                None,
                HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }
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
            let _ = InvalidateRect(self.window, None, false);

            let raw = GetWindowLongPtrW(self.window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let status = context.animation_manager.GetStatus()?;
            if status == UI_ANIMATION_MANAGER_IDLE {
                context.indeterminate_left =
                    context.animation_manager.CreateAnimationVariable(-0.33)?;
                let transition = context
                    .transition_library
                    .CreateLinearTransition(3.0, 1.0)?;
                let seconds_now = context.animation_timer.GetTime()?;
                context.animation_manager.ScheduleTransition(
                    &context.indeterminate_left,
                    &transition,
                    seconds_now,
                )?;
            }
        }
        Ok(())
    }

    fn OnRenderingTooSlow(&self, _frames_per_second: u32) -> Result<()> {
        Ok(())
    }
}

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    let factory = D2D1CreateFactory::<ID2D1Factory1>(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
    let mut rect = RECT::default();
    GetClientRect(window, &mut rect)?;
    let dpi = GetDpiForWindow(window);
    let render_target = factory.CreateHwndRenderTarget(
        &D2D1_RENDER_TARGET_PROPERTIES {
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            ..Default::default()
        },
        &D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: window,
            pixelSize: D2D_SIZE_U {
                width: rect.right as u32,
                height: rect.bottom as u32,
            },
            presentOptions: Default::default(),
        },
    )?;

    let scaling_factor = get_scaling_factor(window);
    let tokens = &state.qt.theme.tokens;
    let corner_diameter = match state.shape {
        Shape::Rounded => rect
            .bottom
            .min((tokens.border_radius_medium * 2f32 * scaling_factor) as i32),
        Shape::Square => rect
            .bottom
            .min((tokens.border_radius_none * 2f32 * scaling_factor) as i32),
    };
    let region = CreateRoundRectRgn(
        0,
        0,
        rect.right + 1,
        rect.bottom + 1,
        corner_diameter,
        corner_diameter,
    );
    SetWindowRgn(window, region, TRUE);
    let animation_timer: IUIAnimationTimer =
        CoCreateInstance(&UIAnimationTimer, None, CLSCTX_INPROC_SERVER)?;
    let transition_library: IUIAnimationTransitionLibrary2 =
        CoCreateInstance(&UIAnimationTransitionLibrary2, None, CLSCTX_INPROC_SERVER)?;
    let animation_manager: IUIAnimationManager2 =
        CoCreateInstance(&UIAnimationManager2, None, CLSCTX_INPROC_SERVER)?;
    let timer_update_handler = animation_manager.cast::<IUIAnimationTimerUpdateHandler>()?;
    animation_timer
        .SetTimerUpdateHandler(&timer_update_handler, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE)?;
    let timer_event_handler: IUIAnimationTimerEventHandler =
        AnimationTimerEventHandler { window }.into();
    animation_timer.SetTimerEventHandler(&timer_event_handler)?;
    let indeterminate_stop_collection = render_target.CreateGradientStopCollection(
        &[
            D2D1_GRADIENT_STOP {
                position: 0.0,
                color: tokens.color_neutral_background6,
            },
            D2D1_GRADIENT_STOP {
                position: 0.5,
                color: tokens.color_compound_brand_background,
            },
            D2D1_GRADIENT_STOP {
                position: 1.0,
                color: tokens.color_neutral_background6,
            },
        ],
        D2D1_GAMMA_2_2,
        D2D1_EXTEND_MODE_WRAP,
    )?;
    let indeterminate_left = animation_manager.CreateAnimationVariable(-0.33)?;
    if let None = state.value {
        let transition = transition_library.CreateLinearTransition(3.0, 1.0)?;
        let seconds_now = animation_timer.GetTime()?;
        animation_manager.ScheduleTransition(&indeterminate_left, &transition, seconds_now)?;
    };
    Ok(Context {
        state,
        render_target,
        animation_manager,
        animation_timer,
        transition_library,
        indeterminate_stop_collection,
        indeterminate_left,
    })
}

unsafe fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    context
        .render_target
        .Clear(Some(&tokens.color_neutral_background6));

    let mut rect = RECT::default();
    GetClientRect(window, &mut rect)?;
    let scaling_factor = get_scaling_factor(window);
    let width = rect.right as f32 / scaling_factor;
    let height = rect.bottom as f32 / scaling_factor;

    match state.value {
        Some(value) => {
            let bar_width = value.min(state.max) / state.max * width;
            let corner_radius = match state.shape {
                Shape::Rounded => (height / 2f32).min(tokens.border_radius_medium),
                Shape::Square => tokens.border_radius_none,
            };
            let bar_rect = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: 0f32,
                    top: 0f32,
                    right: bar_width,
                    bottom: height,
                },
                radiusX: corner_radius,
                radiusY: corner_radius,
            };
            let bar_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_compound_brand_background, None)?;
            context
                .render_target
                .FillRoundedRectangle(&bar_rect, &bar_brush);
        }
        None => {
            let left = context.indeterminate_left.GetValue()?;
            let brush = context.render_target.CreateLinearGradientBrush(
                &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                    startPoint: D2D_POINT_2F {
                        x: left as f32 * width,
                        y: 0.0,
                    },
                    endPoint: D2D_POINT_2F {
                        x: width * 0.33 + left as f32 * width,
                        y: 0.0,
                    },
                },
                None,
                &context.indeterminate_stop_collection,
            )?;
            let indeterminate_rect = D2D_RECT_F {
                left: left as f32 * width,
                top: 0f32,
                right: width * 0.33 + left as f32 * width,
                bottom: height,
            };
            context
                .render_target
                .FillRectangle(&indeterminate_rect, &brush);
        }
    }

    Ok(())
}

unsafe fn on_paint(window: HWND, context: &Context) -> Result<()> {
    let mut ps = PAINTSTRUCT::default();
    BeginPaint(window, &mut ps);
    context.render_target.BeginDraw();
    let paint_result = paint(window, context);

    let result = paint_result.and(context.render_target.EndDraw(None, None));
    let _ = EndPaint(window, &ps);
    result
}

unsafe fn on_dpi_changed(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = context.state.width * scaling_factor;
    let scaled_height = context.state.get_height() * scaling_factor;
    SetWindowPos(
        window,
        None,
        0,
        0,
        scaled_width as i32,
        scaled_height as i32,
        SWP_NOMOVE | SWP_NOZORDER,
    )?;
    context.render_target.Resize(&D2D_SIZE_U {
        width: scaled_width as u32,
        height: scaled_height as u32,
    })?;
    let new_dpi = GetDpiForWindow(window);
    context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
    let _ = InvalidateRect(window, None, false);

    let tokens = &context.state.qt.theme.tokens;
    let corner_diameter = match context.state.shape {
        Shape::Rounded => {
            scaled_height.min(tokens.border_radius_medium * 2f32 * scaling_factor) as i32
        }
        Shape::Square => {
            scaled_height.min(tokens.border_radius_none * 2f32 * scaling_factor) as i32
        }
    };
    let region = CreateRoundRectRgn(
        0,
        0,
        scaled_width as i32 + 1,
        scaled_height as i32 + 1,
        corner_diameter,
        corner_diameter,
    );
    SetWindowRgn(window, region, TRUE);
    Ok(())
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
        WM_PRINTCLIENT | WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            match on_paint(window, context) {
                Ok(_) => LRESULT(0),
                Err(_) => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = on_dpi_changed(window, context);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
