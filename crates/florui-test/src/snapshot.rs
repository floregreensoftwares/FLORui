//! Baseline images. A baseline `name` is `tests/snapshots/name.png` in the
//! package running the test. With `UPDATE_SNAPSHOTS=1` a missing or different
//! baseline is written instead of failing, and the change is then reviewed in
//! the diff like any other file. A mismatch writes `name.actual.png` and a
//! `name.diff.png` marking every differing pixel, next to the baseline.

use std::path::PathBuf;

use florui_platform::HeadlessFrame;
use image::{ImageBuffer, Rgba, RgbaImage};

fn directory() -> PathBuf {
    let root = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    root.join("tests").join("snapshots")
}

fn image_of(frame: &HeadlessFrame) -> RgbaImage {
    ImageBuffer::from_raw(frame.width, frame.height, frame.rgba.clone())
        .expect("a frame holds width x height x 4 bytes")
}

/// # Panics
///
/// Panics when there is no baseline, or when the frame differs from it.
pub(crate) fn assert_matches(name: &str, frame: &HeadlessFrame) {
    let dir = directory();
    let baseline_path = dir.join(format!("{name}.png"));
    let actual = image_of(frame);
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();

    let baseline = image::open(&baseline_path)
        .ok()
        .map(|image| image.to_rgba8());
    match baseline {
        Some(baseline) if baseline == actual => {}
        _ if update => {
            std::fs::create_dir_all(&dir).expect("the snapshot directory can be created");
            actual
                .save(&baseline_path)
                .expect("the baseline image can be written");
        }
        None => panic!(
            "no baseline image at {}. Run with UPDATE_SNAPSHOTS=1 to create it, then review it.",
            baseline_path.display()
        ),
        Some(baseline) => {
            std::fs::create_dir_all(&dir).ok();
            let actual_path = dir.join(format!("{name}.actual.png"));
            let diff_path = dir.join(format!("{name}.diff.png"));
            actual.save(&actual_path).ok();
            let (differing, diff) = diff(&baseline, &actual);
            diff.save(&diff_path).ok();
            panic!(
                "the frame differs from the baseline {name}: {differing} pixels differ \
                 ({}x{} against {}x{}). See {} and {}. If the change is intended, run with \
                 UPDATE_SNAPSHOTS=1 and review the new baseline.",
                actual.width(),
                actual.height(),
                baseline.width(),
                baseline.height(),
                actual_path.display(),
                diff_path.display(),
            );
        }
    }
}

/// How many pixels differ, and an image with each of them in red over a dimmed
/// copy of the baseline.
fn diff(baseline: &RgbaImage, actual: &RgbaImage) -> (usize, RgbaImage) {
    let width = baseline.width().max(actual.width());
    let height = baseline.height().max(actual.height());
    let mut out = RgbaImage::new(width, height);
    let mut differing = 0;
    for y in 0..height {
        for x in 0..width {
            let a = baseline.get_pixel_checked(x, y);
            let b = actual.get_pixel_checked(x, y);
            let pixel = if a == b {
                let p = a.copied().unwrap_or(Rgba([0, 0, 0, 0]));
                Rgba([p[0] / 3, p[1] / 3, p[2] / 3, 255])
            } else {
                differing += 1;
                Rgba([255, 0, 0, 255])
            };
            out.put_pixel(x, y, pixel);
        }
    }
    (differing, out)
}
