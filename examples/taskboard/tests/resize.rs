//! What it costs to drag the window's edge: the frame each size paints, per
//! screen. Not part of the normal run; times only mean something in release.
//!
//! `cargo test --release -p taskboard --test resize -- --ignored --nocapture`

use florui_profile::Phase;
use florui_test::{Harness, by_id};
use taskboard::app::{App, AppProps};

#[test]
#[ignore = "prints timings"]
fn each_size_of_a_resize_as_a_frame() {
    let mut mounted = Harness::new(|| App(AppProps {}))
        .css(&taskboard::stylesheet())
        .viewport(1180.0, 760.0)
        .mount();
    for (screen, key) in [
        ("desk", "nav-desk"),
        ("stack", "nav-stack"),
        ("settings", "nav-settings"),
    ] {
        let node = mounted.get(by_id(key));
        mounted.click(node);
        let sizes = 12;
        let (mut wall, mut raster) = (0.0, 0.0);
        for step in 0..sizes {
            let (width, height) = (1000.0 + step as f32 * 9.0, 700.0 + step as f32 * 4.0);
            let (_, profile) = mounted.profile(|mounted| mounted.resize(width, height));
            let frame = &profile.frames()[0];
            wall += frame.wall().as_secs_f64() * 1000.0;
            raster += frame.total(Phase::Raster).as_secs_f64() * 1000.0;
        }
        println!(
            "{screen}: {:.1} ms a frame, {:.1} of it painting",
            wall / sizes as f64,
            raster / sizes as f64
        );
    }
}
