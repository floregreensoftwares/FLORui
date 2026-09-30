//! The 2D affine matrix a node's `transform` resolves to. Painting and hit
//! testing both read it from here, so a node is hit exactly where it is drawn.

use florui_style::{ComputedStyle, TransformFunction};

use crate::BoxLayout;

/// `x' = a*x + c*y + e`, `y' = b*x + d*y + f` — CSS `matrix()`'s own order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(x: f32, y: f32) -> Self {
        Self {
            e: x,
            f: y,
            ..Self::IDENTITY
        }
    }

    pub fn scale(x: f32, y: f32) -> Self {
        Self {
            a: x,
            d: y,
            ..Self::IDENTITY
        }
    }

    /// A clockwise rotation (y points down) by `degrees`.
    pub fn rotate(degrees: f32) -> Self {
        let radians = degrees.to_radians();
        let (sin, cos) = (radians.sin(), radians.cos());
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            ..Self::IDENTITY
        }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// `self` applied after `inner`.
    pub fn after(self, inner: Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.c * inner.b,
            b: self.b * inner.a + self.d * inner.b,
            c: self.a * inner.c + self.c * inner.d,
            d: self.b * inner.c + self.d * inner.d,
            e: self.a * inner.e + self.c * inner.f + self.e,
            f: self.b * inner.e + self.d * inner.f + self.f,
        }
    }

    /// The matrix undoing `self`, or `None` if it flattens the plane (a zero
    /// scale) and so has none.
    pub fn invert(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det == 0.0 {
            return None;
        }
        Some(Affine {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }

    pub fn map_point(&self, (x, y): (f32, f32)) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

/// `style`'s `transform` function list folded into one matrix about its
/// `transform-origin`, for a node whose border box sits at `(x, y)` with
/// `layout`'s size. `scale_factor` converts the authored lengths to the
/// caller's pixels (`1.0` for logical ones).
pub fn resolve_transform(
    style: &ComputedStyle,
    layout: &BoxLayout,
    x: f32,
    y: f32,
    scale_factor: f32,
) -> Affine {
    if style.transform.is_empty() {
        return Affine::IDENTITY;
    }
    let mut list = Affine::IDENTITY;
    for function in &style.transform {
        let next = match *function {
            TransformFunction::Translate(tx, ty) => Affine::translate(
                tx.length * scale_factor + tx.percentage * layout.width,
                ty.length * scale_factor + ty.percentage * layout.height,
            ),
            TransformFunction::Scale(sx, sy) => Affine::scale(sx, sy),
            TransformFunction::Rotate(degrees) => Affine::rotate(degrees),
            TransformFunction::Matrix { a, b, c, d, e, f } => Affine {
                a,
                b,
                c,
                d,
                e: e * scale_factor,
                f: f * scale_factor,
            },
        };
        list = list.after(next);
    }

    let (origin_x_lp, origin_y_lp) = style.transform_origin;
    let origin_x = x + origin_x_lp.length * scale_factor + origin_x_lp.percentage * layout.width;
    let origin_y = y + origin_y_lp.length * scale_factor + origin_y_lp.percentage * layout.height;
    Affine::translate(origin_x, origin_y)
        .after(list)
        .after(Affine::translate(-origin_x, -origin_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn a_matrix_and_its_inverse_undo_each_other() {
        let m = Affine::translate(10.0, -4.0)
            .after(Affine::rotate(37.0))
            .after(Affine::scale(2.0, 0.5));
        let back = m.invert().expect("invertible");
        assert!(close(back.map_point(m.map_point((3.0, 8.0))), (3.0, 8.0)));
    }

    #[test]
    fn after_applies_the_inner_matrix_first() {
        let m = Affine::translate(10.0, 0.0).after(Affine::scale(2.0, 2.0));
        assert!(close(m.map_point((1.0, 1.0)), (12.0, 2.0)));
    }

    #[test]
    fn rotation_is_clockwise_with_y_down() {
        assert!(close(
            Affine::rotate(90.0).map_point((1.0, 0.0)),
            (0.0, 1.0)
        ));
    }

    #[test]
    fn a_flattening_matrix_has_no_inverse() {
        assert!(Affine::scale(0.0, 1.0).invert().is_none());
    }
}
