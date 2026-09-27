//! End-to-end check against a real Chromium binary: for each fixture
//! under `fixtures/reference/`, launches Chromium, captures its HTML/CSS,
//! and compares it against Florui's own real style/layout/paint render of
//! the same bare element (see `florui_conformance::engine`) — the actual
//! proof that the framework's default element stylesheet (`h1`–`h6`'s
//! font-size/margin scale, `p`'s margin, block display) matches a real
//! browser, not just that its CSS numbers look plausible.
//!
//! `#[ignore]`d because no Chromium revision is pinned/vendored for CI in
//! this slice (see `driver.rs`'s `ChromiumOptions::executable` docs). Run
//! locally with an explicit binary:
//!
//! ```sh
//! FLORUI_CHROMIUM_TEST="C:/Program Files/Google/Chrome/Application/chrome.exe" \
//!     cargo test -p florui-conformance -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use florui_conformance::driver::{ChromiumDriver, ChromiumOptions};
use florui_conformance::engine::render_fixture;
use florui_conformance::geometry::compare_geometry;
use florui_conformance::pixels::{PixelDiffOptions, compare_pixels};
use florui_conformance::reference_fixture::load_reference_fixture;
use florui_conformance::report::{Outcome, classify};

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("reference")
        .join(name)
}

/// Loads `name`'s fixture, captures it in a real Chromium instance,
/// renders the equivalent bare element through Florui's own real
/// pipeline, and asserts the two match per the fixture's own declared
/// [`florui_conformance::reference_fixture::Classification`].
fn run_fixture(name: &str) {
    let Some(chromium) = std::env::var_os("FLORUI_CHROMIUM_TEST") else {
        panic!(
            "set FLORUI_CHROMIUM_TEST to a Chrome/Edge/Chromium executable path to run this test"
        );
    };

    let fixture = load_reference_fixture(&fixture_dir(name)).expect("fixture should load");

    let profile_dir = std::env::temp_dir().join(format!(
        "florui-conformance-it-{name}-{}",
        std::process::id()
    ));
    let driver = ChromiumDriver::launch(ChromiumOptions {
        executable: PathBuf::from(chromium),
        user_data_dir: profile_dir.clone(),
        headless: true,
        launch_timeout: Duration::from_secs(30),
    })
    .expect("driver should launch");

    let capture = driver.capture(&fixture).expect("capture should succeed");

    let viewport = &fixture.manifest.viewport;
    let engine = render_fixture(
        &fixture.manifest.florui,
        &fixture.manifest.florui.css,
        &fixture.manifest.canvas_color,
        viewport.width_css_px,
        viewport.height_css_px,
        viewport.device_pixel_ratio,
    )
    .expect("engine render should succeed");

    let pixel_report = compare_pixels(&capture.image, &engine.image, &PixelDiffOptions::default())
        .expect("dimensions should match");
    // Per-fixture, not a single constant: measured live against Chromium,
    // most fixtures land at or under 1.0px of their own accord (each
    // engine's own float rounding in text shaping/line-height math), but
    // a fixture with real multi-line wrapped content accumulates a few
    // more pixels of that same rounding across more lines — see
    // Classification::Tolerant's own geometry_tolerance_px doc.
    let geometry_report = compare_geometry(
        capture.element_box_css_px,
        engine.element_box_css_px,
        fixture.manifest.classification.geometry_tolerance_px(),
    );

    let outcome = classify(
        &fixture.manifest.classification,
        &pixel_report.summary,
        &geometry_report,
    );

    std::fs::remove_dir_all(&profile_dir).ok();

    assert_eq!(
        outcome,
        Outcome::Pass,
        "fixture {name} failed — pixels: {:?}, geometry: {:?}",
        pixel_report.summary,
        geometry_report
    );
}

macro_rules! fixture_test {
    ($test_name:ident, $fixture_id:literal) => {
        #[test]
        #[ignore = "requires a real Chromium binary, set FLORUI_CHROMIUM_TEST to run"]
        fn $test_name() {
            run_fixture($fixture_id);
        }
    };
}

fixture_test!(div_default_matches_chromium, "div-default");
fixture_test!(p_default_matches_chromium, "p-default");
fixture_test!(h1_default_matches_chromium, "h1-default");
fixture_test!(h2_default_matches_chromium, "h2-default");
fixture_test!(h3_default_matches_chromium, "h3-default");
fixture_test!(h4_default_matches_chromium, "h4-default");
fixture_test!(h5_default_matches_chromium, "h5-default");
fixture_test!(h6_default_matches_chromium, "h6-default");
fixture_test!(card_default_matches_chromium, "card-default");
fixture_test!(card_narrow_matches_chromium, "card-narrow");
fixture_test!(card_long_content_matches_chromium, "card-long-content");
fixture_test!(card_scaled_matches_chromium, "card-scaled");
fixture_test!(
    mixed_inline_default_matches_chromium,
    "mixed-inline-default"
);
fixture_test!(mixed_inline_narrow_matches_chromium, "mixed-inline-narrow");
fixture_test!(
    mixed_inline_long_content_matches_chromium,
    "mixed-inline-long-content"
);
fixture_test!(mixed_inline_scaled_matches_chromium, "mixed-inline-scaled");
fixture_test!(
    grid_z_index_stacking_matches_chromium,
    "grid-z-index-stacking"
);
fixture_test!(
    grid_group_opacity_overlap_matches_chromium,
    "grid-group-opacity-overlap"
);
fixture_test!(box_shadow_default_matches_chromium, "box-shadow-default");
fixture_test!(box_shadow_blur_matches_chromium, "box-shadow-blur");
fixture_test!(border_radius_matches_chromium, "border-radius");
fixture_test!(
    border_radius_overflow_hidden_matches_chromium,
    "border-radius-overflow-hidden"
);
fixture_test!(
    border_radius_box_shadow_matches_chromium,
    "border-radius-box-shadow"
);
fixture_test!(transform_translate_matches_chromium, "transform-translate");
fixture_test!(
    overflow_hidden_clip_matches_chromium,
    "overflow-hidden-clip"
);
fixture_test!(
    overflow_scroll_clip_at_origin_matches_chromium,
    "overflow-scroll-clip-at-origin"
);
fixture_test!(filter_blur_matches_chromium, "filter-blur");
fixture_test!(filter_adjustments_matches_chromium, "filter-adjustments");
fixture_test!(backdrop_filter_matches_chromium, "backdrop-filter");
fixture_test!(custom_properties_matches_chromium, "custom-properties");
fixture_test!(
    media_query_min_width_matches_chromium,
    "media-query-min-width"
);
fixture_test!(
    media_query_min_height_matches_chromium,
    "media-query-min-height"
);
fixture_test!(
    container_query_nested_matches_chromium,
    "container-query-nested"
);
