//! `min-width` and `min-height`, live. Resize the window:
//!
//! - the page fills the window with `min-height` and keeps its footer at the
//!   bottom edge however tall the window is;
//! - the green flex item shrinks with its sibling until it reaches its
//!   `min-width`, the grey one keeps shrinking;
//! - the percentage minimums resolve against the 300x100 frame around them.
//!
//! `cargo run --example min_size -p florui-example-app`

use florui::prelude::*;
use florui_style::Rgba;

const CSS: &str = include_str!("min_size.css");

fn main() {
    florui_platform::run(
        "Florui min-width and min-height",
        CSS,
        Rgba::opaque(0x10, 0x10, 0x14),
        root,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn root() -> Element {
    view! {
        <div class="page">
            <div class="main">
                <div class="title">{"min-width and min-height"}</div>

                <div class="label">{"min-width holds a flex item (green: 220px, grey: none)"}</div>
                <div class="row">
                    <div class="held">{"min-width: 220px"}</div>
                    <div class="free">{"no minimum"}</div>
                </div>

                <div class="label">{"percentage minimums in a 300x100 frame (50% x 50%)"}</div>
                <div class="frame">
                    <div class="half">{"50% x 50%"}</div>
                </div>
            </div>
            <div class="footer">{"min-height fills the window; this stays at the bottom"}</div>
        </div>
    }
}
