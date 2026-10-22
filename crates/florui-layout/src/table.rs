//! What a table part is to layout, and the structure of a table: its rows, its
//! columns and the cells that fill the grid between them. The algorithm that
//! sizes the grid is in [`crate::table_layout`].

use crate::collapse::{Sides, sides_of};
use florui_style::{
    BorderCollapse, CaptionSide, ComputedStyle, Display, NodeId, StyleMap, TableLayout, Tree,
    VerticalAlign, Visibility,
};

/// Where the content of a cell sits in its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellAlign {
    Top,
    Middle,
    Bottom,
    /// On the baseline the row shares: what `vertical-align` says of anything
    /// else (`sub`, `text-top`, a length) is the same for a cell.
    Baseline,
}

/// Which row group a `<thead>`-like box is: the first header group is shown
/// above every row and the first footer group below every row, wherever they
/// are among the children of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupKind {
    Header,
    Body,
    Footer,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TableRole {
    /// `table-layout: fixed` (it only applies when the width is not auto).
    pub(crate) fixed: bool,
    /// `border-collapse: collapse`.
    pub(crate) collapse: bool,
    /// `border-spacing`, horizontal and vertical.
    pub(crate) spacing: (f32, f32),
    /// The borders of the table, which collapse into those of its cells.
    pub(crate) sides: Sides,
    /// `direction: rtl`: the first column is the rightmost.
    pub(crate) rtl: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CellRole {
    pub(crate) colspan: u32,
    /// `0` is every row to the end of the group.
    pub(crate) rowspan: u32,
    pub(crate) align: CellAlign,
    /// `empty-cells: hide`.
    pub(crate) hide_empty: bool,
    pub(crate) sides: Sides,
}

/// What a node is in a table, from its `display` (and the `colspan`, `rowspan`
/// and `span` attributes of the elements that take them).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) enum Role {
    #[default]
    None,
    Table(TableRole),
    Caption {
        bottom: bool,
    },
    ColumnGroup {
        span: u32,
        collapsed: bool,
        sides: Sides,
    },
    Column {
        span: u32,
        collapsed: bool,
        sides: Sides,
    },
    RowGroup {
        kind: GroupKind,
        collapsed: bool,
        sides: Sides,
    },
    Row {
        collapsed: bool,
        sides: Sides,
    },
    Cell(CellRole),
}

impl Role {
    pub(crate) fn is_table(self) -> bool {
        matches!(self, Role::Table(_))
    }
}

/// A span attribute: a whole number from `min`, or `1` when it is missing or
/// not one.
fn span_of(arena: &Tree, node: NodeId, name: &str, min: u32, max: u32) -> u32 {
    arena
        .attr(node, name)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .map_or(1, |value| value.clamp(min, max))
}

fn cell_align(vertical_align: VerticalAlign) -> CellAlign {
    match vertical_align {
        VerticalAlign::Top => CellAlign::Top,
        VerticalAlign::Middle => CellAlign::Middle,
        VerticalAlign::Bottom => CellAlign::Bottom,
        _ => CellAlign::Baseline,
    }
}

/// What `node` is in a table.
pub(crate) fn role_of(arena: &Tree, styles: &StyleMap, node: NodeId) -> Role {
    let Some(style) = styles.get(&node) else {
        return Role::None;
    };
    if !style.display.is_table() && !style.display.is_table_part() {
        return Role::None;
    }
    let collapsed = style.visibility == Visibility::Collapse;
    let sides = sides_of(style);
    match style.display {
        Display::Table | Display::InlineTable => Role::Table(table_role(style)),
        Display::TableCaption => Role::Caption {
            bottom: style.caption_side == CaptionSide::Bottom,
        },
        Display::TableColumnGroup => Role::ColumnGroup {
            span: span_of(arena, node, "span", 1, 1000),
            collapsed,
            sides,
        },
        Display::TableColumn => Role::Column {
            span: span_of(arena, node, "span", 1, 1000),
            collapsed,
            sides,
        },
        Display::TableRowGroup => Role::RowGroup {
            kind: GroupKind::Body,
            collapsed,
            sides,
        },
        Display::TableHeaderGroup => Role::RowGroup {
            kind: GroupKind::Header,
            collapsed,
            sides,
        },
        Display::TableFooterGroup => Role::RowGroup {
            kind: GroupKind::Footer,
            collapsed,
            sides,
        },
        Display::TableRow => Role::Row { collapsed, sides },
        Display::TableCell => {
            // Only the cells of HTML take the attributes.
            let html = matches!(arena.tag(node), "td" | "th");
            Role::Cell(CellRole {
                colspan: if html {
                    span_of(arena, node, "colspan", 1, 1000)
                } else {
                    1
                },
                rowspan: if html {
                    arena
                        .attr(node, "rowspan")
                        .and_then(|value| value.trim().parse::<u32>().ok())
                        .map_or(1, |value| value.min(65534))
                } else {
                    1
                },
                align: cell_align(style.vertical_align),
                hide_empty: style.empty_cells == florui_style::EmptyCells::Hide,
                sides,
            })
        }
        _ => Role::None,
    }
}

fn table_role(style: &ComputedStyle) -> TableRole {
    TableRole {
        fixed: style.table_layout == TableLayout::Fixed,
        collapse: style.border_collapse == BorderCollapse::Collapse,
        spacing: style.border_spacing,
        sides: sides_of(style),
        rtl: style.direction.is_rtl(),
    }
}
