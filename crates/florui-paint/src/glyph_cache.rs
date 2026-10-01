//! A cache of glyph outlines.
//!
//! A glyph's outline is a pure function of its font, its index in that font,
//! the size it is drawn at and the variable-font coordinates of its run.
//! Extracting it (scaling the font's own contours into path segments) cost
//! more than filling all the glyphs of a paragraph together, and the same
//! glyphs come back every frame. The segments are recorded exactly as the
//! outline reports them and replayed at each pen position with the same
//! arithmetic a direct draw uses, so a hit produces the path a fresh
//! extraction would, never an approximation.
//!
//! Bounded by the number of recorded segments; past the budget everything is
//! dropped and refills with whatever is on screen.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use skrifa::outline::OutlinePen;
use tiny_skia::PathBuilder;

/// Segments kept per thread before the cache starts over (about 11 MB).
const SEGMENT_BUDGET: usize = 400_000;

/// One outline command, in the font's own units, Y up.
#[derive(Clone, Copy)]
pub(crate) enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Records what an outline draws.
#[derive(Default)]
pub(crate) struct Recorder(pub(crate) Vec<Segment>);

impl OutlinePen for Recorder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Segment::Move(x, y));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Segment::Line(x, y));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0.push(Segment::Quad(cx0, cy0, x, y));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(Segment::Cubic(cx0, cy0, cx1, cy1, x, y));
    }

    fn close(&mut self) {
        self.0.push(Segment::Close);
    }
}

/// Appends `segments` to `builder`, offset by the pen position and with the
/// Y axis flipped (font units are Y up, the canvas is Y down).
pub(crate) fn replay(
    builder: &mut PathBuilder,
    origin_x: f32,
    origin_y: f32,
    segments: &[Segment],
) {
    for segment in segments {
        match *segment {
            Segment::Move(x, y) => builder.move_to(origin_x + x, origin_y - y),
            Segment::Line(x, y) => builder.line_to(origin_x + x, origin_y - y),
            Segment::Quad(cx0, cy0, x, y) => {
                builder.quad_to(origin_x + cx0, origin_y - cy0, origin_x + x, origin_y - y)
            }
            Segment::Cubic(cx0, cy0, cx1, cy1, x, y) => builder.cubic_to(
                origin_x + cx0,
                origin_y - cy0,
                origin_x + cx1,
                origin_y - cy1,
                origin_x + x,
                origin_y - y,
            ),
            Segment::Close => builder.close(),
        }
    }
}

#[derive(PartialEq, Eq, Hash)]
struct InstanceKey {
    font: u64,
    index: u32,
    size: u32,
    coords: Vec<i16>,
}

/// The glyphs of one font at one size and variation, by glyph id. An empty
/// entry is a glyph with nothing to draw (a space), remembered so it is not
/// looked up in the font again.
#[derive(Default)]
pub(crate) struct Instance {
    glyphs: RefCell<HashMap<u32, Rc<[Segment]>>>,
}

impl Instance {
    /// The recorded outline of `glyph`, recording it with `record` the first
    /// time it is asked for.
    pub(crate) fn outline(
        &self,
        glyph: u32,
        record: impl FnOnce() -> Vec<Segment>,
    ) -> Rc<[Segment]> {
        if let Some(found) = self.glyphs.borrow().get(&glyph) {
            return Rc::clone(found);
        }
        let segments: Rc<[Segment]> = record().into();
        SEGMENTS.with(|total| total.set(total.get() + segments.len() + 1));
        self.glyphs.borrow_mut().insert(glyph, Rc::clone(&segments));
        segments
    }
}

thread_local! {
    static INSTANCES: RefCell<HashMap<InstanceKey, Rc<Instance>>> = RefCell::new(HashMap::new());
    static SEGMENTS: Cell<usize> = const { Cell::new(0) };
}

/// The glyph table for `font` (its blob id and index) at `size` with a run's
/// variation `coords`, created empty on first use.
pub(crate) fn instance(font: u64, index: u32, size: f32, coords: &[i16]) -> Rc<Instance> {
    if SEGMENTS.with(Cell::get) > SEGMENT_BUDGET {
        INSTANCES.with(|instances| instances.borrow_mut().clear());
        SEGMENTS.with(|total| total.set(0));
    }
    let key = InstanceKey {
        font,
        index,
        size: size.to_bits(),
        coords: coords.to_vec(),
    };
    INSTANCES.with(|instances| {
        Rc::clone(
            instances
                .borrow_mut()
                .entry(key)
                .or_insert_with(|| Rc::new(Instance::default())),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outline_is_recorded_once_and_then_answered_from_the_cache() {
        let instance = instance(7, 0, 14.0, &[]);
        let mut recordings = 0;
        let mut ask = |glyph| {
            instance.outline(glyph, || {
                recordings += 1;
                vec![Segment::Move(1.0, 2.0), Segment::Close]
            })
        };
        let first = ask(5);
        let again = ask(5);
        ask(6);
        assert!(Rc::ptr_eq(&first, &again));
        assert_eq!(recordings, 2);
    }

    #[test]
    fn size_and_variation_make_separate_tables() {
        let a = instance(9, 0, 14.0, &[]);
        let same = instance(9, 0, 14.0, &[]);
        let bigger = instance(9, 0, 15.0, &[]);
        let bold = instance(9, 0, 14.0, &[16384]);
        assert!(Rc::ptr_eq(&a, &same));
        assert!(!Rc::ptr_eq(&a, &bigger));
        assert!(!Rc::ptr_eq(&a, &bold));
    }

    #[test]
    fn replay_offsets_and_flips_like_a_direct_draw() {
        let mut builder = PathBuilder::new();
        replay(
            &mut builder,
            10.0,
            20.0,
            &[
                Segment::Move(1.0, 2.0),
                Segment::Line(3.0, 4.0),
                Segment::Close,
            ],
        );
        let path = builder.finish().expect("a closed triangle-ish path");
        let points: Vec<_> = path.points().iter().map(|p| (p.x, p.y)).collect();
        assert_eq!(points, vec![(11.0, 18.0), (13.0, 16.0)]);
    }
}
