//! `<br>`, a line break in the lines of the text around it. Chrome 153's numbers (Open Sans
//! 16px on a line of 20px, 200px wide).

use florui::prelude::Element;

use crate::test_support::*;

const CSS: &str = "body { width: 400px; } .p { width: 200px; font-size: 16px; line-height: 20px; }";

fn br() -> Element {
    element("br", "", vec![])
}

fn height_of(children: Vec<Element>, css: &str) -> f32 {
    let tree = div("p", children);
    lay_out(&tree, &format!("{CSS} {css}")).rect("p").3
}

#[test]
fn a_br_ends_the_line_it_is_in() {
    near(
        height_of(vec![text("a"), br(), text("b")], ""),
        40.0,
        0.01,
        "a, br, b",
    );
    near(
        height_of(vec![text("a"), br(), br(), text("b")], ""),
        60.0,
        0.01,
        "two brs",
    );
    near(
        height_of(vec![text("a "), br(), text(" b")], ""),
        40.0,
        0.01,
        "spaces around",
    );
}

#[test]
fn a_br_at_the_end_does_not_start_another_line_and_a_lone_one_is_a_line() {
    near(height_of(vec![text("a"), br()], ""), 20.0, 0.01, "trailing");
    near(height_of(vec![br()], ""), 20.0, 0.01, "alone");
}

#[test]
fn the_text_after_a_br_wraps_as_a_line_of_its_own() {
    near(
        height_of(
            vec![
                text("ab"),
                br(),
                text("cde fgh ijk lmn opq rst uvw xyz abc def"),
            ],
            "",
        ),
        60.0,
        0.01,
        "wrap",
    );
}

#[test]
fn a_br_in_preformatted_text_and_with_a_font_of_its_own() {
    near(
        height_of(
            vec![text("a"), br(), text("\nb")],
            ".p { white-space: pre; }",
        ),
        60.0,
        0.01,
        "pre",
    );
    near(
        height_of(
            vec![
                text("a"),
                element("span", "big", vec![text("b"), br()]),
                text("c"),
            ],
            ".big { font-size: 40px; }",
        ),
        49.0,
        0.5,
        "br in a span with a font of its own",
    );
}

#[test]
fn a_line_break_at_the_end_of_preformatted_text_does_not_start_another_line() {
    for white_space in ["pre", "pre-line", "pre-wrap"] {
        let css = format!(".p {{ white-space: {white_space}; }}");
        near(height_of(vec![text("a\n")], &css), 20.0, 0.01, white_space);
        near(
            height_of(vec![text("a\n\n")], &css),
            40.0,
            0.01,
            white_space,
        );
    }
}
