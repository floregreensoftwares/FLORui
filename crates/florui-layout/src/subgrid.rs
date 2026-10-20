//! `subgrid`: a grid item that takes the tracks of the grid it is in, on one
//! axis or both, so that its items line up with the items around it.
//!
//! Taffy has no subgrid, so it is worked out around it, one axis at a time.
//! The tracks of the grid that holds the subgrids are sized in a probe where
//! the items of every subgrid take part in that grid, at the lines their
//! subgrid gives them, with what the subgrid's own margin, border, padding
//! and gap add on the edge or between them. The sizes that come out go back
//! as the tracks of that grid, and as the tracks of each subgrid (less what
//! its edges add), and the tree is laid out with those as it is.

use std::collections::HashMap;

use florui_style::{ComputedStyle, GridTemplate, SubgridNames};
use taffy::util::ResolveOrZero;
use taffy::{
    DetailedGridInfo, DetailedGridItemsInfo, DetailedGridTracksInfo, DetailedLayoutInfo,
    GridTemplateComponent, MaxTrackSizingFunction, MinTrackSizingFunction, TrackSizingFunction,
};

use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Column,
    Row,
}

impl Axis {
    fn other(self) -> Axis {
        match self {
            Axis::Column => Axis::Row,
            Axis::Row => Axis::Column,
        }
    }

    fn template(self, style: &ComputedStyle) -> &GridTemplate {
        match self {
            Axis::Column => &style.grid_template_columns,
            Axis::Row => &style.grid_template_rows,
        }
    }

    fn gap_is_normal(self, style: &ComputedStyle) -> bool {
        match self {
            Axis::Column => style.gap_normal.0,
            Axis::Row => style.gap_normal.1,
        }
    }

    /// The first and last line, counted from 1, an item is placed at.
    fn lines(self, item: &DetailedGridItemsInfo) -> (u16, u16) {
        match self {
            Axis::Column => (item.column_start, item.column_end),
            Axis::Row => (item.row_start, item.row_end),
        }
    }

    fn tracks(self, info: &DetailedGridInfo) -> &DetailedGridTracksInfo {
        match self {
            Axis::Column => &info.columns,
            Axis::Row => &info.rows,
        }
    }

    fn gap(self, style: &taffy::Style) -> f32 {
        let gap = match self {
            Axis::Column => style.gap.width,
            Axis::Row => style.gap.height,
        };
        gap.resolve_or_zero(None, |_, _| 0.0)
    }

    fn set_gap(self, style: &mut taffy::Style, gap: f32) {
        match self {
            Axis::Column => style.gap.width = taffy::LengthPercentage::length(gap),
            Axis::Row => style.gap.height = taffy::LengthPercentage::length(gap),
        }
    }

    /// What margin, border and padding take at the start and at the end.
    fn insets(self, style: &taffy::Style) -> (f32, f32) {
        let zero = |_: *const (), _: f32| 0.0;
        let (margin, padding, border) = (style.margin, style.padding, style.border);
        match self {
            Axis::Column => (
                margin.left.resolve_or_zero(None, zero)
                    + padding.left.resolve_or_zero(None, zero)
                    + border.left.resolve_or_zero(None, zero),
                margin.right.resolve_or_zero(None, zero)
                    + padding.right.resolve_or_zero(None, zero)
                    + border.right.resolve_or_zero(None, zero),
            ),
            Axis::Row => (
                margin.top.resolve_or_zero(None, zero)
                    + padding.top.resolve_or_zero(None, zero)
                    + border.top.resolve_or_zero(None, zero),
                margin.bottom.resolve_or_zero(None, zero)
                    + padding.bottom.resolve_or_zero(None, zero)
                    + border.bottom.resolve_or_zero(None, zero),
            ),
        }
    }

    /// What padding and border take, both sides together.
    fn padding_border(self, style: &taffy::Style) -> f32 {
        let zero = |_: *const (), _: f32| 0.0;
        let (padding, border) = (style.padding, style.border);
        match self {
            Axis::Column => {
                padding.left.resolve_or_zero(None, zero)
                    + padding.right.resolve_or_zero(None, zero)
                    + border.left.resolve_or_zero(None, zero)
                    + border.right.resolve_or_zero(None, zero)
            }
            Axis::Row => {
                padding.top.resolve_or_zero(None, zero)
                    + padding.bottom.resolve_or_zero(None, zero)
                    + border.top.resolve_or_zero(None, zero)
                    + border.bottom.resolve_or_zero(None, zero)
            }
        }
    }

    fn add_margins(self, style: &mut taffy::Style, start: f32, end: f32) {
        let zero = |_: *const (), _: f32| 0.0;
        let bump = |margin: &mut taffy::LengthPercentageAuto, by: f32| {
            *margin = taffy::LengthPercentageAuto::length(margin.resolve_or_zero(None, zero) + by);
        };
        match self {
            Axis::Column => {
                bump(&mut style.margin.left, start);
                bump(&mut style.margin.right, end);
            }
            Axis::Row => {
                bump(&mut style.margin.top, start);
                bump(&mut style.margin.bottom, end);
            }
        }
    }

    fn set_placement(self, style: &mut taffy::Style, start: u16, end: u16) {
        let line = taffy::Line {
            start: taffy::style_helpers::line(start as i16),
            end: taffy::style_helpers::line(end as i16),
        };
        match self {
            Axis::Column => style.grid_column = line,
            Axis::Row => style.grid_row = line,
        }
    }

    fn set_size(self, style: &mut taffy::Style, size: f32) {
        match self {
            Axis::Column => style.size.width = taffy::Dimension::length(size),
            Axis::Row => style.size.height = taffy::Dimension::length(size),
        }
    }

    fn size_of(self, layout: &taffy::Layout) -> f32 {
        match self {
            Axis::Column => layout.size.width,
            Axis::Row => layout.size.height,
        }
    }

    fn set_template(
        self,
        style: &mut taffy::Style,
        tracks: Vec<GridTemplateComponent<String>>,
        names: Vec<Vec<String>>,
    ) {
        match self {
            Axis::Column => {
                style.grid_template_columns = tracks;
                style.grid_template_column_names = names;
            }
            Axis::Row => {
                style.grid_template_rows = tracks;
                style.grid_template_row_names = names;
            }
        }
    }

    fn set_implicit(self, style: &mut taffy::Style, tracks: Vec<TrackSizingFunction>) {
        match self {
            Axis::Column => style.grid_auto_columns = tracks,
            Axis::Row => style.grid_auto_rows = tracks,
        }
    }
}

fn fixed(length: f32) -> TrackSizingFunction {
    TrackSizingFunction {
        min: MinTrackSizingFunction::length(length.max(0.0)),
        max: MaxTrackSizingFunction::length(length.max(0.0)),
    }
}

fn auto_track() -> TrackSizingFunction {
    TrackSizingFunction {
        min: MinTrackSizingFunction::auto(),
        max: MaxTrackSizingFunction::auto(),
    }
}

/// The names of the `tracks + 1` lines of a subgrid that starts at line `from`
/// (counted from 0) of the grid it takes its lines from: that grid's names,
/// and the ones the subgrid writes.
fn merge_names(
    source: &[Vec<String>],
    from: usize,
    tracks: usize,
    written: Option<&SubgridNames>,
) -> Vec<Vec<String>> {
    let mut names: Vec<Vec<String>> = (0..=tracks)
        .map(|line| source.get(from + line).cloned().unwrap_or_default())
        .collect();
    if let Some(written) = written {
        for (line, extra) in written.for_lines(tracks + 1).into_iter().enumerate() {
            for name in extra {
                if !names[line].contains(&name) {
                    names[line].push(name);
                }
            }
        }
    }
    names
}

/// `names` for a template: none when no line is named.
fn template_names(names: &[Vec<String>]) -> Vec<Vec<String>> {
    if names.iter().all(Vec::is_empty) {
        Vec::new()
    } else {
        names.to_vec()
    }
}

/// The lines an item of a subgrid of `tracks` tracks is held to: a subgrid has
/// no implicit tracks of its own to the end, so what would go past the last
/// track is put in it (Chrome 153).
fn hold_lines(lines: (u16, u16), tracks: u16) -> (u16, u16) {
    let start = lines.0.min(tracks.max(1));
    let end = lines.1.min(tracks + 1).max(start + 1);
    (start, end)
}

fn grid_info(tree: &TaffyTree<LeafContext>, id: taffy::NodeId) -> Option<&DetailedGridInfo> {
    match tree.detailed_layout_info(id) {
        DetailedLayoutInfo::Grid(info) => Some(info),
        _ => None,
    }
}

/// The children Taffy places as grid items, in the order it lists them.
fn in_flow_children(
    tree: &TaffyTree<LeafContext>,
    id: taffy::NodeId,
) -> Result<Vec<taffy::NodeId>, LayoutError> {
    let mut out = Vec::new();
    for child in tree.children(id).map_err(LayoutError)? {
        let style = tree.style(child).map_err(LayoutError)?;
        if style.display != Display::None && style.position != taffy::Position::Absolute {
            out.push(child);
        }
    }
    Ok(out)
}

/// The tracks of the top grid the probe sized, in the axis.
struct Probed {
    /// The size of each track, explicit ones then implicit.
    sizes: Vec<f32>,
    /// Where each track starts and ends.
    spans: Vec<(f32, f32)>,
    /// How many tracks are explicit.
    explicit: usize,
    /// The names of the explicit lines.
    names: Vec<Vec<String>>,
}

/// A subgrid in the axis, as a grid item of a grid.
struct Sub {
    id: taffy::NodeId,
    /// The subgrid it is an item of, if it is not an item of the top grid.
    parent: Option<usize>,
    /// The lines it spans in its parent, counted from 1.
    in_parent: (u16, u16),
    /// The lines it spans in the top grid, counted from 1.
    top: (u16, u16),
    /// Its items and the lines it places them at, counted from 1.
    items: Vec<(taffy::NodeId, (u16, u16))>,
    /// What its edges take: margin, border and padding, and those of the
    /// subgrids it is at the edge of.
    edge: (f32, f32),
    /// The names of its lines.
    names: Vec<Vec<String>>,
}

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
    // Nearly no document has one: nothing to build then.
    if !styles.iter().any(|(_, style)| {
        style.grid_template_columns.is_subgrid() || style.grid_template_rows.is_subgrid()
    }) {
        return Ok(());
    }
    let is_subgrid = |node: NodeId, axis: Axis| -> bool {
        let Some(style) = styles.get(&node) else {
            return false;
        };
        let in_a_grid = layout_parent(arena, node)
            .and_then(|parent| styles.get(&parent))
            .is_some_and(|parent| parent.display == StyleDisplay::Grid);
        style.display == StyleDisplay::Grid
            && in_a_grid
            && !matches!(
                style.position,
                StylePosition::Absolute | StylePosition::Fixed
            )
            && axis.template(style).is_subgrid()
    };
    let owner: HashMap<taffy::NodeId, NodeId> = styles
        .keys()
        .filter_map(|&node| id_of(node).map(|id| (id, node)))
        .collect();
    for axis in [Axis::Column, Axis::Row] {
        // The grids that hold subgrids and are not subgrids themselves.
        let mut tops: Vec<NodeId> = styles
            .keys()
            .copied()
            .filter(|&node| is_subgrid(node, axis))
            .filter_map(|node| layout_parent(arena, node))
            .filter(|&parent| !is_subgrid(parent, axis))
            .collect();
        tops.sort_unstable();
        tops.dedup();
        for top in tops {
            let Some(top_id) = id_of(top) else {
                continue;
            };
            let ctx = Ctx {
                styles,
                owner: &owner,
                is_subgrid: &|node| is_subgrid(node, axis),
                roots,
                available,
                count,
                axis,
            };
            probe_top(font, tree, &ctx, top_id)?;
        }
    }
    Ok(())
}

struct Ctx<'a> {
    styles: &'a StyleMap,
    owner: &'a HashMap<taffy::NodeId, NodeId>,
    is_subgrid: &'a dyn Fn(NodeId) -> bool,
    roots: &'a [taffy::NodeId],
    available: Size<AvailableSpace>,
    count: usize,
    axis: Axis,
}

impl Ctx<'_> {
    fn is_sub(&self, id: taffy::NodeId) -> bool {
        self.owner
            .get(&id)
            .is_some_and(|&node| (self.is_subgrid)(node))
    }

    fn style_of(&self, id: taffy::NodeId) -> Option<&ComputedStyle> {
        self.owner.get(&id).and_then(|node| self.styles.get(node))
    }
}

fn relayout(
    font: &mut florui_text::Font,
    tree: &mut TaffyTree<LeafContext>,
    ctx: &Ctx<'_>,
) -> Result<(), LayoutError> {
    for &root in ctx.roots {
        run_layout(font, tree, root, ctx.available, ctx.count)?;
    }
    Ok(())
}

fn probe_top(
    font: &mut florui_text::Font,
    tree: &mut TaffyTree<LeafContext>,
    ctx: &Ctx<'_>,
    top: taffy::NodeId,
) -> Result<(), LayoutError> {
    let axis = ctx.axis;
    let other = axis.other();

    // 1. Every subgrid under the top grid, with the lines it spans and the
    // lines it puts its items at. A subgrid needs a track for each line it
    // spans to place its items on, so it gets that many while this is read.
    let Some(top_info) = grid_info(tree, top) else {
        return Ok(());
    };
    if axis.tracks(top_info).negative_implicit_tracks != 0 {
        return Ok(());
    }
    let top_names: Vec<Vec<String>> = {
        let tracks = axis.tracks(top_info);
        (0..=tracks.explicit_tracks as usize)
            .map(|line| {
                tracks
                    .names_for_line(line)
                    .iter()
                    .map(|name| name.to_string())
                    .collect()
            })
            .collect()
    };
    let mut subs: Vec<Sub> = Vec::new();
    // The span a subgrid of the top grid takes in the other axis.
    let mut across: Vec<(u16, u16)> = Vec::new();
    let top_items = in_flow_children(tree, top)?;
    let top_placements: Vec<DetailedGridItemsInfo> = top_info.items.clone();
    for (child, placement) in top_items.iter().zip(&top_placements) {
        if ctx.is_sub(*child) {
            let lines = axis.lines(placement);
            let written = ctx
                .style_of(*child)
                .and_then(|style| axis.template(style).subgrid.as_deref());
            subs.push(Sub {
                id: *child,
                parent: None,
                in_parent: lines,
                top: lines,
                items: Vec::new(),
                edge: (0.0, 0.0),
                names: merge_names(
                    &top_names,
                    lines.0 as usize - 1,
                    (lines.1 - lines.0) as usize,
                    written,
                ),
            });
            across.push(other.lines(placement));
        }
    }
    if subs.is_empty() {
        return Ok(());
    }
    let mut original: Vec<(taffy::NodeId, taffy::Style)> = Vec::new();
    // The subgrids that had an item to hold in their last track.
    let mut held_subs: Vec<usize> = Vec::new();
    let mut known = 0;
    loop {
        let fresh: Vec<usize> = (known..subs.len()).collect();
        if fresh.is_empty() {
            break;
        }
        known = subs.len();
        for &index in &fresh {
            let id = subs[index].id;
            let style = tree.style(id).map_err(LayoutError)?.clone();
            original.push((id, style.clone()));
            let mut placeholder = style;
            let tracks = (subs[index].in_parent.1 - subs[index].in_parent.0) as usize;
            axis.set_template(
                &mut placeholder,
                (0..tracks)
                    .map(|_| GridTemplateComponent::Single(auto_track()))
                    .collect(),
                template_names(&subs[index].names),
            );
            tree.set_style(id, placeholder).map_err(LayoutError)?;
        }
        relayout(font, tree, ctx)?;
        for &index in &fresh {
            let id = subs[index].id;
            let Some(info) = grid_info(tree, id) else {
                continue;
            };
            let placements: Vec<DetailedGridItemsInfo> = info.items.clone();
            let children = in_flow_children(tree, id)?;
            let tracks = subs[index].in_parent.1 - subs[index].in_parent.0;
            for (child, placement) in children.iter().zip(&placements) {
                let lines = axis.lines(placement);
                let held = hold_lines(lines, tracks);
                if held != lines && !held_subs.contains(&index) {
                    held_subs.push(index);
                }
                subs[index].items.push((*child, held));
            }
            let nested: Vec<(taffy::NodeId, (u16, u16))> = subs[index]
                .items
                .iter()
                .filter(|(child, _)| ctx.is_sub(*child))
                .copied()
                .collect();
            let (parent_top, _) = subs[index].top;
            let across_of = across[index];
            for (child, lines) in nested {
                let written = ctx
                    .style_of(child)
                    .and_then(|style| axis.template(style).subgrid.as_deref());
                let names = merge_names(
                    &subs[index].names,
                    lines.0 as usize - 1,
                    (lines.1 - lines.0) as usize,
                    written,
                );
                subs.push(Sub {
                    id: child,
                    parent: Some(index),
                    in_parent: lines,
                    top: (parent_top + lines.0 - 1, parent_top + lines.1 - 1),
                    items: Vec::new(),
                    edge: (0.0, 0.0),
                    names,
                });
                across.push(across_of);
            }
        }
    }

    // 2. What each subgrid's edges take, from the styles as they were.
    let originals: HashMap<taffy::NodeId, &taffy::Style> =
        original.iter().map(|(id, style)| (*id, style)).collect();
    for index in 0..subs.len() {
        let own = axis.insets(originals[&subs[index].id]);
        let (mut start, mut end) = own;
        if let Some(parent) = subs[index].parent {
            let tracks_of_parent = subs[parent].in_parent.1 - subs[parent].in_parent.0;
            if subs[index].in_parent.0 == 1 {
                start += subs[parent].edge.0;
            }
            if subs[index].in_parent.1 == tracks_of_parent + 1 {
                end += subs[parent].edge.1;
            }
        }
        subs[index].edge = (start, end);
    }

    // 3. The probe: the items of the subgrids are items of the top grid.
    let top_style = tree.style(top).map_err(LayoutError)?.clone();
    let top_gap = axis.gap(&top_style);
    let top_children = tree.children(top).map_err(LayoutError)?;
    let mut saved_children: Vec<(taffy::NodeId, Vec<taffy::NodeId>)> =
        vec![(top, top_children.clone())];
    let mut leaves: Vec<taffy::NodeId> = Vec::new();
    let mut leaf_styles: Vec<(taffy::NodeId, taffy::Style)> = Vec::new();
    for (index, sub) in subs.iter().enumerate() {
        saved_children.push((sub.id, tree.children(sub.id).map_err(LayoutError)?));
        let sub_style = ctx.style_of(sub.id);
        let gap_difference = match sub_style {
            Some(style) if !axis.gap_is_normal(style) => {
                let own = axis.gap(originals[&sub.id]);
                (own - top_gap) / 2.0
            }
            _ => 0.0,
        };
        let tracks = sub.in_parent.1 - sub.in_parent.0;
        // The span of the top-level subgrid across the other axis.
        let top_level = {
            let mut at = index;
            while let Some(parent) = subs[at].parent {
                at = parent;
            }
            at
        };
        let span_across = across[top_level];
        for &(item, lines) in &sub.items {
            if ctx.is_sub(item) {
                continue;
            }
            let layout = *tree.layout(item).map_err(LayoutError)?;
            let mut style = tree.style(item).map_err(LayoutError)?.clone();
            leaf_styles.push((item, style.clone()));
            axis.set_placement(&mut style, sub.top.0 + lines.0 - 1, sub.top.0 + lines.1 - 1);
            other.set_placement(&mut style, span_across.0, span_across.1);
            // The border box it has, written as the style takes it.
            let border_box = other.size_of(&layout);
            let size = if style.box_sizing == taffy::BoxSizing::ContentBox {
                (border_box - other.padding_border(&style)).max(0.0)
            } else {
                border_box
            };
            other.set_size(&mut style, size);
            let start = if lines.0 == 1 {
                sub.edge.0
            } else {
                gap_difference
            };
            let end = if lines.1 == tracks + 1 {
                sub.edge.1
            } else {
                gap_difference
            };
            axis.add_margins(&mut style, start, end);
            tree.set_style(item, style).map_err(LayoutError)?;
            leaves.push(item);
        }
        let mut probe = originals[&sub.id].clone();
        if sub.parent.is_none() {
            // Where it is, so that its items, which now take cells of their own,
            // do not push it elsewhere.
            axis.set_placement(&mut probe, sub.top.0, sub.top.1);
            other.set_placement(&mut probe, span_across.0, span_across.1);
        }
        tree.set_style(sub.id, probe).map_err(LayoutError)?;
    }
    for sub in &subs {
        tree.set_children(sub.id, &[]).map_err(LayoutError)?;
    }
    let mut probe_children = top_children.clone();
    probe_children.extend(&leaves);
    tree.set_children(top, &probe_children)
        .map_err(LayoutError)?;
    relayout(font, tree, ctx)?;

    // 4. What the probe sized.
    let probed = grid_info(tree, top).map(|info| {
        let tracks = axis.tracks(info);
        let sizes = tracks.positions.iter().map(|p| p.end - p.start).collect();
        let spans = tracks.positions.iter().map(|p| (p.start, p.end)).collect();
        let explicit = tracks.explicit_tracks as usize;
        let names = (0..=explicit)
            .map(|line| {
                tracks
                    .names_for_line(line)
                    .iter()
                    .map(|name| name.to_string())
                    .collect()
            })
            .collect();
        Probed {
            sizes,
            spans,
            explicit,
            names,
        }
    });

    // 5. Back as it was, with the sizes in.
    for (id, children) in &saved_children {
        tree.set_children(*id, children).map_err(LayoutError)?;
    }
    for (id, style) in leaf_styles.into_iter().chain(original.iter().cloned()) {
        tree.set_style(id, style).map_err(LayoutError)?;
    }
    let Some(Probed {
        sizes,
        spans,
        explicit,
        names,
    }) = probed
    else {
        return relayout(font, tree, ctx);
    };
    let mut patched = top_style;
    axis.set_template(
        &mut patched,
        sizes[..explicit.min(sizes.len())]
            .iter()
            .map(|&size| GridTemplateComponent::Single(fixed(size)))
            .collect(),
        if names.iter().all(Vec::is_empty) {
            Vec::new()
        } else {
            names.clone()
        },
    );
    axis.set_implicit(
        &mut patched,
        sizes
            .get(explicit..)
            .unwrap_or_default()
            .iter()
            .map(|&size| fixed(size))
            .collect(),
    );
    tree.set_style(top, patched).map_err(LayoutError)?;

    for sub in &subs {
        let Some(own) = originals.get(&sub.id).copied() else {
            continue;
        };
        let mut style = own.clone();
        let tracks = (sub.top.1 - sub.top.0) as usize;
        let from = sub.top.0 as usize - 1;
        let mut lengths: Vec<f32> = (0..tracks)
            .map(|i| sizes.get(from + i).copied().unwrap_or(0.0))
            .collect();
        if let Some(first) = lengths.first_mut() {
            *first -= sub.edge.0;
        }
        if let Some(last) = lengths.last_mut() {
            *last -= sub.edge.1;
        }
        // The gutter between its tracks: the grid's gap, and what
        // `justify-content` or `align-content` adds where it spreads them.
        let gutter = if tracks > 1 {
            match (
                spans.get(from),
                spans.get(from + tracks - 1),
                sizes.get(from + 1..from + tracks - 1),
            ) {
                (Some(first), Some(last), Some(inner)) => {
                    ((last.0 - first.1 - inner.iter().sum::<f32>()) / (tracks - 1) as f32).max(0.0)
                }
                _ => top_gap,
            }
        } else {
            top_gap
        };
        let normal = ctx
            .style_of(sub.id)
            .is_none_or(|style| axis.gap_is_normal(style));
        if normal {
            axis.set_gap(&mut style, gutter);
        } else {
            let own_gap = axis.gap(own);
            let difference = gutter - own_gap;
            let last = tracks.saturating_sub(1);
            for (i, length) in lengths.iter_mut().enumerate() {
                let neighbours = usize::from(i > 0) + usize::from(i < last);
                *length += difference / 2.0 * neighbours as f32;
            }
        }
        axis.set_template(
            &mut style,
            lengths
                .iter()
                .map(|&length| GridTemplateComponent::Single(fixed(length)))
                .collect(),
            if sub.names.iter().all(Vec::is_empty) {
                Vec::new()
            } else {
                sub.names.clone()
            },
        );
        tree.set_style(sub.id, style).map_err(LayoutError)?;
    }
    // Where one item goes in the last track the others go where they went, or
    // they would take what is left of it.
    for index in held_subs {
        for &(item, lines) in &subs[index].items {
            let mut style = tree.style(item).map_err(LayoutError)?.clone();
            axis.set_placement(&mut style, lines.0, lines.1);
            tree.set_style(item, style).map_err(LayoutError)?;
        }
    }
    relayout(font, tree, ctx)
}
