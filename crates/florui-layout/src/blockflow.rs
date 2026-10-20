//! Block layout in a block formatting context that has floats.
//!
//! Taffy lays blocks out, and floats too, but its float placement leaves a hole
//! (a float that is taller than the floats beside it is not remembered below
//! them, so the next float and the lines go where Chrome does not put them),
//! and it has no way to give a line of text the room beside a float. A block
//! formatting context that has a float in it, anywhere in the blocks that
//! share it, is laid out here instead; the rest is Taffy's, as before.
//!
//! The blocks of the context are laid out by one recursive walk that keeps
//! the margins collapsing by the rules of CSS 2.1 8.3.1, puts the floats where
//! [`FloatSet`] says, moves a box that clears below the floats it clears and a
//! box that makes a context of its own (a flow-root, a scroll container, a
//! flex or grid container, a replaced element) beside the floats it must not
//! overlap. Whatever is in such a box is its own: it is laid out by whichever
//! algorithm it has, as a child.
//!
//! Where a block is placed is only known once the margins that collapse into
//! its top are: a block is laid out where they say it will be, and again where
//! its own output says it ended up if that is another place (floats placed in
//! the meantime are forgotten first).

use taffy::util::{MaybeMath, MaybeResolve, ResolveOrZero};
use taffy::{
    CollapsibleMarginSet, Display, Layout, Line, NodeId, Point, Position, Rect, Size, Style,
};

use crate::LeafContext;
use crate::floats::FloatSet;
use crate::tree::Run;

/// What a child of a block is, to the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Part {
    /// `display: none`.
    Hidden,
    /// Taken out of the flow and placed against its container.
    Absolute,
    /// Floated.
    Float,
    /// In the flow, and the root of a context of its own.
    Independent,
    /// In the flow, sharing the context of its parent.
    Block,
}

impl Run<'_> {
    pub(crate) fn part_of(&self, id: NodeId) -> Part {
        let node = self.tree.node(id);
        let style = &node.style;
        if style.display == Display::None {
            Part::Hidden
        } else if style.position == Position::Absolute {
            Part::Absolute
        } else if style.float != taffy::Float::None {
            Part::Float
        } else if style.display != Display::Block
            || self.tree.node(id).role.is_table()
            || style.overflow.x.is_scroll_container()
            || style.overflow.y.is_scroll_container()
            || matches!(
                node.context,
                Some(LeafContext::Image(_) | LeafContext::Fixed(_))
            )
        {
            Part::Independent
        } else {
            Part::Block
        }
    }

    /// Whether the block formatting context `id` is the root of has a float in
    /// it: one among its children, or among the children of the blocks that share
    /// its context.
    pub(crate) fn region_has_float(&mut self, id: NodeId) -> bool {
        if !self.floats {
            return false;
        }
        if let Some(&known) = self.regions.get(&id) {
            return known;
        }
        let node = self.tree.node(id);
        let found = matches!(node.style.display, Display::Block | Display::FlowRoot)
            && node
                .children
                .clone()
                .into_iter()
                .any(|child| match self.part_of(child) {
                    Part::Float => true,
                    Part::Block => self.region_has_float(child),
                    _ => false,
                });
        self.regions.insert(id, found);
        found
    }
}

/// The block formatting context being laid out: where its floats are, and
/// whether the layouts of what is in it are to be written (a context asked only
/// for its size is not).
pub(crate) struct Bfc {
    pub(crate) floats: FloatSet,
    pub(crate) commit: bool,
}

/// A block of the context, as its parent decided it.
#[derive(Clone, Copy)]
pub(crate) struct Geo {
    /// Its border box's top left corner, in the context's coordinates (`y`
    /// where the margins collapsed into its top say it is).
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// Its border box's width.
    pub(crate) width: f32,
    /// Its padding and border.
    pub(crate) pb: Rect<f32>,
    /// The border alone, which is where the padding box starts.
    pub(crate) border: Rect<f32>,
    /// Its border box's height, if its style or its parent settles it, and the
    /// bounds of what it may be.
    pub(crate) known_height: Option<f32>,
    pub(crate) min_height: Option<f32>,
    pub(crate) max_height: Option<f32>,
    /// What the percentages of its children are of: its content box.
    pub(crate) basis: Size<Option<f32>>,
    /// Whether the margins of its first and last child may collapse with its own,
    /// as far as its parent is concerned.
    pub(crate) collapsible: Line<bool>,
    /// Whether it is the root of the context, which contains its floats.
    pub(crate) root: bool,
}

/// What laying a block out came to.
pub(crate) struct Out {
    /// Its border box's height.
    pub(crate) height: f32,
    pub(crate) top: CollapsibleMarginSet,
    pub(crate) bottom: CollapsibleMarginSet,
    pub(crate) through: bool,
    pub(crate) baseline: Option<f32>,
    /// The right and bottom edges of what it holds, from its border box's corner.
    pub(crate) reach: Size<f32>,
}

/// The margins of a box, `None` for `auto`.
pub(crate) type Margins = Rect<Option<f32>>;

pub(crate) fn calc(_: *const (), _: f32) -> f32 {
    0.0
}

/// A box's own padding, border, size and bounds, resolved against the width of
/// the box that holds it.
pub(crate) struct Own {
    pub(crate) padding: Rect<f32>,
    pub(crate) border: Rect<f32>,
    /// Padding and border together.
    pub(crate) pb: Rect<f32>,
    pub(crate) size: Size<Option<f32>>,
    pub(crate) min: Size<Option<f32>>,
    pub(crate) max: Size<Option<f32>>,
}

pub(crate) fn own_of(style: &Style, basis: Size<Option<f32>>) -> Own {
    let padding = style.padding.resolve_or_zero(basis.width, calc);
    let border = style.border.resolve_or_zero(basis.width, calc);
    let pb = padding + border;
    let adjust = if style.box_sizing == taffy::BoxSizing::ContentBox {
        pb.sum_axes()
    } else {
        Size::ZERO
    };
    let ratio = style.aspect_ratio;
    Own {
        padding,
        border,
        pb,
        size: style
            .size
            .maybe_resolve(basis, calc)
            .maybe_apply_aspect_ratio(ratio)
            .maybe_add(adjust),
        min: style
            .min_size
            .maybe_resolve(basis, calc)
            .maybe_apply_aspect_ratio(ratio)
            .maybe_add(adjust),
        max: style
            .max_size
            .maybe_resolve(basis, calc)
            .maybe_apply_aspect_ratio(ratio)
            .maybe_add(adjust),
    }
}

pub(crate) fn margins_of(style: &Style, width: f32) -> Margins {
    style
        .margin
        .map(|margin| margin.resolve_to_option(width, calc))
}

pub(crate) fn or_zero(margins: Margins) -> Rect<f32> {
    margins.map(|margin| margin.unwrap_or(0.0))
}

/// How far `position: relative` moves a box.
pub(crate) fn inset_offset(style: &Style, basis: Size<Option<f32>>) -> Point<f32> {
    if style.position != Position::Relative {
        return Point::ZERO;
    }
    let left = style.inset.left.maybe_resolve(basis.width, calc);
    let right = style.inset.right.maybe_resolve(basis.width, calc);
    let top = style.inset.top.maybe_resolve(basis.height, calc);
    let bottom = style.inset.bottom.maybe_resolve(basis.height, calc);
    Point {
        x: left.or(right.map(|x| -x)).unwrap_or(0.0),
        y: top.or(bottom.map(|y| -y)).unwrap_or(0.0),
    }
}

/// The layout of a box placed in a block.
pub(crate) fn layout_of(
    order: u32,
    location: Point<f32>,
    size: Size<f32>,
    reach: Size<f32>,
    own: &Own,
    margin: Rect<f32>,
) -> Layout {
    Layout {
        order,
        location,
        size,
        scrollable_overflow_rect: Rect {
            left: 0.0,
            top: 0.0,
            right: reach.width.max(size.width),
            bottom: reach.height.max(size.height),
        },
        scrollbar_size: Size::ZERO,
        border: own.border,
        padding: own.padding,
        margin,
    }
}
