use std::cell::{Cell, RefCell};
use std::mem::size_of;
use std::rc::Rc;
use std::sync::Once;

use crate::icon::Icon;
use crate::{QT, get_scaling_factor};
use windows::Win32::Foundation::{
    COLORREF, ERROR_INVALID_WINDOW_HANDLE, FALSE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT,
    TRUE, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_SVG_PAINT_TYPE_COLOR, ID2D1DeviceContext5, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush, ID2D1SvgAttribute, ID2D1SvgDocument,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_METRICS,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, CreateRoundRectRgn, EndPaint, GetMonitorInfoW,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, OffsetRect, PAINTSTRUCT, PtInRect,
    RDW_INVALIDATE, RDW_NOCHILDREN, RedrawWindow, SetRect, SetRectEmpty, SetWindowRgn,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_DOWN, VK_END, VK_ESCAPE, VK_F10, VK_HOME, VK_LEFT,
    VK_MENU, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::{Matrix3x2, Vector2};

#[derive(Clone)]
pub enum MenuInfo {
    MenuItem {
        text: PCWSTR,
        command_id: u32,
        disabled: bool,
        secondary_text: Option<PCWSTR>,
        /// Optional leading icon (drawn 20×20). `None` = no icon column for this item.
        icon: Option<Icon>,
    },
    /// A radio-select item: shows a leading checkmark when `checked`, and behaves
    /// like a normal item on click (posts `WM_COMMAND(command_id)`). The caller owns
    /// the selection — set `checked` per item and update it when a pick arrives.
    MenuItemRadio {
        text: PCWSTR,
        command_id: u32,
        checked: bool,
        disabled: bool,
        secondary_text: Option<PCWSTR>,
        /// Optional icon drawn after the checkmark column (checkmark, then icon, then text).
        icon: Option<Icon>,
    },
    SubMenu {
        menu_list: Vec<MenuInfo>,
        text: PCWSTR,
    },
    MenuDivider,
}

enum MenuItem {
    MenuItem {
        text: PCWSTR,
        id: u32,
        rect: RECT,
        disabled: bool,
        secondary_text: Option<PCWSTR>,
        icon: Option<Icon>,
    },
    MenuItemRadio {
        text: PCWSTR,
        id: u32,
        rect: RECT,
        checked: bool,
        disabled: bool,
        secondary_text: Option<PCWSTR>,
        icon: Option<Icon>,
    },
    SubMenu {
        sub_menu: Rc<RefCell<Menu>>,
        text: PCWSTR,
        rect: RECT,
    },
    MenuDivider {
        rect: RECT,
    },
}

struct Menu {
    items: Vec<MenuItem>,
    window: Option<HWND>,
    focused_item_index: Option<usize>,
    /// The item currently held down by the pointer (drawn "pressed"), if any.
    pressed_item_index: Option<usize>,
    menu_list_rect: RECT,
    is_scrolling: bool,
    scroll_position: i32,
}

pub struct Context {
    qt: QT,
    menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
    render_target: ID2D1HwndRenderTarget,
    text_format: IDWriteTextFormat,
    text_brush: ID2D1SolidColorBrush,
    text_focused_brush: ID2D1SolidColorBrush,
    text_pressed_brush: ID2D1SolidColorBrush,
    text_disabled_brush: ID2D1SolidColorBrush,
    secondary_text_format: IDWriteTextFormat,
    secondary_text_brush: ID2D1SolidColorBrush,
    sub_menu_indicator_svg: ID2D1SvgDocument,
    sub_menu_indicator_focused_svg: ID2D1SvgDocument,
    /// Leading checkmark for a checked `MenuItemRadio`.
    checkmark_svg: ID2D1SvgDocument,
    /// Per-item leading icon SVG (parallel to `menu.items`; `None` where the item
    /// has no icon or isn't an item/radio). Built once in `on_create`.
    item_icon_svgs: Vec<Option<ID2D1SvgDocument>>,
    fade_elapsed_ms: Cell<u32>,
}

fn convert_menu_info_list_to_menu(menu_info_list: Vec<MenuInfo>) -> Menu {
    let items = menu_info_list
        .into_iter()
        .map(|menu_info| match menu_info {
            MenuInfo::MenuItem {
                text,
                command_id,
                disabled,
                secondary_text,
                icon,
            } => MenuItem::MenuItem {
                text,
                id: command_id,
                rect: RECT::default(),
                disabled,
                secondary_text,
                icon,
            },
            MenuInfo::MenuItemRadio {
                text,
                command_id,
                checked,
                disabled,
                secondary_text,
                icon,
            } => MenuItem::MenuItemRadio {
                text,
                id: command_id,
                rect: RECT::default(),
                checked,
                disabled,
                secondary_text,
                icon,
            },
            MenuInfo::SubMenu { menu_list, text } => {
                let sub_menu = convert_menu_info_list_to_menu(menu_list);
                MenuItem::SubMenu {
                    sub_menu: Rc::new(RefCell::new(sub_menu)),
                    text,
                    rect: RECT::default(),
                }
            }
            MenuInfo::MenuDivider => MenuItem::MenuDivider {
                rect: RECT::default(),
            },
        })
        .collect();
    Menu {
        items,
        window: None,
        focused_item_index: None,
        pressed_item_index: None,
        menu_list_rect: RECT::default(),
        is_scrolling: false,
        scroll_position: 0,
    }
}

const CLASS_NAME: PCWSTR = w!("QT_MENU");

fn set_svg_color(svg: &ID2D1SvgDocument, color: &D2D1_COLOR_F) -> Result<()> {
    unsafe {
        let svg_paint = svg.CreatePaint(D2D1_SVG_PAINT_TYPE_COLOR, Some(color), w!(""))?;
        svg.GetRoot()?
            .GetFirstChild()?
            .SetAttributeValue(w!("fill"), &svg_paint.cast::<ID2D1SvgAttribute>()?)?;
    }
    Ok(())
}

/// Build a tinted `ID2D1SvgDocument` for `icon` at its native viewBox size.
fn create_icon_svg(
    device_context5: &ID2D1DeviceContext5,
    icon: &Icon,
    color: &D2D1_COLOR_F,
) -> Result<ID2D1SvgDocument> {
    let size = D2D_SIZE_F {
        width: icon.size as f32,
        height: icon.size as f32,
    };
    let svg = unsafe {
        match SHCreateMemStream(Some(icon.svg.as_bytes())) {
            None => device_context5.CreateSvgDocument(None, size)?,
            Some(stream) => device_context5.CreateSvgDocument(&stream, size)?,
        }
    };
    _ = set_svg_color(&svg, color);
    Ok(svg)
}


#[derive(Default)]
pub struct Props {
    pub menu_list: Vec<MenuInfo>,
}

/// Why `track_menu` returned. Lets the in-crate menu bar drive hover / keyboard
/// switching between top-level menus. Standalone popups only ever observe `Ended`.
/// `pub(crate)` — an implementation detail of the menu-bar component.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum TrackExit {
    /// A command was posted (`WM_COMMAND`) or the menu was dismissed — the bar stops.
    Ended,
    /// The pointer moved onto / clicked the owner bar at this screen point. The bar
    /// hit-tests it to decide: switch to a sibling label, or toggle the open one shut.
    YieldMouse(POINT),
    /// Left was pressed at the top level — the bar should open the previous label.
    YieldKeyPrev,
    /// Right was pressed on a non-submenu top item — the bar should open the next label.
    YieldKeyNext,
}

impl QT {
    /// Open a dropdown / context menu at screen point `(x, y)` and track it modally.
    /// Chosen commands are posted to `parent_window` as `WM_COMMAND(command_id)`.
    pub fn open_menu(&self, parent_window: HWND, x: i32, y: i32, props: Props) -> Result<()> {
        self.open_menu_ex(parent_window, x, y, props, None, None, false)
            .map(|_| ())
    }

    /// Like [`open_menu`], but `x` is the menu's **right** edge (right-aligned) —
    /// the menu grows leftward from `x`. Used by the split button so the dropdown
    /// lines up with the button's right edge.
    pub fn open_menu_right_aligned(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<()> {
        self.open_menu_ex(parent_window, x, y, props, None, None, true)
            .map(|_| ())
    }

    /// Internal variant used by the in-crate menu bar. `owner_bar` (+ its open-label
    /// screen rect) makes the tracking loop *yield back* to the bar on bar hover /
    /// click / Left-Right, reporting *why* via [`TrackExit`] so the bar can
    /// hover-switch. With `owner_bar = None` this is exactly [`open_menu`]. Kept
    /// `pub(crate)` because `TrackExit` and the bar coupling are menu-bar internals.
    pub(crate) fn open_menu_ex(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
        owner_bar: Option<HWND>,
        owner_bar_open_rect: Option<RECT>,
        align_right: bool,
    ) -> Result<TrackExit> {
        unsafe {
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let window_class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    lpszClassName: CLASS_NAME,
                    style: CS_DROPSHADOW | CS_SAVEBITS | CS_DBLCLKS,
                    lpfnWndProc: Some(window_proc),
                    hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                    ..Default::default()
                };
                RegisterClassExW(&window_class);
            });
            if !IsWindow(Some(parent_window)).as_bool() {
                return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
            }
            let menu = Rc::new(RefCell::new(convert_menu_info_list_to_menu(props.menu_list)));
            init_popup(self.clone(), parent_window, menu.clone(), x, y, 0, 0, align_right)?;
            init_tracking(parent_window)?;
            let exit = track_menu(
                menu.clone(),
                0,
                0,
                parent_window,
                owner_bar,
                owner_bar_open_rect,
            );
            exit_tracking(parent_window)?;
            exit
        }
    }
}

pub struct CreateParams {
    qt: QT,
    menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
    x_anchor: i32,
    y_anchor: i32,
    align_right: bool,
}

fn init_popup(
    qt: QT,
    owning_window: HWND,
    menu: Rc<RefCell<Menu>>,
    x: i32,
    y: i32,
    x_anchor: i32,
    y_anchor: i32,
    align_right: bool,
) -> Result<()> {
    let boxed = Box::new(CreateParams {
        qt,
        menu: menu.clone(),
        owning_window,
        x_anchor,
        y_anchor,
        align_right,
    });
    let window = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED,
            CLASS_NAME,
            w!(""),
            WS_POPUP,
            x,
            y,
            0,
            0,
            Some(owning_window),
            None,
            Some(HINSTANCE(
                GetWindowLongPtrW(owning_window, GWLP_HINSTANCE) as _
            )),
            Some(Box::<CreateParams>::into_raw(boxed) as _),
        )
    }?;
    menu.borrow_mut().window = Some(window);
    Ok(())
}

fn init_tracking(owning_window: HWND) -> Result<()> {
    unsafe {
        _ = HideCaret(None);
        SendMessageW(
            owning_window,
            WM_ENTERMENULOOP,
            Some(WPARAM(TRUE.0 as usize)),
            None,
        );
        SendMessageW(
            owning_window,
            WM_SETCURSOR,
            Some(WPARAM(owning_window.0 as usize)),
            Some(LPARAM(HTCAPTION as isize)),
        );
    }
    Ok(())
}

struct Tracker {
    current_menu: Rc<RefCell<Menu>>,
    top_menu: Rc<RefCell<Menu>>,
    owning_window: HWND,
    point: POINT,
    /// Set when a menu bar owns this dropdown — enables the yield-to-bar branches.
    owner_bar: Option<HWND>,
    /// Screen rect of the open bar label (suppresses re-yield while hovering it).
    owner_bar_open_rect: Option<RECT>,
}

fn menu_from_point(root: Rc<RefCell<Menu>>, point: &POINT) -> Option<Rc<RefCell<Menu>>> {
    let menu = root.borrow();
    let mut result: Option<Rc<RefCell<Menu>>> = None;
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        if let MenuItem::SubMenu { sub_menu, .. } = item {
            result = menu_from_point(sub_menu.clone(), point)
        }
    }
    if let None = result {
        let hit_window = unsafe { WindowFromPoint(*point) };
        if menu.window == Some(hit_window) {
            result = Some(root.clone())
        }
    };
    result
}

fn adjust_menu_item_rect(menu: &Menu, rect: &RECT) -> RECT {
    let scroll_offset = if menu.is_scrolling {
        menu.scroll_position
    } else {
        0
    };
    let mut rect = rect.clone();
    unsafe {
        let _ = OffsetRect(
            &mut rect,
            menu.menu_list_rect.left,
            menu.menu_list_rect.top - scroll_offset,
        );
    }
    rect
}

#[derive(PartialEq)]
enum HitTest {
    Nowhere,
    Border,
    Item(usize),
    ScrollUp,
    ScrollDown,
}

fn find_item_by_coordinates(menu: &Menu, point: &mut POINT) -> HitTest {
    let mut rect = RECT::default();
    if let Some(window) = menu.window {
        unsafe {
            if GetWindowRect(window, &mut rect).is_err() {
                return HitTest::Nowhere;
            }
            if !PtInRect(&rect, *point).as_bool() {
                return HitTest::Nowhere;
            }

            point.x -= rect.left;
            point.y -= rect.top;

            let scaling_factor = get_scaling_factor(window);
            point.x = (point.x as f32 / scaling_factor) as i32;
            point.y = (point.y as f32 / scaling_factor) as i32;

            if !PtInRect(&menu.menu_list_rect, *point).as_bool() {
                if !menu.is_scrolling
                    || point.x < menu.menu_list_rect.left
                    || point.x >= menu.menu_list_rect.right
                {
                    return HitTest::Border;
                }

                // On a scroll arrow. Update point so that it points to the item just outside menu_list_rect
                return if point.y < menu.menu_list_rect.top {
                    point.y = menu.menu_list_rect.top - 1;
                    HitTest::ScrollUp
                } else {
                    point.y = menu.menu_list_rect.bottom;
                    HitTest::ScrollDown
                };
            }

            for (index, item) in menu.items.iter().enumerate() {
                match item {
                    MenuItem::MenuItem {
                        rect: item_rect, ..
                    }
                    | MenuItem::MenuItemRadio {
                        rect: item_rect, ..
                    }
                    | MenuItem::SubMenu {
                        rect: item_rect, ..
                    }
                    | MenuItem::MenuDivider {
                        rect: item_rect, ..
                    } => {
                        let rect = adjust_menu_item_rect(menu, item_rect);
                        if PtInRect(&rect, *point).as_bool() {
                            return HitTest::Item(index);
                        }
                    }
                }
            }
        }
    }
    HitTest::Nowhere
}

fn switch_tracking(menu: &mut Menu, new_index: usize) -> Result<()> {
    hide_sub_popups(menu)?;
    select_item(menu, Some(new_index));
    Ok(())
}

fn menu_button_down(context: &Context, mt: &mut Tracker, menu: &mut Menu) -> Result<bool> {
    let ht = find_item_by_coordinates(menu, &mut mt.point);
    if let HitTest::Item(item_index) = ht {
        if menu.focused_item_index != Some(item_index) {
            switch_tracking(menu, item_index)?;
        }
        let item = &menu.items[item_index];
        // A non-submenu, non-divider item shows a "pressed" fill while held.
        match item {
            MenuItem::SubMenu { sub_menu, .. } => {
                if sub_menu.borrow().window.is_none() {
                    mt.current_menu =
                        show_sub_popup(&context.qt, context.owning_window, sub_menu.clone())?;
                }
            }
            MenuItem::MenuDivider { .. } => {}
            _ => set_pressed(menu, Some(item_index)),
        }
    }

    match ht {
        HitTest::Nowhere => Ok(false),
        HitTest::Item(_) => Ok(true),
        _ => Ok(true),
    }
}

fn menu_button_up(context: &Context, mt: &mut Tracker, menu: &mut Menu) -> Result<ExecutionResult> {
    set_pressed(menu, None);
    if let HitTest::Item(item_index) = find_item_by_coordinates(menu, &mut mt.point) {
        if menu.focused_item_index == Some(item_index) {
            if let MenuItem::SubMenu { .. } = menu.items[item_index] {
            } else {
                let execution_result = execute_focused_item(context, mt, &menu)?;
                return if execution_result == ExecutionResult::NoExecuted
                    || execution_result == ExecutionResult::ShownPopup
                {
                    Ok(ExecutionResult::NoExecuted)
                } else {
                    Ok(execution_result)
                };
            }
        }
    }
    Ok(ExecutionResult::NoExecuted)
}

fn menu_mouse_move(context: &Context, mt: &mut Tracker, menu: Rc<RefCell<Menu>>) -> Result<bool> {
    let item_index_option = {
        let menu_borrow = menu.borrow_mut();
        find_item_by_coordinates(&menu_borrow, &mut mt.point)
    };

    if let HitTest::Item(item_index) = item_index_option {
        let (focused_item_index, pressed_active) = {
            let menu_borrow = menu.borrow();
            (
                menu_borrow.focused_item_index,
                menu_borrow.pressed_item_index.is_some(),
            )
        };

        if focused_item_index != Some(item_index) {
            {
                let mut menu_borrow = menu.borrow_mut();
                switch_tracking(&mut menu_borrow, item_index)?;
            }
            mt.current_menu = show_sub_popup(&context.qt, context.owning_window, menu)?;
        } else if pressed_active {
            // A held press follows the pointer onto whatever item it's over (unless
            // that's a submenu/divider, which don't press).
            let mut menu_borrow = menu.borrow_mut();
            let pressable = !matches!(
                menu_borrow.items[item_index],
                MenuItem::SubMenu { .. } | MenuItem::MenuDivider { .. }
            );
            set_pressed(&mut menu_borrow, pressable.then_some(item_index));
        }
    } else {
        let mut menu_borrow = menu.borrow_mut();
        hide_sub_popups(&mut menu_borrow)?;
        select_item(&mut menu_borrow, None);
        set_pressed(&mut menu_borrow, None);
    }

    Ok(true)
}

fn select_item(menu: &mut Menu, index: Option<usize>) {
    if menu.focused_item_index == index {
        return;
    }
    menu.focused_item_index = index;
    unsafe {
        if menu.window.is_some() {
            _ = RedrawWindow(menu.window, None, None, RDW_INVALIDATE | RDW_NOCHILDREN);
        }
    }
}

/// Set the pressed (pointer-down) item and repaint. `None` clears the press.
fn set_pressed(menu: &mut Menu, index: Option<usize>) {
    if menu.pressed_item_index == index {
        return;
    }
    menu.pressed_item_index = index;
    unsafe {
        if menu.window.is_some() {
            _ = RedrawWindow(menu.window, None, None, RDW_INVALIDATE | RDW_NOCHILDREN);
        }
    }
}

fn select_previous(menu: &mut Menu) {
    if let Some(mut item_index) = menu.focused_item_index {
        while item_index > 0 {
            item_index = item_index - 1;
            if let MenuItem::MenuDivider { .. } = menu.items[item_index] {
                continue;
            }
            select_item(menu, Some(item_index));
            break;
        }
    }
}

fn select_next(menu: &mut Menu) {
    if let Some(mut item_index) = menu.focused_item_index {
        while item_index + 1 < menu.items.len() {
            item_index = item_index + 1;
            if let MenuItem::MenuDivider { .. } = menu.items[item_index] {
                continue;
            }
            select_item(menu, Some(item_index));
            break;
        }
    }
}

fn select_first(menu: &mut Menu) {
    let mut item_index = 0;
    while item_index < menu.items.len() {
        if let MenuItem::MenuDivider { .. } = menu.items[item_index] {
            item_index = item_index + 1;
            continue;
        }
        select_item(menu, Some(item_index));
        break;
    }
}

fn select_last(menu: &mut Menu) {
    let mut item_index = menu.items.len() as isize - 1;
    while item_index >= 0 {
        if let MenuItem::MenuDivider { .. } = menu.items[item_index as usize] {
            item_index = item_index - 1;
            continue;
        }
        select_item(menu, Some(item_index as usize));
        break;
    }
}

fn menu_key_left(mt: &mut Tracker) -> Result<()> {
    let mut tmp_menu = mt.top_menu.clone();
    let mut prev_menu = mt.top_menu.clone();

    // close topmost popup
    while !Rc::ptr_eq(&tmp_menu, &mt.current_menu) {
        prev_menu = tmp_menu.clone();
        let prev_menu_borrowed = prev_menu.borrow();
        tmp_menu = get_sub_popup(&prev_menu_borrowed).unwrap();
    }

    {
        let mut prev_menu_borrowed = prev_menu.borrow_mut();
        hide_sub_popups(&mut prev_menu_borrowed)?;
    }
    mt.current_menu = prev_menu.clone();

    Ok(())
}

fn menu_key_right(context: &Context, mt: &mut Tracker) -> Result<()> {
    mt.current_menu = show_sub_popup(&context.qt, mt.owning_window, mt.current_menu.clone())?;
    Ok(())
}

fn get_sub_popup(menu: &Menu) -> Option<Rc<RefCell<Menu>>> {
    if let Some(item_index) = menu.focused_item_index {
        if let MenuItem::SubMenu { sub_menu, .. } = &menu.items[item_index] {
            let sub_menu_borrowed = sub_menu.borrow();
            if sub_menu_borrowed.window.is_some() {
                return Some(sub_menu.clone());
            }
        }
    }
    None
}

fn menu_key_escape(mt: &mut Tracker) -> Result<bool> {
    if !Rc::ptr_eq(&mt.current_menu, &mt.top_menu) {
        let mut top = mt.top_menu.clone();
        let mut prev_menu = top.clone();
        while !Rc::ptr_eq(&top, &mt.current_menu) {
            prev_menu = top;
            let prev_menu_borrowed = prev_menu.borrow();
            match get_sub_popup(&prev_menu_borrowed) {
                None => {
                    break;
                }
                Some(sub_popup) => {
                    top = sub_popup;
                }
            }
        }
        let mut prev_menu_borrowed = prev_menu.borrow_mut();
        hide_sub_popups(&mut prev_menu_borrowed)?;
        mt.current_menu = prev_menu.clone();
        return Ok(false);
    }
    Ok(true)
}

const MENU_MARGIN: i32 = 4;
const MENU_BORDER_WIDTH: i32 = 1;
const MENU_LIST_GAP: i32 = 2;
/// Checkmark glyph display size (px) for radio items — matches the dropdown's
/// check column (Fluent Option checkIcon, fontSizeBase400). The 20px source SVG is
/// scaled down to this.
const CHECK_SIZE: i32 = 16;
/// Left gutter reserved before a radio item's label (checkmark + a small gap).
const RADIO_GUTTER: i32 = CHECK_SIZE + 6;
/// Leading icon display size (px) for menu items that carry one.
const MENU_ICON_SIZE: i32 = 20;
/// Gap (px) around a menu item's leading icon (before and after it).
const MENU_ICON_GAP: i32 = 4;
/// Gutter a leading icon adds before the label: icon + a trailing gap.
const MENU_ICON_GUTTER: i32 = MENU_ICON_SIZE + MENU_ICON_GAP;

// Fade-in animation: alpha eases 0 -> 255 over tokens.duration_normal using the
// tokens.curve_decelerate_mid easing curve. (A bare fade wants a shorter duration
// than Fluent's slower slide+fade surface motion.)
const FADE_TIMER_ID: usize = 1;
const FADE_INTERVAL_MS: u32 = 8;

/// CSS cubic-bezier easing: time fraction `t` in [0,1] -> eased value, with
/// control points (c[0],c[1]) and (c[2],c[3]) and implied (0,0)/(1,1).
fn cubic_bezier(t: f64, c: [f64; 4]) -> f64 {
    let axis = |s: f64, a: f64, b: f64| {
        3.0 * (1.0 - s).powi(2) * s * a + 3.0 * (1.0 - s) * s * s * b + s.powi(3)
    };
    let (mut lo, mut hi, mut s) = (0.0, 1.0, t);
    for _ in 0..20 {
        s = 0.5 * (lo + hi);
        if axis(s, c[0], c[2]) < t {
            lo = s;
        } else {
            hi = s;
        }
    }
    axis(s, c[1], c[3])
}

#[derive(PartialEq)]
enum ExecutionResult {
    Executed = 0,
    NoExecuted = -1,
    ShownPopup = -2,
}

fn show_sub_popup(
    qt: &QT,
    owning_window: HWND,
    menu: Rc<RefCell<Menu>>,
) -> Result<Rc<RefCell<Menu>>> {
    {
        let menu = menu.borrow_mut();
        if let Some(focused_item_index) = menu.focused_item_index {
            let item = &menu.items[focused_item_index];
            if let MenuItem::SubMenu {
                sub_menu,
                rect: item_rect,
                ..
            } = item
            {
                if let Some(window) = menu.window {
                    let item_rect = adjust_menu_item_rect(&menu, &item_rect);
                    let mut rect = RECT::default();
                    unsafe {
                        GetWindowRect(window, &mut rect)?;
                    }
                    let scaling_factor = get_scaling_factor(window);
                    rect.left +=
                        ((item_rect.right - MENU_BORDER_WIDTH) as f32 * scaling_factor) as i32;
                    rect.top += (item_rect.top as f32 * scaling_factor) as i32;
                    rect.right = ((item_rect.left - item_rect.right + MENU_BORDER_WIDTH) as f32
                        * scaling_factor) as i32;
                    rect.bottom = ((item_rect.top - item_rect.bottom - 2 * MENU_MARGIN) as f32
                        * scaling_factor) as i32;
                    init_popup(
                        qt.clone(),
                        owning_window,
                        sub_menu.clone(),
                        rect.left,
                        rect.top,
                        rect.right,
                        rect.bottom,
                        false,
                    )?;
                    return Ok(sub_menu.clone());
                }
            }
        }
    }
    Ok(menu)
}

fn hide_sub_popups(menu: &mut Menu) -> Result<()> {
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        if let MenuItem::SubMenu { sub_menu, .. } = item {
            let mut sub_menu = sub_menu.borrow_mut();
            hide_sub_popups(&mut sub_menu)?;
            select_item(&mut sub_menu, None);
            if let Some(sub_menu_window) = sub_menu.window {
                unsafe { DestroyWindow(sub_menu_window)? };
                sub_menu.window = None;
            }
        }
    }
    Ok(())
}
fn execute_focused_item(
    context: &Context,
    mt: &mut Tracker,
    menu: &Menu,
) -> Result<ExecutionResult> {
    if let Some(focused_item_index) = menu.focused_item_index {
        let item = &menu.items[focused_item_index];
        match item {
            MenuItem::MenuItem { id, disabled, .. } => unsafe {
                if *disabled {
                    Ok(ExecutionResult::NoExecuted)
                } else {
                    PostMessageW(
                        Some(mt.owning_window),
                        WM_COMMAND,
                        WPARAM(*id as usize),
                        LPARAM(0),
                    )?;
                    Ok(ExecutionResult::Executed)
                }
            },
            MenuItem::MenuItemRadio { id, disabled, .. } => unsafe {
                // Radios activate exactly like a normal item; the app updates the
                // checkmark on the next open.
                if *disabled {
                    Ok(ExecutionResult::NoExecuted)
                } else {
                    PostMessageW(
                        Some(mt.owning_window),
                        WM_COMMAND,
                        WPARAM(*id as usize),
                        LPARAM(0),
                    )?;
                    Ok(ExecutionResult::Executed)
                }
            },
            MenuItem::SubMenu { sub_menu, .. } => {
                mt.current_menu =
                    show_sub_popup(&context.qt, context.owning_window, sub_menu.clone())?;
                Ok(ExecutionResult::ShownPopup)
            }
            MenuItem::MenuDivider { .. } => Ok(ExecutionResult::NoExecuted),
        }
    } else {
        Ok(ExecutionResult::NoExecuted)
    }
}

fn track_menu(
    menu: Rc<RefCell<Menu>>,
    x: i32,
    y: i32,
    owning_window: HWND,
    owner_bar: Option<HWND>,
    owner_bar_open_rect: Option<RECT>,
) -> Result<TrackExit> {
    let window = {
        let menu = menu.borrow();
        if menu.window.is_none() {
            return Err(Error::from(ERROR_INVALID_WINDOW_HANDLE));
        }
        menu.window.unwrap()
    };

    unsafe {
        SetCapture(window);
        let mut mt = Tracker {
            current_menu: menu.clone(),
            top_menu: menu.clone(),
            owning_window,
            point: POINT { x, y },
            owner_bar,
            owner_bar_open_rect,
        };
        let mut exit_menu = false;
        let mut enter_idle_sent = false;
        let mut track_exit = TrackExit::Ended;
        while !exit_menu {
            let mut msg = MSG::default();
            loop {
                if PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE).into() {
                    if !CallMsgFilterW(&msg, MSGF_MENU as i32).as_bool() {
                        break;
                    }
                    _ = PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
                } else {
                    if !enter_idle_sent {
                        enter_idle_sent = true;
                        SendMessageW(
                            owning_window,
                            WM_ENTERIDLE,
                            Some(WPARAM(MSGF_MENU as usize)),
                            Some(LPARAM(window.0 as _)),
                        );
                    }
                    MsgWaitForMultipleObjectsEx(
                        None,
                        0xffffffff,
                        QS_ALLINPUT,
                        MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS::default(),
                    );
                }
            }

            if msg.message == WM_CANCELMODE {
                _ = PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
                break;
            }

            mt.point = msg.pt;
            if msg.hwnd == window || msg.message != WM_TIMER {
                enter_idle_sent = false;
            }

            let mut remove_message = false;
            if msg.message >= WM_MOUSEFIRST && msg.message <= WM_MOUSELAST {
                mt.point.x = msg.lParam.0 as i16 as i32;
                mt.point.y = (msg.lParam.0 >> 16) as i16 as i32;
                _ = ClientToScreen(window, &mut mt.point);

                // Yield-to-bar (revived Wine menu-bar branch, inert without owner_bar):
                // when the pointer is on the owning menu bar, hand control back so the
                // bar can hover-switch. A move that stays within the already-open
                // label does NOT yield (no flicker); a click always does (toggle/switch).
                let bar_yield = mt.owner_bar.is_some_and(|bar| {
                    let mut bar_rect = RECT::default();
                    if GetWindowRect(bar, &mut bar_rect).is_err()
                        || !PtInRect(&bar_rect, mt.point).as_bool()
                    {
                        return false;
                    }
                    match msg.message {
                        WM_MOUSEMOVE => !mt
                            .owner_bar_open_rect
                            .is_some_and(|r| PtInRect(&r, mt.point).as_bool()),
                        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => true,
                        _ => false,
                    }
                });

                if bar_yield {
                    track_exit = TrackExit::YieldMouse(mt.point);
                    exit_menu = true;
                    remove_message = true;
                } else {
                    let menu_from_point_result = menu_from_point(menu.clone(), &mt.point);

                    match msg.message {
                        WM_RBUTTONDBLCLK | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK | WM_LBUTTONDOWN => {
                            remove_message = match menu_from_point_result {
                                None => false,
                                Some(menu_from_point) => {
                                    let mut menu_from_point_borrowed = menu_from_point.borrow_mut();
                                    let raw =
                                        GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                                    let context = &*raw;
                                    menu_button_down(context, &mut mt, &mut menu_from_point_borrowed)?
                                }
                            };
                            exit_menu = !remove_message;
                        }
                        WM_RBUTTONUP | WM_LBUTTONUP => match menu_from_point_result {
                            Some(menu_from_point) => {
                                let mut menu_from_point_borrowed = menu_from_point.borrow_mut();
                                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                                let context = &*raw;
                                let execution_result =
                                    menu_button_up(context, &mut mt, &mut menu_from_point_borrowed)?;
                                remove_message = execution_result != ExecutionResult::NoExecuted;
                                exit_menu = remove_message;
                            }
                            None => exit_menu = false,
                        },
                        WM_MOUSEMOVE => {
                            if let Some(menu_from_point) = menu_from_point_result {
                                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                                let context = &*raw;
                                exit_menu =
                                    exit_menu | !menu_mouse_move(context, &mut mt, menu_from_point)?
                            }
                        }
                        _ => {}
                    }
                }
            } else if msg.message >= WM_KEYFIRST && msg.message <= WM_KEYLAST {
                remove_message = true;
                match msg.message {
                    WM_KEYDOWN | WM_SYSKEYDOWN => match VIRTUAL_KEY(msg.wParam.0 as u16) {
                        VK_MENU | VK_F10 => {
                            exit_menu = true;
                        }
                        VK_HOME => {
                            let mut menu = mt.current_menu.borrow_mut();
                            select_first(&mut menu)
                        }
                        VK_END => {
                            let mut menu = mt.current_menu.borrow_mut();
                            select_last(&mut menu)
                        }
                        VK_UP => {
                            let mut menu = mt.current_menu.borrow_mut();
                            select_previous(&mut menu)
                        }
                        VK_DOWN => {
                            let mut menu = mt.current_menu.borrow_mut();
                            select_next(&mut menu)
                        }
                        VK_LEFT => {
                            // At the bar's top level, Left walks to the previous bar
                            // label instead of closing a (nonexistent) submenu.
                            if mt.owner_bar.is_some()
                                && Rc::ptr_eq(&mt.current_menu, &mt.top_menu)
                            {
                                track_exit = TrackExit::YieldKeyPrev;
                                exit_menu = true;
                            } else {
                                menu_key_left(&mut mt)?
                            }
                        }
                        VK_RIGHT => {
                            // At the bar's top level, Right opens the focused submenu if
                            // there is one; otherwise it walks to the next bar label.
                            let at_bar_top = mt.owner_bar.is_some()
                                && Rc::ptr_eq(&mt.current_menu, &mt.top_menu);
                            let focused_is_submenu = at_bar_top && {
                                let m = mt.current_menu.borrow();
                                m.focused_item_index.is_some_and(|i| {
                                    matches!(m.items[i], MenuItem::SubMenu { .. })
                                })
                            };
                            if at_bar_top && !focused_is_submenu {
                                track_exit = TrackExit::YieldKeyNext;
                                exit_menu = true;
                            } else {
                                let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
                                let context = &*raw;
                                menu_key_right(context, &mut mt)?
                            }
                        }
                        VK_ESCAPE => exit_menu = menu_key_escape(&mut mt)?,
                        _ => {
                            let _ = TranslateMessage(&mut msg);
                        }
                    },
                    _ => {}
                }
            } else {
                if PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                continue;
            }

            if !exit_menu {
                remove_message = true;
            }

            if remove_message {
                _ = PeekMessageW(&mut msg, None, msg.message, msg.message, PM_REMOVE);
            }
        }

        ReleaseCapture()?;
        if IsWindow(Some(mt.owning_window)).as_bool() {
            {
                let mut top_menu = mt.top_menu.borrow_mut();
                hide_sub_popups(&mut top_menu)?;
            }
            {
                DestroyWindow(window)?;
                let mut menu_mut = menu.borrow_mut();
                menu_mut.window = None;
            }
            {
                let mut top_menu = mt.top_menu.borrow_mut();
                select_item(&mut top_menu, None);
            }
        }
        Ok(track_exit)
    }
}

fn exit_tracking(owning_window: HWND) -> Result<()> {
    unsafe {
        SendMessageW(
            owning_window,
            WM_EXITMENULOOP,
            Some(WPARAM(TRUE.0 as usize)),
            None,
        );
        _ = ShowCaret(None);
    }
    Ok(())
}

fn calc_menu_item_size(
    qt: &QT,
    menu_item: &mut MenuItem,
    org_x: i32,
    org_y: i32,
    text_format: &IDWriteTextFormat,
) -> Result<()> {
    let tokens = &qt.theme.tokens;
    unsafe {
        match menu_item {
            MenuItem::MenuItem {
                rect,
                text,
                secondary_text,
                icon,
                ..
            } => {
                let _ = SetRect(rect, org_x, org_y, org_x, org_y);
                let direct_write_factory = &qt.dwrite_factory;
                let text_layout = direct_write_factory.CreateTextLayout(
                    text.as_wide(),
                    text_format,
                    290f32,
                    500f32,
                )?;
                let mut metrics = DWRITE_TEXT_METRICS::default();
                text_layout.GetMetrics(&mut metrics)?;
                let mut content_width = metrics.width.ceil() as i32;
                if icon.is_some() {
                    content_width += MENU_ICON_GUTTER;
                }
                // Reserve room for the shortcut hint + a gap after the label.
                if let Some(secondary) = secondary_text {
                    let secondary_layout = direct_write_factory.CreateTextLayout(
                        secondary.as_wide(),
                        text_format,
                        290f32,
                        500f32,
                    )?;
                    let mut secondary_metrics = DWRITE_TEXT_METRICS::default();
                    secondary_layout.GetMetrics(&mut secondary_metrics)?;
                    content_width += secondary_metrics.width.ceil() as i32
                        + 6 * tokens.spacing_vertical_s_nudge as i32;
                }
                rect.right += content_width + 2 * tokens.spacing_vertical_s_nudge as i32;
                rect.bottom += (metrics.height.ceil() as i32
                    + 2 * tokens.spacing_vertical_s_nudge as i32)
                    .max(32);
            }
            MenuItem::MenuItemRadio {
                rect,
                text,
                secondary_text,
                icon,
                ..
            } => {
                // Same as MenuItem, plus a left gutter for the checkmark column.
                let _ = SetRect(rect, org_x, org_y, org_x, org_y);
                let direct_write_factory = &qt.dwrite_factory;
                let text_layout = direct_write_factory.CreateTextLayout(
                    text.as_wide(),
                    text_format,
                    290f32,
                    500f32,
                )?;
                let mut metrics = DWRITE_TEXT_METRICS::default();
                text_layout.GetMetrics(&mut metrics)?;
                let mut content_width = metrics.width.ceil() as i32 + RADIO_GUTTER;
                if icon.is_some() {
                    content_width += MENU_ICON_GUTTER;
                }
                if let Some(secondary) = secondary_text {
                    let secondary_layout = direct_write_factory.CreateTextLayout(
                        secondary.as_wide(),
                        text_format,
                        290f32,
                        500f32,
                    )?;
                    let mut secondary_metrics = DWRITE_TEXT_METRICS::default();
                    secondary_layout.GetMetrics(&mut secondary_metrics)?;
                    content_width += secondary_metrics.width.ceil() as i32
                        + 6 * tokens.spacing_vertical_s_nudge as i32;
                }
                rect.right += content_width + 2 * tokens.spacing_vertical_s_nudge as i32;
                rect.bottom += (metrics.height.ceil() as i32
                    + 2 * tokens.spacing_vertical_s_nudge as i32)
                    .max(32);
            }
            MenuItem::SubMenu { rect, text, .. } => {
                let _ = SetRect(rect, org_x, org_y, org_x, org_y);
                let direct_write_factory = &qt.dwrite_factory;
                let text_layout = direct_write_factory.CreateTextLayout(
                    text.as_wide(),
                    text_format,
                    290f32,
                    500f32,
                )?;
                let mut metrics = DWRITE_TEXT_METRICS::default();
                text_layout.GetMetrics(&mut metrics)?;
                rect.right +=
                    metrics.width.ceil() as i32 + 2 * tokens.spacing_vertical_s_nudge as i32;
                rect.bottom += (metrics.height.ceil() as i32
                    + 2 * tokens.spacing_vertical_s_nudge as i32)
                    .max(32);
            }
            MenuItem::MenuDivider { rect } => {
                let _ = SetRect(rect, org_x, org_y, org_x, org_y);
                rect.bottom += 4 + tokens.stroke_width_thin as i32;
            }
        }
    }
    if let MenuItem::SubMenu { rect, .. } = menu_item {
        rect.right = rect.right + 4 + 20;
    }
    Ok(())
}

fn get_text_format(qt: &QT) -> Result<IDWriteTextFormat> {
    unsafe {
        let direct_write_factory = &qt.dwrite_factory;
        let tokens = &qt.theme.tokens;
        direct_write_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base300,
            w!(""),
        )
    }
}

fn calc_popup_menu_size(qt: &QT, menu: &mut Menu, max_height: i32) -> Result<(i32, i32)> {
    unsafe {
        let _ = SetRectEmpty(&mut menu.menu_list_rect);
    }
    let mut start = 0;
    let text_format = get_text_format(qt)?;
    while start < menu.items.len() {
        let org_x = menu.menu_list_rect.right;
        let mut org_y = menu.menu_list_rect.top;

        let mut i = start;
        while i < menu.items.len() {
            let item = &mut menu.items[i];
            calc_menu_item_size(qt, item, org_x, org_y, &text_format)?;
            let desired_width = match item {
                MenuItem::MenuItem { rect, .. }
                | MenuItem::MenuItemRadio { rect, .. }
                | MenuItem::SubMenu { rect, .. }
                | MenuItem::MenuDivider { rect } => rect.right,
            };
            let desired_height = match item {
                MenuItem::MenuItem { rect, .. }
                | MenuItem::MenuItemRadio { rect, .. }
                | MenuItem::SubMenu { rect, .. }
                | MenuItem::MenuDivider { rect } => rect.bottom,
            };

            menu.menu_list_rect.right = menu.menu_list_rect.right.max(desired_width);
            org_y = desired_height + MENU_LIST_GAP;

            i = i + 1;
        }
        org_y -= MENU_LIST_GAP;
        menu.menu_list_rect.right = menu.menu_list_rect.right.max(138);
        while start < i {
            let item = &mut menu.items[start];
            match item {
                MenuItem::MenuItem { rect, .. }
                | MenuItem::MenuItemRadio { rect, .. }
                | MenuItem::SubMenu { rect, .. }
                | MenuItem::MenuDivider { rect } => rect.right = menu.menu_list_rect.right,
            }
            start = start + 1;
        }
        menu.menu_list_rect.bottom = menu.menu_list_rect.bottom.max(org_y);
    }

    unsafe {
        let _ = OffsetRect(
            &mut menu.menu_list_rect,
            MENU_BORDER_WIDTH + MENU_MARGIN,
            MENU_BORDER_WIDTH + MENU_MARGIN,
        );
    }
    let mut height = menu.menu_list_rect.bottom + MENU_BORDER_WIDTH + MENU_MARGIN;
    let width = menu.menu_list_rect.right + MENU_BORDER_WIDTH + MENU_MARGIN;
    if height >= max_height {
        height = max_height;
        menu.is_scrolling = true;
        menu.menu_list_rect.top = MENU_MARGIN;
        menu.menu_list_rect.bottom = height - MENU_MARGIN;
    }

    Ok((width, height))
}

fn show_popup(
    qt: &QT,
    window: HWND,
    menu: &mut Menu,
    x: i32,
    y: i32,
    x_anchor: i32,
    y_anchor: i32,
    align_right: bool,
) -> Result<()> {
    menu.focused_item_index = None;
    let pt = POINT { x, y };
    let monitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut info);
    }
    let max_height = info.rcWork.bottom - info.rcWork.top;
    let (width, height) = calc_popup_menu_size(qt, menu, max_height)?;
    // width/height are DIPs, but x/y, the work area and the anchors are device
    // pixels — and the window is placed at device-pixel size. Do all positioning
    // math in device pixels so right-align and the edge clamps stay correct at
    // non-100% DPI.
    let scaling_factor = get_scaling_factor(window);
    let scaled_width = (width as f32 * scaling_factor) as i32;
    let scaled_height = (height as f32 * scaling_factor) as i32;
    // Right-aligned: the passed x is the menu's right edge; grow leftward.
    let mut x = if align_right { x - scaled_width } else { x };
    if x + scaled_width > info.rcWork.right {
        if x_anchor != 0 && x >= scaled_width - x_anchor {
            x = x - scaled_width - x_anchor;
        }
        if x + scaled_width > info.rcWork.right {
            x = info.rcWork.right - scaled_width;
        }
    }
    if x < info.rcWork.left {
        x = info.rcWork.left;
    }
    let mut y = y;
    if y + scaled_height > info.rcWork.bottom {
        if y_anchor != 0 && y >= scaled_height + y_anchor {
            y -= scaled_height + y_anchor;
        }
        if y + scaled_height > info.rcWork.bottom {
            y = info.rcWork.bottom - scaled_height;
        }
    }
    if y < info.rcWork.top {
        y = info.rcWork.top;
    }
    let corner_diameter = (qt.theme.tokens.border_radius_medium * 2f32 * scaling_factor) as i32;
    unsafe {
        // Start fully transparent (before it's shown, so there's no flash),
        // then fade in via WM_TIMER.
        _ = SetLayeredWindowAttributes(window, COLORREF(0), 0, LWA_ALPHA);
        SetWindowPos(
            window,
            Some(HWND_TOPMOST),
            x,
            y,
            scaled_width,
            scaled_height,
            SWP_SHOWWINDOW | SWP_NOACTIVATE,
        )?;
        let region = CreateRoundRectRgn(
            0,
            0,
            scaled_width + 1,
            scaled_height + 1,
            corner_diameter,
            corner_diameter,
        );
        SetWindowRgn(window, Some(region), false);
        SetTimer(Some(window), FADE_TIMER_ID, FADE_INTERVAL_MS, None);
    }
    Ok(())
}

/// Draw a menu item's leading icon at `icon_x`, tinted `color`, scaled to
/// `MENU_ICON_SIZE` and vertically centered between `top` and `bottom` (item rect
/// bounds, device px).
fn draw_menu_icon(
    context: &Context,
    svg: &ID2D1SvgDocument,
    color: &D2D1_COLOR_F,
    icon_x: f32,
    top: i32,
    bottom: i32,
) -> Result<()> {
    _ = set_svg_color(svg, color);
    unsafe {
        let dc5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
        let vp = svg.GetViewportSize();
        let scale = MENU_ICON_SIZE as f32 / vp.width;
        let icon_y = top as f32 + ((bottom - top) as f32 - MENU_ICON_SIZE as f32) / 2.0;
        dc5.SetTransform(&Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: icon_x,
            M32: icon_y,
        });
        dc5.DrawSvgDocument(svg);
        dc5.SetTransform(&Matrix3x2::identity());
    }
    Ok(())
}

fn draw_menu_item(
    menu: &Menu,
    menu_item: &MenuItem,
    context: &Context,
    focused: bool,
    pressed: bool,
    icon_svg: Option<&ID2D1SvgDocument>,
) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    let rect = match menu_item {
        MenuItem::MenuItem {
            rect: item_rect, ..
        }
        | MenuItem::MenuItemRadio {
            rect: item_rect, ..
        }
        | MenuItem::SubMenu {
            rect: item_rect, ..
        }
        | MenuItem::MenuDivider { rect: item_rect } => adjust_menu_item_rect(menu, item_rect),
    };
    // Highlight fill: pressed takes precedence over focus/hover.
    if focused || pressed {
        let show_fill = match menu_item {
            MenuItem::MenuItem { disabled, .. }
            | MenuItem::MenuItemRadio { disabled, .. } => !*disabled,
            MenuItem::SubMenu { .. } => true,
            MenuItem::MenuDivider { .. } => false,
        };
        unsafe {
            if show_fill {
                let fill = if pressed {
                    tokens.color_neutral_background1_pressed
                } else {
                    tokens.color_neutral_background1_hover
                };
                let fill_brush = context.render_target.CreateSolidColorBrush(&fill, None)?;
                let rounded_rect = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: rect.left as f32,
                        top: rect.top as f32,
                        right: rect.right as f32,
                        bottom: rect.bottom as f32,
                    },
                    radiusX: tokens.border_radius_medium,
                    radiusY: tokens.border_radius_medium,
                };
                context
                    .render_target
                    .FillRoundedRectangle(&rounded_rect, &fill_brush);
            }
        }
    }
    unsafe {
        // Leading-icon tint: disabled color when disabled; brand-pressed when
        // pressed; brand-hover when focused; else the neutral foreground2 label color.
        let icon_color = |disabled: bool| -> &D2D1_COLOR_F {
            if disabled {
                &tokens.color_neutral_foreground_disabled
            } else if pressed {
                &tokens.color_neutral_foreground2_brand_pressed
            } else if focused {
                &tokens.color_neutral_foreground2_brand_hover
            } else {
                &tokens.color_neutral_foreground2
            }
        };
        // Label brush by state (mirrors the icon tint's precedence).
        let text_brush_for = |disabled: bool| -> &ID2D1SolidColorBrush {
            if disabled {
                &context.text_disabled_brush
            } else if pressed {
                &context.text_pressed_brush
            } else if focused {
                &context.text_focused_brush
            } else {
                &context.text_brush
            }
        };
        match menu_item {
            MenuItem::MenuItem {
                text,
                disabled,
                secondary_text,
                ..
            } => {
                let nudge = tokens.spacing_vertical_s_nudge;
                // Leading icon (drawn 20×20, vertically centered); the label shifts
                // right past it when present.
                let text_left = if let Some(svg) = icon_svg {
                    let icon_x = rect.left as f32 + nudge;
                    draw_menu_icon(context, svg, icon_color(*disabled), icon_x, rect.top, rect.bottom)?;
                    icon_x + MENU_ICON_GUTTER as f32
                } else {
                    rect.left as f32 + nudge
                };
                let text_rect = D2D_RECT_F {
                    left: text_left,
                    top: rect.top as f32 + nudge,
                    right: rect.right as f32 - nudge,
                    bottom: rect.bottom as f32 - nudge,
                };
                let text_brush = text_brush_for(*disabled);
                context.render_target.DrawText(
                    text.as_wide(),
                    &context.text_format,
                    &text_rect,
                    text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                if let Some(secondary) = secondary_text {
                    let secondary_brush = if *disabled {
                        &context.text_disabled_brush
                    } else {
                        &context.secondary_text_brush
                    };
                    context.render_target.DrawText(
                        secondary.as_wide(),
                        &context.secondary_text_format,
                        &text_rect,
                        secondary_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
            }
            MenuItem::MenuItemRadio {
                text,
                checked,
                disabled,
                secondary_text,
                ..
            } => {
                // Label indented past the checkmark gutter; otherwise identical to
                // MenuItem. The gutter holds a checkmark when this radio is selected.
                // With an icon the row is: checkmark · 4px · icon · 4px · text.
                let nudge = tokens.spacing_vertical_s_nudge;
                let check_x = rect.left as f32 + nudge;
                let text_left = if icon_svg.is_some() {
                    let icon_x = check_x + (CHECK_SIZE + MENU_ICON_GAP) as f32;
                    if let Some(svg) = icon_svg {
                        draw_menu_icon(context, svg, icon_color(*disabled), icon_x, rect.top, rect.bottom)?;
                    }
                    icon_x + MENU_ICON_GUTTER as f32
                } else {
                    check_x + RADIO_GUTTER as f32
                };
                let text_rect = D2D_RECT_F {
                    left: text_left,
                    top: rect.top as f32 + nudge,
                    right: rect.right as f32 - nudge,
                    bottom: rect.bottom as f32 - nudge,
                };
                let text_brush = text_brush_for(*disabled);
                context.render_target.DrawText(
                    text.as_wide(),
                    &context.text_format,
                    &text_rect,
                    text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                if let Some(secondary) = secondary_text {
                    let secondary_brush = if *disabled {
                        &context.text_disabled_brush
                    } else {
                        &context.secondary_text_brush
                    };
                    context.render_target.DrawText(
                        secondary.as_wide(),
                        &context.secondary_text_format,
                        &text_rect,
                        secondary_brush,
                        D2D1_DRAW_TEXT_OPTIONS_NONE,
                        DWRITE_MEASURING_MODE_NATURAL,
                    );
                }
                if *checked {
                    // Checkmark in the gutter, vertically centered — 20px source SVG
                    // scaled to CHECK_SIZE, exactly like the dropdown's check column.
                    let check_y =
                        rect.top as f32 + ((rect.bottom - rect.top - CHECK_SIZE) as f32 / 2.0);
                    let scale = CHECK_SIZE as f32 / 20.0;
                    let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
                    device_context5.SetTransform(&Matrix3x2 {
                        M11: scale,
                        M12: 0.0,
                        M21: 0.0,
                        M22: scale,
                        M31: check_x,
                        M32: check_y,
                    });
                    device_context5.DrawSvgDocument(&context.checkmark_svg);
                    device_context5.SetTransform(&Matrix3x2::identity());
                }
            }
            MenuItem::SubMenu { text, .. } => {
                let text_rect = D2D_RECT_F {
                    left: rect.left as f32 + tokens.spacing_vertical_s_nudge,
                    top: rect.top as f32 + tokens.spacing_vertical_s_nudge,
                    right: (rect.right - 4 - 20) as f32 - tokens.spacing_vertical_s_nudge,
                    bottom: rect.bottom as f32 - tokens.spacing_vertical_s_nudge,
                };
                context.render_target.DrawText(
                    text.as_wide(),
                    &context.text_format,
                    &text_rect,
                    &context.text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                let device_context5 = context.render_target.cast::<ID2D1DeviceContext5>()?;
                device_context5.SetTransform(&Matrix3x2::translation(
                    rect.right as f32 - tokens.spacing_vertical_s_nudge - 4f32 - 20f32,
                    rect.top as f32 + tokens.spacing_vertical_s_nudge,
                ));
                let svg = if focused {
                    &context.sub_menu_indicator_focused_svg
                } else {
                    &context.sub_menu_indicator_svg
                };
                device_context5.DrawSvgDocument(svg);
                device_context5.SetTransform(&Matrix3x2::identity());
            }
            MenuItem::MenuDivider { .. } => {
                let start = Vector2 {
                    X: (rect.left - MENU_MARGIN) as f32,
                    Y: rect.top as f32 + 2.0,
                };
                let end = Vector2 {
                    X: (rect.right + MENU_MARGIN) as f32,
                    Y: rect.top as f32 + 2.0,
                };
                let divider_brush = context
                    .render_target
                    .CreateSolidColorBrush(&tokens.color_neutral_stroke2, None)?;
                context.render_target.DrawLine(
                    start,
                    end,
                    &divider_brush,
                    tokens.stroke_width_thin,
                    None,
                );
            }
        }
    }
    Ok(())
}

fn draw_scroll_arrows(_window: HWND, _context: &Context) -> Result<()> {
    // TODO
    Ok(())
}

fn draw_popup_menu(window: HWND, context: &Context) -> Result<()> {
    let tokens = &context.qt.theme.tokens;
    unsafe {
        context.render_target.BeginDraw();
        context
            .render_target
            .Clear(Some(&tokens.color_neutral_background1));
    }
    let menu = context.menu.borrow();
    for (index, item) in menu.items.iter().enumerate() {
        let icon_svg = context.item_icon_svgs.get(index).and_then(|o| o.as_ref());
        draw_menu_item(
            &menu,
            item,
            context,
            Some(index) == menu.focused_item_index,
            Some(index) == menu.pressed_item_index,
            icon_svg,
        )?;
    }
    if menu.is_scrolling {
        draw_scroll_arrows(window, context)?;
    }
    unsafe {
        context.render_target.EndDraw(None, None)?;
    }
    Ok(())
}

fn on_create(window: HWND, params: CreateParams, x: i32, y: i32) -> Result<Context> {
    {
        let mut menu = params.menu.borrow_mut();
        show_popup(
            &params.qt,
            window,
            &mut menu,
            x,
            y,
            params.x_anchor,
            params.y_anchor,
            params.align_right,
        )?;
    }

    unsafe {
        let mut client_rect = RECT::default();
        GetClientRect(window, &mut client_rect)?;
        let dpi = GetDpiForWindow(window);
        let factory = &params.qt.d2d_factory;
        let render_target = factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES {
                dpiX: dpi as f32,
                dpiY: dpi as f32,
                ..Default::default()
            },
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: window,
                pixelSize: D2D_SIZE_U {
                    width: (client_rect.right - client_rect.left) as u32,
                    height: (client_rect.bottom - client_rect.top) as u32,
                },
                presentOptions: Default::default(),
            },
        )?;
        let text_format = get_text_format(&params.qt)?;
        let tokens = &params.qt.theme.tokens;
        let text_brush =
            render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground2, None)?;
        let text_focused_brush =
            render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground1_hover, None)?;
        let text_pressed_brush =
            render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground1_pressed, None)?;
        let text_disabled_brush =
            render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground_disabled, None)?;
        // Secondary content (shortcut hints): base200, right-aligned, foreground3.
        let secondary_text_format = params.qt.dwrite_factory.CreateTextFormat(
            tokens.font_family_base,
            None,
            tokens.font_weight_regular,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            tokens.font_size_base200,
            w!(""),
        )?;
        secondary_text_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
        secondary_text_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        let secondary_text_brush =
            render_target.CreateSolidColorBrush(&tokens.color_neutral_foreground3, None)?;
        let device_context5 = render_target.cast::<ID2D1DeviceContext5>()?;
        let sub_menu_indicator_icon = Icon::chevron_right_20_regular();
        let sub_menu_indicator_svg =
            match SHCreateMemStream(Some(sub_menu_indicator_icon.svg.as_bytes())) {
                None => device_context5.CreateSvgDocument(
                    None,
                    D2D_SIZE_F {
                        width: sub_menu_indicator_icon.size as f32,
                        height: sub_menu_indicator_icon.size as f32,
                    },
                )?,
                Some(svg_stream) => device_context5.CreateSvgDocument(
                    &svg_stream,
                    D2D_SIZE_F {
                        width: sub_menu_indicator_icon.size as f32,
                        height: sub_menu_indicator_icon.size as f32,
                    },
                )?,
            };
        _ = set_svg_color(&sub_menu_indicator_svg, &tokens.color_neutral_foreground2);
        let sub_menu_indicator_focused_icon = Icon::chevron_right_20_filled();
        let sub_menu_indicator_focused_svg =
            match SHCreateMemStream(Some(sub_menu_indicator_focused_icon.svg.as_bytes())) {
                None => device_context5.CreateSvgDocument(
                    None,
                    D2D_SIZE_F {
                        width: sub_menu_indicator_focused_icon.size as f32,
                        height: sub_menu_indicator_focused_icon.size as f32,
                    },
                )?,
                Some(svg_stream) => device_context5.CreateSvgDocument(
                    &svg_stream,
                    D2D_SIZE_F {
                        width: sub_menu_indicator_focused_icon.size as f32,
                        height: sub_menu_indicator_focused_icon.size as f32,
                    },
                )?,
            };
        _ = set_svg_color(
            &sub_menu_indicator_focused_svg,
            &tokens.color_neutral_foreground2,
        );
        // Leading checkmark for checked radio items — same glyph the dropdown uses.
        let checkmark_icon = Icon::checkmark_20_filled();
        let checkmark_svg = match SHCreateMemStream(Some(checkmark_icon.svg.as_bytes())) {
            None => device_context5.CreateSvgDocument(
                None,
                D2D_SIZE_F {
                    width: checkmark_icon.size as f32,
                    height: checkmark_icon.size as f32,
                },
            )?,
            Some(svg_stream) => device_context5.CreateSvgDocument(
                &svg_stream,
                D2D_SIZE_F {
                    width: checkmark_icon.size as f32,
                    height: checkmark_icon.size as f32,
                },
            )?,
        };
        _ = set_svg_color(&checkmark_svg, &tokens.color_neutral_foreground2);

        // Per-item leading icons (parallel to menu.items), tinted like the text.
        let item_icon_svgs: Vec<Option<ID2D1SvgDocument>> = {
            let menu = params.menu.borrow();
            menu.items
                .iter()
                .map(|item| {
                    let icon = match item {
                        MenuItem::MenuItem { icon, .. }
                        | MenuItem::MenuItemRadio { icon, .. } => icon.as_ref(),
                        _ => None,
                    };
                    icon.and_then(|ic| {
                        create_icon_svg(&device_context5, ic, &tokens.color_neutral_foreground2).ok()
                    })
                })
                .collect()
        };

        Ok(Context {
            qt: params.qt,
            menu: params.menu,
            owning_window: params.owning_window,
            render_target,
            text_format,
            text_brush,
            text_focused_brush,
            text_pressed_brush,
            text_disabled_brush,
            secondary_text_format,
            secondary_text_brush,
            sub_menu_indicator_svg,
            sub_menu_indicator_focused_svg,
            checkmark_svg,
            item_icon_svgs,
            fade_elapsed_ms: Cell::new(0),
        })
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
            let raw = (*cs).lpCreateParams as *mut CreateParams;
            let params = Box::<CreateParams>::from_raw(raw);
            match on_create(window, *params, (*cs).x, (*cs).y) {
                Ok(context) => {
                    let boxed = Box::new(context);
                    SetWindowLongPtrW(window, GWLP_USERDATA, Box::<Context>::into_raw(boxed) as _);
                    LRESULT(TRUE.0 as isize)
                }
                Err(_) => LRESULT(FALSE.0 as isize),
            }
        },
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_TIMER if w_param.0 == FADE_TIMER_ID => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let tokens = &context.qt.theme.tokens;
            let duration_ms = (tokens.duration_normal * 1000.0) as u32;
            let elapsed = (context.fade_elapsed_ms.get() + FADE_INTERVAL_MS).min(duration_ms);
            context.fade_elapsed_ms.set(elapsed);
            let t = elapsed as f64 / duration_ms as f64;
            let eased = cubic_bezier(t, tokens.curve_decelerate_mid);
            let alpha = (eased * 255.0).round() as u8;
            _ = SetLayeredWindowAttributes(window, COLORREF(0), alpha, LWA_ALPHA);
            if elapsed >= duration_ms {
                _ = KillTimer(Some(window), FADE_TIMER_ID);
            }
            LRESULT(0)
        },
        WM_PAINT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            let mut ps = PAINTSTRUCT::default();
            BeginPaint(window, &mut ps);
            _ = draw_popup_menu(window, context);
            _ = EndPaint(window, &ps);
            LRESULT(0)
        },
        WM_PRINTCLIENT => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            let context = &*raw;
            _ = draw_popup_menu(window, context);
            LRESULT(0)
        },
        WM_ERASEBKGND => LRESULT(1),
        WM_DESTROY => unsafe {
            let raw = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut Context;
            _ = Box::<Context>::from_raw(raw);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
