//! Checks that reaching a state step by step looks the same as being in it
//! from the start. The runtime reuses earlier results in several places (a
//! scroll tick that repaints without laying out, caches of measured text and
//! blurred shadows, clipped nodes that are skipped, animation and input state
//! carried from frame to frame), and each is a place a stale pixel can hide:
//! the same state reached two ways must paint identically.
//!
//! [`assert_matches_clean`] mounts a component and drives it the way a user
//! would (the incremental path), mounts a twin and brings it to the same final
//! state directly (the clean path), settles both, and compares the pixels, where
//! every element is drawn and what an assistive technology would see. A
//! difference writes both images and a diff next to the baselines, and names the
//! elements drawn where they differ.

use std::collections::HashMap;
use std::path::PathBuf;

use florui_platform::HeadlessFrame;
use florui_style::NodeId;
use image::RgbaImage;

use crate::{Harness, Mounted, snapshot};

/// One way to reach a state: how to mount, and what to do afterwards.
pub struct Route<'a> {
    pub harness: Harness,
    pub drive: Box<dyn FnOnce(&mut Mounted) + 'a>,
}

impl<'a> Route<'a> {
    pub fn new(harness: Harness, drive: impl FnOnce(&mut Mounted) + 'a) -> Self {
        Self {
            harness,
            drive: Box::new(drive),
        }
    }
}

/// How many elements a failure names.
const NAMED_ELEMENTS: usize = 6;

/// Reaches a state through `incremental` and through `clean` and asserts the
/// two painted frames agree, element boxes included.
///
/// A path settles after it runs, so it need not. Transitions and animations
/// differ between a window that has been running and a fresh one while they are
/// in flight; mount with `reduced_motion`, or advance the clock past their end
/// in both paths, before comparing.
///
/// # Panics
///
/// Panics naming the first difference. On a pixel difference it also writes
/// `<name>.incremental.png`, `<name>.clean.png` and `<name>.diff.png` into the
/// package's `tests/snapshots` and lists the elements drawn where the frames
/// differ, innermost first, with where each was written when that is known.
pub fn assert_matches_clean(name: &str, incremental: Route<'_>, clean: Route<'_>) {
    let (left, left_frame) = run(incremental);
    let (right, right_frame) = run(clean);

    if left_frame.width != right_frame.width || left_frame.height != right_frame.height {
        panic!(
            "{name}: the two paths painted different sizes: {}x{} incrementally, {}x{} clean",
            left_frame.width, left_frame.height, right_frame.width, right_frame.height
        );
    }

    let incremental_image = snapshot::image_of(&left_frame);
    let clean_image = snapshot::image_of(&right_frame);
    if incremental_image != clean_image {
        let (differing, diff) = snapshot::diff(&clean_image, &incremental_image);
        let region = differing_region(&clean_image, &incremental_image);
        let directory = snapshot::directory();
        std::fs::create_dir_all(&directory).ok();
        let paths: Vec<PathBuf> = ["incremental", "clean", "diff"]
            .iter()
            .map(|kind| directory.join(format!("{name}.{kind}.png")))
            .collect();
        incremental_image.save(&paths[0]).ok();
        clean_image.save(&paths[1]).ok();
        diff.save(&paths[2]).ok();
        let culprits = elements_behind(&left, &left_frame, region);
        panic!(
            "{name}: reaching the state step by step paints differently from being in it from the \
             start: {differing} pixels differ, within x {}..{} and y {}..{}.\n\
             Elements drawn there, innermost first:\n  {}\n\
             Images: {}, {}, {}",
            region.0,
            region.2,
            region.1,
            region.3,
            culprits.join("\n  "),
            paths[0].display(),
            paths[1].display(),
            paths[2].display(),
        );
    }

    if let Some(difference) = first_geometry_difference(&left_frame.bounds, &right_frame.bounds) {
        panic!("{name}: the pixels agree but the boxes differ: {difference}");
    }
    if left_frame.accessibility != right_frame.accessibility {
        let position = left_frame
            .accessibility
            .iter()
            .zip(&right_frame.accessibility)
            .position(|(a, b)| a != b);
        panic!(
            "{name}: the pixels and boxes agree but what an assistive technology sees differs \
             ({} entries incrementally, {} clean; first difference at {:?}: {:?} against {:?})",
            left_frame.accessibility.len(),
            right_frame.accessibility.len(),
            position,
            position.and_then(|i| left_frame.accessibility.get(i)),
            position.and_then(|i| right_frame.accessibility.get(i)),
        );
    }
    drop(right);
}

fn run(path: Route<'_>) -> (Mounted, HeadlessFrame) {
    let mut mounted = path.harness.mount();
    (path.drive)(&mut mounted);
    mounted.settle();
    let frame = mounted.frame();
    (mounted, frame)
}

/// `(left, top, right, bottom)` of every pixel that differs.
fn differing_region(a: &RgbaImage, b: &RgbaImage) -> (u32, u32, u32, u32) {
    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, pixel) in a.enumerate_pixels() {
        if b.get_pixel(x, y) != pixel {
            left = left.min(x);
            top = top.min(y);
            right = right.max(x + 1);
            bottom = bottom.max(y + 1);
        }
    }
    (left, top, right, bottom)
}

fn first_geometry_difference(
    a: &HashMap<NodeId, (f32, f32, f32, f32)>,
    b: &HashMap<NodeId, (f32, f32, f32, f32)>,
) -> Option<String> {
    let mut ids: Vec<&NodeId> = a.keys().chain(b.keys()).collect();
    ids.sort();
    ids.dedup();
    ids.into_iter().find_map(|id| match (a.get(id), b.get(id)) {
        (Some(x), Some(y)) if x == y => None,
        (x, y) => Some(format!(
            "element {id}: {x:?} incrementally, {y:?} clean (x, y, width, height in device pixels)"
        )),
    })
}

/// The elements whose box covers any of `region`, innermost first.
fn elements_behind(
    mounted: &Mounted,
    frame: &HeadlessFrame,
    region: (u32, u32, u32, u32),
) -> Vec<String> {
    let (left, top, right, bottom) = (
        region.0 as f32,
        region.1 as f32,
        region.2 as f32,
        region.3 as f32,
    );
    let arena = mounted.arena();
    let mut covering: Vec<(usize, NodeId)> = frame
        .bounds
        .iter()
        .filter(|(_, (x, y, w, h))| *x < right && x + w > left && *y < bottom && y + h > top)
        .map(|(&id, _)| {
            let mut depth = 0;
            let mut current = arena.parent(id);
            while let Some(parent) = current {
                depth += 1;
                current = arena.parent(parent);
            }
            (depth, id)
        })
        .collect();
    covering.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    covering
        .into_iter()
        .take(NAMED_ELEMENTS)
        .map(|(_, id)| {
            let mut line = format!("<{}", arena.tag(id));
            for class in arena.classes(id) {
                line.push('.');
                line.push_str(class);
            }
            line.push('>');
            if let Some(source) = arena.source(id) {
                line.push_str(&format!(
                    "  ({} {}:{}:{} {})",
                    source.component.unwrap_or("-"),
                    source.file,
                    source.line,
                    source.column,
                    source.path
                ));
            }
            line
        })
        .collect()
}
