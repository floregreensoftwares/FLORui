//! `::first-line` through the paths a window takes: the kept tree, a text that
//! changes, and a hover that changes which first line is styled.

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_test::{Harness, Route, assert_matches_clean, by_class};

const CSS: &str = "\
    .row { width: 200px; background-color: #f0f0f0; font-size: 14px; line-height: 18px; } \
    .row::first-line { font-size: 20px; font-weight: bold; color: #2040c0; } \
    .row:hover::first-line { color: #c02020; } \
    .em:hover { background-color: #ffd0d0; }";

const TEXT: &str = "A paragraph whose first line is set larger and bolder than the rest of it";

#[test]
fn hovering_a_block_restyles_its_first_line_and_leaving_restores_it() {
    let mut mounted = Harness::new(|| view! { <div class="row">{TEXT}</div> })
        .css(CSS)
        .viewport(220.0, 120.0)
        .mount();
    let row = mounted.get(by_class("row"));
    let before = mounted.frame();
    mounted.hover(row);
    let hovered = mounted.frame();
    assert_ne!(before.rgba, hovered.rgba, "the line changes color");
    mounted.move_pointer(400.0, 400.0);
    assert_eq!(before.rgba, mounted.frame().rgba, "leaving restores it");
}

#[test]
fn a_text_that_changes_keeps_the_first_line_of_what_it_says() {
    let counter = || {
        let n = use_signal(|| 0);
        let bumped = n.clone();
        view! {
            <div class="row" onclick={move || bumped.set(bumped.get() + 1)}>
                {format!("{TEXT} {}", n.get())}
            </div>
        }
    };
    assert_matches_clean(
        "first_line_text_change",
        Route::new(Harness::new(counter).css(CSS).viewport(220.0, 120.0), |m| {
            let row = m.get(by_class("row"));
            m.click(row);
            m.click(row);
        }),
        Route::new(
            Harness::new(|| view! { <div class="row">{format!("{TEXT} 2")}</div> })
                .css(CSS)
                .viewport(220.0, 120.0),
            |m| {
                let row = m.get(by_class("row"));
                m.hover(row);
            },
        ),
    );
}

#[test]
fn hovering_an_element_in_the_first_line_paints_it_there_and_leaving_restores_it() {
    let mut mounted = Harness::new(|| {
        view! {
            <div class="row">
                <span class="em">{"Hovered"}</span>{" and then more words to fill the line and wrap"}
            </div>
        }
    })
    .css(CSS)
    .viewport(220.0, 120.0)
    .mount();
    let em = mounted.get(by_class("em"));
    let before = mounted.frame();
    mounted.hover(em);
    assert_ne!(before.rgba, mounted.frame().rgba, "the element is painted");
    mounted.move_pointer(400.0, 400.0);
    assert_eq!(before.rgba, mounted.frame().rgba, "leaving restores it");
}
