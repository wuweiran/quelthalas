use std::mem::size_of;
use std::sync::Once;

use crate::icon::Icon;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR, ID2D1DeviceContext5, ID2D1HwndRenderTarget,
    ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

#[derive(Copy, Clone)]
pub enum Size {
    Medium,
    Large,
}

pub struct MouseEvent {
    pub on_change: Box<dyn Fn(&HWND, bool)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_change: Box::new(|_window, _checked| {}),
        }
    }
}

pub struct Props {
    pub label: PCWSTR,
    pub checked: bool,
    pub size: Size,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            label: w!(""),
            checked: false,
            size: Size::Medium,
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
    fn box_size(&self) -> f32 {
        match self.props.size {
            Size::Medium => 16.0,
            Size::Large => 20.0,
        }
    }

    /// Checkmark glyph size (Fluent's indicator `fontSize`), drawn at its natural
    /// size and centred — not stretched to fill the box.
    fn check_size(&self) -> f32 {
        match self.props.size {
            Size::Medium => 12.0,
            Size::Large => 16.0,
        }
    }

    fn font_size(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.props.size {
            Size::Medium => tokens.font_size_base300,
            Size::Large => tokens.font_size_base400,
        }
    }

    fn line_height(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.props.size {
            Size::Medium => tokens.line_height_base300,
            Size::Large => tokens.line_height_base400,
        }
    }

    /// Padding around the box (Fluent's indicator margin) and trailing the label
    /// — `spacingHorizontalS`.
    fn pad(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_s
    }

    /// Space between the box's padding and the label text — `spacingHorizontalXS`.
    fn gap(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_xs
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    checkmark_svg: ID2D1SvgDocument,
    checked: bool,
    hovered: bool,
    pressed: bool,
}

impl QT {
    pub fn create_checkbox(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_CHECKBOX");
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

    /// Current checked state of a checkbox created by `create_checkbox`.
    pub fn checkbox_checked(&self, checkbox: HWND) -> bool {
        unsafe {
            let raw = GetWindowLongPtrW(checkbox, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                false
            } else {
                (*raw).checked
            }
        }
    }
}

fn set_svg_color(svg: &ID2D1SvgDocument, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let svg_paint = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(color), w!(""))?;
        svg.GetRoot()?
            .GetFirstChild()?
            .SetAttributeValue(w!("fill"), &svg_paint.cast::<ID2D1SvgAttribute>()?)?;
    }
    Ok(())
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    let checked = state.props.checked;
    unsafe {
        let text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            state.font_size(),
            w!(""),
        )?;
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
                pixelSize: D2D_SIZE_U {
                    width: 0,
                    height: 0,
                },
                presentOptions: Default::default(),
            },
        )?;

        let icon = Icon::checkmark_12_filled();
        let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
        let svg_stream = SHCreateMemStream(Some(icon.svg.as_bytes()));
        let checkmark_svg = device_context5.CreateSvgDocument(
            svg_stream.as_ref(),
            D2D_SIZE_F {
                width: icon.size as f32,
                height: icon.size as f32,
            },
        )?;
        _ = set_svg_color(&checkmark_svg, &tokens.color_neutral_foreground_on_brand);

        Ok(Context {
            state,
            text_format,
            render_target,
            checkmark_svg,
            checked,
            hovered: false,
            pressed: false,
        })
    }
}

/// Auto-size to the box + gap + label and resize the render target.
fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    unsafe {
        let text_layout = state.qt.dwrite_factory.CreateTextLayout(
            state.props.label.as_wide(),
            &context.text_format,
            f32::MAX,
            f32::MAX,
        )?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        text_layout.GetMetrics(&mut metrics)?;

        let scaling_factor = get_scaling_factor(window);
        let has_label = !state.props.label.is_null() && !state.props.label.as_wide().is_empty();
        // Fluent: `spacingHorizontalS` padding around the box on every side; the
        // label adds `spacingHorizontalXS` before its text and `spacingHorizontalS`
        // after.
        let width = if has_label {
            state.pad() + state.box_size() + state.pad() + state.gap() + metrics.width.ceil()
                + state.pad()
        } else {
            state.pad() + state.box_size() + state.pad()
        };
        let height = state.box_size().max(state.line_height());
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

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let height = rc.bottom as f32 / scaling_factor;

        let box_size = state.box_size();
        let box_left = state.pad();
        let box_top = (height - box_size) / 2.0;
        let radius = tokens.border_radius_small;
        let box_rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: box_left,
                top: box_top,
                right: box_left + box_size,
                bottom: box_top + box_size,
            },
            radiusX: radius,
            radiusY: radius,
        };

        // Resolve colours by state (per Fluent's checkbox rest/hover/pressed).
        let box_color = if context.checked {
            if context.pressed {
                &tokens.color_compound_brand_background_pressed
            } else if context.hovered {
                &tokens.color_compound_brand_background_hover
            } else {
                &tokens.color_compound_brand_background
            }
        } else if context.pressed {
            &tokens.color_neutral_stroke_accessible_pressed
        } else if context.hovered {
            &tokens.color_neutral_stroke_accessible_hover
        } else {
            &tokens.color_neutral_stroke_accessible
        };
        let label_color = if context.checked {
            &tokens.color_neutral_foreground1
        } else if context.pressed {
            &tokens.color_neutral_foreground1
        } else if context.hovered {
            &tokens.color_neutral_foreground2
        } else {
            &tokens.color_neutral_foreground3
        };

        if context.checked {
            let fill = context.render_target.CreateSolidColorBrush(box_color, None)?;
            context.render_target.FillRoundedRectangle(&box_rect, &fill);
            // Checkmark at its natural glyph size (Fluent's fontSize), centred in
            // the box — not stretched to the edges.
            let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
            let viewport = context.checkmark_svg.GetViewportSize();
            let check = state.check_size();
            let scale = check / viewport.width;
            let inset = (box_size - check) / 2.0;
            device_context5.SetTransform(&Matrix3x2 {
                M11: scale,
                M12: 0.0,
                M21: 0.0,
                M22: scale,
                M31: box_left + inset,
                M32: box_top + inset,
            });
            device_context5.DrawSvgDocument(&context.checkmark_svg);
            device_context5.SetTransform(&Matrix3x2::identity());
        } else {
            let border = context.render_target.CreateSolidColorBrush(box_color, None)?;
            let inset = D2D1_ROUNDED_RECT {
                rect: D2D_RECT_F {
                    left: box_left + tokens.stroke_width_thin * 0.5,
                    top: box_top + tokens.stroke_width_thin * 0.5,
                    right: box_left + box_size - tokens.stroke_width_thin * 0.5,
                    bottom: box_top + box_size - tokens.stroke_width_thin * 0.5,
                },
                radiusX: radius,
                radiusY: radius,
            };
            context.render_target.DrawRoundedRectangle(
                &inset,
                &border,
                tokens.stroke_width_thin,
                &state.qt.stroke_style,
            );
        }

        // Label to the right of the box, vertically centred.
        let has_label = !state.props.label.is_null() && !state.props.label.as_wide().is_empty();
        if has_label {
            let text_brush = context
                .render_target
                .CreateSolidColorBrush(label_color, None)?;
            context.render_target.DrawText(
                state.props.label.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: box_left + box_size + state.pad() + state.gap(),
                    top: 0.0,
                    right: rc.right as f32 / scaling_factor,
                    bottom: height,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
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
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if !context.hovered {
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
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.hovered = false;
            context.pressed = false;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.pressed = true;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.pressed = false;
            context.checked = !context.checked;
            _ = InvalidateRect(Some(window), None, false);
            (context.state.props.mouse_event.on_change)(&window, context.checked);
            LRESULT(0)
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
