//! The borders of a table whose borders collapse (`border-collapse: collapse`):
//! each edge between two cells, or between a cell and the table, is one border,
//! which of the borders written there wins by CSS 2.1 17.6.2.1, centered on the line
//! between the boxes.

use florui_style::{BorderLineStyle, ComputedStyle, Rgba};

use crate::table_structure::{CellSource, Structure};

pub(crate) const TOP: usize = 0;
pub(crate) const RIGHT: usize = 1;
pub(crate) const BOTTOM: usize = 2;
pub(crate) const LEFT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Line {
    /// No border, or `border-style: none`, which loses to any other.
    #[default]
    None,
    /// `border-style: hidden`, which wins over all others.
    Hidden,
    Style(BorderLineStyle),
}

/// A border of a box on one side, as its style says.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Edge {
    pub width: f32,
    pub line: Line,
    pub color: Rgba,
}

impl Default for Edge {
    fn default() -> Self {
        Edge {
            width: 0.0,
            line: Line::None,
            color: Rgba::TRANSPARENT,
        }
    }
}

/// The borders of a box: top, right, bottom, left.
pub type Sides = [Edge; 4];

/// The borders `style` writes.
pub fn sides_of(style: &ComputedStyle) -> Sides {
    let side = |border: florui_style::BorderSide, hidden: bool| Edge {
        width: border.width,
        line: if hidden {
            Line::Hidden
        } else if border.width > 0.0 {
            Line::Style(border.style)
        } else {
            Line::None
        },
        color: border.color,
    };
    [
        side(style.border.top, style.border_hidden.top),
        side(style.border.right, style.border_hidden.right),
        side(style.border.bottom, style.border_hidden.bottom),
        side(style.border.left, style.border_hidden.left),
    ]
}

/// What a border is part of: of two that are alike in all else, the one of a cell wins
/// over that of a row, of a row group, of a column, of a column group, of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Owner {
    Table,
    ColumnGroup,
    Column,
    RowGroup,
    Row,
    Cell,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    edge: Edge,
    owner: Owner,
    /// Of two alike, the one further to the left and then further to the top wins.
    place: (usize, usize),
}

fn line_rank(line: Line) -> u8 {
    match line {
        Line::Style(BorderLineStyle::Double) => 7,
        Line::Style(BorderLineStyle::Solid) => 6,
        Line::Style(BorderLineStyle::Dashed) => 5,
        Line::Style(BorderLineStyle::Dotted) => 4,
        Line::Style(BorderLineStyle::Ridge) => 3,
        Line::Style(BorderLineStyle::Outset) => 2,
        Line::Style(BorderLineStyle::Groove) => 1,
        Line::Style(BorderLineStyle::Inset) | Line::None | Line::Hidden => 0,
    }
}

/// The one border an edge has of those written on it.
fn resolve(candidates: &[Candidate]) -> Edge {
    if candidates.iter().any(|c| c.edge.line == Line::Hidden) {
        return Edge::default();
    }
    let mut best: Option<&Candidate> = None;
    for candidate in candidates
        .iter()
        .filter(|c| c.edge.line != Line::None && c.edge.width > 0.0)
    {
        let better = match best {
            None => true,
            Some(best) => {
                let key = |c: &Candidate| (c.edge.width, line_rank(c.edge.line), c.owner);
                let (a, b) = (key(candidate), key(best));
                if a.0 != b.0 {
                    a.0 > b.0
                } else if a.1 != b.1 {
                    a.1 > b.1
                } else if a.2 != b.2 {
                    a.2 > b.2
                } else {
                    candidate.place < best.place
                }
            }
        };
        if better {
            best = Some(candidate);
        }
    }
    best.map_or_else(Edge::default, |c| c.edge)
}

/// What each part of a table writes.
pub(crate) struct Borders {
    pub(crate) table: Sides,
    pub(crate) cells: Vec<Sides>,
    pub(crate) rows: Vec<Sides>,
    pub(crate) groups: Vec<Sides>,
    pub(crate) columns: Vec<Sides>,
    pub(crate) column_groups: Vec<Sides>,
}

/// The collapsed edges of a table.
pub(crate) struct Grid {
    /// `vertical[row][boundary]`: the edge on the boundary before column `boundary` along
    /// row `row`.
    pub(crate) vertical: Vec<Vec<Edge>>,
    /// `horizontal[boundary][column]`: the edge on the boundary before row `boundary`
    /// along column `column`.
    pub(crate) horizontal: Vec<Vec<Edge>>,
}

impl Grid {
    /// How wide the border of the table is on a side: half of the widest edge there.
    pub(crate) fn table_border(&self, side: usize) -> f32 {
        let widest = |edges: &[Edge]| edges.iter().map(|e| e.width).fold(0.0, f32::max);
        let half = match side {
            TOP => self.horizontal.first().map_or(0.0, |edges| widest(edges)),
            BOTTOM => self.horizontal.last().map_or(0.0, |edges| widest(edges)),
            LEFT => self
                .vertical
                .iter()
                .map(|edges| edges.first().map_or(0.0, |e| e.width))
                .fold(0.0, f32::max),
            _ => self
                .vertical
                .iter()
                .map(|edges| edges.last().map_or(0.0, |e| e.width))
                .fold(0.0, f32::max),
        };
        half / 2.0
    }

    /// How wide the border of a cell is on each side: half of the widest edge along it.
    pub(crate) fn cell_border<I>(&self, cell: &CellSource<I>) -> [f32; 4] {
        let rows = self.vertical.len();
        let top = cell.row.min(rows);
        let bottom = (cell.row + cell.rows).min(rows);
        let horizontal = |boundary: usize| {
            self.horizontal.get(boundary).map_or(0.0, |edges| {
                edges[cell.col.min(edges.len())..(cell.col + cell.cols).min(edges.len())]
                    .iter()
                    .map(|e| e.width)
                    .fold(0.0, f32::max)
            })
        };
        let vertical = |boundary: usize| {
            self.vertical[cell.row.min(rows)..(cell.row + cell.rows).min(rows)]
                .iter()
                .map(|edges| edges.get(boundary).map_or(0.0, |e| e.width))
                .fold(0.0, f32::max)
        };
        let mut sides = [0.0; 4];
        sides[TOP] = horizontal(top) / 2.0;
        sides[BOTTOM] = horizontal(bottom) / 2.0;
        sides[LEFT] = vertical(cell.col) / 2.0;
        sides[RIGHT] = vertical(cell.col + cell.cols) / 2.0;
        sides
    }
}

/// Where the cells of a structure are: the index of the cell at each place of the grid.
#[allow(clippy::needless_range_loop)]
fn cells_at<I>(structure: &Structure<I>) -> Vec<Vec<Option<usize>>> {
    let rows = structure.rows.len();
    let columns = structure.columns.len();
    let mut at = vec![vec![None; columns]; rows];
    for (index, cell) in structure.cells.iter().enumerate() {
        for row in cell.row..(cell.row + cell.rows).min(rows) {
            for col in cell.col..(cell.col + cell.cols).min(columns) {
                at[row][col] = Some(index);
            }
        }
    }
    at
}

/// The edges of the grid of `structure`, with the borders the parts of the table write.
pub(crate) fn resolve_grid<I>(structure: &Structure<I>, borders: &Borders) -> Grid {
    let rows = structure.rows.len();
    let columns = structure.columns.len();
    let at = cells_at(structure);
    let group_of_row = |row: usize| structure.rows[row].group;
    // The column group of a column, and whether the column opens or closes it.
    let column_group = |col: usize| {
        structure
            .column_groups
            .iter()
            .position(|g| g.first <= col && col < g.first + g.count)
    };

    let mut vertical = vec![vec![Edge::default(); columns + 1]; rows];
    for (row, edges) in vertical.iter_mut().enumerate() {
        for (boundary, slot) in edges.iter_mut().enumerate() {
            let before = boundary.checked_sub(1).and_then(|col| at[row][col]);
            let after = at[row].get(boundary).copied().flatten();
            if before.is_some() && before == after {
                // Inside a cell that spans columns.
                continue;
            }
            let mut found: Vec<Candidate> = Vec::new();
            if let Some(cell) = before {
                found.push(Candidate {
                    edge: borders.cells[cell][RIGHT],
                    owner: Owner::Cell,
                    place: (structure.cells[cell].col, row),
                });
            }
            if let Some(cell) = after {
                found.push(Candidate {
                    edge: borders.cells[cell][LEFT],
                    owner: Owner::Cell,
                    place: (structure.cells[cell].col, row),
                });
            }
            if boundary == 0 {
                found.push(Candidate {
                    edge: borders.rows[row][LEFT],
                    owner: Owner::Row,
                    place: (0, row),
                });
                if let Some(group) = group_of_row(row) {
                    found.push(Candidate {
                        edge: borders.groups[group][LEFT],
                        owner: Owner::RowGroup,
                        place: (0, row),
                    });
                }
                found.push(Candidate {
                    edge: borders.table[LEFT],
                    owner: Owner::Table,
                    place: (0, 0),
                });
            }
            if boundary == columns {
                found.push(Candidate {
                    edge: borders.rows[row][RIGHT],
                    owner: Owner::Row,
                    place: (columns, row),
                });
                if let Some(group) = group_of_row(row) {
                    found.push(Candidate {
                        edge: borders.groups[group][RIGHT],
                        owner: Owner::RowGroup,
                        place: (columns, row),
                    });
                }
                found.push(Candidate {
                    edge: borders.table[RIGHT],
                    owner: Owner::Table,
                    place: (columns, 0),
                });
            }
            if boundary > 0 {
                let col = boundary - 1;
                found.push(Candidate {
                    edge: borders.columns[col][RIGHT],
                    owner: Owner::Column,
                    place: (col, 0),
                });
                if let Some(group) = column_group(col)
                    && structure.column_groups[group].first + structure.column_groups[group].count
                        == boundary
                {
                    found.push(Candidate {
                        edge: borders.column_groups[group][RIGHT],
                        owner: Owner::ColumnGroup,
                        place: (col, 0),
                    });
                }
            }
            if boundary < columns {
                found.push(Candidate {
                    edge: borders.columns[boundary][LEFT],
                    owner: Owner::Column,
                    place: (boundary, 0),
                });
                if let Some(group) = column_group(boundary)
                    && structure.column_groups[group].first == boundary
                {
                    found.push(Candidate {
                        edge: borders.column_groups[group][LEFT],
                        owner: Owner::ColumnGroup,
                        place: (boundary, 0),
                    });
                }
            }
            *slot = resolve(&found);
        }
    }

    let mut horizontal = vec![vec![Edge::default(); columns]; rows + 1];
    for (boundary, edges) in horizontal.iter_mut().enumerate() {
        for (col, slot) in edges.iter_mut().enumerate() {
            let above = boundary.checked_sub(1).and_then(|row| at[row][col]);
            let below = at.get(boundary).and_then(|row| row[col]);
            if above.is_some() && above == below {
                // Inside a cell that spans rows.
                continue;
            }
            let mut found: Vec<Candidate> = Vec::new();
            if let Some(cell) = above {
                found.push(Candidate {
                    edge: borders.cells[cell][BOTTOM],
                    owner: Owner::Cell,
                    place: (col, structure.cells[cell].row),
                });
            }
            if let Some(cell) = below {
                found.push(Candidate {
                    edge: borders.cells[cell][TOP],
                    owner: Owner::Cell,
                    place: (col, structure.cells[cell].row),
                });
            }
            if boundary > 0 {
                let row = boundary - 1;
                found.push(Candidate {
                    edge: borders.rows[row][BOTTOM],
                    owner: Owner::Row,
                    place: (col, row),
                });
                if let Some(group) = group_of_row(row)
                    && structure.groups[group].end == boundary
                {
                    found.push(Candidate {
                        edge: borders.groups[group][BOTTOM],
                        owner: Owner::RowGroup,
                        place: (col, row),
                    });
                }
            }
            if boundary < rows {
                found.push(Candidate {
                    edge: borders.rows[boundary][TOP],
                    owner: Owner::Row,
                    place: (col, boundary),
                });
                if let Some(group) = group_of_row(boundary)
                    && structure.groups[group].first == boundary
                {
                    found.push(Candidate {
                        edge: borders.groups[group][TOP],
                        owner: Owner::RowGroup,
                        place: (col, boundary),
                    });
                }
            }
            if boundary == 0 {
                found.push(Candidate {
                    edge: borders.columns[col][TOP],
                    owner: Owner::Column,
                    place: (col, 0),
                });
                if let Some(group) = column_group(col) {
                    found.push(Candidate {
                        edge: borders.column_groups[group][TOP],
                        owner: Owner::ColumnGroup,
                        place: (col, 0),
                    });
                }
                found.push(Candidate {
                    edge: borders.table[TOP],
                    owner: Owner::Table,
                    place: (0, 0),
                });
            }
            if boundary == rows {
                found.push(Candidate {
                    edge: borders.columns[col][BOTTOM],
                    owner: Owner::Column,
                    place: (col, rows),
                });
                if let Some(group) = column_group(col) {
                    found.push(Candidate {
                        edge: borders.column_groups[group][BOTTOM],
                        owner: Owner::ColumnGroup,
                        place: (col, rows),
                    });
                }
                found.push(Candidate {
                    edge: borders.table[BOTTOM],
                    owner: Owner::Table,
                    place: (0, rows),
                });
            }
            *slot = resolve(&found);
        }
    }
    Grid {
        vertical,
        horizontal,
    }
}

/// One stretch of a collapsed border: a line of a width at the middle of the
/// boundary between boxes, in the pixels the layouts are in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSegment {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub horizontal: bool,
    pub width: f32,
    pub color: Rgba,
    pub style: BorderLineStyle,
}

/// What the boxes of a table in the arena are.
struct ArenaSource<'a> {
    arena: &'a florui_style::Tree,
    styles: &'a florui_style::StyleMap,
}

impl crate::table_structure::TableSource for ArenaSource<'_> {
    type Id = florui_style::NodeId;

    fn role(&self, id: Self::Id) -> crate::table::Role {
        crate::table::role_of(self.arena, self.styles, id)
    }

    fn children(&self, id: Self::Id) -> Vec<Self::Id> {
        crate::flow_children(self.arena, self.styles, id).to_vec()
    }

    fn is_hidden(&self, id: Self::Id) -> bool {
        self.styles
            .get(&id)
            .is_some_and(|s| s.display == florui_style::Display::None)
    }
}

/// The collapsed borders of the table `table`, wherever its cells ended up
/// (`layouts`, in the pixels they are in), as the lines to draw. Empty when its
/// borders do not collapse.
#[allow(clippy::needless_range_loop)]
pub fn collapsed_border_segments(
    arena: &florui_style::Tree,
    styles: &florui_style::StyleMap,
    layouts: &std::collections::HashMap<florui_style::NodeId, crate::BoxLayout>,
    table: florui_style::NodeId,
) -> Vec<BorderSegment> {
    use crate::table::Role;
    use crate::table_structure::{Content, structure};
    let source = ArenaSource { arena, styles };
    let Role::Table(role) = crate::table::role_of(arena, styles, table) else {
        return Vec::new();
    };
    if !role.collapse {
        return Vec::new();
    }
    let shape = structure(&source, table, false);
    if shape.rows.is_empty() || shape.columns.is_empty() {
        return Vec::new();
    }
    let none = [Edge::default(); 4];
    let sides = |node: Option<florui_style::NodeId>| -> Sides {
        use crate::table_structure::TableSource;
        match node.map(|node| source.role(node)) {
            Some(Role::Cell(role)) => role.sides,
            Some(Role::Row { sides, .. })
            | Some(Role::RowGroup { sides, .. })
            | Some(Role::Column { sides, .. })
            | Some(Role::ColumnGroup { sides, .. }) => sides,
            _ => none,
        }
    };
    let borders = Borders {
        table: role.sides,
        cells: shape
            .cells
            .iter()
            .map(|cell| match cell.content {
                Content::Cell(node) => sides(Some(node)),
                _ => none,
            })
            .collect(),
        rows: shape.rows.iter().map(|row| sides(row.node)).collect(),
        groups: shape.groups.iter().map(|g| sides(Some(g.node))).collect(),
        columns: shape.columns.iter().map(|c| sides(c.node)).collect(),
        column_groups: shape
            .column_groups
            .iter()
            .map(|g| sides(Some(g.node)))
            .collect(),
    };
    let grid = resolve_grid(&shape, &borders);

    // Where the boundaries between columns and between rows are, from the cells.
    let rows = shape.rows.len();
    let columns = shape.columns.len();
    let mut xs: Vec<Option<f32>> = vec![None; columns + 1];
    let mut ys: Vec<Option<f32>> = vec![None; rows + 1];
    for cell in &shape.cells {
        let Content::Cell(node) = cell.content else {
            continue;
        };
        let Some(layout) = layouts.get(&node) else {
            continue;
        };
        let (x, y) = crate::absolute_position(arena, layouts, node);
        xs[cell.col].get_or_insert(x);
        xs[(cell.col + cell.cols).min(columns)].get_or_insert(x + layout.width);
        ys[cell.row].get_or_insert(y);
        ys[(cell.row + cell.rows).min(rows)].get_or_insert(y + layout.height);
    }
    let fill = |values: Vec<Option<f32>>| -> Option<Vec<f32>> {
        let mut out = Vec::with_capacity(values.len());
        let mut last = values.iter().flatten().next().copied()?;
        for value in values {
            last = value.unwrap_or(last);
            out.push(last);
        }
        Some(out)
    };
    let (Some(xs), Some(ys)) = (fill(xs), fill(ys)) else {
        return Vec::new();
    };

    let mut segments = Vec::new();
    let line_of = |edge: Edge| match edge.line {
        Line::Style(style) => Some(style),
        _ => None,
    };
    // The widest horizontal and vertical edges that meet at a junction: the wider
    // takes the square they have in common, both when they are as wide.
    let junction = |row: usize, boundary: usize| -> (f32, f32) {
        let width = |edge: Option<&Edge>| edge.map_or(0.0, |e| e.width);
        let horizontal = width(
            boundary
                .checked_sub(1)
                .and_then(|c| grid.horizontal[row].get(c)),
        )
        .max(width(grid.horizontal[row].get(boundary)));
        let vertical = width(
            row.checked_sub(1)
                .and_then(|r| grid.vertical[r].get(boundary)),
        )
        .max(width(
            grid.vertical.get(row).and_then(|edges| edges.get(boundary)),
        ));
        (horizontal, vertical)
    };
    // Horizontal edges first, which a vertical edge as wide draws over.
    for boundary in 0..=rows {
        let mut col = 0;
        while col < columns {
            let edge = grid.horizontal[boundary][col];
            let end = col + 1;
            if let Some(style) = line_of(edge) {
                let reach = |at: usize| {
                    let (h, v) = junction(boundary, at);
                    if h >= v { v / 2.0 } else { 0.0 }
                };
                segments.push(BorderSegment {
                    x0: xs[col] - reach(col),
                    y0: ys[boundary],
                    x1: xs[end] + reach(end),
                    y1: ys[boundary],
                    horizontal: true,
                    width: edge.width,
                    color: edge.color,
                    style,
                });
            }
            col = end;
        }
    }
    // Vertical edges, a run of rows with the same edge as one line.
    for boundary in 0..=columns {
        let mut row = 0;
        while row < rows {
            let edge = grid.vertical[row][boundary];
            let end = row + 1;
            if let Some(style) = line_of(edge) {
                let reach = |at: usize| {
                    let (h, v) = junction(at, boundary);
                    if v >= h { h / 2.0 } else { 0.0 }
                };
                segments.push(BorderSegment {
                    x0: xs[boundary],
                    y0: ys[row] - reach(row),
                    x1: xs[boundary],
                    y1: ys[end] + reach(end),
                    horizontal: false,
                    width: edge.width,
                    color: edge.color,
                    style,
                });
            }
            row = end;
        }
    }
    // The wider over the narrower.
    segments.sort_by(|a, b| a.width.total_cmp(&b.width));
    segments
}
