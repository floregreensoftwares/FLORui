//! The root of a block formatting context that has floats: its size, and the
//! helpers the walk over its blocks uses. See [`crate::blockflow`].

use taffy::util::{MaybeMath, ResolveOrZero};
use taffy::{
    AvailableSpace, Baselines, CollapsibleMarginSet, Layout, LayoutInput, LayoutOutput, Line,
    NodeId, Point, Rect, RequestedAxis, RunMode, Size, SizingMode,
};

use crate::blockflow::{Bfc, Geo, Part, calc, own_of};
use crate::floats::{ClearSides, FloatSet};
use crate::tree::Run;

/// Slack for lengths that should be equal.
const EPSILON: f32 = 0.01;

/// The state of the walk over the children of a block.
pub(crate) struct Walk {
    /// Where the next in-flow box may start, from the block's top edge.
    pub(crate) committed: f32,
    /// The margins that collapse into whatever comes next.
    pub(crate) active: CollapsibleMarginSet,
    /// Whether the first child's margins are still collapsing into the block's own.
    pub(crate) collapsing_first: bool,
    pub(crate) first_top: CollapsibleMarginSet,
    /// Whether `active` holds the margins of a self-collapsing box that cleared.
    pub(crate) cleared_active: bool,
}

impl Run<'_> {
    /// Lays out the block formatting context rooted at `id`, which has floats in
    /// it. What Taffy's block layout is for a root with none.
    pub(crate) fn flow_region_root(&mut self, id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let style = self.tree.node(id).style.clone();
        let own = own_of(&style, inputs.parent_size);
        let inherent = inputs.sizing_mode == SizingMode::InherentSize;
        let style_size = if inherent {
            own.size.maybe_clamp(own.min, own.max)
        } else {
            Size::NONE
        };
        let settled_by_bounds = own.min.zip_map(own.max, |min, max| match (min, max) {
            (Some(min), Some(max)) if max <= min => Some(min),
            _ => None,
        });
        let pb_size = own.pb.sum_axes();
        let known = inputs
            .known_dimensions
            .or(settled_by_bounds)
            .or(style_size)
            .maybe_max(pb_size);
        let margin_top = style
            .margin
            .top
            .resolve_or_zero(inputs.parent_size.width, calc);
        let margin_bottom = style
            .margin
            .bottom
            .resolve_or_zero(inputs.parent_size.width, calc);
        let sized = |size: Size<f32>| {
            let mut output = LayoutOutput::from_outer_size(size);
            output.top_margin = CollapsibleMarginSet::from_margin(margin_top);
            output.bottom_margin = CollapsibleMarginSet::from_margin(margin_bottom);
            output
        };
        if inputs.run_mode == RunMode::ComputeSize
            && let (Some(width), Some(height)) = (known.width, known.height)
        {
            return sized(Size { width, height });
        }
        let width = match known.width {
            Some(width) => width,
            None => {
                let room = inputs.available_space.width.maybe_sub(pb_size.width);
                (self.flow_content_width(id, room) + pb_size.width)
                    .maybe_clamp(own.min.width, own.max.width)
                    .max(pb_size.width)
            }
        };
        if inputs.run_mode == RunMode::ComputeSize && inputs.axis == RequestedAxis::Horizontal {
            return sized(Size { width, height: 0.0 });
        }
        let mut bfc = Bfc {
            floats: FloatSet::default(),
            commit: inputs.run_mode == RunMode::PerformLayout,
        };
        let known_height = known.height;
        let geo = Geo {
            x: 0.0,
            y: 0.0,
            width,
            pb: own.pb,
            border: own.border,
            known_height,
            min_height: own.min.height,
            max_height: own.max.height,
            basis: Size {
                width: Some((width - pb_size.width).max(0.0)),
                height: known_height
                    .filter(|_| inputs.known_dimensions_are_definite.height)
                    .map(|height| (height - pb_size.height).max(0.0)),
            },
            collapsible: Line::FALSE,
            root: true,
        };
        let out = self.block(&mut bfc, id, &geo);
        let size = Size {
            width,
            height: out.height,
        };
        let mut output = sized(size);
        output.baselines = Baselines::from_first(out.baseline);
        output.scrollable_overflow_rect = Rect {
            left: 0.0,
            top: 0.0,
            right: out.reach.width.max(size.width),
            bottom: out.reach.height.max(size.height),
        };
        output
    }

    /// What `id`, a block holding floats, would be wide at its narrowest or its
    /// widest, or when it has to fit in `room`: the widest of what is in it.
    fn flow_content_width(&mut self, id: NodeId, room: AvailableSpace) -> f32 {
        let children = self.tree.node(id).children.clone();
        let mut widest = 0.0f32;
        let mut beside = 0.0f32;
        let mut floats = taffy::FloatIntrinsicWidthCalculator::new(room);
        for child in children {
            let part = self.part_of(child);
            if matches!(part, Part::Hidden | Part::Absolute) {
                continue;
            }
            let style = self.tree.node(child).style.clone();
            let margins = style.margin.resolve_or_zero(None, calc).sum_axes().width;
            let inputs = LayoutInput {
                run_mode: RunMode::ComputeSize,
                sizing_mode: SizingMode::InherentSize,
                axis: RequestedAxis::Horizontal,
                known_dimensions: Size::NONE,
                known_dimensions_are_definite: Size {
                    width: false,
                    height: false,
                },
                parent_size: Size::NONE,
                available_space: Size {
                    width: room,
                    height: AvailableSpace::MaxContent,
                },
                vertical_margins_are_collapsible: Line::TRUE,
            };
            let width = self.child_layout(child, inputs, None).size.width + margins;
            if part == Part::Float {
                let direction = if style.float == taffy::Float::Right {
                    taffy::FloatDirection::Right
                } else {
                    taffy::FloatDirection::Left
                };
                floats.add_float(width, direction, style.clear);
                if style.clear == taffy::Clear::None {
                    beside += width;
                }
            } else {
                widest = widest.max(width);
            }
        }
        // The lines of the block itself, beside its floats.
        let lines = self.inline_width(id, room);
        if lines > 0.0 {
            widest = widest.max(lines + beside);
        }
        widest.max(floats.result())
    }

    pub(crate) fn set_layout(&mut self, id: NodeId, layout: Layout) {
        self.tree.live_mut(id).layout = layout;
    }

    /// A child with `display: none`, which has a layout of nothing.
    pub(crate) fn hide(&mut self, id: NodeId, order: u32) {
        self.set_layout(id, Layout::with_order(order));
        self.child_layout(id, LayoutInput::HIDDEN, None);
    }

    /// Where a box whose top margin is `top` starts below `walk.committed`, and
    /// whether it was moved down by clearance. `clear` is what it clears.
    pub(crate) fn offset_of(
        &self,
        bfc: &Bfc,
        g: &Geo,
        walk: &Walk,
        collapse_start: bool,
        top: CollapsibleMarginSet,
        clear: ClearSides,
    ) -> (f32, bool) {
        let joins_parent = walk.collapsing_first && collapse_start;
        let offset = if joins_parent {
            0.0
        } else {
            walk.active.collapse_with_set(top).resolve()
        };
        if clear.any()
            && let Some(bottom) = bfc.floats.clear_bottom(clear)
        {
            // Where the box would be if nothing were cleared.
            let hypothetical = g.y + walk.committed + walk.active.collapse_with_set(top).resolve();
            if hypothetical < bottom - EPSILON {
                return (bottom - (g.y + walk.committed), true);
            }
        }
        (offset, false)
    }

    /// The top margin of `id` that a box above it can be collapsed with, as far
    /// as its style says: its own, with that of its first child when the two
    /// are adjoining.
    pub(crate) fn estimate_top(
        &self,
        id: NodeId,
        width: f32,
        depth: usize,
    ) -> CollapsibleMarginSet {
        let node = self.tree.node(id);
        let own = CollapsibleMarginSet::from_margin(
            node.style.margin.top.resolve_or_zero(Some(width), calc),
        );
        let pb_top = node.style.padding.top.resolve_or_zero(Some(width), calc)
            + node.style.border.top.resolve_or_zero(Some(width), calc);
        if depth == 0 || pb_top > 0.0 || node.style.overflow.y.is_scroll_container() {
            return own;
        }
        let first = node.children.iter().copied().find(|&child| {
            !matches!(
                self.part_of(child),
                Part::Hidden | Part::Absolute | Part::Float
            )
        });
        match first {
            Some(first) if self.part_of(first) == Part::Block => {
                own.collapse_with_set(self.estimate_top(first, width, depth - 1))
            }
            _ => own,
        }
    }
}

/// A point, from two coordinates.
pub(crate) fn at(x: f32, y: f32) -> Point<f32> {
    Point { x, y }
}
