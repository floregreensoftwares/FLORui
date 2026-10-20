//! `display: contents`: no box, and the children take part in the formatting
//! context of the parent. Chrome 153's numbers.

use crate::test_support::*;

#[test]
fn the_children_of_a_contents_wrapper_become_flex_items() {
    let tree = div(
        "fl",
        vec![
            div("c", vec![div("a", vec![]), div("b", vec![])]),
            div("z", vec![]),
        ],
    );
    let css = ".fl { display: flex; width: 300px; } .c { display: contents; } \
               .a { width: 50px; height: 10px; } .b { width: 60px; height: 10px; } \
               .z { width: 70px; height: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("fl"), (0.0, 0.0, 300.0, 10.0), "flex");
    rect_near(laid.rect("c"), (0.0, 0.0, 0.0, 0.0), "wrapper");
    rect_near(laid.rect("a"), (0.0, 0.0, 50.0, 10.0), "a");
    rect_near(laid.rect("b"), (50.0, 0.0, 60.0, 10.0), "b");
    rect_near(laid.rect("z"), (110.0, 0.0, 70.0, 10.0), "z");
}

#[test]
fn items_inside_and_outside_a_contents_wrapper_are_ordered_together() {
    let tree = div(
        "fl",
        vec![
            div("c", vec![div("a", vec![]), div("b", vec![])]),
            div("z", vec![]),
        ],
    );
    let css = ".fl { display: flex; width: 300px; } .c { display: contents; } \
               .a { order: 2; width: 50px; height: 10px; } \
               .b { order: -1; width: 60px; height: 10px; } \
               .z { width: 70px; height: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("b"), (0.0, 0.0, 60.0, 10.0), "b first");
    rect_near(laid.rect("z"), (60.0, 0.0, 70.0, 10.0), "z next");
    rect_near(laid.rect("a"), (130.0, 0.0, 50.0, 10.0), "a last");
}

#[test]
fn contents_nest_to_any_depth() {
    let tree = div(
        "fl",
        vec![div(
            "c",
            vec![div("c", vec![div("a", vec![])]), div("b", vec![])],
        )],
    );
    let css = ".fl { display: flex; width: 300px; gap: 5px; } .c { display: contents; } \
               .a { width: 20px; height: 10px; } .b { width: 30px; height: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("a"), (0.0, 0.0, 20.0, 10.0), "a");
    rect_near(laid.rect("b"), (25.0, 0.0, 30.0, 10.0), "b");
}

#[test]
fn the_box_properties_of_a_contents_element_do_nothing() {
    let tree = div("fl", vec![div("c", vec![div("a", vec![])])]);
    let css = ".fl { display: flex; width: 300px; } \
               .c { display: contents; margin: 20px; padding: 20px; border: 5px solid red; \
                    width: 500px; overflow: hidden; transform: translateX(50px); } \
               .a { width: 20px; height: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("fl"), (0.0, 0.0, 300.0, 10.0), "flex");
    rect_near(laid.rect("a"), (0.0, 0.0, 20.0, 10.0), "a");
}

#[test]
fn text_in_a_contents_wrapper_is_an_anonymous_grid_item() {
    let tree = div("g", vec![div("c", vec![text("txt"), div("a", vec![])])]);
    let css = ".g { display: grid; grid-template-columns: 60px 60px; width: 300px; } \
               .c { display: contents; } .a { height: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("g"), (0.0, 0.0, 300.0, 22.0), "grid");
    rect_near(laid.rect("a"), (60.0, 0.0, 60.0, 10.0), "a");
}

#[test]
fn the_text_of_a_contents_inline_flows_with_its_neighbours() {
    let tree = div(
        "p",
        vec![
            text("aaa "),
            span("c", vec![span("s", vec![text("bbb")]), text(" ccc")]),
            text(" ddd"),
        ],
    );
    let css = ".p { width: 300px; font-size: 16px; line-height: 20px; } \
               .c { display: contents; border: 5px solid red; padding: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("p"), (0.0, 0.0, 300.0, 20.0), "paragraph");
    rect_near(laid.rect("c"), (0.0, 0.0, 0.0, 0.0), "contents");
    rect_near(laid.rect("s"), (30.83, -1.0, 29.38, 22.0), "inline in it");
}

#[test]
fn the_text_of_a_contents_inline_wraps_with_its_neighbours() {
    let tree = div(
        "p",
        vec![
            text("aaa bbb "),
            span(
                "c",
                vec![span("s", vec![text("ccc ddd")]), text(" eee fff")],
            ),
            text(" ggg"),
        ],
    );
    let css = ".p { width: 100px; font-size: 16px; line-height: 20px; } .c { display: contents; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("p"), (0.0, 0.0, 100.0, 60.0), "paragraph");
    rect_near(laid.rect("s"), (0.0, -1.0, 87.36, 42.0), "inline in it");
}

#[test]
fn contents_on_a_control_or_a_replaced_element_acts_as_none() {
    for tag in ["img", "input", "textarea", "select", "br", "hr"] {
        let tree = div(
            "w",
            vec![div(
                "p",
                vec![text("x"), element(tag, "e", vec![]), text("y")],
            )],
        );
        let css = ".p { display: inline-block; font-size: 16px; line-height: 20px; } \
                   .e { display: contents; }";
        let laid = lay_out(&tree, css);
        let (_, _, width, height) = laid.rect("p");
        near(width, 16.39, 0.5, tag);
        near(height, 20.0, 0.5, tag);
    }
}

#[test]
fn the_text_of_a_contents_element_takes_its_style_and_a_button_is_no_exception() {
    for (tag, style, width, height) in [
        ("span", "font-size: 30px; font-weight: 700;", 61.48, 41.0),
        ("button", "font: inherit;", 38.91, 22.0),
    ] {
        let tree = div(
            "w",
            vec![div(
                "p",
                vec![text("x"), element(tag, "e", vec![text("big")]), text("y")],
            )],
        );
        let css = format!(
            ".p {{ display: inline-block; font-size: 16px; line-height: normal; }} \
             .e {{ display: contents; {style} }}"
        );
        let laid = lay_out(&tree, &css);
        let (_, _, w, h) = laid.rect("p");
        near(w, width, 0.5, tag);
        near(h, height, 0.5, tag);
    }
}

#[test]
fn text_and_a_block_in_a_contents_element_split_the_line_in_the_parent() {
    let tree = div(
        "w",
        vec![
            text("a"),
            span("c", vec![text("b"), div("blk", vec![]), text("e")]),
        ],
    );
    let css = ".w { width: 200px; font-size: 16px; line-height: normal; } \
               .c { display: contents; font-size: 30px; } .blk { height: 10px; }";
    let laid = lay_out(&tree, css);
    rect_near(laid.rect("w"), (0.0, 0.0, 200.0, 92.0), "container");
    rect_near(laid.rect("blk"), (0.0, 41.0, 200.0, 10.0), "block");
}

#[test]
fn a_child_of_a_contents_wrapper_is_hit_and_the_wrapper_never_is() {
    let tree = div("fl", vec![div("c", vec![div("a", vec![])])]);
    let css = ".fl { display: flex; width: 100px; height: 40px; } \
               .c { display: contents; overflow: hidden; } \
               .a { width: 30px; height: 30px; }";
    let laid = lay_out(&tree, css);
    let (a, c) = (laid.node("a"), laid.node("c"));
    assert_eq!(
        crate::hit_test(&laid.arena, &laid.layouts, &laid.styles, 10.0, 10.0),
        Some(a)
    );
    assert_ne!(
        crate::hit_test(&laid.arena, &laid.layouts, &laid.styles, 60.0, 10.0),
        Some(c)
    );
}

#[test]
fn a_child_of_a_contents_wrapper_scrolls_with_the_container_around_it() {
    let tree = div("s", vec![div("c", vec![div("a", vec![])])]);
    let css = ".s { width: 100px; height: 40px; overflow: auto; } \
               .c { display: contents; } .a { width: 30px; height: 100px; }";
    let laid = lay_out(&tree, css);
    let offsets = std::collections::HashMap::from([(laid.node("s"), (0.0, 25.0))]);
    let scrolled = crate::apply_scroll_offsets(&laid.arena, &laid.layouts, &offsets, &laid.styles);
    near(
        scrolled[&laid.node("a")].y,
        -25.0,
        0.01,
        "the child moved up",
    );
    near(
        scrolled[&laid.node("s")].y,
        0.0,
        0.01,
        "the container stayed",
    );
}
