//! `::first-letter` through the paths a window takes: the kept tree, a text that
//! changes, and a hover that changes which letters are styled.

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_test::{Harness, Route, assert_matches_clean, by_class};

const CSS: &str = "\
    .row { width: 200px; background-color: #f0f0f0; } \
    .row::first-letter { font-size: 28px; color: #2040c0; } \
    .row:hover::first-letter { color: #c02020; }";

#[test]
fn hovering_a_block_restyles_its_first_letter_and_leaving_restores_it() {
    let mut mounted = Harness::new(|| view! { <div class="row">{"Hello world"}</div> })
        .css(CSS)
        .viewport(220.0, 60.0)
        .mount();
    let row = mounted.get(by_class("row"));
    let before = mounted.frame();
    mounted.hover(row);
    let hovered = mounted.frame();
    assert_ne!(before.rgba, hovered.rgba, "the letter changes color");
    mounted.move_pointer(400.0, 400.0);
    assert_eq!(before.rgba, mounted.frame().rgba, "leaving restores it");
}

#[test]
fn a_text_that_changes_keeps_the_first_letter_of_what_it_says() {
    let counter = || {
        let n = use_signal(|| 0);
        let bumped = n.clone();
        view! {
            <div class="row" onclick={move || bumped.set(bumped.get() + 1)}>
                {format!("Count {}", n.get())}
            </div>
        }
    };
    assert_matches_clean(
        "first_letter_text_change",
        Route::new(Harness::new(counter).css(CSS).viewport(220.0, 60.0), |m| {
            let row = m.get(by_class("row"));
            m.click(row);
            m.click(row);
        }),
        Route::new(
            Harness::new(|| view! { <div class="row">{"Count 2"}</div> })
                .css(CSS)
                .viewport(220.0, 60.0),
            |m| {
                let row = m.get(by_class("row"));
                m.hover(row);
            },
        ),
    );
}
