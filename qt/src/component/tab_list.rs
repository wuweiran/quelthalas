use std::mem::size_of;
use std::sync::Once;

use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1Factory1,
    ID2D1HwndRenderTarget, ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_LEFT, VK_RIGHT,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Vector2;

/// WinUI TabViewItem: min-height 32, top corners rounded (OverlayCornerRadius 8),
/// header font 12.
const TAB_HEIGHT: f32 = 32.0;
const CARD_RADIUS: f32 = 8.0;
/// Outward-curving flare where the selected card meets the baseline (WinUI's
/// LeftRadiusRenderArc/RightRadiusRenderArc). It extends the card past its sides,
/// so the strip reserves this much space on the first/last tab.
const FLARE_RADIUS: f32 = 4.0;

pub struct MouseEvent {
    pub on_change: Box<dyn Fn(&HWND, usize)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_change: Box::new(|_window, _index| {}),
        }
    }
}

/// How the strip decides its width.
#[derive(Copy, Clone, PartialEq)]
pub enum WidthBehavior {
    /// Size-to-content: the strip is exactly as wide as its tabs. Good for a
    /// standalone tab group placed inline.
    Content,
    /// Fill the container: the parent layout owns the strip width (use
    /// `Stack::add_fill`), so the strip — and its bottom line — span the whole
    /// container edge to edge. The typical "page header" use.
    Fill,
}

impl Default for WidthBehavior {
    fn default() -> Self {
        WidthBehavior::Content
    }
}

pub struct Props {
    /// Tab labels. The caller keeps the strings alive (label contract).
    pub tabs: Vec<PCWSTR>,
    pub selected_index: usize,
    pub mouse_event: MouseEvent,
    /// Strip background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
    /// Selected-tab card fill. `None` uses `color_neutral_background1` (white) — a
    /// *lighter* card than the strip, per the WinUI gallery. Set it to your content
    /// background so the selected tab reads as connected to the page below.
    pub selected_background: Option<D2D1_COLOR_F>,
    /// Width behaviour: size-to-content (default) or fill the container.
    pub width_behavior: WidthBehavior,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            tabs: Vec::new(),
            selected_index: 0,
            mouse_event: MouseEvent::default(),
            background: None,
            selected_background: None,
            width_behavior: WidthBehavior::default(),
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    /// WinUI header font is 12px (base200).
    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base200
    }
    /// Horizontal padding inside each tab (WinUI header padding ≈ 8+ per side; we
    /// use spacing_horizontal_m for a comfortable label gutter).
    fn pad_x(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_m
    }
    fn height(&self) -> f32 {
        TAB_HEIGHT
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    /// Semibold format for the selected tab's label.
    bold_text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    selected: usize,
    hovered: Option<usize>,
    pressed: Option<usize>,
    /// Per-tab left edge + width (DIPs), computed in `layout`.
    tab_x: Vec<f32>,
    tab_w: Vec<f32>,
}

impl QT {
    pub fn create_tab_list(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_TAB_LIST");
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

    /// Currently selected tab index of a tab list created by `create_tab_list`.
    pub fn tab_list_selected(&self, tab_list: HWND) -> usize {
        unsafe {
            let raw = GetWindowLongPtrW(tab_list, GWLP_USERDATA) as *const Context;
            if raw.is_null() { 0 } else { (*raw).selected }
        }
    }
}

fn measure_text_width(qt: &QT, format: &IDWriteTextFormat, text: PCWSTR) -> f32 {
    unsafe {
        let Ok(layout) = qt
            .dwrite_factory
            .CreateTextLayout(text.as_wide(), format, f32::MAX, f32::MAX)
        else {
            return 0.0;
        };
        let mut metrics = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut metrics).is_ok() {
            metrics.width.ceil()
        } else {
            0.0
        }
    }
}

/// A rounded-rect with only the TOP two corners rounded (bottom square) — the
/// WinUI tab card that "connects" into the content area below.
fn top_rounded_card(
    factory: &ID2D1Factory1,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    r: f32,
) -> Result<ID2D1PathGeometry1> {
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        // Start at bottom-left, up the left side to the top-left arc.
        sink.BeginFigure(
            Vector2 {
                X: left,
                Y: bottom,
            },
            D2D1_FIGURE_BEGIN_FILLED,
        );
        sink.AddLine(Vector2 {
            X: left,
            Y: top + r,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: left + r, Y: top },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        // Across the top to the top-right arc.
        sink.AddLine(Vector2 {
            X: right - r,
            Y: top,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: right,
                Y: top + r,
            },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        // Down the right side, close along the (square) bottom.
        sink.AddLine(Vector2 {
            X: right,
            Y: bottom,
        });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geometry)
    }
}

/// The selected-tab card: top corners rounded (radius `r`), and the bottom corners
/// flare *outward* by `flare` to meet the baseline (WinUI's LeftRadiusRenderArc /
/// RightRadiusRenderArc) so the tab peels into the content line rather than ending
/// square. The card occupies `[left, right]`; the flares extend to
/// `[left-flare, right+flare]` at the very bottom.
///
/// `closed`: filled shape (true) closes along the bottom between the flare tips;
/// the border stroke (false) leaves the bottom open so no line is drawn where the
/// card meets the content — the card reads as connected to the page below.
fn selected_card(
    factory: &ID2D1Factory1,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    r: f32,
    flare: f32,
    closed: bool,
) -> Result<ID2D1PathGeometry1> {
    unsafe {
        let geometry = factory.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        // Start at the bottom-left flare tip (out past the card's left side).
        sink.BeginFigure(
            Vector2 {
                X: left - flare,
                Y: bottom,
            },
            D2D1_FIGURE_BEGIN_FILLED,
        );
        // Concave flare up to the card's left edge (curves outward at the base,
        // opposite curvature to the top corners — a browser-tab "peel").
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: left,
                Y: bottom - flare,
            },
            size: D2D_SIZE_F {
                width: flare,
                height: flare,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        // Up the left side to the top-left corner arc.
        sink.AddLine(Vector2 {
            X: left,
            Y: top + r,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 { X: left + r, Y: top },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        // Across the top to the top-right corner arc.
        sink.AddLine(Vector2 {
            X: right - r,
            Y: top,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: right,
                Y: top + r,
            },
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        // Down the right side to the bottom-right flare.
        sink.AddLine(Vector2 {
            X: right,
            Y: bottom - flare,
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: Vector2 {
                X: right + flare,
                Y: bottom,
            },
            size: D2D_SIZE_F {
                width: flare,
                height: flare,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        // Close along the bottom (flare tip to flare tip) for the fill; leave it
        // open for the border so no line is stroked where the card meets content.
        sink.EndFigure(if closed {
            D2D1_FIGURE_END_CLOSED
        } else {
            D2D1_FIGURE_END_OPEN
        });
        sink.Close()?;
        Ok(geometry)
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
    let selected = state
        .props
        .selected_index
        .min(state.props.tabs.len().saturating_sub(1));
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
        text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;

        // Semibold format for the selected tab (WinUI bolds the active tab).
        let bold_text_format = state.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_semibold,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            state.font_size(),
            w!(""),
        )?;
        bold_text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        bold_text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;

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
            bold_text_format,
            render_target,
            selected,
            hovered: None,
            pressed: None,
            tab_x: Vec::new(),
            tab_w: Vec::new(),
        })
    }
}

/// Measure tabs, cache their x/width (DIPs), size the strip, resize the target.
fn layout(window: HWND, context: &mut Context) -> Result<()> {
    let state = &context.state;
    let pad_x = state.pad_x();

    let mut tab_x = Vec::with_capacity(state.props.tabs.len());
    let mut tab_w = Vec::with_capacity(state.props.tabs.len());
    // Reserve a flare's width on the left so the first tab's bottom flare (when
    // selected) isn't clipped.
    let mut cursor = FLARE_RADIUS;
    for tab in &state.props.tabs {
        // Measure with the BOLD format so tab widths don't shift when selection
        // changes (the selected label is semibold).
        let label_w = measure_text_width(&state.qt, &context.bold_text_format, *tab);
        let w = label_w + 2.0 * pad_x;
        tab_x.push(cursor);
        tab_w.push(w);
        cursor += w; // WinUI tabs abut; a 1px separator sits between them.
    }
    // Reserve a flare's width on the right for the last tab's flare too.
    let total_w = if tab_w.is_empty() {
        1.0
    } else {
        cursor + FLARE_RADIUS
    };
    let height = state.height();

    let scaling_factor = get_scaling_factor(window);
    let scaled_height = (height * scaling_factor).ceil() as i32;
    let width_behavior = state.props.width_behavior;
    unsafe {
        let scaled_width = match width_behavior {
            // Size-to-content: the strip is exactly as wide as its tabs.
            WidthBehavior::Content => (total_w * scaling_factor).ceil() as i32,
            // Fill: the container owns the width. Keep whatever width we currently
            // have (the parent stretches us via SetWindowPos → WM_SIZE); only the
            // height is ours to set.
            WidthBehavior::Fill => {
                let mut rc = RECT::default();
                GetClientRect(window, &mut rc)?;
                rc.right.max(1)
            }
        };
        SetWindowPos(
            window,
            None,
            0,
            0,
            scaled_width.max(1),
            scaled_height,
            SWP_NOMOVE | SWP_NOZORDER,
        )?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width.max(1) as u32,
            height: scaled_height as u32,
        })?;
    }
    context.tab_x = tab_x;
    context.tab_w = tab_w;
    Ok(())
}

/// Hit-test a client x (device px) to a tab index.
fn hit_test(context: &Context, x_px: i32, scaling_factor: f32) -> Option<usize> {
    let x = x_px as f32 / scaling_factor;
    for i in 0..context.tab_x.len() {
        if x >= context.tab_x[i] && x < context.tab_x[i] + context.tab_w[i] {
            return Some(i);
        }
    }
    None
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
        let stroke = tokens.stroke_width_thin;
        let sep_inset = tokens.spacing_vertical_s; // WinUI separator margin 0,8,0,8

        let border_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_stroke2, None)?;

        // The selected card's fill/flare span along the baseline — the bottom line
        // is broken here and the card is drawn on top, so no line shows below the
        // selected tab (it reads as connected to the content).
        let has_selected = !state.props.tabs.is_empty();
        let (gap_l, gap_r) = if has_selected {
            let s = context.selected;
            let card_left = context.tab_x[s] + stroke * 0.5;
            let card_right = context.tab_x[s] + context.tab_w[s] - stroke * 0.5;
            (card_left - FLARE_RADIUS, card_right + FLARE_RADIUS)
        } else {
            (width, width)
        };

        // Bottom border line under the strip, in two segments that skip the
        // selected card's span.
        for (l, r) in [(0.0, gap_l), (gap_r, width)] {
            if r > l {
                context.render_target.FillRectangle(
                    &D2D_RECT_F {
                        left: l,
                        top: height - stroke,
                        right: r,
                        bottom: height,
                    },
                    &border_brush,
                );
            }
        }

        // Vertical separators between adjacent tabs — hidden next to the selected
        // tab (WinUI hides TabSeparator around the selection).
        for i in 0..state.props.tabs.len().saturating_sub(1) {
            if i == context.selected || i + 1 == context.selected {
                continue;
            }
            let sx = context.tab_x[i] + context.tab_w[i];
            context.render_target.FillRectangle(
                &D2D_RECT_F {
                    left: sx - stroke * 0.5,
                    top: sep_inset,
                    right: sx + stroke * 0.5,
                    bottom: height - sep_inset,
                },
                &border_brush,
            );
        }

        // Hover fills (unselected tabs) first — the selected card is drawn *after*
        // so its flares sit on top of an adjacent hover block, and the fill is inset
        // above the baseline so the bottom line stays visible under it.
        for i in 0..state.props.tabs.len() {
            if i == context.selected || context.hovered != Some(i) {
                continue;
            }
            let tx = context.tab_x[i];
            let tw = context.tab_w[i];
            let hover_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_background1_hover, None)?;
            let card = top_rounded_card(
                &state.qt.d2d_factory,
                tx + stroke * 0.5,
                stroke * 0.5,
                tx + tw - stroke * 0.5,
                height - stroke,
                CARD_RADIUS,
            )?;
            context.render_target.FillGeometry(&card, &hover_brush, None);
        }

        // Selected card: a top-rounded card whose bottom corners flare outward to
        // the baseline (the WinUI "connected card"). Drawn last of the backgrounds
        // so its flares overlap any adjacent hover block. Fill is configurable so it
        // can match the content background; the border is open at the bottom.
        if has_selected {
            let s = context.selected;
            let tx = context.tab_x[s];
            let tw = context.tab_w[s];
            let fill = state
                .props
                .selected_background
                .unwrap_or(tokens.color_neutral_background1);
            let card_fill = context.render_target.CreateSolidColorBrush(&fill, None)?;
            let fill_geo = selected_card(
                &state.qt.d2d_factory,
                tx + stroke * 0.5,
                stroke * 0.5,
                tx + tw - stroke * 0.5,
                height,
                CARD_RADIUS,
                FLARE_RADIUS,
                true,
            )?;
            context.render_target.FillGeometry(&fill_geo, &card_fill, None);
            let border_geo = selected_card(
                &state.qt.d2d_factory,
                tx + stroke * 0.5,
                stroke * 0.5,
                tx + tw - stroke * 0.5,
                height,
                CARD_RADIUS,
                FLARE_RADIUS,
                false,
            )?;
            context.render_target.DrawGeometry(
                &border_geo,
                &border_brush,
                stroke,
                &state.qt.stroke_style,
            );
        }

        // Labels — selected is bold + stronger colour (foreground1); others regular
        // + foreground2.
        for i in 0..state.props.tabs.len() {
            let tx = context.tab_x[i];
            let tw = context.tab_w[i];
            let (color, format) = if i == context.selected {
                (&tokens.color_neutral_foreground1, &context.bold_text_format)
            } else {
                (&tokens.color_neutral_foreground2, &context.text_format)
            };
            let brush = context.render_target.CreateSolidColorBrush(color, None)?;
            context.render_target.DrawText(
                state.props.tabs[i].as_wide(),
                format,
                &D2D_RECT_F {
                    left: tx,
                    top: 0.0,
                    right: tx + tw,
                    bottom: height,
                },
                &brush,
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

/// Select tab `idx`: update state, repaint, fire on_change. (No animation — WinUI
/// TabView switches instantly.)
fn select(window: HWND, context: &mut Context, idx: usize) {
    if idx == context.selected || idx >= context.state.props.tabs.len() {
        return;
    }
    context.selected = idx;
    _ = unsafe { InvalidateRect(Some(window), None, false) };
    (context.state.props.mouse_event.on_change)(&window, idx);
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
                Ok(mut context) => {
                    _ = layout(window, &mut context);
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
        WM_SIZE => unsafe {
            // The container resized us (Fill mode). Match the render target to the
            // new client size so the full-width bottom line spans edge to edge.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if raw.is_null() {
                // Fires once during WM_CREATE's initial layout, before the Context is
                // stored — layout() already sized the target then, so ignore it.
                return DefWindowProcW(window, message, w_param, l_param);
            }
            let context = &*raw;
            let width = (l_param.0 & 0xffff) as u32;
            let height = (l_param.0 >> 16) as u32;
            _ = context.render_target.Resize(&D2D_SIZE_U {
                width: width.max(1),
                height: height.max(1),
            });
            _ = InvalidateRect(Some(window), None, false);
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
            let x = l_param.0 as i16 as i32;
            let hit = hit_test(context, x, get_scaling_factor(window));
            if context.hovered != hit {
                context.hovered = hit;
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
            context.hovered = None;
            context.pressed = None;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            context.pressed = hit_test(context, l_param.0 as i16 as i32, get_scaling_factor(window));
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.pressed = None;
            if let Some(idx) = hit_test(context, l_param.0 as i16 as i32, get_scaling_factor(window))
            {
                select(window, context, idx);
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT(DLGC_WANTARROWS as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let n = context.state.props.tabs.len();
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_LEFT if context.selected > 0 => {
                    let idx = context.selected - 1;
                    select(window, context, idx);
                    LRESULT(0)
                }
                VK_RIGHT if context.selected + 1 < n => {
                    let idx = context.selected + 1;
                    select(window, context, idx);
                    LRESULT(0)
                }
                _ => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
