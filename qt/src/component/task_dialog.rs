//! A task dialog — Win32 `TaskDialog`, Fluent-styled. The richer modal built on
//! top of `dialog`: a status icon, a two-tier text hierarchy (main instruction +
//! content), optional command links (large two-line choices), a verification
//! checkbox, and common command buttons. Reuses `dialog`'s modal scaffold
//! (blocking owned top-level window, embedded child buttons, WM_USER→destroy,
//! center-over-owner) and embeds `create_button` / `create_checkbox` children;
//! command links are custom-painted + hit-tested inside the dialog.

use std::cell::Cell;
use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget, ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_TEXT_METRICS, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, ReleaseCapture, SetActiveWindow, SetCapture, TrackMouseEvent,
    TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

use crate::component::{button, checkbox};
use crate::icon::Icon as SvgIcon;
use crate::icon::path::build_geometry;
use crate::sys::{adjust_window_rect_ex_for_dpi, dpi_for_window};
use crate::{MouseEvent, QT, get_scaling_factor};

// --- layout constants (DIPs) ---
const PAD: f32 = 24.0;
const GAP: f32 = 8.0;
const ICON_SIZE: f32 = 24.0;
const ICON_GUTTER: f32 = ICON_SIZE + 12.0; // icon + trailing space
const TEXT_WIDTH: f32 = 360.0; // wrapping width of the text column
const LINK_PAD: f32 = 12.0; // inner padding of a command link
const LINK_GAP: f32 = 6.0; // between stacked command links
const LINK_CHEVRON: f32 = 16.0;

/// The intent conveyed by the status icon (Fluent MessageBar/Dialog intents).
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Intent {
    None,
    Info,
    Warning,
    Error,
    Success,
}

/// A common command button. The result reports the clicked one's Win32 id.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Button {
    Ok,
    Cancel,
    Yes,
    No,
    Retry,
    Close,
}

impl Button {
    fn id(self) -> i32 {
        match self {
            Button::Ok => 1,     // IDOK
            Button::Cancel => 2, // IDCANCEL
            Button::Retry => 4,  // IDRETRY
            Button::Yes => 6,    // IDYES
            Button::No => 7,     // IDNO
            Button::Close => 8,  // IDCLOSE
        }
    }
    fn label(self) -> Vec<u16> {
        // Localized via user32's string table (same ids MB_GetString uses).
        match self {
            Button::Ok => crate::system_string(800, "OK"),
            Button::Cancel => crate::system_string(801, "Cancel"),
            Button::Yes => crate::system_string(805, "Yes"),
            Button::No => crate::system_string(806, "No"),
            Button::Retry => crate::system_string(803, "Retry"),
            Button::Close => crate::system_string(807, "Close"),
        }
    }
}

/// A command link — a large two-line choice (bold text + optional note).
pub struct CommandLink {
    pub id: i32,
    pub text: PCWSTR,
    pub note: Option<PCWSTR>,
}

pub struct Props {
    pub title: PCWSTR,
    pub instruction: PCWSTR,
    pub content: PCWSTR,
    pub intent: Intent,
    pub buttons: Vec<Button>,
    pub command_links: Vec<CommandLink>,
    pub verification: Option<PCWSTR>,
    pub verification_checked: bool,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            title: w!(""),
            instruction: w!(""),
            content: w!(""),
            intent: Intent::None,
            buttons: Vec::new(),
            command_links: Vec::new(),
            verification: None,
            verification_checked: false,
        }
    }
}

#[derive(Copy, Clone)]
pub struct TaskDialogResult {
    pub button: i32,
    pub verified: bool,
}

struct State {
    qt: QT,
    props: Props,
}

struct Context {
    state: State,
    render_target: ID2D1HwndRenderTarget,
    instruction_format: IDWriteTextFormat,
    content_format: IDWriteTextFormat,
    link_text_format: IDWriteTextFormat,
    link_note_format: IDWriteTextFormat,
    icon_geometry: Option<ID2D1PathGeometry1>,
    chevron_geometry: Option<ID2D1PathGeometry1>,
    buttons: Vec<(HWND, i32)>, // (hwnd, result id)
    // Owned button-label buffers the child buttons' `PCWSTR`s point into; kept
    // alive for the dialog's lifetime (the buttons read them live, never copy).
    _button_labels: Vec<Vec<u16>>,
    checkbox: Option<HWND>,
    result: Cell<i32>,
    verified: Cell<bool>,
    hovered_link: Cell<Option<usize>>,
    pressed_link: Cell<Option<usize>>,
}

impl Context {
    fn dialog_width(&self) -> f32 {
        PAD + ICON_GUTTER + TEXT_WIDTH + PAD
    }
}

impl QT {
    pub fn open_task_dialog(
        &self,
        parent_window: HWND,
        props: Props,
    ) -> Result<TaskDialogResult> {
        let class_name: PCWSTR = w!("QT_TASK_DIALOG");
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: class_name,
                    style: CS_OWNDC,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            let scaling_factor = get_scaling_factor(parent_window);
            _ = EnableWindow(parent_window, false);
            let title = props.title;
            let boxed = Box::new(State {
                qt: self.clone(),
                props,
            });
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                title,
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (480.0 * scaling_factor) as i32,
                (320.0 * scaling_factor) as i32,
                Some(parent_window),
                None,
                Some(HINSTANCE(GetWindowLongPtrW(parent_window, GWLP_HINSTANCE) as _)),
                Some(Box::<State>::into_raw(boxed) as _),
            )?;

            _ = ShowWindow(window, SW_SHOW);

            let mut message = MSG::default();
            let mut result = TaskDialogResult { button: 2, verified: false }; // default IDCANCEL
            while GetMessageW(&mut message, None, 0, 0).into() {
                if message.message == WM_USER {
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &*raw;
                    result = TaskDialogResult {
                        button: context.result.get(),
                        verified: context.verified.get(),
                    };
                    _ = EnableWindow(parent_window, true);
                    _ = SetActiveWindow(parent_window);
                }
                _ = TranslateMessage(&message);
                DispatchMessageW(&message);
                if !IsWindow(Some(window)).as_bool() {
                    break;
                }
            }
            _ = EnableWindow(parent_window, true);
            _ = SetActiveWindow(parent_window);
            Ok(result)
        }
    }
}

fn intent_color(state: &State) -> D2D1_COLOR_F {
    let tokens = &state.qt.theme.tokens;
    match state.props.intent {
        Intent::Warning => tokens.color_status_warning_foreground3,
        Intent::Error => tokens.color_status_danger_foreground1,
        Intent::Success => tokens.color_status_success_foreground1,
        // Info (and None) use the neutral base colour (Fluent's info intent has no
        // colour override).
        _ => tokens.color_neutral_foreground2,
    }
}

fn on_create(window: HWND, state: State) -> Result<Context> {
    let qt = &state.qt;
    unsafe {
        let dwrite = &qt.dwrite_factory;
        let tokens = &qt.theme.tokens;
        let instruction_format = qt.theme.typography_styles.subtitle1.create_text_format(dwrite)?;
        let content_format = qt.theme.typography_styles.body1.create_text_format(dwrite)?;
        // Match the common buttons' font (base300 / semibold / base family).
        let link_text_format = dwrite.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_semibold,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base300,
            w!(""),
        )?;
        let link_note_format = qt.theme.typography_styles.caption1.create_text_format(dwrite)?;

        let dpi = dpi_for_window(window);
        let render_target = qt.d2d_factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U { width: 480, height: 320 },
                presentOptions: Default::default(),
            },
        )?;

        // Icon glyphs are filled path geometries (Direct2D 1.0); the tint is chosen
        // at draw time via a brush, so we only build the shapes here.
        let icon_geometry = match state.props.intent {
            Intent::None => None,
            Intent::Info => build_geometry(&qt.d2d_factory, &SvgIcon::info_20_filled()).ok(),
            Intent::Warning => build_geometry(&qt.d2d_factory, &SvgIcon::warning_20_filled()).ok(),
            Intent::Error => build_geometry(&qt.d2d_factory, &SvgIcon::diamond_dismiss_20_filled()).ok(),
            Intent::Success => build_geometry(&qt.d2d_factory, &SvgIcon::checkmark_circle_20_filled()).ok(),
        };
        let chevron_geometry = if state.props.command_links.is_empty() {
            None
        } else {
            build_geometry(&qt.d2d_factory, &SvgIcon::chevron_right_20_regular()).ok()
        };

        // Common buttons — first Primary, rest Secondary. Empty → single OK.
        let button_specs: Vec<Button> = if state.props.buttons.is_empty() {
            vec![Button::Ok]
        } else {
            state.props.buttons.clone()
        };
        let mut buttons = Vec::new();
        let mut button_labels = Vec::new();
        for (i, b) in button_specs.iter().enumerate() {
            let id = b.id();
            let appearance = if i == 0 {
                button::Appearance::Primary
            } else {
                button::Appearance::Secondary
            };
            // Owned label buffer, kept in Context so it outlives the button.
            let label = b.label();
            let hwnd = qt.create_button(
                window,
                0,
                0,
                button::Props {
                    text: PCWSTR::from_raw(label.as_ptr()),
                    appearance,
                    mouse_event: MouseEvent {
                        on_click: Box::new(move |_| {
                            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                            (*raw).result.set(id);
                            _ = PostMessageW(Some(window), WM_USER, WPARAM(0), LPARAM(0));
                        }),
                    },
                    ..Default::default()
                },
            )?;
            buttons.push((hwnd, id));
            button_labels.push(label);
        }

        let checkbox = match state.props.verification {
            None => None,
            Some(label) => {
                let hwnd = qt.create_checkbox(
                    window,
                    0,
                    0,
                    checkbox::Props {
                        label,
                        checked: state.props.verification_checked,
                        mouse_event: checkbox::MouseEvent {
                            on_change: Box::new(move |_, checked| {
                                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                                (*raw).verified.set(checked);
                            }),
                        },
                        ..Default::default()
                    },
                )?;
                Some(hwnd)
            }
        };

        Ok(Context {
            state,
            render_target,
            instruction_format,
            content_format,
            link_text_format,
            link_note_format,
            icon_geometry,
            chevron_geometry,
            buttons,
            _button_labels: button_labels,
            checkbox,
            result: Cell::new(2), // IDCANCEL default
            verified: Cell::new(false),
            hovered_link: Cell::new(None),
            pressed_link: Cell::new(None),
        })
    }
}

/// Measured height of a text block at the text column width.
fn text_height(context: &Context, text: PCWSTR, format: &IDWriteTextFormat, width: f32) -> f32 {
    if text.is_null() || unsafe { text.as_wide() }.is_empty() {
        return 0.0;
    }
    unsafe {
        let Ok(layout) =
            context.state.qt.dwrite_factory.CreateTextLayout(text.as_wide(), format, width, 4000.0)
        else {
            return 0.0;
        };
        let mut m = DWRITE_TEXT_METRICS::default();
        if layout.GetMetrics(&mut m).is_ok() {
            m.height.ceil()
        } else {
            0.0
        }
    }
}

/// The vertical layout, computed in DIPs. `link_rects` are full command-link
/// rectangles (for paint + hit-test).
struct Geometry {
    text_left: f32,
    instruction_top: f32,
    instruction_h: f32,
    content_top: f32,
    content_h: f32,
    link_rects: Vec<D2D_RECT_F>,
    buttons_top: f32,
}

fn geometry(context: &Context) -> Geometry {
    let state = &context.state;
    let text_left = PAD + ICON_GUTTER;
    let text_w = TEXT_WIDTH;

    let instruction_top = PAD;
    let instruction_h =
        text_height(context, state.props.instruction, &context.instruction_format, text_w);
    let content_top = instruction_top + instruction_h + (if instruction_h > 0.0 { GAP } else { 0.0 });
    let content_h = text_height(context, state.props.content, &context.content_format, text_w);

    let mut y = content_top + content_h + (if content_h > 0.0 { GAP + GAP } else { 0.0 });
    let link_inner_w = text_w - LINK_PAD * 2.0 - LINK_CHEVRON - LINK_PAD;
    let mut link_rects = Vec::new();
    for link in &state.props.command_links {
        let lh = text_height(context, link.text, &context.link_text_format, link_inner_w);
        let nh = match link.note {
            Some(n) => text_height(context, n, &context.link_note_format, link_inner_w),
            None => 0.0,
        };
        let link_h = LINK_PAD * 2.0 + lh + (if nh > 0.0 { 2.0 } else { 0.0 }) + nh;
        link_rects.push(D2D_RECT_F {
            left: text_left,
            top: y,
            right: text_left + text_w,
            bottom: y + link_h,
        });
        y += link_h + LINK_GAP;
    }
    if !link_rects.is_empty() {
        y += GAP; // extra space before the button row
    }

    let buttons_top = y;
    Geometry {
        text_left,
        instruction_top,
        instruction_h,
        content_top,
        content_h,
        link_rects,
        buttons_top,
    }
}

fn layout(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(window);
    let g = geometry(context);
    unsafe {
        // Button row height (device px) — measure the tallest.
        let mut btn_h = 0i32;
        let mut btn_widths = Vec::new();
        for (hwnd, _) in &context.buttons {
            let mut rc = RECT::default();
            _ = GetClientRect(*hwnd, &mut rc);
            btn_widths.push(rc.right - rc.left);
            btn_h = btn_h.max(rc.bottom - rc.top);
        }
        // Checkbox height (device px).
        let mut chk_h = 0i32;
        let mut chk_w = 0i32;
        if let Some(hwnd) = context.checkbox {
            let mut rc = RECT::default();
            _ = GetClientRect(hwnd, &mut rc);
            chk_h = rc.bottom - rc.top;
            chk_w = rc.right - rc.left;
        }

        let width = context.dialog_width();
        let scaled_width = (width * scaling_factor).ceil() as i32;
        let buttons_top_px = (g.buttons_top * scaling_factor) as i32;
        let scaled_height =
            buttons_top_px + btn_h.max(chk_h) + (PAD * scaling_factor) as i32;

        // Size + center over owner (dialog.rs pattern).
        let mut rect = RECT { left: 0, top: 0, right: scaled_width, bottom: scaled_height };
        adjust_window_rect_ex_for_dpi(
            &mut rect,
            WINDOW_STYLE(GetWindowLongPtrW(window, GWL_STYLE) as u32),
            false,
            WINDOW_EX_STYLE(GetWindowLongPtrW(window, GWL_EXSTYLE) as u32),
            dpi_for_window(window),
        )?;
        let win_w = rect.right - rect.left;
        let win_h = rect.bottom - rect.top;
        let owner = GetWindow(window, GW_OWNER).unwrap_or_else(|_| GetDesktopWindow());
        GetWindowRect(owner, &mut rect)?;
        SetWindowPos(
            window,
            None,
            rect.left / 2 + rect.right / 2 - win_w / 2,
            rect.top / 2 + rect.bottom / 2 - win_h / 2,
            win_w,
            win_h,
            SWP_NOZORDER,
        )?;
        context.render_target.Resize(&D2D_SIZE_U {
            width: scaled_width as u32,
            height: scaled_height as u32,
        })?;

        // Place buttons right-to-left along the button row.
        let pad_px = (PAD * scaling_factor) as i32;
        let gap_px = (GAP * scaling_factor) as i32;
        let mut x = scaled_width - pad_px;
        for ((hwnd, _), bw) in context.buttons.iter().zip(btn_widths.iter()) {
            x -= bw;
            _ = MoveWindow(*hwnd, x, buttons_top_px, *bw, btn_h, false);
            x -= gap_px;
        }
        // Verification checkbox bottom-left, vertically centered on the button row.
        if let Some(hwnd) = context.checkbox {
            let cy = buttons_top_px + (btn_h.max(chk_h) - chk_h) / 2;
            _ = MoveWindow(hwnd, pad_px, cy, chk_w, chk_h, false);
        }
    }
    Ok(())
}

fn paint(context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &state.qt.theme.tokens;
    let g = geometry(context);
    unsafe {
        let rt = &context.render_target;
        let text_brush = rt.CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;

        // Status icon (top-left, aligned with the instruction). Tint = the intent
        // colour the SVG baked in.
        if let Some(geometry) = &context.icon_geometry {
            let icon_brush = rt.CreateSolidColorBrush(&intent_color(state), None)?;
            let s = ICON_SIZE / 20.0;
            rt.SetTransform(&Matrix3x2 {
                M11: s,
                M12: 0.0,
                M21: 0.0,
                M22: s,
                M31: PAD,
                M32: g.instruction_top,
            });
            rt.FillGeometry(geometry, &icon_brush, None);
            rt.SetTransform(&Matrix3x2::identity());
        }

        // Instruction.
        if g.instruction_h > 0.0 {
            rt.DrawText(
                state.props.instruction.as_wide(),
                &context.instruction_format,
                &D2D_RECT_F {
                    left: g.text_left,
                    top: g.instruction_top,
                    right: g.text_left + TEXT_WIDTH,
                    bottom: g.instruction_top + g.instruction_h,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
        // Content.
        if g.content_h > 0.0 {
            rt.DrawText(
                state.props.content.as_wide(),
                &context.content_format,
                &D2D_RECT_F {
                    left: g.text_left,
                    top: g.content_top,
                    right: g.text_left + TEXT_WIDTH,
                    bottom: g.content_top + g.content_h,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }

        // Command links.
        let radius = tokens.border_radius_medium;
        for (i, link) in state.props.command_links.iter().enumerate() {
            let r = g.link_rects[i];
            let fill = if context.pressed_link.get() == Some(i) {
                Some(tokens.color_neutral_background1_pressed)
            } else if context.hovered_link.get() == Some(i) {
                Some(tokens.color_subtle_background_hover)
            } else {
                None
            };
            if let Some(color) = fill {
                let brush = rt.CreateSolidColorBrush(&color, None)?;
                rt.FillRoundedRectangle(
                    &D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius },
                    &brush,
                );
            }
            // Border.
            let border = rt.CreateSolidColorBrush(&tokens.color_neutral_stroke1, None)?;
            rt.DrawRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: r.left + tokens.stroke_width_thin * 0.5,
                        top: r.top + tokens.stroke_width_thin * 0.5,
                        right: r.right - tokens.stroke_width_thin * 0.5,
                        bottom: r.bottom - tokens.stroke_width_thin * 0.5,
                    },
                    radiusX: radius,
                    radiusY: radius,
                },
                &border,
                tokens.stroke_width_thin,
                &state.qt.stroke_style,
            );

            let inner_left = r.left + LINK_PAD;
            let inner_right = r.right - LINK_PAD - LINK_CHEVRON - LINK_PAD;
            let lh = text_height(context, link.text, &context.link_text_format, inner_right - inner_left);
            rt.DrawText(
                link.text.as_wide(),
                &context.link_text_format,
                &D2D_RECT_F {
                    left: inner_left,
                    top: r.top + LINK_PAD,
                    right: inner_right,
                    bottom: r.top + LINK_PAD + lh,
                },
                &text_brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            if let Some(note) = link.note {
                let note_brush = rt.CreateSolidColorBrush(&tokens.color_neutral_foreground2, None)?;
                let nh = text_height(context, note, &context.link_note_format, inner_right - inner_left);
                rt.DrawText(
                    note.as_wide(),
                    &context.link_note_format,
                    &D2D_RECT_F {
                        left: inner_left,
                        top: r.top + LINK_PAD + lh + 2.0,
                        right: inner_right,
                        bottom: r.top + LINK_PAD + lh + 2.0 + nh,
                    },
                    &note_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            // Chevron, right-centered. Tint = the neutral foreground2 the SVG baked in.
            if let Some(chevron) = &context.chevron_geometry {
                let chevron_brush =
                    rt.CreateSolidColorBrush(&tokens.color_neutral_foreground2, None)?;
                let s = LINK_CHEVRON / 20.0;
                let gx = r.right - LINK_PAD - LINK_CHEVRON;
                let gy = (r.top + r.bottom) / 2.0 - LINK_CHEVRON / 2.0;
                rt.SetTransform(&Matrix3x2 {
                    M11: s,
                    M12: 0.0,
                    M21: 0.0,
                    M22: s,
                    M31: gx,
                    M32: gy,
                });
                rt.FillGeometry(chevron, &chevron_brush, None);
                rt.SetTransform(&Matrix3x2::identity());
            }
        }
    }
    Ok(())
}

fn on_paint(window: HWND, context: &Context) -> Result<()> {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        BeginPaint(window, &mut ps);
        context.render_target.BeginDraw();
        context
            .render_target
            .Clear(Some(&context.state.qt.theme.tokens.color_neutral_background1));
        let result = paint(context).and(context.render_target.EndDraw(None, None));
        _ = EndPaint(window, &ps);
        result
    }
}

/// Command-link index under a client-DIP point, or None.
fn link_at(context: &Context, x: f32, y: f32) -> Option<usize> {
    let g = geometry(context);
    g.link_rects
        .iter()
        .position(|r| x >= r.left && x <= r.right && y >= r.top && y <= r.bottom)
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
                    let boxed = Box::new(context);
                    let ptr = Box::<Context>::into_raw(boxed);
                    SetWindowLongPtrW(window, GWLP_USERDATA, ptr as _);
                    _ = layout(window, &*ptr);
                    DefWindowProcW(window, message, w_param, l_param)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = on_paint(window, &*raw);
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_MOUSEMOVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let y = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            let now = link_at(context, x, y);
            if now != context.hovered_link.get() {
                context.hovered_link.set(now);
                _ = InvalidateRect(Some(window), None, false);
            }
            // Ask for a WM_MOUSELEAVE so a fast exit doesn't leave a link stuck hovered.
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: window,
                dwHoverTime: 0,
            };
            _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        },
        WM_MOUSELEAVE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            if context.hovered_link.get().is_some() {
                context.hovered_link.set(None);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONDOWN => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let y = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            if let Some(i) = link_at(context, x, y) {
                context.pressed_link.set(Some(i));
                SetCapture(window);
                _ = InvalidateRect(Some(window), None, false);
            }
            LRESULT(0)
        },
        WM_LBUTTONUP => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            if GetCapture() == window {
                _ = ReleaseCapture();
            }
            let pressed = context.pressed_link.get();
            context.pressed_link.set(None);
            let scaling_factor = get_scaling_factor(window);
            let x = (l_param.0 as i16 as i32) as f32 / scaling_factor;
            let y = ((l_param.0 >> 16) as i16 as i32) as f32 / scaling_factor;
            if let Some(i) = link_at(context, x, y) {
                if pressed == Some(i) {
                    context.result.set(context.state.props.command_links[i].id);
                    _ = PostMessageW(Some(window), WM_USER, WPARAM(0), LPARAM(0));
                    return LRESULT(0);
                }
            }
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        WM_SETCURSOR => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *const Context;
            if !raw.is_null() {
                let mut pt = POINT::default();
                _ = GetCursorPos(&mut pt);
                _ = ScreenToClient(window, &mut pt);
                let scaling_factor = get_scaling_factor(window);
                let x = pt.x as f32 / scaling_factor;
                let y = pt.y as f32 / scaling_factor;
                if link_at(&*raw, x, y).is_some() {
                    SetCursor(LoadCursorW(None, IDC_HAND).ok());
                    return LRESULT(1);
                }
            }
            DefWindowProcW(window, message, w_param, l_param)
        },
        WM_GETDPISCALEDSIZE => LRESULT(TRUE.0 as isize),
        WM_DPICHANGED => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let new_dpi_x = w_param.0 as i16 as f32;
            let new_dpi_y = (w_param.0 >> 16) as i16 as f32;
            context.render_target.SetDpi(new_dpi_x, new_dpi_y);
            _ = layout(window, context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(TRUE.0 as isize)
        },
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        WM_USER => unsafe {
            _ = DestroyWindow(window);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
