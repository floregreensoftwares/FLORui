//! A second native window: a tree/style/box inspector over a real
//! `florui-style`/`florui-layout` tree.
//!
//! Uses `egui` (via `egui-winit` + `egui-wgpu`) sharing a host's own
//! `winit` event loop — not `eframe`, which would want to own the loop
//! itself. `wgpu`/`egui*` are devtools-only dependencies; a preview window
//! itself stays on `softbuffer` and never touches a GPU.

use std::fmt;
use std::num::NonZeroU32;
use std::sync::Arc;

use egui::ViewportId;
use egui_wgpu::WgpuError;
use egui_wgpu::winit::Painter;
use florui_style::{Edges, NodeId, Rgba};
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId};

/// A node's absolute content box, in the same physical-pixel space a host
/// paints in — parent-relative `florui_layout::BoxLayout` already resolved
/// via `absolute_position`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// One node's rendering-relevant snapshot, flattened pre-order with `depth`
/// for tree indentation.
pub struct InspectorNode {
    pub id: NodeId,
    pub depth: usize,
    pub tag: String,
    pub display: String,
    pub background: Rgba,
    /// `z-index`. `None` means `auto` (the initial value) — see
    /// `florui_style::ComputedStyle::z_index`'s own doc for when it
    /// actually has an effect (a flex/grid item only, today).
    pub z_index: Option<i32>,
    /// `opacity`, already clamped to `0.0..=1.0`. `1.0` is the initial,
    /// fully-opaque value.
    pub opacity: f32,
    /// Whether this node clips its own content (including descendants)
    /// to its padding box — see
    /// `florui_style::ComputedStyle::overflow_clips`'s own doc.
    pub overflow_clips: bool,
    /// `None` when nothing laid this node out (e.g. a plain `display:
    /// inline` child has no box of its own yet) — not fabricated as zero.
    pub content: Option<ContentBox>,
    pub padding: Edges<f32>,
    pub border: Edges<f32>,
    /// Declared `margin-*`, not a resolved box: `BoxLayout` doesn't carry
    /// resolved auto-margins yet.
    pub margin: Edges<Option<f32>>,
    /// A human-readable explanation of why this node's size doesn't match
    /// what plain flex math alone would produce, if one was found — see
    /// `florui_layout::SizeCause`. `None` either means nothing constrained
    /// it, or (for nodes outside `florui_layout::compute_size_causes`'s
    /// current bound) that no cause was computed at all; the inspector
    /// does not distinguish the two rather than guess which applies.
    pub size_cause: Option<String>,
    /// Things the engine noticed about this node's markup and could not
    /// honor — today, an unsupported `role` attribute, which is otherwise
    /// silently ignored. Empty when there is nothing to report.
    pub diagnostics: Vec<String>,
    /// Where the element was written, when `view!` recorded it: a debug build,
    /// or one with `florui`'s `source-locations` feature. `None` for an element
    /// built by hand.
    pub source: Option<florui::SourceLocation>,
}

/// Everything the inspector needs to render one frame. Rebuilt by the
/// caller whenever the underlying tree renders or the selection changes.
pub struct InspectorModel {
    /// Pre-order flattened tree — a child always immediately follows its
    /// parent, with a strictly greater `depth`.
    pub nodes: Vec<InspectorNode>,
    pub selected: Option<NodeId>,
    /// Mirrors a browser devtools' element picker: off by default (a
    /// preview click does whatever the app does); armed, the next preview
    /// click selects instead of activating, then disarms itself.
    pub picking: bool,
    pub stale: bool,
}

/// What the user did with the inspector this frame, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorAction {
    SelectNode(NodeId),
    TogglePicking,
}

#[derive(Debug)]
pub enum InspectorError {
    CreateWindow(winit::error::OsError),
    CreateSurface(WgpuError),
}

impl fmt::Display for InspectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InspectorError::CreateWindow(err) => {
                write!(f, "could not create inspector window: {err}")
            }
            InspectorError::CreateSurface(err) => {
                write!(f, "could not initialize inspector rendering: {err}")
            }
        }
    }
}

impl std::error::Error for InspectorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InspectorError::CreateWindow(err) => Some(err),
            InspectorError::CreateSurface(err) => Some(err),
        }
    }
}

pub struct Inspector {
    window: Arc<Window>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    painter: Painter,
}

impl Inspector {
    /// Creates the inspector's own native window on `event_loop` — the same
    /// event loop the preview window uses, since a process may only drive
    /// one `winit` event loop.
    pub fn new(event_loop: &ActiveEventLoop) -> Result<Self, InspectorError> {
        let attrs = Window::default_attributes().with_title("Florui inspector");
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(InspectorError::CreateWindow)?,
        );

        let egui_ctx = egui::Context::default();

        let mut painter = pollster::block_on(Painter::new(
            egui_ctx.clone(),
            egui_wgpu::WgpuConfiguration::default(),
            false,
            egui_wgpu::RendererOptions::default(),
        ));
        pollster::block_on(painter.set_window(ViewportId::ROOT, Some(window.clone())))
            .map_err(InspectorError::CreateSurface)?;

        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            window.theme(),
            painter.max_texture_side(),
        );

        Ok(Self {
            window,
            egui_ctx,
            egui_state,
            painter,
        })
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }

    /// Forwards a window event to egui. Returns whether egui consumed it
    /// (the caller should not also act on a consumed event).
    ///
    /// Also requests a redraw: `on_window_event` only queues input for the
    /// next frame, it doesn't run `draw_ui` — without this, a click sits
    /// unprocessed until some unrelated redraw happens to come along.
    pub fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::Resized(size) = event {
            self.resize(*size);
        }
        let consumed = self
            .egui_state
            .on_window_event(&self.window, event)
            .consumed;
        if !matches!(event, WindowEvent::RedrawRequested) {
            self.window.request_redraw();
        }
        consumed
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        {
            self.painter
                .on_window_resized(ViewportId::ROOT, width, height);
        }
    }

    /// Renders one frame and reports the user's action, if any — the
    /// caller owns all state and decides what it means.
    pub fn redraw(&mut self, model: &InspectorModel) -> Option<InspectorAction> {
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let mut action = None;
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            action = draw_ui(ui, model);
        });
        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output);
        let clipped_primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        self.painter.paint_and_update_textures(
            ViewportId::ROOT,
            full_output.pixels_per_point,
            [0.08, 0.08, 0.09, 1.0],
            &clipped_primitives,
            &full_output.textures_delta,
            Vec::new(),
            &self.window,
        );
        action
    }
}

fn format_margin(edge: Option<f32>) -> String {
    match edge {
        Some(px) => format!("{px}px"),
        None => "auto".to_string(),
    }
}

/// The lines the panel shows for where an element was written: the file with
/// its line and column, the component that built it, and its place in the
/// `view!` block.
fn source_lines(source: &florui::SourceLocation) -> Vec<String> {
    let mut lines = vec![format!("{}:{}:{}", source.file, source.line, source.column)];
    if let Some(component) = source.component {
        lines.push(format!("component: {component}"));
    }
    lines.push(format!("in the block: {}", source.path));
    lines
}

fn format_z_index(z_index: Option<i32>) -> String {
    match z_index {
        Some(value) => value.to_string(),
        None => "auto".to_string(),
    }
}

fn draw_ui(ui: &mut egui::Ui, model: &InspectorModel) -> Option<InspectorAction> {
    let mut action = None;

    egui::Panel::top("florui-inspector-toolbar").show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.selectable_label(model.picking, "Pick element").clicked() {
                action = Some(InspectorAction::TogglePicking);
            }
            if model.picking {
                ui.label("click an element in the preview to select it");
            }
        });
    });

    egui::Panel::left("florui-inspector-tree").show(ui, |ui| {
        ui.heading("Tree");
        for node in &model.nodes {
            ui.horizontal(|ui| {
                ui.add_space(node.depth as f32 * 12.0);
                let is_selected = model.selected == Some(node.id);
                let label = format!("<{}>", node.tag);
                if ui.selectable_label(is_selected, label).clicked() {
                    action = Some(InspectorAction::SelectNode(node.id));
                }
            });
        }
    });

    egui::CentralPanel::default().show(ui, |ui| {
        if model.stale {
            ui.colored_label(egui::Color32::RED, "STALE — showing last good revision");
            ui.separator();
        }

        let selected = model
            .selected
            .and_then(|id| model.nodes.iter().find(|node| node.id == id));

        match selected {
            None => {
                ui.label("No element selected — click one in the tree or the preview.");
            }
            Some(node) => {
                ui.heading("Source");
                match &node.source {
                    Some(source) => {
                        for line in source_lines(source) {
                            ui.monospace(line);
                        }
                    }
                    None => {
                        ui.label(
                            "not recorded: built by hand, or in a build that leaves source \
                             locations off",
                        );
                    }
                }
                ui.heading("Styles");
                ui.monospace(format!("display: {}", node.display));
                ui.monospace(format!(
                    "background-color: #{:02x}{:02x}{:02x}",
                    node.background.r, node.background.g, node.background.b
                ));
                ui.monospace(format!("z-index: {}", format_z_index(node.z_index)));
                ui.monospace(format!("opacity: {}", node.opacity));
                ui.monospace(format!(
                    "overflow: {}",
                    if node.overflow_clips {
                        "clips"
                    } else {
                        "visible"
                    }
                ));

                ui.separator();
                ui.heading("Box model");
                match node.content {
                    Some(b) => {
                        ui.monospace(format!(
                            "content: {}, {} — {}x{} (physical px)",
                            b.x, b.y, b.width, b.height
                        ));
                    }
                    None => {
                        ui.label("no box: this node has no layout of its own yet");
                    }
                }
                ui.monospace(format!(
                    "padding: {} {} {} {} (top right bottom left)",
                    node.padding.top, node.padding.right, node.padding.bottom, node.padding.left
                ));
                ui.monospace(format!(
                    "border: {} {} {} {} (top right bottom left)",
                    node.border.top, node.border.right, node.border.bottom, node.border.left
                ));
                ui.monospace(format!(
                    "margin: {} {} {} {} (declared; auto is not yet resolved to a box)",
                    format_margin(node.margin.top),
                    format_margin(node.margin.right),
                    format_margin(node.margin.bottom),
                    format_margin(node.margin.left),
                ));

                if let Some(cause) = &node.size_cause {
                    ui.separator();
                    ui.heading("Why this size");
                    ui.label(cause);
                }

                if !node.diagnostics.is_empty() {
                    ui.separator();
                    ui.heading("Diagnostics");
                    for message in &node.diagnostics {
                        ui.label(message);
                    }
                }
            }
        }
    });

    action
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_source_panel_names_the_file_the_component_and_the_place_in_the_block() {
        let source = florui::SourceLocation {
            file: "src/card.rs",
            line: 14,
            column: 9,
            component: Some("Card"),
            path: "div > ul > li[3]",
        };
        assert_eq!(
            source_lines(&source),
            vec![
                "src/card.rs:14:9".to_string(),
                "component: Card".to_string(),
                "in the block: div > ul > li[3]".to_string(),
            ]
        );
    }

    #[test]
    fn an_element_with_no_component_shows_no_component_line() {
        let source = florui::SourceLocation {
            file: "src/main.rs",
            line: 3,
            column: 5,
            component: None,
            path: "div",
        };
        assert_eq!(source_lines(&source).len(), 2);
    }
}
