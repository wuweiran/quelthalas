use std::mem::size_of;

use crate::QT;
use crate::get_scaling_factor;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use crate::sys::dpi_for_window;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

/// Font size + line height, mapped to the theme typography ramp.
#[derive(Copy, Clone)]
pub enum Size {
    Base200,
    Base300,
    Base400,
    Base500,
    Base600,
    Base700,
    Base800,
}

#[derive(Copy, Clone)]
pub enum Weight {
    Regular,
    Semibold,
}

pub struct Props {
    pub text: PCWSTR,
    pub size: Size,
    pub weight: Weight,
    pub italic: bool,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            text: w!(""),
            size: Size::Base300,
            weight: Weight::Regular,
            italic: false,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

/// Props for a typography preset (`create_body1`, `create_title1`, …). Size and
/// weight are fixed by the preset; the rest of the base `Text` props still apply.
pub struct PresetProps {
    pub text: PCWSTR,
    pub italic: bool,
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for PresetProps {
    fn default() -> Self {
        PresetProps {
            text: w!(""),
            italic: false,
            background: None,
        }
    }
}

impl State {
    fn font_size(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.props.size {
            Size::Base200 => tokens.font_size_base200,
            Size::Base300 => tokens.font_size_base300,
            Size::Base400 => tokens.font_size_base400,
            Size::Base500 => tokens.font_size_base500,
            Size::Base600 => tokens.font_size_base600,
            Size::Base700 => tokens.font_size_base700,
            Size::Base800 => tokens.font_size_base800,
        }
    }

    fn line_height(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.props.size {
            Size::Base200 => tokens.line_height_base200,
            Size::Base300 => tokens.line_height_base300,
            Size::Base400 => tokens.line_height_base400,
            Size::Base500 => tokens.line_height_base500,
            Size::Base600 => tokens.line_height_base600,
            Size::Base700 => tokens.line_height_base700,
            Size::Base800 => tokens.line_height_base800,
        }
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
}

impl QT {
    pub fn create_text(&self, parent_window: HWND, x: i32, y: i32, props: Props) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_TEXT");
        unsafe {
            static REGISTER: std::sync::Once = std::sync::Once::new();
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

/// Generates a typography-preset constructor that delegates to `create_text`
/// with a fixed size + weight.
macro_rules! text_preset {
    ($name:ident, $size:expr, $weight:expr) => {
        impl QT {
            pub fn $name(
                &self,
                parent_window: HWND,
                x: i32,
                y: i32,
                props: PresetProps,
            ) -> Result<HWND> {
                self.create_text(
                    parent_window,
                    x,
                    y,
                    Props {
                        text: props.text,
                        size: $size,
                        weight: $weight,
                        italic: props.italic,
                        background: props.background,
                    },
                )
            }
        }
    };
}

text_preset!(create_caption1, Size::Base200, Weight::Regular);
text_preset!(create_caption1_strong, Size::Base200, Weight::Semibold);
text_preset!(create_body1, Size::Base300, Weight::Regular);
text_preset!(create_body1_strong, Size::Base300, Weight::Semibold);
text_preset!(create_body2, Size::Base400, Weight::Regular);
text_preset!(create_subtitle2, Size::Base400, Weight::Semibold);
text_preset!(create_subtitle1, Size::Base500, Weight::Semibold);
text_preset!(create_title3, Size::Base600, Weight::Semibold);
text_preset!(create_title2, Size::Base700, Weight::Semibold);
text_preset!(create_title1, Size::Base800, Weight::Semibold);


fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let direct_write_factory = &state.qt.dwrite_factory;
        let font_weight = match state.props.weight {
            Weight::Regular => tokens.font_weight_regular,
            Weight::Semibold => tokens.font_weight_semibold,
        };
        let font_style = if state.props.italic {
            DWRITE_FONT_STYLE_ITALIC
        } else {
            DWRITE_FONT_STYLE_NORMAL
        };
        let text_format = direct_write_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            font_weight,
            font_style,
            DWRITE_FONT_STRETCH_NORMAL,
            state.font_size(),
            w!(""),
        )?;

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
        Ok(Context {
            state,
            text_format,
            render_target,
        })
    }
}

/// Auto-size the window to the measured text (single line) and resize the target.
fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    unsafe {
        let direct_write_factory = &state.qt.dwrite_factory;
        let text_layout = direct_write_factory.CreateTextLayout(
            state.props.text.as_wide(),
            &context.text_format,
            f32::MAX,
            f32::MAX,
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        text_layout.GetMetrics(&mut metrics)?;

        let scaling_factor = get_scaling_factor(window);
        let width = metrics.width.ceil().max(1.0);
        let height = state.line_height().max(metrics.height.ceil());
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

        let mut rect = RECT::default();
        GetClientRect(window, &mut rect)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rect.right as f32 / scaling_factor;
        let height = rect.bottom as f32 / scaling_factor;

        let brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
        context.render_target.DrawText(
            state.props.text.as_wide(),
            &context.text_format,
            &D2D_RECT_F {
                left: 0f32,
                top: 0f32,
                right: width,
                bottom: height,
            },
            &brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
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
            let context = &*raw;
            _ = layout(window, context);
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
