//! A real modal `Dialog`, live (built on `Portal`, which still renders as
//! an independent, unclipped overlay root under the hood): opening it
//! moves keyboard focus inside automatically; Tab/Shift+Tab cycle only
//! within it -- a real focus trap, confirmed by tabbing past "Close": it
//! wraps back to the first focusable element inside, never escaping to
//! "Open dialog" behind it; Escape closes it; clicking the backdrop
//! closes it; clicking the dialog box itself does not (`dispatch_click`
//! targets exactly the hit-tested node, no bubbling); closing it by any
//! of these restores focus to "Open dialog".
//!
//! `cargo run --example portal -p florui-example-app`

use florui::prelude::*;
use florui_platform::{Dialog, DialogProps};
use florui_reactive::use_signal;
use florui_style::Rgba;

const PORTAL_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/portal.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui portal",
        PORTAL_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let open = use_signal(|| false);
    let open_trigger = open.clone();
    let close_from_escape = open.clone();
    let close_from_backdrop = open.clone();
    let close_from_button = open.clone();

    view! {
        <div class="page">
            <p class="instructions">
                {"Open the dialog: focus moves inside automatically, Tab/Shift+Tab cycle only \
                  within it, Escape or the backdrop closes it (the dialog box itself does not), \
                  and closing it returns focus to this button."}
            </p>
            <p class="status">
                {if open.get() { "Dialog is open" } else { "Dialog is closed" }}
            </p>
            <button class="action" onclick={move || open_trigger.set(true)}>
                {"Open dialog"}
            </button>
            {open.get().then(move || view! {
                <Dialog
                    label={"Example dialog".to_string()}
                    onclose={Handler::new(move || close_from_escape.set(false))}
                >
                    <div class="backdrop" onclick={move || close_from_backdrop.set(false)}>
                        <div class="dialog">
                            <p class="dialog-title">{"Real modal content"}</p>
                            <p class="dialog-body">
                                {"Escape or the backdrop closes this. Tab never escapes it."}
                            </p>
                            <button
                                class="action"
                                onclick={move || close_from_button.set(false)}
                            >
                                {"Close"}
                            </button>
                        </div>
                    </div>
                </Dialog>
            })}
        </div>
    }
}
