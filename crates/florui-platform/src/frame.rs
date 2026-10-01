//! What painting a window needs beyond the runtime's own geometry: the layouts
//! with scroll offsets applied and scaled to physical pixels, the caret,
//! selection and spinner paint of text fields, and the decoded images and
//! icons. The desktop host and the headless window build it the same way, so
//! what a test paints is what a window paints.

use std::collections::HashMap;
use std::sync::Arc;

use florui_layout::BoxLayout;
use florui_style::{Arena, ComputedStyle, NodeId};

use crate::desktop::{
    TextInputPaintContext, build_control_icon_paint, build_icon_paint, build_image_paint,
    build_text_input_paint, scale_layouts,
};
use crate::scroll::ScrollRegistry;

/// The inputs of [`build_paint_parts`].
pub(crate) struct PaintSources<'a> {
    pub(crate) arena: &'a Arena,
    pub(crate) styles: &'a HashMap<NodeId, ComputedStyle>,
    pub(crate) layouts: &'a HashMap<NodeId, BoxLayout>,
    pub(crate) font: &'a mut florui_text::Font,
    pub(crate) scroll_registry: &'a ScrollRegistry,
    pub(crate) text_input_registry: &'a crate::text_input::TextInputRegistry,
    pub(crate) image_registry: &'a crate::image::ImageRegistry,
    pub(crate) icon_registry: &'a crate::icon::IconRegistry,
    pub(crate) asset_cache: &'a Arc<florui_assets::AssetCache>,
    pub(crate) executor: &'a dyn florui_reactive::Executor,
    pub(crate) focused: Option<NodeId>,
    pub(crate) hovered: Option<NodeId>,
    /// The pointer, in logical pixels.
    pub(crate) cursor: (f32, f32),
    /// The `id` of the textarea whose scrollbar thumb is being dragged.
    pub(crate) scroll_dragging: Option<&'a str>,
    pub(crate) scale_factor: f64,
}

/// The result of [`build_paint_parts`].
pub(crate) struct PaintParts {
    /// Logical pixels, scroll offsets applied.
    pub(crate) scrolled_layouts: HashMap<NodeId, BoxLayout>,
    /// Physical pixels, scroll offsets applied.
    pub(crate) physical_layouts: HashMap<NodeId, BoxLayout>,
    pub(crate) text_inputs: HashMap<NodeId, florui_paint::TextInputPaint>,
    pub(crate) images: HashMap<NodeId, florui_paint::ImagePaint>,
}

pub(crate) fn build_paint_parts(sources: PaintSources<'_>) -> PaintParts {
    let _span = florui_profile::span(florui_profile::Phase::PaintParts);
    let PaintSources {
        arena,
        styles,
        layouts,
        font,
        scroll_registry,
        text_input_registry,
        image_registry,
        icon_registry,
        asset_cache,
        executor,
        focused,
        hovered,
        cursor,
        scroll_dragging,
        scale_factor,
    } = sources;
    let scroll_offsets = scroll_registry.offsets_by_node(arena);
    let scrolled_layouts = florui_layout::apply_scroll_offsets(arena, layouts, &scroll_offsets);
    let physical_layouts = scale_layouts(&scrolled_layouts, scale_factor as f32);
    let spinner_for = |node: NodeId| {
        if arena.input_type(node) != Some("number")
            || arena.is_disabled(node)
            || arena.attr_flag(node, "readonly")
        {
            return None;
        }
        let is_hovered = hovered == Some(node);
        if !is_hovered && focused != Some(node) {
            return None;
        }
        let layout = layouts.get(&node)?;
        let (x, y) = florui_layout::absolute_position(arena, &scrolled_layouts, node);
        Some(if is_hovered {
            florui_paint::spinner_half_at((x, y, layout.width, layout.height), cursor)
        } else {
            florui_paint::SpinnerHover::None
        })
    };
    let text_inputs = build_text_input_paint(
        font,
        TextInputPaintContext {
            arena,
            styles,
            layouts: &scrolled_layouts,
            registry: text_input_registry,
            focused,
            hovered,
            pointer: cursor,
            scroll_dragging,
        },
        &spinner_for,
    );
    // One combined map: `florui_paint` blits either tag's own decoded
    // pixels identically (see its own "img"/"icon" tag check), so it
    // only needs one `NodeId -> ImagePaint` map, not one per registry.
    let mut images = build_image_paint(
        arena,
        styles,
        &physical_layouts,
        image_registry,
        asset_cache,
        executor,
    );
    images.extend(build_icon_paint(
        arena,
        styles,
        &physical_layouts,
        icon_registry,
        asset_cache,
        executor,
    ));
    images.extend(build_control_icon_paint(
        arena,
        styles,
        icon_registry,
        asset_cache,
        executor,
        scale_factor as f32,
    ));
    PaintParts {
        scrolled_layouts,
        physical_layouts,
        text_inputs,
        images,
    }
}

/// Ends the frame being profiled when dropped, including on an early return,
/// with the causes recorded since profiling started or the last frame ended.
pub(crate) struct FrameEnd;

impl Drop for FrameEnd {
    fn drop(&mut self) {
        finish_profile_frame();
    }
}

thread_local! {
    static CAUSES_SEEN: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn finish_profile_frame() {
    if !florui_profile::is_recording() {
        return;
    }
    use florui_reactive::trace::{self, Origin, TraceKind};
    let since = CAUSES_SEEN.with(std::cell::Cell::get);
    let started = florui_profile::started_at().unwrap_or_default();
    let causes = trace::since(since)
        .into_iter()
        .filter(|t| t.at >= started)
        .map(|t| florui_profile::Cause {
            at: t.at,
            component: t.component,
            kind: match t.kind {
                TraceKind::SignalWrite => "signal-write",
                TraceKind::EventDispatched => "event",
                TraceKind::ResourceStarted => "resource-started",
                TraceKind::ResourceCompleted => "resource-completed",
            },
            event: match t.origin {
                Origin::Event { name, target } => Some((name, target)),
                _ => None,
            },
        })
        .collect();
    CAUSES_SEEN.with(|seen| seen.set(trace::next_sequence()));
    florui_profile::finish_frame(causes);
}
