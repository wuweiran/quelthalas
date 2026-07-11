//! A split button — Win32 `BS_SPLITBUTTON` / Fluent `SplitButton`. One pill split
//! into two independently-shaded halves by a full-height divider: the left (action)
//! zone fires the primary `on_click`; the right (menu) zone, marked by a chevron,
//! opens a right-aligned dropdown. Each zone hovers/presses on its own — it reads as
//! two buttons. Chrome + colour animation follow `button`; the dropdown reuses
//! `menu`'s `open_menu_right_aligned` (which posts `WM_COMMAND(command_id)`).

use std::mem::size_of;
use std::sync::Once;

use crate::component::button;
use crate::component::menu::{self, MenuInfo};
use crate::icon::Icon;
use crate::{MouseEvent, QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR,
    ID2D1DeviceContext5, ID2D1HwndRenderTarget, ID2D1StrokeStyle, ID2D1SvgAttribute,
    ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, PAINTSTRUCT, SetWindowRgn,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Animation::{
    IUIAnimationManager2, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionLibrary2, IUIAnimationVariable2, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE,
    UIAnimationManager2, UIAnimationTimer,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_DOWN, VK_MENU,
    VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

/// Chevron glyph size (DIPs) — the 12px Fluent chevron.
const CHEVRON_SIZE: f32 = 12.0;

/// Which half of the split button a pointer / press is in.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Zone {
    None,
    Action,
    Menu,
}

pub struct Props {
    pub text: PCWSTR,
    pub appearance: button::Appearance,
    pub size: button::Size,
    pub menu_list: Vec<MenuInfo>,
    pub mouse_event: MouseEvent,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            text: w!(""),
            appearance: button::Appearance::Secondary,
            size: button::Size::Medium,
            menu_list: Vec::new(),
            mouse_event: MouseEvent::default(),
        }
    }
}

struct State {
    qt: QT,
    parent: HWND,
    text: PCWSTR,
    appearance: button::Appearance,
    size: button::Size,
    menu_list: Vec<MenuInfo>,
    mouse_event: MouseEvent,
}

impl State {
    fn line_height(&self) -> f32 {
        match self.size {
            button::Size::Small => 16.0,
            button::Size::Medium => 20.0,
            button::Size::Large => 22.0,
        }
    }
    fn spacing(&self) -> f32 {
        match self.size {
            button::Size::Small => 3.0,
            button::Size::Medium => 5.0,
            button::Size::Large => 8.0,
        }
    }
    fn horizontal_padding(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            button::Size::Small => tokens.spacing_horizontal_s,
            button::Size::Medium => tokens.spacing_horizontal_m,
            button::Size::Large => tokens.spacing_horizontal_m,
        }
    }
    /// Horizontal padding on each side of the chevron in the menu zone.
    fn menu_padding(&self) -> f32 {
        match self.size {
            button::Size::Small => 1.0,
            button::Size::Medium => 5.0,
            button::Size::Large => 7.0,
        }
    }
    fn min_height(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        self.line_height() + self.spacing() * 2.0 + tokens.stroke_width_thin * 2.0
    }
    fn min_width(&self) -> f32 {
        match self.size {
            button::Size::Large => 64.0,
            _ => 96.0,
        }
    }
    fn menu_zone_width(&self) -> f32 {
        CHEVRON_SIZE + self.menu_padding() * 2.0
    }
    fn font_size(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            button::Size::Small => tokens.font_size_base200,
            button::Size::Medium => tokens.font_size_base300,
            button::Size::Large => tokens.font_size_base400,
        }
    }
    fn font_weight(&self) -> windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            button::Size::Small => tokens.font_weight_regular,
            _ => tokens.font_weight_semibold,
        }
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    stroke_style: ID2D1StrokeStyle,
    chevron_svg: Option<ID2D1SvgDocument>,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    action_bg: IUIAnimationVariable2,
    menu_bg: IUIAnimationVariable2,
    border_color: IUIAnimationVariable2,
    text_color: IUIAnimationVariable2,
    hovered_zone: Zone,
    pressed_zone: Zone,
}

impl QT {
    pub fn create_split_button(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_SPLIT_BUTTON");
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
                parent: parent_window,
                text: props.text,
                appearance: props.appearance,
                size: props.size,
                menu_list: props.menu_list,
                mouse_event: props.mouse_event,
            });
            let init_w = boxed.as_ref().min_width() + boxed.as_ref().menu_zone_width();
            let init_h = boxed.as_ref().min_height();
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (init_w * scaling_factor) as i32,
                (init_h * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }
}

fn set_svg_color(svg: &ID2D1SvgDocument, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let paint = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(color), w!(""))?;
        svg.GetRoot()?
            .GetFirstChild()?
            .SetAttributeValue(w!("fill"), &paint.cast::<ID2D1SvgAttribute>()?)?;
    }
    Ok(())
}

fn glyph_color(state: &State) -> D2D1_COLOR_F {
    let tokens = &state.qt.theme.tokens;
    match state.appearance {
        button::Appearance::Primary => tokens.color_neutral_foreground_on_brand,
        _ => tokens.color_neutral_foreground1,
    }
}

fn base_bg(state: &State) -> D2D1_COLOR_F {
    let tokens = &state.qt.theme.tokens;
    match state.appearance {
        button::Appearance::Primary => tokens.color_brand_background,
        _ => tokens.color_neutral_background1,
    }
}

fn vec3(c: &D2D1_COLOR_F) -> [f64; 3] {
    [c.r as f64, c.g as f64, c.b as f64]
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            state.font_weight(),
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            state.font_size(),
            w!(""),
        )?;
        text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;

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
        let stroke_style = state.qt.stroke_style.clone();

        // Chevron SVG (20px down chevron, scaled to CHEVRON_SIZE at paint).
        let icon = Icon::chevron_down_20_regular();
        let chevron_svg = match SHCreateMemStream(Some(icon.svg.as_bytes())) {
            None => None,
            Some(stream) => {
                let dc5 = render_target.cast::<ID2D1DeviceContext5>()?;
                let svg = dc5.CreateSvgDocument(
                    &stream,
                    D2D_SIZE_F { width: icon.size as f32, height: icon.size as f32 },
                )?;
                _ = set_svg_color(&svg, &glyph_color(&state));
                Some(svg)
            }
        };

        let animation_timer: IUIAnimationTimer =
            CoCreateInstance(&UIAnimationTimer, None, CLSCTX_INPROC_SERVER)?;
        let transition_library = state.qt.transition_library.clone();
        let animation_manager: IUIAnimationManager2 =
            CoCreateInstance(&UIAnimationManager2, None, CLSCTX_INPROC_SERVER)?;
        let timer_update_handler = animation_manager.cast::<IUIAnimationTimerUpdateHandler>()?;
        animation_timer
            .SetTimerUpdateHandler(&timer_update_handler, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE)?;
        let timer_event_handler: IUIAnimationTimerEventHandler =
            AnimationTimerEventHandler { window }.into();
        animation_timer.SetTimerEventHandler(&timer_event_handler)?;

        let bg = base_bg(&state);
        let action_bg = animation_manager.CreateAnimationVectorVariable(&vec3(&bg))?;
        let menu_bg = animation_manager.CreateAnimationVectorVariable(&vec3(&bg))?;
        let border_color =
            animation_manager.CreateAnimationVectorVariable(&vec3(&tokens.color_neutral_stroke1))?;
        let text_color = animation_manager.CreateAnimationVectorVariable(&vec3(&glyph_color(&state)))?;

        Ok(Context {
            state,
            text_format,
            render_target,
            stroke_style,
            chevron_svg,
            animation_manager,
            animation_timer,
            transition_library,
            action_bg,
            menu_bg,
            border_color,
            text_color,
            hovered_zone: Zone::None,
            pressed_zone: Zone::None,
        })
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let text_layout = state.qt.dwrite_factory.CreateTextLayout(
            state.text.as_wide(),
            &context.text_format,
            1000.0,
            500.0,
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        text_layout.GetMetrics(&mut metrics)?;

        let scaling_factor = get_scaling_factor(window);
        let action_w = state.min_width().max(
            metrics.width + 2.0 * tokens.stroke_width_thin + 2.0 * state.horizontal_padding(),
        );
        let width = action_w + state.menu_zone_width();
        let height = state.min_height();
        let scaled_width = (width * scaling_factor).ceil() as i32;
        let scaled_height = (height * scaling_factor).ceil() as i32;

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
        let corner_diameter = (tokens.border_radius_medium * 2.0 * scaling_factor) as i32;
        let region = CreateRoundRectRgn(
            0,
            0,
            scaled_width + 1,
            scaled_height + 1,
            corner_diameter,
            corner_diameter,
        );
        SetWindowRgn(window, Some(region), true);
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
        }
        Ok(())
    }
    fn OnRenderingTooSlow(&self, _fps: u32) -> Result<()> {
        Ok(())
    }
}

/// The action-zone width (DIPs) = client width minus the menu zone.
fn divider_x(window: HWND, context: &Context) -> f32 {
    let scaling_factor = get_scaling_factor(window);
    let mut rc = RECT::default();
    unsafe {
        _ = GetClientRect(window, &mut rc);
    }
    rc.right as f32 / scaling_factor - context.state.menu_zone_width()
}

/// Which zone a client-DIP x is in.
fn zone_at(window: HWND, context: &Context, x: f32) -> Zone {
    if x >= divider_x(window, context) {
        Zone::Menu
    } else {
        Zone::Action
    }
}

fn read_vec(v: &IUIAnimationVariable2) -> D2D1_COLOR_F {
    let mut c = [0f64; 3];
    unsafe {
        _ = v.GetVectorValue(&mut c);
    }
    D2D1_COLOR_F { r: c[0] as f32, g: c[1] as f32, b: c[2] as f32, a: 1.0 }
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let radius = tokens.border_radius_medium;
        let split_x = width - state.menu_zone_width();

        let full_pill = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F { left: 0.0, top: 0.0, right: width, bottom: height },
            radiusX: radius,
            radiusY: radius,
        };

        // Two independently-shaded zones: clip to each half, fill the whole pill in
        // that zone's colour (the axis-aligned clip cuts it at the divider; the
        // rounded corners stay antialiased).
        let action_color = read_vec(&context.action_bg);
        let menu_color = read_vec(&context.menu_bg);
        let action_brush = context.render_target.CreateSolidColorBrush(&action_color, None)?;
        context.render_target.PushAxisAlignedClip(
            &D2D_RECT_F { left: 0.0, top: 0.0, right: split_x, bottom: height },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        context.render_target.FillRoundedRectangle(&full_pill, &action_brush);
        context.render_target.PopAxisAlignedClip();
        let menu_brush = context.render_target.CreateSolidColorBrush(&menu_color, None)?;
        context.render_target.PushAxisAlignedClip(
            &D2D_RECT_F { left: split_x, top: 0.0, right: width, bottom: height },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );
        context.render_target.FillRoundedRectangle(&full_pill, &menu_brush);
        context.render_target.PopAxisAlignedClip();

        // Full-height divider between the two zones.
        let divider_color = match state.appearance {
            button::Appearance::Primary => tokens.color_neutral_stroke_on_brand,
            _ => tokens.color_neutral_stroke1,
        };
        let divider_brush = context.render_target.CreateSolidColorBrush(&divider_color, None)?;
        context.render_target.DrawLine(
            Vector2 { X: split_x, Y: 0.0 },
            Vector2 { X: split_x, Y: height },
            &divider_brush,
            tokens.stroke_width_thin,
            &context.stroke_style,
        );

        // Border (Secondary only, animated on the whole pill).
        if let button::Appearance::Secondary = state.appearance {
            let bc = read_vec(&context.border_color);
            let border_brush = context.render_target.CreateSolidColorBrush(&bc, None)?;
            let inset = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: tokens.stroke_width_thin * 0.5,
                    top: tokens.stroke_width_thin * 0.5,
                    right: width - tokens.stroke_width_thin * 0.5,
                    bottom: height - tokens.stroke_width_thin * 0.5,
                },
                radiusX: radius,
                radiusY: radius,
            };
            context.render_target.DrawRoundedRectangle(
                &inset,
                &border_brush,
                tokens.stroke_width_thin,
                &context.stroke_style,
            );
        }

        // Text in the action zone [0, split_x].
        let tc = read_vec(&context.text_color);
        let text_brush = context.render_target.CreateSolidColorBrush(&tc, None)?;
        let pad = state.horizontal_padding();
        context.render_target.DrawText(
            state.text.as_wide(),
            &context.text_format,
            &D2D_RECT_F { left: pad, top: 0.0, right: split_x - pad, bottom: height },
            &text_brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );

        // Chevron centered in the menu zone — 20px glyph scaled to CHEVRON_SIZE.
        if let Some(svg) = &context.chevron_svg {
            let dc5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
            let vp = svg.GetViewportSize();
            let s = CHEVRON_SIZE / vp.width;
            let gx = split_x + (state.menu_zone_width() - CHEVRON_SIZE) / 2.0;
            let gy = height / 2.0 - CHEVRON_SIZE / 2.0;
            dc5.SetTransform(&Matrix3x2 {
                M11: s,
                M12: 0.0,
                M21: 0.0,
                M22: s,
                M31: gx,
                M32: gy,
            });
            dc5.DrawSvgDocument(svg);
            dc5.SetTransform(&Matrix3x2::identity());
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

/// The background colour a zone should show for its current hover/press state.
fn zone_bg(context: &Context, zone: Zone) -> D2D1_COLOR_F {
    let tokens = &context.state.qt.theme.tokens;
    let pressed = context.pressed_zone == zone;
    let hovered = context.hovered_zone == zone;
    match context.state.appearance {
        button::Appearance::Primary => {
            if pressed {
                tokens.color_brand_background_pressed
            } else if hovered {
                tokens.color_brand_background_hover
            } else {
                tokens.color_brand_background
            }
        }
        _ => {
            if pressed {
                tokens.color_neutral_background1_pressed
            } else if hovered {
                tokens.color_neutral_background1_hover
            } else {
                tokens.color_neutral_background1
            }
        }
    }
}

fn change_color(context: &Context) -> Result<()> {
    let tokens = &context.state.qt.theme.tokens;
    let ease = tokens.curve_easy_ease;
    let dur = tokens.duration_faster;
    unsafe {
        let sb = context.animation_manager.CreateStoryboard()?;
        let mk = |target: &D2D1_COLOR_F| -> Result<_> {
            context.transition_library.CreateCubicBezierLinearVectorTransition(
                dur,
                &vec3(target),
                ease[0],
                ease[1],
                ease[2],
                ease[3],
            )
        };
        let a = zone_bg(context, Zone::Action);
        let m = zone_bg(context, Zone::Menu);
        sb.AddTransition(&context.action_bg, &mk(&a)?)?;
        sb.AddTransition(&context.menu_bg, &mk(&m)?)?;

        // Border + text respond to the whole pill (either zone active).
        let any_pressed = context.pressed_zone != Zone::None;
        let any_hovered = context.hovered_zone != Zone::None;
        if let button::Appearance::Secondary = context.state.appearance {
            let border = if any_pressed {
                &tokens.color_neutral_stroke1_pressed
            } else if any_hovered {
                &tokens.color_neutral_stroke1_hover
            } else {
                &tokens.color_neutral_stroke1
            };
            sb.AddTransition(&context.border_color, &mk(border)?)?;
        }
        let text = match context.state.appearance {
            button::Appearance::Primary => &tokens.color_neutral_foreground_on_brand,
            _ => {
                if any_pressed {
                    &tokens.color_neutral_foreground1_pressed
                } else if any_hovered {
                    &tokens.color_neutral_foreground1_hover
                } else {
                    &tokens.color_neutral_foreground1
                }
            }
        };
        sb.AddTransition(&context.text_color, &mk(text)?)?;

        let now = context.animation_timer.GetTime()?;
        sb.Schedule(now, None)
    }
}

fn open_dropdown(window: HWND, context: &Context) {
    let mut rc = RECT::default();
    unsafe {
        if GetWindowRect(window, &mut rc).is_err() {
            return;
        }
    }
    let qt = context.state.qt.clone();
    let parent = context.state.parent;
    let menu_list = context.state.menu_list.clone();
    // Right-aligned: the menu's right edge lines up with the button's right edge.
    _ = qt.open_menu_right_aligned(parent, rc.right, rc.bottom, menu::Props { menu_list });
}

fn fire_primary(window: &HWND, context: &Context) {
    (context.state.mouse_event.on_click)(window);
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
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            if context.hovered_zone == Zone::None {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                _ = TrackMouseEvent(&mut tme);
            }
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let zone = zone_at(window, context, x);
            if zone != (*raw).hovered_zone {
                (*raw).hovered_zone = zone;
                _ = change_color(context);
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).hovered_zone = Zone::None;
            (*raw).pressed_zone = Zone::None;
            _ = change_color(&*raw);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = SetFocus(Some(window));
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            (*raw).pressed_zone = zone_at(window, context, x);
            _ = change_color(context);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let zone = zone_at(window, context, x);
            (*raw).pressed_zone = Zone::None;
            _ = change_color(context);
            // Fire on release (matches native), routed by the release zone.
            match zone {
                Zone::Menu => open_dropdown(window, context),
                _ => fire_primary(&window, context),
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT((DLGC_WANTALLKEYS | DLGC_WANTARROWS) as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_SPACE | VK_RETURN => fire_primary(&window, context),
                VK_DOWN => open_dropdown(window, context),
                VK_MENU => {}
                _ => return DefWindowProcW(window, message, w_param, l_param),
            }
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
