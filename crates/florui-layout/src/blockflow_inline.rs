//! The inline content of a block of a context that has floats. See
//! [`crate::blockflow`].
//!
//! The lines are shaped one at a time, each in the room the floats before it
//! leave (florui-text breaks them in the slot this asks for), and a float
//! among the content is placed when the lines reach it. A line is as tall as it
//! turns out to be, and where the next one starts is only known then, so the
//! lines are laid out again with the heights the first pass found until they
//! stop changing; the floats placed in a pass are forgotten before the next.

use std::sync::Arc;

use florui_text::{InlineElement, LineEnv, LineSlot, LineSource};
use taffy::{AvailableSpace, NodeId, Size};

use crate::blockflow::{Bfc, Geo};
use crate::blockflow_walk::FloatBox;
use crate::floats::{ClearSides, FloatSet};
use crate::tree::Run;
use crate::{
    InlineContentItem, InlineContext, LeafContext, LeafScratch, measure_leaf, to_inline_items,
};

/// How many times the lines are laid out again when their heights change.
const PASSES: usize = 4;

/// Slack for lengths that should be equal.
const EPSILON: f32 = 0.01;

/// What the lines of a block came to.
pub(crate) struct InlineOut {
    /// Their height, from the top of the content box.
    pub(crate) height: f32,
    /// Where the first line's baseline is, from the top of the content box.
    pub(crate) baseline: Option<f32>,
    /// How far they reach from the left edge of the content box.
    pub(crate) reach_width: f32,
    /// There is no line.
    pub(crate) empty: bool,
}

/// What the lines need to know of a float.
struct FloatSpec {
    /// Its margin box.
    size: Size<f32>,
    left: bool,
    clear: ClearSides,
}

/// The floats of a context, as the lines of one block of it meet them.
struct FlowEnv<'a> {
    floats: &'a mut FloatSet,
    /// Where the content box of the block starts in the context.
    top: f32,
    /// The left and right edge of the content box in the context.
    cb: (f32, f32),
    specs: &'a [FloatSpec],
    /// Where each float was put, by margin box, in the context.
    placed: Vec<Option<(f32, f32)>>,
    /// The floats put by this pass, in the order they were, and how many the
    /// context had before it.
    order: Vec<usize>,
    base: usize,
    /// The height asked for the line being laid out.
    line_height: f32,
}

impl FlowEnv<'_> {
    fn slot_at(&self, y: f32, height: f32) -> LineSlot {
        let (left, right) = self.floats.room(self.top + y, height, self.cb.0, self.cb.1);
        LineSlot {
            x: left - self.cb.0,
            width: (right - left).max(0.0),
            drop: 0.0,
        }
    }

    fn place(&mut self, id: usize, y: f32) {
        let Some(spec) = self.specs.get(id) else {
            return;
        };
        let mut min_y = self.top + y;
        if let Some(bottom) = self.floats.clear_bottom(spec.clear) {
            min_y = min_y.max(bottom);
        }
        let at = self
            .floats
            .place(spec.size.width, spec.size.height, spec.left, min_y, self.cb);
        self.placed[id] = Some(at);
        self.order.push(id);
    }
}

impl LineEnv for FlowEnv<'_> {
    fn slot(&mut self, y: f32, height: f32) -> LineSlot {
        self.line_height = height;
        self.slot_at(y, height)
    }

    fn float_here(&mut self, id: u64, y: f32, used: f32, slot: LineSlot) -> Option<LineSlot> {
        let id = id as usize;
        let width = self.specs.get(id).map_or(0.0, |spec| spec.size.width);
        // Beside what the line already holds, or below it.
        if used > 0.0 && used + width > slot.width + EPSILON {
            return None;
        }
        self.place(id, y);
        Some(self.slot_at(y, self.line_height))
    }

    fn float_below(&mut self, id: u64, y: f32) {
        self.place(id as usize, y);
    }

    fn clear_of_floats(&mut self, y: f32, height: f32) -> Option<f32> {
        self.floats
            .bottom_beside(self.top + y, height)
            .map(|bottom| bottom - self.top)
    }

    fn mark(&mut self) -> usize {
        self.order.len()
    }

    fn rewind(&mut self, mark: usize) {
        for id in self.order.drain(mark..) {
            self.placed[id] = None;
        }
        self.floats.truncate(self.base + mark);
    }
}

impl Run<'_> {
    /// Lays out the lines of `id`, a block with inline content, in a content
    /// box `inner_w` wide.
    pub(crate) fn inline_content(
        &mut self,
        bfc: &mut Bfc,
        id: NodeId,
        g: &Geo,
        inner_w: f32,
    ) -> InlineOut {
        let node = self.tree.live(id);
        let floats = node.children.clone();
        if floats.is_empty() && bfc.floats.is_empty() {
            return self.inline_plain(id, inner_w);
        }
        let context = match &node.context {
            Some(LeafContext::Inline(context)) => InlineContext {
                elements: context.elements.clone(),
                items: context.items.clone(),
                nodes: Vec::new(),
                hidden: Vec::new(),
            },
            Some(LeafContext::Text(text)) => InlineContext {
                elements: vec![InlineElement {
                    parent: 0,
                    style: text.style,
                }],
                items: vec![InlineContentItem::Text {
                    text: text.text.clone(),
                    element: 0,
                }],
                nodes: Vec::new(),
                hidden: Vec::new(),
            },
            _ => return self.inline_plain(id, inner_w),
        };
        let items = to_inline_items(&context);

        let boxes: Vec<FloatBox> = floats
            .iter()
            .map(|&float| self.float_box(bfc, g, float, inner_w))
            .collect();
        let specs: Vec<FloatSpec> = boxes
            .iter()
            .map(|float| FloatSpec {
                size: float.outer(),
                left: float.left,
                clear: float.clear,
            })
            .collect();
        let checkpoint = bfc.floats.len();
        let top = g.y + g.pb.top;
        let cb = (g.x + g.pb.left, g.x + g.width - g.pb.right);
        let mut hints: Vec<f32> = Vec::new();
        let mut outcome = None;
        for _ in 0..PASSES {
            bfc.floats.truncate(checkpoint);
            let mut env = FlowEnv {
                floats: &mut bfc.floats,
                top,
                cb,
                specs: &specs,
                placed: vec![None; specs.len()],
                order: Vec::new(),
                base: checkpoint,
                line_height: 0.0,
            };
            let mut source = LineSource::Env {
                env: &mut env,
                hints: &hints,
            };
            let (layout, trace) = self.font.shape_inline_tree_in_slots(
                &context.elements,
                &items,
                inner_w,
                &mut source,
            );
            let placed = env.placed;
            let settled = trace.heights == hints;
            hints.clone_from(&trace.heights);
            outcome = Some((layout, trace, placed));
            if settled {
                break;
            }
        }
        let Some((layout, trace, placed)) = outcome else {
            unreachable!("at least one pass runs");
        };

        // Floats the lines never reached (there were none) go below them.
        let mut env = FlowEnv {
            floats: &mut bfc.floats,
            top,
            cb,
            specs: &specs,
            placed,
            order: Vec::new(),
            base: checkpoint,
            line_height: 0.0,
        };
        for index in 0..specs.len() {
            if env.placed[index].is_none() {
                env.place(index, layout.height);
            }
        }
        let placed = env.placed;
        if bfc.commit {
            for (index, &float) in floats.iter().enumerate() {
                if let Some(at) = placed[index] {
                    self.set_float_layout(g, float, index as u32, &boxes[index], at);
                }
            }
        }
        let full = LineSlot {
            x: 0.0,
            width: inner_w,
            drop: 0.0,
        };
        let slots: Option<Arc<[LineSlot]>> = if trace.slots.iter().all(|slot| *slot == full) {
            None
        } else {
            Some(trace.slots.into())
        };
        self.tree.live_mut(id).slots = slots;
        InlineOut {
            height: layout.height,
            baseline: Some(layout.baseline),
            reach_width: layout.width,
            empty: layout.height == 0.0,
        }
    }

    /// The lines of `id` with no float anywhere near: what they measure to.
    fn inline_plain(&mut self, id: NodeId, inner_w: f32) -> InlineOut {
        let Run { tree, font, .. } = self;
        let node = tree.live_mut(id);
        node.slots = None;
        let mut baseline = None;
        let size = measure_leaf(
            font,
            &mut LeafScratch::default(),
            node.context.as_mut(),
            Size {
                width: Some(inner_w),
                height: None,
            },
            Size {
                width: AvailableSpace::Definite(inner_w),
                height: AvailableSpace::MaxContent,
            },
            &mut baseline,
        );
        InlineOut {
            height: size.height,
            baseline,
            reach_width: size.width,
            empty: size.height == 0.0,
        }
    }
}

impl Run<'_> {
    /// How wide the lines of `id` are when it has room as `room` says, which is
    /// nothing when it has no content of its own.
    pub(crate) fn inline_width(&mut self, id: NodeId, room: AvailableSpace) -> f32 {
        let Run { tree, font, .. } = self;
        let node = tree.live_mut(id);
        if node.context.is_none() {
            return 0.0;
        }
        let mut baseline = None;
        measure_leaf(
            font,
            &mut LeafScratch::default(),
            node.context.as_mut(),
            Size {
                width: None,
                height: None,
            },
            Size {
                width: room,
                height: AvailableSpace::MaxContent,
            },
            &mut baseline,
        )
        .width
    }
}
