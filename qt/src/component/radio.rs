use std::mem::size_of;
use std::sync::Once;

use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

/// Private message a selected radio sends to its group siblings to clear them.
/// Filtered to `QT_RADIO` windows so it can't be misread by other WM_USER-based
/// controls that happen to share the group.
const WM_RADIO_UNCHECK: u32 = WM_USER + 1;

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
    /// Set `true` on the FIRST radio of each group. It ORs in `WS_GROUP`, which
    /// delimits the group in z-order — exactly how Win32 dialogs group radios.
    pub group_start: bool,
    pub mouse_event: MouseEvent,
    /// Background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            label: w!(""),
            checked: false,
            group_start: false,
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
    /// Fluent's radio is a fixed 16px indicator (no medium/large variants).
    fn box_size(&self) -> f32 {
        16.0
    }

    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base300
    }

    fn line_height(&self) -> f32 {
        self.qt.theme.tokens.line_height_base300
    }

    /// Padding around the ring (Fluent's indicator margin) and trailing the label
    /// — `spacingHorizontalS`.
    fn pad(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_s
    }

    /// Space between the ring's padding and the label text — `spacingHorizontalXS`.
    fn gap(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_xs
    }

    /// Vertical padding above and below the ring — `spacingVerticalS`. Gives the
    /// Fluent 32px row (8 + 16 ring + 8).
    fn vpad(&self) -> f32 {
        self.qt.theme.tokens.spacing_vertical_s
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    checked: bool,
    hovered: bool,
    pressed: bool,
}

impl QT {
    pub fn create_radio(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_RADIO");
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
            let mut style = WS_TABSTOP | WS_VISIBLE | WS_CHILD;
            if props.group_start {
                style |= WS_GROUP;
            }
            let boxed = Box::new(State {
                qt: self.clone(),
                props,
            });
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                style,
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

    /// Current checked state of a radio created by `create_radio`.
    pub fn radio_checked(&self, radio: HWND) -> bool {
        unsafe {
            let raw = GetWindowLongPtrW(radio, GWLP_USERDATA) as *const Context;
            if raw.is_null() {
                false
            } else {
                (*raw).checked
            }
        }
    }
}

/// True when `hwnd` is one of our radio windows. Used to bound the group walk so
/// only radios are toggled, even if `WS_GROUP` boundaries are imperfect.
fn is_radio(hwnd: HWND) -> bool {
    unsafe {
        let mut buf = [0u16; 16];
        let len = GetClassNameW(hwnd, &mut buf);
        len > 0 && String::from_utf16_lossy(&buf[..len as usize]) == "QT_RADIO"
    }
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

        Ok(Context {
            state,
            text_format,
            render_target,
            checked,
            hovered: false,
            pressed: false,
        })
    }
}

/// Auto-size to the ring + gap + label and resize the render target.
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
        // `spacingHorizontalS` padding around the ring on every side; the label
        // adds `spacingHorizontalXS` before its text and `spacingHorizontalS` after.
        let width = if has_label {
            state.pad() + state.box_size() + state.pad() + state.gap() + metrics.width.ceil()
                + state.pad()
        } else {
            state.pad() + state.box_size() + state.pad()
        };
        // Fluent 32px row: `spacingVerticalS` above and below the ring (8 + 16 + 8).
        // `.max(line_height)` guards against clipping if the label is ever taller.
        let height = (state.box_size() + state.vpad() * 2.0).max(state.line_height());
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
        let center = Vector2 {
            X: box_left + box_size / 2.0,
            Y: box_top + box_size / 2.0,
        };

        // Resolve colours by state (per Fluent's radio rest/hover/pressed).
        let ring_color = if context.checked {
            if context.pressed {
                &tokens.color_compound_brand_stroke_pressed
            } else if context.hovered {
                &tokens.color_compound_brand_stroke_hover
            } else {
                &tokens.color_compound_brand_stroke
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

        // Ring: 1px stroke inset so it sits inside the 16px box.
        let ring_brush = context.render_target.CreateSolidColorBrush(ring_color, None)?;
        let ring = D2D1_ELLIPSE {
            point: center,
            radiusX: box_size / 2.0 - tokens.stroke_width_thin * 0.5,
            radiusY: box_size / 2.0 - tokens.stroke_width_thin * 0.5,
        };
        context.render_target.DrawEllipse(
            &ring,
            &ring_brush,
            tokens.stroke_width_thin,
            &state.qt.stroke_style,
        );
        // Checked: concentric dot at Fluent's `scale(0.625)` (10px of the 16px box),
        // same brand colour as the ring.
        if context.checked {
            let dot_radius = box_size * 0.625 / 2.0;
            let dot = D2D1_ELLIPSE {
                point: center,
                radiusX: dot_radius,
                radiusY: dot_radius,
            };
            context.render_target.FillEllipse(&dot, &ring_brush);
        }

        // Label to the right of the ring, vertically centred.
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

/// Uncheck every other radio in this radio's `WS_GROUP` group. Walks the group
/// with `GetNextDlgGroupItem` (the primitive `BS_AUTORADIOBUTTON` uses), sending
/// each radio sibling the private uncheck message. Only `QT_RADIO` windows are
/// touched, so imperfect group boundaries stay harmless.
fn uncheck_group_siblings(window: HWND) {
    unsafe {
        let Ok(parent) = GetParent(window) else {
            return;
        };
        let mut cur = window;
        // The group cycles and wraps back to `window`; the cap is a defensive
        // backstop against any pathological non-terminating walk.
        for _ in 0..256 {
            let next = match GetNextDlgGroupItem(parent, Some(cur), false) {
                Ok(h) => h,
                Err(_) => break,
            };
            if next == window || next == cur {
                break;
            }
            if is_radio(next) {
                SendMessageW(next, WM_RADIO_UNCHECK, None, None);
            }
            cur = next;
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
            // A radio can only be selected, not toggled off by clicking itself.
            if context.checked {
                _ = InvalidateRect(Some(window), None, false);
            } else {
                context.checked = true;
                _ = InvalidateRect(Some(window), None, false);
                uncheck_group_siblings(window);
                (context.state.props.mouse_event.on_change)(&window, true);
            }
            LRESULT(0)
        },
        WM_RADIO_UNCHECK => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if raw.is_null() {
                return LRESULT(0);
            }
            let context = &mut *raw;
            if context.checked {
                context.checked = false;
                _ = InvalidateRect(Some(window), None, false);
                (context.state.props.mouse_event.on_change)(&window, false);
            }
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
