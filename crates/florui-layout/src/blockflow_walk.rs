//! The walk over the blocks of a context that has floats. See
//! [`crate::blockflow`].

use taffy::util::MaybeMath;
use taffy::{
    AvailableSpace, CollapsibleMarginSet, Layout, LayoutInput, Line, NodeId, Position, Rect,
    RunMode, Size, SizingMode,
};

use crate::LeafContext;
use crate::blockflow::{
    Bfc, Geo, Margins, Out, Part, calc, inset_offset, layout_of, margins_of, or_zero, own_of,
};
use crate::blockflow_root::{Walk, at};
use crate::floats::ClearSides;
use crate::tree::Run;

/// How many times a block that landed elsewhere than it was laid out is laid out
/// again.
const RELAYOUTS: usize = 4;

/// Slack for lengths that should be equal.
const EPSILON: f32 = 0.01;

/// A child of a block, placed.
struct Placed {
    /// Its border box's corner from the block's, and its size.
    x: f32,
    y: f32,
    size: Size<f32>,
    top: CollapsibleMarginSet,
    bottom: CollapsibleMarginSet,
    /// Its margins can collapse through it.
    through: bool,
    /// It is a self-collapsing box that cleared.
    self_collapsing_cleared: bool,
    /// It was moved down by clearance, or to get out from beside floats, which
    /// ends the collapsing of its margins with those above it.
    separated: bool,
    baseline: Option<f32>,
    reach: Size<f32>,
}

fn clear_sides(clear: taffy::Clear) -> ClearSides {
    ClearSides {
        left: matches!(clear, taffy::Clear::Left | taffy::Clear::Both),
        right: matches!(clear, taffy::Clear::Right | taffy::Clear::Both),
    }
}

impl Run<'_> {
    /// Lays out the block `id` as part of the context `bfc`, where `g` puts it.
    pub(crate) fn block(&mut self, bfc: &mut Bfc, id: NodeId, g: &Geo) -> Out {
        let (style, children, inline) = {
            let node = self.tree.node(id);
            (
                node.style.clone(),
                node.children.clone(),
                matches!(
                    node.context,
                    Some(LeafContext::Text(_) | LeafContext::Inline(_))
                ),
            )
        };
        let inner_w = (g.width - g.pb.left - g.pb.right).max(0.0);
        let establishes =
            style.overflow.x.is_scroll_container() || style.overflow.y.is_scroll_container();
        let relative = style.position == Position::Relative;
        let collapse = Line {
            start: g.collapsible.start && !g.root && !establishes && relative && g.pb.top == 0.0,
            end: g.collapsible.end
                && !g.root
                && !establishes
                && relative
                && g.pb.bottom == 0.0
                && g.known_height.is_none(),
        };
        let prevents_through = g.root
            || establishes
            || style.position == Position::Absolute
            || g.pb.top > 0.0
            || g.pb.bottom > 0.0
            || matches!(g.known_height, Some(h) if h > 0.0)
            || matches!(g.min_height, Some(h) if h > 0.0);

        let mut walk = Walk {
            committed: g.pb.top,
            active: CollapsibleMarginSet::ZERO,
            collapsing_first: true,
            first_top: CollapsibleMarginSet::ZERO,
            cleared_active: false,
        };
        let mut y_abs = g.pb.top;
        let mut baseline: Option<f32> = None;
        let mut reach = Size {
            width: g.width,
            height: 0.0,
        };
        let mut all_through = true;
        let mut absolutes: Vec<(NodeId, u32, taffy::Point<f32>)> = Vec::new();

        if inline {
            let content = self.inline_content(bfc, id, g, inner_w);
            walk.committed = g.pb.top + content.height;
            baseline = content.baseline.map(|b| g.pb.top + b);
            reach.height = reach.height.max(walk.committed);
            reach.width = reach.width.max(content.reach_width + g.pb.left);
            all_through = content.height == 0.0 && content.empty;
        } else {
            for (index, &child) in children.iter().enumerate() {
                let order = index as u32;
                match self.part_of(child) {
                    Part::Hidden => {
                        if bfc.commit {
                            self.hide(child, order);
                        }
                    }
                    Part::Absolute => absolutes.push((child, order, at(g.pb.left, y_abs))),
                    Part::Float => {
                        let pending = if walk.collapsing_first && collapse.start {
                            0.0
                        } else {
                            walk.active.resolve()
                        };
                        let min_y = g.y + walk.committed + pending;
                        let rect = self.place_float(bfc, g, child, inner_w, min_y, order);
                        reach.width = reach.width.max(rect.right);
                        reach.height = reach.height.max(rect.bottom);
                    }
                    Part::Block | Part::Independent => {
                        let placed = if self.part_of(child) == Part::Block {
                            self.flow_block_child(
                                bfc,
                                g,
                                &walk,
                                collapse.start,
                                child,
                                inner_w,
                                order,
                            )
                        } else {
                            self.flow_independent_child(
                                bfc,
                                g,
                                &walk,
                                collapse.start,
                                child,
                                inner_w,
                                order,
                            )
                        };
                        reach.width = reach.width.max(placed.x + placed.reach.width);
                        reach.height = reach.height.max(placed.y + placed.reach.height);
                        if baseline.is_none() {
                            baseline = placed.baseline.map(|b| placed.y + b);
                        }
                        all_through &= placed.through || placed.self_collapsing_cleared;
                        // Where the margins above this box end up, for what follows.
                        if walk.collapsing_first && placed.separated {
                            walk.collapsing_first = false;
                        } else if walk.collapsing_first {
                            if placed.through {
                                walk.first_top = walk
                                    .first_top
                                    .collapse_with_set(placed.top)
                                    .collapse_with_set(placed.bottom);
                            } else {
                                walk.first_top = walk.first_top.collapse_with_set(placed.top);
                                walk.collapsing_first = false;
                            }
                        }
                        if placed.through {
                            walk.active = walk
                                .active
                                .collapse_with_set(placed.top)
                                .collapse_with_set(placed.bottom);
                            y_abs = placed.y + placed.size.height;
                        } else {
                            walk.committed = placed.y + placed.size.height;
                            if placed.self_collapsing_cleared {
                                walk.committed -= placed.top.resolve();
                                walk.active = placed.top.collapse_with_set(placed.bottom);
                                walk.cleared_active = true;
                            } else {
                                walk.active = placed.bottom;
                                walk.cleared_active = false;
                            }
                            y_abs = walk.committed + walk.active.resolve();
                        }
                    }
                }
            }
        }

        let last_bottom = if walk.cleared_active {
            CollapsibleMarginSet::ZERO
        } else {
            walk.active
        };
        let bottom_offset = if walk.cleared_active {
            walk.active.resolve()
        } else if collapse.end {
            0.0
        } else {
            last_bottom.resolve()
        };
        let mut content_bottom = walk.committed + bottom_offset;
        if g.root || establishes {
            // A context contains its floats.
            if let Some(lowest) = bfc.floats.lowest_bottom() {
                content_bottom = content_bottom.max(lowest - g.y);
            }
        }
        let auto_height = content_bottom + g.pb.bottom;
        let height = g
            .known_height
            .unwrap_or_else(|| auto_height.maybe_clamp(g.min_height, g.max_height))
            .max(g.pb.top + g.pb.bottom);
        // A box taller than its content because of `min-height` keeps the
        // margin of its last child inside.
        let constrained_by_min = matches!(g.min_height, Some(h) if h > 0.0 && h >= height);
        let collapses_end = collapse.end && !constrained_by_min;

        if bfc.commit && !absolutes.is_empty() {
            let area = Size {
                width: g.width - g.border.left - g.border.right,
                height: height - g.border.top - g.border.bottom,
            };
            for (child, order, static_pos) in absolutes {
                let extent = self.layout_absolute(
                    child,
                    order,
                    static_pos,
                    area,
                    at(g.border.left, g.border.top),
                );
                reach.width = reach.width.max(extent.width);
                reach.height = reach.height.max(extent.height);
            }
        }
        reach.height = reach.height.max(height);

        Out {
            height,
            top: if collapse.start {
                walk.first_top
            } else {
                CollapsibleMarginSet::ZERO
            },
            bottom: if collapses_end {
                last_bottom
            } else {
                CollapsibleMarginSet::ZERO
            },
            through: !prevents_through && all_through,
            baseline,
            reach,
        }
    }

    /// Lays out `id`, a block of the context in the flow, where its margins and
    /// the floats say it goes.
    #[allow(clippy::too_many_arguments)]
    fn flow_block_child(
        &mut self,
        bfc: &mut Bfc,
        g: &Geo,
        walk: &Walk,
        collapse_start: bool,
        id: NodeId,
        inner_w: f32,
        order: u32,
    ) -> Placed {
        let style = self.tree.node(id).style.clone();
        let margins = margins_of(&style, inner_w);
        let non_auto = or_zero(margins);
        let own = own_of(
            &style,
            Size {
                width: Some(inner_w),
                height: g.basis.height,
            },
        );
        let stretch = inner_w - non_auto.left - non_auto.right;
        let width = own
            .size
            .width
            .unwrap_or(stretch)
            .maybe_clamp(own.min.width, own.max.width)
            .max(own.pb.left + own.pb.right);
        let (margin_left, margin_right) = auto_margins(margins, stretch - width);
        let known_height = own.size.height.maybe_clamp(own.min.height, own.max.height);
        let clear = clear_sides(style.clear);
        let inset = inset_offset(
            &style,
            Size {
                width: Some(inner_w),
                height: g.basis.height,
            },
        );

        let mut top = self.estimate_top(id, inner_w, 8);
        let mut settled = None;
        for attempt in 0..RELAYOUTS {
            let (offset, cleared) = self.offset_of(bfc, g, walk, collapse_start, top, clear);
            let geo = Geo {
                x: g.x + g.pb.left + margin_left,
                y: g.y + walk.committed + offset,
                width,
                pb: own.pb,
                border: own.border,
                known_height,
                min_height: own.min.height,
                max_height: own.max.height,
                basis: Size {
                    width: Some((width - own.pb.left - own.pb.right).max(0.0)),
                    height: known_height.map(|h| (h - own.pb.top - own.pb.bottom).max(0.0)),
                },
                collapsible: Line::TRUE,
                root: false,
            };
            let checkpoint = bfc.floats.len();
            let out = self.block(bfc, id, &geo);
            let actual_top = out.top.collapse_with_margin(non_auto.top);
            let (offset_now, cleared_now) =
                self.offset_of(bfc, g, walk, collapse_start, actual_top, clear);
            let moved = (offset_now - offset).abs() > EPSILON || cleared_now != cleared;
            if moved && attempt + 1 < RELAYOUTS {
                bfc.floats.truncate(checkpoint);
                top = actual_top;
                continue;
            }
            settled = Some((out, offset_now, cleared_now, actual_top));
            break;
        }
        let Some((out, offset, cleared, top_set)) = settled else {
            unreachable!("the loop settles on its last attempt");
        };
        let bottom_set = out.bottom.collapse_with_margin(non_auto.bottom);
        let y = walk.committed + offset;
        let x = g.pb.left + margin_left;
        if bfc.commit {
            self.set_layout(
                id,
                layout_of(
                    order,
                    at(x + inset.x, y + inset.y),
                    Size {
                        width,
                        height: out.height,
                    },
                    out.reach,
                    &own,
                    Rect {
                        left: margin_left,
                        right: margin_right,
                        top: top_set.resolve(),
                        bottom: bottom_set.resolve(),
                    },
                ),
            );
        }
        Placed {
            x,
            y,
            size: Size {
                width,
                height: out.height,
            },
            top: top_set,
            bottom: bottom_set,
            through: out.through && !cleared,
            self_collapsing_cleared: out.through && cleared,
            separated: cleared,
            baseline: out.baseline,
            reach: out.reach,
        }
    }

    /// Lays out `id`, a box of the flow that is the root of a context of its own
    /// (or a replaced one), beside the floats it may not overlap.
    #[allow(clippy::too_many_arguments)]
    fn flow_independent_child(
        &mut self,
        bfc: &mut Bfc,
        g: &Geo,
        walk: &Walk,
        collapse_start: bool,
        id: NodeId,
        inner_w: f32,
        order: u32,
    ) -> Placed {
        let node = self.tree.node(id);
        let style = node.style.clone();
        let replaced = matches!(
            node.context,
            Some(LeafContext::Image(_) | LeafContext::Fixed(_))
        );
        let margins = margins_of(&style, inner_w);
        let non_auto = or_zero(margins);
        let basis = Size {
            width: Some(inner_w),
            height: g.basis.height,
        };
        let own = own_of(&style, basis);
        let top = CollapsibleMarginSet::from_margin(non_auto.top);
        let bottom = CollapsibleMarginSet::from_margin(non_auto.bottom);
        let clear = clear_sides(style.clear);
        let inset = inset_offset(&style, basis);
        let cb = (g.x + g.pb.left, g.x + g.width - g.pb.right);
        let known_height = own.size.height.maybe_clamp(own.min.height, own.max.height);

        let (offset, cleared) = self.offset_of(bfc, g, walk, collapse_start, top, clear);
        let mut y = g.y + walk.committed + offset;
        let mut moved = false;
        let (left, room, output, width) = loop {
            let (left, right) = bfc.floats.room(y, 0.0, cb.0, cb.1);
            let beside_floats = left > cb.0 + EPSILON || right < cb.1 - EPSILON;
            let room = (right - left - non_auto.left - non_auto.right).max(0.0);
            let known_width = if replaced {
                None
            } else {
                Some(
                    own.size
                        .width
                        .unwrap_or(room)
                        .maybe_clamp(own.min.width, own.max.width)
                        .max(own.pb.left + own.pb.right),
                )
            };
            let inputs = LayoutInput {
                run_mode: if bfc.commit {
                    RunMode::PerformLayout
                } else {
                    RunMode::ComputeSize
                },
                sizing_mode: SizingMode::InherentSize,
                axis: taffy::RequestedAxis::Both,
                known_dimensions: Size {
                    width: known_width,
                    height: if replaced { None } else { known_height },
                },
                known_dimensions_are_definite: Size {
                    width: true,
                    height: true,
                },
                parent_size: basis,
                available_space: Size {
                    width: AvailableSpace::Definite(room),
                    height: AvailableSpace::MaxContent,
                },
                vertical_margins_are_collapsible: Line::FALSE,
            };
            let output = self.child_layout(id, inputs, None);
            if beside_floats && output.size.width > room + EPSILON {
                match bfc.floats.bottom_below(y) {
                    Some(next) if next > y => {
                        y = next;
                        moved = true;
                        continue;
                    }
                    _ => {}
                }
            }
            break (left, room, output, output.size.width);
        };
        let (margin_left, margin_right) = auto_margins(margins, room - width);
        let x = left - g.x + margin_left;
        let y_block = y - g.y;
        if bfc.commit {
            self.set_layout(
                id,
                layout_of(
                    order,
                    at(x + inset.x, y_block + inset.y),
                    output.size,
                    taffy::Size::ZERO,
                    &own,
                    Rect {
                        left: margin_left,
                        right: margin_right,
                        top: non_auto.top,
                        bottom: non_auto.bottom,
                    },
                ),
            );
            // The layout the child's own algorithm kept for its content stays; only
            // the box is ours to place.
            let kept = self.tree.live(id).layout;
            self.tree.live_mut(id).layout.scrollable_overflow_rect = Rect {
                left: 0.0,
                top: 0.0,
                right: output.scrollable_overflow_rect.right.max(output.size.width),
                bottom: output
                    .scrollable_overflow_rect
                    .bottom
                    .max(output.size.height),
            };
            let _ = kept;
        }
        Placed {
            x,
            y: y_block,
            size: output.size,
            top,
            bottom,
            through: false,
            self_collapsing_cleared: false,
            separated: cleared || moved,
            baseline: output.baselines.first,
            reach: Size {
                width: output.scrollable_overflow_rect.right.max(output.size.width),
                height: output
                    .scrollable_overflow_rect
                    .bottom
                    .max(output.size.height),
            },
        }
    }

    /// Lays out the float `id` of the block `g` and measures its margin box.
    pub(crate) fn float_box(&mut self, bfc: &Bfc, g: &Geo, id: NodeId, inner_w: f32) -> FloatBox {
        let style = self.tree.node(id).style.clone();
        let basis = Size {
            width: Some(inner_w),
            height: g.basis.height,
        };
        let margins = or_zero(margins_of(&style, inner_w));
        let own = own_of(&style, basis);
        let room = (inner_w - margins.left - margins.right).max(0.0);
        let known_width = own.size.width.maybe_clamp(own.min.width, own.max.width);
        let known_height = own.size.height.maybe_clamp(own.min.height, own.max.height);
        let inputs = LayoutInput {
            run_mode: if bfc.commit {
                RunMode::PerformLayout
            } else {
                RunMode::ComputeSize
            },
            sizing_mode: SizingMode::InherentSize,
            axis: taffy::RequestedAxis::Both,
            known_dimensions: Size {
                width: known_width,
                height: known_height,
            },
            known_dimensions_are_definite: Size {
                width: true,
                height: true,
            },
            parent_size: basis,
            available_space: Size {
                width: AvailableSpace::Definite(room),
                height: AvailableSpace::MaxContent,
            },
            vertical_margins_are_collapsible: Line::FALSE,
        };
        let output = self.child_layout(id, inputs, None);
        FloatBox {
            size: output.size,
            reach: Size {
                width: output.scrollable_overflow_rect.right,
                height: output.scrollable_overflow_rect.bottom,
            },
            margins,
            own,
            left: style.float == taffy::Float::Left,
            clear: clear_sides(style.clear),
        }
    }

    /// Writes the layout of a float that was placed with its margin box at
    /// `(x, y)` in the context, in the block `g`.
    pub(crate) fn set_float_layout(
        &mut self,
        g: &Geo,
        id: NodeId,
        order: u32,
        float: &FloatBox,
        (x, y): (f32, f32),
    ) {
        self.set_layout(
            id,
            layout_of(
                order,
                at(x - g.x + float.margins.left, y - g.y + float.margins.top),
                float.size,
                float.reach,
                &float.own,
                float.margins,
            ),
        );
    }

    /// Places the float `id` of the block `g`, no higher than `min_y`, and lays
    /// it out. Returns its margin box, from the block's corner.
    pub(crate) fn place_float(
        &mut self,
        bfc: &mut Bfc,
        g: &Geo,
        id: NodeId,
        inner_w: f32,
        min_y: f32,
        order: u32,
    ) -> FloatRect {
        let float = self.float_box(bfc, g, id, inner_w);
        let mut y = min_y;
        if let Some(bottom) = bfc.floats.clear_bottom(float.clear) {
            y = y.max(bottom);
        }
        let cb = (g.x + g.pb.left, g.x + g.width - g.pb.right);
        let outer = float.outer();
        let (fx, fy) = bfc
            .floats
            .place(outer.width, outer.height, float.left, y, cb);
        if bfc.commit {
            self.set_float_layout(g, id, order, &float, (fx, fy));
        }
        FloatRect {
            right: fx - g.x + outer.width,
            bottom: fy - g.y + outer.height,
        }
    }

    /// Lays out `id`, an absolutely positioned child, in the padding box of its
    /// container of size `area` at `offset`, where `static_pos` is where it
    /// would be in the flow. Returns how far it reaches.
    pub(crate) fn layout_absolute(
        &mut self,
        id: NodeId,
        order: u32,
        static_pos: taffy::Point<f32>,
        area: Size<f32>,
        offset: taffy::Point<f32>,
    ) -> Size<f32> {
        let style = self.tree.node(id).style.clone();
        let margin: Margins = style.margin.map(|m| m.resolve_to_option(area.width, calc));
        let own = own_of(&style, area.map(Some));
        let resolve = |side: taffy::LengthPercentageAuto, basis: f32| {
            use taffy::util::MaybeResolve;
            side.maybe_resolve(Some(basis), calc)
        };
        let left = resolve(style.inset.left, area.width);
        let right = resolve(style.inset.right, area.width);
        let top = resolve(style.inset.top, area.height);
        let bottom = resolve(style.inset.bottom, area.height);
        let pb_sum = own.pb.sum_axes();
        let min = own.min.or(pb_sum.map(Some)).maybe_max(pb_sum);
        let mut known = own.size.maybe_clamp(min, own.max);
        if let (None, Some(left), Some(right)) = (known.width, left, right) {
            let width = area.width.maybe_sub(margin.left).maybe_sub(margin.right) - left - right;
            known.width = Some(width.max(0.0));
            known = known.maybe_clamp(min, own.max);
        }
        if let (None, Some(top), Some(bottom)) = (known.height, top, bottom) {
            let height = area.height.maybe_sub(margin.top).maybe_sub(margin.bottom) - top - bottom;
            known.height = Some(height.max(0.0));
            known = known.maybe_clamp(min, own.max);
        }
        let available = Size {
            width: AvailableSpace::Definite(area.width.maybe_clamp(min.width, own.max.width)),
            height: AvailableSpace::Definite(area.height.maybe_clamp(min.height, own.max.height)),
        };
        let measure = |run: &mut Self, known: Size<Option<f32>>, mode: RunMode| {
            run.child_layout(
                id,
                LayoutInput {
                    run_mode: mode,
                    sizing_mode: SizingMode::ContentSize,
                    axis: taffy::RequestedAxis::Both,
                    known_dimensions: known,
                    known_dimensions_are_definite: Size {
                        width: true,
                        height: true,
                    },
                    parent_size: area.map(Some),
                    available_space: available,
                    vertical_margins_are_collapsible: Line::FALSE,
                },
                None,
            )
        };
        let final_size = match (known.width, known.height) {
            (Some(width), Some(height)) => Size { width, height },
            _ => {
                let measured = measure(self, known, RunMode::ComputeSize).size;
                known.unwrap_or(measured)
            }
        }
        .maybe_clamp(min, own.max);
        let output = measure(self, final_size.map(Some), RunMode::PerformLayout);

        let given = Rect {
            left: if left.is_some() {
                margin.left.unwrap_or(0.0)
            } else {
                0.0
            },
            right: if right.is_some() {
                margin.right.unwrap_or(0.0)
            } else {
                0.0
            },
            top: if top.is_some() {
                margin.top.unwrap_or(0.0)
            } else {
                0.0
            },
            bottom: if bottom.is_some() {
                margin.bottom.unwrap_or(0.0)
            } else {
                0.0
            },
        };
        // Auto margins only take the room an inset leaves.
        let space = Size {
            width: right
                .map(|right| area.width - right - left.unwrap_or(0.0))
                .unwrap_or(final_size.width),
            height: bottom
                .map(|bottom| area.height - bottom - top.unwrap_or(0.0))
                .unwrap_or(final_size.height),
        };
        let free = Size {
            width: space.width - final_size.width - given.left - given.right,
            height: space.height - final_size.height - given.top - given.bottom,
        };
        let share = |first: Option<f32>, second: Option<f32>, free: f32| {
            let count = first.is_none() as u8 + second.is_none() as u8;
            if count == 2 && free <= 0.0 {
                0.0
            } else if count > 0 {
                free / f32::from(count)
            } else {
                0.0
            }
        };
        let auto_x = share(margin.left, margin.right, free.width);
        let auto_y = share(margin.top, margin.bottom, free.height);
        let resolved = Rect {
            left: margin.left.unwrap_or(auto_x),
            right: margin.right.unwrap_or(auto_x),
            top: margin.top.unwrap_or(auto_y),
            bottom: margin.bottom.unwrap_or(auto_y),
        };
        let x = match (left, right) {
            (Some(left), _) => left + resolved.left,
            (None, Some(right)) => area.width - final_size.width - right - resolved.right,
            (None, None) => static_pos.x - offset.x + resolved.left,
        };
        let y = match (top, bottom) {
            (Some(top), _) => top + resolved.top,
            (None, Some(bottom)) => area.height - final_size.height - bottom - resolved.bottom,
            (None, None) => static_pos.y - offset.y + resolved.top,
        };
        let location = at(offset.x + x, offset.y + y);
        self.set_layout(
            id,
            Layout {
                order,
                location,
                size: final_size,
                scrollable_overflow_rect: output.scrollable_overflow_rect,
                scrollbar_size: Size::ZERO,
                border: own.border,
                padding: own.padding,
                margin: resolved,
            },
        );
        Size {
            width: location.x + final_size.width,
            height: location.y + final_size.height,
        }
    }
}

/// Where a float ends, from the corner of the block that holds it.
pub(crate) struct FloatRect {
    pub(crate) right: f32,
    pub(crate) bottom: f32,
}

/// The left and right margin of a box with `free` room to spare: the auto ones
/// share it, as long as it is not negative.
fn auto_margins(margins: Margins, free: f32) -> (f32, f32) {
    let count = margins.left.is_none() as u8 + margins.right.is_none() as u8;
    let each = if count > 0 {
        free.max(0.0) / f32::from(count)
    } else {
        0.0
    };
    (margins.left.unwrap_or(each), margins.right.unwrap_or(each))
}

/// A float, laid out and measured, before it is placed.
pub(crate) struct FloatBox {
    pub(crate) size: Size<f32>,
    pub(crate) reach: Size<f32>,
    pub(crate) margins: Rect<f32>,
    pub(crate) own: crate::blockflow::Own,
    /// It floats to the left.
    pub(crate) left: bool,
    pub(crate) clear: ClearSides,
}

impl FloatBox {
    /// Its margin box.
    pub(crate) fn outer(&self) -> Size<f32> {
        Size {
            width: self.size.width + self.margins.left + self.margins.right,
            height: self.size.height + self.margins.top + self.margins.bottom,
        }
    }
}
