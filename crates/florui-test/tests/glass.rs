//! The glass material under incremental rendering: a scene that gets to a
//! state by moving its backdrop, resizing its panel and toggling the
//! material must paint exactly what a fresh render of that state paints,
//! with no stale backdrop pixels left behind.

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_test::{Harness, Key, Route, assert_matches_clean};

const CSS: &str = "\
.scene { position: relative; width: 200px; height: 120px; background-color: #14162a; } \
.blob { position: absolute; top: 10px; width: 50px; height: 100px; background-color: #ff4d6d; } \
.blob2 { position: absolute; top: 40px; left: 150px; width: 40px; height: 60px; background-color: #2de2c4; } \
.glass { position: absolute; top: 20px; border-radius: 16px; \
  backdrop-filter: blur(3px); background-color: rgba(255, 255, 255, 0.12); } \
.refract { --florui-glass: refract; --florui-glass-refraction: 8px; --florui-glass-edge: 16px; \
  --florui-glass-light-angle: 315deg; --florui-glass-light-strength: 0.6; } \
.go { position: absolute; top: 100px; left: 0px; width: 30px; height: 16px; }";

/// The scene in one of its states: where the backdrop is, how wide the panel
/// is, and whether the material is on.
#[derive(Clone, Copy)]
struct State {
    blob_left: i32,
    panel_left: i32,
    panel_width: i32,
    refract: bool,
}

const START: State = State {
    blob_left: 20,
    panel_left: 40,
    panel_width: 90,
    refract: false,
};

const END: State = State {
    blob_left: 85,
    panel_left: 55,
    panel_width: 110,
    refract: true,
};

/// What the button does: jump to the end state, or flip the material.
#[derive(Clone, Copy)]
enum Action {
    Advance,
    Flip,
}

fn scene(initial: State, action: Action) -> Element {
    let state = use_signal(move || {
        (
            initial.blob_left,
            initial.panel_left,
            initial.panel_width,
            initial.refract,
        )
    });
    let press = state.clone();
    let (blob_left, panel_left, panel_width, refract) = state.get();
    view! {
        <div class="scene">
            <div class="blob" style={format!("left: {blob_left}px;")}></div>
            <div class="blob2"></div>
            <div
                class={if refract { "glass refract" } else { "glass" }}
                style={format!("left: {panel_left}px; width: {panel_width}px; height: 80px;")}
            >
            </div>
            <button
                class="go"
                onclick={move || match action {
                    Action::Advance => {
                        press.set((END.blob_left, END.panel_left, END.panel_width, END.refract))
                    }
                    Action::Flip => {
                        let (blob, left, width, refract) = press.get();
                        press.set((blob, left, width, !refract));
                    }
                }}
            >
                {"go"}
            </button>
        </div>
    }
}

fn harness(initial: State, action: Action) -> Harness {
    Harness::new(move || scene(initial, action))
        .css(CSS)
        .viewport(200.0, 120.0)
}

// Both routes end with the button focused by the keyboard: a click would leave
// it focused too, but a twin cannot be focused by a click without counting it.

#[test]
fn moving_the_backdrop_resizing_the_panel_and_turning_the_material_on_matches_a_clean_render() {
    assert_matches_clean(
        "glass_incremental",
        Route::new(harness(START, Action::Advance), |m| {
            m.press(Key::Tab);
            m.press(Key::Enter);
        }),
        Route::new(harness(END, Action::Advance), |m| m.press(Key::Tab)),
    );
}

#[test]
fn turning_the_material_on_and_off_again_leaves_exactly_the_basic_glass() {
    assert_matches_clean(
        "glass_on_and_off",
        Route::new(harness(START, Action::Flip), |m| {
            m.press(Key::Tab);
            m.press(Key::Enter);
            m.press(Key::Enter);
        }),
        Route::new(harness(START, Action::Flip), |m| m.press(Key::Tab)),
    );
}

#[test]
fn the_material_on_differs_from_the_basic_glass() {
    // The check can fail: with the material on, the same scene is not the
    // basic one.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_matches_clean(
            "glass_on_differs",
            Route::new(harness(START, Action::Flip), |m| {
                m.press(Key::Tab);
                m.press(Key::Enter);
            }),
            Route::new(harness(START, Action::Flip), |m| m.press(Key::Tab)),
        );
    }));
    assert!(result.is_err(), "the material changes what is painted");
}
