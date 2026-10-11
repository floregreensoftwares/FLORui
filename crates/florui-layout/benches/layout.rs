//! First performance baseline for layout: block/flex/grid, intrinsic text
//! measurement, variable widths, and tree scaling, at representative
//! "100, 1,000"-scale workloads — 10,000+ waits for when a real
//! large-scale scenario (virtualization) exists to justify it. This is
//! the harness and one recorded run, not a gate: no pass/fail budget is
//! enforced here — defining budgets comes only after a baseline exists,
//! and that's later work.
//!
//! Each group builds its tree/styles once, outside the timed closure, so
//! only [`compute_layout`] itself is measured — tree/cascade construction
//! is real cost elsewhere, but not the declared metric here.

use florui_style::StyleMap;

use criterion::{Criterion, criterion_group, criterion_main};
use florui::Element;
use florui_layout::compute_layout;
use florui_style::{Arena, InteractionState};
use taffy::prelude::*;

fn arena_and_styles(tree: &Element, css: &str) -> (Arena, StyleMap) {
    let arena = Arena::build(tree);
    let rules = florui_style::parse_stylesheet(css).expect("benchmark CSS must be valid");
    let styles = florui_style::compute(
        &arena,
        &rules,
        &InteractionState::new(),
        florui_style::Viewport::default(),
        &mut florui_style::AnimationTimeline::default(),
    );
    (arena, styles)
}

fn wide_block_tree(n: usize) -> Element {
    let children = (0..n)
        .map(|_| Element::node("div", vec![("class".into(), "item".into())], Vec::new()))
        .collect();
    Element::node("div", Vec::new(), children)
}

fn deep_tree(depth: usize) -> Element {
    let mut node = Element::node("div", vec![("class".into(), "leaf".into())], Vec::new());
    for _ in 0..depth {
        node = Element::node("div", Vec::new(), vec![node]);
    }
    node
}

fn flex_row(n: usize) -> Element {
    let children = (0..n)
        .map(|_| Element::node("div", vec![("class".into(), "item".into())], Vec::new()))
        .collect();
    Element::node("div", vec![("class".into(), "row".into())], children)
}

/// A `side`x`side` grid — explicit track lists, not `repeat()`, since this
/// engine doesn't read that back yet (see `florui-style`'s own documented
/// bound on `grid-template-columns`).
fn grid(side: usize) -> Element {
    let children = (0..side * side)
        .map(|_| Element::node("div", Vec::new(), Vec::new()))
        .collect();
    Element::node("div", vec![("class".into(), "grid".into())], children)
}

fn grid_css(side: usize) -> String {
    let track = "10px ".repeat(side);
    format!(
        ".grid {{ display: grid; grid-template-columns: {track}; \
         grid-template-rows: {track}; }}"
    )
}

fn text_leaves(n: usize) -> Element {
    let children = (0..n)
        .map(|i| {
            Element::node(
                "span",
                Vec::new(),
                vec![Element::text(format!("item number {i}"))],
            )
        })
        .collect();
    Element::node("div", vec![("class".into(), "col".into())], children)
}

fn bench_block_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/block_tree");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let tree = wide_block_tree(n);
        let (arena, styles) = arena_and_styles(&tree, ".item { width: 10px; height: 10px; }");
        let mut font = florui_text::Font::load_embedded();
        group.bench_function(format!("{n}_children"), |b| {
            b.iter(|| compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap());
        });
    }
    group.finish();
}

/// 1,000 was the original second tier, but a naive 1,000-deep chain
/// genuinely stack-overflows this process. `Arena::build` and
/// `compute_layout`'s own `build_node` are iterative now, not the cause
/// anymore — the remaining cause is `taffy::TaffyTree::compute_layout_
/// with_measure` itself recursing once per depth internally, third-party
/// code this crate doesn't control. 300 stays comfortably under that
/// limit while still showing real scaling cost.
fn bench_deep_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/deep_tree");
    group.sample_size(20);
    for &depth in &[100usize, 300] {
        let tree = deep_tree(depth);
        let (arena, styles) = arena_and_styles(&tree, ".leaf { width: 10px; height: 10px; }");
        let mut font = florui_text::Font::load_embedded();
        group.bench_function(format!("{depth}_deep"), |b| {
            b.iter(|| compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap());
        });
    }
    group.finish();
}

fn bench_flex_row(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/flex_row");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let tree = flex_row(n);
        let (arena, styles) = arena_and_styles(
            &tree,
            ".row { display: flex; } .item { width: 10px; height: 10px; flex-shrink: 0; }",
        );
        let mut font = florui_text::Font::load_embedded();
        group.bench_function(format!("{n}_children"), |b| {
            b.iter(|| compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap());
        });
    }
    group.finish();
}

/// Grid stays at one, smaller scale (10x10 = 100 cells): a 1,000-cell
/// grid needs a genuinely large explicit track list under this engine's
/// current `repeat()`-less bound, which would benchmark CSS parsing size
/// as much as layout itself.
fn bench_grid(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/grid");
    group.sample_size(20);
    let side = 10;
    let tree = grid(side);
    let (arena, styles) = arena_and_styles(&tree, &grid_css(side));
    let mut font = florui_text::Font::load_embedded();
    group.bench_function("10x10_cells", |b| {
        b.iter(|| compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap());
    });
    group.finish();
}

fn bench_intrinsic_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("layout/intrinsic_text");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let tree = text_leaves(n);
        let (arena, styles) =
            arena_and_styles(&tree, ".col { display: flex; flex-direction: column; }");
        let mut font = florui_text::Font::load_embedded();
        group.bench_function(format!("{n}_leaves"), |b| {
            b.iter(|| compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap());
        });
    }
    group.finish();
}

/// The same wrapping tree laid out at several available widths — the
/// Layout suite's own "variable widths" measurement, not a fixed-size
/// scaling test like the groups above.
fn bench_variable_widths(c: &mut Criterion) {
    // No explicit width on `.col`: a plain block container stretches to
    // whatever `available.width` this benchmark passes in below, so each
    // iteration's text actually wraps differently rather than all
    // producing the same layout regardless of the available space.
    let tree = text_leaves(50);
    let (arena, styles) = arena_and_styles(&tree, "");
    let mut group = c.benchmark_group("layout/variable_widths");
    group.sample_size(20);
    for &width in &[100.0f32, 400.0, 1600.0] {
        let available = Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::MaxContent,
        };
        let mut font = florui_text::Font::load_embedded();
        group.bench_function(format!("{width}px"), |b| {
            b.iter(|| compute_layout(&mut font, &arena, &styles, available).unwrap());
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_block_tree,
    bench_deep_tree,
    bench_flex_row,
    bench_grid,
    bench_intrinsic_text,
    bench_variable_widths,
);
criterion_main!(benches);
