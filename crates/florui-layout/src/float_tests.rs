//! `float` and `clear`. Chrome 153's numbers for the same trees.

use crate::test_support::*;

fn root(children: Vec<florui::prelude::Element>) -> florui::prelude::Element {
    div("r", children)
}

const CSS: &str = ".r { width: 200px; }";

#[test]
fn a_float_is_taken_out_of_the_flow_and_a_block_runs_under_it() {
    let tree = root(vec![div("f", vec![]), div("b", vec![])]);
    let css =
        format!("{CSS} .f {{ float: left; width: 50px; height: 50px; }} .b {{ height: 20px; }}");
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("r"), (0.0, 0.0, 200.0, 20.0), "container");
    rect_near(laid.rect("f"), (0.0, 0.0, 50.0, 50.0), "float");
    rect_near(laid.rect("b"), (0.0, 0.0, 200.0, 20.0), "block");
}

#[test]
fn floats_line_up_and_wrap_to_the_next_row_when_they_do_not_fit() {
    let tree = root(vec![
        div("f1", vec![]),
        div("f2", vec![]),
        div("f3", vec![]),
    ]);
    let css = format!(
        "{CSS} .f1, .f2, .f3 {{ float: left; width: 90px; }} \
         .f1 {{ height: 30px; }} .f2 {{ height: 40px; }} .f3 {{ height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("r"), (0.0, 0.0, 200.0, 0.0), "container");
    rect_near(laid.rect("f1"), (0.0, 0.0, 90.0, 30.0), "first");
    rect_near(laid.rect("f2"), (90.0, 0.0, 90.0, 40.0), "second");
    rect_near(
        laid.rect("f3"),
        (0.0, 40.0, 90.0, 10.0),
        "third, below the taller one",
    );
}

#[test]
fn a_float_to_the_right_sits_at_the_edge_inside_its_margin() {
    let tree = root(vec![div("f", vec![]), div("b", vec![])]);
    let css = format!(
        "{CSS} .f {{ float: right; width: 50px; height: 30px; margin: 5px 10px; }} \
         .b {{ height: 20px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("f"), (140.0, 5.0, 50.0, 30.0), "float");
    rect_near(laid.rect("b"), (0.0, 0.0, 200.0, 20.0), "block");
}

#[test]
fn clear_moves_a_block_below_the_floats() {
    let tree = root(vec![div("f", vec![]), div("b", vec![])]);
    let css = format!(
        "{CSS} .f {{ float: left; width: 50px; height: 50px; }} \
         .b {{ clear: left; height: 20px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("r"), (0.0, 0.0, 200.0, 70.0), "container");
    rect_near(laid.rect("b"), (0.0, 50.0, 200.0, 20.0), "cleared block");
}

#[test]
fn clear_both_waits_for_the_lower_of_the_two_sides() {
    let tree = root(vec![div("f1", vec![]), div("f2", vec![]), div("b", vec![])]);
    let css = format!(
        "{CSS} .f1 {{ float: left; width: 50px; height: 30px; }} \
         .f2 {{ float: right; width: 50px; height: 60px; }} \
         .b {{ clear: both; height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("r"), (0.0, 0.0, 200.0, 70.0), "container");
    rect_near(laid.rect("f2"), (150.0, 0.0, 50.0, 60.0), "right float");
    rect_near(laid.rect("b"), (0.0, 60.0, 200.0, 10.0), "cleared block");
}

#[test]
fn a_block_that_makes_a_context_does_not_overlap_a_float() {
    let tree = root(vec![div("f", vec![]), div("b", vec![])]);
    let css = format!(
        "{CSS} .f {{ float: left; width: 50px; height: 50px; }} \
         .b {{ overflow: hidden; height: 20px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("b"), (50.0, 0.0, 150.0, 20.0), "beside the float");
}

#[test]
fn a_float_inside_a_padded_block_floats_inside_its_content_box() {
    let tree = root(vec![div("a", vec![div("f", vec![])]), div("b", vec![])]);
    let css = format!(
        "{CSS} .a {{ padding: 5px; }} .f {{ float: right; width: 40px; height: 40px; }} \
         .b {{ height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("a"), (0.0, 0.0, 200.0, 10.0), "padded block");
    rect_near(laid.rect("f"), (155.0, 5.0, 40.0, 40.0), "float");
    rect_near(laid.rect("b"), (0.0, 10.0, 200.0, 10.0), "next block");
}

#[test]
fn a_float_without_a_width_is_as_wide_as_its_content() {
    let tree = root(vec![div("f", vec![div("c", vec![])])]);
    let css = format!("{CSS} .f {{ float: left; }} .c {{ width: 70px; height: 20px; }}");
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("f"), (0.0, 0.0, 70.0, 20.0), "float");
}

#[test]
fn a_flow_root_or_a_scroll_container_contains_its_floats_and_a_plain_block_does_not() {
    for (style, height) in [
        ("display: flow-root;", 80.0),
        ("overflow: hidden;", 80.0),
        ("", 0.0),
    ] {
        let tree = root(vec![div("a", vec![div("f", vec![])])]);
        let css =
            format!("{CSS} .a {{ {style} }} .f {{ float: left; width: 50px; height: 80px; }}");
        let laid = lay_out(&tree, &css);
        rect_near(laid.rect("a"), (0.0, 0.0, 200.0, height), style);
    }
}

#[test]
fn a_position_other_than_static_takes_a_box_out_of_floating() {
    let tree = root(vec![div("f", vec![]), div("b", vec![])]);
    let css = format!(
        "{CSS} .f {{ float: left; position: absolute; width: 50px; height: 50px; }} \
         .b {{ height: 20px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("f"), (0.0, 0.0, 50.0, 50.0), "positioned");
    rect_near(laid.rect("b"), (0.0, 0.0, 200.0, 20.0), "block");
}

#[test]
fn a_float_wider_than_its_container_sticks_out_on_its_own_side() {
    let tree = root(vec![div("f", vec![])]);
    for (side, x) in [("left", 0.0), ("right", -50.0)] {
        let css = format!("{CSS} .f {{ float: {side}; width: 250px; height: 10px; }}");
        let laid = lay_out(&tree, &css);
        rect_near(laid.rect("f"), (x, 0.0, 250.0, 10.0), side);
    }
}

#[test]
fn a_float_too_wide_for_the_room_beside_another_goes_below_it() {
    let tree = root(vec![div("f1", vec![]), div("f2", vec![])]);
    let css = format!(
        "{CSS} .f1 {{ float: right; width: 120px; height: 20px; }} \
         .f2 {{ float: left; width: 100px; height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("f1"), (80.0, 0.0, 120.0, 20.0), "right");
    rect_near(laid.rect("f2"), (0.0, 20.0, 100.0, 10.0), "left, below it");
}

#[test]
fn a_float_with_no_height_takes_no_room() {
    let tree = root(vec![div("f1", vec![]), div("f2", vec![])]);
    let css = format!(
        "{CSS} .f1 {{ float: left; width: 50px; height: 0; }} \
         .f2 {{ float: left; width: 50px; height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("f2"), (0.0, 0.0, 50.0, 10.0), "second");
}

#[test]
fn a_float_after_a_block_starts_below_it_and_after_its_margin() {
    let tree = root(vec![div("b", vec![]), div("f", vec![]), div("c", vec![])]);
    let css = format!(
        "{CSS} .b {{ height: 20px; margin-bottom: 30px; }} \
         .f {{ float: left; width: 50px; height: 10px; }} \
         .c {{ height: 5px; margin-top: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("f"), (0.0, 50.0, 50.0, 10.0), "float");
    rect_near(laid.rect("c"), (0.0, 50.0, 200.0, 5.0), "block after it");
}

#[test]
fn clearance_puts_the_border_edge_at_the_float_and_drops_the_margin() {
    let tree = root(vec![div("f", vec![]), div("b", vec![]), div("c", vec![])]);
    let css = format!(
        "{CSS} .f {{ float: left; width: 50px; height: 30px; }} \
         .b {{ clear: left; height: 5px; margin-top: 10px; }} .c {{ height: 5px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("b"), (0.0, 30.0, 200.0, 5.0), "cleared");
    rect_near(laid.rect("c"), (0.0, 35.0, 200.0, 5.0), "after it");
}

#[test]
fn a_box_already_below_the_float_is_not_moved_by_clear() {
    let tree = root(vec![div("f", vec![]), div("s", vec![]), div("b", vec![])]);
    let css = format!(
        "{CSS} .f {{ float: left; width: 50px; height: 10px; }} .s {{ height: 40px; }} \
         .b {{ clear: left; height: 5px; margin-top: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("b"), (0.0, 50.0, 200.0, 5.0), "box");
}

#[test]
fn a_float_moves_with_the_margin_of_its_container_that_collapses_through() {
    let tree = root(vec![div("p", vec![div("f", vec![])])]);
    let css = format!(
        "{CSS} .p {{ margin-top: 10px; }} \
         .f {{ float: left; width: 50px; height: 50px; margin-top: 20px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("r"), (0.0, 10.0, 200.0, 0.0), "root");
    rect_near(laid.rect("p"), (0.0, 10.0, 200.0, 0.0), "parent");
    rect_near(laid.rect("f"), (0.0, 30.0, 50.0, 50.0), "float");
}

const FOX: &str =
    "The quick brown fox jumps over the lazy dog and keeps running along the road home again";

const TEXT_CSS: &str = ".p { width: 200px; font-size: 16px; line-height: 20px; }";

#[test]
fn text_wraps_around_a_float_to_the_left() {
    let tree = div("p", vec![div("f", vec![]), text(FOX)]);
    let css = format!("{TEXT_CSS} .f {{ float: left; width: 50px; height: 50px; }}");
    let laid = lay_out(&tree, &css);
    // Chrome 153: five lines, the first three beside the float.
    rect_near(laid.rect("p"), (0.0, 0.0, 200.0, 100.0), "paragraph");
    rect_near(laid.rect("f"), (0.0, 0.0, 50.0, 50.0), "float");
}

#[test]
fn text_wraps_around_a_float_to_the_right_with_margins() {
    let tree = div("p", vec![div("f", vec![]), text(FOX)]);
    let css =
        format!("{TEXT_CSS} .f {{ float: right; width: 50px; height: 30px; margin: 5px 10px; }}");
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("p"), (0.0, 0.0, 200.0, 100.0), "paragraph");
    rect_near(laid.rect("f"), (140.0, 5.0, 50.0, 30.0), "float");
}

#[test]
fn a_float_among_the_text_goes_to_the_top_of_the_line_it_is_met_on() {
    let tree = div(
        "p",
        vec![
            text("ab "),
            div("f", vec![]),
            text(" cd ef gh ij kl mn op qr st uv wx yz 01 23 45 67 89 ab cd ef"),
        ],
    );
    let css = format!("{TEXT_CSS} .f {{ float: right; width: 60px; height: 30px; }}");
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("p"), (0.0, 0.0, 200.0, 60.0), "paragraph");
    rect_near(laid.rect("f"), (140.0, 0.0, 60.0, 30.0), "float");
}

#[test]
fn a_float_beside_text_adds_to_how_wide_the_text_needs_its_container_to_be() {
    // Chrome 153: the float (30) and the text (28.77) are on one line.
    let tree = div("r", vec![div("s", vec![div("f", vec![]), text("text")])]);
    let css =
        ".r { width: 200px; } .s { float: left; } .f { float: left; width: 30px; height: 50px; }";
    let laid = lay_out(&tree, css);
    near(laid.rect("s").2, 58.77, 0.05, "shrink-to-fit width");
}
