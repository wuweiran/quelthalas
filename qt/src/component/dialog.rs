use std::mem::size_of;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory1, ID2D1HwndRenderTarget, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_TEXT_METRICS,
};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetActiveWindow};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::component::button;
use crate::{get_scaling_factor, MouseEvent, QT};

#[derive(Copy, Clone)]
pub enum DialogResult {
    OK,
    Cancel,
    Close,
}

pub enum ModelType {
    Modal,
    Alert,
}

struct State {
    qt_ptr: *const QT,
    title: PCWSTR,
    content: PCWSTR,
}

struct Context {
    state: State,
    result: DialogResult,
    title_text_format: IDWriteTextFormat,
    content_text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    ok_button: HWND,
    cancel_button: HWND,
}
impl QT {
    pub fn open_dialog(
        &self,
        parent_window: &HWND,
        instance: &HINSTANCE,
        title: PCWSTR,
        content: PCWSTR,
        modal_type: &ModelType,
    ) -> Result<DialogResult> {
        let class_name: PCWSTR = w!("QT_DIALOG");
        unsafe {
            let window_class: WNDCLASSEXW = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpszClassName: class_name,
                style: CS_OWNDC,
                lpfnWndProc: Some(window_proc),
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
            let scaling_factor = get_scaling_factor(parent_window);
            EnableWindow(*parent_window, FALSE);
            let boxed = Box::new(State {
                qt_ptr: self as *const Self,
                title,
                content,
            });
            let window_style = match modal_type {
                ModelType::Modal => WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                ModelType::Alert => WS_OVERLAPPED | WS_DLGFRAME,
            };
            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                title,
                window_style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (600f32 * scaling_factor) as i32,
                (400f32 * scaling_factor) as i32,
                *parent_window,
                None,
                *instance,
                Some(Box::<State>::into_raw(boxed) as _),
            );

            ShowWindow(window, SW_SHOW);

            let mut message = MSG::default();
            let mut result = DialogResult::Cancel;
            while GetMessageW(&mut message, None, 0, 0).into() {
                if message.message == WM_USER {
                    let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                    let context = &*raw;
                    result = context.result;
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
                let window_exists: bool = IsWindow(window).into();
                if !window_exists {
                    break;
                }
            }
            EnableWindow(*parent_window, TRUE);
            SetActiveWindow(*parent_window);
            Ok(result)
        }
    }
}

unsafe fn on_create(window: HWND, state: State) -> Result<Context> {
    let instance = HINSTANCE(GetWindowLongPtrW(window, GWLP_HINSTANCE));
    let qt = &(*state.qt_ptr);
    let direct_write_factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
    let title_typo = &qt.typography_styles.subtitle1;
    let title_text_format = title_typo.create_text_format(&direct_write_factory)?;
    let content_typo = &qt.typography_styles.body1;
    let content_text_format = content_typo.create_text_format(&direct_write_factory)?;

    let factory = D2D1CreateFactory::<ID2D1Factory1>(
        D2D1_FACTORY_TYPE_SINGLE_THREADED,
        Some(&D2D1_FACTORY_OPTIONS::default()),
    )?;
    let render_target = factory.CreateHwndRenderTarget(
        &D2D1_RENDER_TARGET_PROPERTIES::default(),
        &D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd: window,
            pixelSize: D2D_SIZE_U {
                width: 600u32,
                height: 400u32,
            },
            presentOptions: Default::default(),
        },
    )?;

    let ok_button = qt.creat_button(
        &window,
        &instance,
        0,
        0,
        w!("OK"),
        &button::Appearance::Primary,
        None,
        None,
        &button::Shape::Rounded,
        &button::Size::Medium,
        MouseEvent {
            on_click: Box::new(move |_| {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                (*raw).result = DialogResult::OK;
                _ = PostMessageW(window, WM_USER, WPARAM(0), LPARAM(0));
            }),
        },
    )?;
    let cancel_button = qt.creat_button(
        &window,
        &instance,
        0,
        0,
        w!("Cancel"),
        &button::Appearance::Secondary,
        None,
        None,
        &button::Shape::Rounded,
        &button::Size::Medium,
        MouseEvent {
            on_click: Box::new(move |_| {
                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                (*raw).result = DialogResult::Cancel;
                _ = PostMessageW(window, WM_USER, WPARAM(0), LPARAM(0));
            }),
        },
    )?;
    Ok(Context {
        state,
        title_text_format,
        content_text_format,
        render_target,
        result: DialogResult::Close,
        ok_button,
        cancel_button,
    })
}

unsafe fn layout(window: HWND, context: &Context) -> Result<()> {
    let scaling_factor = get_scaling_factor(&window);

    let mut button_rect = RECT::default();
    GetClientRect(context.cancel_button, &mut button_rect)?;
    let cancel_button_width = button_rect.right - button_rect.left;
    let cancel_button_height = button_rect.bottom - button_rect.top;
    GetClientRect(context.ok_button, &mut button_rect)?;
    let ok_button_width = button_rect.right - button_rect.left;
    let ok_button_height = button_rect.bottom - button_rect.top;

    let surface_padding = 24f32;
    let gap = 8f32;

    let state = &context.state;
    let direct_write_factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
    let title_text_layout = direct_write_factory.CreateTextLayout(
        state.title.as_wide(),
        &context.title_text_format,
        600f32 - 24f32 - 24f32,
        1000f32,
    )?;
    let mut title_metrics = DWRITE_TEXT_METRICS::default();
    title_text_layout.GetMetrics(&mut title_metrics)?;
    let content_text_layout = direct_write_factory.CreateTextLayout(
        state.content.as_wide(),
        &context.content_text_format,
        600f32 - 24f32 - 24f32,
        1000f32,
    )?;
    let mut content_metrics = DWRITE_TEXT_METRICS::default();
    content_text_layout.GetMetrics(&mut content_metrics)?;

    let scaled_width = (((surface_padding * 2f32 + title_metrics.width)
        .max(surface_padding * 2f32 + content_metrics.width)
        .min(600f32))
        * scaling_factor)
        .ceil() as i32;
    let buttons_top = surface_padding + title_metrics.height + gap + content_metrics.height + gap;
    let scaled_height = ((buttons_top + surface_padding) * scaling_factor).ceil() as i32
        + ok_button_height.max(cancel_button_height);

    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scaled_width,
        bottom: scaled_height,
    };
    AdjustWindowRect(
        &mut rect,
        WINDOW_STYLE(GetWindowLongPtrW(window, GWL_STYLE) as u32),
        FALSE,
    )?;
    let window_width = rect.right - rect.left;
    let window_height = rect.bottom - rect.top;
    let parent_window = GetAncestor(window, GA_PARENT);
    GetWindowRect(parent_window, &mut rect)?;
    SetWindowPos(
        window,
        None,
        rect.left / 2 + rect.right / 2 - window_width / 2,
        rect.top / 2 + rect.bottom / 2 - window_height / 2,
        window_width,
        window_height,
        SWP_NOZORDER,
    )?;
    context.render_target.Resize(&D2D_SIZE_U {
        width: scaled_width as u32,
        height: scaled_height as u32,
    })?;
    MoveWindow(
        context.cancel_button,
        scaled_width - (cancel_button_width + (24f32 * scaling_factor) as i32),
        (buttons_top * scaling_factor) as i32,
        cancel_button_width,
        cancel_button_height,
        FALSE,
    )?;
    MoveWindow(
        context.ok_button,
        scaled_width - (cancel_button_width + ok_button_width + (32f32 * scaling_factor) as i32),
        (buttons_top * scaling_factor) as i32,
        ok_button_width,
        ok_button_height,
        FALSE,
    )?;

    Ok(())
}

unsafe fn on_paint(window: HWND, context: &Context) -> Result<()> {
    let state = &context.state;
    let tokens = &(*state.qt_ptr).tokens;
    let mut window_rect = RECT::default();
    GetClientRect(window, &mut window_rect)?;
    let scaling_factor = get_scaling_factor(&window);
    let width = (window_rect.right - window_rect.left) as f32 / scaling_factor;
    let height = (window_rect.bottom - window_rect.top) as f32 / scaling_factor;

    let mut ps = PAINTSTRUCT::default();
    BeginPaint(window, &mut ps);
    context.render_target.BeginDraw();
    context
        .render_target
        .Clear(Some(&tokens.color_neutral_background1));

    let text_brush = context
        .render_target
        .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
    context.render_target.DrawText(
        state.title.as_wide(),
        &context.title_text_format,
        &D2D_RECT_F {
            left: 24f32,
            top: 24f32,
            right: width - 24f32,
            bottom: height - 24f32,
        },
        &text_brush,
        D2D1_DRAW_TEXT_OPTIONS_NONE,
        DWRITE_MEASURING_MODE_NATURAL,
    );

    let direct_write_factory = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED)?;
    let title_text_layout = direct_write_factory.CreateTextLayout(
        state.title.as_wide(),
        &context.title_text_format,
        width - 24f32 - 24f32,
        height - 24f32 - 24f32,
    )?;
    let mut title_metrics = DWRITE_TEXT_METRICS::default();
    title_text_layout.GetMetrics(&mut title_metrics)?;
    context.render_target.DrawText(
        state.content.as_wide(),
        &context.content_text_format,
        &D2D_RECT_F {
            left: 24f32,
            top: 24f32 + title_metrics.height + 8f32,
            right: width - 24f32,
            bottom: height - 24f32,
        },
        &text_brush,
        D2D1_DRAW_TEXT_OPTIONS_NONE,
        DWRITE_MEASURING_MODE_NATURAL,
    );

    context.render_target.EndDraw(None, None)?;
    EndPaint(window, &ps);
    Ok(())
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
                    DefWindowProcW(window, message, w_param, l_param)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_PAINT | WM_DISPLAYCHANGE => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = on_paint(window, context);
            DefWindowProcW(window, message, w_param, l_param)
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
