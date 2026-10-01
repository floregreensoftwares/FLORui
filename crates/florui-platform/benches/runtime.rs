//! Baseline for the window-independent frame pipeline: `UiRuntime::update`
//! (render, cascade, layout) on lists of rows, the same call after a
//! signal write, a scroll box's per-update cost, hit testing, and a full
//! update-plus-paint frame. Trees are ordinary rows of text and a button,
//! not synthetic empty boxes. No pass/fail budget.

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use florui::Element;
use florui_platform::UiRuntime;
use florui_reactive::{Signal, use_signal};
use florui_style::Rgba;
use taffy::prelude::*;

const CSS: &str = "
    .list { display: flex; flex-direction: column; width: 800px; }
    .scroll { height: 600px; overflow: auto; display: flex; flex-direction: column; }
    .row { display: flex; flex-direction: row; gap: 8px; padding: 4px 8px;
           border-bottom: 1px solid #ddd; background-color: #fff; }
    .row:hover { background-color: #eef; }
    .label { flex-grow: 1; color: #222; font-size: 14px; }
    .action { padding: 2px 8px; background-color: #36c; color: #fff; }
";

const VIEWPORT: Size<AvailableSpace> = Size {
    width: AvailableSpace::Definite(800.0),
    height: AvailableSpace::Definite(600.0),
};

fn row(i: usize) -> Element {
    Element::node(
        "div",
        vec![("class".into(), "row".into())],
        vec![
            Element::node(
                "span",
                vec![("class".into(), "label".into())],
                vec![Element::text(format!("Item number {i}"))],
            ),
            Element::node(
                "button",
                vec![("class".into(), "action".into())],
                vec![Element::text("Open")],
            ),
        ],
    )
}

fn list(class: &'static str, n: usize) -> Element {
    Element::node(
        "div",
        vec![("class".into(), class.into())],
        (0..n).map(row).collect(),
    )
}

fn runtime(class: &'static str, n: usize) -> UiRuntime {
    UiRuntime::new(CSS, move || list(class, n), VIEWPORT).expect("benchmark CSS must be valid")
}

fn bench_update(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform/update");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let mut rt = runtime("list", n);
        group.bench_function(format!("{n}_rows"), |b| b.iter(|| rt.update(VIEWPORT)));
    }
    // Every wheel tick pays this today: a full update over a scroll box.
    for &n in &[100usize, 1000] {
        let mut rt = runtime("scroll", n);
        group.bench_function(format!("{n}_rows_in_scroll_box"), |b| {
            b.iter(|| rt.update(VIEWPORT))
        });
    }
    group.finish();
}

fn bench_update_after_signal(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform/update_after_signal");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let handle: Rc<RefCell<Option<Signal<u32>>>> = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&handle);
        let mut rt = UiRuntime::new(
            CSS,
            move || {
                let count = use_signal(|| 0u32);
                *sink.borrow_mut() = Some(count.clone());
                Element::node(
                    "div",
                    vec![("class".into(), "list".into())],
                    std::iter::once(Element::text(format!("Count {}", count.get())))
                        .chain((0..n).map(row))
                        .collect(),
                )
            },
            VIEWPORT,
        )
        .expect("benchmark CSS must be valid");
        let signal = handle.borrow().clone().expect("root renders once in new");
        let mut value = 0u32;
        group.bench_function(format!("{n}_rows"), |b| {
            b.iter(|| {
                value += 1;
                signal.set(value);
                rt.update(VIEWPORT);
            })
        });
    }
    group.finish();
}

fn row_with_text(i: usize, tick: u32) -> Element {
    Element::node(
        "div",
        vec![("class".into(), "row".into())],
        vec![
            Element::node(
                "span",
                vec![("class".into(), "label".into())],
                vec![Element::text(format!("Item {tick} / {i}"))],
            ),
            Element::node(
                "button",
                vec![("class".into(), "action".into())],
                vec![Element::text(format!("Open {tick}"))],
            ),
        ],
    )
}

/// The memo's worst case: every label and button changes on every update,
/// so nothing it remembers is ever asked for again.
fn bench_update_all_text_new(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform/update_all_text_new");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let handle: Rc<RefCell<Option<Signal<u32>>>> = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&handle);
        let mut rt = UiRuntime::new(
            CSS,
            move || {
                let tick = use_signal(|| 0u32);
                *sink.borrow_mut() = Some(tick.clone());
                Element::node(
                    "div",
                    vec![("class".into(), "list".into())],
                    (0..n).map(|i| row_with_text(i, tick.get())).collect(),
                )
            },
            VIEWPORT,
        )
        .expect("benchmark CSS must be valid");
        let signal = handle.borrow().clone().expect("root renders once in new");
        let mut value = 0u32;
        group.bench_function(format!("{n}_rows"), |b| {
            b.iter(|| {
                value += 1;
                signal.set(value);
                rt.update(VIEWPORT);
            })
        });
    }
    group.finish();
}

fn bench_hit_test(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform/hit_test");
    for &n in &[100usize, 1000] {
        let rt = runtime("list", n);
        // A point deep in the list, so the walk has to pass many siblings.
        let y = (n as f32) * 12.0;
        group.bench_function(format!("{n}_rows"), |b| {
            b.iter(|| black_box(rt.hit_test(100.0, y)))
        });
    }
    // Same node count per step as a chain instead of a list: shows whether
    // the per-node cost grows with depth.
    for &depth in &[50usize, 100, 200] {
        let rt = UiRuntime::new(
            CSS,
            move || {
                let mut node = row(0);
                for _ in 0..depth {
                    node = Element::node("div", vec![("class".into(), "list".into())], vec![node]);
                }
                node
            },
            VIEWPORT,
        )
        .expect("benchmark CSS must be valid");
        group.bench_function(format!("{depth}_deep"), |b| {
            b.iter(|| black_box(rt.hit_test(10.0, 10.0)))
        });
    }
    group.finish();
}

fn bench_frame(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform/frame");
    group.sample_size(10);
    for &n in &[100usize, 1000] {
        let mut rt = runtime("list", n);
        group.bench_function(format!("{n}_rows_update_and_paint"), |b| {
            b.iter(|| {
                rt.update(VIEWPORT);
                let (arena, styles, layouts, font) = rt.geometry_and_font_mut();
                black_box(florui_paint::paint_to_buffer(
                    font,
                    800,
                    600,
                    Rgba::opaque(255, 255, 255),
                    arena,
                    styles,
                    layouts,
                    1.0,
                ))
            })
        });
    }
    group.finish();
}

/// Where `UiRuntime::update` spends its time, reproduced from the public
/// stages on the same rows: building the element tree, the arena, the
/// cascade alone, and cascade plus layout (`compute_with_style`, which is
/// what `update` calls). Compare the sum against `platform/update`; any
/// remainder is the runtime's own bookkeeping.
fn bench_update_phases(c: &mut Criterion) {
    let mut group = c.benchmark_group("platform/update_phases");
    group.sample_size(20);
    let rules = florui_style::parse_stylesheet(CSS).expect("benchmark CSS must be valid");
    let viewport = florui_style::Viewport {
        width: 800.0,
        height: 600.0,
    };
    for &n in &[100usize, 1000] {
        group.bench_function(format!("{n}_rows_build_tree"), |b| {
            b.iter(|| black_box(list("list", n)))
        });

        let tree = list("list", n);
        group.bench_function(format!("{n}_rows_arena_build"), |b| {
            b.iter(|| black_box(florui_style::Arena::build(&tree)))
        });

        let arena = florui_style::Arena::build(&tree);
        let state = florui_style::InteractionState::new();
        let mut timeline = florui_style::AnimationTimeline::default();
        group.bench_function(format!("{n}_rows_cascade"), |b| {
            b.iter(|| {
                black_box(florui_style::compute(
                    &arena,
                    &rules,
                    &state,
                    viewport,
                    &mut timeline,
                ))
            })
        });

        let mut font = florui_text::Font::load_embedded();
        let mut timeline = florui_style::AnimationTimeline::default();
        group.bench_function(format!("{n}_rows_cascade_and_layout"), |b| {
            b.iter(|| {
                black_box(
                    florui_layout::compute_with_style(
                        &mut font,
                        &arena,
                        &rules,
                        &state,
                        viewport,
                        &mut timeline,
                        VIEWPORT,
                    )
                    .expect("benchmark tree must lay out"),
                )
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_update,
    bench_update_after_signal,
    bench_update_all_text_new,
    bench_hit_test,
    bench_frame,
    bench_update_phases,
);
criterion_main!(benches);
