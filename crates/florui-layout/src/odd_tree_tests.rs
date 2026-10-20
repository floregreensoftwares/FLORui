//! Trees nobody writes on purpose: layout must come back with finite boxes for them.

use florui::prelude::Element;

use crate::test_support::*;

fn el(tag: &'static str, attrs: &[(&str, &str)], children: Vec<Element>) -> Element {
    Element::node(
        tag,
        attrs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect(),
        children,
    )
}

#[track_caller]
fn lays_out_finite(tree: &Element, css: &str) {
    let laid = lay_out(tree, css);
    for (id, layout) in &laid.layouts {
        for value in [layout.x, layout.y, layout.width, layout.height] {
            assert!(
                value.is_finite(),
                "node {id:#x} has a box that is not finite"
            );
        }
    }
}

const CSS: &str = "";

#[test]
fn tables_with_nothing_or_only_a_part_in_them() {
    for children in [
        vec![],
        vec![el("caption", &[], vec![text("only")])],
        vec![el(
            "colgroup",
            &[],
            vec![el("col", &[("span", "3")], vec![])],
        )],
        vec![el("tr", &[], vec![])],
        vec![el("tbody", &[], vec![el("tr", &[], vec![])])],
        vec![el(
            "tr",
            &[],
            vec![el("td", &[("style", "display: none")], vec![])],
        )],
        vec![text("loose text"), el("td", &[], vec![text("loose cell")])],
    ] {
        lays_out_finite(&div("p", vec![el("table", &[], children)]), CSS);
    }
}

#[test]
fn spans_that_are_zero_huge_or_not_numbers() {
    for (colspan, rowspan) in [
        ("0", "0"),
        ("1000", "65534"),
        ("99999", "99999"),
        ("x", "-3"),
        ("", ""),
    ] {
        let cell = el(
            "td",
            &[("colspan", colspan), ("rowspan", rowspan)],
            vec![text("c")],
        );
        let tree = el(
            "table",
            &[],
            vec![
                el("tr", &[], vec![cell, el("td", &[], vec![text("d")])]),
                el("tr", &[], vec![el("td", &[], vec![text("e")])]),
            ],
        );
        lays_out_finite(&div("p", vec![tree]), CSS);
    }
}

#[test]
fn a_table_whose_parts_are_display_contents_or_none_or_floated() {
    let tree = el(
        "table",
        &[],
        vec![el(
            "tbody",
            &[("class", "c")],
            vec![
                el(
                    "tr",
                    &[("class", "c")],
                    vec![el("td", &[("class", "c")], vec![text("a")])],
                ),
                el(
                    "tr",
                    &[("class", "n")],
                    vec![el("td", &[], vec![text("b")])],
                ),
                el(
                    "tr",
                    &[],
                    vec![el("td", &[("class", "f")], vec![text("c")])],
                ),
            ],
        )],
    );
    let css = ".c { display: contents; } .n { display: none; } .f { float: left; }";
    lays_out_finite(&div("p", vec![tree]), css);
}

#[test]
fn floats_in_cells_in_floats_with_clear_and_no_width() {
    let inner = el(
        "table",
        &[("class", "f")],
        vec![el(
            "tr",
            &[],
            vec![el(
                "td",
                &[],
                vec![
                    div("f z", vec![text("deep")]),
                    div("k", vec![]),
                    text("after"),
                ],
            )],
        )],
    );
    let tree = div(
        "p",
        vec![div("f", vec![inner]), div("k", vec![text("below")])],
    );
    let css = ".f { float: left; } .z { width: 0; height: 0; } .k { clear: both; }";
    lays_out_finite(&tree, css);
}

#[test]
fn tables_nested_deeply() {
    let mut tree = el("td", &[], vec![text("core")]);
    for _ in 0..12 {
        tree = el(
            "td",
            &[],
            vec![el("table", &[], vec![el("tr", &[], vec![tree])])],
        );
    }
    lays_out_finite(
        &div("p", vec![el("table", &[], vec![el("tr", &[], vec![tree])])]),
        CSS,
    );
}

#[test]
fn list_items_that_are_empty_inside_and_in_cells() {
    let item = || el("li", &[("class", "i")], vec![]);
    let tree = div(
        "p",
        vec![
            el("ol", &[("start", "-99999999999")], vec![item(), item()]),
            el(
                "ul",
                &[],
                vec![el("li", &[("class", "i")], vec![div("f", vec![])])],
            ),
            el(
                "table",
                &[],
                vec![el(
                    "tr",
                    &[],
                    vec![el("td", &[("class", "i")], vec![text("x")])],
                )],
            ),
        ],
    );
    let css = ".i { display: list-item; list-style-position: inside; } .f { float: left; } li { counter-increment: list-item 2147483647; }";
    lays_out_finite(&tree, css);
}
