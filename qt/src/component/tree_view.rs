//! A hierarchical list — Win32 `SysTreeView32`, Fluent-styled. Rows are the
//! flattened set of currently-visible nodes; expand/collapse shows/hides subtrees.
//! Children are fetched lazily via an `on_expand(path)` callback the first time a
//! node expands (Win32 `TVN_ITEMEXPANDING` model). The row rendering, selection,
//! accent animation, and scrollbar host wiring are the same pattern as `list_box`;
//! the chevron twisty reuses `Icon::chevron_right_20_regular`, rotated on expand.

use std::cell::RefCell;
use std::mem::size_of;
use std::sync::Once;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_COLOR_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, ID2D1HwndRenderTarget, ID2D1PathGeometry1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateRoundRectRgn, EndPaint, InvalidateRect, PAINTSTRUCT, RDW_INVALIDATE,
    RedrawWindow, SetWindowRgn,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Animation::{
    IUIAnimationManager, IUIAnimationTimer, IUIAnimationTimerEventHandler,
    IUIAnimationTimerEventHandler_Impl, IUIAnimationTimerUpdateHandler,
    IUIAnimationTransitionFactory, IUIAnimationTransitionLibrary, IUIAnimationVariable,
    UI_ANIMATION_IDLE_BEHAVIOR_DISABLE, UIAnimationManager, UIAnimationTimer,
};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use crate::sys::dpi_for_window;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VIRTUAL_KEY, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE,
    VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;
use windows_numerics::Matrix3x2;

use crate::component::input;
use crate::component::scroll::{SCROLLBAR_W, ScrollHit, VScroll};
use crate::icon::Icon;
use crate::icon::path::build_geometry;
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
/// Indentation added per tree depth level (DIPs).
const INDENT_W: f32 = 16.0;
/// The chevron glyph size (DIPs) — the Fluent TreeItem chevron is 12×12.
const CHEVRON_GLYPH: f32 = 12.0;
/// Left margin before the first-level chevron (past the accent bar).
const CHEVRON_MARGIN: f32 = 4.0;
/// Gap between the chevron and the row text (DIPs).
const CHEVRON_GAP: f32 = 4.0;

/// One node in the tree. `has_children` shows a chevron even before children are
/// loaded; children are fetched lazily via `Props::on_expand` on first expand.
pub struct Node {
    pub text: PCWSTR,
    pub has_children: bool,
    children: Option<Vec<Node>>,
    expanded: bool,
}

impl Node {
    /// A leaf (no children, no chevron).
    pub fn leaf(text: PCWSTR) -> Self {
        Node { text, has_children: false, children: None, expanded: false }
    }
    /// A branch whose children are loaded lazily on first expand.
    pub fn branch(text: PCWSTR) -> Self {
        Node { text, has_children: true, children: None, expanded: false }
    }
    /// A branch with children supplied up front.
    pub fn with_children(text: PCWSTR, children: Vec<Node>) -> Self {
        Node { text, has_children: true, children: Some(children), expanded: false }
    }
}

/// A flattened, currently-visible row.
struct VisibleRow {
    path: Vec<usize>,
    depth: usize,
    text: PCWSTR,
    has_children: bool,
    expanded: bool,
}

pub struct MouseEvent {
    /// Fired when the selection changes, with the selected node's path.
    pub on_select: Box<dyn Fn(&HWND, &[usize])>,
}

impl Default for MouseEvent {
    fn default() -> Self {
        MouseEvent {
            on_select: Box::new(|_, _| {}),
        }
    }
}

pub struct Props {
    pub roots: Vec<Node>,
    /// Lazily supplies a node's children the first time it expands, keyed by path.
    pub on_expand: Box<dyn Fn(&[usize]) -> Vec<Node>>,
    pub width: i32,
    pub height: i32,
    pub size: input::Size,
    pub mouse_event: MouseEvent,
    pub background: Option<D2D1_COLOR_F>,
}

impl Default for Props {
    fn default() -> Self {
        Props {
            roots: Vec::new(),
            on_expand: Box::new(|_| Vec::new()),
            width: 0,
            height: 0,
            size: input::Size::Medium,
            mouse_event: MouseEvent::default(),
            background: None,
        }
    }
}

struct State {
    qt: QT,
    width: f32,
    height: f32,
    size: input::Size,
    background: Option<D2D1_COLOR_F>,
    on_select: Box<dyn Fn(&HWND, &[usize])>,
    on_expand: Box<dyn Fn(&[usize]) -> Vec<Node>>,
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
    fn row_gap(&self) -> f32 {
        self.qt.theme.tokens.spacing_vertical_xxs
    }
    fn row_slot(&self) -> f32 {
        self.row_height() + self.row_gap()
    }
}

struct Context {
    state: State,
    text_format: IDWriteTextFormat,
    render_target: ID2D1HwndRenderTarget,
    roots: Vec<Node>,
    visible: Vec<VisibleRow>,
    selected: Option<Vec<usize>>,
    hovered: Option<usize>,
    is_focused: bool,
    is_hovered: bool,
    scroll: VScroll,
    animation_manager: IUIAnimationManager,
    animation_timer: IUIAnimationTimer,
    transition_library: IUIAnimationTransitionLibrary,
    transition_factory: IUIAnimationTransitionFactory,
    accent_height: IUIAnimationVariable,
    /// Lazily-built chevron geometry, tinted at draw time and rotated per-row.
    chevron_geometry: RefCell<Option<ID2D1PathGeometry1>>,
    /// Animated rotation (degrees) of the chevron currently expanding/collapsing.
    chevron_rotation: IUIAnimationVariable,
    /// The path of the node whose chevron is mid-animation (others draw static).
    animating_path: Option<Vec<usize>>,
}

impl Context {
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

    /// Visible-row index at a client-DIP y, or None if past the last row / in a gap.
    fn row_at(&self, y: f32) -> Option<usize> {
        let c = self.content_rect();
        let rel = y - c.top + self.scroll.offset();
        if rel < 0.0 {
            return None;
        }
        let slot = self.state.row_slot();
        let i = (rel / slot) as usize;
        if rel - i as f32 * slot > self.state.row_height() {
            return None;
        }
        if i < self.visible.len() { Some(i) } else { None }
    }

    /// Left DIP where a row's chevron glyph box starts, for the given depth.
    fn glyph_x(&self, depth: usize) -> f32 {
        let pad = self.state.qt.theme.tokens.spacing_horizontal_xs;
        self.content_rect().left + CHEVRON_MARGIN + pad + depth as f32 * INDENT_W
    }

    /// Left DIP where a row's text starts, for the given depth.
    fn text_x(&self, depth: usize) -> f32 {
        self.glyph_x(depth) + CHEVRON_GLYPH + CHEVRON_GAP
    }

    /// The flat visible index of the currently-selected path, if visible.
    fn selected_row(&self) -> Option<usize> {
        let sel = self.selected.as_ref()?;
        self.visible.iter().position(|r| r.path == *sel)
    }
}

// --- tree model helpers ---

fn node_at_path<'a>(roots: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let mut nodes = roots;
    let mut node = None;
    for &idx in path {
        let n = nodes.get(idx)?;
        node = Some(n);
        nodes = n.children.as_deref().unwrap_or(&[]);
    }
    node
}

fn node_at_path_mut<'a>(roots: &'a mut [Node], path: &[usize]) -> Option<&'a mut Node> {
    let mut nodes = roots;
    let mut node = None;
    for &idx in path {
        let n = nodes.get_mut(idx)?;
        node = Some(n as *mut Node);
        // SAFETY: reborrow through the raw pointer to continue descending.
        nodes = unsafe { (*node.unwrap()).children.as_deref_mut().unwrap_or(&mut []) };
    }
    node.map(|p| unsafe { &mut *p })
}

fn flatten(roots: &[Node]) -> Vec<VisibleRow> {
    fn walk(nodes: &[Node], prefix: &mut Vec<usize>, depth: usize, out: &mut Vec<VisibleRow>) {
        for (i, node) in nodes.iter().enumerate() {
            prefix.push(i);
            out.push(VisibleRow {
                path: prefix.clone(),
                depth,
                text: node.text,
                has_children: node.has_children,
                expanded: node.expanded,
            });
            if node.expanded {
                if let Some(children) = &node.children {
                    walk(children, prefix, depth + 1, out);
                }
            }
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    walk(roots, &mut prefix, 0, &mut out);
    out
}

fn rebuild_visible(context: &mut Context) {
    context.visible = flatten(&context.roots);
    context.hovered = None;
}

/// Transform placing the 12×12 chevron at `(gx, gy)`, rotated `angle` degrees
/// clockwise about its own centre. D2D row-vector convention (`[x y 1] * M`):
/// local point → rotate about local centre (6,6) → place at screen centre.
fn chevron_matrix(gx: f32, gy: f32, angle: f32) -> Matrix3x2 {
    let r = angle.to_radians();
    let (c, s) = (r.cos(), r.sin());
    let half = CHEVRON_GLYPH / 2.0;
    let cx = gx + half; // screen centre
    let cy = gy + half;
    Matrix3x2 {
        M11: c,
        M12: s,
        M21: -s,
        M22: c,
        M31: cx - half * (c - s),
        M32: cy - half * (s + c),
    }
}

impl QT {
    pub fn create_tree_view(
        &self,
        parent_window: HWND,
        x: i32,
        y: i32,
        props: Props,
    ) -> Result<HWND> {
        let class_name: PCWSTR = w!("QT_TREE_VIEW");
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
            let width = if props.width > 0 { props.width as f32 / scaling_factor } else { 260.0 };
            let height = if props.height > 0 { props.height as f32 / scaling_factor } else { 220.0 };
            let roots = props.roots;
            let boxed = Box::new((
                State {
                    qt: self.clone(),
                    width,
                    height,
                    size: props.size,
                    background: props.background,
                    on_select: props.mouse_event.on_select,
                    on_expand: props.on_expand,
                },
                roots,
            ));
            CreateWindowExW(
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
                Some(Box::<(State, Vec<Node>)>::into_raw(boxed) as _),
            )
        }
    }

    /// The current selection path, or None.
    pub fn tree_view_selection(&self, tree_view: HWND) -> Option<Vec<usize>> {
        unsafe {
            let raw = GetWindowLongPtrW(tree_view, GWLP_USERDATA) as *const Context;
            if raw.is_null() { None } else { (*raw).selected.clone() }
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
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }
}

fn on_create(window: HWND, state: State, roots: Vec<Node>) -> Result<Context> {
    let font_size = state.font_size();
    unsafe {
        let text_format = create_text_format(&state.qt, font_size)?;
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

        let animation_timer: IUIAnimationTimer =
            CoCreateInstance(&UIAnimationTimer, None, CLSCTX_INPROC_SERVER)?;
        let transition_library = state.qt.transition_library.clone();
        let transition_factory = state.qt.transition_factory.clone();
        let animation_manager: IUIAnimationManager =
            CoCreateInstance(&UIAnimationManager, None, CLSCTX_INPROC_SERVER)?;
        let timer_update_handler = animation_manager.cast::<IUIAnimationTimerUpdateHandler>()?;
        animation_timer
            .SetTimerUpdateHandler(&timer_update_handler, UI_ANIMATION_IDLE_BEHAVIOR_DISABLE)?;
        let timer_event_handler: IUIAnimationTimerEventHandler =
            AnimationTimerEventHandler { window }.into();
        animation_timer.SetTimerEventHandler(&timer_event_handler)?;
        let accent_height = animation_manager.CreateAnimationVariable(ACCENT_H as f64)?;
        let chevron_rotation = animation_manager.CreateAnimationVariable(0.0)?;

        let visible = flatten(&roots);
        Ok(Context {
            state,
            text_format,
            render_target,
            roots,
            visible,
            selected: None,
            hovered: None,
            is_focused: false,
            is_hovered: false,
            scroll: VScroll::new(),
            animation_manager,
            animation_timer,
            transition_library,
            transition_factory,
            accent_height,
            chevron_geometry: RefCell::new(None),
            chevron_rotation,
            animating_path: None,
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

fn animate_accent(context: &mut Context, target: f64, duration: f64) -> Result<()> {
    let curve = context.state.qt.theme.tokens.curve_easy_ease;
    unsafe {
        let transition = if duration <= 0.0 {
            context.transition_library.CreateInstantaneousTransition(target)?
        } else {
            crate::anim::cubic_bezier_linear_transition(
                &context.transition_factory,
                duration,
                target,
                curve,
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

/// Animate the chevron of `path` toward `expanded` (0°→90° or 90°→0°) with the
/// Fluent easyEaseMax curve. Records `animating_path` so paint applies the live
/// angle only to that row.
fn animate_chevron(context: &mut Context, path: Vec<usize>, expanded: bool) -> Result<()> {
    let curve = context.state.qt.theme.tokens.curve_easy_ease_max;
    let duration = context.state.qt.theme.tokens.duration_normal;
    let target = if expanded { 90.0 } else { 0.0 };
    unsafe {
        // Start from the opposite angle so the ease plays over the full sweep.
        let start = if expanded { 0.0 } else { 90.0 };
        context.chevron_rotation = context.animation_manager.CreateAnimationVariable(start)?;
        let transition = crate::anim::cubic_bezier_linear_transition(
            &context.transition_factory,
            duration,
            target,
            curve,
        )?;
        let seconds_now = context.animation_timer.GetTime()?;
        context.animation_manager.ScheduleTransition(
            &context.chevron_rotation,
            &transition,
            seconds_now,
        )?;
    }
    context.animating_path = Some(path);
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
    let n = context.visible.len() as f32;
    let content_h = (n * slot - context.state.row_gap()).max(0.0);
    let c = context.content_rect();
    let viewport_h = c.bottom - c.top;
    context.scroll.set_metrics(content_h, viewport_h, slot);
}

fn ensure_row_visible(context: &mut Context, i: usize) {
    let slot = context.state.row_slot();
    let rh = context.state.row_height();
    context.scroll.ensure_visible(i as f32 * slot, i as f32 * slot + rh);
}

/// Select the visible row `i` (by its path), fire on_select, scroll into view.
fn select_row(window: HWND, context: &mut Context, i: usize) {
    let path = context.visible[i].path.clone();
    context.selected = Some(path.clone());
    ensure_row_visible(context, i);
    (context.state.on_select)(&window, &path);
    unsafe {
        _ = InvalidateRect(Some(window), None, false);
    }
}

/// Toggle expand/collapse of the visible row `i` (lazy-loads children on expand).
fn toggle_row(window: HWND, context: &mut Context, i: usize) {
    let path = context.visible[i].path.clone();
    let (has_children, expanded, loaded) = {
        let node = match node_at_path(&context.roots, &path) {
            Some(n) => n,
            None => return,
        };
        (node.has_children, node.expanded, node.children.is_some())
    };
    if !has_children {
        return;
    }
    if expanded {
        if let Some(node) = node_at_path_mut(&mut context.roots, &path) {
            node.expanded = false;
        }
        // If the selection was inside the collapsed subtree, move it to this node.
        if let Some(sel) = &context.selected {
            if sel.len() > path.len() && sel[..path.len()] == path[..] {
                context.selected = Some(path.clone());
            }
        }
    } else {
        if !loaded {
            let kids = (context.state.on_expand)(&path);
            if let Some(node) = node_at_path_mut(&mut context.roots, &path) {
                node.children = Some(kids);
            }
        }
        if let Some(node) = node_at_path_mut(&mut context.roots, &path) {
            node.expanded = true;
        }
    }
    // Animate the chevron toward its new state (0°→90° expand, 90°→0° collapse).
    _ = animate_chevron(context, path.clone(), !expanded);
    rebuild_visible(context);
    update_metrics(context);
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

        // --- rows (clipped to the padding box, like list_box/textarea) ---
        let content = context.content_rect();
        let offset = context.scroll.offset();
        let rh = state.row_height();
        let slot = state.row_slot();
        let row_right = content.right;
        let selected_row = context.selected_row();
        context.render_target.PushAxisAlignedClip(
            &D2D_RECT_F {
                left: content.left,
                top: stroke,
                right: content.right,
                bottom: height - stroke,
            },
            D2D1_ANTIALIAS_MODE_ALIASED,
        );

        // Chevron geometry (lazily built once). The tint is applied at draw time
        // via a solid brush using the constant neutral token.
        if context.chevron_geometry.borrow().is_none() {
            let icon = Icon::chevron_right_12_regular();
            let geometry = build_geometry(&state.qt.d2d_factory, &icon)?;
            *context.chevron_geometry.borrow_mut() = Some(geometry);
        }
        let chevron_brush = context
            .render_target
            .CreateSolidColorBrush(&tokens.color_neutral_foreground3, None)?;

        for (i, vr) in context.visible.iter().enumerate() {
            let top = content.top + i as f32 * slot - offset;
            let bottom = top + rh;
            if bottom < 0.0 || top > height {
                continue; // offscreen
            }
            let is_selected = selected_row == Some(i);
            let is_hovered = context.hovered == Some(i);

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

            // Brand accent bar on the selected row (animated height).
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

            // Chevron twisty (branches only) — points right (0°), rotates to point
            // down (90°) when expanded; animated on the toggling row.
            if vr.has_children {
                if let Some(geometry) = context.chevron_geometry.borrow().as_ref() {
                    let gx = context.glyph_x(vr.depth);
                    let gy = top + (rh - CHEVRON_GLYPH) / 2.0;
                    let is_animating = context.animating_path.as_ref() == Some(&vr.path);
                    let angle = if is_animating {
                        context.chevron_rotation.GetValue()? as f32
                    } else if vr.expanded {
                        90.0
                    } else {
                        0.0
                    };
                    context
                        .render_target
                        .SetTransform(&chevron_matrix(gx, gy, angle));
                    context
                        .render_target
                        .FillGeometry(geometry, &chevron_brush, None);
                    context.render_target.SetTransform(&Matrix3x2::identity());
                }
            }

            // Row text (indented past the chevron column).
            let text_brush = context
                .render_target
                .CreateSolidColorBrush(&tokens.color_neutral_foreground1, None)?;
            context.render_target.DrawText(
                vr.text.as_wide(),
                &context.text_format,
                &D2D_RECT_F {
                    left: context.text_x(vr.depth),
                    top,
                    right: row_right - tokens.spacing_horizontal_xs,
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
            let raw = (*cs).lpCreateParams as *mut (State, Vec<Node>);
            let boxed = Box::<(State, Vec<Node>)>::from_raw(raw);
            let (state, roots) = *boxed;
            match on_create(window, state, roots) {
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
                // Click in the chevron zone toggles; elsewhere selects.
                let (depth, has_children) =
                    (context.visible[i].depth, context.visible[i].has_children);
                let gx = context.glyph_x(depth);
                if has_children && px >= gx && px <= gx + CHEVRON_GLYPH {
                    toggle_row(window, context, i);
                } else {
                    select_row(window, context, i);
                    _ = animate_accent(context, ACCENT_PRESSED_H as f64, 0.0);
                }
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
            // Double-click a branch row toggles it (classic Win32).
            if context.scroll.on_l_button_down(px, py, context.track_rect()) == ScrollHit::Miss {
                if let Some(i) = context.row_at(py) {
                    if context.visible[i].has_children {
                        toggle_row(window, context, i);
                    }
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
            let cur = context.selected_row();
            match VIRTUAL_KEY(w_param.0 as u16) {
                VK_UP => {
                    let to = match cur {
                        Some(i) if i > 0 => Some(i - 1),
                        None if !context.visible.is_empty() => Some(0),
                        _ => None,
                    };
                    if let Some(i) = to {
                        select_row(window, context, i);
                    }
                }
                VK_DOWN => {
                    let to = match cur {
                        Some(i) if i + 1 < context.visible.len() => Some(i + 1),
                        None if !context.visible.is_empty() => Some(0),
                        _ => None,
                    };
                    if let Some(i) = to {
                        select_row(window, context, i);
                    }
                }
                VK_HOME => {
                    if !context.visible.is_empty() {
                        select_row(window, context, 0);
                    }
                }
                VK_END => {
                    if !context.visible.is_empty() {
                        select_row(window, context, context.visible.len() - 1);
                    }
                }
                VK_PRIOR => {
                    if let Some(i) = cur {
                        let to = i.saturating_sub(page_rows(context));
                        select_row(window, context, to);
                    } else if !context.visible.is_empty() {
                        select_row(window, context, 0);
                    }
                }
                VK_NEXT => {
                    if let Some(i) = cur {
                        let to = (i + page_rows(context)).min(context.visible.len() - 1);
                        select_row(window, context, to);
                    } else if !context.visible.is_empty() {
                        select_row(window, context, 0);
                    }
                }
                VK_RIGHT => {
                    if let Some(i) = cur {
                        let vr = &context.visible[i];
                        if vr.has_children && !vr.expanded {
                            toggle_row(window, context, i);
                        } else if vr.has_children && vr.expanded && i + 1 < context.visible.len() {
                            select_row(window, context, i + 1); // into first child
                        }
                    }
                }
                VK_LEFT => {
                    if let Some(i) = cur {
                        let vr = &context.visible[i];
                        if vr.has_children && vr.expanded {
                            toggle_row(window, context, i);
                        } else if vr.depth > 0 {
                            // Move to parent (path minus last segment).
                            let parent: Vec<usize> =
                                vr.path[..vr.path.len() - 1].to_vec();
                            if let Some(pi) =
                                context.visible.iter().position(|r| r.path == parent)
                            {
                                select_row(window, context, pi);
                            }
                        }
                    }
                }
                VK_RETURN | VK_SPACE => {
                    if let Some(i) = cur {
                        if context.visible[i].has_children {
                            toggle_row(window, context, i);
                        }
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
            let new_dpi = dpi_for_window(window);
            context.render_target.SetDpi(new_dpi as f32, new_dpi as f32);
            update_metrics(context);
            _ = InvalidateRect(Some(window), None, false);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(window, message, w_param, l_param) },
    }
}
