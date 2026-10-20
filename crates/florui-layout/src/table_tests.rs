//! Tables. Chrome 153's numbers for the same trees (Open Sans 16px, the cell
//! padding and the border spacing of the default style sheet).

use florui::prelude::Element;

use crate::test_support::*;

fn table(class: &str, children: Vec<Element>) -> Element {
    element("table", class, children)
}

fn tr(class: &str, children: Vec<Element>) -> Element {
    element("tr", class, children)
}

fn td(class: &str, children: Vec<Element>) -> Element {
    element("td", class, children)
}

fn with_attr(
    tag: &'static str,
    class: &str,
    attrs: &[(&str, &str)],
    children: Vec<Element>,
) -> Element {
    let mut list = vec![("class".to_string(), class.to_string())];
    list.extend(
        attrs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
    );
    Element::node(tag, list, children)
}

fn page(table: Element) -> Element {
    div("page", vec![table])
}

const CSS: &str = ".page { width: 400px; }";

#[track_caller]
fn close(laid: &Laid, class: &str, expected: (f32, f32, f32, f32)) {
    let actual = laid.rect(class);
    for (name, a, e) in [
        ("x", actual.0, expected.0),
        ("y", actual.1, expected.1),
        ("width", actual.2, expected.2),
        ("height", actual.3, expected.3),
    ] {
        near(a, e, 0.05, &format!("{class} {name}"));
    }
}

#[test]
fn a_cell_that_states_a_width_keeps_it_and_its_text_wraps_to_it() {
    // Chrome: the table is 118.38 wide, the cell 102 (100 and its padding).
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td(
                    "a",
                    vec![text("aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll")],
                ),
                td("b", vec![text("x")]),
            ],
        )],
    ));
    let css = format!("{CSS} .a {{ width: 100px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 118.38, 94.0));
    close(&laid, "a", (2.0, 2.0, 102.0, 90.0));
    close(&laid, "b", (106.0, 2.0, 10.38, 90.0));
}

#[test]
fn a_table_with_a_width_shares_it_among_auto_columns_by_how_wide_they_would_be() {
    // Chrome: 66.31 and 227.69 in 300.
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![
                    td("a", vec![text("aaa")]),
                    td("b", vec![text("bbb bbb bbb")]),
                ],
            ),
            tr(
                "r2",
                vec![td("c", vec![text("c")]), td("d", vec![text("d")])],
            ),
        ],
    ));
    let css = format!("{CSS} .t {{ width: 300px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 300.0, 54.0));
    close(&laid, "a", (2.0, 2.0, 66.31, 24.0));
    close(&laid, "b", (70.31, 2.0, 227.69, 24.0));
    close(&laid, "c", (2.0, 28.0, 66.31, 24.0));
}

#[test]
fn a_column_with_a_fixed_width_keeps_it_when_the_table_is_wider() {
    // Chrome: 102 / 149.95 / 40.05 in 300.
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td("a", vec![text("aaa")]),
                td("b", vec![text("bbb bbb")]),
                td("c", vec![text("cc")]),
            ],
        )],
    ));
    let css = format!("{CSS} .t {{ width: 300px; }} .a {{ width: 100px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "a", (2.0, 2.0, 102.0, 24.0));
    close(&laid, "b", (106.0, 2.0, 149.95, 24.0));
    close(&laid, "c", (257.95, 2.0, 40.05, 24.0));
}

#[test]
fn a_percentage_column_is_that_share_of_the_columns() {
    // Chrome: 30% of 294, and in an auto table the width that makes it so.
    let make = || {
        page(table(
            "t",
            vec![tr(
                "r",
                vec![td("a", vec![text("aaa")]), td("b", vec![text("bbb bbb")])],
            )],
        ))
    };
    let css = format!("{CSS} .t {{ width: 300px; }} .a {{ width: 30%; }}");
    let laid = lay_out(&make(), &css);
    close(&laid, "a", (2.0, 2.0, 88.19, 24.0));
    close(&laid, "b", (92.19, 2.0, 205.81, 24.0));
    let css = format!("{CSS} .a {{ width: 30%; }}");
    let laid = lay_out(&make(), &css);
    close(&laid, "t", (0.0, 0.0, 101.56, 28.0));
    close(&laid, "a", (2.0, 2.0, 28.67, 24.0));
    close(&laid, "b", (32.67, 2.0, 66.89, 24.0));
}

#[test]
fn a_cell_across_columns_widens_them_by_how_wide_they_are() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
            ),
            tr(
                "r2",
                vec![with_attr(
                    "td",
                    "w",
                    &[("colspan", "2")],
                    vec![text("wide wide wide wide wide wide")],
                )],
            ),
        ],
    ));
    let laid = lay_out(&tree, CSS);
    close(&laid, "t", (0.0, 0.0, 238.05, 54.0));
    close(&laid, "a", (2.0, 2.0, 111.38, 24.0));
    close(&laid, "b", (115.38, 2.0, 120.67, 24.0));
    close(&laid, "w", (2.0, 28.0, 234.05, 24.0));
}

#[test]
fn a_cell_across_rows_gives_them_the_height_it_needs() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![
                    with_attr(
                        "td",
                        "s",
                        &[("rowspan", "2")],
                        vec![
                            div("", vec![text("r")]),
                            div("", vec![text("r")]),
                            div("", vec![text("r")]),
                        ],
                    ),
                    td("a", vec![text("a")]),
                ],
            ),
            tr("r2", vec![td("b", vec![text("b")])]),
        ],
    ));
    let laid = lay_out(&tree, CSS);
    close(&laid, "s", (2.0, 2.0, 8.55, 68.0));
    close(&laid, "r1", (2.0, 2.0, 22.34, 33.0));
    close(&laid, "b", (12.55, 37.0, 11.8, 33.0));
}

#[test]
fn a_fixed_table_takes_its_columns_from_its_first_row() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![
                    td("a", vec![text("a")]),
                    td("b", vec![text("b")]),
                    td("c", vec![text("cccccccccccccccccccccccccccc")]),
                ],
            ),
            tr(
                "r2",
                vec![
                    td("d", vec![text("1")]),
                    td("e", vec![text("2")]),
                    td("f", vec![text("3")]),
                ],
            ),
        ],
    ));
    let css = format!(
        "{CSS} .t {{ table-layout: fixed; width: 300px; }} .a {{ width: 50px; }} \
         .e {{ width: 200px; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "a", (2.0, 2.0, 52.0, 24.0));
    close(&laid, "b", (56.0, 2.0, 120.0, 24.0));
    close(&laid, "c", (178.0, 2.0, 120.0, 24.0));
    close(&laid, "e", (56.0, 28.0, 120.0, 24.0));
}

#[test]
fn a_table_with_a_height_shares_the_rest_among_its_rows_by_their_height() {
    let tree = page(table(
        "t",
        vec![
            tr("r1", vec![td("a", vec![text("a")])]),
            tr(
                "r2",
                vec![td(
                    "b",
                    vec![div("", vec![text("b")]), div("", vec![text("b")])],
                )],
            ),
        ],
    ));
    let css = format!("{CSS} .t {{ height: 200px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 15.8, 200.0));
    close(&laid, "r1", (2.0, 2.0, 11.8, 66.5));
    close(&laid, "r2", (2.0, 70.5, 11.8, 127.5));
}

#[test]
fn a_table_border_and_padding_surround_its_grid() {
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
        )],
    ));
    let css = format!("{CSS} .t {{ border: 5px solid #000; padding: 3px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 44.69, 44.0));
    close(&laid, "a", (10.0, 10.0, 10.89, 24.0));
}

#[test]
fn a_column_is_as_wide_as_the_least_its_content_can_be() {
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td("a", vec![text("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")]),
                td("b", vec![text("b b b b b b")]),
            ],
        )],
    ));
    let css = ".page { width: 100px; } .b { white-space: nowrap; }";
    let laid = lay_out(&tree, css);
    close(&laid, "t", (0.0, 0.0, 391.8, 28.0));
    close(&laid, "a", (2.0, 2.0, 304.28, 24.0));
    close(&laid, "b", (308.28, 2.0, 81.52, 24.0));
}

#[test]
fn a_caption_goes_above_or_below_the_grid_across_the_width_of_the_table() {
    let make = |caption_side: &str| {
        let tree = page(table(
            "t",
            vec![
                element("caption", "c", vec![text("Title text")]),
                tr(
                    "r",
                    vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
                ),
            ],
        ));
        let css = format!(
            "{CSS} .t {{ border: 5px solid #000; }} .c {{ caption-side: {caption_side}; }}"
        );
        lay_out(&tree, &css)
    };
    let top = make("top");
    close(&top, "t", (0.0, 0.0, 38.69, 82.0));
    close(&top, "c", (0.0, 0.0, 38.69, 44.0));
    close(&top, "a", (7.0, 51.0, 10.89, 24.0));
    let bottom = make("bottom");
    close(&bottom, "c", (0.0, 38.0, 38.69, 44.0));
    close(&bottom, "a", (7.0, 7.0, 10.89, 24.0));
}

#[test]
fn a_table_with_no_cell_has_no_spacing() {
    let tree = page(div(
        "",
        vec![
            table("e1", vec![]),
            table("e4", vec![tr("", vec![])]),
            table("e5", vec![tr("", vec![td("c", vec![])])]),
        ],
    ));
    let laid = lay_out(&tree, CSS);
    close(&laid, "e1", (0.0, 0.0, 0.0, 0.0));
    close(&laid, "e4", (0.0, 0.0, 0.0, 0.0));
    close(&laid, "e5", (0.0, 0.0, 6.0, 6.0));
    close(&laid, "c", (2.0, 2.0, 2.0, 2.0));
}

#[test]
fn border_spacing_goes_between_the_cells_and_around_them() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
            ),
            tr(
                "r2",
                vec![td("c", vec![text("c")]), td("d", vec![text("d")])],
            ),
        ],
    ));
    let laid = lay_out(&tree, &format!("{CSS} .t {{ border-spacing: 10px 5px; }}"));
    close(&laid, "t", (0.0, 0.0, 52.69, 63.0));
    close(&laid, "a", (10.0, 5.0, 10.89, 24.0));
    close(&laid, "b", (30.89, 5.0, 11.8, 24.0));
    close(&laid, "c", (10.0, 34.0, 10.89, 24.0));
}

#[test]
fn the_header_group_is_first_and_the_footer_last_wherever_they_are_written() {
    let tree = page(table(
        "t",
        vec![
            element(
                "tfoot",
                "tf",
                vec![tr("", vec![td("f", vec![text("foot")])])],
            ),
            element(
                "tbody",
                "tb",
                vec![tr("", vec![td("b", vec![text("body")])])],
            ),
            element(
                "thead",
                "th",
                vec![tr("", vec![td("h", vec![text("head")])])],
            ),
        ],
    ));
    let laid = lay_out(&tree, CSS);
    close(&laid, "t", (0.0, 0.0, 43.48, 80.0));
    close(&laid, "th", (2.0, 2.0, 39.48, 24.0));
    close(&laid, "tb", (2.0, 28.0, 39.48, 24.0));
    close(&laid, "tf", (2.0, 54.0, 39.48, 24.0));
}

#[test]
fn a_row_that_collapses_takes_no_room_but_its_cells_still_size_the_columns() {
    let tree = page(table(
        "t",
        vec![
            tr("r1", vec![td("a", vec![text("a")])]),
            tr(
                "r2",
                vec![td("b", vec![text("zzzzzzzzzzzzzzzzzzzzzzzzzz")])],
            ),
            tr("r3", vec![td("c", vec![text("b")])]),
        ],
    ));
    let laid = lay_out(&tree, &format!("{CSS} .r2 {{ visibility: collapse; }}"));
    close(&laid, "t", (0.0, 0.0, 201.0, 54.0));
    close(&laid, "r2", (2.0, 26.0, 197.0, 0.0));
    close(&laid, "r3", (2.0, 28.0, 197.0, 24.0));
}

#[test]
fn boxes_that_are_not_parts_of_a_table_are_laid_out_in_cells_made_around_them() {
    let tree = div(
        "t",
        vec![
            div(
                "r",
                vec![
                    div("c1", vec![text("a")]),
                    div("p", vec![text("plain")]),
                    div("c2", vec![text("b")]),
                ],
            ),
            div("l", vec![text("loose")]),
            div("c3", vec![text("cellonly")]),
        ],
    );
    let css =
        ".t { display: table; } .r { display: table-row; } .c1, .c2, .c3 { display: table-cell; }";
    let laid = lay_out(&tree, css);
    close(&laid, "t", (0.0, 0.0, 105.92, 44.0));
    close(&laid, "r", (0.0, 0.0, 105.92, 22.0));
    close(&laid, "c1", (0.0, 0.0, 39.91, 22.0));
    close(&laid, "p", (39.91, 0.0, 56.22, 22.0));
    close(&laid, "c2", (96.13, 0.0, 9.8, 22.0));
    close(&laid, "l", (0.0, 22.0, 39.91, 22.0));
    close(&laid, "c3", (39.91, 22.0, 56.22, 22.0));
}

#[test]
fn text_directly_in_a_table_is_in_a_cell_of_its_own() {
    let tree = div("t", vec![text("direct text")]);
    let laid = lay_out(&tree, ".t { display: table; }");
    close(&laid, "t", (0.0, 0.0, 75.64, 22.0));
}

#[test]
fn a_cell_stated_wider_than_the_rest_of_its_column_does_not_widen_it_to_the_others() {
    // Chrome: the column is 52 wide, the long text wraps in it.
    let tree = page(table(
        "t",
        vec![
            tr("r1", vec![td("a", vec![text("a")])]),
            tr(
                "r2",
                vec![td("b", vec![text("quite a few words in this cell here")])],
            ),
        ],
    ));
    let laid = lay_out(&tree, &format!("{CSS} .a {{ width: 50px; }}"));
    close(&laid, "t", (0.0, 0.0, 56.0, 164.0));
    close(&laid, "b", (2.0, 28.0, 52.0, 134.0));
    // Two stated widths: the wider.
    let tree = page(table(
        "t",
        vec![
            tr("r1", vec![td("a", vec![text("a")])]),
            tr("r2", vec![td("b", vec![text("b")])]),
        ],
    ));
    let laid = lay_out(
        &tree,
        &format!("{CSS} .a {{ width: 50px; }} .b {{ width: 80px; }}"),
    );
    close(&laid, "t", (0.0, 0.0, 86.0, 54.0));
}

#[test]
fn percentages_that_add_up_to_more_than_the_whole_leave_the_later_columns_what_is_left() {
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td("a", vec![text("a")]),
                td("b", vec![text("b")]),
                td("c", vec![text("c")]),
            ],
        )],
    ));
    let css = format!("{CSS} .t {{ width: 300px; }} .a {{ width: 70%; }} .b {{ width: 50%; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "a", (2.0, 2.0, 197.44, 24.0));
    close(&laid, "b", (201.44, 2.0, 84.89, 24.0));
    close(&laid, "c", (288.33, 2.0, 9.67, 24.0));
}

#[test]
fn a_cell_across_columns_with_a_width_asks_the_columns_for_it() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
            ),
            tr(
                "r2",
                vec![with_attr("td", "c", &[("colspan", "2")], vec![text("c")])],
            ),
        ],
    ));
    let laid = lay_out(&tree, &format!("{CSS} .c {{ width: 200px; }}"));
    close(&laid, "t", (0.0, 0.0, 206.0, 54.0));
    close(&laid, "a", (2.0, 2.0, 96.0, 24.0));
    close(&laid, "b", (100.0, 2.0, 104.0, 24.0));
    close(&laid, "c", (2.0, 28.0, 202.0, 24.0));
}

#[test]
fn the_margins_of_what_is_in_a_cell_stay_in_it() {
    let tree = page(table(
        "t",
        vec![tr("r", vec![td("c", vec![div("d", vec![])])])],
    ));
    let laid = lay_out(
        &tree,
        &format!("{CSS} .d {{ margin: 5px 7px; width: 30px; height: 10px; }}"),
    );
    close(&laid, "t", (0.0, 0.0, 50.0, 26.0));
    close(&laid, "c", (2.0, 2.0, 46.0, 22.0));
    close(&laid, "d", (10.0, 8.0, 30.0, 10.0));
}

#[test]
fn a_row_height_and_a_cell_height_are_the_least_they_state() {
    let tree = page(table(
        "t",
        vec![
            tr("r1", vec![td("a", vec![text("a")])]),
            tr("r2", vec![td("b", vec![text("b")])]),
        ],
    ));
    let laid = lay_out(
        &tree,
        &format!("{CSS} .r1 {{ height: 50px; }} .b {{ height: 40px; }}"),
    );
    close(&laid, "t", (0.0, 0.0, 15.8, 98.0));
    close(&laid, "r1", (2.0, 2.0, 11.8, 50.0));
    close(&laid, "r2", (2.0, 54.0, 11.8, 42.0));
}

#[test]
fn a_caption_has_margins_and_padding_of_its_own_and_the_grid_goes_below_them() {
    let tree = page(table(
        "t",
        vec![
            element("caption", "c", vec![text("cap")]),
            tr("r", vec![td("a", vec![text("a")])]),
        ],
    ));
    let laid = lay_out(
        &tree,
        &format!("{CSS} .t {{ width: 200px; }} .c {{ margin: 10px 5px; padding: 3px; }}"),
    );
    close(&laid, "t", (0.0, 0.0, 200.0, 76.0));
    close(&laid, "c", (5.0, 10.0, 190.0, 28.0));
    close(&laid, "a", (2.0, 50.0, 196.0, 24.0));
}

#[test]
fn columns_and_column_groups_state_widths_and_have_the_boxes_of_their_columns() {
    let tree = page(table(
        "t",
        vec![
            element(
                "colgroup",
                "g",
                vec![
                    with_attr("col", "c1", &[], vec![]),
                    with_attr("col", "c2", &[("span", "2")], vec![]),
                ],
            ),
            tr(
                "r",
                vec![
                    td("a", vec![text("a")]),
                    td("b", vec![text("b")]),
                    td("c", vec![text("c")]),
                    td("d", vec![text("d")]),
                ],
            ),
        ],
    ));
    let laid = lay_out(
        &tree,
        &format!("{CSS} .c1 {{ width: 100px; }} .c2 {{ width: 50px; }}"),
    );
    close(&laid, "t", (0.0, 0.0, 221.8, 28.0));
    close(&laid, "g", (2.0, 2.0, 204.0, 24.0));
    close(&laid, "c1", (2.0, 2.0, 100.0, 24.0));
    close(&laid, "c2", (104.0, 2.0, 102.0, 24.0));
    close(&laid, "a", (2.0, 2.0, 100.0, 24.0));
    close(&laid, "d", (208.0, 2.0, 11.8, 24.0));
}

#[test]
fn cells_in_a_row_sit_on_the_top_the_middle_the_bottom_or_the_baseline_they_share() {
    // Chrome: the row is as tall as the cell of three lines; the cell that
    // sits on the baseline is 15px lower than the top because the one with the
    // larger font has the baseline lower.
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td(
                    "a",
                    vec![
                        div("", vec![text("t")]),
                        div("", vec![text("t")]),
                        div("", vec![text("t")]),
                    ],
                ),
                td("b", vec![text("mid")]),
                td("c", vec![text("bot")]),
                td("d", vec![text("base")]),
                td("e", vec![text("B")]),
            ],
        )],
    ));
    let css = format!(
        "{CSS} .a {{ vertical-align: top; }} .c {{ vertical-align: bottom; }} \
         .d, .e {{ vertical-align: baseline; }} .e {{ font-size: 30px; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "r", (2.0, 2.0, 132.16, 68.0));
    for class in ["a", "b", "c", "d", "e"] {
        near(laid.rect(class).3, 68.0, 0.05, class);
    }
    // Where the content starts in the cell: the padding and what is above.
    let shift = |class: &str| crate::cell_shift(&laid.layouts, laid.node(class));
    near(shift("a"), 0.0, 0.05, "top");
    near(shift("b"), 22.0, 0.05, "middle");
    near(shift("c"), 44.0, 0.05, "bottom");
    near(shift("d"), 15.0, 0.05, "baseline");
    near(shift("e"), 0.0, 0.05, "the one with the baseline lowest");
}

#[test]
fn a_row_with_cells_on_the_baseline_is_as_tall_as_what_is_above_and_below_it() {
    // Chrome: 57.
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td("a", vec![text("a")]),
                td("b", vec![text("B")]),
                td("c", vec![text("c")]),
            ],
        )],
    ));
    let css = format!(
        "{CSS} td {{ vertical-align: baseline; }} .a {{ font-size: 10px; }} \
         .b {{ font-size: 40px; }} .c {{ padding-top: 20px; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "r", (2.0, 2.0, 49.08, 57.0));
    close(&laid, "a", (2.0, 2.0, 7.56, 57.0));
    close(&laid, "b", (11.56, 2.0, 27.84, 57.0));
}

#[test]
fn an_inline_table_sits_on_the_baseline_of_its_first_row() {
    let make = |css: &str| {
        let tree = page(div(
            "p",
            vec![
                text("before "),
                table("t", vec![tr("r", vec![td("a", vec![text("a")])])]),
                text(" after"),
            ],
        ));
        lay_out(
            &tree,
            &format!("{CSS} .t {{ display: inline-table; }} {css}"),
        )
    };
    // The cell is in the middle of its row, which has no baseline of its own:
    // the table sits on the bottom of the content of its first cell.
    let laid = make("");
    close(&laid, "p", (0.0, 0.0, 400.0, 30.0));
    close(&laid, "t", (53.47, 0.0, 14.89, 28.0));
    close(&laid, "a", (55.47, 2.0, 10.89, 24.0));
    // A cell on the baseline gives it to the table.
    let laid = make("td { vertical-align: baseline; }");
    close(&laid, "p", (0.0, 0.0, 400.0, 28.0));
    close(&laid, "t", (53.47, 0.0, 14.89, 28.0));
}

#[test]
fn the_rows_and_cells_of_a_group_are_placed_from_the_group() {
    let tree = page(table(
        "t",
        vec![
            element(
                "tfoot",
                "tf",
                vec![tr("r3", vec![td("a3", vec![text("f")])])],
            ),
            element(
                "tbody",
                "tb",
                vec![tr(
                    "r2",
                    vec![td("a2", vec![text("a")]), td("b2", vec![text("b")])],
                )],
            ),
            element(
                "thead",
                "th",
                vec![tr("r1", vec![td("a1", vec![text("h")])])],
            ),
        ],
    ));
    let laid = lay_out(&tree, CSS);
    // Absolute positions from the layout; every row is as wide as the grid.
    close(&laid, "th", (2.0, 2.0, 25.61, 24.0));
    close(&laid, "r1", (2.0, 2.0, 25.61, 24.0));
    close(&laid, "a1", (2.0, 2.0, 11.81, 24.0));
    close(&laid, "r2", (2.0, 28.0, 25.61, 24.0));
    close(&laid, "b2", (15.81, 28.0, 11.8, 24.0));
    close(&laid, "r3", (2.0, 54.0, 25.61, 24.0));
}

fn two_cells(css: &str) -> Laid {
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
        )],
    ));
    lay_out(
        &tree,
        &format!("{CSS} .t {{ border-collapse: collapse; }} .a, .b {{ padding: 1px; }} {css}"),
    )
}

#[test]
fn collapsed_borders_give_each_cell_half_of_each_edge_and_the_cells_touch() {
    // Chrome: 2px borders, the cells abut and the table has the outer halves.
    let laid = two_cells(".a, .b { border: 2px solid #000; }");
    close(&laid, "t", (0.0, 0.0, 28.69, 28.0));
    close(&laid, "a", (1.0, 1.0, 12.89, 26.0));
    close(&laid, "b", (13.89, 1.0, 13.8, 26.0));
}

#[test]
fn collapsed_edges_take_the_widest_border_and_halves_may_be_fractions() {
    // 3px: 1.5 on each side.
    let laid = two_cells(".a, .b { border: 3px solid #000; }");
    close(&laid, "t", (0.0, 0.0, 31.69, 30.0));
    close(&laid, "a", (1.5, 1.5, 13.89, 27.0));
    close(&laid, "b", (15.39, 1.5, 14.8, 27.0));
    // The table's border wins the outer edge and gives its half to the cell.
    let laid = two_cells(".t { border: 6px solid #000; } .a, .b { border: 2px solid #f00; }");
    close(&laid, "t", (0.0, 0.0, 36.69, 36.0));
    close(&laid, "a", (3.0, 3.0, 14.89, 30.0));
    close(&laid, "b", (17.89, 3.0, 15.8, 30.0));
    // The wider of two cells wins the edge between them.
    let laid = two_cells(".a { border: 4px solid #000; } .b { border: 2px solid #f00; }");
    close(&laid, "t", (0.0, 0.0, 32.69, 32.0));
    close(&laid, "a", (2.0, 2.0, 14.89, 28.0));
    close(&laid, "b", (16.89, 2.0, 14.8, 28.0));
}

#[test]
fn a_hidden_border_wins_over_all_and_none_loses_to_all() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![
                    td("a1", vec![text("a")]),
                    td("b1", vec![text("b")]),
                    td("a3", vec![text("c")]),
                ],
            ),
            tr(
                "r2",
                vec![
                    td("a2", vec![text("d")]),
                    td("c", vec![text("e")]),
                    td("a4", vec![text("f")]),
                ],
            ),
        ],
    ));
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; }} td {{ padding: 1px; }} \
         .a1, .a2, .a3, .a4 {{ border: 4px solid #000; }} .b1 {{ border: 4px hidden #f00; }} \
         .c {{ border: 4px none; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 46.45, 60.0));
    close(&laid, "a1", (2.0, 2.0, 15.8, 28.0));
    close(&laid, "b1", (17.8, 2.0, 14.98, 28.0));
    close(&laid, "a3", (32.78, 2.0, 11.67, 28.0));
}

#[test]
fn collapsing_borders_ignore_the_spacing_and_the_padding_of_the_table() {
    let laid = two_cells(".t { border-spacing: 10px; padding: 7px; border: 2px solid #000; }");
    close(&laid, "t", (0.0, 0.0, 26.69, 28.0));
    close(&laid, "a", (1.0, 1.0, 11.89, 26.0));
    close(&laid, "b", (12.89, 1.0, 12.8, 26.0));
}

#[test]
fn a_cell_across_columns_takes_the_widest_edge_along_each_side() {
    // Chrome: a 5px border on the cell across both columns, 2px on the others.
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
            ),
            tr(
                "r2",
                vec![with_attr("td", "w", &[("colspan", "2")], vec![text("c")])],
            ),
        ],
    ));
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; }} td {{ padding: 1px; border: 2px solid #000; }} \
         .w {{ border: 5px solid #f00; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 31.69, 60.0));
    close(&laid, "a", (2.5, 1.0, 12.89, 27.5));
    close(&laid, "b", (15.39, 1.0, 13.8, 27.5));
    close(&laid, "w", (2.5, 28.5, 26.69, 29.0));
}

#[test]
fn the_borders_of_rows_and_columns_collapse_with_those_of_the_cells() {
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
            ),
            tr(
                "r2",
                vec![td("c", vec![text("c")]), td("d", vec![text("d")])],
            ),
        ],
    ));
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; }} td {{ padding: 1px; }} \
         tr {{ border-bottom: 6px solid #00f; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 22.69, 60.0));
    close(&laid, "r1", (0.0, 0.0, 22.69, 27.0));
    close(&laid, "r2", (0.0, 27.0, 22.69, 30.0));
    close(&laid, "c", (0.0, 27.0, 10.89, 30.0));
    // Borders on the columns.
    let tree = page(table(
        "t",
        vec![
            element(
                "colgroup",
                "",
                vec![element("col", "c1", vec![]), element("col", "c2", vec![])],
            ),
            tr(
                "r",
                vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
            ),
        ],
    ));
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; }} td {{ padding: 1px; }} \
         .c1, .c2 {{ border: 4px solid #0f0; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 34.69, 32.0));
    close(&laid, "a", (2.0, 2.0, 14.89, 28.0));
    close(&laid, "b", (16.89, 2.0, 15.8, 28.0));
}

#[test]
fn a_fixed_table_with_collapsing_borders_shares_what_is_left_of_the_width() {
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
        )],
    ));
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; table-layout: fixed; width: 200px; }} \
         td {{ border: 4px solid #000; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 200.0, 32.0));
    close(&laid, "a", (2.0, 2.0, 98.0, 28.0));
    close(&laid, "b", (100.0, 2.0, 98.0, 28.0));
}

#[test]
fn the_collapsed_borders_of_a_table_are_lines_between_its_cells() {
    // Two cells with 2px borders: three vertical lines (the middle one shared)
    // and a top and a bottom line over each cell, centered on the edges.
    let laid = two_cells(".a, .b { border: 2px solid #000; }");
    let table = laid.node("t");
    let segments =
        crate::collapsed_border_segments(&laid.arena, &laid.styles, &laid.layouts, table);
    let vertical: Vec<_> = segments.iter().filter(|s| !s.horizontal).collect();
    assert_eq!(vertical.len(), 3, "{segments:?}");
    assert!(segments.iter().all(|s| (s.width - 2.0).abs() < 0.01));
    let mut xs: Vec<f32> = vertical.iter().map(|s| s.x0).collect();
    xs.sort_by(f32::total_cmp);
    for (x, expected) in xs.iter().zip([1.0, 13.89, 27.69]) {
        near(*x, expected, 0.05, "line");
    }
    // A table that does not collapse has none.
    let tree = page(table_with_cells());
    let laid = lay_out(&tree, CSS);
    let table = laid.node("t");
    assert!(
        crate::collapsed_border_segments(&laid.arena, &laid.styles, &laid.layouts, table)
            .is_empty()
    );
}

fn table_with_cells() -> Element {
    table(
        "t",
        vec![tr(
            "r",
            vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
        )],
    )
}

#[test]
fn a_pointer_is_over_a_cell_or_the_table_never_over_a_row() {
    // Chrome 153: a cell across rows is hit where it lies over the second row, and the
    // gap between cells hits the table.
    let tree = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![
                    with_attr("td", "big", &[("rowspan", "2")], vec![text("big")]),
                    td("a", vec![text("a")]),
                ],
            ),
            tr("r2", vec![td("b", vec![text("b")])]),
        ],
    ));
    let css =
        format!("{CSS} td {{ width: 40px; height: 20px; }} .r2 {{ background-color: #fcc; }}");
    let laid = lay_out(&tree, &css);
    let (x, y, w, h) = laid.rect("big");
    let hit = |x: f32, y: f32| crate::hit_test(&laid.arena, &laid.layouts, &laid.styles, x, y);
    assert_eq!(hit(x + 5.0, y + 5.0), Some(laid.node("big")));
    assert_eq!(hit(x + 5.0, y + h - 5.0), Some(laid.node("big")));
    // In the row, between the cell across rows and the cell b.
    let (_, ry, ..) = laid.rect("r2");
    assert_eq!(hit(x + w + 1.0, ry + 3.0), Some(laid.node("t")));
}

#[test]
fn a_caption_centers_the_blocks_in_it_and_a_header_cell_does_not() {
    // Chrome 153: `text-align: -webkit-center` on a caption puts a block of its own
    // width in the middle.
    let tree = page(div(
        "",
        vec![
            table(
                "t1",
                vec![
                    element("caption", "c", vec![div("d1", vec![])]),
                    tr("", vec![td("a", vec![text("a")])]),
                ],
            ),
            table(
                "t2",
                vec![tr("", vec![element("th", "h", vec![div("d2", vec![])])])],
            ),
        ],
    ));
    let css =
        format!("{CSS} .t1, .t2 {{ width: 200px; }} .d1, .d2 {{ width: 40px; height: 10px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "d1", (80.0, 0.0, 40.0, 10.0));
    near(laid.rect("d2").0, 3.0, 0.05, "block in a header cell");
}

fn one_cell_table() -> Element {
    table("t", vec![tr("", vec![td("c", vec![text("aaa")])])])
}

#[test]
fn a_table_that_is_a_flex_item_is_as_wide_as_its_columns_and_stretches_to_the_line() {
    // Chrome 153.
    let tree = page(div("f", vec![one_cell_table(), div("d", vec![text("x")])]));
    let css =
        format!("{CSS} .f {{ display: flex; height: 100px; width: 300px; }} .d {{ flex: 1; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 32.67, 100.0));
    close(&laid, "c", (2.0, 2.0, 28.67, 96.0));
    close(&laid, "d", (32.67, 0.0, 267.33, 100.0));
}

#[test]
fn a_table_in_a_column_flex_container_or_a_grid_cell_takes_the_room() {
    let tree = page(div("f", vec![one_cell_table()]));
    let css = format!("{CSS} .f {{ display: flex; flex-direction: column; width: 300px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 300.0, 28.0));
    close(&laid, "c", (2.0, 2.0, 296.0, 24.0));
    let tree = page(div("g", vec![div("", vec![text("x")]), one_cell_table()]));
    let css =
        format!("{CSS} .g {{ display: grid; grid-template-columns: 100px 1fr; width: 300px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (100.0, 0.0, 200.0, 28.0));
    close(&laid, "c", (102.0, 2.0, 196.0, 24.0));
}

#[test]
fn an_absolute_table_and_one_in_a_padded_block_take_the_room_they_are_given() {
    let tree = page(div(
        "r",
        vec![table(
            "t",
            vec![tr(
                "",
                vec![td("a", vec![text("aaa")]), td("b", vec![text("bbb")])],
            )],
        )],
    ));
    let css = format!(
        "{CSS} .r {{ position: relative; width: 300px; height: 100px; }} \
         .t {{ position: absolute; right: 10px; top: 5px; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (223.95, 5.0, 66.05, 28.0));
    let tree = page(div(
        "b",
        vec![table("t", vec![tr("", vec![td("a", vec![text("a")])])])],
    ));
    let css = format!(
        "{CSS} .b {{ width: 300px; padding: 10px; border: 3px solid #000; }} .t {{ width: 100%; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (13.0, 13.0, 300.0, 28.0));
    let css = format!("{CSS} .t {{ margin: 0 auto; min-width: 200px; }}");
    let laid = lay_out(&page(one_cell_table()), &css);
    close(&laid, "t", (100.0, 0.0, 200.0, 28.0));
}

#[test]
fn a_float_in_a_cell_is_part_of_what_the_cell_needs() {
    let tree = page(table(
        "t",
        vec![tr("", vec![td("c", vec![div("fl", vec![]), text("text")])])],
    ));
    let css = format!("{CSS} .fl {{ float: left; width: 30px; height: 50px; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 64.77, 56.0));
    close(&laid, "c", (2.0, 2.0, 60.77, 52.0));
}

#[test]
fn a_floated_table_is_as_wide_as_its_columns() {
    let tree = page(div(
        "p",
        vec![
            table(
                "t",
                vec![tr(
                    "",
                    vec![td("a", vec![text("a")]), td("b", vec![text("b")])],
                )],
            ),
            text("Text that wraps beside the table here and there"),
        ],
    ));
    let css = format!(
        "{CSS} .p {{ width: 200px; font-size: 16px; line-height: 20px; }} .t {{ float: left; }}"
    );
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 28.69, 26.0));
    close(&laid, "p", (0.0, 0.0, 200.0, 60.0));
}

#[test]
fn a_percentage_height_in_a_cell_is_of_the_height_the_cell_states() {
    let tree = page(table(
        "t",
        vec![tr("", vec![td("c", vec![div("p", vec![text("x")])])])],
    ));
    let css = format!("{CSS} td {{ height: 100px; }} .p {{ height: 50%; }}");
    let laid = lay_out(&tree, &css);
    close(&laid, "t", (0.0, 0.0, 14.38, 106.0));
    close(&laid, "p", (3.0, 28.0, 8.38, 50.0));
}

fn segments_of(laid: &Laid) -> Vec<crate::BorderSegment> {
    crate::collapsed_border_segments(&laid.arena, &laid.styles, &laid.layouts, laid.node("t"))
}

/// The dashes of a dashed collapsed border start again in each segment: Chrome 153 splits the
/// line of two cells at the right edge of the middle border (the junction goes with the cell
/// before it), where the lines of a spanning cell run into each other by a junction's width.
#[test]
fn the_segments_of_a_dashed_border_do_not_overlap_but_where_a_cell_spans() {
    let two = |cells: Vec<Element>| page(table("t", vec![tr("r", cells)]));
    let cell = |class: &str| td(class, vec![div("d", vec![])]);
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; }} td {{ padding: 1px; border: 3px dashed #000; }} \
         .d {{ width: 20px; height: 10px; }}"
    );
    let laid = lay_out(&two(vec![cell("a"), cell("b")]), &css);
    let mut tops: Vec<_> = segments_of(&laid)
        .into_iter()
        .filter(|s| s.horizontal && s.y0 < 2.0)
        .collect();
    tops.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let spans: Vec<(f32, f32)> = tops.iter().map(|s| (s.x0, s.x1)).collect();
    assert_eq!(spans, vec![(0.0, 28.0), (28.0, 53.0)]);
    let mut sides: Vec<_> = segments_of(&laid)
        .into_iter()
        .filter(|s| !s.horizontal && s.x0 < 3.0)
        .collect();
    sides.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    assert_eq!(sides.len(), 1);
    near(sides[0].y0, 0.0, 0.01, "left edge from the top");
    near(sides[0].y1, 18.0, 0.01, "left edge to the bottom");

    // Two rows: the left border is a segment to the right edge of the border between them.
    let rows = page(table(
        "t",
        vec![tr("r1", vec![cell("a")]), tr("r2", vec![cell("b")])],
    ));
    let laid = lay_out(&rows, &css);
    let mut left: Vec<_> = segments_of(&laid)
        .into_iter()
        .filter(|s| !s.horizontal && s.x0 < 3.0)
        .collect();
    left.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let spans: Vec<(f32, f32)> = left.iter().map(|s| (s.y0, s.y1)).collect();
    assert_eq!(spans, vec![(0.0, 18.0), (18.0, 33.0)]);

    // A cell across two columns has the line along its top in two segments that overlap by
    // the junction: Chrome 153 has them at 0..27 and 27..53 on the top edge (no border
    // between the columns above), and at 0..28 and 25..53 under it.
    let wide = page(table(
        "t",
        vec![
            tr(
                "r1",
                vec![
                    with_attr("td", "a", &[("colspan", "2")], vec![div("d", vec![])]),
                    cell("b"),
                ],
            ),
            tr("r2", vec![cell("c"), cell("e"), cell("g")]),
        ],
    ));
    let laid = lay_out(&wide, &css);
    let mut top: Vec<_> = segments_of(&laid)
        .into_iter()
        .filter(|s| s.horizontal && s.y0 < 2.0)
        .collect();
    top.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let spans: Vec<(f32, f32)> = top.iter().map(|s| (s.x0, s.x1)).collect();
    assert_eq!(spans, vec![(0.0, 27.0), (27.0, 53.0), (53.0, 78.0)]);
    let mut inner: Vec<_> = segments_of(&laid)
        .into_iter()
        .filter(|s| s.horizontal && (s.y0 - 16.5).abs() < 0.1)
        .collect();
    inner.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let spans: Vec<(f32, f32)> = inner.iter().map(|s| (s.x0, s.x1)).collect();
    assert_eq!(spans, vec![(0.0, 28.0), (25.0, 53.0), (53.0, 78.0)]);
}

/// A vertical border that is wider than the dashed line it meets takes the square they have in
/// common: the dashes of the line start after it (Chrome 153, a 6px solid edge and a 3px
/// dashed one: dashes at 6..28 and 34..56).
#[test]
fn a_dashed_line_stops_at_a_wider_border_that_crosses_it() {
    let tree = page(table(
        "t",
        vec![tr(
            "r",
            vec![
                td("a", vec![div("d", vec![])]),
                td("b", vec![div("d", vec![])]),
            ],
        )],
    ));
    let css = format!(
        "{CSS} .t {{ border-collapse: collapse; }} \
         td {{ padding: 1px; border: 6px solid #000; border-top: 3px dashed #000; \
         border-bottom: 3px dashed #000; }} .d {{ width: 20px; height: 10px; }}"
    );
    let laid = lay_out(&tree, &css);
    let mut tops: Vec<_> = segments_of(&laid)
        .into_iter()
        .filter(|s| s.horizontal && s.y0 < 2.0)
        .collect();
    tops.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let spans: Vec<(f32, f32)> = tops.iter().map(|s| (s.x0, s.x1)).collect();
    assert_eq!(spans, vec![(6.0, 28.0), (34.0, 56.0)]);
}
