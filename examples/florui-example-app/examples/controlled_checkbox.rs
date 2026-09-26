//! A real `<input type="checkbox">`, live: click or Tab+Space toggles
//! it, a real author `:checked` rule fills it in, Tab reaches it but
//! skips the disabled one, and the platform accessibility bridge
//! (Narrator, on Windows) reports it as a real checkbox with its real
//! toggled state -- not a styled `<button>` standing in for one.
//!
//! No `Binding` here: `checked` is a plain, self-toggling `bool` prop,
//! same shape as `disabled_button.rs`'s own external toggle, just
//! applied to the control's own `onclick` instead of a sibling's.
//!
//! `cargo run --example controlled_checkbox -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CONTROLLED_CHECKBOX_CSS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/controlled_checkbox.css"
);

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled checkbox",
        CONTROLLED_CHECKBOX_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let subscribed = use_signal(|| false);
    let toggle_subscribed = subscribed.clone();

    let disabled_target = use_signal(|| true);

    view! {
        <div class="page">
            <p class="instructions">
                {"Click, or Tab to it and press Space. The disabled one below never toggles."}
            </p>

            <div class="row">
                <input
                    id="subscribe-checkbox"
                    class="checkbox"
                    type="checkbox"
                    checked={subscribed.get()}
                    onclick={move || toggle_subscribed.set(!toggle_subscribed.get())}
                />
                <label class="field-label" for="subscribe-checkbox">
                    {"Subscribe to updates"}
                </label>
            </div>
            <p class="status">{format!("Committed: {}", subscribed.get())}</p>

            <div class="row">
                <input
                    id="disabled-checkbox"
                    class="checkbox"
                    type="checkbox"
                    checked={disabled_target.get()}
                    disabled="true"
                />
                <label class="field-label" for="disabled-checkbox">
                    {"Locked setting (disabled, stays checked)"}
                </label>
            </div>
        </div>
    }
}
