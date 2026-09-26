//! A real `<input type="radio">` group, live: click or Tab+Space selects
//! one, the arrow keys move focus and selection within the group, Tab
//! enters the group once (at the selected radio) and skips the disabled
//! one, and the platform accessibility bridge reports real radio
//! buttons with their real checked state.
//!
//! Exclusivity is the app's own: one `Signal` holds the selected value
//! and each radio derives `checked` from it, so `name` only defines the
//! keyboard group -- the framework never rewrites `checked` itself.
//!
//! `cargo run --example controlled_radio -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CONTROLLED_RADIO_CSS_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/examples/controlled_radio.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled radio",
        CONTROLLED_RADIO_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let size = use_signal(|| "medium".to_string());
    let (pick_small, pick_medium, pick_large) = (size.clone(), size.clone(), size.clone());

    view! {
        <div class="page">
            <p class="instructions">
                {"Click, or Tab in and use the arrow keys. The disabled option is skipped."}
            </p>

            <div class="row">
                <input
                    id="size-small"
                    class="radio"
                    type="radio"
                    name="size"
                    checked={size.get() == "small"}
                    onclick={move || pick_small.set("small".to_string())}
                />
                <label class="field-label" for="size-small">{"Small"}</label>
            </div>
            <div class="row">
                <input
                    id="size-medium"
                    class="radio"
                    type="radio"
                    name="size"
                    checked={size.get() == "medium"}
                    onclick={move || pick_medium.set("medium".to_string())}
                />
                <label class="field-label" for="size-medium">{"Medium"}</label>
            </div>
            <div class="row">
                <input
                    id="size-huge"
                    class="radio"
                    type="radio"
                    name="size"
                    disabled="true"
                />
                <label class="field-label" for="size-huge">{"Huge (disabled)"}</label>
            </div>
            <div class="row">
                <input
                    id="size-large"
                    class="radio"
                    type="radio"
                    name="size"
                    checked={size.get() == "large"}
                    onclick={move || pick_large.set("large".to_string())}
                />
                <label class="field-label" for="size-large">{"Large"}</label>
            </div>
            <p class="status">{format!("Selected: {}", size.get())}</p>
        </div>
    }
}
