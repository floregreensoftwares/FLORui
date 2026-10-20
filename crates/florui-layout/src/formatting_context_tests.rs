//! What makes a box the root of a block formatting context of its own, which
//! keeps the margin of its first child inside it. Chrome 153's numbers.

use crate::test_support::*;

fn margin_of_first_child(style: &str) -> Laid {
    let tree = div(
        "root",
        vec![div("a", vec![div("c", vec![])]), div("b", vec![])],
    );
    lay_out(
        &tree,
        &format!(
            ".root {{ width: 200px; }} .a {{ {style} }} \
             .c {{ margin-top: 20px; height: 20px; }} .b {{ height: 10px; }}"
        ),
    )
}

#[test]
fn flow_root_keeps_the_margin_of_its_first_child_inside() {
    let laid = margin_of_first_child("display: flow-root;");
    rect_near(laid.rect("a"), (0.0, 0.0, 200.0, 40.0), "flow-root");
    rect_near(laid.rect("c"), (0.0, 20.0, 200.0, 20.0), "child");
    rect_near(laid.rect("b"), (0.0, 40.0, 200.0, 10.0), "next box");
}

#[test]
fn a_block_lets_the_margin_of_its_first_child_through() {
    let laid = margin_of_first_child("");
    rect_near(laid.rect("a"), (0.0, 20.0, 200.0, 20.0), "block");
    rect_near(laid.rect("c"), (0.0, 20.0, 200.0, 20.0), "child");
    rect_near(laid.rect("b"), (0.0, 40.0, 200.0, 10.0), "next box");
}

#[test]
fn overflow_hidden_and_auto_make_a_context_and_clip_and_visible_do_not() {
    for (overflow, contained) in [
        ("hidden", true),
        ("auto", true),
        ("clip", false),
        ("visible", false),
    ] {
        let laid = margin_of_first_child(&format!("overflow: {overflow};"));
        let expected = if contained {
            (0.0, 0.0, 200.0, 40.0)
        } else {
            (0.0, 20.0, 200.0, 20.0)
        };
        rect_near(laid.rect("a"), expected, overflow);
        rect_near(laid.rect("c"), (0.0, 20.0, 200.0, 20.0), overflow);
    }
}

#[test]
fn a_scroll_container_flex_item_can_shrink_below_its_content() {
    for (overflow, width) in [("visible", 150.0), ("hidden", 60.0)] {
        let tree = div(
            "row",
            vec![div("a", vec![div("wide", vec![])]), div("b", vec![])],
        );
        let laid = lay_out(
            &tree,
            &format!(
                ".row {{ display: flex; width: 100px; }} .a {{ overflow: {overflow}; }} \
                 .wide {{ width: 150px; height: 10px; }} \
                 .b {{ width: 40px; height: 10px; flex: none; }}"
            ),
        );
        rect_near(laid.rect("a"), (0.0, 0.0, width, 10.0), overflow);
    }
}
