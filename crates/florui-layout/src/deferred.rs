//! Sizes only a laid-out tree can give: a math function over a percentage
//! (`calc(100% - 20px)`, `clamp(50px, 30%, 100px)`), a percentage where Taffy
//! has none (`max-width`, `margin`, `padding`, `gap`, `flex-basis`), and the
//! keywords that name what a box holds or the room it has (`min-content`,
//! `max-content`, `fit-content`, `stretch`).
//!
//! Taffy takes a length or a plain percentage, so each such value is worked
//! out here from the box it is a percentage of, as laid out by a first pass,
//! and written into the node's Taffy style as a length. The tree is laid out
//! again with those, and the values worked out again from that layout, until
//! they stop changing (a box whose size depends on its parent's, which may
//! depend on its own, settles in a pass or two; three at most).

use florui_style::{
    BoxSizing, ComputedStyle, DeferredSlot as Slot, DeferredValue, LengthPercentage, SizeKeyword,
};

use super::*;

/// The most times the sizes are worked out and the tree laid out again.
const PASSES: usize = 3;

/// The border-box size a box takes at its smallest and its largest.
#[derive(Clone, Copy)]
struct Intrinsic {
    min_width: f32,
    max_width: f32,
}

/// Works out the deferred values of every node that has any, and lays
/// `roots` out again with them. `id_of` finds the Taffy node of an arena
/// node; `roots` are the roots the tree was laid out from, with `available`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    tree: &mut TaffyTree<LeafContext>,
    id_of: &dyn Fn(NodeId) -> Option<taffy::NodeId>,
    roots: &[taffy::NodeId],
    available: Size<AvailableSpace>,
    count: usize,
) -> Result<(), LayoutError> {
    let mut nodes: Vec<NodeId> = styles
        .iter()
        .filter(|(_, style)| !style.deferred.is_empty() || style.has_grid_calc())
        .map(|(&node, _)| node)
        .filter(|&node| id_of(node).is_some())
        .collect();
    if nodes.is_empty() {
        return Ok(());
    }
    // Parents come first: ids follow document order.
    nodes.sort_unstable();

    let intrinsics = measure_intrinsics(font, styles, tree, id_of, &nodes, count)?;
    for _ in 0..PASSES {
        let mut changed = false;
        for &node in &nodes {
            let (Some(tid), Some(style)) = (id_of(node), styles.get(&node)) else {
                continue;
            };
            let basis = containing_block(arena, styles, tree, id_of, node, available);
            let own = own_content_box(arena, styles, tree, tid, node, available);
            let patched = patch(style, basis, own, intrinsics.get(&node).copied());
            let taffy_style = to_taffy_style(Some(&patched));
            if tree.style(tid).map_err(LayoutError)? != &taffy_style {
                tree.set_style(tid, taffy_style).map_err(LayoutError)?;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        for &root in roots {
            run_layout(font, tree, root, available, count)?;
        }
    }
    Ok(())
}

/// The smallest and largest width of each node with a keyword for a width,
/// as the node alone would take with the room it has not mattering.
fn measure_intrinsics(
    font: &mut florui_text::Font,
    styles: &StyleMap,
    tree: &mut TaffyTree<LeafContext>,
    id_of: &dyn Fn(NodeId) -> Option<taffy::NodeId>,
    nodes: &[NodeId],
    count: usize,
) -> Result<HashMap<NodeId, Intrinsic>, LayoutError> {
    let mut out = HashMap::new();
    for &node in nodes {
        let (Some(tid), Some(style)) = (id_of(node), styles.get(&node)) else {
            continue;
        };
        let wants_width = style.deferred.iter().any(|d| {
            matches!(
                d.slot,
                Slot::Width | Slot::MinWidth | Slot::MaxWidth | Slot::FlexBasis
            ) && matches!(
                d.value,
                DeferredValue::Keyword(
                    SizeKeyword::MinContent | SizeKeyword::MaxContent | SizeKeyword::FitContent
                ) | DeferredValue::FitContentOf(_)
            )
        });
        if !wants_width {
            continue;
        }
        let mut width_at = |space: AvailableSpace| -> Result<f32, LayoutError> {
            run_layout(
                font,
                tree,
                tid,
                Size {
                    width: space,
                    height: AvailableSpace::MaxContent,
                },
                count,
            )?;
            Ok(tree.layout(tid).map_err(LayoutError)?.size.width)
        };
        out.insert(
            node,
            Intrinsic {
                min_width: width_at(AvailableSpace::MinContent)?,
                max_width: width_at(AvailableSpace::MaxContent)?,
            },
        );
    }
    Ok(out)
}

/// The box a percentage of `node` is of: width, and height when it is known,
/// and whether the main axis of the flex container around it is horizontal.
#[derive(Clone, Copy)]
struct Basis {
    width: f32,
    height: Option<f32>,
    row: bool,
}

fn containing_block(
    arena: &Tree,
    styles: &StyleMap,
    tree: &TaffyTree<LeafContext>,
    id_of: &dyn Fn(NodeId) -> Option<taffy::NodeId>,
    node: NodeId,
    available: Size<AvailableSpace>,
) -> Basis {
    let viewport = || Basis {
        width: match available.width {
            AvailableSpace::Definite(width) => width,
            _ => 0.0,
        },
        height: match available.height {
            AvailableSpace::Definite(height) => Some(height),
            _ => None,
        },
        row: true,
    };
    let Some(parent) =
        layout_parent(arena, node).filter(|_| !is_viewport_fixed(arena, styles, node))
    else {
        return viewport();
    };
    let (Some(parent_style), Some(parent_id)) = (styles.get(&parent), id_of(parent)) else {
        return viewport();
    };
    let Ok(layout) = tree.layout(parent_id) else {
        return viewport();
    };
    let own_position = styles.get(&node).map(|s| s.position);
    // An absolutely positioned box is placed against the padding box.
    let (left, right, top, bottom) = if own_position == Some(StylePosition::Absolute) {
        (
            parent_style.border.left.width,
            parent_style.border.right.width,
            parent_style.border.top.width,
            parent_style.border.bottom.width,
        )
    } else {
        (
            parent_style.border.left.width + parent_style.padding.left,
            parent_style.border.right.width + parent_style.padding.right,
            parent_style.border.top.width + parent_style.padding.top,
            parent_style.border.bottom.width + parent_style.padding.bottom,
        )
    };
    Basis {
        width: (layout.size.width - left - right).max(0.0),
        height: definite_height(arena, styles, parent, available)
            .then(|| (layout.size.height - top - bottom).max(0.0)),
        row: is_row(parent_style.flex_direction),
    }
}

/// The content box of `node` as the last layout left it, for what a grid
/// track is a percentage of. Its height only when that is known without
/// laying out what it holds.
fn own_content_box(
    arena: &Tree,
    styles: &StyleMap,
    tree: &TaffyTree<LeafContext>,
    id: taffy::NodeId,
    node: NodeId,
    available: Size<AvailableSpace>,
) -> Basis {
    let (Some(style), Ok(layout)) = (styles.get(&node), tree.layout(id)) else {
        return Basis {
            width: 0.0,
            height: None,
            row: true,
        };
    };
    let across = style.border.left.width
        + style.border.right.width
        + style.padding.left
        + style.padding.right;
    let down = style.border.top.width
        + style.border.bottom.width
        + style.padding.top
        + style.padding.bottom;
    Basis {
        width: (layout.size.width - across).max(0.0),
        height: definite_height(arena, styles, node, available)
            .then(|| (layout.size.height - down).max(0.0)),
        row: true,
    }
}

/// Whether the height of `node` is known without laying out what it holds,
/// which a percentage height needs of its parent.
fn definite_height(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    available: Size<AvailableSpace>,
) -> bool {
    let mut current = Some(node);
    while let Some(id) = current {
        let Some(style) = styles.get(&id) else {
            return false;
        };
        if style.height.is_some() {
            return true;
        }
        if style.height_percent.is_none()
            && !style
                .deferred
                .iter()
                .any(|d| d.slot == Slot::Height && !matches!(d.value, DeferredValue::Keyword(_)))
        {
            return false;
        }
        current = layout_parent(arena, id);
    }
    matches!(available.height, AvailableSpace::Definite(_))
}

/// `style` with its deferred values worked out against `basis`.
fn patch(
    style: &ComputedStyle,
    basis: Basis,
    own: Basis,
    intrinsic: Option<Intrinsic>,
) -> ComputedStyle {
    let mut patched = style.clone();
    patched.deferred.clear();
    let margin_x = style.margin.left.unwrap_or(0.0) + style.margin.right.unwrap_or(0.0);
    let padding_x = style.padding.left
        + style.padding.right
        + style.border.left.width
        + style.border.right.width;
    // The width a border box of `outer` is written as in this box's sizing.
    let written = |outer: f32| match style.box_sizing {
        BoxSizing::BorderBox => outer,
        BoxSizing::ContentBox => (outer - padding_x).max(0.0),
    };
    for deferred in &style.deferred {
        let slot = deferred.slot;
        let horizontal = matches!(
            slot,
            Slot::Width
                | Slot::MinWidth
                | Slot::MaxWidth
                | Slot::MarginLeft
                | Slot::MarginRight
                | Slot::PaddingLeft
                | Slot::PaddingRight
                | Slot::InsetLeft
                | Slot::InsetRight
                | Slot::ColumnGap
        ) || (slot == Slot::FlexBasis && basis.row);
        // Margins and paddings are of the width on every side.
        let of_width = horizontal
            || matches!(
                slot,
                Slot::MarginTop | Slot::MarginBottom | Slot::PaddingTop | Slot::PaddingBottom
            );
        // A gap is a percentage of the container's own content box, not of
        // the box around it.
        let reference = match slot {
            Slot::ColumnGap => Some(own.width),
            Slot::RowGap => own.height,
            _ if of_width => Some(basis.width),
            _ => basis.height,
        };
        let value = match &deferred.value {
            DeferredValue::Math(calc) => reference.map(|reference| calc.resolve(reference)),
            DeferredValue::Keyword(keyword) => match keyword {
                SizeKeyword::Stretch => {
                    if horizontal {
                        Some(written((basis.width - margin_x).max(0.0)))
                    } else {
                        basis.height.map(|height| {
                            let margin_y = style.margin.top.unwrap_or(0.0)
                                + style.margin.bottom.unwrap_or(0.0);
                            (height - margin_y).max(0.0)
                        })
                    }
                }
                // For a height these are what `auto` already is.
                _ if !horizontal => None,
                SizeKeyword::MinContent => intrinsic.map(|i| written(i.min_width)),
                SizeKeyword::MaxContent => intrinsic.map(|i| written(i.max_width)),
                SizeKeyword::FitContent => intrinsic.map(|i| {
                    let room = (basis.width - margin_x).max(0.0);
                    written(room.min(i.max_width).max(i.min_width))
                }),
            },
            DeferredValue::FitContentOf(calc) => {
                if horizontal {
                    intrinsic.map(|i| {
                        written(calc.resolve(basis.width).min(i.max_width).max(i.min_width))
                    })
                } else {
                    None
                }
            }
        };
        let Some(value) = value else {
            continue;
        };
        let length = LengthPercentage {
            length: value,
            percentage: 0.0,
        };
        match slot {
            Slot::Width => {
                patched.width = Some(value);
                patched.width_percent = None;
            }
            Slot::Height => {
                patched.height = Some(value);
                patched.height_percent = None;
            }
            Slot::MinWidth => patched.min_width = Some(length),
            Slot::MinHeight => patched.min_height = Some(length),
            Slot::MaxWidth => patched.max_width = Some(value),
            Slot::MaxHeight => patched.max_height = Some(value),
            Slot::FlexBasis => patched.flex_basis = Some(value),
            Slot::MarginTop => patched.margin.top = Some(value),
            Slot::MarginRight => patched.margin.right = Some(value),
            Slot::MarginBottom => patched.margin.bottom = Some(value),
            Slot::MarginLeft => patched.margin.left = Some(value),
            Slot::PaddingTop => patched.padding.top = value,
            Slot::PaddingRight => patched.padding.right = value,
            Slot::PaddingBottom => patched.padding.bottom = value,
            Slot::PaddingLeft => patched.padding.left = value,
            Slot::InsetTop => patched.inset.top = Some(length),
            Slot::InsetRight => patched.inset.right = Some(length),
            Slot::InsetBottom => patched.inset.bottom = Some(length),
            Slot::InsetLeft => patched.inset.left = Some(length),
            Slot::ColumnGap => patched.column_gap = value,
            Slot::RowGap => patched.row_gap = value,
        }
    }
    if style.has_grid_calc() {
        patched.grid_template_columns = style.grid_template_columns.resolve_calc(Some(own.width));
        patched.grid_template_rows = style.grid_template_rows.resolve_calc(own.height);
        patched.grid_auto_columns = style
            .grid_auto_columns
            .iter()
            .map(|track| track.resolve_calc(Some(own.width)))
            .collect();
        patched.grid_auto_rows = style
            .grid_auto_rows
            .iter()
            .map(|track| track.resolve_calc(own.height))
            .collect();
    }
    patched
}
