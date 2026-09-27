//! A real [`UiRuntime`]-backed preview window paired with the inspector —
//! the counterpart to [`crate::preview`]'s bootstrap, CSS-only fixture
//! loop, for an app that already has a real component tree the way
//! [`florui_platform::run`] does.
//!
//! `wgpu`/`egui` stay devtools-only dependencies, so this re-implements
//! `florui_platform::desktop`'s private window/surface/event-loop shape
//! rather than exporting it from there.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::rc::Rc;

use florui::Element;
use florui_layout::{BoxLayout, SizeCause, absolute_position, compute_size_causes};
use florui_platform::UiRuntime;
use florui_style::{Arena, ComputedStyle, Display, Edges, NodeId, Rgba, StyleError};
use taffy::prelude::*;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::inspector::{ContentBox, Inspector, InspectorAction, InspectorModel, InspectorNode};
use crate::scene::{ElementBox, outline_rect};

const SELECTION_HIGHLIGHT: crate::color::Rgba = crate::color::Rgba::opaque(250, 204, 21);
/// Outline around whatever's under the cursor while "Pick element" is
/// armed, distinct from [`SELECTION_HIGHLIGHT`].
const PICK_HOVER_HIGHLIGHT: crate::color::Rgba = crate::color::Rgba::opaque(56, 189, 248);
/// Drawn at the selected node's own padding box, nested inside
/// [`SELECTION_HIGHLIGHT`]'s border-box outline, only when that node's
/// own `overflow_clips` is set — the padding box is real CSS's own clip
/// boundary (see `ComputedStyle::overflow_clips`'s own doc), distinct
/// from the border box the selection outline already traces.
const CLIP_HIGHLIGHT: crate::color::Rgba = crate::color::Rgba::opaque(217, 70, 239);

#[derive(Debug)]
pub enum LiveError {
    EventLoop(winit::error::EventLoopError),
    Stylesheet(StyleError),
    WindowCreation(winit::error::OsError),
    SurfaceCreation(softbuffer::SoftBufferError),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveError::EventLoop(err) => write!(f, "event loop failed: {err}"),
            LiveError::Stylesheet(err) => write!(f, "stylesheet failed to parse: {err}"),
            LiveError::WindowCreation(err) => write!(f, "window could not be created: {err}"),
            LiveError::SurfaceCreation(err) => {
                write!(f, "render surface could not be created: {err}")
            }
        }
    }
}

impl std::error::Error for LiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LiveError::EventLoop(err) => Some(err),
            LiveError::Stylesheet(err) => Some(err),
            LiveError::WindowCreation(err) => Some(err),
            LiveError::SurfaceCreation(err) => Some(err),
        }
    }
}

enum UserEvent {
    Dirty,
}

/// Opens a preview window titled `title` over `root`, paired with a second
/// inspector window sharing the same event loop and [`UiRuntime`]. `css` is
/// parsed once; it does not get watched for changes.
///
/// Blocks the calling thread until the preview window closes.
pub fn run(
    title: &str,
    css: &str,
    canvas_color: Rgba,
    root: impl Fn() -> Element + 'static,
) -> Result<(), LiveError> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .map_err(LiveError::EventLoop)?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut host = LiveHost::new(
        title.to_string(),
        canvas_color,
        css.to_string(),
        Box::new(root),
        event_loop.create_proxy(),
    );
    event_loop
        .run_app(&mut host)
        .map_err(LiveError::EventLoop)?;
    match host.fatal_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn display_name(display: Display) -> &'static str {
    match display {
        Display::Block => "block",
        Display::Flex => "flex",
        Display::Inline => "inline",
        Display::InlineBlock => "inline-block",
        Display::Grid => "grid",
    }
}

/// Flattens `arena` pre-order into real [`InspectorNode`]s.
fn build_inspector_model(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    selected: Option<NodeId>,
    picking: bool,
) -> InspectorModel {
    // Diagnostic-only, opted into here rather than in the hot preview
    // paint path — see `compute_size_causes`'s own doc.
    let causes = compute_size_causes(arena, styles, layouts);
    let mut nodes = Vec::new();
    let mut stack: Vec<(NodeId, usize)> =
        arena.roots().iter().rev().map(|&root| (root, 0)).collect();
    while let Some((id, depth)) = stack.pop() {
        push_node(arena, styles, layouts, &causes, id, depth, &mut nodes);
        stack.extend(
            arena
                .children(id)
                .iter()
                .rev()
                .map(|&child| (child, depth + 1)),
        );
    }
    InspectorModel {
        nodes,
        selected,
        picking,
        stale: false,
    }
}

fn format_size_cause(cause: &SizeCause) -> String {
    match cause {
        SizeCause::MinContentClampedWidth { intrinsic_width } => format!(
            "width held to this element's own content — flex-shrink wanted it narrower than \
             its natural {intrinsic_width:.0}px"
        ),
        SizeCause::MinContentClampedHeight { intrinsic_height } => format!(
            "height held to this element's own content — flex-shrink wanted it shorter than \
             its natural {intrinsic_height:.0}px"
        ),
        SizeCause::GridTrackNarrowerThanContent { intrinsic_width } => format!(
            "width held narrower than this element's own content ({intrinsic_width:.0}px) by \
             the grid track it landed in"
        ),
    }
}

fn push_node(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    causes: &HashMap<NodeId, SizeCause>,
    id: NodeId,
    depth: usize,
    out: &mut Vec<InspectorNode>,
) {
    let style = styles
        .get(&id)
        .expect("compute() resolves a ComputedStyle for every arena node");
    let content = layouts.get(&id).map(|layout| {
        let (x, y) = absolute_position(arena, layouts, id);
        ContentBox {
            x,
            y,
            width: layout.width,
            height: layout.height,
        }
    });
    let border = Edges {
        top: style.border.top.width,
        right: style.border.right.width,
        bottom: style.border.bottom.width,
        left: style.border.left.width,
    };

    out.push(InspectorNode {
        id,
        depth,
        tag: arena.tag(id).to_string(),
        display: display_name(style.display).to_string(),
        background: style.background_color,
        z_index: style.z_index,
        opacity: style.opacity,
        overflow_clips: style.overflow_clips,
        content,
        padding: style.padding,
        border,
        margin: style.margin,
        size_cause: causes.get(&id).map(format_size_cause),
        diagnostics: node_diagnostics(arena, id),
    });
}

/// Markup problems the engine silently worked around — see
/// [`InspectorNode::diagnostics`].
fn node_diagnostics(arena: &Arena, id: NodeId) -> Vec<String> {
    let mut messages = Vec::new();
    if let Some(role) = arena.unsupported_role(id) {
        messages.push(format!(
            "role=\"{role}\" is not supported and was ignored (only \"switch\", on a checkbox \
             input, is understood)"
        ));
    }
    messages
}

/// A node's border box, in the same physical-pixel space the preview
/// paints in. [`InspectorNode::content`] already *is* this box despite
/// its name — `BoxLayout::width`/`height` are Taffy's own final
/// border-box size, padding and border already folded in, not the
/// smaller inner content box — see [`padding_box_rect`]'s own doc for
/// the real numbers that confirmed it. This function previously added
/// padding and border on top of `content` as if it still needed them,
/// inflating every selection/hover outline well past the node's own real
/// edges.
fn border_box_rect(node: &InspectorNode) -> Option<ElementBox> {
    let content = node.content?;
    Some(ElementBox {
        x: content.x.max(0.0).round() as u32,
        y: content.y.max(0.0).round() as u32,
        width: content.width.max(0.0).round() as u32,
        height: content.height.max(0.0).round() as u32,
    })
}

/// A node's padding box — content plus padding, excluding its border —
/// real CSS's own clip boundary for `overflow_clips`, see
/// [`CLIP_HIGHLIGHT`]'s own doc. Despite its name, [`InspectorNode::content`]
/// already holds the node's whole *border* box (`BoxLayout::width`/
/// `height` are Taffy's own final border-box size, confirmed directly
/// against a real laid-out node with both padding and border: a 40x20
/// content box with padding 8/6/5/7 and a 3px left / 2px top border comes
/// back as `BoxLayout { width: 57, height: 34 }`, exactly content +
/// padding + border) — so unlike [`border_box_rect`], which needs no
/// adjustment at all, the padding box here only has to shed the border.
fn padding_box_rect(node: &InspectorNode) -> Option<ElementBox> {
    let content = node.content?;
    let x = (content.x + node.border.left).max(0.0);
    let y = (content.y + node.border.top).max(0.0);
    let width = (content.width - node.border.left - node.border.right).max(0.0);
    let height = (content.height - node.border.top - node.border.bottom).max(0.0);
    Some(ElementBox {
        x: x.round() as u32,
        y: y.round() as u32,
        width: width.round() as u32,
        height: height.round() as u32,
    })
}

/// Owns both windows, the `softbuffer` surface, and the event loop;
/// delegates every rendering, hit-testing, and dispatch decision to one
/// shared [`UiRuntime`].
struct LiveHost {
    title: String,
    canvas_color: Rgba,
    css: String,
    root: Option<Box<dyn Fn() -> Element>>,
    proxy: EventLoopProxy<UserEvent>,
    runtime: Option<UiRuntime>,
    pressed: Option<NodeId>,
    selected: Option<NodeId>,
    picking: bool,
    /// Tracked separately from `UiRuntime`'s own `:hover` state, purely to
    /// draw [`PICK_HOVER_HIGHLIGHT`] while picking.
    hovered: Option<NodeId>,
    last_cursor: (f64, f64),
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    _context: Option<softbuffer::Context<Rc<Window>>>,
    inspector: Option<Inspector>,
    fatal_error: Option<LiveError>,
}

impl LiveHost {
    fn new(
        title: String,
        canvas_color: Rgba,
        css: String,
        root: Box<dyn Fn() -> Element>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            title,
            canvas_color,
            css,
            root: Some(root),
            proxy,
            runtime: None,
            pressed: None,
            selected: None,
            picking: false,
            hovered: None,
            last_cursor: (0.0, 0.0),
            window: None,
            surface: None,
            _context: None,
            inspector: None,
            fatal_error: None,
        }
    }

    fn viewport_size(&self) -> Size<AvailableSpace> {
        let size = self
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .unwrap_or_default();
        Size {
            width: AvailableSpace::Definite(size.width as f32),
            height: AvailableSpace::Definite(size.height as f32),
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: LiveError) {
        eprintln!("florui-devtools: {error}");
        self.fatal_error = Some(error);
        event_loop.exit();
    }

    fn inspector_model(&self) -> Option<InspectorModel> {
        let runtime = self.runtime.as_ref()?;
        let (arena, styles, layouts) = runtime.geometry();
        Some(build_inspector_model(
            arena,
            styles,
            layouts,
            self.selected,
            self.picking,
        ))
    }

    fn redraw(&mut self) {
        let (Some(window), Some(runtime)) = (self.window.clone(), self.runtime.as_mut()) else {
            return;
        };
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };

        let (arena, styles, layouts, font) = runtime.geometry_and_font_mut();
        let canvas = florui_paint::paint_to_buffer(
            font,
            size.width,
            size.height,
            self.canvas_color,
            arena,
            styles,
            layouts,
            1.0,
        );
        let mut pixels: Vec<u32> = canvas
            .pixels()
            .iter()
            .map(|p| u32::from_be_bytes([0, p.red(), p.green(), p.blue()]))
            .collect();

        if let Some(model) = self.inspector_model() {
            if let Some(selected) = self.selected
                && let Some(node) = model.nodes.iter().find(|node| node.id == selected)
                && let Some(rect) = border_box_rect(node)
            {
                outline_rect(
                    &mut pixels,
                    size.width,
                    size.height,
                    rect,
                    SELECTION_HIGHLIGHT,
                    2,
                );
                if node.overflow_clips
                    && let Some(clip_rect) = padding_box_rect(node)
                {
                    outline_rect(
                        &mut pixels,
                        size.width,
                        size.height,
                        clip_rect,
                        CLIP_HIGHLIGHT,
                        1,
                    );
                }
            }
            if self.picking
                && let Some(hovered) = self.hovered
                && let Some(node) = model.nodes.iter().find(|node| node.id == hovered)
                && let Some(rect) = border_box_rect(node)
            {
                outline_rect(
                    &mut pixels,
                    size.width,
                    size.height,
                    rect,
                    PICK_HOVER_HIGHLIGHT,
                    2,
                );
            }
        }

        let Some(surface) = &mut self.surface else {
            return;
        };
        if let Err(error) = surface.resize(width, height) {
            eprintln!("florui-devtools: could not resize the render surface: {error}");
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(error) => {
                eprintln!("florui-devtools: render surface buffer unavailable: {error}");
                return;
            }
        };
        buffer.copy_from_slice(&pixels);
        if let Err(error) = buffer.present() {
            eprintln!("florui-devtools: could not present the frame: {error}");
        }
    }

    fn redraw_inspector(&mut self) {
        let Some(model) = self.inspector_model() else {
            return;
        };
        let Some(inspector) = &mut self.inspector else {
            return;
        };
        match inspector.redraw(&model) {
            Some(InspectorAction::SelectNode(id)) => {
                self.selected = Some(id);
                self.request_redraws();
            }
            Some(InspectorAction::TogglePicking) => {
                self.picking = !self.picking;
                self.request_redraws();
            }
            None => {}
        }
    }

    fn request_redraws(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        if let Some(inspector) = &self.inspector {
            inspector.request_redraw();
        }
    }

    fn update_and_request_redraw(&mut self) {
        let viewport = self.viewport_size();
        if let Some(runtime) = &mut self.runtime {
            runtime.clear_dirty();
            runtime.update(viewport);
        }
        self.request_redraws();
    }

    fn handle_cursor_moved(&mut self, x: f64, y: f64) {
        self.last_cursor = (x, y);
        let hit = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.hit_test(x as f32, y as f32));
        if self.picking && self.hovered != hit {
            self.hovered = hit;
            self.request_redraws();
        }
        self.set_hovered_and_redraw(hit);
    }

    fn handle_cursor_left(&mut self) {
        self.pressed = None;
        if self.picking && self.hovered.take().is_some() {
            self.request_redraws();
        }
        self.set_hovered_and_redraw(None);
    }

    fn set_hovered_and_redraw(&mut self, hit: Option<NodeId>) {
        let viewport = self.viewport_size();
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        if !runtime.set_hovered(hit) {
            return;
        }
        runtime.update(viewport);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn handle_press(&mut self) {
        let (x, y) = self.last_cursor;
        self.pressed = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.hit_test(x as f32, y as f32));
    }

    /// While picking, a release selects whatever it landed on (or
    /// deselects, over empty canvas) and disarms picking, without
    /// dispatching a `click` — the same one-shot pick a browser's own
    /// devtools uses. Otherwise, activation stays strict (press and
    /// release on the same node) and selection is left alone.
    fn handle_release(&mut self) {
        let (x, y) = self.last_cursor;
        let pressed = self.pressed.take();
        let Some(runtime) = &self.runtime else {
            return;
        };
        let released_over = runtime.hit_test(x as f32, y as f32);
        if self.picking {
            self.selected = released_over;
            self.picking = false;
        } else if let (Some(pressed), Some(released_over)) = (pressed, released_over)
            && pressed == released_over
        {
            runtime.dispatch_click(pressed);
        }
        self.request_redraws();
    }
}

impl ApplicationHandler<UserEvent> for LiveHost {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(self.title.clone());
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Rc::new(window),
            Err(error) => return self.fail(event_loop, LiveError::WindowCreation(error)),
        };
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(context) => context,
            Err(error) => return self.fail(event_loop, LiveError::SurfaceCreation(error)),
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(surface) => surface,
            Err(error) => return self.fail(event_loop, LiveError::SurfaceCreation(error)),
        };

        let size = window.inner_size();
        let viewport = Size {
            width: AvailableSpace::Definite(size.width as f32),
            height: AvailableSpace::Definite(size.height as f32),
        };
        let root = self
            .root
            .take()
            .expect("resumed only builds the runtime once, guarded by self.window");
        let mut runtime = match UiRuntime::new(&self.css, root, viewport) {
            Ok(runtime) => runtime,
            Err(error) => return self.fail(event_loop, LiveError::Stylesheet(error)),
        };
        let proxy = self.proxy.clone();
        runtime.on_needs_update(move || {
            let _ = proxy.send_event(UserEvent::Dirty);
        });
        // A mount effect that itself calls `Signal::set` marks the flag
        // before the listener above exists to see it.
        if runtime.is_dirty() {
            runtime.clear_dirty();
            runtime.update(viewport);
        }

        self.runtime = Some(runtime);
        self.window = Some(window);
        self._context = Some(context);
        self.surface = Some(surface);

        match Inspector::new(event_loop) {
            Ok(inspector) => self.inspector = Some(inspector),
            Err(err) => {
                // Losing the inspector should not take down the preview.
                eprintln!("florui-devtools: {err}");
            }
        }

        self.redraw();
        self.redraw_inspector();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.inspector.as_ref().map(Inspector::window_id) == Some(window_id) {
            if let Some(inspector) = &mut self.inspector {
                let _ = inspector.handle_window_event(&event);
            }
            match event {
                WindowEvent::CloseRequested => self.inspector = None,
                WindowEvent::RedrawRequested => self.redraw_inspector(),
                _ => {}
            }
            return;
        }

        if self.window.as_ref().map(|w| w.id()) != Some(window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) => self.update_and_request_redraw(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y);
            }
            WindowEvent::CursorLeft { .. } => self.handle_cursor_left(),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => self.handle_press(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.handle_release(),
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Dirty => self.update_and_request_redraw(),
        }
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_style::{InteractionState, Viewport, compute, parse_stylesheet};

    use super::*;

    #[test]
    fn inspector_nodes_carry_z_index_opacity_and_overflow_clips_from_real_css() {
        let tree: Element = view! { <div class="stacked" /> };
        let css = ".stacked { z-index: 3; opacity: 0.4; overflow: hidden; }";
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet(css).unwrap();
        let styles = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let layouts = HashMap::new();

        let model = build_inspector_model(&arena, &styles, &layouts, None, false);

        let node = &model.nodes[0];
        assert_eq!(node.z_index, Some(3));
        assert_eq!(node.opacity, 0.4);
        assert!(node.overflow_clips);
    }

    #[test]
    fn an_unsupported_role_is_reported_as_a_diagnostic_and_a_supported_one_is_not() {
        let tree: Element = view! {
            <div>
                <input type="checkbox" role="slider" />
                <input type="checkbox" role="switch" />
                <input type="checkbox" />
            </div>
        };
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet("").unwrap();
        let styles = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let model = build_inspector_model(&arena, &styles, &HashMap::new(), None, false);

        let inputs: Vec<_> = model.nodes.iter().filter(|n| n.tag == "input").collect();
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].diagnostics.len(), 1);
        assert!(inputs[0].diagnostics[0].contains("role=\"slider\""));
        assert!(inputs[1].diagnostics.is_empty(), "switch is supported");
        assert!(inputs[2].diagnostics.is_empty(), "no role, nothing to say");
    }

    #[test]
    fn inspector_nodes_default_to_css_initial_values_with_zero_author_css() {
        let tree: Element = view! { <div /> };
        let arena = Arena::build(&tree);
        let rules = parse_stylesheet("").unwrap();
        let styles = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let layouts = HashMap::new();

        let model = build_inspector_model(&arena, &styles, &layouts, None, false);

        let node = &model.nodes[0];
        assert_eq!(node.z_index, None);
        assert_eq!(node.opacity, 1.0);
        assert!(!node.overflow_clips);
    }

    /// `InspectorNode::content` is `BoxLayout::width`/`height` — Taffy's
    /// own final *border* box, confirmed directly against a real
    /// compute_layout run: a content-box 40x20 element with padding
    /// top/right/bottom/left 5/6/7/8 and a 2px top / 3px left border
    /// comes back as `BoxLayout { width: 57, height: 34 }` (40 + 8 + 6 +
    /// 3, 20 + 5 + 7 + 2). These are that exact run's own numbers.
    fn node_with_real_border_box() -> InspectorNode {
        InspectorNode {
            id: 0,
            depth: 0,
            tag: "div".to_string(),
            display: "block".to_string(),
            background: Rgba::TRANSPARENT,
            z_index: None,
            opacity: 1.0,
            overflow_clips: true,
            content: Some(ContentBox {
                x: 0.0,
                y: 0.0,
                width: 57.0,
                height: 34.0,
            }),
            padding: Edges {
                top: 5.0,
                right: 6.0,
                bottom: 7.0,
                left: 8.0,
            },
            border: Edges {
                top: 2.0,
                right: 0.0,
                bottom: 0.0,
                left: 3.0,
            },
            margin: Edges {
                top: Some(0.0),
                right: Some(0.0),
                bottom: Some(0.0),
                left: Some(0.0),
            },
            size_cause: None,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn border_box_rect_reports_content_unchanged_since_it_already_is_the_border_box() {
        // Regression test: this function used to add padding and border
        // on top of `content` as if it still needed them, inflating the
        // reported box well past the node's own real border-box edges —
        // see `node_with_real_border_box`'s own doc for why that was
        // wrong. `width: 57, height: 34` is the *whole* border box, no
        // adjustment needed at all.
        let node = node_with_real_border_box();

        let rect = border_box_rect(&node).expect("a laid-out node has a border box");
        assert_eq!(rect.x, 0);
        assert_eq!(rect.y, 0);
        assert_eq!(rect.width, 57, "not 57 + padding + border inflated further");
        assert_eq!(
            rect.height, 34,
            "not 34 + padding + border inflated further"
        );
    }

    #[test]
    fn padding_box_rect_subtracts_only_the_border_from_the_reported_border_box() {
        let node = node_with_real_border_box();

        let rect = padding_box_rect(&node).expect("a laid-out node has a padding box");
        assert_eq!(rect.x, 3, "shed only the 3px left border, not padding too");
        assert_eq!(rect.y, 2, "shed only the 2px top border, not padding too");
        assert_eq!(rect.width, 54, "57 border-box width minus the 3px border");
        assert_eq!(rect.height, 32, "34 border-box height minus the 2px border");
    }
}
