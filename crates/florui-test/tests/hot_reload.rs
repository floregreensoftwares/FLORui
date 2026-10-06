//! What a stylesheet reload keeps. A reload swaps the rules and re-renders; the
//! components, and so the state they hold, stay. These go through the public
//! harness only: a counter, what was typed in a field and a textarea, which
//! element has the focus and how far a list was scrolled, before and after
//! `reload_css`, and the same on a window mounted fresh with the new stylesheet,
//! which has none of it.

use florui::prelude::*;
use florui_platform::use_scroll_offset;
use florui_reactive::{Binding, use_signal};
use florui_test::{Harness, Mounted, by_class, by_id};

const BEFORE: &str = "\
    .page { display: flex; flex-direction: column; padding: 8px; width: 300px; } \
    .list { height: 150px; overflow-y: auto; display: flex; flex-direction: column; } \
    .row { height: 26px; margin: 3px; padding: 4px; background-color: #ffffff; } \
    .go { width: 80px; height: 26px; background-color: #c81e1e; } \
    .name { width: 180px; } \
    .notes { width: 200px; height: 50px; }";

const AFTER: &str = "\
    .page { display: flex; flex-direction: column; padding: 20px; width: 300px; } \
    .list { height: 150px; overflow-y: auto; display: flex; flex-direction: column; } \
    .row { height: 26px; margin: 3px; padding: 4px; background-color: #eeeeff; } \
    .go { width: 140px; height: 26px; background-color: #1ec81e; } \
    .name { width: 180px; } \
    .notes { width: 200px; height: 50px; }";

fn panel() -> Element {
    let count = use_signal(|| 0);
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
    let offset = use_signal(|| 0.0_f32);
    let reported = offset.clone();
    let _scroll = use_scroll_offset("list", move |_, y| reported.set(y));

    let rows: Vec<Element> = (0..40)
        .map(|i| {
            Element::node(
                "div",
                vec![("class".into(), "row".into())],
                vec![Element::text(format!("Row {i}"))],
            )
        })
        .collect();
    view! {
        <div class="page">
            <div id="list" class="list">{rows}</div>
            <button class="go" onclick={move || clicked.set(clicked.get() + 1)}>{"Add"}</button>
            <p class="count">{count.get().to_string()}</p>
            <p class="offset">{format!("{:.0}", offset.get())}</p>
            <input id="name" class="name" type="text" value={name_binding} />
            <p class="typed">{name.get()}</p>
            <textarea id="notes" class="notes" value={notes_binding} />
            <p class="written">{notes.get()}</p>
        </div>
    }
}

fn mount(css: &str) -> Mounted {
    Harness::new(panel)
        .css(css)
        .viewport(340.0, 480.0)
        .reduced_motion(true)
        .mount()
}

fn text_of(m: &mut Mounted, class: &str) -> String {
    let node = m.get(by_class(class));
    m.text(node)
}

fn scroll_the_list(m: &mut Mounted, amount: f32) {
    let list = m.get(by_id("list"));
    let bounds = m.bounds(list).expect("the list is drawn");
    m.move_pointer(bounds.x + 20.0, bounds.y + 20.0);
    m.wheel(0.0, amount);
}

#[test]
fn a_reload_keeps_the_count_what_was_typed_the_focus_and_the_scroll_offset() {
    let mut m = mount(BEFORE);
    let add = m.get(by_class("go"));
    for _ in 0..3 {
        m.click(add);
    }
    let name = m.get(by_id("name"));
    m.click(name);
    m.type_text("hello");
    let notes = m.get(by_id("notes"));
    m.click(notes);
    m.type_text("first");
    m.press(florui_test::Key::Enter);
    m.type_text("second");
    scroll_the_list(&mut m, 90.0);
    let offset = text_of(&mut m, "offset");
    assert_ne!(offset, "0", "the list did not scroll before the reload");
    let go_before = m.bounds(add).expect("the button is drawn").width;
    let frame_before = m.frame().rgba;

    m.reload_css(AFTER);

    assert_ne!(
        m.frame().rgba,
        frame_before,
        "the new stylesheet was not painted"
    );
    let go = m.get(by_class("go"));
    let go_after = m.bounds(go).expect("the button is drawn").width;
    assert_ne!(go_after, go_before, "the button kept its old width");
    assert_eq!(text_of(&mut m, "count"), "3", "the counter was reset");
    assert_eq!(
        text_of(&mut m, "typed"),
        "hello",
        "the field's text was lost"
    );
    assert_eq!(
        text_of(&mut m, "written"),
        "first\nsecond",
        "the textarea's text was lost"
    );
    assert_eq!(
        text_of(&mut m, "offset"),
        offset,
        "the scroll position moved"
    );
    let notes = m.get(by_id("notes"));
    assert!(m.is_focused(notes), "the focus moved off the textarea");

    m.type_text("!");
    assert_eq!(
        text_of(&mut m, "written"),
        "first\nsecond!",
        "typing after the reload did not continue where it was"
    );
}

#[test]
fn a_window_mounted_with_the_new_stylesheet_has_none_of_that_state() {
    let mut fresh = mount(AFTER);

    assert_eq!(text_of(&mut fresh, "count"), "0");
    assert_eq!(text_of(&mut fresh, "typed"), "");
    assert_eq!(text_of(&mut fresh, "written"), "");
    assert_eq!(text_of(&mut fresh, "offset"), "0");
    assert!(fresh.focused().is_none());
}

#[test]
fn reloading_the_stylesheet_over_and_over_never_resets_anything() {
    let mut m = mount(BEFORE);
    let add = m.get(by_class("go"));
    m.click(add);
    let name = m.get(by_id("name"));
    m.click(name);
    m.type_text("ab");

    for round in 0..6 {
        m.reload_css(if round % 2 == 0 { AFTER } else { BEFORE });
    }

    assert_eq!(text_of(&mut m, "count"), "1");
    assert_eq!(text_of(&mut m, "typed"), "ab");
    let name = m.get(by_id("name"));
    assert!(m.is_focused(name));
}
