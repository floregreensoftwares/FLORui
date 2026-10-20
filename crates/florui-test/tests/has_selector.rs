//! `:has()` through the paths a window takes: a state below an element reaching it
//! (a whole cascade is run for that), and a structural condition kept across updates.

use florui::prelude::*;
use florui_test::{Harness, Route, assert_matches_clean, by_id};

const CSS: &str = "\
    .list { width: 220px; } \
    .row { height: 22px; background-color: #f0f0f0; } \
    .cell { display: block; width: 80px; height: 18px; background-color: #c0c0c0; } \
    .row:has(.cell:hover) { background-color: #cce0ff; } \
    .row:has(.mark) { background-color: #d8f0d8; }";

fn list() -> Element {
    let rows: Vec<Element> = (0..6)
        .map(|i| {
            let mut children = vec![Element::node(
                "span",
                vec![("class".into(), "cell".into())],
                vec![Element::text(format!("Row {i}"))],
            )];
            if i == 4 {
                children.push(Element::node(
                    "i",
                    vec![("class".into(), "mark".into())],
                    vec![],
                ));
            }
            Element::node("div", vec![("class".into(), "row".into())], children)
        })
        .collect();
    Element::node(
        "div",
        vec![("id".into(), "list".into()), ("class".into(), "list".into())],
        rows,
    )
}

fn harness() -> Harness {
    Harness::new(list).css(CSS).viewport(240.0, 160.0)
}

#[test]
fn hovering_a_cell_restyles_the_row_that_has_it_and_leaving_restores_it() {
    let mut mounted = harness().mount();
    let list = mounted.get(by_id("list"));
    let b = mounted.bounds(list).expect("the list is drawn");
    let before = mounted.frame();
    mounted.move_pointer(b.x + 30.0, b.y + 30.0);
    let hovered = mounted.frame();
    assert_ne!(
        before.rgba, hovered.rgba,
        "the row that has the hovered cell changes"
    );
    mounted.move_pointer(b.x + 300.0, b.y + 30.0);
    let left = mounted.frame();
    assert_eq!(before.rgba, left.rgba, "leaving paints the first frame");
}

#[test]
fn hovering_across_rows_matches_pointing_at_the_last_one() {
    assert_matches_clean(
        "has_hover_across_rows",
        Route::new(harness(), |m| {
            let list = m.get(by_id("list"));
            let b = m.bounds(list).expect("the list is drawn");
            for step in 0..6 {
                m.move_pointer(b.x + 30.0, b.y + 10.0 + step as f32 * 22.0);
            }
        }),
        Route::new(harness(), |m| {
            let list = m.get(by_id("list"));
            let b = m.bounds(list).expect("the list is drawn");
            m.move_pointer(b.x + 30.0, b.y + 10.0 + 5.0 * 22.0);
        }),
    );
}
