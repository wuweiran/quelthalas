use std::mem::size_of;

use windows::core::*;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_COLOR_F, D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DeviceContext5, ID2D1Factory1, ID2D1HwndRenderTarget, ID2D1StrokeStyle,
    ID2D1SvgDocument, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
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
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::icon::Icon;
use crate::QT;
use crate::{get_scaling_factor, MouseEvent};

#[derive(Copy, Clone)]
pub enum Appearance {
    Secondary,
    Primary,
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
    icon: Option<Icon>,
    icon_position: Option<IconPosition>,
    shape: Shape,
    size: Size,
    mouse_event: MouseEvent,
}

impl State {
    fn get_min_width(&self) -> f32 {
        (match &self.size {
            Size::Small => 96,
            Size::Medium => 96,
            Size::Large => 64,
        }) as f32
    }

    fn get_line_height(&self) -> f32 {
        (match &self.size {
            Size::Small => 16,
            Size::Medium => 20,
            Size::Large => 22,
        }) as f32
    }

    fn get_spacing(&self) -> f32 {
        (match &self.size {
            Size::Small => 3,
            Size::Medium => 5,
            Size::Large => 8,
        }) as f32
    }

    unsafe fn get_horizontal_padding(&self) -> f32 {
        let tokens = &(*self.qt_ptr).tokens;
        match &self.size {
            Size::Small => tokens.spacing_horizontal_s,
            Size::Medium => tokens.spacing_horizontal_m,
            Size::Large => tokens.spacing_horizontal_m,
        }
    }

    unsafe fn get_min_height(&self) -> f32 {
        let tokens = &(*self.qt_ptr).tokens;
        self.get_line_height() + self.get_spacing() * 2f32 + tokens.stroke_width_thin * 2f32
    }

    fn get_desired_icon_size(&self) -> f32 {
        (match &self.size {
            Size::Small => 20,
            Size::Medium => 20,
            Size::Large => 24,
        }) as f32
    }

    unsafe fn get_desired_icon_spacing(&self) -> f32 {
        let tokens = &(*self.qt_ptr).tokens;
        match &self.size {
            Size::Small => tokens.spacing_horizontal_xs,
            Size::Medium => tokens.spacing_horizontal_xs,
            Size::Large => tokens.spacing_horizontal_s_nudge,
        }
    }

    fn has_icon(&self) -> bool {
        self.icon.is_some()
    }
}

struct Context {
    state: State,
    width: f32,
    height: f32,
    icon_svg: Option<ID2D1SvgDocument>,
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
        icon: Option<&Icon>,
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
                icon: icon.map(|a| *a),
                icon_position: icon_position.map(|a| *a),
                shape: *shape,
                size: *size,
                mouse_event,
            });
            let scaling_factor = get_scaling_factor(parent_window);
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (boxed.as_ref().get_min_width() * scaling_factor) as i32,
                (boxed.as_ref().get_min_height() * scaling_factor) as i32,
                *parent_window,
                None,
                *instance,
                Some(Box::<State>::into_raw(boxed) as _),
            );
            Ok(window)
        }
    }
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

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    let qt = &(*state.qt_ptr);
    let tokens = &qt.tokens;

    let direct_write_factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
    let font_size = match state.size {
        Size::Small => tokens.font_size_base200,
        Size::Medium => tokens.font_size_base300,
        Size::Large => tokens.font_size_base400,
    };
    let font_weight = match state.size {
        Size::Small => tokens.font_weight_regular,
        Size::Medium => tokens.font_weight_semibold,
        Size::Large => tokens.font_weight_semibold,
    };
    let text_format = direct_write_factory.CreateTextFormat(
        tokens.font_family_name,
        None,
        font_weight,
        DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_STRETCH_NORMAL,
        font_size,
        w!(""),
    )?;
    text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
    text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    let text_layout = direct_write_factory.CreateTextLayout(
        state.text.as_wide(),
        &text_format,
        1000f32,
        500f32,
    )?;
    let mut metrics = DWRITE_TEXT_METRICS::default();
    text_layout.GetMetrics(&mut metrics)?;

    let scaling_factor = get_scaling_factor(&window);
    let icon_and_space_width = if state.has_icon() {
        state.get_desired_icon_spacing() + state.get_desired_icon_size()
    } else {
        0f32
    };
    let horizontal_padding = state.get_horizontal_padding();
    let scaled_width = (((state.get_min_width() - 2f32 * horizontal_padding)
        .max(metrics.width + 2f32 * tokens.stroke_width_thin)
        + 2f32 * horizontal_padding
        + icon_and_space_width)
        * scaling_factor)
        .ceil() as i32;
    let scaled_height = ((state.get_line_height() * metrics.lineCount.max(1) as f32
        + state.get_spacing() * 2f32
        + tokens.stroke_width_thin * 2f32)
        * scaling_factor)
        .ceil() as i32;

    SetWindowPos(
        window,
        None,
        0,
        0,
        scaled_width,
        scaled_height,
        SWP_NOMOVE | SWP_NOZORDER,
    )?;

    let corner_diameter = match &state.shape {
        Shape::Circular => scaled_width.min(scaled_height),
        Shape::Rounded => (tokens.border_radius_medium * 2f32 * scaling_factor) as i32,
        Shape::Square => (tokens.border_radius_none * 2f32 * scaling_factor) as i32,
    };
    let region = CreateRoundRectRgn(
        0,
        0,
        scaled_width + 1,
        scaled_height + 1,
        corner_diameter,
        corner_diameter,
    );
    SetWindowRgn(window, region, TRUE);
    let factory = D2D1CreateFactory::<ID2D1Factory1>(
        D2D1_FACTORY_TYPE_SINGLE_THREADED,
        Some(&D2D1_FACTORY_OPTIONS::default()),
    )?;
    let render_target = create_render_target(
        &window,
        D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        },
        &factory,
    )?;
    let stroke_style = factory.CreateStrokeStyle(&D2D1_STROKE_STYLE_PROPERTIES::default(), None)?;
    let svg_document = match state.icon {
        None => None,
        Some(icon) => match SHCreateMemStream(Some(icon.svg.as_bytes())) {
            None => None,
            Some(svg_stream) => {
                let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
                Some(device_context5.CreateSvgDocument(
                    &svg_stream,
                    D2D_SIZE_F {
                        width: icon.size as f32,
                        height: icon.size as f32,
                    },
                )?)
            }
        },
    };

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
    let background_color = match state.appearance {
        Appearance::Primary => &tokens.color_brand_background,
        _ => &tokens.color_neutral_background1,
    };
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
    let text_color = match state.appearance {
        Appearance::Primary => &tokens.color_neutral_foreground_on_brand,
        _ => &tokens.color_neutral_foreground1,
    };
    let text_color_variable = animation_manager.CreateAnimationVectorVariable(&[
        text_color.r as f64,
        text_color.g as f64,
        text_color.b as f64,
    ])?;
    let context = Context {
        state,
        height: scaled_height as f32 / scaling_factor,
        width: scaled_width as f32 / scaling_factor,
        text_format,
        render_target,
        icon_svg: svg_document,
        stroke_style,
        animation_manager,
        animation_timer,
        transition_library,
        background_color_variable,
        border_color_variable,
        text_color_variable,
        mouse_within: false,
        mouse_clicking: false,
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

    fn OnRenderingTooSlow(&self, _frames_per_second: u32) -> Result<()> {
        Ok(())
    }
}

unsafe fn on_paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &(*state.qt_ptr).tokens;
    let mut ps = PAINTSTRUCT::default();

    let corner_radius = match state.shape {
        Shape::Circular => context.width.min(context.height) / 2f32,
        Shape::Rounded => tokens.border_radius_medium,
        Shape::Square => tokens.border_radius_none,
    };

    BeginPaint(window, &mut ps);

    context.render_target.BeginDraw();

    let rounded_rect = D2D1_ROUNDED_RECT {
        rect: D2D_RECT_F {
            left: 0f32,
            top: 0f32,
            right: context.width,
            bottom: context.height,
        },
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

    match state.appearance {
        Appearance::Primary => {}
        _ => {
            context
                .border_color_variable
                .GetVectorValue(&mut vector_variable)?;
            let border_color = D2D1_COLOR_F {
                r: vector_variable[0] as f32,
                g: vector_variable[1] as f32,
                b: vector_variable[2] as f32,
                a: 1.0,
            };
            let border_brush = context
                .render_target
                .CreateSolidColorBrush(&border_color, None)?;
            let rounded_rect = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: tokens.stroke_width_thin * 0.5,
                    top: tokens.stroke_width_thin * 0.5,
                    right: context.width - tokens.stroke_width_thin * 0.5,
                    bottom: context.height - tokens.stroke_width_thin * 0.5,
                },
                radiusX: corner_radius,
                radiusY: corner_radius,
            };
            context.render_target.DrawRoundedRectangle(
                &rounded_rect,
                &border_brush,
                tokens.stroke_width_thin,
                &context.stroke_style,
            );
        }
    }

    context
        .text_color_variable
        .GetVectorValue(&mut vector_variable)?;
    let text_color = D2D1_COLOR_F {
        r: vector_variable[0] as f32,
        g: vector_variable[1] as f32,
        b: vector_variable[2] as f32,
        a: 1.0,
    };
    let text_brush = context
        .render_target
        .CreateSolidColorBrush(&text_color, None)?;
    let spacing = state.get_spacing();
    let horizontal_padding = state.get_horizontal_padding();
    let top = spacing + tokens.stroke_width_thin;
    let left = horizontal_padding + tokens.stroke_width_thin;
    let right = context.width - horizontal_padding - tokens.stroke_width_thin;
    let bottom = context.height - spacing - tokens.stroke_width_thin;
    let text_rect = if state.has_icon() {
        let icon_and_space_width =
            state.get_desired_icon_size() + state.get_desired_icon_spacing();
        match state.icon_position.unwrap_or(IconPosition::Before) {
            IconPosition::Before => D2D_RECT_F {
                left: left + icon_and_space_width,
                top,
                right,
                bottom,
            },
            IconPosition::After => D2D_RECT_F {
                left,
                top,
                right: right - icon_and_space_width,
                bottom,
            },
        }
    } else {
        D2D_RECT_F {
            left,
            top,
            right,
            bottom,
        }
    };
    context.render_target.DrawText(
        state.text.as_wide(),
        &context.text_format,
        &text_rect,
        &text_brush,
        D2D1_DRAW_TEXT_OPTIONS_NONE,
        DWRITE_MEASURING_MODE_NATURAL,
    );

    if state.has_icon() {
        match &context.icon_svg {
            None => {}
            Some(svg) => {
                let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
                let viewport_size = svg.GetViewportSize();
                let desired_size = state.get_desired_icon_size();
                match state.icon_position.unwrap_or(IconPosition::Before) {
                    IconPosition::Before => {
                        device_context5.SetTransform(&Matrix3x2::translation(
                            left + desired_size / 2f32 - viewport_size.width / 2f32,
                            top / 2f32 + bottom / 2f32 - viewport_size.height / 2f32));
                    }
                    IconPosition::After => device_context5.SetTransform(&Matrix3x2::translation(
                        right - desired_size / 2f32 - viewport_size.width / 2f32,
                        top / 2f32 + bottom / 2f32 - viewport_size.height / 2f32,
                    )),
                }
                device_context5.DrawSvgDocument(svg);
                device_context5.SetTransform(&Matrix3x2::identity());
            }
        }
    }

    context.render_target.EndDraw(None, None)?;

    EndPaint(window, &ps);
    Ok(())
}

unsafe fn change_color(context: &Context) -> Result<()> {
    let qt = &(*context.state.qt_ptr);
    let tokens = &qt.tokens;
    let storyboard = context.animation_manager.CreateStoryboard()?;

    let appearance = &context.state.appearance;
    let background_color = if context.mouse_clicking {
        match appearance {
            Appearance::Primary => &tokens.color_brand_background_pressed,
            _ => &tokens.color_neutral_background1_pressed,
        }
    } else if context.mouse_within {
        match appearance {
            Appearance::Primary => &tokens.color_brand_background_hover,
            _ => &tokens.color_neutral_background1_hover,
        }
    } else {
        match appearance {
            Appearance::Primary => &tokens.color_brand_background,
            _ => &tokens.color_neutral_background1,
        }
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
    storyboard.AddTransition(
        &context.background_color_variable,
        &background_color_transition,
    )?;

    match appearance {
        Appearance::Primary => {}
        _ => {
            let border_color = if context.mouse_clicking {
                &tokens.color_neutral_stroke1_pressed
            } else if context.mouse_within {
                &tokens.color_neutral_stroke1_hover
            } else {
                &tokens.color_neutral_stroke1
            };
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
            storyboard.AddTransition(&context.border_color_variable, &border_color_transition)?;
        }
    }

    let text_color = match appearance {
        Appearance::Primary => &tokens.color_neutral_foreground_on_brand,
        _ => {
            if context.mouse_clicking {
                &tokens.color_neutral_foreground1_pressed
            } else if context.mouse_within {
                &tokens.color_neutral_foreground1_hover
            } else {
                &tokens.color_neutral_foreground1
            }
        }
    };
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
    storyboard.AddTransition(&context.text_color_variable, &text_color_transition)?;

    let seconds_now = context.animation_timer.GetTime()?;
    storyboard.Schedule(seconds_now, None)
}

unsafe fn on_mouse_enter(window: &HWND, context: &Context) -> Result<()> {
    let mut tme = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: *window,
        dwHoverTime: 0,
    };
    TrackMouseEvent(&mut tme)?;
    _ = change_color(context);
    Ok(())
}

unsafe fn on_mouse_leave(window: &HWND, context: &Context) -> Result<()> {
    _ = change_color(context);
    Ok(())
}

unsafe fn on_mouse_click(window: &HWND, context: &Context) -> Result<()> {
    (context.state.mouse_event.on_click)(window);
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
                        let _ = on_mouse_enter(&window, context);
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
                            let _ = on_mouse_enter(&window, context);
                        }
                    } else {
                        if (*raw).mouse_within {
                            (*raw).mouse_within = false;
                            (*raw).mouse_clicking = false;
                            let _ = on_mouse_leave(&window, context);
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
            let _ = on_mouse_leave(&window, context);
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
            let _ = on_mouse_click(&window, context);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
