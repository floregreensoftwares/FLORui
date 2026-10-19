//! `clip-path` resolved for one box: the outline of the shape in the caller's
//! pixels, as polygons. Painting fills them as a mask and hit testing asks
//! whether a point is inside, so a box is hit exactly where it is drawn.
//!
//! Curves are flattened finely enough (a hundredth of a pixel) that filling the
//! polygon is the same, at any scale a screen has, as filling the curve.

use florui_style::clip::PathSegment;
use florui_style::{
    ClipPath, ClipShape, ComputedStyle, Corners, LengthPercentage, ReferenceBox, RoundedRect,
    ShapeRadius,
};

use crate::BoxLayout;

/// How far a flattened curve may stray from the true one, in pixels.
const FLATNESS: f32 = 0.01;

type Contour = Vec<(f32, f32)>;

/// The outline of a `clip-path` shape: closed polygons filled by the nonzero
/// rule, or the even-odd rule when `even_odd`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipGeometry {
    pub contours: Vec<Contour>,
    pub even_odd: bool,
}

impl ClipGeometry {
    /// Whether the point is inside the shape.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let mut winding = 0i32;
        let mut crossings = 0u32;
        for contour in &self.contours {
            let count = contour.len();
            for i in 0..count {
                let (a, b) = (contour[i], contour[(i + 1) % count]);
                // A ray to the right of the point, counting the edges it crosses.
                if (a.1 <= y) != (b.1 <= y) {
                    let t = (y - a.1) / (b.1 - a.1);
                    if a.0 + t * (b.0 - a.0) > x {
                        crossings += 1;
                        winding += if b.1 > a.1 { 1 } else { -1 };
                    }
                }
            }
        }
        if self.even_odd {
            crossings % 2 == 1
        } else {
            winding != 0
        }
    }
}

/// `style`'s `clip-path` for a box whose border box sits at `(x, y)` with
/// `layout`'s size, in the caller's pixels (`scale_factor` converts the authored
/// lengths); `None` when it has none. An empty shape is `Some` with no
/// contours: it clips everything.
pub fn resolve_clip_path(
    style: &ComputedStyle,
    layout: &BoxLayout,
    x: f32,
    y: f32,
    scale_factor: f32,
) -> Option<ClipGeometry> {
    match &style.clip_path {
        ClipPath::None | ClipPath::Url(_) => None,
        ClipPath::Box(reference) => {
            let rounded = reference_box(style, layout, x, y, scale_factor, *reference);
            Some(ClipGeometry {
                contours: vec![rounded_contour(&rounded)],
                even_odd: false,
            })
        }
        ClipPath::Shape(shape, reference) => {
            let rect = reference_box(style, layout, x, y, scale_factor, *reference);
            Some(resolve_shape(shape, &rect, scale_factor))
        }
    }
}

/// Whether `point` (in the page) is inside the `clip-path` of the box at `at`,
/// or the box has none. A `url()` to an SVG `<clipPath>` is drawn for the box and
/// its coverage read at the point; one that does not resolve clips nothing.
pub fn clip_admits(
    arena: &florui_style::Tree,
    styles: &florui_style::StyleMap,
    style: &ComputedStyle,
    layout: &BoxLayout,
    at: (f32, f32),
    point: (f32, f32),
) -> bool {
    let ClipPath::Url(url) = &style.clip_path else {
        return resolve_clip_path(style, layout, at.0, at.1, 1.0)
            .is_none_or(|shape| shape.contains(point.0, point.1));
    };
    let Some(reference) = florui_svg::reference(arena, styles, url) else {
        return true;
    };
    let size = (
        (layout.width.ceil() as u32).max(1),
        (layout.height.ceil() as u32).max(1),
    );
    let canvas = florui_svg::Canvas {
        size,
        scale: 1.0,
        origin: at,
        border_box: (at.0, at.1, layout.width, layout.height),
    };
    let Some(coverage) = florui_svg::clip_coverage(&reference, &canvas) else {
        return true;
    };
    let (x, y) = ((point.0 - at.0).floor(), (point.1 - at.1).floor());
    if x < 0.0 || y < 0.0 || x >= size.0 as f32 || y >= size.1 as f32 {
        return true;
    }
    coverage[y as usize * size.0 as usize + x as usize] >= 128
}

/// The reference box as a rounded rectangle: the box's own `border-radius`
/// brought in by the borders (padding box) and the padding (content box), or
/// pushed out by the margins (margin box, where a corner is round).
fn reference_box(
    style: &ComputedStyle,
    layout: &BoxLayout,
    x: f32,
    y: f32,
    scale_factor: f32,
    reference: ReferenceBox,
) -> RoundedRect {
    let border_box = RoundedRect::new(
        x,
        y,
        layout.width,
        layout.height,
        &style.border_radius,
        scale_factor,
    );
    let border = |side: f32| side * scale_factor;
    let (bt, br, bb, bl) = (
        border(style.border.top.width),
        border(style.border.right.width),
        border(style.border.bottom.width),
        border(style.border.left.width),
    );
    match reference {
        ReferenceBox::Border => border_box,
        ReferenceBox::Padding => border_box.inset(bt, br, bb, bl),
        ReferenceBox::Content => border_box.inset(
            bt + style.padding.top * scale_factor,
            br + style.padding.right * scale_factor,
            bb + style.padding.bottom * scale_factor,
            bl + style.padding.left * scale_factor,
        ),
        ReferenceBox::Margin => {
            // What layout made of `margin`, `auto` and percentages included.
            let used = layout.margin;
            let (mt, mr, mb, ml) = (
                used.top * scale_factor,
                used.right * scale_factor,
                used.bottom * scale_factor,
                used.left * scale_factor,
            );
            let grow = |(h, v): (f32, f32), dh: f32, dv: f32| {
                if h <= 0.0 || v <= 0.0 {
                    (0.0, 0.0)
                } else {
                    (h + dh, v + dv)
                }
            };
            let [tl, tr, brr, blr] = border_box.radii;
            RoundedRect {
                x: x - ml,
                y: y - mt,
                width: layout.width + ml + mr,
                height: layout.height + mt + mb,
                radii: [
                    grow(tl, ml, mt),
                    grow(tr, mr, mt),
                    grow(brr, mr, mb),
                    grow(blr, ml, mb),
                ],
            }
        }
    }
}

fn resolve_shape(shape: &ClipShape, rect: &RoundedRect, scale_factor: f32) -> ClipGeometry {
    let (w, h) = (rect.width, rect.height);
    let along =
        |lp: &LengthPercentage, basis: f32| lp.length * scale_factor + lp.percentage * basis;
    let point = |(px, py): &(LengthPercentage, LengthPercentage)| {
        (rect.x + along(px, w), rect.y + along(py, h))
    };
    let nonzero = |contours: Vec<Contour>| ClipGeometry {
        contours,
        even_odd: false,
    };
    match shape {
        ClipShape::Inset {
            top,
            right,
            bottom,
            left,
            radii,
        } => {
            let (x0, y0) = (rect.x + along(left, w), rect.y + along(top, h));
            let (x1, y1) = (rect.x + w - along(right, w), rect.y + h - along(bottom, h));
            if x1 <= x0 || y1 <= y0 {
                return nonzero(Vec::new());
            }
            let corners = Corners {
                top_left: radii[0],
                top_right: radii[1],
                bottom_right: radii[2],
                bottom_left: radii[3],
            };
            let rounded = RoundedRect::new(x0, y0, x1 - x0, y1 - y0, &corners, scale_factor);
            nonzero(vec![rounded_contour(&rounded)])
        }
        ClipShape::Circle { center, radius } => {
            let (cx, cy) = point(center);
            let r = shape_radius(radius, rect, cx, cy, Axis::Diagonal, scale_factor);
            nonzero(ellipse_contour(cx, cy, r, r).into_iter().collect())
        }
        ClipShape::Ellipse {
            center,
            radius_x,
            radius_y,
        } => {
            let (cx, cy) = point(center);
            let rx = shape_radius(radius_x, rect, cx, cy, Axis::Horizontal, scale_factor);
            let ry = shape_radius(radius_y, rect, cx, cy, Axis::Vertical, scale_factor);
            nonzero(ellipse_contour(cx, cy, rx, ry).into_iter().collect())
        }
        ClipShape::Polygon { even_odd, points } => ClipGeometry {
            contours: if points.len() >= 3 {
                vec![points.iter().map(point).collect()]
            } else {
                Vec::new()
            },
            even_odd: *even_odd,
        },
        ClipShape::Path { even_odd, segments } => ClipGeometry {
            contours: flatten_path(segments, &point, &|(px, py)| (along(px, w), along(py, h))),
            even_odd: *even_odd,
        },
    }
}

#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
    /// A circle's radius, whose percentages are of `sqrt((w^2 + h^2) / 2)`.
    Diagonal,
}

fn shape_radius(
    radius: &ShapeRadius,
    rect: &RoundedRect,
    cx: f32,
    cy: f32,
    axis: Axis,
    scale_factor: f32,
) -> f32 {
    let (left, right) = ((cx - rect.x).abs(), (rect.x + rect.width - cx).abs());
    let (top, bottom) = ((cy - rect.y).abs(), (rect.y + rect.height - cy).abs());
    let sides: &[f32] = match axis {
        Axis::Horizontal => &[left, right],
        Axis::Vertical => &[top, bottom],
        Axis::Diagonal => &[left, right, top, bottom],
    };
    let value = match radius {
        ShapeRadius::ClosestSide => sides.iter().copied().fold(f32::INFINITY, f32::min),
        ShapeRadius::FarthestSide => sides.iter().copied().fold(0.0, f32::max),
        ShapeRadius::Length(lp) => {
            let basis = match axis {
                Axis::Horizontal => rect.width,
                Axis::Vertical => rect.height,
                Axis::Diagonal => {
                    ((rect.width * rect.width + rect.height * rect.height) / 2.0).sqrt()
                }
            };
            lp.length * scale_factor + lp.percentage * basis
        }
    };
    value.max(0.0)
}

/// How many segments make a quarter turn of a curve of radius `r` stay within
/// [`FLATNESS`] of the true one.
fn segments_for_quarter(r: f32) -> usize {
    if r <= FLATNESS {
        return 1;
    }
    let step = 2.0 * (1.0 - FLATNESS / r).clamp(-1.0, 1.0).acos();
    ((std::f32::consts::FRAC_PI_2 / step).ceil() as usize).clamp(1, 256)
}

fn ellipse_contour(cx: f32, cy: f32, rx: f32, ry: f32) -> Option<Contour> {
    if rx <= 0.0 || ry <= 0.0 {
        return None;
    }
    let n = 4 * segments_for_quarter(rx.max(ry));
    Some(
        (0..n)
            .map(|i| {
                let angle = std::f32::consts::TAU * i as f32 / n as f32;
                (cx + rx * angle.cos(), cy + ry * angle.sin())
            })
            .collect(),
    )
}

/// A rounded rectangle's outline, clockwise from the top-left corner's end.
fn rounded_contour(rounded: &RoundedRect) -> Contour {
    let (x0, y0) = (rounded.x, rounded.y);
    let (x1, y1) = (rounded.x + rounded.width, rounded.y + rounded.height);
    let [tl, tr, br, bl] = rounded.radii;
    let mut out = Vec::new();
    // One corner: its arc about `(cx, cy)` from `start` to `end` radians.
    let mut corner =
        |radius: (f32, f32), cx: f32, cy: f32, start: f32, end: f32, at: (f32, f32)| {
            if radius.0 <= 0.0 || radius.1 <= 0.0 {
                out.push(at);
                return;
            }
            let n = segments_for_quarter(radius.0.max(radius.1));
            for i in 0..=n {
                let angle = start + (end - start) * i as f32 / n as f32;
                out.push((cx + radius.0 * angle.cos(), cy + radius.1 * angle.sin()));
            }
        };
    use std::f32::consts::{FRAC_PI_2, PI};
    corner(tl, x0 + tl.0, y0 + tl.1, PI, PI + FRAC_PI_2, (x0, y0));
    corner(tr, x1 - tr.0, y0 + tr.1, PI + FRAC_PI_2, 2.0 * PI, (x1, y0));
    corner(br, x1 - br.0, y1 - br.1, 0.0, FRAC_PI_2, (x1, y1));
    corner(bl, x0 + bl.0, y1 - bl.1, FRAC_PI_2, PI, (x0, y1));
    out
}

/// Reads a pair of lengths and percentages as pixels.
type PointFn<'a> = &'a dyn Fn(&(LengthPercentage, LengthPercentage)) -> (f32, f32);

/// A path's segments as polygons, one per subpath, each closed by filling.
fn flatten_path(segments: &[PathSegment], point: PointFn, size: PointFn) -> Vec<Contour> {
    let mut contours: Vec<Contour> = Vec::new();
    let mut current: Contour = Vec::new();
    let mut at = (0.0f32, 0.0f32);
    let mut start = (0.0f32, 0.0f32);
    let finish = |current: &mut Contour, contours: &mut Vec<Contour>| {
        if current.len() >= 2 {
            contours.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for segment in segments {
        match segment {
            PathSegment::Move(p) => {
                finish(&mut current, &mut contours);
                at = point(p);
                start = at;
                current.push(at);
            }
            PathSegment::Line(p) => {
                at = point(p);
                current.push(at);
            }
            PathSegment::Cubic(c1, c2, p) => {
                let (c1, c2, end) = (point(c1), point(c2), point(p));
                flatten_cubic(at, c1, c2, end, &mut current);
                at = end;
            }
            PathSegment::Quad(c1, p) => {
                let (c1, end) = (point(c1), point(p));
                // A quadratic is the cubic with the control points two thirds
                // of the way from each end to the one control point.
                let a = (
                    at.0 + 2.0 / 3.0 * (c1.0 - at.0),
                    at.1 + 2.0 / 3.0 * (c1.1 - at.1),
                );
                let b = (
                    end.0 + 2.0 / 3.0 * (c1.0 - end.0),
                    end.1 + 2.0 / 3.0 * (c1.1 - end.1),
                );
                flatten_cubic(at, a, b, end, &mut current);
                at = end;
            }
            PathSegment::Arc {
                radii,
                rotation,
                large,
                clockwise,
                to,
            } => {
                let end = point(to);
                flatten_arc(
                    at,
                    end,
                    *rotation,
                    *large,
                    *clockwise,
                    size(radii),
                    &mut current,
                );
                at = end;
            }
            PathSegment::Close => {
                at = start;
                if current.len() >= 2 {
                    finish(&mut current, &mut contours);
                }
                current.push(at);
            }
        }
    }
    finish(&mut current, &mut contours);
    contours
}

fn flatten_cubic(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    out: &mut Contour,
) {
    let length = |a: (f32, f32), b: (f32, f32)| ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
    let polygon = length(p0, p1) + length(p1, p2) + length(p2, p3);
    let n = ((polygon / 4.0).sqrt() * 4.0).ceil().clamp(8.0, 256.0) as usize;
    for i in 1..=n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push((
            a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
            a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
        ));
    }
}

/// The endpoint form of an elliptical arc (SVG's), flattened onto `out`.
fn flatten_arc(
    from: (f32, f32),
    to: (f32, f32),
    rotation_degrees: f32,
    large: bool,
    clockwise: bool,
    radii: (f32, f32),
    out: &mut Contour,
) {
    let (mut rx, mut ry) = (radii.0.abs(), radii.1.abs());
    if (from.0 == to.0 && from.1 == to.1) || rx == 0.0 || ry == 0.0 {
        out.push(to);
        return;
    }
    let phi = rotation_degrees.to_radians();
    let (sin_phi, cos_phi) = phi.sin_cos();
    let (dx, dy) = ((from.0 - to.0) / 2.0, (from.1 - to.1) / 2.0);
    let (x1p, y1p) = (cos_phi * dx + sin_phi * dy, -sin_phi * dx + cos_phi * dy);
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let scale = lambda.sqrt();
        rx *= scale;
        ry *= scale;
    }
    let numerator = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let denominator = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let factor = if denominator == 0.0 {
        0.0
    } else {
        (numerator / denominator).max(0.0).sqrt()
    };
    let sign = if large == clockwise { -1.0 } else { 1.0 };
    let (cxp, cyp) = (
        sign * factor * rx * y1p / ry,
        -sign * factor * ry * x1p / rx,
    );
    let (cx, cy) = (
        cos_phi * cxp - sin_phi * cyp + (from.0 + to.0) / 2.0,
        sin_phi * cxp + cos_phi * cyp + (from.1 + to.1) / 2.0,
    );
    let angle = |ux: f32, uy: f32, vx: f32, vy: f32| {
        let dot = ux * vx + uy * vy;
        let length = (ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt();
        let a = (dot / length).clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 { -a } else { a }
    };
    let start = angle(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut sweep = angle(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !clockwise && sweep > 0.0 {
        sweep -= std::f32::consts::TAU;
    } else if clockwise && sweep < 0.0 {
        sweep += std::f32::consts::TAU;
    }
    let n = ((sweep.abs() / std::f32::consts::FRAC_PI_2).ceil() as usize).max(1)
        * segments_for_quarter(rx.max(ry));
    for i in 1..=n {
        let a = start + sweep * i as f32 / n as f32;
        let (ex, ey) = (rx * a.cos(), ry * a.sin());
        out.push((
            cos_phi * ex - sin_phi * ey + cx,
            sin_phi * ex + cos_phi * ey + cy,
        ));
    }
}
