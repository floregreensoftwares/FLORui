//! A real `<a>`, live: the plain link both fires its own `onclick` (the
//! counter below it increments) and opens `https://example.com` in the
//! OS's own default browser -- its default action, same as a real
//! anchor's. The second link calls `event.prevent_default()`, so its
//! `onclick` still fires but the browser never opens. Tab moves focus
//! between them (a focus ring shows which one); Enter activates the
//! focused one the same way a real click does; Space does nothing to
//! either, unlike a button.
//!
//! `cargo run --example anchor -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const ANCHOR_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/anchor.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui anchor",
        ANCHOR_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let plain_clicks = use_signal(|| 0);
    let on_plain_click = plain_clicks.clone();
    let prevented_clicks = use_signal(|| 0);
    let on_prevented_click = prevented_clicks.clone();

    view! {
        <div class="page">
            <p class="status">
                {format!("Plain link clicked {} time(s)", plain_clicks.get())}
            </p>
            <p class="status">
                {format!("Prevented link clicked {} time(s)", prevented_clicks.get())}
            </p>
            <a
                class="link"
                href="https://example.com"
                onclick={move || on_plain_click.set(on_plain_click.get() + 1)}
            >
                {"Opens example.com"}
            </a>
            <a
                class="link"
                href="https://example.com"
                onclick={move |event: &Event| {
                    event.prevent_default();
                    on_prevented_click.set(on_prevented_click.get() + 1);
                }}
            >
                {"Never opens (prevent_default)"}
            </a>
        </div>
    }
}
