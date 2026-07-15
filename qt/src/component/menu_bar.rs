//! A classic Win32 **menu bar** (File / Edit / View / Help), Fluent-restyled.
//!
//! It is a full-width child-window strip of top-level labels. Clicking a label
//! (or Alt/F10 + arrows) opens that label's dropdown — reusing the existing
//! [`menu`](crate::component::menu) flyout via `open_menu_ex` (the in-crate
//! variant that reports why tracking ended). All *bar* logic (which label is
//! open, hover-switch, keyboard walk) lives here; the dropdown component stays
//! dropdown-only. See `run_menu_mode`.

use std::mem::size_of;
use std::sync::Once;

use crate::component::menu;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient, UpdateWindow,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use crate::sys::dpi_for_window;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_F10,
    VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

/// Menu-bar strip height (DIPs). Slimmer than the 32px tab strip.
const MENU_BAR_HEIGHT: f32 = 28.0;
/// Vertical inset of a label's highlight from the strip edges (DIPs).
const HIGHLIGHT_INSET_Y: f32 = 2.0;

/// Post this to a menu bar window to enter keyboard menu mode: highlight the first
/// label + take focus, without opening a dropdown (classic Win32 Alt/F10). The
/// sample forwards the app's clean-Alt (`SC_KEYMENU`) here.
pub const WM_ENTER_MENU_MODE: u32 = WM_APP + 1;

/// One top-level bar entry: a label plus the dropdown it opens.
pub struct MenuBarItem {
    pub text: PCWSTR,
    /// The dropdown contents, reusing the flyout menu's item type.
    pub menu_list: Vec<menu::MenuInfo>,
}

pub struct Props {
    pub items: Vec<MenuBarItem>,
    /// Strip background fill. `None` uses the theme surface (`color_neutral_background1`).
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            items: Vec::new(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    props: Props,
}

impl State {
    /// Top-level bar labels use base300 (14px), like the dropdown item text.
    fn font_size(&self) -> f32 {
        self.qt.theme.tokens.font_size_base300
    }
    fn pad_x(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_m
    }
    /// Left inset before the first label, so the bar isn't flush to the window
    /// edge — classic menu-bar breathing room.
    fn inset_x(&self) -> f32 {
        self.qt.theme.tokens.spacing_horizontal_s
    }
    fn height(&self) -> f32 {
        MENU_BAR_HEIGHT
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    hovered: Option<usize>,
    /// The label whose dropdown is currently open (drawn "pressed"), if any.
    open_index: Option<usize>,
    /// Keyboard menu mode: the highlighted label (Alt/F10 + arrows), not yet open.
    active: Option<usize>,
    /// Per-label left edge + width (DIPs), computed in `layout`.
    item_x: Vec<f32>,
    item_w: Vec<f32>,
}

impl QT {
    pub fn create_menu_bar(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_MENU_BAR");
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

    /// Select the radio item with `command_id`, unchecking its siblings (the radios
    /// sharing the same submenu list). Call this from your `WM_COMMAND` handler so
    /// the checkmark follows the selection the next time the menu opens.
    ///
    /// The bar holds its menu content, so this mutates the stored `checked` flags
    /// in place. Safe from `WM_COMMAND`: by then the modal dropdown has closed.
    pub fn menu_bar_set_radio(&self, bar: HWND, command_id: u32) {
        unsafe {
            let raw = GetWindowLongPtrW(bar, GWLP_USERDATA) as *mut Context;
            if raw.is_null() {
                return;
            }
            for item in &mut (*raw).state.props.items {
                if set_radio_in_list(&mut item.menu_list, command_id) {
                    break;
                }
            }
        }
    }
}

/// Find the radio group (one `menu_list` level) containing `command_id` and set
/// each radio's `checked` to whether it *is* `command_id`. Recurses into submenus.
/// Returns true once handled so the caller can stop.
fn set_radio_in_list(list: &mut [menu::MenuInfo], command_id: u32) -> bool {
    // Is the target radio directly in this list? If so, this list is the group.
    let is_group = list.iter().any(|i| {
        matches!(i, menu::MenuInfo::MenuItemRadio { command_id: id, .. } if *id == command_id)
    });
    if is_group {
        for i in list.iter_mut() {
            if let menu::MenuInfo::MenuItemRadio {
                command_id: id,
                checked,
                ..
            } = i
            {
                *checked = *id == command_id;
            }
        }
        return true;
    }
    // Otherwise descend into submenus.
    for i in list.iter_mut() {
        if let menu::MenuInfo::SubMenu { menu_list, .. } = i {
            if set_radio_in_list(menu_list, command_id) {
                return true;
            }
        }
    }
    false
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

fn on_create(window: HWND, state: State) -> Result<Context> {
    let tokens = &state.qt.theme.tokens;
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
            hovered: None,
            open_index: None,
            active: None,
            item_x: Vec::new(),
            item_w: Vec::new(),
        })
    }
}

/// Measure labels, cache their x/width (DIPs), size the strip to fill its
/// container width, resize the render target.
fn layout(window: HWND, context: &mut Context) -> Result<()> {
    let state = &context.state;
    let pad_x = state.pad_x();

    let mut item_x = Vec::with_capacity(state.props.items.len());
    let mut item_w = Vec::with_capacity(state.props.items.len());
    // Start past a small left inset so the first label isn't flush to the edge.
    let mut cursor = state.inset_x();
    for item in &state.props.items {
        let label_w = measure_text_width(&state.qt, &context.text_format, item.text);
        let w = label_w + 2.0 * pad_x;
        item_x.push(cursor);
        item_w.push(w);
        cursor += w;
    }
    let height = state.height();

    let scaling_factor = get_scaling_factor(window);
    let scaled_height = (height * scaling_factor).ceil() as i32;
    unsafe {
        // Full-width header: the parent (Stack::add_fill) owns the width; we keep
        // whatever it stretched us to and only own the height.
        let mut rc = RECT::default();
        GetClientRect(window, &mut rc)?;
        let scaled_width = rc.right.max(1);
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
    context.item_x = item_x;
    context.item_w = item_w;
    Ok(())
}

/// Hit-test a client x (device px) to a label index.
fn hit_test(context: &Context, x_px: i32, scaling_factor: f32) -> Option<usize> {
    let x = x_px as f32 / scaling_factor;
    for i in 0..context.item_x.len() {
        if x >= context.item_x[i] && x < context.item_x[i] + context.item_w[i] {
            return Some(i);
        }
    }
    None
}

/// Hit-test a *screen* point to a label index (used by the switch loop).
fn hit_test_screen(window: HWND, context: &Context, pt: POINT) -> Option<usize> {
    let mut client = pt;
    unsafe {
        if !ScreenToClient(window, &mut client).as_bool() {
            return None;
        }
    }
    hit_test(context, client.x, get_scaling_factor(window))
}

/// The screen rect of label `idx` — anchor for its dropdown and the "open label"
/// exclusion rect passed to the menu.
fn label_screen_rect(window: HWND, context: &Context, idx: usize) -> RECT {
    let scale = get_scaling_factor(window);
    let mut client = RECT::default();
    unsafe {
        _ = GetClientRect(window, &mut client);
    }
    let left = (context.item_x[idx] * scale).round() as i32;
    let right = ((context.item_x[idx] + context.item_w[idx]) * scale).round() as i32;
    let mut tl = POINT { x: left, y: 0 };
    let mut br = POINT {
        x: right,
        y: client.bottom,
    };
    unsafe {
        _ = ClientToScreen(window, &mut tl);
        _ = ClientToScreen(window, &mut br);
    }
    RECT {
        left: tl.x,
        top: tl.y,
        right: br.x,
        bottom: br.y,
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
        let height = rc.bottom as f32 / scaling_factor;
        let radius = tokens.border_radius_medium;

        for i in 0..state.props.items.len() {
            let tx = context.item_x[i];
            let tw = context.item_w[i];

            // Highlight: open label → "pressed" fill; hovered or keyboard-active
            // label → hover fill.
            let fill = if context.open_index == Some(i) {
                Some(tokens.color_neutral_background1_pressed)
            } else if context.hovered == Some(i) || context.active == Some(i) {
                Some(tokens.color_neutral_background1_hover)
            } else {
                None
            };
            if let Some(color) = fill {
                let brush = context.render_target.CreateSolidColorBrush(&color, None)?;
                let rect = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: tx,
                        top: HIGHLIGHT_INSET_Y,
                        right: tx + tw,
                        bottom: height - HIGHLIGHT_INSET_Y,
                    },
                    radiusX: radius,
                    radiusY: radius,
                };
                context.render_target.FillRoundedRectangle(&rect, &brush);
            }

            // Label — always foreground1, regular weight.
            let brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
            context.render_target.DrawText(
                state.props.items[i].text.as_wide(),
                &context.text_format,
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

/// The menu-mode loop: open label `start`'s dropdown, then keep switching as the
/// user hovers/clicks siblings or walks Left/Right, until the menu ends.
///
/// The dropdown is opened with `owner_bar = Some(bar)` so its modal loop yields
/// back here (via `TrackExit`) instead of swallowing bar interactions. Commands
/// are posted straight to the app window (the dropdown's owner), so there's no
/// command routing here.
fn run_menu_mode(window: HWND, start: usize) {
    unsafe {
        let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
        if raw.is_null() {
            return;
        }
        let (qt, n) = {
            let ctx = &*raw;
            (ctx.state.qt.clone(), ctx.state.props.items.len())
        };
        let app_window = GetParent(window).unwrap_or(window);
        if n == 0 {
            return;
        }
        let mut idx = start.min(n - 1);

        loop {
            // Draw this label as open, immediately (the modal loop below only
            // dispatches our WM_PAINT lazily).
            (*raw).open_index = Some(idx);
            _ = InvalidateRect(Some(window), None, false);
            _ = UpdateWindow(window);

            let rect = label_screen_rect(window, &*raw, idx);
            let menu_list = {
                let ctx = &*raw;
                ctx.state.props.items[idx].menu_list.clone()
            };
            let exit = qt.open_menu_ex(
                app_window,
                rect.left,
                rect.bottom,
                menu::Props { menu_list },
                Some(window),
                Some(rect),
                false,
            );

            match exit {
                Ok(menu::TrackExit::YieldMouse(pt)) => match hit_test_screen(window, &*raw, pt) {
                    // Clicked the already-open label → toggle it closed.
                    Some(j) if j == idx => break,
                    // Slid/clicked a sibling → switch to it.
                    Some(j) => idx = j,
                    // Off the bar → dismiss.
                    None => break,
                },
                Ok(menu::TrackExit::YieldKeyPrev) => idx = (idx + n - 1) % n,
                Ok(menu::TrackExit::YieldKeyNext) => idx = (idx + 1) % n,
                // Command chosen, dismissed, or error → done.
                Ok(menu::TrackExit::Ended) | Err(_) => break,
            }
        }

        (*raw).open_index = None;
        _ = InvalidateRect(Some(window), None, false);
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
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if raw.is_null() {
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
            // While a menu is open the dropdown has capture; ignore stray moves.
            if context.open_index.is_some() {
                return LRESULT(0);
            }
            let x = l_param.0 as i16 as i32;
            let hit = hit_test(context, x, get_scaling_factor(window));
            if context.hovered != hit || context.active.is_some() {
                context.hovered = hit;
                // Mouse takes over from any keyboard highlight.
                context.active = None;
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
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = SetFocus(Some(window));
            context.active = None;
            if let Some(idx) = hit_test(context, l_param.0 as i16 as i32, get_scaling_factor(window))
            {
                // Enters the modal switch loop (the dropdown takes capture inside).
                run_menu_mode(window, idx);
            }
            LRESULT(0)
        },
        WM_ENTER_MENU_MODE => unsafe {
            // Classic Win32: highlight the first label + take focus, WITHOUT opening
            // a dropdown. Down/Enter/Space opens it; Left/Right walk; Esc exits.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if !raw.is_null() && !(*raw).state.props.items.is_empty() {
                _ = SetFocus(Some(window));
                (*raw).active = Some(0);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTALLKEYS) as isize),
        WM_KEYDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            let n = context.state.props.items.len();
            // Only steer when in keyboard menu mode (a label is highlighted).
            let Some(active) = context.active else {
                return DefWindowProcW(window, message, w_param, l_param);
            };
            if n == 0 {
                return LRESULT(0);
            }
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_LEFT => {
                    context.active = Some((active + n - 1) % n);
                    _ = InvalidateRect(Some(window), None, false);
                }
                VK_RIGHT => {
                    context.active = Some((active + 1) % n);
                    _ = InvalidateRect(Some(window), None, false);
                }
                VK_DOWN | VK_RETURN | VK_SPACE => {
                    // Open the highlighted label's dropdown (enters the modal loop).
                    context.active = None;
                    run_menu_mode(window, active);
                    // Back from the loop → leave menu mode, focus the app window.
                    if let Ok(parent) = GetParent(window) {
                        _ = SetFocus(Some(parent));
                    }
                }
                VK_ESCAPE => {
                    context.active = None;
                    _ = InvalidateRect(Some(window), None, false);
                    if let Ok(parent) = GetParent(window) {
                        _ = SetFocus(Some(parent));
                    }
                }
                _ => return DefWindowProcW(window, message, w_param, l_param),
            }
            LRESULT(0)
        },
        WM_SYSKEYDOWN => unsafe {
            // Alt/F10 while already in menu mode toggles it back off.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_MENU | VK_F10 if !raw.is_null() && (*raw).active.is_some() => {
                    (*raw).active = None;
                    _ = InvalidateRect(Some(window), None, false);
                    if let Ok(parent) = GetParent(window) {
                        _ = SetFocus(Some(parent));
                    }
                    LRESULT(0)
                }
                _ => DefWindowProcW(window, message, w_param, l_param),
            }
        },
        WM_KILLFOCUS => unsafe {
            // Lost focus (Alt+Tab, click elsewhere) → drop the keyboard highlight.
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            if !raw.is_null() && (*raw).active.take().is_some() {
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_DPICHANGED_BEFOREPARENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &mut *raw;
            _ = layout(window, context);
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
