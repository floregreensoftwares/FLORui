//! `<icon src="check.svg">` (contract B): the same `currentColor` source
//! icon, rasterized in five differently-colored swatches at once — proves
//! theming actually reads each swatch's own resolved `color` (real CSS
//! cascade, nothing icon-specific) rather than a single baked-in tint,
//! and that distinct colors of the same source produce distinct cached
//! rasters rather than all sharing whichever one rasterized first.
//!
//! `cargo run --example icon_theming -p florui-example-app`

use florui::prelude::*;
use florui_style::Rgba;

const ICON_THEMING_CSS_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/examples/icon_theming.css");
const CHECK_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "\\assets\\images\\check.svg");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui <icon src=\"check.svg\">",
        ICON_THEMING_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    view! {
        <div class="page">
            <p class="instructions">
                {"The same real check.svg (fill=\"currentColor\"), themed by each \
                  swatch's own resolved color."}
            </p>
            <div class="row">
                <div class="swatch">
                    <icon class="icon-red" src={CHECK_PATH} />
                    <p class="swatch-label">{"red"}</p>
                </div>
                <div class="swatch">
                    <icon class="icon-green" src={CHECK_PATH} />
                    <p class="swatch-label">{"green"}</p>
                </div>
                <div class="swatch">
                    <icon class="icon-blue" src={CHECK_PATH} />
                    <p class="swatch-label">{"blue"}</p>
                </div>
                <div class="swatch">
                    <icon class="icon-amber" src={CHECK_PATH} />
                    <p class="swatch-label">{"amber"}</p>
                </div>
                <div class="swatch">
                    <icon class="icon-inherited" src={CHECK_PATH} />
                    <p class="swatch-label">{"inherited (white)"}</p>
                </div>
            </div>
        </div>
    }
}
