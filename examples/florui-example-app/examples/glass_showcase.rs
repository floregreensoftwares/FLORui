//! Glass built from plain CSS: a translucent fill, a border, a shadow and
//! `backdrop-filter`, over a moving, high-contrast backdrop. Panels nest (a
//! menu over a card), carry text over and under them, and hold controls.
//!
//! The controls switch between an opaque and a glass rendering, the blur
//! radius and whether the backdrop moves; the line at the bottom reports
//! what is in effect. `--freeze <ms>` stops animation time at that instant,
//! so a capture of the moving scene is reproducible. The stylesheet reloads
//! when saved.
//!
//! This is CSS glass only: no refraction or edge lighting, and no claim of
//! matching any operating system's material.
//!
//! `cargo run --example glass_showcase -p florui-example-app [-- --freeze 1500]`

use florui::prelude::*;
use florui_platform::WindowOptions;
use florui_platform::accessibility::prefers_reduced_motion;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/glass_showcase.css");

const BLURS: [u32; 3] = [8, 16, 28];

fn main() {
    let frozen = frozen_animation_time();
    florui_platform::run_with_css_reload_and_options(
        "Florui -- glass",
        CSS_PATH,
        Rgba::opaque(0x0c, 0x0d, 0x14),
        WindowOptions {
            size: Some((1100.0, 720.0)),
            min_size: Some((820.0, 560.0)),
            frozen_animation_time: frozen,
            ..WindowOptions::default()
        },
        showcase,
    )
    .expect("event loop should not fail on a real desktop session");
}

/// The value of `--freeze <ms>`, in seconds.
fn frozen_animation_time() -> Option<f64> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--freeze" {
            return args.next()?.parse::<f64>().ok().map(|ms| ms / 1000.0);
        }
    }
    None
}

fn showcase() -> Element {
    let glass = use_signal(|| true);
    let blur_step = use_signal(|| 1usize);
    let moving = use_signal(|| true);

    let blur = BLURS[blur_step.get() % BLURS.len()];
    let system_reduces_motion = prefers_reduced_motion();
    let motion = if !moving.get() {
        "off (set here)"
    } else if system_reduces_motion {
        "off (system reduced motion)"
    } else {
        "on"
    };
    let panels = if glass.get() { 4 } else { 0 };
    let status = format!(
        "mode: {} | blur: {}px | glass panels: {} | motion: {}",
        if glass.get() { "glass" } else { "opaque" },
        if glass.get() { blur } else { 0 },
        panels,
        motion,
    );

    let root_class = format!(
        "stage {} blur-{} {}",
        if glass.get() { "glass-on" } else { "glass-off" },
        blur,
        if moving.get() { "moving" } else { "still" },
    );

    let set_glass = glass.clone();
    let set_opaque = glass.clone();
    let cycle_blur = blur_step.clone();
    let toggle_motion = moving.clone();

    view! {
        <div class={root_class}>
            <div class="world">
                <div class="blob blob-a"></div>
                <div class="blob blob-b"></div>
                <div class="blob blob-c"></div>
                <div class="bars">
                    <div class="bar bar-1"></div>
                    <div class="bar bar-2"></div>
                    <div class="bar bar-3"></div>
                    <div class="bar bar-4"></div>
                    <div class="bar bar-5"></div>
                    <div class="bar bar-6"></div>
                </div>
                <div class="banner">{"FLORUI GLASS"}</div>
            </div>
            <div class="ui">
                <div class="toolbar panel">
                    <button
                        class={if glass.get() { "seg" } else { "seg on" }}
                        onclick={move || set_opaque.set(false)}
                    >
                        {"Opaque"}
                    </button>
                    <button
                        class={if glass.get() { "seg on" } else { "seg" }}
                        onclick={move || set_glass.set(true)}
                    >
                        {"Glass"}
                    </button>
                    <div class="divider"></div>
                    <button class="seg" onclick={move || cycle_blur.set(cycle_blur.get() + 1)}>
                        {format!("Blur {blur}px")}
                    </button>
                    <button class="seg" onclick={move || toggle_motion.set(!toggle_motion.get())}>
                        {if moving.get() { "Pause motion" } else { "Play motion" }}
                    </button>
                </div>
                <div class="row">
                    <div class="card panel">
                        <h1 class="card-title">{"Glass over a moving scene"}</h1>
                        <p class="card-text">
                            {"The backdrop moves behind this card. What shows through is blurred \
                              and saturated by the panel, and the text stays readable on top."}
                        </p>
                        <div class="actions">
                            <button class="primary">{"Continue"}</button>
                            <button class="ghost">{"Not now"}</button>
                        </div>
                        <div class="menu panel">
                            <div class="menu-item">{"Rename"}</div>
                            <div class="menu-item">{"Duplicate"}</div>
                            <div class="menu-item">{"Move to trash"}</div>
                        </div>
                    </div>
                    <div class="side">
                        <div class="tile panel">
                            <span class="tile-label">{"Volume"}</span>
                            <div class="track"><div class="fill"></div><div class="thumb"></div></div>
                        </div>
                        <div class="tile panel">
                            <span class="tile-label">{"Over the banner"}</span>
                            <span class="tile-value">{"62%"}</span>
                        </div>
                    </div>
                </div>
                <div class="status">{status}</div>
            </div>
        </div>
    }
}
