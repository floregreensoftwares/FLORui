//! The comparison itself: it passes when two paths agree and fails, with the
//! images and the elements it names, when they do not.

use std::panic::{AssertUnwindSafe, catch_unwind};

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_test::{Harness, Key, Route, assert_matches_clean, by_class};

const CSS: &str = ".count { width: 80px; height: 20px; } .go { width: 60px; height: 24px; }";

fn counter(initial: i32) -> Element {
    let count = use_signal(move || initial);
    let clicked = count.clone();
    view! {
        <div>
            <button class="go" onclick={move || clicked.set(clicked.get() + 1)}>{"Add"}</button>
            <p class="count">{count.get().to_string()}</p>
        </div>
    }
}

fn harness(initial: i32) -> Harness {
    Harness::new(move || counter(initial))
        .css(CSS)
        .viewport(160.0, 80.0)
}

#[test]
fn two_paths_to_the_same_state_agree() {
    assert_matches_clean(
        "equivalence_agree",
        // Both end with the button focused by the keyboard: a click would leave
        // it focused too, but a twin cannot be focused by a click without
        // counting it.
        Route::new(harness(0), |m| {
            m.press(Key::Tab);
            for _ in 0..3 {
                m.press(Key::Enter);
            }
        }),
        Route::new(harness(3), |m| m.press(Key::Tab)),
    );
}

#[test]
fn two_paths_to_different_states_fail_with_the_images_and_the_elements() {
    let name = "equivalence_differ";
    let result = catch_unwind(AssertUnwindSafe(|| {
        assert_matches_clean(
            name,
            Route::new(harness(0), |m| {
                let add = m.get(by_class("go"));
                m.click(add);
                m.click(add);
            }),
            Route::new(harness(3), |_| {}),
        );
    }));
    let message = match result {
        Ok(()) => panic!("two different states must not compare equal"),
        Err(panic) => panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_string()))
            .expect("the check panics with a message"),
    };

    assert!(message.contains("paints differently"), "{message}");
    assert!(message.contains("pixels differ"), "{message}");
    assert!(
        message.contains("<p.count>"),
        "the differing text is named by its element: {message}"
    );

    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    for kind in ["incremental", "clean", "diff"] {
        let path = directory.join(format!("{name}.{kind}.png"));
        assert!(path.exists(), "{} was written", path.display());
        std::fs::remove_file(path).ok();
    }
}
