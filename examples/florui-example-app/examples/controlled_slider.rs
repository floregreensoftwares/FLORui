//! A real `Slider`, live: click the track to jump, drag the thumb, or Tab
//! to it and use the arrow keys/Page Up/Page Down/Home/End, to move it.
//! The disabled one never moves or takes focus, and the platform
//! accessibility bridge (Narrator, on Windows) reports each as a real
//! slider with its name and numeric value.
//!
//! Both ways to wire a slider are shown. "Volume" uses a `Binding<f32>`
//! (`Signal::binding`, which always accepts). "Brightness" uses the
//! explicit contract -- a plain value plus a `FloatHandler` told the
//! value the slider is asking for -- logs every request, and fires
//! `on_commit` once an interaction ends. "Zoom" has a custom `step` of 25
//! on a 50-400 range.
//!
//! `cargo run --example controlled_slider -p florui-example-app`

use florui::prelude::*;
use florui_platform::{Slider, SliderProps, SliderValue};
use florui_reactive::use_signal;
use florui_style::Rgba;

const CONTROLLED_SLIDER_CSS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/controlled_slider.css"
);

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled slider",
        CONTROLLED_SLIDER_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let volume = use_signal(|| 30.0_f32);
    let brightness = use_signal(|| 50.0_f32);
    let last_request = use_signal(|| String::from("none yet"));
    let commits = use_signal(|| 0_u32);
    let zoom = use_signal(|| 100.0_f32);
    let locked = use_signal(|| 60.0_f32);

    let record_request = last_request.clone();
    let accept_brightness = brightness.clone();
    let count_commit = commits.clone();

    view! {
        <div class="page">
            <p class="instructions">
                {"Click the track to jump, drag the thumb, or Tab in and use the arrow keys, \
                  Page Up/Page Down, or Home/End. The disabled one never moves."}
            </p>

            <div class="row">
                <label class="field-label" for="volume-slider">{"Volume (Binding)"}</label>
                <Slider
                    id={"volume-slider".to_string()}
                    value={SliderValue::bound(volume.binding())}
                    min={0.0}
                    max={100.0}
                    step={1.0}
                    disabled={false}
                    accessible_label={"Volume".to_string()}
                    on_commit={Handler::new(|| {})}
                />
            </div>
            <p class="status">{format!("Committed: {}", volume.get())}</p>

            <div class="row">
                <label class="field-label" for="brightness-slider">
                    {"Brightness (value + handler)"}
                </label>
                <Slider
                    id={"brightness-slider".to_string()}
                    value={SliderValue::controlled(
                        brightness.get(),
                        FloatHandler::new(move |requested| {
                            record_request.set(format!("{requested}"));
                            accept_brightness.set(requested);
                        }),
                    )}
                    min={0.0}
                    max={100.0}
                    step={1.0}
                    disabled={false}
                    accessible_label={"Brightness".to_string()}
                    on_commit={Handler::new(move || count_commit.set(count_commit.get() + 1))}
                />
            </div>
            <p class="status">
                {format!(
                    "Committed: {}  Last requested: {}  Commits: {}",
                    brightness.get(),
                    last_request.get(),
                    commits.get(),
                )}
            </p>

            <div class="row">
                <label class="field-label" for="zoom-slider">{"Zoom (step 25)"}</label>
                <Slider
                    id={"zoom-slider".to_string()}
                    value={SliderValue::bound(zoom.binding())}
                    min={50.0}
                    max={400.0}
                    step={25.0}
                    disabled={false}
                    accessible_label={"Zoom".to_string()}
                    on_commit={Handler::new(|| {})}
                />
            </div>
            <p class="status">{format!("Committed: {}%", zoom.get())}</p>

            <div class="row">
                <label class="field-label" for="locked-slider">{"Locked (disabled)"}</label>
                <Slider
                    id={"locked-slider".to_string()}
                    value={SliderValue::bound(locked.binding())}
                    min={0.0}
                    max={100.0}
                    step={1.0}
                    disabled={true}
                    accessible_label={"Locked".to_string()}
                    on_commit={Handler::new(|| {})}
                />
            </div>
        </div>
    }
}
