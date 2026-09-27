//! Turns a [`RoundedRect`] outline into a rasterizer path.

use florui_style::RoundedRect;
use tiny_skia::{Path, PathBuilder, Rect};

/// Cubic-Bezier control-point distance for a quarter ellipse.
const KAPPA: f32 = 0.552_284_7;

pub(crate) trait RoundedRectPath {
    /// The outline's closed path, or `None` when the box is empty.
    fn path(&self) -> Option<Path>;
}

impl RoundedRectPath for RoundedRect {
    fn path(&self) -> Option<Path> {
        if self.width <= 0.0 || self.height <= 0.0 {
            return None;
        }
        let mut builder = PathBuilder::new();
        if self.is_square() {
            builder.push_rect(Rect::from_xywh(self.x, self.y, self.width, self.height)?);
            return builder.finish();
        }
        let (x0, y0) = (self.x, self.y);
        let (x1, y1) = (self.x + self.width, self.y + self.height);
        let [tl, tr, br, bl] = self.radii;
        builder.move_to(x0 + tl.0, y0);
        builder.line_to(x1 - tr.0, y0);
        builder.cubic_to(
            x1 - tr.0 * (1.0 - KAPPA),
            y0,
            x1,
            y0 + tr.1 * (1.0 - KAPPA),
            x1,
            y0 + tr.1,
        );
        builder.line_to(x1, y1 - br.1);
        builder.cubic_to(
            x1,
            y1 - br.1 * (1.0 - KAPPA),
            x1 - br.0 * (1.0 - KAPPA),
            y1,
            x1 - br.0,
            y1,
        );
        builder.line_to(x0 + bl.0, y1);
        builder.cubic_to(
            x0 + bl.0 * (1.0 - KAPPA),
            y1,
            x0,
            y1 - bl.1 * (1.0 - KAPPA),
            x0,
            y1 - bl.1,
        );
        builder.line_to(x0, y0 + tl.1);
        builder.cubic_to(
            x0,
            y0 + tl.1 * (1.0 - KAPPA),
            x0 + tl.0 * (1.0 - KAPPA),
            y0,
            x0 + tl.0,
            y0,
        );
        builder.close();
        builder.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_square_box_paths_as_its_plain_rect() {
        let bounds = RoundedRect::square(1.0, 2.0, 30.0, 20.0)
            .path()
            .unwrap()
            .bounds();
        assert_eq!(
            (bounds.left(), bounds.top(), bounds.right(), bounds.bottom()),
            (1.0, 2.0, 31.0, 22.0)
        );
    }

    #[test]
    fn a_rounded_box_stays_within_its_own_bounds() {
        let mut rect = RoundedRect::square(0.0, 0.0, 20.0, 20.0);
        rect.radii = [(10.0, 10.0); 4];
        let bounds = rect.path().unwrap().bounds();
        assert_eq!(
            (bounds.left(), bounds.top(), bounds.right(), bounds.bottom()),
            (0.0, 0.0, 20.0, 20.0)
        );
    }

    #[test]
    fn an_empty_box_has_no_path() {
        assert!(RoundedRect::square(0.0, 0.0, 0.0, 5.0).path().is_none());
    }
}
