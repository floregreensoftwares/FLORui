//! What it costs to drag the window's edge: the frame each size paints, per
//! screen, and where in the painting the time goes.
//!
//! The print is not part of the normal run, and times only mean something in
//! release:
//!
//! `cargo test --release -p taskboard --test resize -- --ignored --nocapture`

use florui_profile::{Counter, Phase};
use florui_test::{Harness, Mounted, by_id};
use taskboard::app::{App, AppProps};

const SCREENS: [(&str, &str); 3] = [
    ("desk", "nav-desk"),
    ("stack", "nav-stack"),
    ("settings", "nav-settings"),
];

/// The window is mounted at `TASKBOARD_DPR` (1 when unset): a frame painted at a
/// fraction of the window's size, as while it is dragged, has that scale.
fn mounted() -> Mounted {
    let dpr = std::env::var("TASKBOARD_DPR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1.0);
    Harness::new(|| App(AppProps {}))
        .css(&taskboard::stylesheet())
        .viewport(1180.0, 760.0)
        .dpr(dpr)
        .parallel_paint(std::env::var_os("TASKBOARD_BANDS").is_some())
        .mount()
}

const PARTS: [Phase; 7] = [
    Phase::RasterBackdrop,
    Phase::RasterShadows,
    Phase::RasterFill,
    Phase::RasterLayers,
    Phase::RasterBorders,
    Phase::RasterContent,
    Phase::RasterComposite,
];

const CACHES: [Counter; 5] = [
    Counter::LayersDrawnDirect,
    Counter::LayersRendered,
    Counter::LayersReused,
    Counter::ShadowMasksBlurred,
    Counter::ShadowMasksReused,
];

#[test]
fn a_frame_says_where_its_painting_went() {
    let mut mounted = mounted();
    let stack = mounted.get(by_id("nav-stack"));
    mounted.click(stack);
    // A layer is kept the second time it is painted, not the first.
    let _ = mounted.frame();
    let _ = mounted.frame();

    let ((), profile) = mounted.profile(|m| m.resize(1000.0, 700.0));
    let frame = &profile.frames()[0];
    for phase in PARTS {
        assert!(
            frame.phase(phase).is_some(),
            "the stack has {} work to do in a new size",
            phase.name()
        );
    }
    let parts: f64 = PARTS.iter().map(|&p| frame.total(p).as_secs_f64()).sum();
    assert!(
        parts <= frame.total(Phase::Raster).as_secs_f64(),
        "the parts of painting are inside painting"
    );
    assert!(
        profile.counter(Counter::LayersRendered) > 0,
        "a new size draws the layers that scale with it again"
    );
    assert!(
        profile.counter(Counter::LayersReused) > 0,
        "the layers that keep their size are not drawn again"
    );
    assert!(profile.counter(Counter::ShadowMasksBlurred) > 0);
    assert!(profile.counter(Counter::ShadowMasksReused) > 0);
}

#[test]
#[ignore = "prints timings"]
fn each_size_of_a_resize_as_a_frame() {
    let mut mounted = mounted();
    for (screen, key) in SCREENS {
        let node = mounted.get(by_id(key));
        mounted.click(node);
        // The first sizes warm the caches of every thread.
        for step in 0..2 {
            mounted.resize(1000.0 + step as f32, 700.0);
        }
        let sizes = 12;
        let (mut wall, mut raster) = (0.0, 0.0);
        let mut parts = [0.0f64; PARTS.len()];
        let mut caches = [0u64; CACHES.len()];
        for step in 0..sizes {
            let (width, height) = (1000.0 + step as f32 * 9.0, 700.0 + step as f32 * 4.0);
            let (_, profile) = mounted.profile(|mounted| mounted.resize(width, height));
            let frame = &profile.frames()[0];
            wall += frame.wall().as_secs_f64() * 1000.0;
            raster += frame.total(Phase::Raster).as_secs_f64() * 1000.0;
            for (sum, phase) in parts.iter_mut().zip(PARTS) {
                *sum += frame.total(phase).as_secs_f64() * 1000.0;
            }
            for (sum, counter) in caches.iter_mut().zip(CACHES) {
                *sum += profile.counter(counter);
            }
        }
        let each = sizes as f64;
        println!(
            "{screen}: {:.1} ms a frame, {:.1} of it painting",
            wall / each,
            raster / each
        );
        for (sum, phase) in parts.iter().zip(PARTS) {
            println!("    {:<16} {:>6.1} ms", phase.name(), sum / each);
        }
        for (sum, counter) in caches.iter().zip(CACHES) {
            println!(
                "    {:<20} {:>6.1} a frame",
                counter.name(),
                *sum as f64 / each
            );
        }
    }
}

/// What it costs when the pointer goes over a key or a card: the frame painted
/// over the last one, per screen, and where in the painting the time goes.
#[test]
#[ignore = "prints timings"]
fn each_hover_as_a_frame() {
    let mut mounted = Harness::new(|| App(AppProps {}))
        .css(&taskboard::stylesheet())
        .viewport(1180.0, 760.0)
        .repaint(true)
        .mount();
    for (screen, key) in SCREENS {
        let node = mounted.get(by_id(key));
        mounted.click(node);
        let _ = mounted.frame();
        let mut targets = Vec::new();
        for id in ["nav-desk", "nav-stack", "nav-settings"] {
            targets.push(mounted.get(by_id(id)));
        }
        targets.extend(
            mounted
                .all(florui_test::by_class("paper-card"))
                .into_iter()
                .take(4),
        );
        targets.extend(
            mounted
                .all(florui_test::by_class("segment"))
                .into_iter()
                .take(3),
        );
        let rounds = 4;
        let (mut wall, mut raster) = (0.0, 0.0);
        let mut parts = [0.0f64; PARTS.len()];
        let mut frames = 0;
        for _ in 0..rounds {
            for &target in &targets {
                let (_, profile) = mounted.profile(|mounted| mounted.hover(target));
                let frame = &profile.frames()[0];
                wall += frame.wall().as_secs_f64() * 1000.0;
                raster += frame.total(Phase::Raster).as_secs_f64() * 1000.0;
                for (sum, phase) in parts.iter_mut().zip(PARTS) {
                    *sum += frame.total(phase).as_secs_f64() * 1000.0;
                }
                frames += 1;
            }
        }
        let each = frames as f64;
        println!(
            "{screen}: {:.1} ms a hover, {:.1} of it painting",
            wall / each,
            raster / each
        );
        for (sum, phase) in parts.iter().zip(PARTS) {
            println!("    {:<16} {:>6.2} ms", phase.name(), sum / each);
        }
    }
}
