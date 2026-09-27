//! A real `<select>`, matching native `<select>` 1:1: click or Tab+Enter/
//! Space opens it, arrow keys move a *highlight* while open (Enter/click
//! commits it, Escape/click-outside cancels without changing the value),
//! Up/Down change the value directly while *closed* (no opening), and the
//! closed box auto-sizes to its widest option, not just the selected
//! one. The dropdown itself escapes the small `overflow: hidden` box it
//! sits in below (a real anchored overlay, not a clipped `position:
//! absolute` child).
//!
//! Every bit of state (`open`, `selected`, `active`) is this app's own
//! `Signal` -- the framework only reports intents (`onclick`/
//! `onactivate`/`ondismiss`), never rewrites markup itself, the same
//! contract every other control here already follows. `active` tracks
//! the keyboard highlight separately from `selected` (the committed
//! value) -- this app resyncs it to the committed value whenever the
//! dropdown opens or closes, matching real HTML's own behavior. Options
//! are grouped under two `<optgroup>`s to show their own bold `label`
//! headers.
//!
//! `cargo run --example controlled_select -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CONTROLLED_SELECT_CSS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/controlled_select.css"
);

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled select",
        CONTROLLED_SELECT_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let is_open = use_signal(|| false);
    let value = use_signal(|| "medium".to_string());
    let active = use_signal(|| "medium".to_string());

    let toggle = {
        let (is_open, value, active) = (is_open.clone(), value.clone(), active.clone());
        move || {
            let opening = !is_open.get();
            if opening {
                active.set(value.get());
            }
            is_open.set(opening);
        }
    };
    let close = {
        let (is_open, value, active) = (is_open.clone(), value.clone(), active.clone());
        move || {
            active.set(value.get());
            is_open.set(false);
        }
    };
    let pick = |choice: &'static str| {
        let (value, active) = (value.clone(), active.clone());
        move || {
            value.set(choice.to_string());
            active.set(choice.to_string());
        }
    };
    let highlight = |choice: &'static str| {
        let active = active.clone();
        move || active.set(choice.to_string())
    };

    view! {
        <div class="page">
            <p class="instructions">
                {"Click, or Tab in and press Enter/Space. Arrow keys preview \
                  an option while open (Escape cancels without changing the \
                  value); closed, they change the value directly. The \
                  dropdown escapes the scrolling box below instead of being \
                  clipped by it, and the closed box sizes to its widest \
                  option, not just the selected one."}
            </p>
            <div class="clip-demo">
                <select
                    id="size"
                    class="select"
                    open={is_open.get()}
                    onclick={toggle}
                    ondismiss={close}
                >
                    <optgroup label="Standard sizes">
                        <option
                            value="small"
                            selected={value.get() == "small"}
                            active={active.get() == "small"}
                            onclick={pick("small")}
                            onactivate={highlight("small")}
                        >
                            {"Small"}
                        </option>
                        <option
                            value="medium"
                            selected={value.get() == "medium"}
                            active={active.get() == "medium"}
                            onclick={pick("medium")}
                            onactivate={highlight("medium")}
                        >
                            {"Medium"}
                        </option>
                    </optgroup>
                    <optgroup label="Bulk sizes">
                        <option
                            value="large"
                            selected={value.get() == "large"}
                            active={active.get() == "large"}
                            onclick={pick("large")}
                            onactivate={highlight("large")}
                        >
                            {"Large"}
                        </option>
                        <option
                            value="extra-large"
                            selected={value.get() == "extra-large"}
                            active={active.get() == "extra-large"}
                            onclick={pick("extra-large")}
                            onactivate={highlight("extra-large")}
                        >
                            {"Extra Large"}
                        </option>
                    </optgroup>
                </select>
                <p class="filler">{"This box clips its own content..."}</p>
                <p class="filler">{"...but not the select's dropdown."}</p>
            </div>
            <p class="status">{format!("Selected: {}", value.get())}</p>
        </div>
    }
}
