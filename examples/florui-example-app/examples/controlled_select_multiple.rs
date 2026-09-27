//! A real `<select multiple>`: always a visible listbox, never a
//! collapsible dropdown -- real HTML has no open/closed state for it at
//! all. Plain click replaces the whole selection with just that option;
//! Ctrl-click toggles it, leaving every other option's own state alone;
//! Shift-click selects every option between the last plain/Ctrl click and
//! this one, inclusive -- real HTML's own multi-select semantics.
//!
//! The framework computes the whole new selection and reports it once via
//! `onselectionchange={move |values: Vec<String>| ...}` on the `<select>`
//! itself, not per-option -- this app just keeps that `Vec<String>` in one
//! `Signal` and derives each `<option>`'s own `selected` from whether it
//! contains that option's value, and highlights it via a real
//! `[selected="true"]` attribute selector in the stylesheet.
//!
//! `cargo run --example controlled_select_multiple -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CSS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/controlled_select_multiple.css"
);

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled select multiple",
        CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let selected = use_signal(|| vec!["banana".to_string()]);

    let on_selection_change = {
        let selected = selected.clone();
        move |values: Vec<String>| selected.set(values)
    };

    let is_selected = |value: &str| selected.get().contains(&value.to_string());

    view! {
        <div class="page">
            <p class="instructions">
                {"Click picks just one. Ctrl-click toggles one without \
                  touching the rest. Shift-click selects the range from \
                  your last plain/Ctrl click to here."}
            </p>
            <select id="fruits" class="listbox" multiple="true" onselectionchange={on_selection_change}>
                <option value="apple" selected={is_selected("apple")}>{"Apple"}</option>
                <option value="banana" selected={is_selected("banana")}>{"Banana"}</option>
                <option value="cherry" selected={is_selected("cherry")}>{"Cherry"}</option>
                <option value="date" selected={is_selected("date")}>{"Date"}</option>
                <option value="elderberry" selected={is_selected("elderberry")}>{"Elderberry"}</option>
            </select>
            <p class="status">{format!("Selected: {}", selected.get().join(", "))}</p>
        </div>
    }
}
