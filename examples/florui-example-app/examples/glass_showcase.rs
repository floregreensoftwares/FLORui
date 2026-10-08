//! Glass built from plain CSS: a translucent fill, a border, a shadow and
//! `backdrop-filter`, over a moving, high-contrast backdrop. Panels nest (a
//! menu over a card), carry text over and under them, and hold controls.
//!
//! Three modes: opaque (no glass at all), glass (CSS only), and advanced,
//! which opts the panels in to the refracting material with
//! `--florui-glass: refract` and its parameters. The controls also set the
//! blur radius, the refraction, the material's quality and whether the
//! backdrop moves; the line at the bottom reports what is in effect, after
//! the material's limits. `--freeze <ms>` stops animation time at that
//! instant, so a capture of the moving scene is reproducible. The stylesheet
//! reloads when saved.
//!
//! No claim is made of matching any operating system's material.
//!
//! `cargo run --example glass_showcase -p florui-example-app [-- --freeze 1500]`

use florui::prelude::*;
use florui_paint::glass_effective;
use florui_platform::WindowOptions;
use florui_platform::accessibility::prefers_reduced_motion;
use florui_reactive::use_signal;
use florui_style::{GlassMaterial, GlassQuality, Rgba};

const CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/glass_showcase.css");

const BLURS: [u32; 3] = [8, 16, 28];
const REFRACTIONS: [f32; 4] = [6.0, 12.0, 20.0, 48.0];
const EDGE: f32 = 28.0;
const LIGHT_ANGLE: f32 = 315.0;
const LIGHT_STRENGTH: f32 = 0.5;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Opaque,
    Glass,
    Advanced,
}

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
    let mode = use_signal(|| Mode::Glass);
    let blur_step = use_signal(|| 1usize);
    let refraction_step = use_signal(|| 1usize);
    let reduced = use_signal(|| false);
    let moving = use_signal(|| true);
    let fast = use_signal(|| false);

    let blur = BLURS[blur_step.get() % BLURS.len()];
    let refraction = REFRACTIONS[refraction_step.get() % REFRACTIONS.len()];
    let quality = if reduced.get() {
        GlassQuality::Reduced
    } else {
        GlassQuality::Full
    };
    let material = GlassMaterial {
        refraction,
        edge: EDGE,
        light_angle: LIGHT_ANGLE,
        light_strength: LIGHT_STRENGTH,
        quality,
    };

    let system_reduces_motion = prefers_reduced_motion();
    let motion = if !moving.get() {
        "off (set here)"
    } else if system_reduces_motion {
        "off (system reduced motion)"
    } else {
        "on"
    };
    let fast_note = if fast.get() {
        " (fast: large blurs on a shrunk copy)"
    } else {
        ""
    };
    let status = match mode.get() {
        Mode::Opaque => format!("mode: opaque | glass panels: 0 | motion: {motion}"),
        Mode::Glass => {
            format!("mode: glass | blur: {blur}px{fast_note} | glass panels: 4 | motion: {motion}")
        }
        Mode::Advanced => {
            let effective = glass_effective(&material)
                .expect("the quality here is never off, so the material is in effect");
            format!(
                "mode: advanced | blur: {blur}px{fast_note} | refraction: {}px{} | edge: {}px | quality: {} \
                 | glass panels: 4 | motion: {motion}",
                effective.refraction,
                if effective.clamped {
                    format!(" (asked {refraction}px, cut to the edge band)")
                } else {
                    String::new()
                },
                effective.edge,
                if reduced.get() { "reduced" } else { "full" },
            )
        }
    };

    let root_class = format!(
        "stage {} blur-{} {}",
        match mode.get() {
            Mode::Opaque => "glass-off",
            Mode::Glass | Mode::Advanced => "glass-on",
        },
        blur,
        if moving.get() { "moving" } else { "still" },
    );
    // Custom properties inherit, so the material set here reaches every panel
    // that has a `backdrop-filter`.
    let material_style = if mode.get() == Mode::Advanced {
        format!(
            "--florui-glass: refract; --florui-glass-refraction: {refraction}px; \
             --florui-glass-edge: {EDGE}px; --florui-glass-light-angle: {LIGHT_ANGLE}deg; \
             --florui-glass-light-strength: {LIGHT_STRENGTH}; --florui-glass-quality: {};",
            if reduced.get() { "reduced" } else { "full" }
        )
    } else {
        String::new()
    };
    let material_style = if fast.get() {
        format!("{material_style} --florui-backdrop-blur: fast;")
    } else {
        material_style
    };

    let set_opaque = mode.clone();
    let set_glass = mode.clone();
    let set_advanced = mode.clone();
    let cycle_blur = blur_step.clone();
    let cycle_refraction = refraction_step.clone();
    let toggle_quality = reduced.clone();
    let toggle_motion = moving.clone();
    let toggle_fast = fast.clone();
    let seg = |active: bool| if active { "seg on" } else { "seg" };

    view! {
        <div class={root_class} style={material_style}>
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
                    <button class={seg(mode.get() == Mode::Opaque)} onclick={move || set_opaque.set(Mode::Opaque)}>
                        {"Opaque"}
                    </button>
                    <button class={seg(mode.get() == Mode::Glass)} onclick={move || set_glass.set(Mode::Glass)}>
                        {"Glass"}
                    </button>
                    <button class={seg(mode.get() == Mode::Advanced)} onclick={move || set_advanced.set(Mode::Advanced)}>
                        {"Advanced"}
                    </button>
                    <div class="divider"></div>
                    <button class="seg" onclick={move || cycle_blur.set(cycle_blur.get() + 1)}>
                        {format!("Blur {blur}px")}
                    </button>
                    <button class="seg" onclick={move || cycle_refraction.set(cycle_refraction.get() + 1)}>
                        {format!("Refraction {refraction}px")}
                    </button>
                    <button class="seg" onclick={move || toggle_fast.set(!toggle_fast.get())}>
                        {if fast.get() { "Blur: fast" } else { "Blur: exact" }}
                    </button>
                    <button class="seg" onclick={move || toggle_quality.set(!toggle_quality.get())}>
                        {if reduced.get() { "Quality: reduced" } else { "Quality: full" }}
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
