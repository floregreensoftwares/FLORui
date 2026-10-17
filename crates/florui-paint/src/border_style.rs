//! Border sides drawn in a style other than solid, and the outline ring.
//!
//! Each side is drawn in a layer the size of the box and cut to its own wedge
//! (the diagonals from the outer corners through the inner ones), so sides of
//! different style or color meet along the diagonal, as they do in a browser.

use florui_style::{BorderLineStyle, BorderSide, Edges, Rgba, RoundedRect};
use tiny_skia::{
    FillRule, LineCap, Mask, Paint, PathBuilder, PixmapPaint, Rect, Stroke, StrokeDash, Transform,
};

use super::decoration::{dashes, fitted_gap};
use super::rounded::RoundedRectPath;
use super::{Surface, draw_shifted, side_wedges, surface_transform};

/// Whether every visible side is solid, so the plain painters apply.
pub(crate) fn is_plain(border: &Edges<BorderSide>) -> bool {
    [border.top, border.right, border.bottom, border.left]
        .iter()
        .all(|side| side.width <= 0.0 || side.style == BorderLineStyle::Solid)
}

/// Paints `border` around `outer`, each side in its own style.
pub(crate) fn paint(
    buffer: &mut Surface,
    outer: &RoundedRect,
    border: Edges<BorderSide>,
    clip: Option<&Mask>,
) {
    let sides = [border.top, border.right, border.bottom, border.left];
    if sides.iter().all(|side| side.width <= 0.0) {
        return;
    }
    let inner = outer.inset(
        border.top.width,
        border.right.width,
        border.bottom.width,
        border.left.width,
    );
    let wedges = side_wedges(outer, &inner);
    let (local_x0, local_y0) = buffer.local(outer.x, outer.y);
    let rx0 = (local_x0.floor() as i32).max(0);
    let ry0 = (local_y0.floor() as i32).max(0);
    let rx1 = ((local_x0 + outer.width).ceil() as i32).min(buffer.width() as i32);
    let ry1 = ((local_y0 + outer.height).ceil() as i32).min(buffer.height() as i32);
    if rx1 <= rx0 || ry1 <= ry0 {
        return;
    }
    let (width, height) = ((rx1 - rx0) as u32, (ry1 - ry0) as u32);
    let Some(mut layer) = Surface::new(
        width,
        height,
        (buffer.origin.0 + rx0, buffer.origin.1 + ry0),
    ) else {
        return;
    };
    let transform = surface_transform(&layer);
    // Dashes and dots are not mitered where the sides share a color.
    let one_color = sides.iter().filter(|side| side.width > 0.0).all(|side| {
        side.color
            == sides
                .iter()
                .find(|s| s.width > 0.0)
                .map_or(side.color, |s| s.color)
    });
    for (index, (side, corners)) in sides.iter().zip(wedges).enumerate() {
        if side.width <= 0.0 {
            continue;
        }
        let mut wedge = PathBuilder::new();
        wedge.move_to(corners[0].0, corners[0].1);
        for &(px, py) in &corners[1..] {
            wedge.line_to(px, py);
        }
        wedge.close();
        let (Some(mut wedge), Some(mut mask)) = (wedge.finish(), Mask::new(width, height)) else {
            continue;
        };
        let pieces = matches!(
            side.style,
            BorderLineStyle::Dashed | BorderLineStyle::Dotted
        );
        if pieces && one_color && outer.is_square() {
            let Some(whole) = Rect::from_xywh(outer.x, outer.y, outer.width, outer.height) else {
                continue;
            };
            wedge = PathBuilder::from_rect(whole);
        }
        mask.fill_path(&wedge, FillRule::Winding, true, transform);
        let target = Target {
            transform,
            mask: &mask,
        };
        draw_side(&mut layer, &target, index, side, outer, &inner, &border);
    }
    draw_shifted(
        &mut buffer.pixmap,
        &layer.pixmap,
        (rx0, ry0),
        &PixmapPaint::default(),
        clip,
    );
}

struct Target<'a> {
    transform: Transform,
    mask: &'a Mask,
}

fn paint_of(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    paint
}

/// The ring between two outlines.
fn fill_ring(
    layer: &mut Surface,
    target: &Target<'_>,
    outer: &RoundedRect,
    inner: &RoundedRect,
    color: Rgba,
) {
    let Some(outer_path) = outer.path() else {
        return;
    };
    let mut ring = PathBuilder::new();
    ring.push_path(&outer_path);
    if let Some(inner_path) = inner.path() {
        ring.push_path(&inner_path);
    }
    if let Some(ring) = ring.finish() {
        layer.pixmap.fill_path(
            &ring,
            &paint_of(color),
            FillRule::EvenOdd,
            target.transform,
            Some(target.mask),
        );
    }
}

fn draw_side(
    layer: &mut Surface,
    target: &Target<'_>,
    index: usize,
    side: &BorderSide,
    outer: &RoundedRect,
    inner: &RoundedRect,
    border: &Edges<BorderSide>,
) {
    let widths = [
        border.top.width,
        border.right.width,
        border.bottom.width,
        border.left.width,
    ];
    let inset_by = |amounts: [f32; 4]| outer.inset(amounts[0], amounts[1], amounts[2], amounts[3]);
    match side.style {
        BorderLineStyle::Solid => fill_ring(layer, target, outer, inner, side.color),
        BorderLineStyle::Inset | BorderLineStyle::Outset => {
            let (shade, _) = bevel(side.style, index, side.color);
            fill_ring(layer, target, outer, inner, shade);
        }
        BorderLineStyle::Groove | BorderLineStyle::Ridge => {
            let (first, second) = bevel(side.style, index, side.color);
            let middle = inset_by(widths.map(|w| w / 2.0));
            fill_ring(layer, target, outer, &middle, first);
            fill_ring(layer, target, &middle, inner, second);
        }
        BorderLineStyle::Double => {
            if side.width < 3.0 {
                fill_ring(layer, target, outer, inner, side.color);
                return;
            }
            let line = widths.map(|w| if w < 3.0 { w } else { (w / 3.0).round() });
            let first_end = inset_by(line);
            let second_start = inset_by([
                widths[0] - line[0],
                widths[1] - line[1],
                widths[2] - line[2],
                widths[3] - line[3],
            ]);
            fill_ring(layer, target, outer, &first_end, side.color);
            fill_ring(layer, target, &second_start, inner, side.color);
        }
        BorderLineStyle::Dashed | BorderLineStyle::Dotted => {
            if outer.is_square() {
                straight_pieces(layer, target, index, side, outer);
            } else {
                loop_pieces(layer, target, side, outer, &widths);
            }
        }
    }
}

/// Whether the style draws an outline.
pub(crate) fn has_outline(style: &florui_style::ComputedStyle) -> bool {
    use florui_style::OutlineStyle;
    style.visibility.is_visible()
        && match style.outline.style {
            OutlineStyle::None => false,
            OutlineStyle::Auto => true,
            OutlineStyle::Line(_) => style.outline.width > 0.0,
        }
}

/// How far an outline reaches past the border box, in painted pixels.
pub(crate) fn outline_reach(style: &florui_style::ComputedStyle, scale: f32) -> f32 {
    if !has_outline(style) {
        return 0.0;
    }
    ((style.outline.offset + style.outline.width) * scale).max(0.0)
}

/// Paints `outline` around `border_box` (painted pixels; the box's own corner
/// radii carry over, grown by the outline's distance and width).
pub(crate) fn paint_outline(
    buffer: &mut Surface,
    outline: &florui_style::Outline,
    border_box: &RoundedRect,
    scale: f32,
    clip: Option<&Mask>,
) {
    use florui_style::OutlineStyle;
    // The platform ring is 2px thick whatever the width, its outer edge the
    // offset from the border box.
    let (outer, width, style) = match outline.style {
        OutlineStyle::None => return,
        OutlineStyle::Auto => {
            let mut ring = border_box.grown(outline.offset * scale);
            if ring.is_square() {
                // The focus ring turns its corners a little, a square box too.
                let radius = (4.0 * scale).min(ring.width / 2.0).min(ring.height / 2.0);
                ring.radii = [(radius, radius); 4];
            }
            (ring, 2.0 * scale, BorderLineStyle::Solid)
        }
        OutlineStyle::Line(style) => {
            let width = outline.width * scale;
            let grown = border_box.grown(outline.offset * scale + width);
            (grown, width, style)
        }
    };
    if width <= 0.0 || outer.width <= 0.0 || outer.height <= 0.0 {
        return;
    }
    let side = BorderSide {
        width,
        color: outline.color,
        style,
    };
    let border = Edges {
        top: side,
        right: side,
        bottom: side,
        left: side,
    };
    if !is_plain(&border) {
        paint(buffer, &outer, border, clip);
    } else if outer.is_square() {
        super::paint_border(
            buffer,
            outer.x,
            outer.y,
            outer.width,
            outer.height,
            border,
            clip,
        );
    } else {
        super::paint_rounded_border(buffer, &outer, border, clip);
    }
}

/// An outline around boxes that make up one element (the line boxes of an
/// inline element): the ring follows the outline of their union, so where two
/// lines overlap there is no edge between them. Styles other than solid are
/// drawn around each box on its own.
pub(crate) fn paint_outline_rects(
    buffer: &mut Surface,
    outline: &florui_style::Outline,
    rects: &[(f32, f32, f32, f32)],
    scale: f32,
    clip: Option<&Mask>,
) {
    use florui_style::OutlineStyle;
    let (inner_by, outer_by, style) = match outline.style {
        OutlineStyle::None => return,
        OutlineStyle::Auto => (
            outline.offset * scale - 2.0 * scale,
            outline.offset * scale,
            BorderLineStyle::Solid,
        ),
        OutlineStyle::Line(style) => (
            outline.offset * scale,
            (outline.offset + outline.width) * scale,
            style,
        ),
    };
    if outer_by <= inner_by || rects.is_empty() {
        return;
    }
    let bevel = matches!(
        style,
        BorderLineStyle::Groove
            | BorderLineStyle::Ridge
            | BorderLineStyle::Inset
            | BorderLineStyle::Outset
    );
    if bevel || rects.len() == 1 {
        for &(x, y, width, height) in rects {
            paint_outline(
                buffer,
                outline,
                &RoundedRect::square(x, y, width, height),
                scale,
                clip,
            );
        }
        return;
    }
    // Boxes of consecutive lines are joined across the space between the lines,
    // so the ring has no edge there.
    let mut rects = rects.to_vec();
    rects.sort_by(|a, b| a.1.total_cmp(&b.1));
    for index in 1..rects.len() {
        let (above, below) = (rects[index - 1], rects[index]);
        let (bottom, top) = (above.1 + above.3, below.1);
        if top > bottom {
            let middle = (top + bottom) / 2.0;
            rects[index - 1].3 = middle - above.1;
            rects[index].3 = below.1 + below.3 - middle;
            rects[index].1 = middle;
        }
    }
    let grown = |by: f32| -> Vec<Rect> {
        rects
            .iter()
            .filter_map(|&(x, y, w, h)| Rect::from_xywh(x - by, y - by, w + 2.0 * by, h + 2.0 * by))
            .collect()
    };
    let (outer, inner) = (grown(outer_by), grown(inner_by));
    let (Some(first), true) = (outer.first(), !outer.is_empty()) else {
        return;
    };
    let bounds = outer.iter().fold(
        (first.left(), first.top(), first.right(), first.bottom()),
        |(l, t, r, b), rect| {
            (
                l.min(rect.left()),
                t.min(rect.top()),
                r.max(rect.right()),
                b.max(rect.bottom()),
            )
        },
    );
    let (local_x0, local_y0) = buffer.local(bounds.0, bounds.1);
    let rx0 = (local_x0.floor() as i32).max(0);
    let ry0 = (local_y0.floor() as i32).max(0);
    let rx1 = ((local_x0 + bounds.2 - bounds.0).ceil() as i32).min(buffer.width() as i32);
    let ry1 = ((local_y0 + bounds.3 - bounds.1).ceil() as i32).min(buffer.height() as i32);
    if rx1 <= rx0 || ry1 <= ry0 {
        return;
    }
    let Some(mut layer) = Surface::new(
        (rx1 - rx0) as u32,
        (ry1 - ry0) as u32,
        (buffer.origin.0 + rx0, buffer.origin.1 + ry0),
    ) else {
        return;
    };
    let transform = surface_transform(&layer);
    if style == BorderLineStyle::Solid {
        let ring = paint_of(outline.color);
        for rect in &outer {
            layer.pixmap.fill_rect(*rect, &ring, transform, None);
        }
        let clear = Paint {
            blend_mode: tiny_skia::BlendMode::Clear,
            anti_alias: true,
            ..Paint::default()
        };
        for rect in &inner {
            layer.pixmap.fill_rect(*rect, &clear, transform, None);
        }
    } else {
        let thickness = outer_by - inner_by;
        let polygon = |by: f32| union_polygon(&rects, by);
        match style {
            BorderLineStyle::Double if thickness >= 3.0 => {
                let line = (thickness / 3.0).round();
                for (from, to) in [(outer_by, outer_by - line), (inner_by + line, inner_by)] {
                    if let (Some(a), Some(b)) = (polygon(from), polygon(to)) {
                        let mut ring = PathBuilder::new();
                        ring.push_path(&a);
                        ring.push_path(&b);
                        if let Some(ring) = ring.finish() {
                            layer.pixmap.fill_path(
                                &ring,
                                &paint_of(outline.color),
                                FillRule::EvenOdd,
                                transform,
                                None,
                            );
                        }
                    }
                }
            }
            BorderLineStyle::Dashed | BorderLineStyle::Dotted => {
                if let Some(path) = polygon((outer_by + inner_by) / 2.0) {
                    stroke_pattern(
                        &mut layer,
                        transform,
                        &path,
                        thickness,
                        outline.color,
                        style,
                    );
                }
            }
            _ => {
                if let (Some(a), Some(b)) = (polygon(outer_by), polygon(inner_by)) {
                    let mut ring = PathBuilder::new();
                    ring.push_path(&a);
                    ring.push_path(&b);
                    if let Some(ring) = ring.finish() {
                        layer.pixmap.fill_path(
                            &ring,
                            &paint_of(outline.color),
                            FillRule::EvenOdd,
                            transform,
                            None,
                        );
                    }
                }
            }
        }
    }
    draw_shifted(
        &mut buffer.pixmap,
        &layer.pixmap,
        (rx0, ry0),
        &PixmapPaint::default(),
        clip,
    );
}

/// The outline of the union of boxes stacked on one another (the line boxes of
/// an element, already joined across the spaces between lines), each grown by
/// `by`: down the right side and up the left, stepping where a line is wider
/// or narrower than the next.
fn union_polygon(rects: &[(f32, f32, f32, f32)], by: f32) -> Option<tiny_skia::Path> {
    let last = rects.len().checked_sub(1)?;
    let bottom = |i: usize| rects[i].1 + rects[i].3;
    let (first_top, last_bottom) = (rects[0].1 - by, bottom(last) + by);
    // Where two lines meet, the wider one's side runs on past the join by `by`
    // and the narrower one's starts `by` early: the step is at the wider one's
    // end. `wider` says whether line `i` is wider than the next on this side.
    let step = |i: usize, wider: bool| bottom(i) + if wider { by } else { -by };
    let mut points: Vec<(f32, f32)> = Vec::new();
    for i in 0..=last {
        let r = rects[i].0 + rects[i].2 + by;
        let from = if i == 0 {
            first_top
        } else {
            let before = rects[i - 1].0 + rects[i - 1].2 + by;
            step(i - 1, before > r)
        };
        let to = if i == last {
            last_bottom
        } else {
            let after = rects[i + 1].0 + rects[i + 1].2 + by;
            step(i, r > after)
        };
        points.push((r, from));
        points.push((r, to));
    }
    for i in (0..=last).rev() {
        let l = rects[i].0 - by;
        let from = if i == 0 {
            first_top
        } else {
            let before = rects[i - 1].0 - by;
            step(i - 1, before < l)
        };
        let to = if i == last {
            last_bottom
        } else {
            let after = rects[i + 1].0 - by;
            step(i, l < after)
        };
        points.push((l, to));
        points.push((l, from));
    }
    let mut path = PathBuilder::new();
    let (first, rest) = points.split_first()?;
    path.move_to(first.0, first.1);
    for point in rest {
        path.line_to(point.0, point.1);
    }
    path.close();
    path.finish()
}

/// The length of a closed path made of straight lines.
fn polygon_length(path: &tiny_skia::Path) -> f32 {
    use tiny_skia::PathSegment;
    let (mut total, mut start, mut pen) = (0.0_f32, (0.0, 0.0), (0.0_f32, 0.0_f32));
    for segment in path.segments() {
        match segment {
            PathSegment::MoveTo(p) => {
                start = (p.x, p.y);
                pen = start;
            }
            PathSegment::LineTo(p) => {
                total += (p.x - pen.0).hypot(p.y - pen.1);
                pen = (p.x, p.y);
            }
            PathSegment::Close => {
                total += (start.0 - pen.0).hypot(start.1 - pen.1);
                pen = start;
            }
            _ => {}
        }
    }
    total
}

/// A dashed or dotted line along a closed path, fitted to its length.
fn stroke_pattern(
    layer: &mut Surface,
    transform: Transform,
    path: &tiny_skia::Path,
    width: f32,
    color: Rgba,
    style: BorderLineStyle,
) {
    let length = polygon_length(path);
    let dotted = style == BorderLineStyle::Dotted;
    let (dash, gap) = if dotted {
        (width, width)
    } else {
        dash_pattern(width)
    };
    let gap = fitted_gap(length, dash, gap, true);
    let round = dotted && width >= 3.0;
    let intervals = if round {
        vec![0.001, dash + gap - 0.001]
    } else {
        vec![dash, gap]
    };
    let stroke = Stroke {
        width,
        line_cap: if round { LineCap::Round } else { LineCap::Butt },
        dash: StrokeDash::new(intervals, 0.0),
        ..Stroke::default()
    };
    layer
        .pixmap
        .stroke_path(path, &paint_of(color), &stroke, transform, None);
}

/// The dash and gap of a dashed line `width` thick, in Chromium's ratios.
pub(crate) fn dash_pattern(width: f32) -> (f32, f32) {
    if width >= 3.0 {
        (width * 2.0, width)
    } else {
        (width * 3.0, width * 2.0)
    }
}

/// A dashed or dotted side of a square box: pieces along the side's whole
/// length, fitted to it, starting in the corner.
fn straight_pieces(
    layer: &mut Surface,
    target: &Target<'_>,
    index: usize,
    side: &BorderSide,
    outer: &RoundedRect,
) {
    let width = side.width;
    let (dash, gap) = match side.style {
        BorderLineStyle::Dotted => (width, width),
        _ => dash_pattern(width),
    };
    let horizontal = index.is_multiple_of(2);
    let (from, to) = if horizontal {
        (outer.x, outer.x + outer.width)
    } else {
        (outer.y, outer.y + outer.height)
    };
    let across = match index {
        0 => outer.y,
        1 => outer.x + outer.width - width,
        2 => outer.y + outer.height - width,
        _ => outer.x,
    };
    let round = side.style == BorderLineStyle::Dotted && width >= 3.0;
    // Thin dots are squares on whole pixels, each starting where its place in
    // the pattern falls, rounded down.
    let crisp = side.style == BorderLineStyle::Dotted && width < 3.0;
    let mut paint = paint_of(side.color);
    paint.anti_alias = !crisp;
    for (a, b) in dashes((from, to), dash, gap) {
        let (a, b) = if crisp {
            let start = from + (a - from + 1.0e-3).floor();
            (start, start + width)
        } else {
            (a, b)
        };
        if round {
            let along = (a + b) / 2.0;
            let (cx, cy) = if horizontal {
                (along, across + width / 2.0)
            } else {
                (across + width / 2.0, along)
            };
            let mut dot = PathBuilder::new();
            dot.push_circle(cx, cy, width / 2.0 - 0.25);
            if let Some(dot) = dot.finish() {
                layer.pixmap.fill_path(
                    &dot,
                    &paint,
                    FillRule::Winding,
                    target.transform,
                    Some(target.mask),
                );
            }
        } else {
            let rect = if horizontal {
                Rect::from_xywh(a, across, b - a, width)
            } else {
                Rect::from_xywh(across, a, width, b - a)
            };
            if let Some(rect) = rect {
                layer
                    .pixmap
                    .fill_rect(rect, &paint, target.transform, Some(target.mask));
            }
        }
    }
}

/// A dashed or dotted side of a rounded box: the middle of the border as one
/// closed line, dashed to fit, cut to the side by the wedge.
fn loop_pieces(
    layer: &mut Surface,
    target: &Target<'_>,
    side: &BorderSide,
    outer: &RoundedRect,
    widths: &[f32; 4],
) {
    let middle = outer.inset(
        widths[0] / 2.0,
        widths[1] / 2.0,
        widths[2] / 2.0,
        widths[3] / 2.0,
    );
    let Some(path) = middle.path() else {
        return;
    };
    let width = side.width;
    let length = perimeter(&middle);
    let dotted = side.style == BorderLineStyle::Dotted;
    let (dash, gap) = if dotted {
        (width, width)
    } else {
        dash_pattern(width)
    };
    let gap = fitted_gap(length, dash, gap, true);
    let round = dotted && width >= 3.0;
    let intervals = if round {
        // A dash of no length with round caps is a dot.
        vec![0.001, dash + gap - 0.001]
    } else {
        vec![dash, gap]
    };
    let stroke = Stroke {
        width,
        line_cap: if round { LineCap::Round } else { LineCap::Butt },
        dash: StrokeDash::new(intervals, 0.0),
        ..Stroke::default()
    };
    layer.pixmap.stroke_path(
        &path,
        &paint_of(side.color),
        &stroke,
        target.transform,
        Some(target.mask),
    );
}

/// The length of the outline's closed path.
fn perimeter(rect: &RoundedRect) -> f32 {
    let [tl, tr, br, bl] = rect.radii;
    let straight = (rect.width - tl.0 - tr.0).max(0.0)
        + (rect.height - tr.1 - br.1).max(0.0)
        + (rect.width - br.0 - bl.0).max(0.0)
        + (rect.height - bl.1 - tl.1).max(0.0);
    let arcs: f32 = rect
        .radii
        .iter()
        .map(|&(a, b)| {
            // Ramanujan's approximation of a quarter of an ellipse.
            std::f32::consts::FRAC_PI_4 * (3.0 * (a + b) - ((3.0 * a + b) * (a + 3.0 * b)).sqrt())
        })
        .sum();
    straight + arcs
}

/// The two colors of a beveled side: for `inset` and `outset` the one color,
/// for `groove` and `ridge` the outer half's then the inner half's.
pub(crate) fn bevel(style: BorderLineStyle, side: usize, color: Rgba) -> (Rgba, Rgba) {
    let (dark, light) = (darker(color), lighter(color));
    let top_or_left = side == 0 || side == 3;
    match style {
        BorderLineStyle::Inset => {
            let shade = if top_or_left { dark } else { light };
            (shade, shade)
        }
        BorderLineStyle::Outset => {
            let shade = if top_or_left { light } else { dark };
            (shade, shade)
        }
        BorderLineStyle::Groove => {
            if top_or_left {
                (dark, light)
            } else {
                (light, dark)
            }
        }
        BorderLineStyle::Ridge => {
            if top_or_left {
                (light, dark)
            } else {
                (dark, light)
            }
        }
        _ => (color, color),
    }
}

/// A color so dark that scaling it would barely change it, which a bevel
/// lifts by fixed amounts instead.
fn is_very_dark(color: Rgba) -> bool {
    0.299 * f32::from(color.r) + 0.587 * f32::from(color.g) + 0.114 * f32::from(color.b) <= 32.5
}

fn scaled(color: Rgba, multiplier: impl Fn(f32) -> f32) -> Rgba {
    let top = f32::from(color.r.max(color.g).max(color.b)) / 255.0;
    let factor = multiplier(top);
    let channel =
        |value: u8| ((f32::from(value) / 255.0 * factor * 255.999).floor()).clamp(0.0, 255.0) as u8;
    Rgba {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
        a: color.a,
    }
}

/// A very dark color with its brightest channel raised by `by` (of 255) and
/// the others in proportion; black, with none, becomes a gray.
fn lifted(color: Rgba, by: f32) -> Rgba {
    let top = f32::from(color.r.max(color.g).max(color.b)) / 255.0;
    let channel = |value: u8| {
        let value = f32::from(value) / 255.0;
        let lifted = if top <= 0.0 {
            by / 255.0
        } else {
            value * (top + by / 255.0) / top
        };
        (lifted * 255.999).floor().clamp(0.0, 255.0) as u8
    };
    Rgba {
        r: channel(color.r),
        g: channel(color.g),
        b: channel(color.b),
        a: color.a,
    }
}

/// The color of the shaded side of a bevel.
fn darker(color: Rgba) -> Rgba {
    if is_very_dark(color) {
        return lifted(color, 84.05);
    }
    scaled(color, |top| ((top - 0.33) / top).max(0.0))
}

/// The color of the lit side of a bevel.
fn lighter(color: Rgba) -> Rgba {
    if is_very_dark(color) {
        return lifted(color, 168.15);
    }
    scaled(color, |top| (top + 0.33).min(1.0) / top)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bevel_darkens_and_lightens_a_color_as_chromium_does() {
        let blue = Rgba::opaque(0x33, 0x66, 0xcc);
        assert_eq!(darker(blue), Rgba::opaque(0x1e, 0x3c, 0x78));
        assert_eq!(lighter(blue), Rgba::opaque(0x3f, 0x7f, 0xff));
        let gray = Rgba::opaque(0x80, 0x80, 0x80);
        assert_eq!(darker(gray), Rgba::opaque(0x2c, 0x2c, 0x2c));
        assert_eq!(lighter(gray), Rgba::opaque(0xd4, 0xd4, 0xd4));
        let peach = Rgba::opaque(0xff, 0xcc, 0x99);
        assert_eq!(darker(peach), Rgba::opaque(0xab, 0x89, 0x66));
        assert_eq!(lighter(peach), peach);
    }

    #[test]
    fn a_very_dark_color_is_lifted_by_fixed_amounts() {
        let black = Rgba::opaque(0, 0, 0);
        assert_eq!(darker(black), Rgba::opaque(0x54, 0x54, 0x54));
        assert_eq!(lighter(black), Rgba::opaque(0xa8, 0xa8, 0xa8));
        let near = Rgba::opaque(0x20, 0x20, 0x20);
        assert_eq!(darker(near), Rgba::opaque(0x74, 0x74, 0x74));
        let just_above = Rgba::opaque(0x21, 0x21, 0x21);
        assert_eq!(darker(just_above), Rgba::opaque(0, 0, 0));
        assert_eq!(lighter(just_above), Rgba::opaque(0x75, 0x75, 0x75));
        assert_eq!(lighter(Rgba::opaque(0x40, 0, 0)), Rgba::opaque(0xe9, 0, 0));
        assert_eq!(darker(Rgba::opaque(0, 0, 0x20)), Rgba::opaque(0, 0, 0x74));
    }

    #[test]
    fn each_style_shades_its_sides_the_way_chromium_does() {
        let blue = Rgba::opaque(0x33, 0x66, 0xcc);
        let (dark, light) = (darker(blue), lighter(blue));
        assert_eq!(bevel(BorderLineStyle::Inset, 0, blue).0, dark);
        assert_eq!(bevel(BorderLineStyle::Inset, 2, blue).0, light);
        assert_eq!(bevel(BorderLineStyle::Outset, 3, blue).0, light);
        assert_eq!(bevel(BorderLineStyle::Outset, 1, blue).0, dark);
        assert_eq!(bevel(BorderLineStyle::Groove, 0, blue), (dark, light));
        assert_eq!(bevel(BorderLineStyle::Groove, 2, blue), (light, dark));
        assert_eq!(bevel(BorderLineStyle::Ridge, 0, blue), (light, dark));
        assert_eq!(bevel(BorderLineStyle::Ridge, 1, blue), (dark, light));
    }

    #[test]
    fn the_polygon_round_stacked_boxes_steps_at_right_angles_where_lines_differ() {
        use tiny_skia::PathSegment;
        // A wide line over a narrow one, and then a wider one still.
        let rects = [
            (0.0, 0.0, 60.0, 20.0),
            (0.0, 20.0, 30.0, 20.0),
            (0.0, 40.0, 90.0, 20.0),
        ];
        let path = union_polygon(&rects, 3.0).expect("a polygon");
        let mut pen = None;
        for segment in path.segments() {
            if let PathSegment::LineTo(p) | PathSegment::MoveTo(p) = segment {
                if let Some((x, y)) = pen {
                    assert!(x == p.x || y == p.y, "({x},{y}) to ({},{})", p.x, p.y);
                }
                pen = Some((p.x, p.y));
            }
        }
        let bounds = path.bounds();
        assert_eq!(
            (bounds.left(), bounds.top(), bounds.right(), bounds.bottom()),
            (-3.0, -3.0, 93.0, 63.0)
        );
    }

    #[test]
    fn dashes_follow_chromiums_ratios() {
        assert_eq!(dash_pattern(1.0), (3.0, 2.0));
        assert_eq!(dash_pattern(2.0), (6.0, 4.0));
        assert_eq!(dash_pattern(6.0), (12.0, 6.0));
    }

    #[test]
    fn a_box_with_only_solid_sides_is_plain() {
        let side = |style| BorderSide {
            width: 2.0,
            color: Rgba::opaque(0, 0, 0),
            style,
        };
        let edges = |style| Edges {
            top: side(style),
            right: side(style),
            bottom: side(style),
            left: side(style),
        };
        assert!(is_plain(&edges(BorderLineStyle::Solid)));
        assert!(!is_plain(&edges(BorderLineStyle::Dashed)));
    }
}
