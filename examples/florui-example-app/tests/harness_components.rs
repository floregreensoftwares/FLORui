//! The example app's components, tested through the public component harness:
//! real clicks on the drawn elements, nothing reached into.

use florui::prelude::*;
use florui_example_app::components::counter::{Counter, CounterProps};
use florui_example_app::components::stepper::{Stepper, StepperProps};
use florui_reactive::use_signal;
use florui_test::{Harness, Key, Mounted, by_class, by_role_named};

const COUNTER_CSS: &str = include_str!("../src/components/counter.css");
const STEPPER_CSS: &str = include_str!("../src/components/stepper.css");

fn counter() -> Mounted {
    Harness::new(|| Counter(CounterProps {}))
        .css(COUNTER_CSS)
        .viewport(240.0, 200.0)
        .mount()
}

fn text(mounted: &mut Mounted, class: &str) -> String {
    let node = mounted.get(by_class(class));
    mounted.text(node)
}

#[test]
fn the_counter_starts_at_zero_and_a_render_with_no_click_keeps_it() {
    let mut mounted = counter();
    assert_eq!(text(&mut mounted, "count"), "0");
    mounted.settle();
    assert_eq!(text(&mut mounted, "count"), "0");
}

#[test]
fn clicking_the_counter_increments_and_the_count_persists() {
    let mut mounted = counter();
    let button = mounted.get(by_class("increment"));
    mounted.click(button);
    assert_eq!(text(&mut mounted, "count"), "1");
    mounted.settle();
    assert_eq!(
        text(&mut mounted, "count"),
        "1",
        "it persists without another click"
    );
    mounted.click(button);
    assert_eq!(text(&mut mounted, "count"), "2");
}

#[test]
fn the_counter_can_be_driven_from_the_keyboard_too() {
    let mut mounted = counter();
    mounted.press(Key::Tab);
    mounted.press(Key::Enter);
    mounted.press(Key::Space);
    assert_eq!(text(&mut mounted, "count"), "2");
}

#[test]
fn the_doubled_value_tracks_the_count() {
    let mut mounted = counter();
    assert_eq!(text(&mut mounted, "doubled"), "x2 = 0");
    let button = mounted.get(by_class("increment"));
    mounted.click(button);
    assert_eq!(text(&mut mounted, "doubled"), "x2 = 2");
    mounted.click(button);
    assert_eq!(text(&mut mounted, "doubled"), "x2 = 4");
}

#[test]
fn the_counter_button_is_named_for_assistive_technology_and_disposes_cleanly() {
    let mut mounted = counter();
    let button = mounted.get(by_role_named("Button", "+1"));
    assert_eq!(button, mounted.get(by_class("increment")));
    mounted.dispose().assert_clean();
}

/// The owner clamps to `min..=max` and rejects anything outside it, so the
/// stepper is shown to be driven by its owner's validation, not just by an
/// adapter that accepts everything.
fn stepper(initial: i32, min: i32, max: i32) -> Mounted {
    Harness::new(move || {
        let count = use_signal(|| initial);
        let value = Binding::new(count.get(), move |requested: i32| {
            if (min..=max).contains(&requested) {
                count.set(requested);
            }
        });
        Stepper(StepperProps { value })
    })
    .css(STEPPER_CSS)
    .viewport(240.0, 120.0)
    .mount()
}

#[test]
fn the_stepper_starts_at_the_owners_value() {
    let mut mounted = stepper(3, 0, 10);
    assert_eq!(text(&mut mounted, "value"), "3");
}

#[test]
fn the_stepper_increments_and_decrements_within_range() {
    let mut mounted = stepper(0, 0, 10);
    let up = mounted.get(by_class("increment"));
    let down = mounted.get(by_class("decrement"));
    mounted.click(up);
    mounted.click(up);
    assert_eq!(text(&mut mounted, "value"), "2");
    mounted.click(down);
    assert_eq!(text(&mut mounted, "value"), "1");
}

#[test]
fn the_owner_rejects_a_request_below_its_minimum() {
    let mut mounted = stepper(0, 0, 10);
    let down = mounted.get(by_class("decrement"));
    mounted.click(down);
    assert_eq!(text(&mut mounted, "value"), "0");
}

#[test]
fn the_owner_rejects_a_request_above_its_maximum() {
    let mut mounted = stepper(10, 0, 10);
    let up = mounted.get(by_class("increment"));
    mounted.click(up);
    assert_eq!(text(&mut mounted, "value"), "10");
    mounted.dispose().assert_clean();
}
