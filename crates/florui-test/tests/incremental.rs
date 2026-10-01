//! Reaching a state step by step must look exactly like being in it from the
//! start. Each scenario drives a component the way a user would and compares the
//! result with a twin brought to the same state directly; see
//! [`florui_test::assert_matches_clean`].

use florui::prelude::*;
use florui_platform::use_scroll_offset;
use florui_reactive::{Binding, use_signal};
use florui_test::{Harness, Key, Mounted, Route, assert_matches_clean, by_id};

const CSS: &str = "\
    .page { display: flex; flex-direction: column; padding: 8px; width: 300px; } \
    .list { height: 150px; overflow-y: auto; display: flex; flex-direction: column; } \
    .row { height: 26px; margin: 3px; padding: 4px; border-radius: 6px; background-color: #ffffff; \
           box-shadow: 0px 2px 6px rgba(0, 0, 0, 0.3); transition: background-color 0.2s; } \
    .row:hover { background-color: #cce0ff; } \
    .go { width: 80px; height: 26px; } \
    .go:focus-visible { background-color: #ffcc66; } \
    .name { width: 180px; } \
    .notes { width: 200px; height: 50px; }";

const ROWS: usize = 60;

/// A component with the things an incremental path has to get right: a clipped
/// scroll box of rows with radii and blurred shadows, hover and focus styles, a
/// counter, a text field and a textarea. With `reads_offset` the scroll position
/// is shown in the page, so a wheel tick re-renders instead of only repainting.
fn panel(initial: i32, reads_offset: bool) -> Element {
    let count = use_signal(move || initial);
    let clicked = count.clone();
    let name = use_signal(String::new);
    let name_binding = Binding::new(name.get(), {
        let name = name.clone();
        move |value: String| name.set(value)
    });
    let notes = use_signal(String::new);
    let notes_binding = Binding::new(notes.get(), {
        let notes = notes.clone();
        move |value: String| notes.set(value)
    });
    let scroll = use_scroll_offset("list", |_, _| {});

    let rows: Vec<Element> = (0..ROWS)
        .map(|i| {
            Element::node(
                "div",
                vec![("class".into(), "row".into())],
                vec![Element::text(format!("Row {i}"))],
            )
        })
        .collect();
    let list = Element::node(
        "div",
        vec![
            ("id".into(), "list".into()),
            ("class".into(), "list".into()),
        ],
        rows,
    );
    let offset = if reads_offset {
        Some(format!("offset {:?}", scroll.offset()))
    } else {
        None
    };
    Element::node(
        "div",
        vec![("class".into(), "page".into())],
        vec![
            list,
            view! { <button class="go" onclick={move || clicked.set(clicked.get() + 1)}>{"Add"}</button> },
            view! { <p class="count">{count.get().to_string()}</p> },
            view! { <p class="offset">{offset.unwrap_or_default()}</p> },
            view! { <input id="name" class="name" type="text" value={name_binding} /> },
            view! { <textarea id="notes" class="notes" value={notes_binding} /> },
        ],
    )
}

fn harness(initial: i32, reads_offset: bool) -> Harness {
    Harness::new(move || panel(initial, reads_offset))
        .css(CSS)
        .viewport(320.0, 360.0)
        .reduced_motion(true)
}

/// A wheel over the list, as a user's would be.
fn over_list(m: &mut Mounted) {
    let list = m.get(by_id("list"));
    let bounds = m.bounds(list).expect("the list is drawn");
    m.move_pointer(bounds.x + 20.0, bounds.y + 20.0);
}

#[test]
fn scrolling_in_ticks_matches_scrolling_the_whole_distance() {
    for reads_offset in [false, true] {
        assert_matches_clean(
            if reads_offset {
                "scroll_ticks_rendering_the_offset"
            } else {
                "scroll_ticks"
            },
            Route::new(harness(0, reads_offset), |m| {
                over_list(m);
                for tick in [13.0, 7.0, 41.0, 5.0, 29.0, 3.0, 17.0] {
                    m.wheel(0.0, tick);
                }
            }),
            Route::new(harness(0, reads_offset), |m| {
                over_list(m);
                m.wheel(0.0, 13.0 + 7.0 + 41.0 + 5.0 + 29.0 + 3.0 + 17.0);
            }),
        );
    }
}

#[test]
fn scrolling_forward_and_back_matches_never_scrolling_past_the_end() {
    assert_matches_clean(
        "scroll_there_and_back",
        Route::new(harness(0, false), |m| {
            over_list(m);
            m.wheel(0.0, 900.0);
            m.wheel(0.0, -400.0);
            m.wheel(0.0, 120.0);
        }),
        Route::new(harness(0, false), |m| {
            over_list(m);
            // The end of the list is 60 rows of 40px (margins included) in a
            // 150px box; 900 - 400 + 120 clamped is where the first path ends.
            m.wheel(0.0, 900.0 - 400.0 + 120.0);
        }),
    );
}

#[test]
fn hovering_across_rows_matches_pointing_at_the_last_one() {
    assert_matches_clean(
        "hover_across_rows",
        Route::new(harness(0, false), |m| {
            let list = m.get(by_id("list"));
            let b = m.bounds(list).expect("the list is drawn");
            for step in 0..6 {
                m.move_pointer(b.x + 30.0, b.y + 10.0 + step as f32 * 21.0);
            }
        }),
        Route::new(harness(0, false), |m| {
            let list = m.get(by_id("list"));
            let b = m.bounds(list).expect("the list is drawn");
            m.move_pointer(b.x + 30.0, b.y + 10.0 + 5.0 * 21.0);
        }),
    );
}

#[test]
fn scrolling_under_a_stationary_pointer_restyles_like_a_fresh_scroll() {
    assert_matches_clean(
        "scroll_under_pointer",
        Route::new(harness(0, false), |m| {
            over_list(m);
            for _ in 0..5 {
                m.wheel(0.0, 18.0);
            }
        }),
        Route::new(harness(0, false), |m| {
            // Scroll with the pointer elsewhere over the list, then put it where
            // it was: the row under it must be hovered either way.
            let list = m.get(by_id("list"));
            let b = m.bounds(list).expect("the list is drawn");
            m.move_pointer(b.x + 150.0, b.y + 100.0);
            m.wheel(0.0, 90.0);
            m.move_pointer(b.x + 20.0, b.y + 20.0);
        }),
    );
}

#[test]
fn a_counter_pressed_three_times_matches_one_that_started_at_three() {
    // Both end with the button focused by the keyboard (a click would focus it
    // too, but a twin cannot be focused by a click without counting it).
    assert_matches_clean(
        "counter_pressed",
        Route::new(harness(0, false), |m| {
            m.press(Key::Tab);
            for _ in 0..3 {
                m.press(Key::Enter);
            }
        }),
        Route::new(harness(3, false), |m| m.press(Key::Tab)),
    );
}

#[test]
fn a_reloaded_stylesheet_matches_one_mounted_with_it() {
    const RELOADED: &str = "\
        .page { display: flex; flex-direction: column; padding: 8px; width: 300px; } \
        .list { height: 150px; overflow-y: auto; display: flex; flex-direction: column; } \
        .row { height: 26px; margin: 3px; padding: 4px; border-radius: 12px; background-color: #ffeedd; \
               box-shadow: 0px 4px 10px rgba(0, 0, 0, 0.4); } \
        .go { width: 120px; height: 30px; background-color: #336699; } \
        .name { width: 240px; } \
        .notes { width: 200px; height: 80px; }";
    assert_matches_clean(
        "stylesheet_reload",
        Route::new(harness(0, false), |m| {
            over_list(m);
            m.wheel(0.0, 60.0);
            m.reload_css(RELOADED);
        }),
        Route::new(
            Harness::new(|| panel(0, false))
                .css(RELOADED)
                .viewport(320.0, 360.0)
                .reduced_motion(true),
            |m| {
                over_list(m);
                m.wheel(0.0, 60.0);
            },
        ),
    );
}

#[test]
fn shrinking_a_window_scrolled_to_the_end_matches_mounting_it_small() {
    assert_matches_clean(
        "shrink_scrolled",
        Route::new(harness(0, false), |m| {
            over_list(m);
            m.wheel(0.0, 5000.0);
            m.resize(320.0, 240.0);
        }),
        Route::new(
            Harness::new(|| panel(0, false))
                .css(CSS)
                .viewport(320.0, 240.0)
                .reduced_motion(true),
            |m| {
                over_list(m);
                m.wheel(0.0, 5000.0);
            },
        ),
    );
}

#[test]
fn tabbing_forward_and_back_matches_tabbing_forward_once() {
    assert_matches_clean(
        "tab_there_and_back",
        Route::new(harness(0, false), |m| {
            m.press(Key::Tab);
            m.press(Key::Tab);
            m.modifiers(false, true, false);
            m.press(Key::Tab);
            m.modifiers(false, false, false);
        }),
        Route::new(harness(0, false), |m| {
            m.press(Key::Tab);
        }),
    );
}

#[test]
fn typing_and_correcting_matches_typing_the_result() {
    assert_matches_clean(
        "typing_in_a_field",
        Route::new(harness(0, false), |m| {
            let name = m.get(by_id("name"));
            m.click(name);
            m.type_text("hellp");
            m.press(Key::Backspace);
            m.type_text("o there");
            m.press(Key::Backspace);
            m.press(Key::Backspace);
            m.type_text("re");
        }),
        Route::new(harness(0, false), |m| {
            let name = m.get(by_id("name"));
            m.click(name);
            m.type_text("hello there");
        }),
    );
}

#[test]
fn typing_in_a_textarea_matches_typing_the_result() {
    assert_matches_clean(
        "typing_in_a_textarea",
        Route::new(harness(0, false), |m| {
            let notes = m.get(by_id("notes"));
            m.click(notes);
            m.type_text("first");
            m.press(Key::Enter);
            m.type_text("secondd");
            m.press(Key::Backspace);
        }),
        Route::new(harness(0, false), |m| {
            let notes = m.get(by_id("notes"));
            m.click(notes);
            m.type_text("first");
            m.press(Key::Enter);
            m.type_text("second");
        }),
    );
}

#[test]
fn a_wheel_jump_paints_what_a_re_render_at_the_same_size_would() {
    // A large jump changes which row is under the pointer. The frame painted
    // right after it must already show that row hovered, with the hover
    // transition of the stylesheet honoring the window's reduced-motion setting,
    // the same as the frame a re-render would paint.
    assert_matches_clean(
        "wheel_jump_against_a_re_render",
        Route::new(harness(0, false), |m| {
            over_list(m);
            m.wheel(0.0, 400.0);
        }),
        Route::new(harness(0, false), |m| {
            over_list(m);
            m.wheel(0.0, 400.0);
            m.resize(320.0, 360.0);
        }),
    );
}
