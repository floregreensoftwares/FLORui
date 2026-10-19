//! A 4x4 transform matrix in CSS order, and what an element's `transform`, `translate`,
//! `rotate` and `scale` resolve to about a box.

use crate::{ComputedStyle, TransformFunction};

/// A 4x4 transform in CSS's `matrix3d()` order: the array is column-major, so
/// `m[column * 4 + row]` and a point maps as `x' = m[0]*x + m[4]*y + m[8]*z +
/// m[12]` (and likewise `y'`, `z'`, `w'` from rows 1 to 3). A 2D `matrix(a, b,
/// c, d, e, f)` is `[a, b, 0, 0, c, d, 0, 0, 0, 0, 1, 0, e, f, 0, 1]`.
///
/// Held in `f64`: a chain of rotations and a perspective divide loses visible
/// precision in `f32`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix4(pub [f64; 16]);

/// Points whose `w` is below this are behind the eye and are not drawn.
pub const NEAR_W: f64 = 1.0e-6;

impl Matrix4 {
    pub const IDENTITY: Matrix4 = Matrix4([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);

    pub fn translate(x: f64, y: f64, z: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[12] = x;
        m.0[13] = y;
        m.0[14] = z;
        m
    }

    pub fn scale(x: f64, y: f64, z: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0] = x;
        m.0[5] = y;
        m.0[10] = z;
        m
    }

    /// A rotation by `degrees` about the axis `(x, y, z)` (any length; the
    /// zero vector is no rotation), exactly CSS's `rotate3d()`: about z it is
    /// clockwise on screen, y pointing down.
    pub fn rotate(x: f64, y: f64, z: f64, degrees: f64) -> Self {
        let length = (x * x + y * y + z * z).sqrt();
        if !length.is_finite() || length == 0.0 {
            return Self::IDENTITY;
        }
        let (x, y, z) = (x / length, y / length, z / length);
        let half = degrees.to_radians() / 2.0;
        let (sin, cos) = half.sin_cos();
        let (sc, sq) = (sin * cos, sin * sin);
        Matrix4([
            1.0 - 2.0 * (y * y + z * z) * sq,
            2.0 * (x * y * sq + z * sc),
            2.0 * (x * z * sq - y * sc),
            0.0,
            2.0 * (x * y * sq - z * sc),
            1.0 - 2.0 * (x * x + z * z) * sq,
            2.0 * (y * z * sq + x * sc),
            0.0,
            2.0 * (x * z * sq + y * sc),
            2.0 * (y * z * sq - x * sc),
            1.0 - 2.0 * (x * x + y * y) * sq,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    /// CSS `skew(ax, ay)`: x shears with the tangent of `ax_degrees`, y with
    /// that of `ay_degrees`.
    pub fn skew(ax_degrees: f64, ay_degrees: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[4] = ax_degrees.to_radians().tan();
        m.0[1] = ay_degrees.to_radians().tan();
        m
    }

    /// CSS `perspective(d)`; an infinite or non-positive `d` is no perspective.
    pub fn perspective(d: f64) -> Self {
        let mut m = Self::IDENTITY;
        if d.is_finite() && d > 0.0 {
            m.0[11] = -1.0 / d;
        }
        m
    }

    /// CSS `matrix(a, b, c, d, e, f)`.
    pub fn from_2d(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0] = a;
        m.0[1] = b;
        m.0[4] = c;
        m.0[5] = d;
        m.0[12] = e;
        m.0[13] = f;
        m
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// `self` applied after `inner`: the product `self * inner`.
    pub fn after(self, inner: Matrix4) -> Matrix4 {
        let (a, b) = (&self.0, &inner.0);
        let mut out = [0.0; 16];
        for column in 0..4 {
            for row in 0..4 {
                out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
            }
        }
        Matrix4(out)
    }

    /// The matrix as it acts on the `z = 0` plane alone, with its own `z`
    /// output zeroed: whatever lies in front of or behind the plane no longer
    /// shows. This is what flattening a 3D context into its plane does.
    pub fn flatten(&self) -> Matrix4 {
        let mut m = self.0;
        for column in 0..4 {
            m[column * 4 + 2] = 0.0;
        }
        m[8] = 0.0;
        m[9] = 0.0;
        m[10] = 1.0;
        m[11] = 0.0;
        Matrix4(m)
    }

    /// The 3x3 homography of the `z = 0` plane, row-major: `(x, y, 1)` maps to
    /// `(h0*x + h1*y + h2, h3*x + h4*y + h5, h6*x + h7*y + h8)` with the last
    /// component the divisor.
    pub fn homography(&self) -> [f64; 9] {
        let m = &self.0;
        [m[0], m[4], m[12], m[1], m[5], m[13], m[3], m[7], m[15]]
    }

    /// The inverse of [`Self::homography`], row-major, or `None` when the
    /// plane is edge-on or flattened.
    pub fn inverse_homography(&self) -> Option<[f64; 9]> {
        invert3(&self.homography())
    }
    /// Whether the plane is exactly where it started, whatever the matrix does
    /// off the plane.
    pub fn is_plane_identity(&self) -> bool {
        self.homography() == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    }

    /// The plane's `(a, b, c, d, e, f)` when it stays a 2D affine map (no
    /// perspective on the plane), in CSS `matrix()` order.
    pub fn plane_affine(&self) -> Option<[f64; 6]> {
        let h = self.homography();
        (h[6] == 0.0 && h[7] == 0.0 && h[8] == 1.0).then(|| [h[0], h[3], h[1], h[4], h[2], h[5]])
    }

    /// The `(a, b, c, d, e, f)` of `matrix()` that a browser draws an element of an
    /// SVG with: the entries of the matrix as they stand, the row that carries a
    /// perspective dropped, never divided by. A rotation about x or y shows as the
    /// squeeze of the plane, and a perspective as the shear and scale its
    /// terms leave once the transform origin is folded in; Chrome draws it so.
    pub fn flattened_affine(&self) -> [f64; 6] {
        let m = &self.0;
        [m[0], m[1], m[4], m[5], m[12], m[13]]
    }

    /// `(x, y, z)` mapped to homogeneous `[x', y', z', w']`.
    pub fn map_homogeneous(&self, (x, y, z): (f64, f64, f64)) -> [f64; 4] {
        let m = &self.0;
        [
            m[0] * x + m[4] * y + m[8] * z + m[12],
            m[1] * x + m[5] * y + m[9] * z + m[13],
            m[2] * x + m[6] * y + m[10] * z + m[14],
            m[3] * x + m[7] * y + m[11] * z + m[15],
        ]
    }

    /// The plane point `(x, y)` on screen, or `None` behind the eye.
    pub fn map_point(&self, (x, y): (f32, f32)) -> Option<(f32, f32)> {
        let [px, py, _, w] = self.map_homogeneous((f64::from(x), f64::from(y), 0.0));
        (w > NEAR_W).then(|| ((px / w) as f32, (py / w) as f32))
    }

    /// The plane point that lands on screen at `(x, y)`, or `None` when the
    /// plane is edge-on or flattened, or the point is only reached behind the
    /// eye.
    pub fn unmap_point(&self, (x, y): (f32, f32)) -> Option<(f32, f32)> {
        let inverse = invert3(&self.homography())?;
        let (x, y) = (f64::from(x), f64::from(y));
        let u = inverse[0] * x + inverse[1] * y + inverse[2];
        let v = inverse[3] * x + inverse[4] * y + inverse[5];
        let w = inverse[6] * x + inverse[7] * y + inverse[8];
        if !w.is_finite() || w == 0.0 {
            return None;
        }
        let (u, v) = (u / w, v / w);
        let [_, _, _, forward] = self.map_homogeneous((u, v, 0.0));
        (forward > NEAR_W && u.is_finite() && v.is_finite()).then_some((u as f32, v as f32))
    }

    /// The screen bounding box `(x0, y0, x1, y1)` of the rectangle on the
    /// plane, with the part behind the eye cut away; `None` when none of it is
    /// in front.
    pub fn map_rect_bounds(&self, rect: (f32, f32, f32, f32)) -> Option<(f32, f32, f32, f32)> {
        let polygon = self.project_quad(rect);
        if polygon.is_empty() {
            return None;
        }
        let mut bounds = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for (x, y) in polygon {
            bounds = (
                bounds.0.min(x),
                bounds.1.min(y),
                bounds.2.max(x),
                bounds.3.max(y),
            );
        }
        Some(bounds)
    }

    /// The outline of the rectangle `(x0, y0, x1, y1)` on screen, clipped to
    /// the half space in front of the eye (`w > 0`).
    pub fn project_quad(&self, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> Vec<(f32, f32)> {
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            .map(|(x, y)| self.map_homogeneous((f64::from(x), f64::from(y), 0.0)));
        let mut clipped: Vec<[f64; 4]> = Vec::with_capacity(5);
        for i in 0..4 {
            let (current, next) = (corners[i], corners[(i + 1) % 4]);
            let (current_in, next_in) = (current[3] > NEAR_W, next[3] > NEAR_W);
            if current_in {
                clipped.push(current);
            }
            if current_in != next_in {
                let t = (NEAR_W - current[3]) / (next[3] - current[3]);
                clipped.push(std::array::from_fn(|k| {
                    current[k] + t * (next[k] - current[k])
                }));
            }
        }
        clipped
            .into_iter()
            .map(|p| ((p[0] / p[3]) as f32, (p[1] / p[3]) as f32))
            .collect()
    }

    /// Whether the plane shows its back to the viewer. The sign of the
    /// determinant of three corners' homogeneous coordinates is the sign of
    /// the transformed normal along the line of sight, and stays right when
    /// perspective makes some corners' `w` differ; an untransformed plane
    /// gives a positive one.
    pub fn faces_away(&self, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> bool {
        let map = |x: f32, y: f32| self.map_homogeneous((f64::from(x), f64::from(y), 0.0));
        let (a, b, c) = (map(x0, y0), map(x1, y0), map(x1, y1));
        let h = |p: [f64; 4]| [p[0], p[1], p[3]];
        let (a, b, c) = (h(a), h(b), h(c));
        let det = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
        det < 0.0
    }
}

/// The inverse of a row-major 3x3, or `None` when it is singular.
fn invert3(h: &[f64; 9]) -> Option<[f64; 9]> {
    let minor = |a: f64, b: f64, c: f64, d: f64| a * d - b * c;
    let c00 = minor(h[4], h[5], h[7], h[8]);
    let c01 = -minor(h[3], h[5], h[6], h[8]);
    let c02 = minor(h[3], h[4], h[6], h[7]);
    let det = h[0] * c00 + h[1] * c01 + h[2] * c02;
    if !det.is_finite() || det == 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        c00 * inv,
        -minor(h[1], h[2], h[7], h[8]) * inv,
        minor(h[1], h[2], h[4], h[5]) * inv,
        c01 * inv,
        minor(h[0], h[2], h[6], h[8]) * inv,
        -minor(h[0], h[2], h[3], h[5]) * inv,
        c02 * inv,
        -minor(h[0], h[1], h[6], h[7]) * inv,
        minor(h[0], h[1], h[3], h[4]) * inv,
    ])
}

/// `style`'s own transforms (`translate`, `rotate`, `scale`, then the `transform`
/// list, in that order) folded into one matrix about its `transform-origin`, for
/// the reference box `(x, y, width, height)` that the origin and the percentages
/// are of, and the `origin` in it. `scale_factor` converts the authored lengths to the caller's pixels
/// (`1.0` for logical ones).
pub fn transform_matrix(
    style: &ComputedStyle,
    (x, y, width, height): (f64, f64, f64, f64),
    (origin_x_lp, origin_y_lp): (crate::LengthPercentage, crate::LengthPercentage),
    scale_factor: f32,
) -> Matrix4 {
    let sf = f64::from(scale_factor);
    let length = |lp: crate::LengthPercentage, basis: f64| {
        f64::from(lp.length) * sf + f64::from(lp.percentage) * basis
    };

    let mut list = Matrix4::IDENTITY;
    if let Some((tx, ty, tz)) = style.translate {
        list = list.after(Matrix4::translate(
            length(tx, width),
            length(ty, height),
            f64::from(tz) * sf,
        ));
    }
    if let Some((ax, ay, az, degrees)) = style.rotate {
        list = list.after(Matrix4::rotate(
            f64::from(ax),
            f64::from(ay),
            f64::from(az),
            f64::from(degrees),
        ));
    }
    if let Some((sx, sy, sz)) = style.scale {
        list = list.after(Matrix4::scale(f64::from(sx), f64::from(sy), f64::from(sz)));
    }
    for function in &style.transform {
        let next = match *function {
            TransformFunction::Translate(tx, ty) => {
                Matrix4::translate(length(tx, width), length(ty, height), 0.0)
            }
            TransformFunction::Translate3d(tx, ty, tz) => {
                Matrix4::translate(length(tx, width), length(ty, height), f64::from(tz) * sf)
            }
            TransformFunction::Scale(sx, sy) => Matrix4::scale(f64::from(sx), f64::from(sy), 1.0),
            TransformFunction::Scale3d(sx, sy, sz) => {
                Matrix4::scale(f64::from(sx), f64::from(sy), f64::from(sz))
            }
            TransformFunction::Rotate(degrees) => {
                Matrix4::rotate(0.0, 0.0, 1.0, f64::from(degrees))
            }
            TransformFunction::Rotate3d { x, y, z, degrees } => {
                Matrix4::rotate(f64::from(x), f64::from(y), f64::from(z), f64::from(degrees))
            }
            TransformFunction::Skew(ax, ay) => Matrix4::skew(f64::from(ax), f64::from(ay)),
            TransformFunction::Perspective(d) => Matrix4::perspective(f64::from(d) * sf),
            TransformFunction::Matrix { a, b, c, d, e, f } => Matrix4::from_2d(
                f64::from(a),
                f64::from(b),
                f64::from(c),
                f64::from(d),
                f64::from(e) * sf,
                f64::from(f) * sf,
            ),
            TransformFunction::Matrix3d(m) => {
                // Authored in CSS pixels: lengths grow with the device scale, the
                // per-length perspective terms shrink with it.
                let mut m = m.map(f64::from);
                for translation in &mut m[12..15] {
                    *translation *= sf;
                }
                for perspective in [3, 7, 11] {
                    m[perspective] /= sf;
                }
                Matrix4(m)
            }
        };
        list = list.after(next);
    }

    let origin_x = x + length(origin_x_lp, width);
    let origin_y = y + length(origin_y_lp, height);
    let origin_z = f64::from(style.transform_origin_z) * sf;
    Matrix4::translate(origin_x, origin_y, origin_z)
        .after(list)
        .after(Matrix4::translate(-origin_x, -origin_y, -origin_z))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattening_keeps_the_entries_and_drops_the_perspective_row() {
        // perspective(100px) rotateY(40deg): x leaves a share of itself in w, and
        // the flat matrix has no use for it.
        let m = Matrix4::perspective(100.0).after(Matrix4::rotate(0.0, 1.0, 0.0, 40.0));
        let [a, b, c, d, e, f] = m.flattened_affine();
        assert!((a - 40.0f64.to_radians().cos()).abs() < 1e-12);
        assert_eq!((b, c, d, e, f), (0.0, 0.0, 1.0, 0.0, 0.0));
        assert!(m.plane_affine().is_none());
        // A plane that is affine flattens to exactly what it is.
        let m = Matrix4::translate(3.0, 4.0, 0.0).after(Matrix4::scale(2.0, 5.0, 1.0));
        assert_eq!(m.flattened_affine(), [2.0, 0.0, 0.0, 5.0, 3.0, 4.0]);
        assert_eq!(m.plane_affine(), Some(m.flattened_affine()));
    }
}
