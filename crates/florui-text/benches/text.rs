//! First performance baseline for text: shaping, line breaking, fallback,
//! and loading, at representative scale. This is the harness and one
//! recorded run, not a gate.
//!
//! Deliberately missing: a cold-vs-warm-cache comparison. This crate's
//! own module doc already states nothing here caches a shaped or
//! measured result between calls, so there is no real cache to warm —
//! benchmarking one would fabricate a distinction that doesn't exist
//! yet, rather than measure something real.

use criterion::{Criterion, criterion_group, criterion_main};
use florui_text::{Font, FontFamily};

const SHORT: &str = "a short line of text";
const LONG: &str = "a much longer paragraph of text repeated several times over to give the shaper meaningfully more work to do per call, the way a real paragraph or article body would";
const EMOJI: &str = "\u{1F600}";

fn bench_load_embedded(c: &mut Criterion) {
    c.bench_function("text/load_embedded", |b| {
        b.iter(Font::load_embedded);
    });
}

fn bench_shape(c: &mut Criterion) {
    let mut font = Font::load_embedded();
    let mut group = c.benchmark_group("text/shape");
    group.bench_function("short", |b| {
        b.iter(|| {
            font.shape(
                florui_text::TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                SHORT,
            )
        });
    });
    group.bench_function("long", |b| {
        b.iter(|| {
            font.shape(
                florui_text::TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                LONG,
            )
        });
    });
    group.finish();
}

fn bench_shape_wrapped(c: &mut Criterion) {
    let mut font = Font::load_embedded();
    c.bench_function("text/shape_wrapped_narrow", |b| {
        b.iter(|| {
            font.shape_wrapped(
                florui_text::TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                LONG,
                120.0,
            )
        });
    });
}

fn bench_fallback(c: &mut Criterion) {
    let mut font = Font::load_embedded();
    c.bench_function("text/fallback_emoji", |b| {
        b.iter(|| {
            font.shape(
                florui_text::TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
                EMOJI,
            )
        });
    });
}

criterion_group!(
    benches,
    bench_load_embedded,
    bench_shape,
    bench_shape_wrapped,
    bench_fallback,
);
criterion_main!(benches);
