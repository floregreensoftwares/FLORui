//! `<img src="icon.svg">` (contract A): the same source SVG at four very
//! different box sizes on one screen, at once — proves rasterization
//! tracks each box's own real size rather than a single fixed default
//! (a 24px icon and a 220px one both need to look crisp, not the small
//! one oversized or the large one blurry from an undersized raster).
//!
//! `cargo run --example svg_image -p florui-example-app`

use florui::prelude::*;
use florui_style::Rgba;

const SVG_IMAGE_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/svg_image.css");
const ICON_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "\\assets\\images\\icon.svg");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui <img src=\"icon.svg\">",
        SVG_IMAGE_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    view! {
        <div class="page">
            <p class="instructions">
                {"The same real icon.svg, rasterized at each box's own real size."}
            </p>
            <div class="row">
                <div class="swatch">
                    <img class="icon-tiny" src={ICON_PATH} alt="A tiny icon" />
                    <p class="swatch-label">{"24px"}</p>
                </div>
                <div class="swatch">
                    <img class="icon-small" src={ICON_PATH} alt="A small icon" />
                    <p class="swatch-label">{"48px"}</p>
                </div>
                <div class="swatch">
                    <img class="icon-medium" src={ICON_PATH} alt="A medium icon" />
                    <p class="swatch-label">{"96px"}</p>
                </div>
                <div class="swatch">
                    <img class="icon-large" src={ICON_PATH} alt="A large icon" />
                    <p class="swatch-label">{"220px"}</p>
                </div>
            </div>
        </div>
    }
}
