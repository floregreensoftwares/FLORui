//! The built-in `<select>` chevron and checked-checkbox check mark --
//! neither had any visual indicator at all before this delivery (a real
//! gap, not a placeholder to migrate). Both reuse the same themable-SVG
//! pipeline `<icon>` uses: their color follows the control's own
//! resolved `color` (`currentColor`, same mechanism), and
//! `--florui-appearance: none` removes the built-in decoration entirely
//! (a real custom property, since real CSS's `appearance` isn't
//! reachable from this project's Stylo configuration -- see this
//! delivery's own doc).
//!
//! Live-interactive: click the checkbox to see its check mark appear/
//! disappear for real, not just a static screenshot.
//!
//! `cargo run --example control_icons -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CONTROL_ICONS_CSS_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/examples/control_icons.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui built-in control icons",
        CONTROL_ICONS_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let checked = use_signal(|| true);
    let toggle_checked = checked.clone();

    view! {
        <div class="page">
            <p class="instructions">
                {"The built-in chevron/check, colored by each swatch's own \
                  resolved color -- click the checkbox to see its check \
                  mark appear and disappear for real."}
            </p>
            <div class="row">
                <div class="swatch">
                    <select class="select-white" />
                    <p class="swatch-label">{"select (white)"}</p>
                </div>
                <div class="swatch">
                    <select class="select-amber" />
                    <p class="swatch-label">{"select (amber)"}</p>
                </div>
                <div class="swatch">
                    <select class="select-hidden" />
                    <p class="swatch-label">{"select (appearance: none)"}</p>
                </div>
            </div>
            <div class="row">
                <div class="swatch">
                    <input
                        id="live-checkbox"
                        class="checkbox-green"
                        type="checkbox"
                        checked={checked.get()}
                        onclick={move || toggle_checked.set(!toggle_checked.get())}
                    />
                    <p class="swatch-label">{"click to toggle"}</p>
                </div>
                <div class="swatch">
                    <input class="checkbox-blue" type="checkbox" checked="true" />
                    <p class="swatch-label">{"checked (blue)"}</p>
                </div>
                <div class="swatch">
                    <input class="checkbox-blue" type="checkbox" checked="false" />
                    <p class="swatch-label">{"unchecked"}</p>
                </div>
            </div>
        </div>
    }
}
