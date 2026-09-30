//! First performance baseline for paint: background-rectangle scaling in
//! depth and width, and — the gap `florui-paint`'s own module doc has
//! flagged since real glyph rendering landed, with no number to back or
//! refute a glyph-cache decision — real text/glyph rasterization cost.
//! Same convention as [`florui-layout`'s own baseline]: tree/styles/
//! layouts built once outside the timed closure, so only [`paint_to_buffer`]
//! itself is measured; representative "100, 1,000"-scale workloads; no
//! pass/fail budget enforced here.
//!
//! [`florui-layout`'s own baseline]: https://github.com/floregreensoftwares/florui/blob/grow/main/crates/florui-layout/benches/layout.rs

use std::collections::HashMap;

use criterion::{Criterion, criterion_group, criterion_main};
use florui::Element;
use florui_layout::compute_layout;
use florui_paint::paint_to_buffer;
use florui_style::{Arena, ComputedStyle, InteractionState, NodeId, Rgba};
use florui_text::Font;
use taffy::prelude::*;

fn arena_styles_layouts(
    tree: &Element,
    css: &str,
) -> (
    Arena,
    HashMap<NodeId, ComputedStyle>,
    HashMap<NodeId, florui_layout::BoxLayout>,
) {
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
    (arena, styles, layouts)
}

fn deep_tree(depth: usize) -> Element {
    let mut node = Element::node("div", vec![("class".into(), "leaf".into())], Vec::new());
    for _ in 0..depth {
        node = Element::node("div", vec![("class".into(), "box".into())], vec![node]);
    }
    node
}

fn wide_tree(n: usize) -> Element {
    let children = (0..n)
        .map(|_| Element::node("div", vec![("class".into(), "item".into())], Vec::new()))
        .collect();
    Element::node("div", Vec::new(), children)
}

/// `n` small boxes, each `opacity`/`transform`'d into its own
/// [`paint_group`](florui_paint) surface -- the shape of a real animated
/// UI (`motion_test`'s own hover/keyframe rows), not just a static tree.
/// Locks in, as an actual number, the improvement bounded offscreen
/// surfaces bought over the old always-canvas-sized allocation per group.
fn animated_groups_tree(n: usize) -> Element {
    let children = (0..n)
        .map(|i| {
            Element::node(
                "div",
                vec![("class".into(), format!("item item-{i}"))],
                Vec::new(),
            )
        })
        .collect();
    Element::node("div", vec![("class".into(), "row".into())], children)
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

/// 300: the same depth `florui-layout`'s own `deep_tree` group stays under
/// — `compute_layout` (needed here to produce real `layouts`, not just
/// `paint_to_buffer` itself) still depends on Taffy's own per-depth
/// recursion, third-party code neither crate controls.
fn bench_deep_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("paint/deep_tree");
    group.sample_size(20);
    for &depth in &[100usize, 300] {
        let tree = deep_tree(depth);
        let (arena, styles, layouts) = arena_styles_layouts(
            &tree,
            ".box { width: 20px; height: 20px; background-color: #335577; } \
             .leaf { width: 10px; height: 10px; background-color: #ff0000; }",
        );
        let mut font = Font::load_embedded();
        group.bench_function(format!("{depth}_deep"), |b| {
            b.iter(|| {
                paint_to_buffer(
                    &mut font,
                    400,
                    400,
                    Rgba::opaque(0, 0, 0),
                    &arena,
                    &styles,
                    &layouts,
                    1.0,
                )
            });
        });
    }
    group.finish();
}

fn bench_wide_tree(c: &mut Criterion) {
    let mut group = c.benchmark_group("paint/wide_tree");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let tree = wide_tree(n);
        let (arena, styles, layouts) = arena_styles_layouts(
            &tree,
            ".item { width: 10px; height: 10px; background-color: #335577; }",
        );
        let mut font = Font::load_embedded();
        group.bench_function(format!("{n}_children"), |b| {
            b.iter(|| {
                paint_to_buffer(
                    &mut font,
                    2000,
                    2000,
                    Rgba::opaque(0, 0, 0),
                    &arena,
                    &styles,
                    &layouts,
                    1.0,
                )
            });
        });
    }
    group.finish();
}

/// The measurement `florui-paint`'s own module doc has been missing since
/// real glyph rendering landed: every glyph's outline is extracted by
/// skrifa and rasterized fresh, every call, with no cache — this is the
/// number that decides whether that's actually worth caching, not a guess.
fn bench_text(c: &mut Criterion) {
    let mut group = c.benchmark_group("paint/text");
    group.sample_size(20);
    for &n in &[100usize, 1000] {
        let tree = text_leaves(n);
        let (arena, styles, layouts) =
            arena_styles_layouts(&tree, ".col { display: flex; flex-direction: column; }");
        let mut font = Font::load_embedded();
        group.bench_function(format!("{n}_leaves"), |b| {
            b.iter(|| {
                paint_to_buffer(
                    &mut font,
                    800,
                    2000,
                    Rgba::opaque(0, 0, 0),
                    &arena,
                    &styles,
                    &layouts,
                    1.0,
                )
            });
        });
    }
    group.finish();
}

fn bench_animated_groups(c: &mut Criterion) {
    let mut group = c.benchmark_group("paint/animated_groups");
    group.sample_size(20);
    for &n in &[8usize, 32] {
        let tree = animated_groups_tree(n);
        let (arena, styles, layouts) = arena_styles_layouts(
            &tree,
            ".row { display: flex; flex-direction: row; flex-wrap: wrap; gap: 20px; padding: 40px; } \
             .item { width: 80px; height: 80px; background-color: #335577; opacity: 0.8; \
             transform: rotate(15deg) translate(4px, 4px); }",
        );
        let mut font = Font::load_embedded();
        group.bench_function(format!("{n}_groups_on_900x600"), |b| {
            b.iter(|| {
                paint_to_buffer(
                    &mut font,
                    900,
                    600,
                    Rgba::opaque(0x10, 0x10, 0x14),
                    &arena,
                    &styles,
                    &layouts,
                    1.0,
                )
            });
        });
    }
    group.finish();
}

/// Text that stays as `n` separate leaves: block-level `div`s and
/// `inline-block` spans don't merge into one inline run the way plain
/// `<span>`s do in [`bench_text`], so this isolates per-leaf glyph cost.
fn separate_text_leaves(n: usize, tag: &'static str, class: &'static str) -> Element {
    let children = (0..n)
        .map(|i| {
            Element::node(
                tag,
                vec![("class".into(), class.into())],
                vec![Element::text(format!("item number {i}"))],
            )
        })
        .collect();
    Element::node("div", vec![("class".into(), "col".into())], children)
}

fn bench_text_separate_leaves(c: &mut Criterion) {
    let mut group = c.benchmark_group("paint/text_separate_leaves");
    group.sample_size(20);
    // Wrapped into a grid so every leaf lands on the 2000x2000 canvas; in a
    // single column 1,000 leaves overflow it and most are never painted.
    let css = ".col { display: flex; flex-direction: row; flex-wrap: wrap; } \
               .block { display: block; width: 120px; } \
               .ib { display: inline-block; width: 120px; }";
    for &n in &[1usize, 10, 100, 1000] {
        for (name, tag, class) in [("block", "div", "block"), ("inline_block", "span", "ib")] {
            let tree = separate_text_leaves(n, tag, class);
            let (arena, styles, layouts) = arena_styles_layouts(&tree, css);
            let mut font = Font::load_embedded();
            group.bench_function(format!("{n}_{name}"), |b| {
                b.iter(|| {
                    paint_to_buffer(
                        &mut font,
                        2000,
                        2000,
                        Rgba::opaque(0, 0, 0),
                        &arena,
                        &styles,
                        &layouts,
                        1.0,
                    )
                });
            });
        }
    }
    group.finish();
}

/// Isolates the whole-canvas clear from per-rectangle cost: an empty tree
/// at each canvas size is essentially the clear alone, and a fixed 100
/// rectangles across the same sizes shows whether total time tracks canvas
/// area or rectangle count.
fn bench_canvas_size(c: &mut Criterion) {
    let mut group = c.benchmark_group("paint/canvas_size");
    group.sample_size(20);
    let empty = Element::node("div", Vec::new(), Vec::new());
    let (empty_arena, empty_styles, empty_layouts) = arena_styles_layouts(&empty, "");
    let rects = wide_tree(100);
    let (arena, styles, layouts) = arena_styles_layouts(
        &rects,
        ".item { width: 10px; height: 10px; background-color: #335577; }",
    );
    let mut font = Font::load_embedded();
    for &side in &[500u32, 1000, 2000] {
        group.bench_function(format!("{side}px_empty"), |b| {
            b.iter(|| {
                paint_to_buffer(
                    &mut font,
                    side,
                    side,
                    Rgba::opaque(0, 0, 0),
                    &empty_arena,
                    &empty_styles,
                    &empty_layouts,
                    1.0,
                )
            });
        });
        group.bench_function(format!("{side}px_100_rects"), |b| {
            b.iter(|| {
                paint_to_buffer(
                    &mut font,
                    side,
                    side,
                    Rgba::opaque(0, 0, 0),
                    &arena,
                    &styles,
                    &layouts,
                    1.0,
                )
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_deep_tree,
    bench_wide_tree,
    bench_text,
    bench_animated_groups,
    bench_text_separate_leaves,
    bench_canvas_size,
);
criterion_main!(benches);
