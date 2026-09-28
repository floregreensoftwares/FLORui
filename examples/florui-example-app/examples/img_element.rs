//! A real `<img>`, live: five `object-fit` values on the same source in
//! identical boxes, `object-position` corners, a real src swap (a
//! different real PNG, different intrinsic size, loaded from disk), and
//! a real load failure (a path that doesn't exist).
//!
//! `cargo run --example img_element -p florui-example-app`

use florui::prelude::*;
use florui_reactive::use_signal;
use florui_style::Rgba;

const IMG_ELEMENT_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/img_element.css");
const LANDSCAPE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "\\assets\\images\\landscape.png"
);
const PORTRAIT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "\\assets\\images\\portrait.png");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui <img>",
        IMG_ELEMENT_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let showing_portrait = use_signal(|| false);
    let toggle = showing_portrait.clone();
    let current_src = if showing_portrait.get() {
        PORTRAIT_PATH
    } else {
        LANDSCAPE_PATH
    };

    view! {
        <div class="page">
            <p class="instructions">
                {"Every box below is the same 140x100 size; only object-fit/object-position differ."}
            </p>

            <p class="section-title">{"object-fit"}</p>
            <div class="row">
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="fit-fill" src={LANDSCAPE_PATH} alt="A landscape photo, stretched to fill" />
                    </div>
                    <p class="swatch-label">{"fill"}</p>
                </div>
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="fit-contain" src={LANDSCAPE_PATH} alt="A landscape photo, contained" />
                    </div>
                    <p class="swatch-label">{"contain"}</p>
                </div>
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="fit-cover" src={LANDSCAPE_PATH} alt="A landscape photo, covering" />
                    </div>
                    <p class="swatch-label">{"cover"}</p>
                </div>
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="fit-none" src={LANDSCAPE_PATH} alt="A landscape photo, unscaled" />
                    </div>
                    <p class="swatch-label">{"none"}</p>
                </div>
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="fit-scale-down" src={LANDSCAPE_PATH} alt="A landscape photo, scaled down" />
                    </div>
                    <p class="swatch-label">{"scale-down"}</p>
                </div>
            </div>

            <p class="section-title">{"object-position (object-fit: none)"}</p>
            <div class="row">
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="position-top-left" src={LANDSCAPE_PATH} alt="A landscape photo, top-left" />
                    </div>
                    <p class="swatch-label">{"0% 0%"}</p>
                </div>
                <div class="swatch">
                    <div class="swatch-box">
                        <img class="position-bottom-right" src={LANDSCAPE_PATH} alt="A landscape photo, bottom-right" />
                    </div>
                    <p class="swatch-label">{"100% 100%"}</p>
                </div>
            </div>

            <p class="section-title">{"src swap -- a different real file, a different real intrinsic size"}</p>
            <p class="current-src">{format!("Showing: {}", if showing_portrait.get() { "portrait.png (120x180)" } else { "landscape.png (240x160)" })}</p>
            <button class="swap-button" onclick={move || toggle.set(!toggle.get())}>
                {"Swap image"}
            </button>
            <div class="swapped-box">
                <img class="fit-contain" src={current_src} alt="The currently selected photo" />
            </div>

            <p class="section-title">{"real load failure -- a src that does not exist"}</p>
            <div class="broken-box">
                <img class="fit-contain" src="does/not/exist.png" alt="This image intentionally fails to load" />
            </div>
        </div>
    }
}
