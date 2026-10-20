//! `inline-flex` and `inline-grid` are one atomic box in a line, sitting on the
//! first baseline of their items. The numbers are Chrome 153's
//! `getBoundingClientRect` for the same tree with the same font.

use crate::test_support::*;

const CSS: &str = ".p { font-size: 16px; line-height: 22px; width: 300px; }";

fn line_of(inner: Vec<florui::prelude::Element>) -> florui::prelude::Element {
    div("p", {
        let mut children = vec![text("before ")];
        children.extend(inner);
        children.push(text(" after"));
        children
    })
}

#[test]
fn an_inline_flex_container_stays_in_the_line_and_sizes_to_its_item() {
    let tree = line_of(vec![span("i", vec![span("a", vec![text("flex item")])])]);
    let laid = lay_out(&tree, &format!("{CSS} .i {{ display: inline-flex; }}"));
    rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 22.0), "line");
    rect_near(laid.rect("i"), (53.47, 0.0, 64.48, 22.0), "inline-flex");
    rect_near(laid.rect("a"), (53.47, 0.0, 64.48, 22.0), "item");
}

#[test]
fn an_inline_flex_container_sits_on_the_baseline_of_its_first_item() {
    let tree = line_of(vec![span(
        "i",
        vec![div("a", vec![text("Big")]), div("b", vec![text("small")])],
    )]);
    let css = format!(
        "{CSS} .i {{ display: inline-flex; align-items: flex-start; }} \
         .a {{ font-size: 30px; line-height: 36px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 36.0), "line");
    rect_near(laid.rect("i"), (53.47, 0.0, 82.66, 36.0), "inline-flex");
    rect_near(laid.rect("a"), (53.47, 0.0, 43.25, 36.0), "first item");
    rect_near(laid.rect("b"), (96.72, 0.0, 39.41, 22.0), "second item");
}

#[test]
fn an_inline_grid_container_sits_on_the_baseline_of_its_first_item() {
    let tree = line_of(vec![span(
        "i",
        vec![div("a", vec![]), div("b", vec![text("x")])],
    )]);
    let css = format!(
        "{CSS} .i {{ display: inline-grid; grid-template-columns: 40px 50px; }} \
         .a {{ height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 29.0), "line");
    rect_near(laid.rect("i"), (53.47, 7.0, 90.0, 22.0), "inline-grid");
    rect_near(laid.rect("a"), (53.47, 7.0, 40.0, 10.0), "first cell");
    rect_near(laid.rect("b"), (93.47, 7.0, 50.0, 22.0), "second cell");
}

#[test]
fn an_empty_inline_flex_container_sits_on_the_baseline_by_its_bottom_edge() {
    let tree = line_of(vec![span("i", vec![])]);
    let css = format!("{CSS} .i {{ display: inline-flex; width: 30px; height: 12px; }}");
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 22.0), "line");
    rect_near(laid.rect("i"), (53.47, 5.0, 30.0, 12.0), "inline-flex");
}

#[test]
fn a_column_inline_flex_container_takes_the_baseline_of_its_first_line() {
    let tree = div(
        "p",
        vec![
            span("t", vec![text("before")]),
            text(" "),
            span(
                "i",
                vec![div("a", vec![text("one")]), div("b", vec![text("two")])],
            ),
            text(" after"),
        ],
    );
    let css = format!("{CSS} .i {{ display: inline-flex; flex-direction: column; }}");
    let laid = lay_out(&tree, &css);
    rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 44.0), "line");
    // The text next to it is level with its first line, not its last.
    rect_near(laid.rect("t"), (0.0, 0.0, 49.31, 22.0), "text");
    rect_near(laid.rect("i"), (53.47, 0.0, 28.42, 44.0), "inline-flex");
    rect_near(laid.rect("a"), (53.47, 0.0, 28.42, 22.0), "first item");
    rect_near(laid.rect("b"), (53.47, 22.0, 28.42, 22.0), "second item");
}

fn wrapped_text_beside(display: &str) -> Laid {
    let tree = div(
        "p",
        vec![
            span("t", vec![text("before")]),
            text(" "),
            span("i", vec![text("alpha beta gamma")]),
            text(" after"),
        ],
    );
    lay_out(
        &tree,
        &format!("{CSS} .i {{ display: {display}; width: 60px; }}"),
    )
}

#[test]
fn a_flex_or_grid_box_of_wrapped_text_sits_on_its_first_line_and_a_block_on_its_last() {
    for (display, text_top) in [
        ("inline-flex", 0.0),
        ("inline-grid", 0.0),
        ("inline-block", 44.0),
    ] {
        let laid = wrapped_text_beside(display);
        rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 66.0), display);
        rect_near(laid.rect("t"), (0.0, text_top, 49.31, 22.0), display);
        rect_near(laid.rect("i"), (53.47, 0.0, 60.0, 66.0), display);
    }
}

#[test]
fn a_line_of_atomic_boxes_alone_is_as_tall_as_its_strut_makes_it() {
    for display in ["inline-block", "inline-flex"] {
        let tree = div("o", vec![span("i", vec![])]);
        let css = format!(
            ".o {{ width: 200px; font-size: 16px; }} \
             .i {{ display: {display}; width: 40px; height: 20px; }}"
        );
        let laid = lay_out(&tree, &css);
        rect_near(laid.rect("o"), (0.0, 0.0, 200.0, 25.0), display);
        rect_near(laid.rect("i"), (0.0, 0.0, 40.0, 20.0), display);
    }
}
