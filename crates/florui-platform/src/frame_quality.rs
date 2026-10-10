//! How sharp a frame is painted while the window is being dragged to a new
//! size. Every size is a frame nobody keeps, so what is wanted is a frame soon:
//! when full-size frames cost too much they are painted at a fraction of the
//! window's pixels and stretched, and the frame at full size follows once the
//! drag has stopped.

use std::time::Duration;

/// How long after the last size change the window is still taken to be
/// resizing.
pub(crate) const SETTLE: Duration = Duration::from_millis(140);

/// What a frame painted while resizing should cost, in milliseconds.
const TARGET_MS: f32 = 34.0;

/// The fractions of the window's size a frame may be painted at, largest first.
const STEPS: [f32; 3] = [1.0, 0.75, 0.5];

/// Every this many frames the sizes above the one in use are measured again: a
/// frame is cheaper on a window that has stopped changing under it, and what a
/// small frame costs says little about what a big one does (much of it does not
/// follow the area).
const REPROBE_EVERY: u32 = 12;

/// What painting a frame has cost at each size, as measured.
#[derive(Default)]
pub(crate) struct FrameQuality {
    cost_ms: [Option<f32>; STEPS.len()],
    frames: u32,
}

impl FrameQuality {
    /// The fraction of the window's size to paint the next frame at: the largest
    /// whose cost is not known to be over the budget. A size not yet measured is
    /// tried, so the first frame of a drag is at full size and finds out.
    pub(crate) fn pick(&mut self) -> f32 {
        self.frames += 1;
        let choose = |cost_ms: &[Option<f32>; STEPS.len()]| {
            STEPS
                .iter()
                .zip(cost_ms)
                .find(|(_, cost)| cost.is_none_or(|cost| cost <= TARGET_MS))
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
    fn what_a_size_costs_follows_the_frames_as_they_change() {
        let mut quality = FrameQuality::default();
        quality.record(1.0, 100.0);
        for _ in 0..8 {
            quality.record(1.0, 10.0);
        }
        assert_eq!(quality.pick(), 1.0);
    }
}
