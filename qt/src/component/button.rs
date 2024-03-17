use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRectRgn, CreateRoundRectRgn, DeleteObject, EndPaint, GetWindowRgn,
    InvalidateRect, PtInRegion, SetWindowRgn, PAINTSTRUCT,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Animation::{
    IUIAnimationManager2, IUIAnimationTimer, IUIAnimationTimerEventHandler_Impl,
    IUIAnimationTransitionLibrary2, IUIAnimationVariable2, UIAnimationTimer,
    UIAnimationTransitionLibrary2,
};
use windows::Win32::UI::Animation::{
    IUIAnimationTimerEventHandler, IUIAnimationTimerUpdateHandler, UIAnimationManager2,
    UI_ANIMATION_IDLE_BEHAVIOR_DISABLE,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use qt::{get_scaling_factor, MouseEvent};

use crate::QT;

#[derive(Copy, Clone)]
pub enum Appearance {
    Subtle,
    Outline,
    Secondary,
    Primary,
    Transparent,
}

#[derive(Copy, Clone)]
pub enum IconPosition {
    Before,
    After,
}

#[derive(Copy, Clone)]
pub enum Shape {
    Circular,
    Rounded,
    Square,
}

#[derive(Copy, Clone)]
pub enum Size {
    Small,
    Medium,
    Large,
}

struct State {
    qt_ptr: *const QT,
    text: PCWSTR,
    appearance: Appearance,
    icon_position: Option<IconPosition>,
    shape: Shape,
    size: Size,
    mouse_event: MouseEvent,
}

struct Context {
    state: State,
    factory: ID2D1Factory1,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    stroke_style: ID2D1StrokeStyle,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    background_color_variable: IUIAnimationVariable2,
    border_color_variable: IUIAnimationVariable2,
    text_color_variable: IUIAnimationVariable2,
    mouse_within: bool,
    mouse_clicking: bool,
}

impl QT {
    pub fn creat_button(
        &self,
        parent_window: &HWND,
        instance: &HINSTANCE,
        x: i32,
        y: i32,
        text: PCWSTR,
        appearance: &Appearance,
        icon_position: Option<&IconPosition>,
        shape: &Shape,
        size: &Size,
        mouse_event: MouseEvent,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_BUTTON");
        let window_class: WNDCLASSEXW = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpszClassName: class_name,
            style: CS_OWNDC,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        unsafe {
            RegisterClassExW(&window_class);
            let boxed = Box::new(State {
                qt_ptr: self as *const Self,
                text,
                appearance: *appearance,
                icon_position: icon_position.map(|a| *a),
                shape: *shape,
                size: *size,
                mouse_event,
            });
            let scaling_factor = get_scaling_factor(parent_window);
            let width = get_width(boxed.as_ref(), scaling_factor) as i32;
            let height = get_height(boxed.as_ref(), scaling_factor) as i32;
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                width,
                height,
                *parent_window,
                None,
                *instance,
                Some(Box::<State>::into_raw(boxed) as _),
            );
            Ok(window)
        }
    }
}

fn get_width(state: &State, scaling_factor: f32) -> f32 {
    (match &state.size {
        Size::Small => 96,
        Size::Medium => 96,
        Size::Large => 64,
    }) as f32
        * scaling_factor
}

fn get_line_height(state: &State, scaling_factor: f32) -> f32 {
    (match &state.size {
        Size::Small => 16,
        Size::Medium => 20,
        Size::Large => 22,
    }) as f32
        * scaling_factor
}

fn get_spacing(state: &State, scaling_factor: f32) -> f32 {
    (match &state.size {
        Size::Small => 3,
        Size::Medium => 5,
        Size::Large => 8,
    }) as f32
        * scaling_factor
}

unsafe fn get_height(state: &State, scaling_factor: f32) -> f32 {
    let tokens = &(*state.qt_ptr).tokens;
    get_line_height(state, scaling_factor)
        + get_spacing(state, scaling_factor) * 2f32
        + tokens.stroke_width_thin * 2f32
}

unsafe fn create_factory() -> Result<ID2D1Factory1> {
    let options = D2D1_FACTORY_OPTIONS::default();
    D2D1CreateFactory::<ID2D1Factory1>(D2D1_FACTORY_TYPE_SINGLE_THREADED, Some(&options))
}

fn create_render_target(
    window: &HWND,
    size: D2D_SIZE_U,
    factory: &ID2D1Factory1,
) -> Result<ID2D1HwndRenderTarget> {
    unsafe {
        factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES::default(),
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: *window,
                pixelSize: size,
                presentOptions: Default::default(),
            },
        )
    }
}

fn create_style(factory: &ID2D1Factory1) -> Result<ID2D1StrokeStyle> {
    let props = D2D1_STROKE_STYLE_PROPERTIES::default();

    unsafe { factory.CreateStrokeStyle(&props, None) }
}

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    let qt = &(*state.qt_ptr);
    let tokens = &qt.tokens;
    let scaling_factor = get_scaling_factor(&window);
    let width = get_width(&state, scaling_factor);
    let height = get_height(&state, scaling_factor);
    let corner_diameter = match &state.shape {
        Shape::Circular => width.min(height),
        Shape::Rounded => tokens.border_radius_medium * 2f32,
        Shape::Square => tokens.border_radius_none * 2f32,
    };

    let region = CreateRoundRectRgn(
        0,
        0,
        width as i32 + 1,
        height as i32 + 1,
        corner_diameter as i32,
        corner_diameter as i32,
    );
    SetWindowRgn(window, region, TRUE);
    let factory = create_factory()?;
    let direct_write_factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
    let text_format = direct_write_factory.CreateTextFormat(
        tokens.font_family_name,
        None,
        tokens.font_weight_semibold,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        tokens.font_size_base300 * scaling_factor,
        w!(""),
    )?;
    text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    let render_target = create_render_target(
        &window,
        D2D_SIZE_U {
            width: width as u32,
            height: height as u32,
        },
        &factory,
    )?;
    let stroke_style = create_style(&factory)?;

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
    let background_color = &tokens.color_neutral_background1;
    let background_color_variable = animation_manager.CreateAnimationVectorVariable(&[
        background_color.r as f64,
        background_color.g as f64,
        background_color.b as f64,
    ])?;
    let border_color = &tokens.color_neutral_stroke1;
    let border_color_variable = animation_manager.CreateAnimationVectorVariable(&[
        border_color.r as f64,
        border_color.g as f64,
        border_color.b as f64,
    ])?;
    let text_color = &tokens.color_neutral_foreground1;
    let text_color_variable = animation_manager.CreateAnimationVectorVariable(&[
        text_color.r as f64,
        text_color.g as f64,
        text_color.b as f64,
    ])?;
    let context = Context {
        state,
        factory,
        text_format,
        render_target,
        stroke_style,
        animation_manager,
        animation_timer,
        transition_library,
        background_color_variable,
        border_color_variable,
        text_color_variable,
        mouse_within: false,
        mouse_clicking: false
    };
    Ok(context)
}

#[implement(IUIAnimationTimerEventHandler)]
struct AnimationTimerEventHandler {
    window: HWND,
}

impl IUIAnimationTimerEventHandler_Impl for AnimationTimerEventHandler {
    fn OnPreUpdate(&self) -> Result<()> {
        Ok(())
    }

    fn OnPostUpdate(&self) -> Result<()> {
        unsafe {
            InvalidateRect(self.window, None, false);
        }
        Ok(())
    }

    fn OnRenderingTooSlow(&self, framespersecond: u32) -> Result<()> {
        Ok(())
    }
}

unsafe fn on_paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &(*state.qt_ptr).tokens;
    let mut ps = PAINTSTRUCT::default();
    let scaling_factor = get_scaling_factor(&window);

    let width = get_width(state, scaling_factor);
    let height = get_height(state, scaling_factor);
    let corner_radius = match state.shape {
        Shape::Circular => width.min(height) / 2f32,
        Shape::Rounded => tokens.border_radius_medium,
        Shape::Square => tokens.border_radius_none,
    };

    BeginPaint(window, &mut ps);

    context.render_target.BeginDraw();

    match state.appearance {
        Appearance::Transparent => {}
        _ => {
            let rect = D2D_RECT_F {
                left: tokens.stroke_width_thin * 0.5 * scaling_factor,
                top: tokens.stroke_width_thin * 0.5 * scaling_factor,
                right: width - tokens.stroke_width_thin * 0.5 * scaling_factor,
                bottom: height - tokens.stroke_width_thin * 0.5 * scaling_factor,
            };
            let rounded_rect = D2D1_ROUNDED_RECT {
                rect,
                radiusX: corner_radius,
                radiusY: corner_radius,
            };
            let mut vector_variable = [0f64; 3];
            context
                .background_color_variable
                .GetVectorValue(&mut vector_variable)?;
            let background_color = D2D1_COLOR_F {
                r: vector_variable[0] as f32,
                g: vector_variable[1] as f32,
                b: vector_variable[2] as f32,
                a: 1.0,
            };
            let background_brush = context
                .render_target
                .CreateSolidColorBrush(&background_color, None)?;
            context
                .render_target
                .FillRoundedRectangle(&rounded_rect, &background_brush);
            context
                .border_color_variable
                .GetVectorValue(&mut vector_variable)?;
            let border_color = D2D1_COLOR_F {
                r: vector_variable[0] as f32,
                g: vector_variable[1] as f32,
                b: vector_variable[2] as f32,
                a: 1.0,
            };
            context
                .text_color_variable
                .GetVectorValue(&mut vector_variable)?;
            let border_brush = context.render_target.CreateSolidColorBrush(&border_color, None)?;
            let text_color = D2D1_COLOR_F {
                r: vector_variable[0] as f32,
                g: vector_variable[1] as f32,
                b: vector_variable[2] as f32,
                a: 1.0,
            };
            let text_brush =
                context.render_target.CreateSolidColorBrush(&text_color, None)?;
            context.render_target.DrawRoundedRectangle(
                &rounded_rect,
                &border_brush,
                tokens.stroke_width_thin * scaling_factor,
                &context.stroke_style,
            );
            let spacing = get_spacing(state, scaling_factor);
            let text_rect = D2D_RECT_F {
                left: tokens.spacing_horizontal_m + tokens.stroke_width_thin,
                top: spacing + tokens.stroke_width_thin,
                right: width - tokens.spacing_horizontal_m - tokens.stroke_width_thin,
                bottom: height - spacing - tokens.stroke_width_thin,
            };
            context.render_target.DrawText(
                state.text.as_wide(),
                &context.text_format,
                &text_rect,
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }
    context.render_target.EndDraw(None, None)?;

    EndPaint(window, &ps);
    Ok(())
}

unsafe fn change_color(context: &Context) -> Result<()> {
    let qt = &(*context.state.qt_ptr);
    let tokens = &qt.tokens;

    let background_color = if context.mouse_clicking {
        &tokens.color_neutral_background1_pressed
    } else if context.mouse_within {
        &tokens.color_neutral_background1_hover
    } else {
        &tokens.color_neutral_background1
    };
    let border_color = if context.mouse_clicking {
        &tokens.color_neutral_stroke1_pressed
    } else if context.mouse_within {
        &tokens.color_neutral_stroke1_hover
    } else {
        &tokens.color_neutral_stroke1
    };
    let text_color = if context.mouse_clicking {
        &tokens.color_neutral_foreground1_pressed
    } else if context.mouse_within {
        &tokens.color_neutral_foreground1_hover
    } else {
        &tokens.color_neutral_foreground1
    };
    let background_color_transition = context
        .transition_library
        .CreateCubicBezierLinearVectorTransition(
            tokens.duration_faster,
            &[
                background_color.r as f64,
                background_color.g as f64,
                background_color.b as f64,
            ],
            tokens.curve_easy_ease[0],
            tokens.curve_easy_ease[1],
            tokens.curve_easy_ease[2],
            tokens.curve_easy_ease[3],
        )?;
    let border_color_transition = context
        .transition_library
        .CreateCubicBezierLinearVectorTransition(
            tokens.duration_faster,
            &[
                border_color.r as f64,
                border_color.g as f64,
                border_color.b as f64,
            ],
            tokens.curve_easy_ease[0],
            tokens.curve_easy_ease[1],
            tokens.curve_easy_ease[2],
            tokens.curve_easy_ease[3],
        )?;
    let text_color_transition = context
        .transition_library
        .CreateCubicBezierLinearVectorTransition(
            tokens.duration_faster,
            &[
                text_color.r as f64,
                text_color.g as f64,
                text_color.b as f64,
            ],
            tokens.curve_easy_ease[0],
            tokens.curve_easy_ease[1],
            tokens.curve_easy_ease[2],
            tokens.curve_easy_ease[3],
        )?;

    let storyboard = context.animation_manager.CreateStoryboard()?;
    storyboard.AddTransition(
        &context.background_color_variable,
        &background_color_transition,
    )?;
    storyboard.AddTransition(
        &context.border_color_variable,
        &border_color_transition,
    )?;
    storyboard.AddTransition(
        &context.text_color_variable,
        &text_color_transition,
    )?;
    let seconds_now = context.animation_timer.GetTime()?;
    storyboard.Schedule(seconds_now, None)
}

unsafe fn on_mouse_enter(window: HWND, context: &Context) -> Result<()> {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: window,
        dwHoverTime: 0,
    };
    TrackMouseEvent(&mut tme)?;
    _ = change_color(context);
    Ok(())
}

unsafe fn on_mouse_leave(window: HWND, context: &Context) -> Result<()> {
    _ = change_color(context);
    Ok(())
}
unsafe fn on_mouse_click(window: HWND, context: &Context) -> Result<()> {
    (context.state.mouse_event.on_click)();
    _ = change_color(context);
    Ok(())
}

extern "system" fn window_proc(
    window: HWND,
    message: u32,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => unsafe {
            let cs = l_param.0 as *const CREATESTRUCTW;
            let raw = (*cs).lpCreateParams as *mut State;
            let state = Box::<State>::from_raw(raw);
            match on_create(window, *state) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    DefWindowProcW(window, message, w_param, l_param)
                }
                Err(_) => {
                    _ = DestroyWindow(window);
                    LRESULT(-1)
                }
            }
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        WM_PAINT | WM_DISPLAYCHANGE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            match on_paint(window, context) {
                Ok(_) => LRESULT(0),
                Err(_) => LRESULT(-1),
            }
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            match context.state.shape {
                Shape::Square => {
                    if !(*raw).mouse_within {
                        (*raw).mouse_within = true;
                        let _ = on_mouse_enter(window, context);
                    }
                }
                _ => {
                    let mouse_x = l_param.0 as i16 as i32;
                    let mouse_y = (l_param.0 >> 16) as i16 as i32;
                    let region = CreateRectRgn(0, 0, 0, 0);
                    GetWindowRgn(window, region);
                    if PtInRegion(region, mouse_x, mouse_y).into() {
                        if !(*raw).mouse_within {
                            (*raw).mouse_within = true;
                            let _ = on_mouse_enter(window, context);
                        }
                    } else {
                        if (*raw).mouse_within {
                            (*raw).mouse_within = false;
                            (*raw).mouse_clicking = false;
                            let _ = on_mouse_leave(window, context);
                        }
                    }
                    DeleteObject(region);
                }
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            (*raw).mouse_within = false;
            (*raw).mouse_clicking = false;
            let _ = on_mouse_leave(window, context);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            (*raw).mouse_clicking = true;
            let _ = change_color(context);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            (*raw).mouse_clicking = false;
            let _ = on_mouse_click(window, context);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
