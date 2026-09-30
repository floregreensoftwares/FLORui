//! Time and peak heap for the effects that allocate offscreen surfaces:
//! `filter` (blur in particular), `box-shadow` with a blur radius,
//! `backdrop-filter`, and an ordered filter chain. Their cost scales with the
//! affected area and the blur radius, which makes them the likeliest source
//! of a frame-time regression.
//!
//! Each case prints a `MEMORY` line before it is timed: the peak heap one
//! `paint_to_buffer` call reaches, and how much of it the effect adds over
//! the same scene painted without it. The figure counts every allocation
//! (the output canvas included, in both), which is what "offscreen memory"
//! means to a caller, not only the surfaces this crate names. A case also
//! checks that the effect changed the pixels, so a stylesheet the engine
//! silently ignores cannot pass as a fast benchmark.
//!
//! The counting allocator lives in this binary only and counts while a
//! measurement runs, so the timed loops and the other paint benchmarks are
//! unaffected apart from one relaxed load per allocation.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use criterion::{
    BenchmarkGroup, Criterion, criterion_group, criterion_main, measurement::WallTime,
};
use florui::Element;
use florui_layout::{BoxLayout, compute_layout};
use florui_paint::{Canvas, paint_to_buffer};
use florui_style::{Arena, ComputedStyle, InteractionState, NodeId, Rgba};
use florui_text::Font;
use taffy::prelude::*;

struct CountingAllocator;

static COUNTING: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every call is forwarded unchanged to the system allocator; the
// counters only observe sizes.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && COUNTING.load(Ordering::Relaxed) {
            let live =
                LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed) + layout.size() as isize;
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        if COUNTING.load(Ordering::Relaxed) {
            LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Runs `f` and returns its result with the most heap it held at once.
fn peak_heap<R>(f: impl FnOnce() -> R) -> (R, usize) {
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let result = f();
    COUNTING.store(false, Ordering::Relaxed);
    (result, PEAK.load(Ordering::Relaxed).max(0) as usize)
}

struct Scene {
    arena: Arena,
    styles: HashMap<NodeId, ComputedStyle>,
    layouts: HashMap<NodeId, BoxLayout>,
    side: u32,
}

impl Scene {
    fn new(tree: &Element, css: &str, side: u32) -> Self {
        let arena = Arena::build(tree);
        let rules = florui_style::parse_stylesheet(css).expect("benchmark CSS must be valid");
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts = compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT)
            .expect("benchmark tree must lay out successfully");
        Self {
            arena,
            styles,
            layouts,
            side,
        }
    }

    fn paint(&self, font: &mut Font) -> Canvas {
        paint_to_buffer(
            font,
            self.side,
            self.side,
            Rgba::opaque(0x10, 0x10, 0x14),
            &self.arena,
            &self.styles,
            &self.layouts,
            1.0,
        )
    }
}

const CANVAS: u32 = 1000;

fn kib(bytes: usize) -> usize {
    bytes.div_ceil(1024)
}

/// Times a scene painted without the effect, the figure an effect's own
/// time is read against.
fn timed(group: &mut BenchmarkGroup<'_, WallTime>, name: &str, scene: &Scene) {
    let mut font = Font::load_embedded();
    group.bench_function(name, |b| b.iter(|| scene.paint(&mut font)));
}

/// Times `effect`, after reporting the heap it adds over `plain` and
/// checking it changed the picture.
fn case(group: &mut BenchmarkGroup<'_, WallTime>, name: &str, plain: &Scene, effect: &Scene) {
    let mut font = Font::load_embedded();
    let (plain_canvas, plain_peak) = peak_heap(|| plain.paint(&mut font));
    let first_paint = std::time::Instant::now();
    let (effect_canvas, effect_peak) = peak_heap(|| effect.paint(&mut font));
    let first_paint = first_paint.elapsed();
    assert_ne!(
        plain_canvas.data(),
        effect_canvas.data(),
        "{name}: the effect did not change the picture"
    );
    eprintln!(
        "MEMORY {name}: peak {} KiB, plain {} KiB, effect adds {} KiB; first paint {:.1} ms",
        kib(effect_peak),
        kib(plain_peak),
        kib(effect_peak.saturating_sub(plain_peak)),
        first_paint.as_secs_f64() * 1000.0,
    );
    group.bench_function(name, |b| b.iter(|| effect.paint(&mut font)));
}

/// One 1000 px stage with an absolutely placed box at (50, 50).
fn boxed(class: &str) -> Element {
    Element::node(
        "div",
        vec![("class".into(), "stage".into())],
        vec![Element::node(
            "div",
            vec![("class".into(), class.into())],
            vec![Element::node(
                "div",
                vec![("class".into(), "core".into())],
                Vec::new(),
            )],
        )],
    )
}

fn stage_css(size: u32, box_style: &str) -> String {
    format!(
        ".stage {{ width: {CANVAS}px; height: {CANVAS}px; background-color: #223355; }} \
         .box {{ position: absolute; left: 50px; top: 50px; width: {size}px; height: {size}px; \
         background-color: #ff8800; {box_style} }} \
         .core {{ width: {half}px; height: {half}px; background-color: #0088ff; }}",
        half = size / 2
    )
}

fn bench_filter_blur(c: &mut Criterion) {
    let mut group = c.benchmark_group("effects/filter_blur");
    group.sample_size(10);
    for size in [200u32, 600] {
        let plain = Scene::new(&boxed("box"), &stage_css(size, ""), CANVAS);
        timed(&mut group, &format!("{size}px_box_plain"), &plain);
        for radius in [4u32, 16, 32] {
            let effect = Scene::new(
                &boxed("box"),
                &stage_css(size, &format!("filter: blur({radius}px);")),
                CANVAS,
            );
            case(
                &mut group,
                &format!("{size}px_box_blur_{radius}px"),
                &plain,
                &effect,
            );
        }
    }
    group.finish();
}

fn bench_filter_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("effects/filter_chain");
    group.sample_size(10);
    let plain = Scene::new(&boxed("box"), &stage_css(400, ""), CANVAS);
    timed(&mut group, "plain", &plain);
    let chains = [
        ("1_blur", "blur(8px)"),
        ("3_blur_first", "blur(8px) brightness(1.2) contrast(1.1)"),
        ("3_blur_last", "brightness(1.2) contrast(1.1) blur(8px)"),
        (
            "5_blur_first",
            "blur(8px) brightness(1.2) contrast(1.1) saturate(1.3) brightness(0.9)",
        ),
    ];
    for (name, chain) in chains {
        let effect = Scene::new(
            &boxed("box"),
            &stage_css(400, &format!("filter: {chain};")),
            CANVAS,
        );
        case(&mut group, name, &plain, &effect);
    }
    group.finish();
}

fn bench_box_shadow(c: &mut Criterion) {
    let mut group = c.benchmark_group("effects/box_shadow");
    group.sample_size(10);
    for (shape, radius) in [("square", 0u32), ("rounded", 16)] {
        let rounded = format!("border-radius: {radius}px;");
        let plain = Scene::new(&boxed("box"), &stage_css(200, &rounded), CANVAS);
        timed(&mut group, &format!("one_{shape}_plain"), &plain);
        for blur in [8u32, 32] {
            let effect = Scene::new(
                &boxed("box"),
                &stage_css(
                    200,
                    &format!("{rounded} box-shadow: 0 8px {blur}px rgba(0, 0, 0, 0.5);"),
                ),
                CANVAS,
            );
            case(
                &mut group,
                &format!("one_{shape}_blur_{blur}px"),
                &plain,
                &effect,
            );
        }
    }
    // Many small shadowed, rounded boxes: each one allocates for itself.
    for count in [100usize, 400] {
        let grid = |shadow: &str| {
            let cells = (0..count)
                .map(|_| Element::node("div", vec![("class".into(), "cell".into())], Vec::new()))
                .collect();
            let tree = Element::node("div", vec![("class".into(), "grid".into())], cells);
            let css = format!(
                ".grid {{ display: flex; flex-direction: row; flex-wrap: wrap; width: {CANVAS}px; }} \
                 .cell {{ width: 30px; height: 30px; margin: 10px; border-radius: 8px; \
                 background-color: #ff8800; {shadow} }}"
            );
            Scene::new(&tree, &css, CANVAS)
        };
        let plain = grid("");
        timed(&mut group, &format!("{count}_rounded_cells_plain"), &plain);
        let effect = grid("box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);");
        case(
            &mut group,
            &format!("{count}_rounded_cells"),
            &plain,
            &effect,
        );
    }
    group.finish();
}

fn bench_backdrop_filter(c: &mut Criterion) {
    let mut group = c.benchmark_group("effects/backdrop_filter");
    group.sample_size(10);
    // A busy background: 1,600 small boxes in eight colors, so the region a
    // backdrop samples is not one flat color.
    let busy = || {
        let mut children = (0..1600)
            .map(|i| {
                Element::node(
                    "div",
                    vec![("class".into(), format!("tile c{}", i % 8))],
                    Vec::new(),
                )
            })
            .collect::<Vec<_>>();
        children.push(Element::node(
            "div",
            vec![("class".into(), "panel".into())],
            Vec::new(),
        ));
        Element::node("div", vec![("class".into(), "busy".into())], children)
    };
    for size in [300u32, 800] {
        let css = |backdrop: &str| {
            let mut css = format!(
                ".busy {{ display: flex; flex-direction: row; flex-wrap: wrap; width: {CANVAS}px; }} \
                 .tile {{ width: 25px; height: 25px; }} \
                 .panel {{ position: absolute; left: 100px; top: 100px; width: {size}px; \
                 height: {size}px; background-color: rgba(255, 255, 255, 0.1); {backdrop} }}"
            );
            for (index, color) in [
                "#e63946", "#f4a261", "#e9c46a", "#2a9d8f", "#264653", "#8338ec", "#3a86ff",
                "#06d6a0",
            ]
            .iter()
            .enumerate()
            {
                css.push_str(&format!(".c{index} {{ background-color: {color}; }}"));
            }
            css
        };
        let plain = Scene::new(&busy(), &css(""), CANVAS);
        timed(&mut group, &format!("{size}px_panel_plain"), &plain);
        for radius in [8u32, 24] {
            let effect = Scene::new(
                &busy(),
                &css(&format!("backdrop-filter: blur({radius}px);")),
                CANVAS,
            );
            case(
                &mut group,
                &format!("{size}px_panel_blur_{radius}px"),
                &plain,
                &effect,
            );
        }
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_filter_blur,
    bench_filter_chain,
    bench_box_shadow,
    bench_backdrop_filter,
);
criterion_main!(benches);
