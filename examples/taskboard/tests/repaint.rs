//! A frame painted over the last must be the frame painted whole. Every step
//! here is a frame that is checked against the whole, and the steps a person
//! makes most (the pointer going over a key, a click) must be painted as a part
//! of the window and not all of it.

use florui_paint::Repaint;
use florui_test::{Harness, Mounted, by_class, by_id, by_text};
use taskboard::app::{App, AppProps};

fn app() -> Mounted {
    Harness::new(|| App(AppProps {}))
        .css(&taskboard::stylesheet())
        .viewport(1180.0, 760.0)
        .verify_repaint(true)
        .mount()
}

/// A frame, checked against the whole one, and how it was painted.
fn painted(mounted: &mut Mounted) -> Option<Repaint> {
    let _ = mounted.frame();
    mounted.last_repaint()
}

fn is_part(repaint: Option<Repaint>) -> bool {
    matches!(repaint, Some(Repaint::Part { .. }) | Some(Repaint::Nothing))
}

#[test]
fn the_pointer_going_over_keys_repaints_a_part() {
    let mut mounted = app();
    assert_eq!(
        painted(&mut mounted),
        Some(Repaint::Whole),
        "the first frame"
    );
    for id in ["nav-stack", "nav-settings", "nav-desk"] {
        let key = mounted.get(by_id(id));
        mounted.hover(key);
        let repaint = painted(&mut mounted);
        assert!(is_part(repaint), "hovering {id}: {repaint:?}");
    }
}

#[test]
fn a_click_repaints_what_it_changed() {
    let mut mounted = app();
    painted(&mut mounted);
    let stack = mounted.get(by_id("nav-stack"));
    mounted.click(stack);
    // A screen is another tree: any way of painting it is right if it is whole.
    painted(&mut mounted);
    let status = mounted.get(by_text("Todo"));
    mounted.click(status);
    painted(&mut mounted);
    let card = mounted.all(by_class("paper-card"))[2];
    mounted.hover(card);
    let repaint = painted(&mut mounted);
    assert!(is_part(repaint), "hovering a card: {repaint:?}");
}

#[test]
fn typing_scrolling_and_the_settings_are_painted_right() {
    let mut mounted = app();
    painted(&mut mounted);
    let stack = mounted.get(by_id("nav-stack"));
    mounted.click(stack);
    painted(&mut mounted);

    let title = mounted.get(by_id("new-title"));
    mounted.click(title);
    painted(&mut mounted);
    mounted.type_text("Wax the bench");
    painted(&mut mounted);

    let card = mounted.all(by_class("paper-card"))[0];
    mounted.scroll(card, 0.0, 300.0);
    painted(&mut mounted);
    mounted.scroll(card, 0.0, -120.0);
    painted(&mut mounted);

    let settings = mounted.get(by_id("nav-settings"));
    mounted.click(settings);
    painted(&mut mounted);
    for toggle in mounted.all(by_class("toggle")) {
        mounted.click(toggle);
        painted(&mut mounted);
    }
    let knob = mounted.get(by_class("knob"));
    mounted.click(knob);
    painted(&mut mounted);
    mounted.hover(knob);
    painted(&mut mounted);
    mounted.advance(0.3);
    painted(&mut mounted);
}

#[test]
fn a_new_size_is_painted_whole() {
    let mut mounted = app();
    painted(&mut mounted);
    mounted.resize(1000.0, 700.0);
    assert_eq!(painted(&mut mounted), Some(Repaint::Whole));
    let key = mounted.get(by_id("nav-stack"));
    mounted.hover(key);
    assert!(is_part(painted(&mut mounted)));
}
