//! The structure of a table: which rows and columns it has and which cell fills
//! what of the grid between them. It is worked out the same way for what layout
//! lays out and for what painting draws, over whatever holds the boxes
//! ([`TableSource`]).

use crate::table::{CellAlign, GroupKind, Role};

/// What the structure of a table is read from: the boxes of the table and what
/// each of them is in it.
pub(crate) trait TableSource {
    type Id: Copy + PartialEq;

    fn role(&self, id: Self::Id) -> Role;
    fn children(&self, id: Self::Id) -> Vec<Self::Id>;
    /// The box takes no part in the table: `display: none`.
    fn is_hidden(&self, id: Self::Id) -> bool;
}

/// What a cell is made of.
pub(crate) enum Content<I> {
    /// A cell of the table.
    Cell(I),
    /// Children of the table that are not cells, which a cell is made around.
    Wrap(Vec<I>),
    /// The text of the table itself, which a cell is made around.
    Own,
}

pub(crate) struct CellSource<I> {
    pub(crate) content: Content<I>,
    pub(crate) row: usize,
    pub(crate) col: usize,
    pub(crate) rows: usize,
    pub(crate) cols: usize,
    pub(crate) align: CellAlign,
}

pub(crate) struct ColumnSource<I> {
    pub(crate) node: Option<I>,
    pub(crate) collapsed: bool,
}

pub(crate) struct ColumnGroupSource<I> {
    pub(crate) node: I,
    pub(crate) first: usize,
    pub(crate) count: usize,
}

pub(crate) struct RowSource<I> {
    pub(crate) node: Option<I>,
    /// The index of its group among the groups of the table, if it has one.
    pub(crate) group: Option<usize>,
    /// Its cells, as indices of the cells of the table.
    pub(crate) cells: Vec<usize>,
    pub(crate) collapsed: bool,
}

pub(crate) struct GroupSource<I> {
    pub(crate) node: I,
    pub(crate) first: usize,
    pub(crate) end: usize,
}

pub(crate) struct Structure<I> {
    pub(crate) captions: Vec<(I, bool)>,
    pub(crate) hidden: Vec<I>,
    pub(crate) columns: Vec<ColumnSource<I>>,
    pub(crate) column_groups: Vec<ColumnGroupSource<I>>,
    pub(crate) rows: Vec<RowSource<I>>,
    pub(crate) groups: Vec<GroupSource<I>>,
    pub(crate) cells: Vec<CellSource<I>>,
}

/// A row of a section as the children of the table gave it.
struct RowIn<I> {
    node: Option<I>,
    collapsed: bool,
    pieces: Vec<Content<I>>,
}

struct Section<I> {
    kind: GroupKind,
    node: Option<I>,
    collapsed: bool,
    rows: Vec<RowIn<I>>,
}

/// The cells of a row, from the children of a box: the cells themselves, and a cell
/// made around each run of children that are not cells.
fn pieces_of<S: TableSource>(
    source: &S,
    children: &[S::Id],
    hidden: &mut Vec<S::Id>,
) -> Vec<Content<S::Id>> {
    let mut pieces = Vec::new();
    let mut run: Vec<S::Id> = Vec::new();
    for &child in children {
        match source.role(child) {
            Role::Cell(_) => {
                if !run.is_empty() {
                    pieces.push(Content::Wrap(std::mem::take(&mut run)));
                }
                pieces.push(Content::Cell(child));
            }
            _ if source.is_hidden(child) => hidden.push(child),
            _ => run.push(child),
        }
    }
    if !run.is_empty() {
        pieces.push(Content::Wrap(run));
    }
    pieces
}

/// The rows of a section, from its children: the rows themselves, and a row made
/// around each run of the others.
fn rows_of<S: TableSource>(
    source: &S,
    children: &[S::Id],
    hidden: &mut Vec<S::Id>,
) -> Vec<RowIn<S::Id>> {
    let mut rows: Vec<RowIn<S::Id>> = Vec::new();
    let mut run: Vec<S::Id> = Vec::new();
    let close = |source: &S,
                 rows: &mut Vec<RowIn<S::Id>>,
                 run: &mut Vec<S::Id>,
                 hidden: &mut Vec<S::Id>| {
        if run.is_empty() {
            return;
        }
        let children = std::mem::take(run);
        let pieces = pieces_of(source, &children, hidden);
        rows.push(RowIn {
            node: None,
            collapsed: false,
            pieces,
        });
    };
    for &child in children {
        match source.role(child) {
            Role::Row { collapsed, .. } => {
                close(source, &mut rows, &mut run, hidden);
                let inner = source.children(child);
                let pieces = pieces_of(source, &inner, hidden);
                rows.push(RowIn {
                    node: Some(child),
                    collapsed,
                    pieces,
                });
            }
            _ if source.is_hidden(child) => hidden.push(child),
            _ => run.push(child),
        }
    }
    close(source, &mut rows, &mut run, hidden);
    rows
}

/// The structure of the table `id`. `own_text` is whether the table has text and
/// nothing else, which a cell is made around.
pub(crate) fn structure<S: TableSource>(source: &S, id: S::Id, own_text: bool) -> Structure<S::Id> {
    let children = source.children(id);
    let mut out = Structure {
        captions: Vec::new(),
        hidden: Vec::new(),
        columns: Vec::new(),
        column_groups: Vec::new(),
        rows: Vec::new(),
        groups: Vec::new(),
        cells: Vec::new(),
    };
    let mut sections: Vec<Section<S::Id>> = Vec::new();
    if children.is_empty() && own_text {
        sections.push(Section {
            kind: GroupKind::Body,
            node: None,
            collapsed: false,
            rows: vec![RowIn {
                node: None,
                collapsed: false,
                pieces: vec![Content::Own],
            }],
        });
    }
    // Rows and other boxes between groups make a group of their own.
    let mut loose: Vec<S::Id> = Vec::new();
    let mut header_seen = false;
    let mut footer_seen = false;
    let close_loose = |source: &S,
                       sections: &mut Vec<Section<S::Id>>,
                       loose: &mut Vec<S::Id>,
                       hidden: &mut Vec<S::Id>| {
        if loose.is_empty() {
            return;
        }
        let children = std::mem::take(loose);
        let rows = rows_of(source, &children, hidden);
        sections.push(Section {
            kind: GroupKind::Body,
            node: None,
            collapsed: false,
            rows,
        });
    };
    for &child in &children {
        match source.role(child) {
            Role::Caption { bottom } => out.captions.push((child, bottom)),
            Role::ColumnGroup {
                span, collapsed, ..
            } => {
                let inner = source.children(child);
                let first = out.columns.len();
                let mut any = false;
                for col in inner {
                    if let Role::Column {
                        span,
                        collapsed: own,
                        ..
                    } = source.role(col)
                    {
                        any = true;
                        for _ in 0..span {
                            out.columns.push(ColumnSource {
                                node: Some(col),
                                collapsed: collapsed || own,
                            });
                        }
                    } else if source.is_hidden(col) {
                        out.hidden.push(col);
                    }
                }
                if !any {
                    for _ in 0..span {
                        out.columns.push(ColumnSource {
                            node: Some(child),
                            collapsed,
                        });
                    }
                }
                out.column_groups.push(ColumnGroupSource {
                    node: child,
                    first,
                    count: out.columns.len() - first,
                });
            }
            Role::Column {
                span, collapsed, ..
            } => {
                for _ in 0..span {
                    out.columns.push(ColumnSource {
                        node: Some(child),
                        collapsed,
                    });
                }
            }
            Role::RowGroup {
                kind, collapsed, ..
            } => {
                close_loose(source, &mut sections, &mut loose, &mut out.hidden);
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
                let inner = source.children(child);
                let rows = rows_of(source, &inner, &mut out.hidden);
                sections.push(Section {
                    kind,
                    node: Some(child),
                    collapsed,
                    rows,
                });
            }
            _ if source.is_hidden(child) => out.hidden.push(child),
            _ => loose.push(child),
        }
    }
    close_loose(source, &mut sections, &mut loose, &mut out.hidden);

    // Header first, footer last, the rest as they came.
    let mut ordered: Vec<Section<S::Id>> = Vec::with_capacity(sections.len());
    let mut footer: Option<Section<S::Id>> = None;
    let mut header: Option<Section<S::Id>> = None;
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

    // Place the cells: the next free column of the row, spanning rows down to the
    // end of the section.
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let mut column_count = out.columns.len();
    for section in ordered {
        let first_row = out.rows.len();
        let section_rows = section.rows.len();
        let group = section.node.map(|_| out.groups.len());
        for (offset, row) in section.rows.into_iter().enumerate() {
            let row_index = out.rows.len();
            let mut row_cells = Vec::new();
            let mut col = 0usize;
            if occupied.len() <= row_index {
                occupied.resize_with(row_index + 1, Vec::new);
            }
            for piece in row.pieces {
                while occupied[row_index].get(col).copied().unwrap_or(false) {
                    col += 1;
                }
                let (colspan, rowspan, align) = match &piece {
                    Content::Cell(node) => {
                        let Role::Cell(role) = source.role(*node) else {
                            continue;
                        };
                        (
                            role.colspan.max(1) as usize,
                            role.rowspan as usize,
                            role.align,
                        )
                    }
                    Content::Wrap(_) | Content::Own => (1, 1, CellAlign::Top),
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
                row_cells.push(out.cells.len());
                out.cells.push(CellSource {
                    content: piece,
                    row: row_index,
                    col,
                    rows: rowspan,
                    cols: colspan,
                    align,
                });
                col += colspan;
            }
            out.rows.push(RowSource {
                node: row.node,
                group,
                cells: row_cells,
                collapsed: row.collapsed || section.collapsed,
            });
        }
        if let Some(node) = section.node {
            out.groups.push(GroupSource {
                node,
                first: first_row,
                end: out.rows.len(),
            });
        }
    }
    while out.columns.len() < column_count {
        out.columns.push(ColumnSource {
            node: None,
            collapsed: false,
        });
    }
    out
}

impl<I> Default for Structure<I> {
    fn default() -> Self {
        Structure {
            captions: Vec::new(),
            hidden: Vec::new(),
            columns: Vec::new(),
            column_groups: Vec::new(),
            rows: Vec::new(),
            groups: Vec::new(),
            cells: Vec::new(),
        }
    }
}
