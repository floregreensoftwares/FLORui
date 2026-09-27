//! A compact retro chiptune player -- a CSS-fidelity showcase, not a
//! feature demo of the framework's interaction primitives. Real, audible
//! playback (synthesized tones, not licensed audio -- see `audio.rs`),
//! custom window chrome, live-swappable skins via real CSS custom
//! properties, real `box-shadow`/`transform` depth (no `<img>` and no
//! `border-radius` exist in florui yet, so the "cover art" is built from
//! real CSS shapes instead of either), and a real `@keyframes` crossfade
//! on track change.
//!
//! `cargo run -p florui-player`

mod app;
mod audio;
mod components;
mod theme;

use std::rc::Rc;

use florui::prelude::*;
use florui_platform::appearance::DecorationMode;
use florui_platform::{WindowOptions, run_with_css_reload_and_options};
use florui_style::Rgba;

use app::{App, AppProps};

const APP_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/app.css");

fn main() {
    let player = Rc::new(audio::Player::new());

    run_with_css_reload_and_options(
        "florui-player",
        APP_CSS_PATH,
        Rgba::opaque(0x05, 0x14, 0x0a),
        WindowOptions {
            decorations: DecorationMode::Custom,
            size: Some((480.0, 560.0)),
            min_size: Some((260.0, 420.0)),
            ..WindowOptions::default()
        },
        move || {
            let player = Rc::clone(&player);
            view! { <App player={player} /> }
        },
    )
    .expect("event loop should not fail on a real desktop session");
}
