//! A divider — Fluent `Divider`: a thin horizontal rule that separates sections,
//! with an optional inline label aligned start / center / end. An `Appearance`
//! (Default / Subtle / Brand / Strong) tints the label *and* the line together.
//! Non-interactive; modeled on `text` (auto-height, no animation), with the rule
//! drawn via `DrawLine` (like `split_button`'s divider).

use std::mem::size_of;
use std::sync::Once;

use crate::QT;
use crate::get_scaling_factor;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use crate::sys::dpi_for_window;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

#[derive(Copy, Clone)]
pub enum Appearance {
    Default,
    Subtle,
    Brand,
    Strong,
}

#[derive(Copy, Clone)]
pub enum Alignment {
    Start,
    Center,
    End,
}

pub struct Props {
    /// Optional inline label. `None` draws a plain full-width rule.
    pub label: Option<PCWSTR>,
    /// Label position along the rule (ignored when `label` is `None`).
    pub alignment: Alignment,
    pub appearance: Appearance,
    /// The rule spans this width (DIPs).
    pub width: i32,
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            label: None,
            alignment: Alignment::Center,
            appearance: Appearance::Default,
            width: 0,
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    /// The label colour for the appearance.
    fn label_color(&self) -> D2D1_COLOR_F {
        let tokens = &self.qt.theme.tokens;
        match self.props.appearance {
            Appearance::Default => tokens.color_neutral_foreground2,
            Appearance::Subtle => tokens.color_neutral_foreground3,
            Appearance::Brand => tokens.color_brand_foreground1,
            Appearance::Strong => tokens.color_neutral_foreground1,
        }
    }
    /// The line (rule) colour for the appearance.
    fn line_color(&self) -> D2D1_COLOR_F {
        let tokens = &self.qt.theme.tokens;
        match self.props.appearance {
            Appearance::Default => tokens.color_neutral_stroke2,
            Appearance::Subtle => tokens.color_neutral_stroke3,
            Appearance::Brand => tokens.color_brand_stroke1,
            Appearance::Strong => tokens.color_neutral_stroke1,
        }
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
}

impl QT {
    pub fn create_divider(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_DIVIDER");
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
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )
        }
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base200,
            w!(""),
        )?;
        text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;

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
        Ok(Context {
            state,
            text_format,
            render_target,
        })
    }
}

fn width(state: &State) -> f32 {
    if state.props.width > 0 { state.props.width as f32 } else { 240.0 }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    let scaling_factor = get_scaling_factor(window);
    let w = width(state);
    let h = tokens.line_height_base200;
    let scaled_width = (w * scaling_factor).ceil() as i32;
    let scaled_height = (h * scaling_factor).ceil() as i32;
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

/// Measured width of the label (DIPs).
fn label_width(context: &Context, label: PCWSTR) -> f32 {
    unsafe {
        let Ok(layout) = context.state.qt.dwrite_factory.CreateTextLayout(
            label.as_wide(),
            &context.text_format,
            f32::MAX,
            f32::MAX,
        ) else {
            return 0.0;
        };
        let mut m = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut m).is_ok() {
            m.width.ceil()
        } else {
            0.0
        }
    }
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
        let w = rc.right as f32 / scaling_factor;
        let h = rc.bottom as f32 / scaling_factor;
        let cy = h / 2.0;
        let stroke = tokens.stroke_width_thin;

        let brush = context
            .render_target
            .CreateSolidColorBrush(&state.line_color(), None)?;

        let line = |x0: f32, x1: f32| {
            if x1 > x0 {
                context.render_target.DrawLine(
                    Vector2 { X: x0, Y: cy },
                    Vector2 { X: x1, Y: cy },
                    &brush,
                    stroke,
                    &state.qt.stroke_style,
                );
            }
        };

        match state.props.label {
            None => line(0.0, w),
            Some(label) if label.is_null() || label.as_wide().is_empty() => line(0.0, w),
            Some(label) => {
                let lw = label_width(context, label);
                let gap = tokens.spacing_horizontal_m; // 12px between line and text
                let inset = tokens.spacing_horizontal_s; // 8px short segment on the aligned side
                // Label x-range by alignment.
                let label_left = match state.props.alignment {
                    Alignment::Start => inset + gap,
                    Alignment::Center => (w - lw) / 2.0,
                    Alignment::End => (w - inset - gap - lw).max(0.0),
                };
                let label_right = label_left + lw;

                // Line segments left and right of the label.
                line(0.0, label_left - gap);
                line(label_right + gap, w);

                // Label, vertically centered in the full height.
                let text_brush = context
                    .render_target
                    .CreateSolidColorBrush(&state.label_color(), None)?;
                context.render_target.DrawText(
                    label.as_wide(),
                    &context.text_format,
                    &D2D_RECT_F {
                        left: label_left,
                        top: 0.0,
                        right: label_right,
                        bottom: h,
                    },
                    &text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
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
