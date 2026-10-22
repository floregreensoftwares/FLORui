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

#[test]
fn an_absolutely_positioned_box_with_no_inset_sits_at_the_start_edge() {
    let css = ".a { position: relative; width: 200px; height: 40px; direction: rtl; } \
        .abs { position: absolute; width: 20px; height: 20px; }";
    let block = lay_out(&div("a", vec![div("abs", vec![])]), css);
    rect_near(block.rect("abs"), (180.0, 0.0, 20.0, 20.0), "a block");
    let inline = lay_out(&div("a", vec![element("i", "abs", vec![])]), css);
    rect_near(
        inline.rect("abs"),
        (180.0, 0.0, 20.0, 20.0),
        "an inline element",
    );
}

#[test]
fn an_absolutely_positioned_box_with_no_inset_beside_one_with_an_inset_sits_at_the_start_edge() {
    let css = ".a { position: relative; width: 200px; height: 40px; direction: rtl; } \
        .a i { position: absolute; width: 20px; height: 20px; } \
        .a b { position: absolute; top: 10px; width: 20px; height: 20px; right: 5px; }";
    let laid = lay_out(
        &div(
            "a",
            vec![
                element("i", "first", vec![]),
                element("b", "second", vec![]),
            ],
        ),
        css,
    );
    rect_near(laid.rect("first"), (180.0, 0.0, 20.0, 20.0), "no inset");
    rect_near(
        laid.rect("second"),
        (175.0, 10.0, 20.0, 20.0),
        "with an inset",
    );
}

#[test]
fn an_absolutely_positioned_box_with_an_empty_text_sits_at_the_start_edge() {
    let css = ".a { position: relative; width: 200px; height: 40px; direction: rtl; } \
        .abs { position: absolute; width: 20px; height: 20px; }";
    let laid = lay_out(&div("a", vec![element("i", "abs", vec![text("")])]), css);
    rect_near(
        laid.rect("abs"),
        (180.0, 0.0, 20.0, 20.0),
        "a box with an empty text",
    );
}

/// A float in the context makes the blocks of it lay out through the walk that places them
/// beside floats, which has to put the same things at the start edge.
const WITH_FLOAT: &str = ".wrap { width: 300px; } .rtl { direction: rtl; } \
    .box { width: 40px; height: 14px; } \
    .fl { float: left; width: 30px; height: 20px; } \
    .abs { position: absolute; width: 20px; height: 20px; } \
    .rel { position: relative; width: 200px; height: 40px; } \
    .flow { display: flow-root; width: 60px; height: 10px; }";

#[test]
fn blocks_beside_a_float_still_start_at_the_right_edge_of_a_right_to_left_box() {
    let laid = lay_out(
        &div(
            "wrap rtl",
            vec![
                div("fl", vec![]),
                div("box a", vec![]),
                div("flow b", vec![]),
                div("box c", vec![]),
            ],
        ),
        WITH_FLOAT,
    );
    rect_near(laid.rect("a"), (260.0, 0.0, 40.0, 14.0), "a block");
    rect_near(
        laid.rect("fl"),
        (0.0, 0.0, 30.0, 20.0),
        "the float stays on the left",
    );
    // Beside the float: the room left of it is the room of a box of its own context, which
    // goes against the right edge.
    let (x, _, width, _) = laid.rect("b");
    near(
        x + width,
        300.0,
        0.05,
        "a context of its own ends at the right edge",
    );
}

#[test]
fn an_absolutely_positioned_box_in_a_context_with_floats_sits_at_the_start_edge() {
    let laid = lay_out(
        &div(
            "wrap",
            vec![
                div("fl", vec![]),
                div("rel rtl", vec![element("i", "abs", vec![])]),
            ],
        ),
        WITH_FLOAT,
    );
    let (x, _, width, _) = laid.rect("abs");
    near(
        x + width,
        200.0,
        0.05,
        "against the right edge of its container",
    );
}

#[test]
fn margins_in_a_right_to_left_box_with_a_float_are_as_chrome_has_them() {
    let css = format!(
        "{WITH_FLOAT} .m {{ width: 40px; height: 14px; margin: 0 7px 0 3px; }} \
         .auto {{ width: 40px; height: 14px; margin: 0 auto; }}"
    );
    let laid = lay_out(
        &div(
            "wrap rtl",
            vec![
                div("fl", vec![]),
                div("box a", vec![]),
                div("flow b", vec![]),
                div("box c", vec![]),
                div("m d", vec![]),
                div("auto e", vec![]),
            ],
        ),
        &css,
    );
    rect_near(laid.rect("a"), (260.0, 0.0, 40.0, 14.0), "a block");
    rect_near(
        laid.rect("b"),
        (240.0, 14.0, 60.0, 10.0),
        "beside the float, at the right edge",
    );
    rect_near(laid.rect("c"), (260.0, 24.0, 40.0, 14.0), "below it");
    rect_near(
        laid.rect("d"),
        (253.0, 38.0, 40.0, 14.0),
        "the right margin is kept",
    );
    rect_near(
        laid.rect("e"),
        (130.0, 52.0, 40.0, 14.0),
        "centred by auto margins",
    );
}
