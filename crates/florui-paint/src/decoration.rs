//! The lines of `text-decoration`: underline, overline and line-through, in
//! their styles, over the text of the element that declares them and of the
//! inline elements inside it, each in the color of the element that
//! declared it.
//!
//! Placed as Chromium does (checked against it, not assumed): a line is as
//! thick as `max(1, floor(font-size / 10))` unless the property says
//! otherwise; an underline starts `max(1, ceil(thickness / 2))` below the
//! baseline; an overline ends at the rounded ascent above it; a line-through
//! is centered a third of the ascent above it. An underline or overline
//! leaves out the stretches where the glyphs' ink reaches it
//! (`text-decoration-skip-ink`).

use std::collections::HashMap;

use florui_style::{
    DecorationStyle, DecorationThickness, Rgba, SkipInk, TextDecoration, UnderlineOffset,
    UnderlinePosition,
};
use florui_text::ShapedRun;
use tiny_skia::Mask;

use crate::glyph_cache::{self, Segment};
use crate::{Surface, fill_rect};

/// A decoration an element declares, with what it needs of that element.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Declared {
    pub decoration: TextDecoration,
    /// The color of the element's text, which a decoration with no color of
    /// its own takes.
    pub color: Rgba,
    pub font_size: f32,
}

/// Which lines are painted: those that go behind the text, or the one over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    UnderAndOver,
    LineThrough,
}

/// Where each element's decorations come from, for the elements of an inline
/// formatting context: `declared[e]` is what element `e` declares itself, and
/// `effective[e]` the elements whose decorations reach it, outermost first,
/// itself last.
pub(crate) fn effective_lists(declared: &[Option<Declared>], parents: &[usize]) -> Vec<Vec<usize>> {
    let mut lists: Vec<Vec<usize>> = Vec::with_capacity(declared.len());
    for element in 0..declared.len() {
        let mut list = if element == 0 {
            Vec::new()
        } else {
            lists[parents[element]].clone()
        };
        if declared[element].is_some() {
            list.push(element);
        }
        lists.push(list);
    }
    lists
}

/// One decoration over what one line holds of the element that declared it.
struct Span<'a> {
    origin: usize,
    runs: Vec<&'a ShapedRun>,
    x0: f32,
    x1: f32,
    /// The baseline, from the top of the text, of the runs of the declaring
    /// element, or of the first run there is.
    baseline: f32,
    /// The font metrics of those same runs.
    metrics: florui_text::RunMetrics,
    /// Whether `baseline` and `metrics` are the declaring element's own.
    own: bool,
}

fn spans<'a>(
    runs: &'a [ShapedRun],
    declared: &[Option<Declared>],
    effective: &[Vec<usize>],
) -> Vec<Span<'a>> {
    let mut found: Vec<Span<'a>> = Vec::new();
    let mut index: HashMap<(usize, u32), usize> = HashMap::new();
    for run in runs {
        let Some(first) = run.glyphs.first() else {
            continue;
        };
        let Some(list) = effective.get(run.element) else {
            continue;
        };
        for &origin in list {
            if declared[origin].is_none() {
                continue;
            }
            let at = *index.entry((origin, run.line)).or_insert_with(|| {
                found.push(Span {
                    origin,
                    runs: Vec::new(),
                    x0: f32::INFINITY,
                    x1: f32::NEG_INFINITY,
                    baseline: first.y,
                    metrics: run.metrics,
                    own: false,
                });
                found.len() - 1
            });
            let span = &mut found[at];
            span.runs.push(run);
            span.x0 = span.x0.min(run.x);
            span.x1 = span.x1.max(run.x + run.width);
            if run.element == origin && !span.own {
                span.baseline = first.y;
                span.metrics = run.metrics;
                span.own = true;
            }
        }
    }
    found
}

/// Paints the decorations of `runs` (positioned from `origin`, scaled by
/// `scale`); `effective[e]` lists the elements whose decorations reach a run
/// of element `e`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    buffer: &mut Surface,
    runs: &[ShapedRun],
    declared: &[Option<Declared>],
    effective: &[Vec<usize>],
    origin: (f32, f32),
    scale: f32,
    clip: Option<&Mask>,
    phase: Phase,
) {
    if declared.iter().all(Option::is_none) {
        return;
    }
    for span in spans(runs, declared, effective) {
        let Some(declared) = declared[span.origin] else {
            continue;
        };
        let lines = declared.decoration.lines;
        let thickness = thickness_of(&declared, &span.metrics);
        let color = declared.decoration.color.unwrap_or(declared.color);
        let ascent = span.metrics.ascent;
        let baseline = span.baseline;
        let mut lines_to_paint: Vec<(f32, bool)> = Vec::new();
        match phase {
            Phase::UnderAndOver => {
                if lines.underline {
                    let top = baseline + underline_offset(&declared, &span.metrics, thickness);
                    lines_to_paint.push((top, true));
                    if declared.decoration.style == DecorationStyle::Double {
                        lines_to_paint.push((top + thickness + 1.0, true));
                    }
                }
                if lines.overline {
                    let top = baseline - ascent.round() - thickness;
                    lines_to_paint.push((top, true));
                    if declared.decoration.style == DecorationStyle::Double {
                        lines_to_paint.push((top - thickness - 1.0, true));
                    }
                }
            }
            Phase::LineThrough => {
                if lines.line_through {
                    let center = baseline - ascent / 3.0;
                    if declared.decoration.style == DecorationStyle::Double {
                        let half = (thickness + 1.0) / 2.0;
                        lines_to_paint.push((center - half - thickness / 2.0, false));
                        lines_to_paint.push((center + half - thickness / 2.0, false));
                    } else {
                        lines_to_paint.push((center - thickness / 2.0, false));
                    }
                }
            }
        }
        for (top, skips_ink) in lines_to_paint {
            let gaps = if skips_ink && declared.decoration.skip_ink != SkipInk::None {
                ink_gaps(&span, top, top + thickness, thickness, scale)
            } else {
                Vec::new()
            };
            paint_line(
                buffer,
                declared.decoration.style,
                (span.x0, span.x1),
                (top, thickness),
                color,
                &gaps,
                origin,
                scale,
                clip,
            );
        }
    }
}

fn thickness_of(declared: &Declared, metrics: &florui_text::RunMetrics) -> f32 {
    let size = declared.font_size;
    match declared.decoration.thickness {
        DecorationThickness::Auto => (size / 10.0).floor().max(1.0),
        DecorationThickness::FromFont => metrics.underline_size.round().max(1.0),
        DecorationThickness::Length(px) => px.max(0.0),
        DecorationThickness::Percent(fraction) => (size * fraction).max(0.0),
    }
}

/// How far below the baseline an underline starts.
fn underline_offset(declared: &Declared, metrics: &florui_text::RunMetrics, thickness: f32) -> f32 {
    let size = declared.font_size;
    match declared.decoration.underline_offset {
        UnderlineOffset::Length(px) => px,
        UnderlineOffset::Percent(fraction) => size * fraction,
        UnderlineOffset::Auto => match declared.decoration.position {
            UnderlinePosition::Under => metrics.descent.round(),
            _ => (thickness / 2.0).ceil().max(1.0),
        },
    }
}

/// The stretches of `span` where the ink of its glyphs reaches the band from
/// `top` to `bottom` (logical pixels, from the top of the text), each made a
/// little wider.
fn ink_gaps(span: &Span<'_>, top: f32, bottom: f32, thickness: f32, scale: f32) -> Vec<(f32, f32)> {
    // Chromium leaves out the width of the line again on both sides.
    let dilation = thickness;
    let mut gaps: Vec<(f32, f32)> = Vec::new();
    for run in &span.runs {
        let size = run.font_size * scale;
        let instance = glyph_cache::instance(
            run.font.data.id(),
            run.font.index,
            size,
            &run.normalized_coords,
        );
        let mut outlines = None;
        for glyph in &run.glyphs {
            let segments = crate::glyph_outline(&instance, run, glyph.id, size, &mut outlines);
            // The band as heights above this glyph's baseline.
            let low = (glyph.y - bottom) * scale;
            let high = (glyph.y - top) * scale;
            if let Some((x0, x1)) = band_extent(&segments, low, high, run.skew) {
                gaps.push((
                    glyph.x + x0 / scale - dilation,
                    glyph.x + x1 / scale + dilation,
                ));
            }
        }
    }
    gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f32, f32)> = Vec::new();
    for gap in gaps {
        match merged.last_mut() {
            Some(last) if gap.0 <= last.1 => last.1 = last.1.max(gap.1),
            _ => merged.push(gap),
        }
    }
    merged
}

/// The horizontal extent of an outline between two heights above its
/// baseline, `None` when it does not reach them.
fn band_extent(segments: &[Segment], low: f32, high: f32, skew: f32) -> Option<(f32, f32)> {
    let mut extent: Option<(f32, f32)> = None;
    let mut add = |x: f32, y: f32| {
        if y >= low && y <= high {
            let x = x + skew * y;
            extent = Some(match extent {
                None => (x, x),
                Some((a, b)) => (a.min(x), b.max(x)),
            });
        }
    };
    // Where a straight piece crosses a height, too.
    let cross = |from: (f32, f32), to: (f32, f32), add: &mut dyn FnMut(f32, f32)| {
        for edge in [low, high] {
            if (from.1 - edge) * (to.1 - edge) < 0.0 {
                let along = (edge - from.1) / (to.1 - from.1);
                add(from.0 + (to.0 - from.0) * along, edge);
            }
        }
    };
    let mut pen = (0.0f32, 0.0f32);
    let mut start = (0.0f32, 0.0f32);
    const STEPS: usize = 12;
    for segment in segments {
        match *segment {
            Segment::Move(x, y) => {
                pen = (x, y);
                start = pen;
                add(x, y);
            }
            Segment::Line(x, y) => {
                cross(pen, (x, y), &mut add);
                add(x, y);
                pen = (x, y);
            }
            Segment::Quad(cx, cy, x, y) => {
                let mut last = pen;
                for step in 1..=STEPS {
                    let t = step as f32 / STEPS as f32;
                    let u = 1.0 - t;
                    let point = (
                        u * u * pen.0 + 2.0 * u * t * cx + t * t * x,
                        u * u * pen.1 + 2.0 * u * t * cy + t * t * y,
                    );
                    cross(last, point, &mut add);
                    add(point.0, point.1);
                    last = point;
                }
                pen = (x, y);
            }
            Segment::Cubic(c0x, c0y, c1x, c1y, x, y) => {
                let mut last = pen;
                for step in 1..=STEPS {
                    let t = step as f32 / STEPS as f32;
                    let u = 1.0 - t;
                    let point = (
                        u * u * u * pen.0
                            + 3.0 * u * u * t * c0x
                            + 3.0 * u * t * t * c1x
                            + t * t * t * x,
                        u * u * u * pen.1
                            + 3.0 * u * u * t * c0y
                            + 3.0 * u * t * t * c1y
                            + t * t * t * y,
                    );
                    cross(last, point, &mut add);
                    add(point.0, point.1);
                    last = point;
                }
                pen = (x, y);
            }
            Segment::Close => {
                cross(pen, start, &mut add);
                pen = start;
            }
        }
    }
    extent
}

/// One line from `x.0` to `x.1` (logical pixels from the text's left) at
/// `place.0` from its top, `place.1` thick, with `gaps` left out.
#[allow(clippy::too_many_arguments)]
fn paint_line(
    buffer: &mut Surface,
    style: DecorationStyle,
    x: (f32, f32),
    place: (f32, f32),
    color: Rgba,
    gaps: &[(f32, f32)],
    origin: (f32, f32),
    scale: f32,
    clip: Option<&Mask>,
) {
    let (top, thickness) = place;
    // The pieces the style draws along the line, before the gaps are cut.
    let pieces: Vec<(f32, f32)> = match style {
        DecorationStyle::Solid | DecorationStyle::Double => vec![x],
        DecorationStyle::Dotted => dashes(x, thickness, thickness),
        DecorationStyle::Dashed => {
            let (dash, gap) = if thickness >= 3.0 {
                (thickness * 2.0, thickness)
            } else {
                (thickness * 3.0, thickness * 2.0)
            };
            dashes(x, dash, gap)
        }
        DecorationStyle::Wavy => {
            wave(buffer, x, place, color, gaps, origin, scale, clip);
            return;
        }
    };
    let smooth = matches!(style, DecorationStyle::Dotted | DecorationStyle::Dashed);
    for (from, to) in pieces {
        for (a, b) in cut(from, to, gaps) {
            let rect = (
                origin.0 + a * scale,
                origin.1 + top * scale,
                (b - a) * scale,
                thickness * scale,
            );
            if smooth {
                fill_rect_smooth(buffer, rect, color, clip);
            } else {
                fill_rect(buffer, rect.0, rect.1, rect.2, rect.3, color, clip);
            }
        }
    }
}

/// `from..to` with the `gaps` taken out.
fn cut(from: f32, to: f32, gaps: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let mut pieces = Vec::new();
    let mut at = from;
    for &(start, end) in gaps {
        if end <= at || start >= to {
            continue;
        }
        if start > at {
            pieces.push((at, start));
        }
        at = at.max(end);
    }
    if at < to {
        pieces.push((at, to));
    }
    pieces
}

/// Dashes of `dash` with a gap between, the gap stretched or shrunk so the
/// line begins and ends with a dash (as Chromium picks it).
fn dashes(x: (f32, f32), dash: f32, gap: f32) -> Vec<(f32, f32)> {
    let length = x.1 - x.0;
    if length <= 0.0 || dash <= 0.0 {
        return Vec::new();
    }
    let available = length + gap;
    let fewest = (available / (dash + gap)).floor();
    if fewest < 1.0 {
        return vec![x];
    }
    let most = fewest + 1.0;
    let gap_with = |count: f32| {
        if count > 1.0 {
            (length - count * dash) / (count - 1.0)
        } else {
            gap
        }
    };
    let (min_gap, max_gap) = (gap_with(fewest), gap_with(most));
    let gap = if max_gap <= 0.0 || (min_gap - gap).abs() < (max_gap - gap).abs() {
        min_gap
    } else {
        max_gap
    };
    let mut pieces = Vec::new();
    let mut at = x.0;
    while at < x.1 - 0.001 {
        pieces.push((at, (at + dash).min(x.1)));
        at += dash + gap;
    }
    pieces
}

/// A wavy line, as a row of quadratic arcs stroked `thickness` wide.
#[allow(clippy::too_many_arguments)]
fn wave(
    buffer: &mut Surface,
    x: (f32, f32),
    place: (f32, f32),
    color: Rgba,
    gaps: &[(f32, f32)],
    origin: (f32, f32),
    scale: f32,
    clip: Option<&Mask>,
) {
    use tiny_skia::{LineCap, Paint, PathBuilder, Stroke, Transform};

    let (top, thickness) = place;
    // A wave 3 thicknesses high (the stroke included) and 5.5 long.
    let amplitude = thickness * 1.25;
    let wavelength = thickness * 5.5;
    let middle = top + thickness * 1.75;
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: thickness * scale,
        line_cap: LineCap::Butt,
        ..Stroke::default()
    };
    for (from, to) in cut(x.0, x.1, gaps) {
        let mut builder = PathBuilder::new();
        let (lx, ly) = buffer.local(origin.0 + from * scale, origin.1 + middle * scale);
        builder.move_to(lx, ly);
        let mut at = from;
        let mut up = false;
        while at < to {
            let next = (at + wavelength / 2.0).min(to);
            let control_x = (at + next) / 2.0;
            let control_y = middle
                + if up {
                    -amplitude * 2.0
                } else {
                    amplitude * 2.0
                };
            let (cx, cy) = buffer.local(origin.0 + control_x * scale, origin.1 + control_y * scale);
            let (ex, ey) = buffer.local(origin.0 + next * scale, origin.1 + middle * scale);
            builder.quad_to(cx, cy, ex, ey);
            at = next;
            up = !up;
        }
        if let Some(path) = builder.finish() {
            buffer
                .pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), clip);
        }
    }
}

/// A rectangle with anti-aliased edges, for the pieces of a dashed line, which
/// Chromium draws that way (a plain fill snaps each edge to a pixel).
fn fill_rect_smooth(
    buffer: &mut Surface,
    rect: (f32, f32, f32, f32),
    color: Rgba,
    clip: Option<&Mask>,
) {
    use tiny_skia::{Paint, Rect, Transform};
    let (x, y) = buffer.local(rect.0, rect.1);
    let Some(rect) = Rect::from_xywh(x, y, rect.2, rect.3) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    buffer
        .pixmap
        .fill_rect(rect, &paint, Transform::identity(), clip);
}
