//! Where floats go. A float is placed against the floats before it by the rules
//! of CSS 2.1 section 9.5.1: as high as it can be, as far to its side as it can
//! be, never above an earlier float, and below the earlier floats it would
//! otherwise overlap when the room beside them is too narrow. The lines of the
//! content that follows are shortened by the floats beside them.
//!
//! Positions are in the coordinates of one block formatting context: `x` from
//! the left edge of its root's content box, `y` from its top. Every float is
//! kept as its margin box.

/// A float that was placed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PlacedFloat {
    pub(crate) left: bool,
    pub(crate) x0: f32,
    pub(crate) y0: f32,
    pub(crate) x1: f32,
    pub(crate) y1: f32,
}

/// The sides a `clear` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClearSides {
    pub(crate) left: bool,
    pub(crate) right: bool,
}

impl ClearSides {
    pub(crate) fn any(self) -> bool {
        self.left || self.right
    }
}

/// The floats of a block formatting context, in the order they were placed.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FloatSet {
    floats: Vec<PlacedFloat>,
}

/// Slack for the sums of lengths that should be equal.
const EPSILON: f32 = 0.001;

impl FloatSet {
    pub(crate) fn len(&self) -> usize {
        self.floats.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    /// Forgets the floats placed after the first `len`.
    pub(crate) fn truncate(&mut self, len: usize) {
        self.floats.truncate(len);
    }

    /// The bottom of the lowest float, whatever its side.
    pub(crate) fn lowest_bottom(&self) -> Option<f32> {
        self.floats.iter().map(|f| f.y1).reduce(f32::max)
    }

    /// The bottom of the lowest float on the sides `clear` names: where a
    /// box that clears them must start at the least.
    pub(crate) fn clear_bottom(&self, clear: ClearSides) -> Option<f32> {
        self.floats
            .iter()
            .filter(|f| (f.left && clear.left) || (!f.left && clear.right))
            .map(|f| f.y1)
            .reduce(f32::max)
    }

    /// The top of the lowest-placed float: a float is never above it.
    fn top_floor(&self) -> Option<f32> {
        self.floats.iter().map(|f| f.y0).reduce(f32::max)
    }

    fn overlaps(f: &PlacedFloat, y: f32, height: f32) -> bool {
        // A zero-height query is a point: the float is beside it if it covers it.
        let end = if height > 0.0 {
            y + height
        } else {
            y + EPSILON
        };
        f.y1 > f.y0 && f.y0 < end && f.y1 > y
    }

    /// The room between the floats for something occupying `y..y + height`,
    /// as the left and the right edge it may touch, within a containing block
    /// that spans `left..right`.
    pub(crate) fn room(&self, y: f32, height: f32, left: f32, right: f32) -> (f32, f32) {
        let mut l = left;
        let mut r = right;
        for f in self.floats.iter().filter(|f| Self::overlaps(f, y, height)) {
            if f.left {
                l = l.max(f.x1);
            } else {
                r = r.min(f.x0);
            }
        }
        (l, r)
    }

    /// The least bottom, past `y`, of a float that is beside `y..y + height`.
    pub(crate) fn bottom_below(&self, y: f32) -> Option<f32> {
        self.next_bottom(y, 0.0)
    }

    /// The least bottom, past `y`, of a float beside `y..y + height`.
    pub(crate) fn bottom_beside(&self, y: f32, height: f32) -> Option<f32> {
        self.next_bottom(y, height)
    }

    fn next_bottom(&self, y: f32, height: f32) -> Option<f32> {
        self.floats
            .iter()
            .filter(|f| Self::overlaps(f, y, height))
            .map(|f| f.y1)
            .reduce(f32::min)
    }

    /// Places a float of `width` by `height` (its margin box) on the left or the
    /// right of the containing block spanning `left..right`, not above `min_y`.
    /// Returns where its margin box starts.
    pub(crate) fn place(
        &mut self,
        width: f32,
        height: f32,
        on_left: bool,
        min_y: f32,
        (left, right): (f32, f32),
    ) -> (f32, f32) {
        let mut y = self.top_floor().map_or(min_y, |floor| min_y.max(floor));
        let x = loop {
            let (l, r) = self.room(y, height, left, right);
            let beside_floats = self.floats.iter().any(|f| Self::overlaps(f, y, height));
            if r - l + EPSILON >= width || !beside_floats {
                break if on_left { l } else { r - width };
            }
            match self.next_bottom(y, height) {
                Some(next) if next > y => y = next,
                _ => break if on_left { l } else { r - width },
            }
        };
        self.floats.push(PlacedFloat {
            left: on_left,
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
        });
        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: (f32, f32) = (0.0, 200.0);

    #[test]
    fn floats_on_one_side_line_up_and_the_next_row_starts_below_the_lowest_it_overlaps() {
        // Chrome 153: 90x30, 90x40 and 90x10 in 200 wide: the third goes to
        // y = 40, not under the first at y = 30, because it would be above the
        // bottom of the second, which is a left float it may not overlap.
        let mut floats = FloatSet::default();
        assert_eq!(floats.place(90.0, 30.0, true, 0.0, WIDTH), (0.0, 0.0));
        assert_eq!(floats.place(90.0, 40.0, true, 0.0, WIDTH), (90.0, 0.0));
        assert_eq!(floats.place(90.0, 10.0, true, 0.0, WIDTH), (0.0, 40.0));
    }

    #[test]
    fn a_float_is_never_above_the_one_before_it() {
        let mut floats = FloatSet::default();
        floats.place(50.0, 10.0, true, 20.0, WIDTH);
        assert_eq!(floats.place(50.0, 10.0, true, 0.0, WIDTH), (50.0, 20.0));
    }

    #[test]
    fn a_right_float_sits_at_the_right_edge_and_a_left_one_stops_at_it() {
        let mut floats = FloatSet::default();
        assert_eq!(floats.place(50.0, 60.0, false, 0.0, WIDTH), (150.0, 0.0));
        assert_eq!(floats.room(10.0, 5.0, 0.0, 200.0), (0.0, 150.0));
        // Too wide for the room beside it: below it.
        assert_eq!(floats.place(160.0, 10.0, true, 0.0, WIDTH), (0.0, 60.0));
    }

    #[test]
    fn clearing_a_side_asks_for_the_lowest_float_on_it() {
        let mut floats = FloatSet::default();
        floats.place(50.0, 30.0, true, 0.0, WIDTH);
        floats.place(50.0, 60.0, false, 0.0, WIDTH);
        let both = ClearSides {
            left: true,
            right: true,
        };
        let left = ClearSides {
            left: true,
            right: false,
        };
        assert_eq!(floats.clear_bottom(both), Some(60.0));
        assert_eq!(floats.clear_bottom(left), Some(30.0));
        assert_eq!(
            floats.clear_bottom(ClearSides {
                left: false,
                right: false
            }),
            None
        );
    }

    #[test]
    fn a_float_wider_than_its_container_with_nothing_beside_it_sticks_out() {
        let mut floats = FloatSet::default();
        assert_eq!(floats.place(250.0, 10.0, true, 0.0, WIDTH), (0.0, 0.0));
    }

    #[test]
    fn the_room_beside_a_float_ends_where_it_ends() {
        let mut floats = FloatSet::default();
        floats.place(50.0, 20.0, true, 0.0, WIDTH);
        assert_eq!(floats.room(0.0, 10.0, 0.0, 200.0), (50.0, 200.0));
        assert_eq!(floats.room(19.0, 10.0, 0.0, 200.0), (50.0, 200.0));
        assert_eq!(floats.room(20.0, 10.0, 0.0, 200.0), (0.0, 200.0));
    }
}
