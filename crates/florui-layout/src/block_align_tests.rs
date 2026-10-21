//! `text-align: -webkit-center | -webkit-left | -webkit-right`, which puts the blocks of a
//! container where the text goes. Chrome 153's numbers.

use crate::test_support::*;

const CSS: &str = "body { width: 400px; } .p { width: 300px; } .m { width: 100px; }";

fn block_x(css: &str) -> f32 {
    let tree = div("p", vec![div("m", vec![text("b")])]);
    lay_out(&tree, &format!("{CSS} {css}")).rect("m").0
}

#[test]
fn a_block_is_centered_with_its_margins_beside_it() {
    let center = ".p { text-align: -webkit-center; }";
    for (margin, expected) in [
        ("", 100.0),
        ("margin-left: 0;", 100.0),
        ("margin-left: 10px;", 105.0),
        ("margin-right: 10px;", 95.0),
        ("margin-left: 10px; margin-right: 30px;", 90.0),
        ("margin-left: auto;", 200.0),
    ] {
        near(
            block_x(&format!("{center} .m {{ {margin} }}")),
            expected,
            0.01,
            margin,
        );
    }
}

#[test]
fn a_block_is_at_the_end_or_the_start_with_its_margins() {
    let margins = ".m { margin-left: 10px; margin-right: 7px; }";
    near(
        block_x(&format!(".p {{ text-align: -webkit-right; }} {margins}")),
        193.0,
        0.01,
        "right",
    );
    near(
        block_x(&format!(".p {{ text-align: -webkit-left; }} {margins}")),
        10.0,
        0.01,
        "left",
    );
}

#[test]
fn every_kind_of_block_in_the_flow_is_aligned() {
    for display in ["block", "flex", "grid", "flow-root", "table", "list-item"] {
        near(
            block_x(&format!(
                ".p {{ text-align: -webkit-center; }} .m {{ margin-left: 10px; display: {display}; }}"
            )),
            105.0,
            0.01,
            display,
        );
    }
    near(
        block_x(".p { text-align: -webkit-center; } .m { margin-left: 10px; overflow: hidden; }"),
        105.0,
        0.01,
        "overflow",
    );
}

#[test]
fn the_room_is_that_of_the_content_box_and_never_negative() {
    // Chrome: a parent with padding and a border puts the block at 3 + 6 + 10 + 95.
    near(
        block_x(
            ".p { text-align: -webkit-center; padding: 0 20px 0 6px; border: 3px solid #000; \
             box-sizing: content-box; } .m { margin-left: 10px; }",
        ),
        114.0,
        0.01,
        "padding and border",
    );
    // A block wider than its container stays at its margin.
    near(
        block_x(
            ".p { text-align: -webkit-center; } .m { width: 340px; margin-left: 10px; \
             margin-right: 4px; }",
        ),
        10.0,
        0.01,
        "wider",
    );
}

#[test]
fn a_block_sized_by_a_percentage_or_a_maximum_is_aligned_too() {
    near(
        block_x(".p { text-align: -webkit-center; } .m { width: 50%; margin-left: 10px; }"),
        80.0,
        0.01,
        "percentage",
    );
    near(
        block_x(
            ".p { text-align: -webkit-center; } .m { width: auto; max-width: 100px; \
             margin-left: 10px; }",
        ),
        105.0,
        0.01,
        "maximum",
    );
}

#[test]
fn a_float_a_box_out_of_the_flow_and_an_auto_margin_are_left_as_they_are() {
    let css = ".p { text-align: -webkit-center; } .m { margin-left: 10px; float: left; }";
    near(block_x(css), 10.0, 0.01, "float");
    let css = ".p { text-align: -webkit-center; position: relative; } \
               .m { margin-left: 10px; position: absolute; left: 20px; }";
    near(block_x(css), 30.0, 0.01, "absolute");
    let css = ".p { text-align: -webkit-center; } .m { margin-left: 10px; position: relative; left: 7px; }";
    near(block_x(css), 112.0, 0.01, "relative");
    let css = ".p { text-align: -webkit-center; } .m { margin-left: 10px; margin-right: auto; }";
    near(block_x(css), 10.0, 0.01, "an auto margin on the other side");
}
