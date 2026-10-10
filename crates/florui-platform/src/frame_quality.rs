//! How sharp a frame is painted while the window is being dragged to a new
//! size. Every size is a frame nobody keeps, so what is wanted is a frame soon:
//! it is painted at a fraction of the window's pixels and stretched, and the
//! frame at full size follows once the drag has stopped.

use std::time::Duration;

/// How long after the last size change the window is still taken to be
/// resizing.
pub(crate) const SETTLE: Duration = Duration::from_millis(140);

/// What a frame painted while resizing should cost, in milliseconds.
const TARGET_MS: f32 = 34.0;

/// The fractions of the window's size a frame may be painted at, largest first.
const STEPS: [f32; 3] = [1.0, 0.75, 0.5];

/// What painting a frame costs at full size, learnt from the frames painted so
/// far.
#[derive(Default)]
pub(crate) struct FrameQuality {
    full_size_ms: Option<f32>,
}

impl FrameQuality {
    /// The fraction of the window's size to paint the next frame at. A cost
    /// still unknown starts at a half, so the first frame of a drag is not the
    /// slow one.
    pub(crate) fn pick(&self) -> f32 {
        let Some(cost) = self.full_size_ms else {
            return 0.5;
        };
        // The cost follows the number of pixels, the square of the fraction.
        let wanted = (TARGET_MS / cost.max(0.1)).sqrt();
        STEPS
            .into_iter()
            .find(|&step| step <= wanted)
            .unwrap_or(STEPS[STEPS.len() - 1])
    }

    /// Learns from a frame painted at `fraction` of the window's size that took
    /// `painted_ms`.
    pub(crate) fn record(&mut self, fraction: f32, painted_ms: f32) {
        let full = painted_ms / (fraction * fraction).max(0.01);
        self.full_size_ms = Some(match self.full_size_ms {
            None => full,
            Some(before) => 0.5 * before + 0.5 * full,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_frame_of_a_drag_is_painted_small() {
        assert_eq!(FrameQuality::default().pick(), 0.5);
    }

    #[test]
    fn a_cheap_window_is_painted_at_full_size_and_a_dear_one_smaller() {
        let mut quality = FrameQuality::default();
        quality.record(1.0, 8.0);
        assert_eq!(
            quality.pick(),
            1.0,
            "8 ms at full size is within the budget"
        );
        let mut quality = FrameQuality::default();
        quality.record(1.0, 100.0);
        assert_eq!(
            quality.pick(),
            0.5,
            "100 ms wants less than half a side, and half is the least"
        );
        let mut quality = FrameQuality::default();
        quality.record(1.0, 50.0);
        assert_eq!(
            quality.pick(),
            0.75,
            "50 ms wants about four fifths of a side"
        );
    }

    #[test]
    fn a_small_frame_says_what_a_full_one_would_cost() {
        let mut quality = FrameQuality::default();
        // A half-size frame is a quarter of the pixels: 12 ms there is 48 here.
        quality.record(0.5, 12.0);
        assert_eq!(quality.pick(), 0.75);
        quality.record(0.5, 12.0);
        assert_eq!(quality.pick(), 0.75);
    }

    #[test]
    fn the_estimate_follows_the_frames_as_they_change() {
        let mut quality = FrameQuality::default();
        quality.record(1.0, 100.0);
        for _ in 0..8 {
            quality.record(1.0, 10.0);
        }
        assert_eq!(quality.pick(), 1.0);
    }
}
