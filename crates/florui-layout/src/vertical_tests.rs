//! Boxes with a vertical `writing-mode`: Chrome's numbers for the same trees, in the page
//! of the fixture this is checked against (the font is 14px with 18px lines).

use crate::test_support::*;

const CSS: &str = ".wrap { position: relative; width: 340px; height: 300px; font-size: 14px; \
    line-height: 18px; display: flex; flex-wrap: wrap; align-content: flex-start; gap: 8px; \
    padding: 8px; box-sizing: border-box; } \
    .a { writing-mode: vertical-rl; height: 100px; } \
    .b { writing-mode: vertical-lr; height: 100px; } \
    .c { writing-mode: vertical-rl; height: 100px; } .c p { margin: 0 0 4px 0; } \
    .d { writing-mode: vertical-rl; width: 60px; height: 120px; } \
    .f { writing-mode: vertical-rl; height: 100px; display: flex; } \
    .f span { margin-bottom: 4px; }";

fn page() -> florui::prelude::Element {
    div(
        "wrap",
        vec![
            div("a", vec![text("abc def ghi")]),
            div("b", vec![text("abc def ghi")]),
            div(
                "c",
                vec![
                    element("p", "p1", vec![text("first")]),
                    element("p", "p2", vec![text("second")]),
                ],
            ),
            div(
                "d",
                vec![text("text that wraps into several columns in this box")],
            ),
            div(
                "f",
                vec![span("s1", vec![text("one")]), span("s2", vec![text("two")])],
            ),
        ],
    )
}

#[test]
fn a_vertical_box_is_as_wide_as_its_lines_and_as_tall_as_it_is_told() {
    let laid = lay_out(&page(), CSS);
    rect_near(
        laid.rect("a"),
        (8.0, 8.0, 18.0, 100.0),
        "vertical-rl, one line",
    );
    rect_near(
        laid.rect("b"),
        (34.0, 8.0, 18.0, 100.0),
        "vertical-lr, one line",
    );
}

#[test]
fn blocks_in_a_vertical_rl_box_stack_from_the_right_and_keep_their_physical_margins() {
    let laid = lay_out(&page(), CSS);
    rect_near(laid.rect("c"), (60.0, 8.0, 36.0, 100.0), "the box");
    // The first block is on the right edge; the margin below stays below the block, so it
    // takes the room of the line along which the text runs.
    rect_near(laid.rect("p1"), (78.0, 8.0, 18.0, 96.0), "the first block");
    rect_near(laid.rect("p2"), (60.0, 8.0, 18.0, 96.0), "the second block");
}

#[test]
fn a_fixed_width_box_has_the_lines_that_do_not_fit_overflow_it() {
    let laid = lay_out(&page(), CSS);
    rect_near(laid.rect("d"), (104.0, 8.0, 60.0, 120.0), "the box");
}

#[test]
fn a_flex_row_in_a_vertical_box_runs_down_the_page() {
    let laid = lay_out(&page(), CSS);
    rect_near(laid.rect("f"), (172.0, 8.0, 18.0, 100.0), "the container");
    let (_, y1, w1, h1) = laid.rect("s1");
    let (_, y2, _, _) = laid.rect("s2");
    near(w1, 18.0, 0.05, "the width of an item is its line");
    near(h1, 24.88, 0.05, "the height of an item is its text");
    near(y2 - y1, h1 + 4.0, 0.05, "the margin is below the item");
}

const MORE: &str = ".wrap { position: relative; width: 340px; height: 300px; font-size: 14px; \
    line-height: 18px; display: flex; flex-wrap: wrap; align-content: flex-start; gap: 8px; \
    padding: 8px; box-sizing: border-box; } \
    .h { writing-mode: vertical-lr; width: 60px; height: 70px; } \
    .i { writing-mode: vertical-lr; width: 60px; height: 100px; } \
    .j { writing-mode: vertical-rl; height: 100px; } \
    .s { padding: 3px 2px; border-right: 2px solid #c02020; } \
    .l { writing-mode: vertical-rl; height: 100px; } \
    .hz { writing-mode: horizontal-tb; margin: 0; }";

fn more() -> florui::prelude::Element {
    div(
        "wrap",
        vec![
            div("h", vec![text("text that wraps into columns")]),
            div("i", vec![text("abc")]),
            div(
                "j",
                vec![text("ab"), span("s", vec![text("cd")]), text("ef")],
            ),
            div("l", vec![element("p", "hz", vec![text("abc def")])]),
        ],
    )
}

#[test]
fn a_vertical_lr_box_keeps_the_size_it_is_given() {
    let laid = lay_out(&more(), MORE);
    rect_near(laid.rect("h"), (8.0, 8.0, 60.0, 70.0), "narrow height");
    rect_near(laid.rect("i"), (76.0, 8.0, 60.0, 100.0), "wide, one line");
}

#[test]
fn an_inline_box_in_vertical_lines_has_its_padding_and_border_on_the_sides_of_the_page() {
    let laid = lay_out(&more(), MORE);
    // The padding along the line is the top and bottom one; the one across it is on the left
    // and right, the right one with the border outside the text.
    rect_near(laid.rect("s"), (142.0, 24.36, 25.0, 21.28), "the span");
}

#[test]
fn a_box_of_the_other_flow_is_as_wide_as_its_text() {
    let laid = lay_out(&more(), MORE);
    rect_near(
        laid.rect("l"),
        (170.0, 8.0, 47.83, 100.0),
        "the vertical box",
    );
    rect_near(
        laid.rect("hz"),
        (170.0, 8.0, 47.83, 18.0),
        "the horizontal box in it",
    );
}

#[test]
fn padding_and_border_of_a_vertical_box_are_on_its_sides_of_the_page() {
    let css = format!(
        "{MORE} .pa {{ writing-mode: vertical-rl; height: 100px; padding: 5px 7px 3px 2px; \
         border: 2px solid #888; margin-top: 3px; }}"
    );
    let tree = div("wrap", vec![div("pa", vec![text("padded")])]);
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("pa"), (8.0, 11.0, 31.0, 112.0), "the box");
}

#[test]
fn an_inline_block_in_vertical_lines_keeps_its_size_on_the_page_and_sits_on_the_middle_of_the_line()
{
    let css = format!(
        "{MORE} .ib {{ writing-mode: vertical-rl; height: 100px; }} \
         .ib b {{ display: inline-block; width: 20px; height: 30px; }}"
    );
    let tree = div(
        "wrap",
        vec![div(
            "ib",
            vec![
                text("ab "),
                element("b", "box", vec![text("x")]),
                text(" cd"),
            ],
        )],
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("ib"), (8.0, 8.0, 20.0, 100.0), "the line");
    rect_near(laid.rect("box"), (8.0, 27.98, 20.0, 30.0), "the box");
}

const SIDEWAYS: &str = ".wrap { position: relative; width: 340px; height: 300px; font-size: 14px; \
    line-height: 18px; display: flex; flex-wrap: wrap; align-content: flex-start; gap: 8px; \
    padding: 8px; box-sizing: border-box; } \
    .a { writing-mode: sideways-rl; height: 100px; } \
    .b { writing-mode: sideways-lr; height: 100px; } \
    .c { writing-mode: sideways-lr; height: 100px; width: 50px; } \
    .d { writing-mode: sideways-lr; height: 100px; } \
    .s { padding: 3px 2px; border-right: 2px solid #c02020; } \
    .e { writing-mode: sideways-lr; height: 100px; display: flex; } \
    .e span { margin-bottom: 4px; }";

fn sideways() -> florui::prelude::Element {
    div(
        "wrap",
        vec![
            div("a", vec![text("abc def ghi")]),
            div("b", vec![text("abc def ghi")]),
            div("c", vec![text("text that wraps into columns here")]),
            div(
                "d",
                vec![text("ab"), span("s", vec![text("cd")]), text("ef")],
            ),
            div(
                "e",
                vec![span("s1", vec![text("one")]), span("s2", vec![text("two")])],
            ),
        ],
    )
}

#[test]
fn the_lines_of_a_sideways_lr_box_stack_from_the_left_and_start_at_the_bottom() {
    let laid = lay_out(&sideways(), SIDEWAYS);
    rect_near(laid.rect("a"), (8.0, 8.0, 18.0, 100.0), "sideways-rl");
    rect_near(laid.rect("b"), (34.0, 8.0, 18.0, 100.0), "sideways-lr");
    rect_near(
        laid.rect("c"),
        (60.0, 8.0, 50.0, 100.0),
        "sideways-lr, wider than its lines",
    );
    // The line starts at the bottom of the box, so the span is where the text before it ends.
    rect_near(laid.rect("s"), (115.0, 70.36, 25.0, 21.28), "the span");
    // The flex row runs up the page, from the bottom, and the margin below an item is at the
    // start of the row.
    rect_near(
        laid.rect("s1"),
        (144.0, 79.13, 18.0, 24.88),
        "the first item",
    );
    rect_near(
        laid.rect("s2"),
        (144.0, 50.86, 18.0, 24.27),
        "the second item",
    );
}
