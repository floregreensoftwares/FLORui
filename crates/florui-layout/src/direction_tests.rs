//! `direction: rtl` on boxes: what flows from the right edge. Chrome 155's numbers for the
//! same trees.

use crate::test_support::*;

const CSS: &str = ".wrap { width: 300px; } .rtl { direction: rtl; } \
    .box { width: 40px; height: 14px; } \
    .flex { display: flex; } .flex .box { margin-right: 6px; } \
    .grid { display: grid; grid-template-columns: 40px 40px 40px; gap: 4px; } \
    .table { display: table; } .cell { display: table-cell; width: 50px; }";

#[test]
fn a_block_narrower_than_its_container_sits_at_the_right_edge() {
    let laid = lay_out(
        &div("wrap rtl", vec![div("box a", vec![]), div("box b", vec![])]),
        CSS,
    );
    rect_near(laid.rect("a"), (260.0, 0.0, 40.0, 14.0), "the first block");
    rect_near(
        laid.rect("b"),
        (260.0, 14.0, 40.0, 14.0),
        "the second block",
    );
}

#[test]
fn a_left_to_right_block_stays_at_the_left_edge() {
    let laid = lay_out(&div("wrap", vec![div("box a", vec![])]), CSS);
    rect_near(laid.rect("a"), (0.0, 0.0, 40.0, 14.0), "the block");
}

#[test]
fn direction_is_inherited_by_the_boxes_inside() {
    let laid = lay_out(
        &div("wrap rtl", vec![div("", vec![div("box a", vec![])])]),
        CSS,
    );
    rect_near(laid.rect("a"), (260.0, 0.0, 40.0, 14.0), "the block");
}

#[test]
fn a_flex_row_runs_from_the_right_edge_and_keeps_its_physical_margins() {
    let laid = lay_out(
        &div(
            "wrap flex rtl",
            vec![
                div("box a", vec![]),
                div("box b", vec![]),
                div("box c", vec![]),
            ],
        ),
        CSS,
    );
    rect_near(laid.rect("a"), (254.0, 0.0, 40.0, 14.0), "the first item");
    rect_near(laid.rect("b"), (208.0, 0.0, 40.0, 14.0), "the second item");
    rect_near(laid.rect("c"), (162.0, 0.0, 40.0, 14.0), "the third item");
}

#[test]
fn the_columns_of_a_grid_start_at_the_right_edge() {
    let laid = lay_out(
        &div(
            "wrap grid rtl",
            vec![
                div("box a", vec![]),
                div("box b", vec![]),
                div("box c", vec![]),
            ],
        ),
        CSS,
    );
    near(laid.rect("a").0, 260.0, 0.05, "the first column");
    near(laid.rect("b").0, 216.0, 0.05, "the second column");
    near(laid.rect("c").0, 172.0, 0.05, "the third column");
}

#[test]
fn the_first_cell_of_a_table_is_the_rightmost() {
    let laid = lay_out(
        &div(
            "wrap",
            vec![div(
                "table rtl",
                vec![
                    div("cell a", vec![]),
                    div("cell b", vec![]),
                    div("cell c", vec![]),
                ],
            )],
        ),
        CSS,
    );
    let (a, b, c) = (laid.rect("a").0, laid.rect("b").0, laid.rect("c").0);
    assert!(a > b && b > c, "{a} {b} {c}");
    near(b - c, 50.0, 0.05, "the cells are side by side");
}
