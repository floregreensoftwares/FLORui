//! How sharp a frame is painted while the window is being dragged to a new
//! size. Every size is a frame nobody keeps, so what is wanted is a frame soon:
//! a frame can be painted at a fraction of the window's pixels and stretched,
//! and the frame at full size follows once the drag has stopped. [`LiveResize`]
//! is the window's choice of how that is decided.

use std::time::Duration;

/// How a window is painted while its edge is being dragged to a new size.
///
/// The choice is the window's own (`WindowOptions::live_resize`), and a window
/// that makes none gets [`LiveResize::default`]. Whatever it is, the frame at the
/// window's full size follows about 140 ms after the last change of size.
///
/// | choice | while dragging | costs |
/// |---|---|---|
/// | [`Sharp`](Self::Sharp) | always the window's own size | as long as a full frame takes, which is the drag's pace |
/// | [`Adaptive`](Self::Adaptive) (default) | the window's own size while a frame fits `budget_ms`, else smaller | sharp on a machine that keeps up, soft on one that does not |
/// | [`Soft`](Self::Soft) | always a fraction of it, stretched | the lightest drag, and a deliberately softer look |
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LiveResize {
    /// Every frame is painted at the window's own size, however long it takes.
    Sharp,
    /// Frames are painted at the window's own size while that costs no more than
    /// `budget_ms` of painting, and at three quarters or a half of each side,
    /// stretched, when it does. The cost of each size is measured as it is used,
    /// and the larger sizes are tried again now and then.
    Adaptive {
        /// What painting a frame may cost while dragging, in milliseconds.
        budget_ms: f32,
    },
    /// Every frame is painted at `scale` of each side of the window and
    /// stretched, whether or not the machine could do more. `scale` is kept
    /// between a quarter and nine tenths.
    Soft {
        /// The fraction of each side painted.
        scale: f32,
    },
}

impl Default for LiveResize {
    /// Adaptive, within [`DEFAULT_BUDGET_MS`].
    fn default() -> Self {
        Self::Adaptive {
            budget_ms: DEFAULT_BUDGET_MS,
        }
    }
}

impl LiveResize {
    /// What `FLORUI_SHARP_RESIZE` (any value) or `FLORUI_SOFT_RESIZE` (the
    /// fraction of each side, such as `0.5`) in the environment ask for, over
    /// whatever a window chose: a way to compare the looks without changing code.
    pub fn from_environment() -> Option<Self> {
        if std::env::var_os("FLORUI_SHARP_RESIZE").is_some() {
            return Some(Self::Sharp);
        }
        std::env::var("FLORUI_SOFT_RESIZE")
            .ok()
            .and_then(|value| value.trim().parse::<f32>().ok())
            .map(|scale| Self::Soft { scale })
    }
}

/// What a frame painted while dragging should cost unless a window says
/// otherwise, in milliseconds: about thirty frames a second.
pub const DEFAULT_BUDGET_MS: f32 = 34.0;

/// The smallest and largest fraction of a side a [`LiveResize::Soft`] frame is
/// painted at.
const SOFT_RANGE: (f32, f32) = (0.25, 0.9);

/// A window's choice of [`LiveResize`], and what it has learnt of the frames
/// painted under it.
pub(crate) struct LiveResizer {
    choice: LiveResize,
    quality: FrameQuality,
}

impl LiveResizer {
    pub(crate) fn new(choice: LiveResize) -> Self {
        let budget_ms = match choice {
            LiveResize::Adaptive { budget_ms } if budget_ms > 0.0 => budget_ms,
            _ => DEFAULT_BUDGET_MS,
        };
        Self {
            choice,
            quality: FrameQuality::with_budget(budget_ms),
        }
    }

    /// The fraction of each side to paint the next frame at while dragging.
    pub(crate) fn fraction(&mut self) -> f32 {
        match self.choice {
            LiveResize::Sharp => 1.0,
            LiveResize::Soft { scale } => scale.clamp(SOFT_RANGE.0, SOFT_RANGE.1),
            LiveResize::Adaptive { .. } => self.quality.pick(),
        }
    }

    /// Learns from a frame painted at `fraction` that took `painted_ms`.
    pub(crate) fn record(&mut self, fraction: f32, painted_ms: f32) {
        if matches!(self.choice, LiveResize::Adaptive { .. }) {
            self.quality.record(fraction, painted_ms);
        }
    }
}

/// How long after the last size change the window is still taken to be
/// resizing.
pub(crate) const SETTLE: Duration = Duration::from_millis(140);

/// The fractions of the window's size a frame may be painted at, largest first.
const STEPS: [f32; 3] = [1.0, 0.75, 0.5];

/// Every this many frames the sizes above the one in use are measured again: a
/// frame is cheaper on a window that has stopped changing under it, and what a
/// small frame costs says little about what a big one does (much of it does not
/// follow the area).
const REPROBE_EVERY: u32 = 12;

/// What painting a frame has cost at each size, as measured.
pub(crate) struct FrameQuality {
    budget_ms: f32,
    cost_ms: [Option<f32>; STEPS.len()],
    frames: u32,
}

impl Default for FrameQuality {
    fn default() -> Self {
        Self::with_budget(DEFAULT_BUDGET_MS)
    }
}

impl FrameQuality {
    pub(crate) fn with_budget(budget_ms: f32) -> Self {
        Self {
            budget_ms,
            cost_ms: [None; STEPS.len()],
            frames: 0,
        }
    }

    /// The fraction of the window's size to paint the next frame at: the largest
    /// whose cost is not known to be over the budget. A size not yet measured is
    /// tried, so the first frame of a drag is at full size and finds out.
    pub(crate) fn pick(&mut self) -> f32 {
        self.frames += 1;
        let budget_ms = self.budget_ms;
        let choose = |cost_ms: &[Option<f32>; STEPS.len()]| {
            STEPS
                .iter()
                .zip(cost_ms)
                .find(|(_, cost)| cost.is_none_or(|cost| cost <= budget_ms))
                .map_or(STEPS[STEPS.len() - 1], |(&step, _)| step)
        };
        let chosen = choose(&self.cost_ms);
        if self.frames.is_multiple_of(REPROBE_EVERY) {
            // Forget what the larger sizes cost, and so try the one above.
            let index = STEPS.iter().position(|&step| step == chosen).unwrap_or(0);
            for cost in &mut self.cost_ms[..index] {
                *cost = None;
            }
            return choose(&self.cost_ms);
        }
        chosen
    }

    /// Learns from a frame painted at `fraction` of the window's size that took
    /// `painted_ms`.
    pub(crate) fn record(&mut self, fraction: f32, painted_ms: f32) {
        let Some(index) = STEPS
            .iter()
            .position(|&step| (step - fraction).abs() < 1.0e-3)
        else {
            return;
        };
        self.cost_ms[index] = Some(match self.cost_ms[index] {
            None => painted_ms,
            Some(before) => 0.5 * before + 0.5 * painted_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_of_a_drag_is_painted_at_full_size() {
        assert_eq!(FrameQuality::default().pick(), 1.0);
    }

    #[test]
    fn a_size_that_fits_the_budget_stays_and_one_that_does_not_gives_way() {
        let mut quality = FrameQuality::default();
        quality.record(1.0, 20.0);
        assert_eq!(quality.pick(), 1.0, "20 ms is within the budget");

        let mut quality = FrameQuality::default();
        quality.record(1.0, 90.0);
        assert_eq!(
            quality.pick(),
            0.75,
            "90 ms is not, and 0.75 is not known yet"
        );
        quality.record(0.75, 60.0);
        assert_eq!(quality.pick(), 0.5);
        quality.record(0.5, 50.0);
        assert_eq!(quality.pick(), 0.5, "half is the least, whatever it costs");
    }

    #[test]
    fn a_small_frame_does_not_stand_for_a_big_one() {
        // Painted at a half the frame took 25 ms; that says nothing of full size,
        // which has been measured at 30 and fits.
        let mut quality = FrameQuality::default();
        quality.record(1.0, 30.0);
        quality.record(0.5, 25.0);
        assert_eq!(quality.pick(), 1.0);
    }

    #[test]
    fn the_larger_sizes_are_tried_again_now_and_then() {
        let mut quality = FrameQuality::default();
        quality.record(1.0, 90.0);
        quality.record(0.75, 60.0);
        quality.record(0.5, 20.0);
        let mut picks = Vec::new();
        for _ in 0..REPROBE_EVERY {
            picks.push(quality.pick());
        }
        assert!(picks[..picks.len() - 1].iter().all(|&pick| pick == 0.5));
        assert_eq!(
            picks.last(),
            Some(&1.0),
            "the twelfth frame tries full size again: {picks:?}"
        );
        // It fits now, and stays.
        quality.record(1.0, 25.0);
        assert_eq!(quality.pick(), 1.0);
    }

    #[test]
    fn a_window_that_asks_for_sharp_is_never_painted_small() {
        let mut sharp = LiveResizer::new(LiveResize::Sharp);
        for _ in 0..30 {
            assert_eq!(sharp.fraction(), 1.0);
            sharp.record(1.0, 500.0);
        }
    }

    #[test]
    fn a_window_that_asks_for_soft_is_always_painted_at_its_scale() {
        let mut soft = LiveResizer::new(LiveResize::Soft { scale: 0.6 });
        for _ in 0..30 {
            assert_eq!(soft.fraction(), 0.6);
            soft.record(0.6, 1.0);
        }
        assert_eq!(
            LiveResizer::new(LiveResize::Soft { scale: 0.05 }).fraction(),
            0.25,
            "no smaller than a quarter"
        );
        assert_eq!(
            LiveResizer::new(LiveResize::Soft { scale: 3.0 }).fraction(),
            0.9,
            "and still stretched"
        );
    }

    #[test]
    fn an_adaptive_window_follows_its_own_budget() {
        let mut quick = LiveResizer::new(LiveResize::Adaptive { budget_ms: 100.0 });
        quick.record(1.0, 80.0);
        assert_eq!(quick.fraction(), 1.0, "80 ms fits 100");
        let mut strict = LiveResizer::new(LiveResize::Adaptive { budget_ms: 10.0 });
        strict.record(1.0, 80.0);
        assert_eq!(strict.fraction(), 0.75, "80 ms does not fit 10");
        assert_eq!(
            LiveResize::default(),
            LiveResize::Adaptive {
                budget_ms: DEFAULT_BUDGET_MS
            }
        );
    }

    #[test]
    fn what_a_size_costs_follows_the_frames_as_they_change() {
        let mut quality = FrameQuality::default();
        quality.record(1.0, 100.0);
        for _ in 0..8 {
            quality.record(1.0, 10.0);
        }
        assert_eq!(quality.pick(), 1.0);
    }
}
