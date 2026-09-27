//! A visual check for `border-radius`, live: a rounded card with a border,
//! a pill, a circle, an elliptical corner, per-corner radii, a shadow,
//! a translucent panel with a `backdrop-filter`, and an `overflow: hidden`
//! frame that clips its child to the rounded corner. Clicking a box only
//! counts where it's drawn: the cut corners of the circle don't take the
//! click, which the status line reports.
//!
//! `cargo run --example border_radius -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const BORDER_RADIUS_CSS_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/examples/border_radius.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui border radius",
        BORDER_RADIUS_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let clicks = use_signal(|| 0_u32);
    let count_click = clicks.clone();

    view! {
        <div class="page">
            <p class="instructions">
                {"Rounded corners on backgrounds, borders, shadows, backdrop-filter and \
                  overflow clipping. Click the circle's corner: it misses."}
            </p>
            <div class="row">
                <div class="card"></div>
                <div class="pill"></div>
                <button class="circle" onclick={move || count_click.set(count_click.get() + 1)}>
                </button>
                <div class="ellipse"></div>
                <div class="corners"></div>
            </div>
            <p class="status">{format!("Circle clicks: {}", clicks.get())}</p>
            <div class="row">
                <div class="shadowed"></div>
                <div class="inset"></div>
                <div class="frame">
                    <div class="content"></div>
                </div>
                <div class="backdrop-stage">
                    <div class="glass"></div>
                </div>
            </div>
        </div>
    }
}
