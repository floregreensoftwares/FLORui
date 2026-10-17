//! A node's transform as paint uses it, and the compositor for a group whose
//! plane is not a plain 2D affine map (a perspective or a 3D turn).

use florui_layout::Matrix4;
use tiny_skia::{Mask, Pixmap, Transform};

/// Where a node's plane lands on screen: [`florui_layout::resolve_node_transform`]
/// for the node, in the pixels of the surface it is painted on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Projection(pub Matrix4);

impl Projection {
    /// The plane does not move at all (what the matrix does off the plane
    /// does not show for a flat node).
    pub fn is_identity(&self) -> bool {
        self.0.is_plane_identity()
    }

    /// The plane as a 2D affine transform, when it stays one.
    pub fn affine(&self) -> Option<Transform> {
        self.0.plane_affine().map(|[a, b, c, d, e, f]| {
            Transform::from_row(a as f32, b as f32, c as f32, d as f32, e as f32, f as f32)
        })
    }

    /// The screen bounding box `(x0, y0, x1, y1)` of a rectangle on the plane;
    /// `None` when all of it is behind the eye.
    pub fn map_rect_aabb(
        &self,
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
    ) -> Option<(f32, f32, f32, f32)> {
        self.0.map_rect_bounds((x0, y0, x1, y1))
    }
}

/// Draws `source` (its pixel `(0, 0)` at `source_origin` in absolute
/// coordinates) onto `target` (likewise at `target_origin`) through the plane
/// homography of `plane`, at `opacity` and within `mask`.
///
/// Only the destination pixels whose centres lie inside `outline` (the
/// convex polygon, in absolute coordinates, that the visible part of the
/// source covers; the whole source rectangle when `None`) are drawn. Each is
/// sampled bilinearly at the point of the source that lands on it, so a
/// foreshortened or magnified plane has no gaps and no holes. Pieces of one
/// plane that meet along a cut share an exact edge: a pixel belongs to the
/// piece its centre is in, so nothing is drawn twice or left out.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_projected(
    target: &mut Pixmap,
    target_origin: (i32, i32),
    source: &Pixmap,
    source_origin: (i32, i32),
    plane: &Matrix4,
    outline: Option<&[(f32, f32)]>,
    opacity: f32,
    mask: Option<&Mask>,
) {
    let Some(inverse) = plane.inverse_homography() else {
        return;
    };
    let h = plane.homography();
    let source_rect = (
        source_origin.0 as f32,
        source_origin.1 as f32,
        source_origin.0 as f32 + source.width() as f32,
        source_origin.1 as f32 + source.height() as f32,
    );
    let whole;
    let outline = match outline {
        Some(outline) => outline,
        None => {
            whole = plane.project_quad(source_rect);
            &whole
        }
    };
    if outline.len() < 3 {
        return;
    }
    let (mut min_y, mut max_y) = (f32::INFINITY, f32::NEG_INFINITY);
    for &(_, y) in outline {
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    let (target_w, target_h) = (target.width() as i32, target.height() as i32);
    let row_from = ((min_y - 0.5).ceil() as i64 - i64::from(target_origin.1)).max(0);
    let row_to =
        ((max_y - 0.5).ceil() as i64 - i64::from(target_origin.1)).min(i64::from(target_h));
    let mask = mask.filter(|m| m.width() as i32 == target_w && m.height() as i32 == target_h);
    let (src_w, src_h) = (source.width() as i32, source.height() as i32);
    let src_data = source.data();
    let stride = target_w as usize * 4;
    let dst_data = target.data_mut();

    for row in row_from..row_to {
        let abs_y = f64::from(target_origin.1) + row as f64 + 0.5;
        let Some((x_lo, x_hi)) = scan_range(outline, abs_y as f32) else {
            continue;
        };
        let col_from = ((x_lo - 0.5).ceil() as i64 - i64::from(target_origin.0)).max(0);
        let col_to =
            ((x_hi - 0.5).ceil() as i64 - i64::from(target_origin.0)).min(i64::from(target_w));
        for col in col_from..col_to {
            let abs_x = f64::from(target_origin.0) + col as f64 + 0.5;
            let w = inverse[6] * abs_x + inverse[7] * abs_y + inverse[8];
            if !(w.is_finite() && w != 0.0) {
                continue;
            }
            let u = (inverse[0] * abs_x + inverse[1] * abs_y + inverse[2]) / w;
            let v = (inverse[3] * abs_x + inverse[4] * abs_y + inverse[5]) / w;
            // The plane point must be in front of the eye.
            if h[6] * u + h[7] * v + h[8] <= 1.0e-9 {
                continue;
            }
            let sx = u - f64::from(source_origin.0) - 0.5;
            let sy = v - f64::from(source_origin.1) - 0.5;
            let texel = bilinear(src_data, src_w, src_h, sx, sy);
            if texel[3] <= 0.0 {
                continue;
            }
            let mut coverage = opacity;
            if let Some(mask) = mask {
                coverage *=
                    f32::from(mask.data()[row as usize * target_w as usize + col as usize]) / 255.0;
            }
            if coverage <= 0.0 {
                continue;
            }
            let at = row as usize * stride + col as usize * 4;
            let dst = &mut dst_data[at..at + 4];
            let keep = 1.0 - texel[3] * coverage;
            for channel in 0..4 {
                let mixed = texel[channel] * coverage * 255.0 + f32::from(dst[channel]) * keep;
                dst[channel] = mixed.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// The part of the horizontal line at height `y` that lies inside the convex
/// `polygon`, as `(left, right)`.
fn scan_range(polygon: &[(f32, f32)], y: f32) -> Option<(f32, f32)> {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        let (top, bottom) = if a.1 <= b.1 { (a, b) } else { (b, a) };
        if y < top.1 || y >= bottom.1 {
            continue;
        }
        let t = (y - top.1) / (bottom.1 - top.1);
        let x = top.0 + t * (bottom.0 - top.0);
        lo = lo.min(x);
        hi = hi.max(x);
    }
    (lo <= hi).then_some((lo, hi))
}

/// The premultiplied colour `[r, g, b, a]` (each `0..=1`) of the source at the
/// continuous texel coordinates `(x, y)`, interpolated between the four
/// nearest texels; texels outside the source are clear.
fn bilinear(data: &[u8], width: i32, height: i32, x: f64, y: f64) -> [f32; 4] {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let fetch = |tx: i32, ty: i32| -> [f32; 4] {
        if tx < 0 || ty < 0 || tx >= width || ty >= height {
            return [0.0; 4];
        }
        let at = (ty as usize * width as usize + tx as usize) * 4;
        std::array::from_fn(|c| f32::from(data[at + c]) / 255.0)
    };
    let (p00, p10, p01, p11) = (
        fetch(x0, y0),
        fetch(x0 + 1, y0),
        fetch(x0, y0 + 1),
        fetch(x0 + 1, y0 + 1),
    );
    std::array::from_fn(|c| {
        let top = p00[c] + (p10[c] - p00[c]) * fx;
        let bottom = p01[c] + (p11[c] - p01[c]) * fx;
        top + (bottom - top) * fy
    })
}
