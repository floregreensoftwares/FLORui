//! What the harness promises, tested through its public surface only.

use std::rc::Rc;

use florui::prelude::*;
use florui_reactive::{Signal, use_signal};
use florui_test::{Harness, Key, Rect, by_class, by_id, by_role_named, by_text};

const CSS: &str = "\
    .page { display: flex; flex-direction: column; padding-top: 8px; padding-left: 8px; } \
    .add { width: 60px; height: 24px; } \
    .name { width: 120px; } \
    .box { width: 40px; height: 20px; transition: width 1s linear; } \
    .box:hover { width: 120px; }";

/// A component a user of the framework could have written: a counter, a text
/// field and a box that grows on hover.
fn panel() -> Element {
    let count = use_signal(|| 0);
    let clicked = count.clone();
    let text = use_signal(String::new);
    let binding = florui_reactive::Binding::new(text.get(), {
        let text = text.clone();
        move |value: String| text.set(value)
    });
    view! {
        <div class="page">
            <button class="add" onclick={move || clicked.set(clicked.get() + 1)}>{"Add"}</button>
            <p class="count">{count.get().to_string()}</p>
            <input id="name" class="name" type="text" value={binding} />
            <p class="typed">{text.get()}</p>
            <div class="box" />
        </div>
    }
}

fn mount_panel() -> florui_test::Mounted {
    Harness::new(panel).css(CSS).viewport(160.0, 120.0).mount()
}

#[test]
fn a_component_is_mounted_driven_inspected_compared_and_disposed() {
    let mut mounted = mount_panel();

    // Drive it with a pointer, then with the keyboard.
    let add = mounted.get(by_text("Add"));
    mounted.click(add);
    let count = mounted.get(by_class("count"));
    assert_eq!(mounted.text(count), "1");

    // A click focuses the button, so Enter activates it; Tab then moves on to the field.
    assert!(mounted.is_focused(add));
    mounted.press(Key::Enter);
    assert_eq!(mounted.text(count), "2");

    mounted.press(Key::Tab);
    let name = mounted.get(by_id("name"));
    assert!(mounted.is_focused(name));
    mounted.type_text("Ada");
    let typed = mounted.get(by_class("typed"));
    assert_eq!(mounted.text(typed), "Ada");

    // Inspect it: geometry, focus and what an assistive technology sees.
    mounted.assert_bounds(
        add,
        Rect {
            x: 8.0,
            y: 8.0,
            width: 62.0,
            height: 26.0,
        },
        0.5,
    );
    assert!(mounted.is_focused(name));
    let button = mounted.get(by_role_named("Button", "Add"));
    assert_eq!(button, add);
    let entry = mounted
        .accessibility(name)
        .expect("the field is in the tree");
    assert_eq!(entry.role, "TextInput");
    assert_eq!(entry.value.as_deref(), Some("Ada"));

    // Compare how it looks with a reviewed baseline.
    mounted.assert_snapshot("panel");

    // Dispose it: nothing is left behind.
    mounted.dispose().assert_clean();
}

#[test]
#[should_panic(expected = "bounds differ")]
fn a_geometry_regression_fails_the_test() {
    let mut mounted = mount_panel();
    let add = mounted.get(by_text("Add"));
    // The button was once 80px wide; it is 62px with its border.
    mounted.assert_bounds(
        add,
        Rect {
            x: 8.0,
            y: 8.0,
            width: 82.0,
            height: 26.0,
        },
        0.5,
    );
}

/// A callback stored in a signal that captures the same signal: the classic
/// Rust leak. The scope is released, the signal is not.
fn leaky() -> Element {
    type Callback = Rc<dyn Fn() -> usize>;
    let holder: Signal<Option<Callback>> = use_signal(|| None);
    let captured = holder.clone();
    let callback: Callback = Rc::new(move || captured.get().map_or(0, |_| 1));
    holder.set(Some(callback));
    view! { <div /> }
}

#[test]
fn a_leaked_signal_is_reported_when_the_component_is_disposed() {
    let mounted = Harness::new(leaky).viewport(40.0, 40.0).mount();
    let report = mounted.dispose();
    assert!(!report.is_clean());
    assert_eq!(report.left_behind.signals, 1, "{report:?}");
    assert_eq!(
        report.left_behind.scopes, 0,
        "the scope itself was released"
    );
}

#[test]
#[should_panic(expected = "left state alive")]
fn assert_clean_fails_on_a_leak() {
    let mounted = Harness::new(leaky).viewport(40.0, 40.0).mount();
    mounted.dispose().assert_clean();
}

#[test]
#[should_panic(expected = "left state alive")]
fn a_leak_fails_even_when_the_test_forgets_to_dispose() {
    let _mounted = Harness::new(leaky).viewport(40.0, 40.0).mount();
}

#[test]
fn a_component_that_cleans_up_passes_the_teardown_check() {
    let mounted = mount_panel();
    drop(mounted);
}

#[test]
fn rendering_is_deterministic_across_mounts() {
    let first = mount_panel().frame().rgba;
    let second = mount_panel().frame().rgba;
    assert_eq!(first, second);
}

#[test]
fn a_transition_steps_with_the_clock_the_test_controls() {
    let mut mounted = mount_panel();
    let boxed = mounted.get(by_class("box"));
    mounted.hover(boxed);
    mounted.advance(0.5);
    let halfway = mounted.bounds(boxed).expect("drawn").width;
    mounted.advance(1.0);
    let done = mounted.bounds(boxed).expect("drawn").width;
    assert!((halfway - 80.0).abs() < 2.0, "halfway through: {halfway}");
    assert_eq!(done, 120.0);
}

#[test]
fn the_device_pixel_ratio_scales_the_frame_not_the_layout() {
    let mut mounted = Harness::new(panel)
        .css(CSS)
        .viewport(160.0, 120.0)
        .dpr(2.0)
        .mount();
    let frame = mounted.frame();
    assert_eq!((frame.width, frame.height), (320, 240));
    let add = mounted.get(by_text("Add"));
    assert_eq!(mounted.bounds(add).expect("drawn").width, 62.0);
    mounted.click(add);
    let count = mounted.get(by_class("count"));
    assert_eq!(mounted.text(count), "1");
}

#[test]
fn a_changed_frame_fails_its_snapshot_and_leaves_the_actual_image_and_a_diff() {
    let mut mounted = mount_panel();
    let add = mounted.get(by_text("Add"));
    mounted.click(add);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        mounted.assert_snapshot("panel");
    }));
    let message = match outcome {
        Ok(()) => panic!("the count changed, so the frame must differ from the baseline"),
        Err(payload) => payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default(),
    };
    assert!(message.contains("pixels differ"), "{message}");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    for artifact in ["panel.actual.png", "panel.diff.png"] {
        let path = dir.join(artifact);
        assert!(path.exists(), "{artifact} was not written");
        std::fs::remove_file(path).expect("the artifact can be removed");
    }
}

#[test]
#[should_panic(expected = "the harness paints at most")]
fn a_viewport_too_large_to_paint_is_refused() {
    let _ = Harness::new(panel).viewport(20_000.0, 20_000.0).mount();
}

#[test]
#[should_panic(expected = "the harness paints at most")]
fn resizing_past_the_ceiling_is_refused() {
    let mut mounted = mount_panel();
    mounted.resize(9_000.0, 9_000.0);
}
