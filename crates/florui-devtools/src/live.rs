//! The inspector, run against a window of the real desktop host.
//!
//! [`run`] opens the application's window through `florui_platform`'s own
//! host, so keyboard, focus, text input, overlays and every other behavior
//! are the real ones, and watches it with an [`InspectorObserver`]: a second
//! window (see [`crate::inspector`]) that reads each finished frame and draws
//! a selection outline over it. Nothing the observer does changes the
//! application's state.

use std::collections::HashMap;
use std::sync::Arc;

use florui::Element;
use florui_layout::{BoxLayout, SizeCause, absolute_position, compute_size_causes};
use florui_platform::{
    HostObserver, ObservedFrame, OverlayCanvas, RunError, WindowOptions, WindowSpec,
    run_windows_observed,
};
use florui_style::{Arena, ComputedStyle, Display, Edges, NodeId, Rgba};
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

use crate::inspector::{ContentBox, Inspector, InspectorAction, InspectorModel, InspectorNode};

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
    Run(RunError),
}

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiveError::Run(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for LiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LiveError::Run(err) => Some(err),
        }
    }
}

/// Opens a window titled `title` over `root` on the real desktop host, paired
/// with the inspector window. `css` is parsed once; it does not get watched
/// for changes.
///
/// Blocks the calling thread until the application window closes.
pub fn run(
    title: &str,
    css: &str,
    canvas_color: Rgba,
    root: impl Fn() -> Element + 'static,
) -> Result<(), LiveError> {
    let spec = WindowSpec::new(title, css, canvas_color, WindowOptions::default(), root)
        .map_err(LiveError::Run)?;
    run_windows_observed(vec![spec], InspectorObserver::default()).map_err(LiveError::Run)
}

/// Same as [`run`], for several windows on one host. The inspector panel
/// follows whichever application window has focus.
pub fn run_windows(specs: Vec<WindowSpec>) -> Result<(), LiveError> {
    run_windows_observed(specs, InspectorObserver::default()).map_err(LiveError::Run)
}

fn display_name(display: Display) -> &'static str {
    match display {
        Display::Block => "block",
        Display::Flex => "flex",
        Display::Inline => "inline",
        Display::InlineBlock => "inline-block",
        Display::Grid => "grid",
        Display::None => "none",
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

/// A node's padding box — its border box inset by the border — real CSS's own
/// clip boundary for `overflow_clips`, see [`CLIP_HIGHLIGHT`]. `bounds` is the
/// border box where it is drawn, `(x, y, width, height)` in physical pixels
/// (`BoxLayout::width`/`height` are Taffy's final border-box size, padding and
/// border already folded in, confirmed against a real laid-out node: a 40x20
/// content box with padding 8/6/5/7 and a 3px left / 2px top border comes back
/// as `BoxLayout { width: 57, height: 34 }`), and `border` is in CSS pixels.
fn padding_box(
    bounds: (f32, f32, f32, f32),
    border: &Edges<f32>,
    scale_factor: f64,
) -> (f32, f32, f32, f32) {
    let scale = scale_factor as f32;
    let (x, y, width, height) = bounds;
    let (left, top) = (border.left * scale, border.top * scale);
    (
        x + left,
        y + top,
        (width - left - border.right * scale).max(0.0),
        (height - top - border.bottom * scale).max(0.0),
    )
}

/// Draws the outline of `rect` (physical pixels) `thickness` wide onto the
/// canvas, clipped to it.
fn outline(
    canvas: &mut OverlayCanvas<'_>,
    rect: (f32, f32, f32, f32),
    color: crate::color::Rgba,
    thickness: u32,
) {
    let (x0, y0) = (rect.0.round() as i64, rect.1.round() as i64);
    let (x1, y1) = (
        (rect.0 + rect.2).round() as i64,
        (rect.1 + rect.3).round() as i64,
    );
    let (width, height) = (i64::from(canvas.width), i64::from(canvas.height));
    let t = i64::from(thickness);
    let mut paint = |x: i64, y: i64| {
        if (0..width).contains(&x) && (0..height).contains(&y) {
            let at = ((y * width + x) * 4) as usize;
            canvas.rgba[at..at + 4].copy_from_slice(&[color.r, color.g, color.b, 255]);
        }
    };
    for x in x0..x1 {
        for d in 0..t {
            paint(x, y0 + d);
            paint(x, y1 - 1 - d);
        }
    }
    for y in y0..y1 {
        for d in 0..t {
            paint(x0 + d, y);
            paint(x1 - 1 - d, y);
        }
    }
}

/// Shows the application's windows in a second window: the element tree of
/// whichever one has focus, with its resolved styles and boxes, an element
/// picker, and a selection outline drawn over that window's frames. Each window
/// keeps its own selection. Read-only — it never touches a watched window's
/// state.
#[derive(Default)]
pub struct InspectorObserver {
    inspector: Option<Inspector>,
    app_windows: HashMap<WindowId, Arc<Window>>,
    /// The window the panel shows: the one focused last.
    watched: Option<WindowId>,
    model: Option<InspectorModel>,
    bounds: HashMap<NodeId, (f32, f32, f32, f32)>,
    scale_factor: f64,
    selections: HashMap<WindowId, NodeId>,
    picking: bool,
    hovered: Option<NodeId>,
}

impl InspectorObserver {
    fn watches(&self, window: WindowId) -> bool {
        self.watched == Some(window)
    }

    fn selected(&self) -> Option<NodeId> {
        self.watched
            .and_then(|window| self.selections.get(&window).copied())
    }

    fn select(&mut self, node: Option<NodeId>) {
        let Some(window) = self.watched else {
            return;
        };
        match node {
            Some(node) => self.selections.insert(window, node),
            None => self.selections.remove(&window),
        };
    }

    /// Points the panel at `window`; its next frame rebuilds the model.
    fn watch(&mut self, window: WindowId) {
        self.watched = Some(window);
        self.model = None;
        self.bounds.clear();
        self.picking = false;
        self.hovered = None;
        if let Some(handle) = self.app_windows.get(&window) {
            handle.request_redraw();
        }
    }

    fn request_redraws(&self) {
        if let Some(handle) = self
            .watched
            .and_then(|window| self.app_windows.get(&window))
        {
            handle.request_redraw();
        }
        if let Some(inspector) = &self.inspector {
            inspector.request_redraw();
        }
    }

    fn redraw_inspector(&mut self) {
        let selected = self.selected();
        let (Some(model), Some(inspector)) = (self.model.as_mut(), self.inspector.as_mut()) else {
            return;
        };
        model.selected = selected;
        model.picking = self.picking;
        match inspector.redraw(model) {
            Some(InspectorAction::SelectNode(id)) => {
                self.select(Some(id));
                self.request_redraws();
            }
            Some(InspectorAction::TogglePicking) => {
                self.picking = !self.picking;
                self.hovered = None;
                self.request_redraws();
            }
            None => {}
        }
    }
}

impl HostObserver for InspectorObserver {
    fn window_opened(&mut self, event_loop: &ActiveEventLoop, window: &Arc<Window>) {
        self.app_windows.insert(window.id(), window.clone());
        if self.inspector.is_none() {
            match Inspector::new(event_loop) {
                Ok(inspector) => self.inspector = Some(inspector),
                Err(err) => {
                    // Losing the inspector should not take down the application.
                    eprintln!("florui-devtools: {err}");
                }
            }
        }
        if self.watched.is_none() {
            self.watched = Some(window.id());
        }
        // The first frame was painted before this observer knew the window.
        window.request_redraw();
    }

    fn window_closed(&mut self, window: WindowId) {
        self.app_windows.remove(&window);
        self.selections.remove(&window);
        if self.watched == Some(window) {
            self.watched = None;
            self.model = None;
            self.bounds.clear();
            self.picking = false;
            self.hovered = None;
            if let Some(next) = self.app_windows.keys().next().copied() {
                self.watch(next);
            }
            if let Some(inspector) = &self.inspector {
                inspector.request_redraw();
            }
        }
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window: WindowId,
        event: &WindowEvent,
    ) -> bool {
        if self.inspector.as_ref().map(Inspector::window_id) != Some(window) {
            // An application window taking focus is what the panel follows.
            if matches!(event, WindowEvent::Focused(true))
                && self.app_windows.contains_key(&window)
                && self.watched != Some(window)
            {
                self.watch(window);
            }
            return false;
        }
        if let Some(inspector) = &mut self.inspector {
            let _ = inspector.handle_window_event(event);
        }
        match event {
            WindowEvent::CloseRequested => self.inspector = None,
            WindowEvent::RedrawRequested => self.redraw_inspector(),
            _ => {}
        }
        true
    }

    fn frame(&mut self, window: WindowId, frame: &ObservedFrame<'_>) {
        if !self.watches(window) || self.inspector.is_none() {
            return;
        }
        self.model = Some(build_inspector_model(
            frame.arena,
            frame.styles,
            frame.layouts,
            self.selected(),
            self.picking,
        ));
        self.bounds = frame.bounds.clone();
        self.scale_factor = frame.scale_factor;
        if let Some(inspector) = &self.inspector {
            inspector.request_redraw();
        }
    }

    fn paint_overlay(&mut self, window: WindowId, mut canvas: OverlayCanvas<'_>) {
        if !self.watches(window) {
            return;
        }
        if let Some(selected) = self.selected()
            && let Some(&rect) = self.bounds.get(&selected)
        {
            outline(&mut canvas, rect, SELECTION_HIGHLIGHT, 2);
            if let Some(node) = self
                .model
                .as_ref()
                .and_then(|model| model.nodes.iter().find(|node| node.id == selected))
                && node.overflow_clips
            {
                let clip = padding_box(rect, &node.border, self.scale_factor);
                outline(&mut canvas, clip, CLIP_HIGHLIGHT, 1);
            }
        }
        if self.picking
            && let Some(hovered) = self.hovered
            && let Some(&rect) = self.bounds.get(&hovered)
        {
            outline(&mut canvas, rect, PICK_HOVER_HIGHLIGHT, 2);
        }
    }

    fn is_picking(&self, window: WindowId) -> bool {
        self.picking && self.watches(window)
    }

    fn pointer_moved(&mut self, _window: WindowId, node: Option<NodeId>) {
        if self.hovered != node {
            self.hovered = node;
            self.request_redraws();
        }
    }

    fn picked(&mut self, _window: WindowId, node: Option<NodeId>) {
        self.select(node);
        self.picking = false;
        self.hovered = None;
        self.request_redraws();
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
    fn the_padding_box_sheds_only_the_border_from_the_drawn_border_box() {
        let node = node_with_real_border_box();
        let content = node.content.unwrap();
        let bounds = (content.x, content.y, content.width, content.height);

        let (x, y, width, height) = padding_box(bounds, &node.border, 1.0);
        assert_eq!(x, 3.0, "shed only the 3px left border, not padding too");
        assert_eq!(y, 2.0, "shed only the 2px top border, not padding too");
        assert_eq!(width, 54.0, "57 border-box width minus the 3px border");
        assert_eq!(height, 32.0, "34 border-box height minus the 2px border");
    }

    #[test]
    fn the_padding_box_border_scales_with_the_display() {
        let node = node_with_real_border_box();
        let content = node.content.unwrap();
        // Bounds are already physical; the border is in CSS pixels.
        let bounds = (0.0, 0.0, content.width * 2.0, content.height * 2.0);

        let (x, y, width, height) = padding_box(bounds, &node.border, 2.0);
        assert_eq!((x, y), (6.0, 4.0));
        assert_eq!((width, height), (108.0, 64.0));
    }
}
