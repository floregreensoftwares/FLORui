//! Why the event loop ran and how many frames it drew, counted since the last
//! time they were read. An app that looks idle and still uses the processor is
//! either woken by something (a timer, a signal written on a schedule) or
//! redrawing for an animation that never ends; these counts say which.
//!
//! Counting is a relaxed atomic add, and only in a build that can measure
//! ([`crate::ENABLED`]). Nothing here records what the application did, only
//! how often the loop ran for each kind of reason.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// What woke the event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wake {
    /// Pointer, keyboard, text input or focus.
    Input,
    /// Another event of the window: a resize, a move, a theme change.
    Window,
    /// A signal was written or a resource became ready.
    Signal,
    /// An animation or transition is running and asked for its next frame.
    Animation,
    /// Smooth scrolling or a held scroll button asked for its next step.
    Scroll,
    /// A timer of the host: a held spinner button, the end of a resize, the
    /// debounced save of the window's bounds.
    Timer,
    /// The stylesheet file changed.
    Stylesheet,
    /// The system or another thread: tray, dialogs, accessibility, activation,
    /// a download that finished.
    System,
}

const COUNT: usize = 8;

impl Wake {
    pub const ALL: [Wake; COUNT] = [
        Wake::Input,
        Wake::Window,
        Wake::Signal,
        Wake::Animation,
        Wake::Scroll,
        Wake::Timer,
        Wake::Stylesheet,
        Wake::System,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Wake::Input => "input",
            Wake::Window => "window",
            Wake::Signal => "signal",
            Wake::Animation => "animation",
            Wake::Scroll => "scroll",
            Wake::Timer => "timer",
            Wake::Stylesheet => "stylesheet",
            Wake::System => "system",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

static WAKES: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
static FRAMES: AtomicU64 = AtomicU64::new(0);
static FRAME_NANOS: AtomicU64 = AtomicU64::new(0);
static LAST_TAKEN: Mutex<Option<Instant>> = Mutex::new(None);

/// The loop ran because of `reason`.
pub fn note(reason: Wake) {
    if crate::ENABLED {
        WAKES[reason.index()].fetch_add(1, Ordering::Relaxed);
    }
}

/// A frame was drawn and presented, `took` from the redraw request being
/// handled until the present call returned.
pub fn note_frame(took: Duration) {
    if crate::ENABLED {
        FRAMES.fetch_add(1, Ordering::Relaxed);
        FRAME_NANOS.fetch_add(
            u64::try_from(took.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }
}

/// What happened since the last [`take`] (or since the first one, which starts
/// the count and reports nothing before it).
#[derive(Debug, Clone, PartialEq)]
pub struct Activity {
    /// How long the counts cover.
    pub span: Duration,
    /// Times the loop ran per reason, in the order of [`Wake::ALL`].
    pub wakes: Vec<(Wake, u64)>,
    pub frames: u64,
    /// The summed time of the frames.
    pub frame_time: Duration,
}

impl Activity {
    pub fn total_wakes(&self) -> u64 {
        self.wakes.iter().map(|(_, count)| count).sum()
    }

    pub fn frames_per_second(&self) -> f64 {
        self.frames as f64 / self.span.as_secs_f64().max(f64::EPSILON)
    }

    /// The share of the span spent drawing frames, 0 to 1 (above 1 only if
    /// frames overlapped, which one window does not do).
    pub fn busy_share(&self) -> f64 {
        self.frame_time.as_secs_f64() / self.span.as_secs_f64().max(f64::EPSILON)
    }
}

/// Reads the counts and starts them over. `None` the first time, which only
/// marks where counting begins, and in a build that cannot measure.
pub fn take() -> Option<Activity> {
    if !crate::ENABLED {
        return None;
    }
    let now = Instant::now();
    let mut last = LAST_TAKEN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let since = last.replace(now);
    let wakes: Vec<(Wake, u64)> = Wake::ALL
        .iter()
        .map(|&reason| (reason, WAKES[reason.index()].swap(0, Ordering::Relaxed)))
        .collect();
    let frames = FRAMES.swap(0, Ordering::Relaxed);
    let frame_time = Duration::from_nanos(FRAME_NANOS.swap(0, Ordering::Relaxed));
    let since = since?;
    Some(Activity {
        span: now.duration_since(since),
        wakes,
        frames,
        frame_time,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_read_once_and_start_over() {
        if !crate::ENABLED {
            return;
        }
        let _ = take();
        note(Wake::Animation);
        note(Wake::Animation);
        note(Wake::Input);
        note_frame(Duration::from_millis(3));
        let activity = take().expect("counting had begun");
        let count = |reason| {
            activity
                .wakes
                .iter()
                .find(|(r, _)| *r == reason)
                .map(|(_, n)| *n)
        };
        assert!(count(Wake::Animation) >= Some(2));
        assert!(activity.frames >= 1);
        assert!(activity.frame_time >= Duration::from_millis(3));
        let again = take().unwrap();
        assert_eq!(again.frames, 0, "read once, then counted from zero");
    }
}
