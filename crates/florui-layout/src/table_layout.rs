//! The layout of a table: which cells fill which rows and columns, how wide each
//! column comes out and how tall each row, and where every part of the table
//! ends up. Chrome 153 is the reference, which follows CSS 2.1 chapter 17 with
//! the choices the specification leaves open.
//!
//! The cells and the captions are laid out by whichever algorithm they have as
//! roots of a context of their own. A child of a table that is not a part of
//! one is laid out in a cell made around it, which is not a node of the tree
//! for longer than the layout takes.

use taffy::util::MaybeMath;
use taffy::{
    AvailableSpace, Baselines, CompactLength, Layout, LayoutInput, LayoutOutput, LengthPercentage,
    Line, NodeId, Point, Rect, RequestedAxis, RunMode, Size, SizingMode, Style,
};

use crate::blockflow::{Own, layout_of, margins_of, or_zero, own_of};
use crate::table::{CellAlign, GroupKind, Role, TableRole};
use crate::tree::Run;
use crate::{LeafContext, TextContext};

/// What Chrome keeps lengths in, a 64th of a pixel.
const UNIT: f32 = 64.0;

fn floor_unit(value: f32) -> f32 {
    (value * UNIT).floor() / UNIT
}

/// A cell of the grid: where it is and what it came to.
struct Cell {
    /// The node laid out: the cell, or the box made around children of the
    /// table that are not parts of it.
    id: NodeId,
    /// Those children, when `id` is such a box.
    wrapped: Vec<NodeId>,
    /// `id` is a box made for the layout alone.
    temporary: bool,
    row: usize,
    col: usize,
    rows: usize,
    cols: usize,
    align: CellAlign,
    /// What it asks of its columns: the least and the most it needs and what it
    /// states (border boxes).
    min: f32,
    max: f32,
    fixed: Option<f32>,
    percent: Option<f32>,
    /// The least height it states.
    least_height: f32,
    /// As laid out at the width of its columns.
    height: f32,
    baseline: Option<f32>,
}

struct Column {
    node: Option<NodeId>,
    collapsed: bool,
    fixed: Option<f32>,
    percent: Option<f32>,
}

struct ColumnGroup {
    node: NodeId,
    first: usize,
    count: usize,
}

struct Row {
    node: Option<NodeId>,
    group: Option<usize>,
    cells: Vec<usize>,
    collapsed: bool,
    least_height: f32,
}

struct Group {
    node: NodeId,
    first: usize,
    end: usize,
}

#[derive(Default)]
struct Model {
    /// Where the grid is in the box of the table when captions take up some of it.
    grid: Option<(f32, f32)>,
    captions: Vec<(NodeId, bool)>,
    hidden: Vec<NodeId>,
    columns: Vec<Column>,
    column_groups: Vec<ColumnGroup>,
    rows: Vec<Row>,
    groups: Vec<Group>,
    cells: Vec<Cell>,
}

/// A row of a section as the children of the table gave it.
struct RowSource {
    node: Option<NodeId>,
    collapsed: bool,
    /// Real cells, or children that are not cells to wrap in one.
    cells: Vec<Piece>,
}

enum Piece {
    Cell(NodeId),
    Wrap(Vec<NodeId>),
    /// The text of the table itself, which a cell is made around.
    Own(LeafContext),
}

struct Section {
    kind: GroupKind,
    node: Option<NodeId>,
    collapsed: bool,
    rows: Vec<RowSource>,
}

/// What a column asks of the table.
#[derive(Clone, Copy, Default)]
struct Constraint {
    min: f32,
    max: f32,
    fixed: Option<f32>,
    percent: Option<f32>,
}

/// `extra` shared out by `weights` (equally when they are all nothing), added to
/// `values` of the entries `take` says.
fn share(values: &mut [f32], weights: &[f32], extra: f32, take: &[bool]) {
    if extra <= 0.0 {
        return;
    }
    let total: f32 = weights
        .iter()
        .zip(take)
        .filter(|&(_, &t)| t)
        .map(|(w, _)| *w)
        .sum();
    let count = take.iter().filter(|&&t| t).count();
    if count == 0 {
        return;
    }
    for (index, value) in values.iter_mut().enumerate() {
        if !take[index] {
            continue;
        }
        *value += if total > 0.0 {
            extra * weights[index] / total
        } else {
            extra / count as f32
        };
    }
}

impl Run<'_> {
    fn role_of(&self, id: NodeId) -> Role {
        self.tree.node(id).role
    }

    fn is_hidden_display(&self, id: NodeId) -> bool {
        self.tree.node(id).style.display == taffy::Display::None
    }

    /// The cells of a row, from the children of a box: the cells themselves,
    /// and a cell made around each run of children that are not cells.
    fn pieces_of(&mut self, children: &[NodeId], hidden: &mut Vec<NodeId>) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut run: Vec<NodeId> = Vec::new();
        for &child in children {
            match self.role_of(child) {
                Role::Cell(_) => {
                    if !run.is_empty() {
                        pieces.push(Piece::Wrap(std::mem::take(&mut run)));
                    }
                    pieces.push(Piece::Cell(child));
                }
                _ if self.is_hidden_display(child) => hidden.push(child),
                _ => run.push(child),
            }
        }
        if !run.is_empty() {
            pieces.push(Piece::Wrap(run));
        }
        pieces
    }

    /// The rows of a section, from its children: the rows themselves, and a row
    /// made around each run of the others.
    fn rows_of(&mut self, children: &[NodeId], hidden: &mut Vec<NodeId>) -> Vec<RowSource> {
        let mut rows = Vec::new();
        let mut run: Vec<NodeId> = Vec::new();
        for &child in children {
            match self.role_of(child) {
                Role::Row { collapsed } => {
                    self.close_row(&mut rows, &mut run, hidden);
                    let inner = self.tree.node(child).children.clone();
                    let cells = self.pieces_of(&inner, hidden);
                    rows.push(RowSource {
                        node: Some(child),
                        collapsed,
                        cells,
                    });
                }
                _ if self.is_hidden_display(child) => hidden.push(child),
                _ => run.push(child),
            }
        }
        self.close_row(&mut rows, &mut run, hidden);
        rows
    }

    fn close_row(
        &mut self,
        rows: &mut Vec<RowSource>,
        run: &mut Vec<NodeId>,
        hidden: &mut Vec<NodeId>,
    ) {
        if run.is_empty() {
            return;
        }
        let children = std::mem::take(run);
        let cells = self.pieces_of(&children, hidden);
        rows.push(RowSource {
            node: None,
            collapsed: false,
            cells,
        });
    }

    /// The structure of the table `id`: sections (header first, footer last),
    /// the grid of its cells, its columns and its captions.
    fn gather(&mut self, id: NodeId) -> Model {
        let children = self.tree.node(id).children.clone();
        let mut model = Model::default();
        let mut sections: Vec<Section> = Vec::new();
        // A table with text and nothing else has a cell around its text.
        if children.is_empty() {
            let own = match &self.tree.node(id).context {
                Some(LeafContext::Text(text)) => Some(LeafContext::Text(TextContext {
                    text: text.text.clone(),
                    style: text.style,
                })),
                Some(LeafContext::Inline(context)) => Some(LeafContext::Inline(context.clone())),
                _ => None,
            };
            if let Some(context) = own {
                sections.push(Section {
                    kind: GroupKind::Body,
                    node: None,
                    collapsed: false,
                    rows: vec![RowSource {
                        node: None,
                        collapsed: false,
                        cells: vec![Piece::Own(context)],
                    }],
                });
            }
        }
        // Rows and other boxes between groups make a group of their own.
        let mut loose: Vec<NodeId> = Vec::new();
        let mut header_seen = false;
        let mut footer_seen = false;
        for &child in &children {
            match self.role_of(child) {
                Role::Caption { bottom } => model.captions.push((child, bottom)),
                Role::ColumnGroup { span, collapsed } => {
                    let inner = self.tree.node(child).children.clone();
                    let first = model.columns.len();
                    let mut any = false;
                    for col in inner {
                        if let Role::Column {
                            span,
                            collapsed: own,
                        } = self.role_of(col)
                        {
                            any = true;
                            for _ in 0..span {
                                model.columns.push(Column {
                                    node: Some(col),
                                    collapsed: collapsed || own,
                                    fixed: None,
                                    percent: None,
                                });
                            }
                        } else if self.is_hidden_display(col) {
                            model.hidden.push(col);
                        }
                    }
                    if !any {
                        for _ in 0..span {
                            model.columns.push(Column {
                                node: Some(child),
                                collapsed,
                                fixed: None,
                                percent: None,
                            });
                        }
                    }
                    model.column_groups.push(ColumnGroup {
                        node: child,
                        first,
                        count: model.columns.len() - first,
                    });
                }
                Role::Column { span, collapsed } => {
                    for _ in 0..span {
                        model.columns.push(Column {
                            node: Some(child),
                            collapsed,
                            fixed: None,
                            percent: None,
                        });
                    }
                }
                Role::RowGroup { kind, collapsed } => {
                    self.close_loose(&mut sections, &mut loose, &mut model.hidden);
                    // Only the first header and the first footer are those.
                    let kind = match kind {
                        GroupKind::Header if !header_seen => {
                            header_seen = true;
                            GroupKind::Header
                        }
                        GroupKind::Footer if !footer_seen => {
                            footer_seen = true;
                            GroupKind::Footer
                        }
                        _ => GroupKind::Body,
                    };
                    let inner = self.tree.node(child).children.clone();
                    let rows = self.rows_of(&inner, &mut model.hidden);
                    sections.push(Section {
                        kind,
                        node: Some(child),
                        collapsed,
                        rows,
                    });
                }
                _ if self.is_hidden_display(child) => model.hidden.push(child),
                _ => loose.push(child),
            }
        }
        self.close_loose(&mut sections, &mut loose, &mut model.hidden);

        // Header first, footer last, the rest as they came.
        let mut ordered: Vec<Section> = Vec::with_capacity(sections.len());
        let mut footer: Option<Section> = None;
        let mut header: Option<Section> = None;
        for section in sections {
            match section.kind {
                GroupKind::Header => header = Some(section),
                GroupKind::Footer => footer = Some(section),
                GroupKind::Body => ordered.push(section),
            }
        }
        if let Some(header) = header {
            ordered.insert(0, header);
        }
        if let Some(footer) = footer {
            ordered.push(footer);
        }

        // Place the cells: the next free column of the row, spanning rows down
        // to the end of the section.
        let mut occupied: Vec<Vec<bool>> = Vec::new();
        let mut column_count = model.columns.len();
        for section in ordered {
            let first_row = model.rows.len();
            let section_rows = section.rows.len();
            let group = section.node.map(|_| model.groups.len());
            for (offset, row) in section.rows.into_iter().enumerate() {
                let row_index = model.rows.len();
                let mut row_cells = Vec::new();
                let mut col = 0usize;
                if occupied.len() <= row_index {
                    occupied.resize_with(row_index + 1, Vec::new);
                }
                for piece in row.cells {
                    while occupied[row_index].get(col).copied().unwrap_or(false) {
                        col += 1;
                    }
                    let mut temporary = false;
                    let (id, wrapped, colspan, rowspan, align) = match piece {
                        Piece::Cell(node) => {
                            let Role::Cell(role) = self.role_of(node) else {
                                continue;
                            };
                            (
                                node,
                                Vec::new(),
                                role.colspan.max(1) as usize,
                                role.rowspan as usize,
                                role.align,
                            )
                        }
                        Piece::Wrap(nodes) => {
                            let wrapper = self.tree.new_detached(
                                Style {
                                    display: taffy::Display::FlowRoot,
                                    ..Default::default()
                                },
                                &nodes,
                            );
                            temporary = true;
                            (wrapper, nodes, 1, 1, CellAlign::Top)
                        }
                        Piece::Own(context) => {
                            temporary = true;
                            let wrapper = self.tree.new_detached_leaf(
                                Style {
                                    display: taffy::Display::FlowRoot,
                                    ..Default::default()
                                },
                                context,
                            );
                            (wrapper, Vec::new(), 1, 1, CellAlign::Top)
                        }
                    };
                    let left = section_rows - offset;
                    let rowspan = if rowspan == 0 {
                        left
                    } else {
                        rowspan.min(left)
                    };
                    for r in row_index..row_index + rowspan {
                        if occupied.len() <= r {
                            occupied.resize_with(r + 1, Vec::new);
                        }
                        if occupied[r].len() < col + colspan {
                            occupied[r].resize(col + colspan, false);
                        }
                        for slot in &mut occupied[r][col..col + colspan] {
                            *slot = true;
                        }
                    }
                    column_count = column_count.max(col + colspan);
                    row_cells.push(model.cells.len());
                    model.cells.push(Cell {
                        id,
                        wrapped,
                        temporary,
                        row: row_index,
                        col,
                        rows: rowspan,
                        cols: colspan,
                        align,
                        min: 0.0,
                        max: 0.0,
                        fixed: None,
                        percent: None,
                        least_height: 0.0,
                        height: 0.0,
                        baseline: None,
                    });
                    col += colspan;
                }
                let least_height = row.node.map_or(0.0, |node| {
                    let height = self.tree.node(node).style.size.height;
                    if height.tag() == CompactLength::LENGTH_TAG {
                        height.value()
                    } else {
                        0.0
                    }
                });
                model.rows.push(Row {
                    node: row.node,
                    group,
                    cells: row_cells,
                    collapsed: row.collapsed || section.collapsed,
                    least_height,
                });
            }
            if let Some(node) = section.node {
                model.groups.push(Group {
                    node,
                    first: first_row,
                    end: model.rows.len(),
                });
            }
        }
        while model.columns.len() < column_count {
            model.columns.push(Column {
                node: None,
                collapsed: false,
                fixed: None,
                percent: None,
            });
        }
        // What the columns state.
        for column in &mut model.columns {
            if let Some(node) = column.node {
                let size = self.tree.node(node).style.size.width;
                match size.tag() {
                    CompactLength::LENGTH_TAG => column.fixed = Some(size.value()),
                    CompactLength::PERCENT_TAG => column.percent = Some(size.value()),
                    _ => {}
                }
            }
        }
        model
    }

    fn close_loose(
        &mut self,
        sections: &mut Vec<Section>,
        loose: &mut Vec<NodeId>,
        hidden: &mut Vec<NodeId>,
    ) {
        if loose.is_empty() {
            return;
        }
        let children = std::mem::take(loose);
        let rows = self.rows_of(&children, hidden);
        sections.push(Section {
            kind: GroupKind::Body,
            node: None,
            collapsed: false,
            rows,
        });
    }

    /// What a cell asks of a column.
    fn cell_width(&mut self, id: NodeId, space: AvailableSpace) -> f32 {
        let inputs = LayoutInput {
            run_mode: RunMode::ComputeSize,
            sizing_mode: SizingMode::ContentSize,
            axis: RequestedAxis::Horizontal,
            known_dimensions: Size::NONE,
            known_dimensions_are_definite: Size {
                width: false,
                height: false,
            },
            parent_size: Size::NONE,
            available_space: Size {
                width: space,
                height: AvailableSpace::MaxContent,
            },
            vertical_margins_are_collapsible: Line::FALSE,
        };
        self.child_layout(id, inputs, None).size.width
    }

    /// Lays out `id` at `width`, and at `height` when that is known.
    fn cell_layout(&mut self, id: NodeId, width: f32, height: Option<f32>) -> LayoutOutput {
        let inputs = LayoutInput {
            run_mode: RunMode::PerformLayout,
            sizing_mode: SizingMode::ContentSize,
            axis: RequestedAxis::Both,
            known_dimensions: Size {
                width: Some(width),
                height,
            },
            known_dimensions_are_definite: Size {
                width: true,
                height: height.is_some(),
            },
            parent_size: Size {
                width: Some(width),
                height: None,
            },
            available_space: Size {
                width: AvailableSpace::Definite(width),
                height: height.map_or(AvailableSpace::MaxContent, AvailableSpace::Definite),
            },
            vertical_margins_are_collapsible: Line::FALSE,
        };
        self.child_layout(id, inputs, None)
    }

    /// Lays out the caption `id` at `width`: a block, whose height is the one it states
    /// or its content's.
    fn caption_layout(&mut self, id: NodeId, width: f32) -> LayoutOutput {
        let inputs = LayoutInput {
            run_mode: RunMode::PerformLayout,
            sizing_mode: SizingMode::InherentSize,
            axis: RequestedAxis::Both,
            known_dimensions: Size {
                width: Some(width),
                height: None,
            },
            known_dimensions_are_definite: Size {
                width: true,
                height: false,
            },
            parent_size: Size {
                width: Some(width),
                height: None,
            },
            available_space: Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::MaxContent,
            },
            vertical_margins_are_collapsible: Line::FALSE,
        };
        self.child_layout(id, inputs, None)
    }

    /// Lays out `id` with `before` and `after` more padding above and below its
    /// content than it states: what puts the content of a cell where its
    /// alignment says.
    fn cell_layout_padded(
        &mut self,
        id: NodeId,
        basis: f32,
        (before, after): (f32, f32),
        width: f32,
        height: f32,
    ) -> LayoutOutput {
        let original = self.tree.node(id).style.clone();
        let own = own_of(
            &original,
            Size {
                width: Some(basis),
                height: None,
            },
        );
        let mut padded = original.clone();
        padded.padding.top = LengthPercentage::length(own.padding.top + before);
        padded.padding.bottom = LengthPercentage::length(own.padding.bottom + after);
        self.tree.live_mut(id).style = padded;
        self.tree.live_mut(id).cache.clear();
        let out = self.cell_layout(id, width, Some(height));
        let node = self.tree.live_mut(id);
        node.style = original;
        node.cache.clear();
        out
    }

    pub(crate) fn compute_table(&mut self, id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let Role::Table(role) = self.role_of(id) else {
            unreachable!("only a table is laid out as one");
        };
        let style = self.tree.node(id).style.clone();
        let own = own_of(&style, inputs.parent_size);
        let mut model = self.gather(id);
        let output = self.lay_out_table(inputs, role, &own, &mut model);
        if inputs.run_mode == RunMode::PerformLayout {
            self.tree.live_mut(id).grid = model.grid;
        }
        // The boxes made around children are not part of the tree.
        for cell in &model.cells {
            if cell.temporary {
                self.tree.discard(cell.id);
            }
        }
        if inputs.run_mode == RunMode::PerformLayout {
            for (order, &hidden) in model.hidden.iter().enumerate() {
                self.hide(hidden, order as u32);
            }
        }
        output
    }

    /// What each cell asks of its columns, then what the columns ask.
    fn column_constraints(&mut self, model: &mut Model, spacing_h: f32) -> Vec<Constraint> {
        let ids: Vec<NodeId> = model.cells.iter().map(|cell| cell.id).collect();
        for (index, cell_id) in ids.into_iter().enumerate() {
            let min = self.cell_width(cell_id, AvailableSpace::MinContent);
            let max = self
                .cell_width(cell_id, AvailableSpace::MaxContent)
                .max(min);
            let cell_style = self.tree.node(cell_id).style.clone();
            let own = own_of(&cell_style, Size::NONE);
            let content_box = cell_style.box_sizing == taffy::BoxSizing::ContentBox;
            let (adjust_w, adjust_h) = if content_box {
                (own.pb.sum_axes().width, own.pb.sum_axes().height)
            } else {
                (0.0, 0.0)
            };
            let cell = &mut model.cells[index];
            cell.min = min;
            cell.max = max;
            match cell_style.size.width.tag() {
                CompactLength::LENGTH_TAG => {
                    cell.fixed = Some(cell_style.size.width.value() + adjust_w);
                }
                CompactLength::PERCENT_TAG => cell.percent = Some(cell_style.size.width.value()),
                _ => {}
            }
            if cell_style.size.height.tag() == CompactLength::LENGTH_TAG {
                cell.least_height = cell_style.size.height.value() + adjust_h;
            }
        }

        let mut cols: Vec<Constraint> = model
            .columns
            .iter()
            .map(|column| Constraint {
                min: 0.0,
                max: column.fixed.unwrap_or(0.0),
                fixed: column.fixed,
                percent: column.percent,
            })
            .collect();
        for cell in model.cells.iter().filter(|cell| cell.cols == 1) {
            let col = &mut cols[cell.col];
            col.min = col.min.max(cell.min);
            let wants = cell.fixed.map_or(cell.max, |fixed| fixed.max(cell.min));
            col.max = col.max.max(wants);
            if let Some(fixed) = cell.fixed {
                col.fixed = Some(col.fixed.unwrap_or(0.0).max(fixed.max(cell.min)));
            }
            if let Some(percent) = cell.percent {
                col.percent = Some(col.percent.unwrap_or(0.0).max(percent));
            }
        }
        for col in &mut cols {
            if let Some(fixed) = col.fixed {
                col.max = fixed.max(col.min);
            }
            col.max = col.max.max(col.min);
        }
        for (col, column) in cols.iter_mut().zip(&model.columns) {
            if column.collapsed {
                *col = Constraint::default();
            }
        }
        // A cell across columns asks what its columns do not have between them.
        let mut spanning: Vec<&Cell> = model.cells.iter().filter(|cell| cell.cols > 1).collect();
        spanning.sort_by_key(|cell| (cell.cols, cell.col));
        for cell in spanning {
            let range = cell.col..cell.col + cell.cols;
            let inner = spacing_h * (cell.cols - 1) as f32;
            let take: Vec<bool> = model.columns[range.clone()]
                .iter()
                .map(|column| !column.collapsed)
                .collect();
            let need_min = cell.min - inner;
            let have_min: f32 = cols[range.clone()].iter().map(|c| c.min).sum();
            let weights: Vec<f32> = cols[range.clone()].iter().map(|c| c.max).collect();
            let mut mins: Vec<f32> = cols[range.clone()].iter().map(|c| c.min).collect();
            share(&mut mins, &weights, need_min - have_min, &take);
            for (col, min) in cols[range.clone()].iter_mut().zip(mins) {
                col.min = min;
                col.max = col.max.max(min);
            }
            let wanted = cell.fixed.map_or(cell.max, |fixed| fixed.max(cell.min)) - inner;
            let have_max: f32 = cols[range.clone()].iter().map(|c| c.max).sum();
            let weights: Vec<f32> = cols[range.clone()].iter().map(|c| c.max).collect();
            let mut maxes: Vec<f32> = cols[range.clone()].iter().map(|c| c.max).collect();
            share(&mut maxes, &weights, wanted - have_max, &take);
            for (col, max) in cols[range.clone()].iter_mut().zip(maxes) {
                col.max = max;
            }
            if let Some(percent) = cell.percent {
                let each = percent / cell.cols as f32;
                for col in &mut cols[range] {
                    col.percent = Some(col.percent.unwrap_or(0.0).max(each));
                }
            }
        }
        // The shares the columns state add up to the whole at the most: the later ones
        // get what the earlier left.
        let mut used = 0.0f32;
        for col in &mut cols {
            if let Some(percent) = col.percent {
                let percent = percent.min((1.0 - used).max(0.0));
                col.percent = Some(percent);
                used += percent;
            }
        }
        cols
    }

    /// What the columns need when nothing limits them: their widest, and wide
    /// enough that a column that states a share of the table has it.
    fn grid_max(cols: &[Constraint]) -> f32 {
        let plain: f32 = cols.iter().map(|c| c.max).sum();
        let percent_total: f32 = cols.iter().filter_map(|c| c.percent).sum();
        if percent_total <= 0.0 {
            return plain;
        }
        let mut widest = plain;
        let mut others = 0.0f32;
        for col in cols {
            match col.percent {
                Some(percent) if percent > 0.0 => widest = widest.max(col.max / percent),
                _ => others += col.max,
            }
        }
        if percent_total < 1.0 {
            widest = widest.max(others / (1.0 - percent_total));
        }
        widest
    }

    /// The widths of the columns of an auto table that has `target` to give them.
    fn distribute(cols: &[Constraint], target: f32) -> Vec<f32> {
        let count = cols.len();
        let min: Vec<f32> = cols.iter().map(|c| c.min).collect();
        let percent: Vec<f32> = cols
            .iter()
            .map(|c| c.percent.map_or(c.min, |p| (p * target).max(c.min)))
            .collect();
        let specified: Vec<f32> = cols
            .iter()
            .zip(&percent)
            .map(|(c, &p)| match (c.percent, c.fixed) {
                (Some(_), _) => p,
                (None, Some(fixed)) => fixed.max(c.min),
                _ => c.min,
            })
            .collect();
        let max: Vec<f32> = cols
            .iter()
            .zip(&percent)
            .map(|(c, &p)| match c.percent {
                Some(_) => p.max(c.min),
                None => c.max.max(c.min),
            })
            .collect();
        let sum = |values: &[f32]| values.iter().sum::<f32>();
        let mut widths: Vec<f32>;
        if target >= sum(&max) {
            widths = max.clone();
            let extra = target - sum(&max);
            let auto: Vec<bool> = cols
                .iter()
                .map(|c| c.percent.is_none() && c.fixed.is_none())
                .collect();
            let fixed: Vec<bool> = cols
                .iter()
                .map(|c| c.percent.is_none() && c.fixed.is_some())
                .collect();
            let by_percent: Vec<bool> = cols.iter().map(|c| c.percent.is_some()).collect();
            let weights: Vec<f32> = max.clone();
            let take = if auto.iter().any(|&a| a) {
                auto
            } else if fixed.iter().any(|&f| f) {
                fixed
            } else {
                by_percent
            };
            if take.iter().any(|&t| t) {
                share(&mut widths, &weights, extra, &take);
            } else {
                share(&mut widths, &weights, extra, &vec![true; count]);
            }
        } else {
            let levels = [&min, &percent, &specified, &max];
            widths = min.clone();
            if target > sum(&min) {
                // The first level that has room for the target, and the one
                // before it: the columns are somewhere between.
                let mut from = &min;
                for level in levels.iter().skip(1) {
                    if target <= sum(level) {
                        let (low, high) = (sum(from), sum(level));
                        let ratio = if high > low {
                            (target - low) / (high - low)
                        } else {
                            1.0
                        };
                        widths = from
                            .iter()
                            .zip(level.iter())
                            .map(|(a, b)| a + (b - a) * ratio)
                            .collect();
                        break;
                    }
                    from = level;
                }
            }
        }
        // Chrome works in 64ths of a pixel, the last column taking what is left.
        let total: f32 = widths.iter().sum();
        let mut snapped: Vec<f32> = widths.iter().map(|&w| floor_unit(w)).collect();
        if let Some(last) = snapped.len().checked_sub(1) {
            let rest: f32 = snapped[..last].iter().sum();
            snapped[last] = (total - rest).max(0.0);
        }
        snapped
    }

    /// The widths a fixed table states: its columns', else those of its first
    /// row's cells; the columns that state none share what is left.
    fn fixed_columns(model: &Model, target: Option<f32>) -> Vec<Option<f32>> {
        let mut stated: Vec<Option<f32>> = model
            .columns
            .iter()
            .map(|column| {
                column
                    .fixed
                    .or_else(|| column.percent.zip(target).map(|(p, t)| p * t))
            })
            .collect();
        for cell in model.cells.iter().filter(|cell| cell.row == 0) {
            let share = cell
                .fixed
                .or_else(|| cell.percent.zip(target).map(|(p, t)| p * t));
            let Some(width) = share else { continue };
            let each = (width / cell.cols as f32).max(0.0);
            for slot in &mut stated[cell.col..cell.col + cell.cols] {
                if slot.is_none() {
                    *slot = Some(each);
                }
            }
        }
        stated
    }

    /// The heights of the rows: what the cells in them need.
    fn row_heights(model: &Model, spacing_v: f32) -> Vec<f32> {
        let rows = model.rows.len();
        let mut heights = vec![0.0f32; rows];
        for (index, row) in model.rows.iter().enumerate() {
            let mut height = row.least_height;
            let mut above = 0.0f32;
            let mut below = 0.0f32;
            for &cell in &row.cells {
                let cell = &model.cells[cell];
                if cell.rows != 1 {
                    continue;
                }
                match (cell.align, cell.baseline) {
                    (CellAlign::Baseline, Some(baseline)) => {
                        above = above.max(baseline);
                        below = below.max(cell.height - baseline);
                    }
                    _ => height = height.max(cell.height),
                }
            }
            heights[index] = if row.collapsed {
                0.0
            } else {
                height.max(above + below)
            };
        }
        let mut spanning: Vec<&Cell> = model.cells.iter().filter(|cell| cell.rows > 1).collect();
        spanning.sort_by_key(|cell| (cell.rows, cell.row));
        for cell in spanning {
            let range = cell.row..(cell.row + cell.rows).min(rows);
            let take: Vec<bool> = range.clone().map(|r| !model.rows[r].collapsed).collect();
            let inner = spacing_v * (take.iter().filter(|&&t| t).count().saturating_sub(1)) as f32;
            let have: f32 = heights[range.clone()].iter().sum();
            let weights: Vec<f32> = heights[range.clone()].to_vec();
            let mut values = weights.clone();
            share(&mut values, &weights, cell.height - inner - have, &take);
            heights[range].copy_from_slice(&values);
        }
        heights
    }

    /// How far below its top each row's baseline is: the lowest of those of the
    /// cells on it.
    fn row_baselines(model: &Model) -> Vec<Option<f32>> {
        model
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|&cell| &model.cells[cell])
                    .filter(|cell| cell.rows == 1 && cell.align == CellAlign::Baseline)
                    .filter_map(|cell| cell.baseline)
                    .reduce(f32::max)
            })
            .collect()
    }

    fn lay_out_table(
        &mut self,
        inputs: LayoutInput,
        role: TableRole,
        own: &Own,
        model: &mut Model,
    ) -> LayoutOutput {
        let (spacing_h, spacing_v) = if role.collapse {
            (0.0, 0.0)
        } else {
            role.spacing
        };
        let columns = model.columns.len();
        let rows = model.rows.len();
        let pb = own.pb;
        let horizontal = pb.left + pb.right;
        let vertical = pb.top + pb.bottom;
        // A table with no cell has nothing to put the spacing around.
        let visible_columns = model.columns.iter().filter(|c| !c.collapsed).count();
        let visible_rows = model.rows.iter().filter(|r| !r.collapsed).count();
        let gridded = visible_columns > 0 && visible_rows > 0;
        let spacing_total_h = if gridded {
            spacing_h * (visible_columns + 1) as f32
        } else {
            0.0
        };
        let spacing_total_v = if gridded {
            spacing_v * (visible_rows + 1) as f32
        } else {
            0.0
        };
        let visible: Vec<bool> = model.columns.iter().map(|c| !c.collapsed).collect();

        let constraints = self.column_constraints(model, spacing_h);

        // Captions need a width of their own, the least they can do with.
        let mut caption_min = 0.0f32;
        let captions: Vec<NodeId> = model.captions.iter().map(|&(node, _)| node).collect();
        for &caption in &captions {
            let style = self.tree.node(caption).style.clone();
            let margins = or_zero(margins_of(&style, 0.0));
            let width = self.cell_width(caption, AvailableSpace::MinContent);
            caption_min = caption_min.max(width + margins.left + margins.right);
        }

        let sum_min: f32 = constraints.iter().map(|c| c.min).sum();
        let grid_max = Self::grid_max(&constraints);
        let min_width = (sum_min + spacing_total_h + horizontal).max(caption_min);
        let max_width = (grid_max + spacing_total_h + horizontal).max(caption_min);

        // The width of the table.
        let stated = inputs.known_dimensions.width.or(own.size.width);
        let fixed_layout = role.fixed && stated.is_some();
        let fixed_floor = if fixed_layout {
            let stated_columns = Self::fixed_columns(model, None);
            stated_columns.iter().flatten().sum::<f32>() + spacing_total_h + horizontal
        } else {
            min_width
        };
        let mut width = match stated {
            Some(stated) => stated.max(fixed_floor),
            None => match inputs.available_space.width {
                AvailableSpace::Definite(available) => max_width.min(min_width.max(available)),
                AvailableSpace::MinContent => min_width,
                AvailableSpace::MaxContent => max_width,
            },
        };
        if inputs.known_dimensions.width.is_none() {
            width = width
                .maybe_clamp(own.min.width, own.max.width)
                .max(fixed_floor);
        }
        if inputs.run_mode == RunMode::ComputeSize && inputs.axis == RequestedAxis::Horizontal {
            return LayoutOutput::from_outer_size(Size { width, height: 0.0 });
        }

        // The columns.
        let target = (width - horizontal - spacing_total_h).max(0.0);
        let widths = if fixed_layout {
            let stated_columns = Self::fixed_columns(model, Some(target));
            let set: f32 = stated_columns.iter().flatten().sum();
            let open = stated_columns.iter().filter(|w| w.is_none()).count();
            let mut widths: Vec<f32> = if open > 0 {
                let each = ((target - set) / open as f32).max(0.0);
                stated_columns.iter().map(|w| w.unwrap_or(each)).collect()
            } else {
                stated_columns.iter().map(|w| w.unwrap_or(0.0)).collect()
            };
            if open == 0 {
                let weights = widths.clone();
                share(&mut widths, &weights, target - set, &vec![true; columns]);
            }
            widths
        } else {
            Self::distribute(&constraints, target)
        };
        let widths: Vec<f32> = widths
            .into_iter()
            .zip(&visible)
            .map(|(w, v)| if *v { w } else { 0.0 })
            .collect();

        // The cells at the width of their columns, and what that makes the rows.
        let origin_x = pb.left + spacing_h;
        let mut column_x: Vec<f32> = Vec::with_capacity(columns);
        let mut at = origin_x;
        for (index, w) in widths.iter().enumerate() {
            column_x.push(at);
            if visible[index] {
                at += w + spacing_h;
            }
        }
        let span_width = |widths: &[f32], col: usize, cols: usize| -> f32 {
            let shown = visible[col..col + cols].iter().filter(|v| **v).count();
            widths[col..col + cols].iter().sum::<f32>() + spacing_h * shown.saturating_sub(1) as f32
        };
        for index in 0..model.cells.len() {
            let (cell_id, col, cols) = {
                let cell = &model.cells[index];
                (cell.id, cell.col, cell.cols)
            };
            let out = self.cell_layout(cell_id, span_width(&widths, col, cols), None);
            let cell = &mut model.cells[index];
            cell.height = out.size.height.max(cell.least_height);
            cell.baseline = out.baselines.first;
        }
        let mut heights = Self::row_heights(model, spacing_v);
        let natural_rows: f32 = heights.iter().sum();

        // Captions above and below.
        let mut top_captions = 0.0f32;
        let mut bottom_captions = 0.0f32;
        struct CaptionBox {
            id: NodeId,
            bottom: bool,
            outer: f32,
            margins: Rect<f32>,
            size: Size<f32>,
            reach: Size<f32>,
        }
        let mut caption_boxes: Vec<CaptionBox> = Vec::new();
        for &(caption, bottom) in &model.captions {
            let style = self.tree.node(caption).style.clone();
            let margins = or_zero(margins_of(&style, width));
            let inner = (width - margins.left - margins.right).max(0.0);
            let out = self.caption_layout(caption, inner);
            let outer = out.size.height + margins.top + margins.bottom;
            if bottom {
                bottom_captions += outer;
            } else {
                top_captions += outer;
            }
            caption_boxes.push(CaptionBox {
                id: caption,
                bottom,
                outer,
                margins,
                size: out.size,
                reach: Size {
                    width: out.scrollable_overflow_rect.right,
                    height: out.scrollable_overflow_rect.bottom,
                },
            });
        }

        if top_captions > 0.0 || bottom_captions > 0.0 {
            model.grid = Some((top_captions, 0.0));
        }
        // The height of the table, and the extra it gives the rows.
        let natural_height =
            vertical + spacing_total_v + natural_rows + top_captions + bottom_captions;
        let stated_height = inputs.known_dimensions.height.or(own.size.height);
        let height = stated_height
            .unwrap_or(natural_height)
            .maybe_clamp(own.min.height, own.max.height)
            .max(natural_height);
        if inputs.run_mode == RunMode::ComputeSize {
            return LayoutOutput::from_outer_size(Size { width, height });
        }
        if let Some(grid) = &mut model.grid {
            grid.1 = height - top_captions - bottom_captions;
        }
        if height > natural_height {
            let extra = height - natural_height;
            let weights = heights.clone();
            let take: Vec<bool> = model.rows.iter().map(|row| !row.collapsed).collect();
            share(&mut heights, &weights, extra, &take);
        }

        // Where the rows are.
        let grid_top = top_captions + pb.top + spacing_v;
        let mut row_y: Vec<f32> = Vec::with_capacity(rows);
        let mut y = grid_top;
        for (index, h) in heights.iter().enumerate() {
            if model.rows[index].collapsed {
                row_y.push(y - spacing_v);
            } else {
                row_y.push(y);
                y += h + spacing_v;
            }
        }
        let grid_width =
            widths.iter().sum::<f32>() + spacing_h * visible_columns.saturating_sub(1) as f32;
        let grid_bottom = match (row_y.last(), heights.last()) {
            (Some(&y), Some(&h)) => y + h,
            _ => grid_top,
        };

        // Each cell, laid out again to fill the rows it spans, its content
        // placed in them.
        let row_baselines = Self::row_baselines(model);
        for index in 0..model.cells.len() {
            let (cell_id, row, col, cell_rows, cols, align, natural, baseline) = {
                let cell = &model.cells[index];
                (
                    cell.id,
                    cell.row,
                    cell.col,
                    cell.rows,
                    cell.cols,
                    cell.align,
                    cell.height,
                    cell.baseline,
                )
            };
            let cell_width = span_width(&widths, col, cols);
            let last = (row + cell_rows).min(rows);
            let cell_height: f32 = heights[row..last].iter().sum::<f32>()
                + spacing_v * (last - row).saturating_sub(1) as f32;
            let extra = (cell_height - natural).max(0.0);
            let (before, after) = match align {
                CellAlign::Top => (0.0, extra),
                CellAlign::Middle => {
                    let before = floor_unit(extra / 2.0);
                    (before, extra - before)
                }
                CellAlign::Bottom => (extra, 0.0),
                CellAlign::Baseline => {
                    let shift = (row_baselines[row].unwrap_or(0.0) - baseline.unwrap_or(natural))
                        .clamp(0.0, extra);
                    (shift, extra - shift)
                }
            };
            self.tree.live_mut(cell_id).shift = before;
            let out = if before == 0.0 && after == 0.0 {
                self.cell_layout(cell_id, cell_width, Some(cell_height))
            } else {
                self.cell_layout_padded(cell_id, width, (before, after), cell_width, cell_height)
            };
            // Where the parent of the cell is, in the table.
            let parent = match model.rows[row].node {
                Some(_) => (origin_x, row_y[row]),
                None => match model.rows[row].group {
                    Some(group) => (origin_x, row_y[model.groups[group].first]),
                    None => (0.0, 0.0),
                },
            };
            let location = Point {
                x: column_x[col] - parent.0,
                y: row_y[row] - parent.1,
            };
            let cell_style = self.tree.node(cell_id).style.clone();
            let cell_own = own_of(
                &cell_style,
                Size {
                    width: Some(width),
                    height: None,
                },
            );
            let reach = Size {
                width: out.scrollable_overflow_rect.right,
                height: out.scrollable_overflow_rect.bottom,
            };
            let layout = layout_of(
                index as u32,
                location,
                Size {
                    width: cell_width,
                    height: cell_height,
                },
                reach,
                &cell_own,
                Rect::ZERO,
            );
            if !model.cells[index].temporary {
                self.set_layout(cell_id, layout);
            } else {
                // The children of a made box are where it is, in their parent.
                let wrapped = model.cells[index].wrapped.clone();
                for child in wrapped {
                    let mut child_layout = self.tree.node(child).layout;
                    child_layout.location.x += location.x;
                    child_layout.location.y += location.y;
                    self.set_layout(child, child_layout);
                }
            }
        }

        // The baseline of a table is that of its first row: where the cells that
        // sit on the baseline have it, else the bottom of the content of its first
        // cell.
        let first_baseline = match (rows, row_baselines.first()) {
            (0, _) => None,
            (_, Some(Some(baseline))) => Some(row_y[0] + baseline),
            _ => model.rows[0].cells.first().map(|&cell| {
                let style = self.tree.node(model.cells[cell].id).style.clone();
                let own = own_of(
                    &style,
                    Size {
                        width: Some(width),
                        height: None,
                    },
                );
                row_y[0] + heights[0] - own.pb.bottom
            }),
        };

        // Rows, groups, columns and captions.
        for (index, row) in model.rows.iter().enumerate() {
            let Some(node) = row.node else { continue };
            let parent_y = match row.group {
                Some(group) => row_y[model.groups[group].first],
                None => 0.0,
            };
            let size = Size {
                width: grid_width,
                height: heights[index],
            };
            let row_style = self.tree.node(node).style.clone();
            let layout = layout_of(
                index as u32,
                Point {
                    x: if row.group.is_some() { 0.0 } else { origin_x },
                    y: row_y[index] - parent_y,
                },
                size,
                size,
                &own_of(&row_style, Size::NONE),
                Rect::ZERO,
            );
            self.set_layout(node, layout);
        }
        for group in &model.groups {
            let top = row_y.get(group.first).copied().unwrap_or(grid_top);
            let bottom = if group.end > group.first {
                row_y[group.end - 1] + heights[group.end - 1]
            } else {
                top
            };
            let size = Size {
                width: grid_width,
                height: bottom - top,
            };
            let style = self.tree.node(group.node).style.clone();
            let layout = layout_of(
                group.first as u32,
                Point {
                    x: origin_x,
                    y: top,
                },
                size,
                size,
                &own_of(&style, Size::NONE),
                Rect::ZERO,
            );
            self.set_layout(group.node, layout);
        }
        let column_height = (grid_bottom - grid_top).max(0.0);
        let mut done: Vec<NodeId> = Vec::new();
        for (index, column) in model.columns.iter().enumerate() {
            let Some(node) = column.node else { continue };
            if done.contains(&node) {
                continue;
            }
            done.push(node);
            let count = model
                .columns
                .iter()
                .filter(|other| other.node == Some(node))
                .count();
            let style = self.tree.node(node).style.clone();
            let parent = self.tree.node(node).parent;
            let group = model
                .column_groups
                .iter()
                .find(|group| Some(group.node) == parent);
            let origin = group.map_or(0.0, |group| column_x[group.first]);
            let layout = layout_of(
                index as u32,
                Point {
                    x: column_x[index] - origin,
                    y: if group.is_some() { 0.0 } else { grid_top },
                },
                Size {
                    width: span_width(&widths, index, count.min(columns - index)),
                    height: column_height,
                },
                Size::ZERO,
                &own_of(&style, Size::NONE),
                Rect::ZERO,
            );
            self.set_layout(node, layout);
        }
        for group in &model.column_groups {
            if group.count == 0 {
                self.set_layout(group.node, Layout::with_order(0));
                continue;
            }
            let style = self.tree.node(group.node).style.clone();
            let layout = layout_of(
                group.first as u32,
                Point {
                    x: column_x[group.first],
                    y: grid_top,
                },
                Size {
                    width: span_width(&widths, group.first, group.count),
                    height: column_height,
                },
                Size::ZERO,
                &own_of(&style, Size::NONE),
                Rect::ZERO,
            );
            self.set_layout(group.node, layout);
        }
        let mut top_y = 0.0f32;
        let mut bottom_y = height - bottom_captions;
        for caption in caption_boxes {
            let style = self.tree.node(caption.id).style.clone();
            let y = if caption.bottom {
                let y = bottom_y + caption.margins.top;
                bottom_y += caption.outer;
                y
            } else {
                let y = top_y + caption.margins.top;
                top_y += caption.outer;
                y
            };
            let layout = layout_of(
                0,
                Point {
                    x: caption.margins.left,
                    y,
                },
                caption.size,
                caption.reach,
                &own_of(
                    &style,
                    Size {
                        width: Some(width),
                        height: None,
                    },
                ),
                caption.margins,
            );
            self.set_layout(caption.id, layout);
        }

        let mut output = LayoutOutput::from_outer_size(Size { width, height });
        output.baselines = Baselines::from_first(first_baseline);
        output.scrollable_overflow_rect = Rect {
            left: 0.0,
            top: 0.0,
            right: width,
            bottom: height,
        };
        output
    }
}
