//! A box's rounded outline: `border-radius` resolved against the box, with
//! the spec's overlap clamping, and the geometry every rounded consumer
//! (background, border ring, clip, shadow, hit-test) shares. Lives here,
//! not in `florui-paint`, so `florui-layout`'s hit-testing can use it too;
//! turning it into a rasterizer path is paint's own job.

use crate::cascade::{Corners, LengthPercentage};

/// Corner order is top-left, top-right, bottom-right, bottom-left; each
/// radius is a (horizontal, vertical) pair in painted pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoundedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub radii: [(f32, f32); 4],
}

impl RoundedRect {
    /// Resolves `radius` against this border box. Lengths are logical
    /// pixels and scale with `scale_factor`; percentages resolve against
    /// the already-scaled box. Radii that would overlap are all scaled down
    /// by the same factor, as real CSS requires.
    pub fn new(
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: &Corners<(LengthPercentage, LengthPercentage)>,
        scale_factor: f32,
    ) -> Self {
        let resolve = |(h, v): &(LengthPercentage, LengthPercentage)| {
            (
                (h.length * scale_factor + h.percentage * width).max(0.0),
                (v.length * scale_factor + v.percentage * height).max(0.0),
            )
        };
        let mut radii = [
            resolve(&radius.top_left),
            resolve(&radius.top_right),
            resolve(&radius.bottom_right),
            resolve(&radius.bottom_left),
        ];
        let [tl, tr, br, bl] = radii;
        let mut factor = 1.0_f32;
        for (available, sum) in [
            (width, tl.0 + tr.0),
            (width, bl.0 + br.0),
            (height, tl.1 + bl.1),
            (height, tr.1 + br.1),
        ] {
            if sum > available {
                factor = factor.min(available / sum);
            }
        }
        if factor < 1.0 {
            for (h, v) in &mut radii {
                *h *= factor;
                *v *= factor;
            }
        }
        Self {
            x,
            y,
            width,
            height,
            radii,
        }
    }

    pub fn is_square(&self) -> bool {
        self.radii.iter().all(|&(h, v)| h <= 0.0 || v <= 0.0)
    }

    /// The box shrunk by `top/right/bottom/left` (a border's widths, to get
    /// the padding box). Each corner radius shrinks by the adjacent side
    /// widths; a corner with either axis used up becomes square.
    pub fn inset(&self, top: f32, right: f32, bottom: f32, left: f32) -> Self {
        let shrink = |(h, v): (f32, f32), dh: f32, dv: f32| {
            let (h, v) = ((h - dh).max(0.0), (v - dv).max(0.0));
            if h <= 0.0 || v <= 0.0 {
                (0.0, 0.0)
            } else {
                (h, v)
            }
        };
        Self {
            x: self.x + left,
            y: self.y + top,
            width: (self.width - left - right).max(0.0),
            height: (self.height - top - bottom).max(0.0),
            radii: [
                shrink(self.radii[0], left, top),
                shrink(self.radii[1], right, top),
                shrink(self.radii[2], right, bottom),
                shrink(self.radii[3], left, bottom),
            ],
        }
    }

    /// A square-cornered box.
    pub fn square(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            radii: [(0.0, 0.0); 4],
        }
    }

    pub fn translated(&self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..*self
        }
    }

    /// The box grown by `amount` on every side (negative shrinks), as a
    /// `box-shadow` spread does: a rounded corner's radius grows with it,
    /// while a square corner stays square.
    pub fn grown(&self, amount: f32) -> Self {
        let grow = |(h, v): (f32, f32)| {
            if h <= 0.0 || v <= 0.0 {
                return (0.0, 0.0);
            }
            let (h, v) = ((h + amount).max(0.0), (v + amount).max(0.0));
            if h <= 0.0 || v <= 0.0 {
                (0.0, 0.0)
            } else {
                (h, v)
            }
        };
        Self {
            x: self.x - amount,
            y: self.y - amount,
            width: (self.width + 2.0 * amount).max(0.0),
            height: (self.height + 2.0 * amount).max(0.0),
            radii: self.radii.map(grow),
        }
    }

    /// Whether the point lies inside the outline, corners included.
    pub fn contains(&self, px: f32, py: f32) -> bool {
        let (x1, y1) = (self.x + self.width, self.y + self.height);
        if px < self.x || px >= x1 || py < self.y || py >= y1 {
            return false;
        }
        let [tl, tr, br, bl] = self.radii;
        let corners = [
            (
                tl,
                self.x + tl.0,
                self.y + tl.1,
                px < self.x + tl.0 && py < self.y + tl.1,
            ),
            (
                tr,
                x1 - tr.0,
                self.y + tr.1,
                px >= x1 - tr.0 && py < self.y + tr.1,
            ),
            (br, x1 - br.0, y1 - br.1, px >= x1 - br.0 && py >= y1 - br.1),
            (
                bl,
                self.x + bl.0,
                y1 - bl.1,
                px < self.x + bl.0 && py >= y1 - bl.1,
            ),
        ];
        for ((rx, ry), cx, cy, in_corner) in corners {
            if in_corner && rx > 0.0 && ry > 0.0 {
                let (dx, dy) = ((px - cx) / rx, (py - cy) / ry);
                return dx * dx + dy * dy <= 1.0;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(length: f32) -> LengthPercentage {
        LengthPercentage {
            length,
            percentage: 0.0,
        }
    }

    fn pct(percentage: f32) -> LengthPercentage {
        LengthPercentage {
            length: 0.0,
            percentage,
        }
    }

    fn all(
        h: LengthPercentage,
        v: LengthPercentage,
    ) -> Corners<(LengthPercentage, LengthPercentage)> {
        Corners {
            top_left: (h, v),
            top_right: (h, v),
            bottom_right: (h, v),
            bottom_left: (h, v),
        }
    }

    #[test]
    fn zero_radius_is_square() {
        let rect = RoundedRect::new(1.0, 2.0, 30.0, 20.0, &all(px(0.0), px(0.0)), 1.0);
        assert!(rect.is_square());
    }

    #[test]
    fn lengths_scale_with_the_canvas_and_percentages_do_not_double_scale() {
        let rect = RoundedRect::new(0.0, 0.0, 200.0, 100.0, &all(px(8.0), pct(0.1)), 2.0);
        assert_eq!(rect.radii[0], (16.0, 10.0));
    }

    #[test]
    fn fifty_percent_on_a_square_is_a_circle() {
        let rect = RoundedRect::new(0.0, 0.0, 20.0, 20.0, &all(pct(0.5), pct(0.5)), 1.0);
        assert_eq!(rect.radii, [(10.0, 10.0); 4]);
    }

    #[test]
    fn overlapping_radii_scale_down_uniformly() {
        // 100px radii on a 100x40 box: the tallest sum (200 vs 40) wins.
        let rect = RoundedRect::new(0.0, 0.0, 100.0, 40.0, &all(px(100.0), px(100.0)), 1.0);
        assert_eq!(rect.radii, [(20.0, 20.0); 4]);
    }

    #[test]
    fn contains_excludes_the_cut_corner_but_not_the_edge_midpoints() {
        let rect = RoundedRect::new(0.0, 0.0, 20.0, 20.0, &all(px(10.0), px(10.0)), 1.0);
        assert!(!rect.contains(0.5, 0.5));
        assert!(rect.contains(10.0, 0.5));
        assert!(rect.contains(10.0, 10.0));
        assert!(!rect.contains(-1.0, 10.0));
    }

    #[test]
    fn grown_keeps_square_corners_square_and_grows_rounded_ones() {
        let rect = RoundedRect::new(0.0, 0.0, 20.0, 20.0, &all(px(4.0), px(4.0)), 1.0);
        let bigger = rect.grown(3.0);
        assert_eq!((bigger.x, bigger.width), (-3.0, 26.0));
        assert_eq!(bigger.radii, [(7.0, 7.0); 4]);
        assert!(
            RoundedRect::square(0.0, 0.0, 5.0, 5.0)
                .grown(2.0)
                .is_square()
        );
    }

    #[test]
    fn inset_shrinks_radii_by_the_border_and_squares_a_used_up_corner() {
        let rect = RoundedRect::new(0.0, 0.0, 100.0, 100.0, &all(px(10.0), px(10.0)), 1.0);
        let inner = rect.inset(2.0, 2.0, 2.0, 2.0);
        assert_eq!(inner.radii, [(8.0, 8.0); 4]);
        assert_eq!((inner.x, inner.width), (2.0, 96.0));
        assert!(rect.inset(10.0, 10.0, 10.0, 10.0).is_square());
    }
}
