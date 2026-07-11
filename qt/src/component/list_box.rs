//! A single-select list of text rows — Win32 `ListBox` (`LBS_NOTIFY`), Fluent-styled.
//! A vertical column of selectable rows that scrolls when it overflows. The scroll
//! host wiring (track rect, wheel/thumb/track-repeat, cursor) is the same pattern as
//! `textarea`, delegated to the shared `scroll::VScroll` helper; the row rendering
//! (32px rows, rounded hover fill, centered text) mirrors `dropdown`'s popup list.

use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, PAINTSTRUCT, RDW_INVALIDATE,
    RedrawWindow, ScreenToClient, SetWindowRgn,
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
    GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VIRTUAL_KEY, VK_DOWN, VK_END, VK_HOME, VK_NEXT, VK_PRIOR, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::component::input;
use crate::component::option::Item;
use crate::component::scroll::{SCROLLBAR_W, ScrollHit, VScroll};
use crate::{QT, get_scaling_factor};

const REPEAT_TIMER_ID: usize = 1;
const REPEAT_INITIAL_MS: u32 = 250;
const REPEAT_INTERVAL_MS: u32 = 40;
/// Gap between the scrollbar's outer edge and the field outline (DIPs).
const SCROLLBAR_MARGIN: f32 = 2.0;
/// Width of the brand accent bar on the left of the selected row (DIPs).
const ACCENT_W: f32 = 3.0;
/// Height of the brand accent bar (DIPs), vertically centered in the row.
const ACCENT_H: f32 = 16.0;
/// Accent bar height while the row is pressed (DIPs); eases back to ACCENT_H.
const ACCENT_PRESSED_H: f32 = 10.0;

pub struct MouseEvent {
    /// Fired when the selection changes (click, double-click, or keyboard), with
    /// the newly selected item index.
    pub on_select: Box<dyn Fn(&HWND, usize)>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_select: Box::new(|_, _| {}),
        }
    }
}

pub struct Props {
    pub items: Vec<Item>,
    pub width: i32,
    pub height: i32,
    pub selected: Option<usize>,
    pub size: input::Size,
    pub mouse_event: MouseEvent,
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            items: Vec::new(),
            width: 0,
            height: 0,
            selected: None,
            size: input::Size::Medium,
            mouse_event: MouseEvent::default(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    items: Vec<Item>,
    width: f32,
    height: f32,
    size: input::Size,
    background: Option<D2D1_COLOR_F>,
    on_select: Box<dyn Fn(&HWND, usize)>,
}

impl State {
    fn row_height(&self) -> f32 {
        match self.size {
            input::Size::Small => 24.0,
            input::Size::Medium => 32.0,
            input::Size::Large => 40.0,
        }
    }
    fn font_size(&self) -> f32 {
        let tokens = &self.qt.theme.tokens;
        match self.size {
            input::Size::Small => tokens.font_size_base200,
            input::Size::Medium => tokens.font_size_base300,
            input::Size::Large => tokens.font_size_base400,
        }
    }
    /// Vertical gap between adjacent rows (DIPs).
    fn row_gap(&self) -> f32 {
        self.qt.theme.tokens.spacing_vertical_xxs
    }
    /// The per-row slot: the row plus its trailing gap.
    fn row_slot(&self) -> f32 {
        self.row_height() + self.row_gap()
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    selected: Option<usize>,
    hovered: Option<usize>,
    is_focused: bool,
    is_hovered: bool,
    scroll: VScroll,
    animation_manager: IUIAnimationManager2,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary2,
    /// Animated height of the selected row's accent bar (press-shrink → release).
    accent_height: IUIAnimationVariable2,
}

impl Context {
    /// The row area (inside the border padding), in DIPs. Rows span the full width
    /// (the scrollbar overlays them, like a modern overlay scrollbar).
    fn content_rect(&self) -> D2D_RECT_F {
        let tokens = &self.state.qt.theme.tokens;
        let hpad = tokens.spacing_horizontal_xs;
        let vpad = tokens.spacing_vertical_xs;
        D2D_RECT_F {
            left: hpad,
            top: vpad,
            right: self.state.width - hpad,
            bottom: self.state.height - vpad,
        }
    }

    /// The scrollbar track rect, in the right margin near the outline.
    fn track_rect(&self) -> D2D_RECT_F {
        let stroke = self.state.qt.theme.tokens.stroke_width_thin;
        let right = self.state.width - stroke - SCROLLBAR_MARGIN;
        D2D_RECT_F {
            left: right - SCROLLBAR_W,
            top: self.content_rect().top,
            right,
            bottom: self.content_rect().bottom,
        }
    }

    /// Row index at a client-DIP y, or None if past the last row / in a gap.
    fn row_at(&self, y: f32) -> Option<usize> {
        let c = self.content_rect();
        let rel = y - c.top + self.scroll.offset();
        if rel < 0.0 {
            return None;
        }
        let slot = self.state.row_slot();
        let i = (rel / slot) as usize;
        // Reject the gap band below each row.
        if rel - i as f32 * slot > self.state.row_height() {
            return None;
        }
        if i < self.state.items.len() { Some(i) } else { None }
    }
}

impl QT {
    pub fn create_list_box(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_LIST_BOX");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_CLASSDC | CS_DBLCLKS,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let scaling_factor = get_scaling_factor(parent_window);
            let width = if props.width > 0 { props.width as f32 } else { 240.0 };
            let height = if props.height > 0 { props.height as f32 } else { 160.0 };
            let selected = props.selected;
            let boxed = Box::new(State {
                qt: self.clone(),
                items: props.items,
                width,
                height,
                size: props.size,
                background: props.background,
                on_select: props.mouse_event.on_select,
            });
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!(""),
                WS_TABSTOP | WS_VISIBLE | WS_CHILD,
                x,
                y,
                (width * scaling_factor) as i32,
                (height * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )?;
            if selected.is_some() {
                let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Context;
                if !raw.is_null() {
                    let context = &mut *raw;
                    context.selected = selected;
                    if let Some(i) = selected {
                        ensure_row_visible(context, i);
                    }
                    _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            Ok(hwnd)
        }
    }

    /// The current selection, or None.
    pub fn list_box_selection(&self, list_box: HWND) -> Option<usize> {
        unsafe {
            let raw = GetWindowLongPtrW(list_box, GWLP_USERDATA) as *const Context;
            if raw.is_null() { None } else { (*raw).selected }
        }
    }
}

fn create_text_format(qt: &QT, font_size: f32) -> Result<IDWriteTextFormat> {
    let tokens = &qt.theme.tokens;
    unsafe {
        let format = qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            font_size,
            w!(""),
        )?;
        // Vertical centering within the row rect.
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let font_size = state.font_size();
    unsafe {
        let text_format = create_text_format(&state.qt, font_size)?;
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
        let accent_height = animation_manager.CreateAnimationVariable(ACCENT_H as f64)?;

        Ok(Context {
            state,
            text_format,
            render_target,
            selected: None,
            hovered: None,
            is_focused: false,
            is_hovered: false,
            scroll: VScroll::new(),
            animation_manager,
            animation_timer,
            transition_library,
            accent_height,
        })
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
            _ = InvalidateRect(Some(self.window), None, false);
        }
        Ok(())
    }
    fn OnRenderingTooSlow(&self, _fps: u32) -> Result<()> {
        Ok(())
    }
}

/// Animate the accent bar to `target` height. Instant when `duration` is 0.
fn animate_accent(context: &mut Context, target: f64, duration: f64) -> Result<()> {
    let curve = context.state.qt.theme.tokens.curve_easy_ease;
    unsafe {
        let transition = if duration <= 0.0 {
            context.transition_library.CreateInstantaneousTransition(target)?
        } else {
            context.transition_library.CreateCubicBezierLinearTransition(
                duration,
                target,
                curve[0],
                curve[1],
                curve[2],
                curve[3],
            )?
        };
        let seconds_now = context.animation_timer.GetTime()?;
        context.animation_manager.ScheduleTransition(
            &context.accent_height,
            &transition,
            seconds_now,
        )?;
    }
    Ok(())
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (state.width * scaling_factor).ceil() as i32;
    let scaled_height = (state.height * scaling_factor).ceil() as i32;
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
        let corner_diameter =
            (state.qt.theme.tokens.border_radius_medium * scaling_factor * 2.0) as i32;
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

fn update_metrics(context: &mut Context) {
    let slot = context.state.row_slot();
    let n = context.state.items.len() as f32;
    // n rows + (n-1) inter-row gaps = n*slot - one trailing gap.
    let content_h = (n * slot - context.state.row_gap()).max(0.0);
    let c = context.content_rect();
    let viewport_h = c.bottom - c.top;
    context.scroll.set_metrics(content_h, viewport_h, slot);
}

// --- selection / navigation (skip disabled items) ---

fn first_enabled(context: &Context) -> Option<usize> {
    context.state.items.iter().position(|it| !it.disabled)
}

fn last_enabled(context: &Context) -> Option<usize> {
    (0..context.state.items.len()).rev().find(|&i| !context.state.items[i].disabled)
}

fn next_enabled(context: &Context, from: Option<usize>) -> Option<usize> {
    let start = match from {
        Some(i) => i + 1,
        None => 0,
    };
    (start..context.state.items.len())
        .find(|&i| !context.state.items[i].disabled)
        .or(from)
}

fn prev_enabled(context: &Context, from: Option<usize>) -> Option<usize> {
    match from {
        Some(i) => (0..i).rev().find(|&j| !context.state.items[j].disabled).or(from),
        None => last_enabled(context),
    }
}

fn ensure_row_visible(context: &mut Context, i: usize) {
    let slot = context.state.row_slot();
    let rh = context.state.row_height();
    context.scroll.ensure_visible(i as f32 * slot, i as f32 * slot + rh);
}

fn select(window: HWND, context: &mut Context, i: usize) {
    if context.state.items[i].disabled {
        return;
    }
    context.selected = Some(i);
    ensure_row_visible(context, i);
    (context.state.on_select)(&window, i);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

fn paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    unsafe {
        let background = state.background.unwrap_or(tokens.color_neutral_background1);
        context.render_target.Clear(Some(&background));

        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaling_factor = get_scaling_factor(window);
        let width = rc.right as f32 / scaling_factor;
        let height = rc.bottom as f32 / scaling_factor;
        let stroke = tokens.stroke_width_thin;
        let radius = tokens.border_radius_medium;

        // Field box.
        let field_rect = D2D1_ROUNDED_RECT {
            rect: D2D_RECT_F {
                left: stroke * 0.5,
                top: stroke * 0.5,
                right: width - stroke * 0.5,
                bottom: height - stroke * 0.5,
            },
            radiusX: radius,
            radiusY: radius,
        };
        let fill_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_background1, None)?;
        context.render_target.FillRoundedRectangle(&field_rect, &fill_brush);

        // --- rows ---
        // Rows are laid out inside the padding, but they scroll through the padding
        // and clip at the *padding box* (just inside the border), like textarea — so
        // the top/bottom padding stays filled with row content while scrolling.
        let content = context.content_rect();
        let offset = context.scroll.offset();
        let rh = state.row_height();
        let slot = state.row_slot();
        let row_right = content.right;
        context.render_target.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: content.left,
                top: stroke,
                right: content.right,
                bottom: height - stroke,
            },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );

        for (i, item) in state.items.iter().enumerate() {
            let top = content.top + i as f32 * slot - offset;
            let bottom = top + rh;
            if bottom < 0.0 || top > height {
                continue; // offscreen
            }
            let is_selected = context.selected == Some(i);
            let is_hovered = context.hovered == Some(i) && !item.disabled;

            // Row background (selected wins over hover).
            let fill = if is_selected {
                Some(tokens.color_neutral_background1_selected)
            } else if is_hovered {
                Some(tokens.color_neutral_background1_hover)
            } else {
                None
            };
            if let Some(color) = fill {
                let brush = context.render_target.CreateSolidColorBrush(&color, None)?;
                context.render_target.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: content.left,
                            top,
                            right: row_right,
                            bottom,
                        },
                        radiusX: radius,
                        radiusY: radius,
                    },
                    &brush,
                );
            }

            // Brand accent bar on the left of the selected row — animated height
            // (press-shrink to 10px, eases back to 16px on release), centered.
            if is_selected {
                let accent = context
                    .render_target
                    .CreateSolidColorBrush(&tokens.color_compound_brand_stroke, None)?;
                let accent_h = context.accent_height.GetValue()? as f32;
                let bar_inset = (rh - accent_h) / 2.0;
                context.render_target.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: content.left,
                            top: top + bar_inset,
                            right: content.left + ACCENT_W,
                            bottom: bottom - bar_inset,
                        },
                        radiusX: ACCENT_W / 2.0,
                        radiusY: ACCENT_W / 2.0,
                    },
                    &accent,
                );
            }

            // Row text.
            let text_color = if item.disabled {
                &tokens.color_neutral_foreground_disabled
            } else {
                &tokens.color_neutral_foreground1
            };
            let text_brush = context.render_target.CreateSolidColorBrush(text_color, None)?;
            let text_pad = tokens.spacing_horizontal_m_nudge;
            context.render_target.DrawText(
                item.text.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: content.left + text_pad,
                    top,
                    right: row_right - text_pad,
                    bottom,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        context.render_target.PopAxisAlignedClip();

        // Scrollbar (rail at rest, expanded bar on hover).
        context
            .scroll
            .paint(&context.render_target, context.track_rect(), tokens)?;

        // Outline border.
        let border_color = if context.is_focused {
            &tokens.color_neutral_stroke1_pressed
        } else if context.is_hovered {
            &tokens.color_neutral_stroke1_hover
        } else {
            &tokens.color_neutral_stroke1
        };
        let border_brush = context.render_target.CreateSolidColorBrush(border_color, None)?;
        context.render_target.DrawRoundedRectangle(
            &field_rect,
            &border_brush,
            stroke,
            &state.qt.stroke_style,
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

fn page_rows(context: &Context) -> usize {
    let c = context.content_rect();
    (((c.bottom - c.top) / context.state.row_slot()).floor() as usize).max(1)
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
                    _ = layout(window, &context);
                    update_metrics(&mut context);
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
        WM_TIMER if w_param.0 == REPEAT_TIMER_ID => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if context.scroll.is_repeating() {
                SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INTERVAL_MS, None);
                if context.scroll.repeat_step() {
                    _ = InvalidateRect(Some(window), None, false);
                }
            } else {
                _ = KillTimer(Some(window), REPEAT_TIMER_ID);
            }
            LRESULT(0)
        },
        WM_SETFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = true;
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_KILLFOCUS => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            (*raw).is_focused = false;
            _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE);
            LRESULT(0)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            if !context.is_hovered {
                context.is_hovered = true;
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: window,
                    dwHoverTime: 0,
                };
                _ = TrackMouseEvent(&mut tme);
                _ = InvalidateRect(Some(window), None, false);
            }
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            // Expand the rail into the full bar only while over the scrollbar region.
            let t = context.track_rect();
            let over_track = px >= t.left && px <= t.right && py >= t.top && py <= t.bottom;
            if context.scroll.set_expanded(over_track) {
                _ = InvalidateRect(Some(window), None, false);
            }
            if context.scroll.is_dragging() {
                let (_, redraw) = context.scroll.on_mouse_move(px, py, context.track_rect());
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
            } else {
                let (over, redraw) = context.scroll.on_mouse_move(px, py, context.track_rect());
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
                // Row hover (only over the row area, not the scrollbar).
                let new_hover = if over { None } else { context.row_at(py) };
                if new_hover != context.hovered {
                    context.hovered = new_hover;
                    _ = InvalidateRect(Some(window), None, false);
                }
            }
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            context.is_hovered = false;
            context.hovered = None;
            _ = context.scroll.clear_hover();
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let (handled, redraw) = match context
                .scroll
                .on_l_button_down(px, py, context.track_rect())
            {
                ScrollHit::Miss => (false, false),
                ScrollHit::Thumb => (true, true),
                ScrollHit::Track | ScrollHit::Up | ScrollHit::Down => {
                    SetTimer(Some(window), REPEAT_TIMER_ID, REPEAT_INITIAL_MS, None);
                    (true, true)
                }
            };
            if handled {
                SetCapture(window);
                if redraw {
                    _ = InvalidateRect(Some(window), None, false);
                }
            } else if let Some(i) = context.row_at(py) {
                select(window, context, i);
                // Press: shrink the accent bar instantly; WM_LBUTTONUP eases it back.
                _ = animate_accent(context, ACCENT_PRESSED_H as f64, 0.0);
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let redraw = context.scroll.on_l_button_up();
            _ = KillTimer(Some(window), REPEAT_TIMER_ID);
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            // Release: ease the accent bar back to full height.
            let duration = context.state.qt.theme.tokens.duration_normal;
            _ = animate_accent(context, ACCENT_H as f64, duration);
            if redraw {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONDBLCLK => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let scaling_factor = get_scaling_factor(window);
            let px = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let py = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            // Only over the row area (not the scrollbar) — re-fire select (activate).
            if context.scroll.on_l_button_down(px, py, context.track_rect()) == ScrollHit::Miss {
                if let Some(i) = context.row_at(py) {
                    select(window, context, i);
                    _ = animate_accent(context, ACCENT_PRESSED_H as f64, 0.0);
                }
            }
            LRESULT(0)
        },
        WM_MOUSEWHEEL => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let delta = (w_param.0 >> 16) as i16 as i32;
            if context.scroll.on_wheel(delta) {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT(DLGC_WANTARROWS as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_UP => {
                    if let Some(i) = prev_enabled(context, context.selected) {
                        select(window, context, i);
                    }
                }
                VK_DOWN => {
                    if let Some(i) = next_enabled(context, context.selected) {
                        select(window, context, i);
                    }
                }
                VK_HOME => {
                    if let Some(i) = first_enabled(context) {
                        select(window, context, i);
                    }
                }
                VK_END => {
                    if let Some(i) = last_enabled(context) {
                        select(window, context, i);
                    }
                }
                VK_PRIOR => {
                    let page = page_rows(context);
                    let from = context.selected.unwrap_or(0);
                    let target = from.saturating_sub(page);
                    // Walk to the nearest enabled at/after target.
                    let i = (target..context.state.items.len())
                        .find(|&j| !context.state.items[j].disabled)
                        .or_else(|| first_enabled(context));
                    if let Some(i) = i {
                        select(window, context, i);
                    }
                }
                VK_NEXT => {
                    let page = page_rows(context);
                    let n = context.state.items.len();
                    let from = context.selected.unwrap_or(0);
                    let target = (from + page).min(n.saturating_sub(1));
                    let i = (0..=target).rev().find(|&j| !context.state.items[j].disabled);
                    if let Some(i) = i {
                        select(window, context, i);
                    }
                }
                _ => return DefWindowProcW(window, message, w_param, l_param),
            }
            LRESULT(0)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = GetDpiForWindow(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            update_metrics(context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
