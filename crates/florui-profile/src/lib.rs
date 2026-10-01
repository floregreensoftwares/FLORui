//! Phase timing for the frame pipeline. Code that does a stage of the work
//! holds a [`span`] for its duration; [`finish_frame`] closes everything
//! measured since the last frame into a [`FrameProfile`] that stays in a
//! bounded record of recent frames.
//!
//! Nothing is measured unless [`start`] was called, and in a release build
//! nothing can be measured without the `profiling` feature ([`ENABLED`]); a
//! span then does not exist at all. Idle in a build that has it, a span costs
//! one thread-local flag read.
//!
//! A frame's summary (calls and total time per phase, wall time, causes) is
//! always kept. The individual spans, with their nesting, are kept only when
//! [`start`] asked for detail, and are capped per frame. Nothing here records a
//! value from the application, only phase names, durations and counts.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Whether this build can measure.
pub const ENABLED: bool = cfg!(debug_assertions) || cfg!(feature = "profiling");

/// How many finished frames are kept, oldest dropped first.
pub const RETAINED_FRAMES: usize = 240;
/// Spans kept per frame in detail mode; later ones are counted, not kept.
pub const MAX_SPANS_PER_FRAME: usize = 4096;

/// A stage of the work. `Update` and `Present` contain the stages listed
/// with them; every phase's total is inclusive of anything nested in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    /// One re-render of the tree against the viewport: everything below until
    /// `Observers`.
    Update,
    /// Component render and its effects.
    Render,
    /// Async work that progressed before the tree committed.
    Async,
    /// Building the arena from the element tree.
    ArenaBuild,
    /// Image, icon, hover and focus registries catching up with the tree.
    Sync,
    /// The style cascade. A container query can run it more than once.
    Cascade,
    /// Box layout, text measuring included.
    Layout,
    /// Corrections applied after layout (select widths, image sizes, overlays).
    PostLayout,
    /// Size, position, focus, scroll and text-input registries after layout.
    Observers,
    /// Turning the laid-out tree into paint inputs (scroll offsets, scaling).
    PaintParts,
    /// The accessibility tree for the frame.
    Accessibility,
    /// CPU rasterization into the frame buffer.
    Raster,
    /// What an observer draws or reads after the frame is painted.
    Overlay,
    /// Handing the finished buffer to the window: the stages below on the GPU
    /// path, or the copy to the window's own buffer otherwise.
    Present,
    /// Copying the buffer into a GPU texture.
    Upload,
    /// Waiting for the window's next surface texture.
    Acquire,
    /// Recording and submitting the copy to the surface.
    Submit,
    /// Presenting the surface.
    Flip,
}

impl Phase {
    pub const ALL: [Phase; 18] = [
        Phase::Update,
        Phase::Render,
        Phase::Async,
        Phase::ArenaBuild,
        Phase::Sync,
        Phase::Cascade,
        Phase::Layout,
        Phase::PostLayout,
        Phase::Observers,
        Phase::PaintParts,
        Phase::Accessibility,
        Phase::Raster,
        Phase::Overlay,
        Phase::Present,
        Phase::Upload,
        Phase::Acquire,
        Phase::Submit,
        Phase::Flip,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Phase::Update => "update",
            Phase::Render => "render",
            Phase::Async => "async",
            Phase::ArenaBuild => "arena",
            Phase::Sync => "sync",
            Phase::Cascade => "cascade",
            Phase::Layout => "layout",
            Phase::PostLayout => "post-layout",
            Phase::Observers => "observers",
            Phase::PaintParts => "paint-parts",
            Phase::Accessibility => "accessibility",
            Phase::Raster => "raster",
            Phase::Overlay => "overlay",
            Phase::Present => "present",
            Phase::Upload => "upload",
            Phase::Acquire => "acquire",
            Phase::Submit => "submit",
            Phase::Flip => "flip",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Why a frame happened, one cause per recorded trace since the previous
/// frame: a signal write, an event, a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cause {
    pub at: Duration,
    pub component: Option<&'static str>,
    /// `signal-write`, `event`, `resource-started` or `resource-completed`.
    pub kind: &'static str,
    /// The event name and the arena index of its element, when an event
    /// caused or contained this.
    pub event: Option<(&'static str, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseTotal {
    pub phase: Phase,
    pub calls: u32,
    pub total: Duration,
}

/// One measured stretch, in detail mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanRecord {
    pub phase: Phase,
    pub start: Duration,
    pub end: Duration,
    /// How many spans were open around it.
    pub depth: u16,
    /// Index into the same frame's spans of the span that contained it.
    pub parent: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameProfile {
    /// Counts up from the first frame since [`start`].
    pub index: u64,
    /// First span's start and last span's end, on the clock [`now`] reads.
    pub start: Duration,
    pub end: Duration,
    /// Every phase that ran, in the order of [`Phase::ALL`].
    pub phases: Vec<PhaseTotal>,
    pub causes: Vec<Cause>,
    /// Empty unless [`start`] asked for detail.
    pub spans: Vec<SpanRecord>,
    /// Spans beyond [`MAX_SPANS_PER_FRAME`] that were timed but not kept.
    pub dropped_spans: u32,
}

impl FrameProfile {
    pub fn wall(&self) -> Duration {
        self.end.saturating_sub(self.start)
    }

    /// Calls and total time of `phase`, if it ran.
    pub fn phase(&self, phase: Phase) -> Option<&PhaseTotal> {
        self.phases.iter().find(|p| p.phase == phase)
    }

    pub fn total(&self, phase: Phase) -> Duration {
        self.phase(phase).map_or(Duration::ZERO, |p| p.total)
    }
}

/// The time since this process first asked, on a monotonic clock. Everything
/// here and every update trace is stamped with it, so they line up.
pub fn now() -> Duration {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed()
}

#[derive(Default)]
struct Recorder {
    detail: bool,
    started_at: Option<Duration>,
    frame_start: Option<Duration>,
    frame_end: Duration,
    totals: [(u32, Duration); Phase::ALL.len()],
    spans: Vec<SpanRecord>,
    open: Vec<u32>,
    dropped: u32,
    frames: VecDeque<FrameProfile>,
    next_index: u64,
}

thread_local! {
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static RECORDER: RefCell<Recorder> = RefCell::new(Recorder::default());
}

/// Begins measuring on this thread. With `detail`, each span is kept with its
/// nesting as well as summed. Discards what an earlier run recorded.
pub fn start(detail: bool) {
    if !ENABLED {
        return;
    }
    RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        *r = Recorder::default();
        r.detail = detail;
        r.started_at = Some(now());
    });
    RECORDING.with(|f| f.set(true));
}

/// Stops measuring. Frames already finished stay readable.
pub fn stop() {
    RECORDING.with(|f| f.set(false));
}

/// When [`start`] was last called on this thread, on the clock [`now`] reads.
pub fn started_at() -> Option<Duration> {
    RECORDER.with(|r| r.borrow().started_at)
}

pub fn is_recording() -> bool {
    ENABLED && RECORDING.with(Cell::get)
}

/// The finished frames kept, oldest first.
pub fn frames() -> Vec<FrameProfile> {
    RECORDER.with(|r| r.borrow().frames.iter().cloned().collect())
}

/// The most recent finished frame.
pub fn last_frame() -> Option<FrameProfile> {
    RECORDER.with(|r| r.borrow().frames.back().cloned())
}

/// Forgets finished frames and anything measured since the last one.
pub fn clear() {
    RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        let (detail, started_at, next_index) = (r.detail, r.started_at, r.next_index);
        *r = Recorder::default();
        r.detail = detail;
        r.started_at = started_at;
        r.next_index = next_index;
    });
}

/// Measures a stretch of work until dropped.
#[must_use = "a span measures until it is dropped; bind it with `let _span = ...`"]
pub struct Span {
    phase: Phase,
    start: Option<Duration>,
    slot: Option<u32>,
}

/// Starts measuring `phase`. Inert when nothing is recording.
pub fn span(phase: Phase) -> Span {
    if !ENABLED || !RECORDING.with(Cell::get) {
        return Span {
            phase,
            start: None,
            slot: None,
        };
    }
    let start = now();
    let slot = RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        r.frame_start.get_or_insert(start);
        if !r.detail {
            return None;
        }
        if r.spans.len() >= MAX_SPANS_PER_FRAME {
            r.dropped += 1;
            return None;
        }
        let slot = r.spans.len() as u32;
        let record = SpanRecord {
            phase,
            start,
            end: start,
            depth: r.open.len() as u16,
            parent: r.open.last().copied(),
        };
        r.spans.push(record);
        r.open.push(slot);
        Some(slot)
    });
    Span {
        phase,
        start: Some(start),
        slot,
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        let end = now();
        RECORDER.with(|r| {
            let mut r = r.borrow_mut();
            let entry = &mut r.totals[self.phase.index()];
            entry.0 += 1;
            entry.1 += end.saturating_sub(start);
            r.frame_end = r.frame_end.max(end);
            if let Some(slot) = self.slot {
                r.spans[slot as usize].end = end;
                r.open.pop();
            }
        });
    }
}

/// Closes everything measured since the previous frame into a
/// [`FrameProfile`] carrying `causes`, and keeps it. Does nothing when
/// nothing was measured, so a host can call it after every frame.
pub fn finish_frame(causes: Vec<Cause>) {
    if !is_recording() {
        return;
    }
    RECORDER.with(|r| {
        let mut r = r.borrow_mut();
        let Some(start) = r.frame_start.take() else {
            return;
        };
        let phases = Phase::ALL
            .iter()
            .filter(|p| r.totals[p.index()].0 > 0)
            .map(|&phase| PhaseTotal {
                phase,
                calls: r.totals[phase.index()].0,
                total: r.totals[phase.index()].1,
            })
            .collect();
        let frame = FrameProfile {
            index: r.next_index,
            start,
            end: r.frame_end,
            phases,
            causes,
            spans: std::mem::take(&mut r.spans),
            dropped_spans: std::mem::take(&mut r.dropped),
        };
        r.next_index += 1;
        r.totals = Default::default();
        r.open.clear();
        r.frame_end = Duration::ZERO;
        if r.frames.len() == RETAINED_FRAMES {
            r.frames.pop_front();
        }
        r.frames.push_back(frame);
    });
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

    use super::*;

    fn busy(ms: u64) {
        sleep(Duration::from_millis(ms));
    }

    #[test]
    fn nothing_is_measured_until_started() {
        stop();
        clear();
        {
            let _span = span(Phase::Layout);
            busy(1);
        }
        finish_frame(Vec::new());
        assert!(frames().is_empty());
    }

    #[test]
    fn a_frame_sums_calls_and_time_per_phase() {
        start(false);
        for _ in 0..2 {
            let _span = span(Phase::Cascade);
            busy(2);
        }
        {
            let _span = span(Phase::Layout);
            busy(3);
        }
        finish_frame(Vec::new());
        let frame = last_frame().unwrap();
        stop();

        let cascade = frame.phase(Phase::Cascade).unwrap();
        assert_eq!(cascade.calls, 2);
        assert!(cascade.total >= Duration::from_millis(4));
        assert_eq!(frame.phase(Phase::Layout).unwrap().calls, 1);
        assert!(
            frame.phase(Phase::Raster).is_none(),
            "a phase that did not run is absent"
        );
        assert!(frame.spans.is_empty(), "no detail was asked for");
        assert!(frame.wall() >= cascade.total + frame.total(Phase::Layout));
    }

    #[test]
    fn detail_keeps_spans_nested_inside_their_parents() {
        start(true);
        {
            let _update = span(Phase::Update);
            {
                let _render = span(Phase::Render);
                busy(1);
            }
            {
                let _layout = span(Phase::Layout);
                busy(1);
            }
        }
        finish_frame(Vec::new());
        let frame = last_frame().unwrap();
        stop();

        assert_eq!(frame.spans.len(), 3);
        let update = frame.spans[0];
        assert_eq!(
            (update.phase, update.depth, update.parent),
            (Phase::Update, 0, None)
        );
        for child in &frame.spans[1..] {
            assert_eq!(child.parent, Some(0));
            assert_eq!(child.depth, 1);
            assert!(update.start <= child.start && child.end <= update.end);
        }
        assert!(
            frame.spans[1].end <= frame.spans[2].start,
            "siblings do not overlap"
        );
        assert!(
            frame.total(Phase::Update) >= frame.total(Phase::Render) + frame.total(Phase::Layout)
        );
    }

    #[test]
    fn finishing_a_frame_starts_the_next_from_nothing() {
        start(false);
        {
            let _span = span(Phase::Raster);
        }
        finish_frame(Vec::new());
        {
            let _span = span(Phase::Flip);
        }
        finish_frame(Vec::new());
        let frames = frames();
        stop();
        assert_eq!(frames.len(), 2);
        assert!(frames[1].phase(Phase::Raster).is_none());
        assert_eq!(frames[1].index, frames[0].index + 1);
    }

    #[test]
    fn a_frame_with_nothing_measured_is_not_recorded() {
        start(false);
        finish_frame(Vec::new());
        let count = frames().len();
        stop();
        assert_eq!(count, 0);
    }

    #[test]
    fn the_record_of_frames_is_bounded() {
        start(false);
        for _ in 0..(RETAINED_FRAMES + 5) {
            {
                let _span = span(Phase::Raster);
            }
            finish_frame(Vec::new());
        }
        let frames = frames();
        stop();
        assert_eq!(frames.len(), RETAINED_FRAMES);
        assert_eq!(frames[0].index, 5, "the oldest were dropped");
    }

    #[test]
    fn spans_past_the_cap_are_counted_but_still_timed() {
        start(true);
        for _ in 0..(MAX_SPANS_PER_FRAME + 10) {
            let _span = span(Phase::Layout);
        }
        finish_frame(Vec::new());
        let frame = last_frame().unwrap();
        stop();
        assert_eq!(frame.spans.len(), MAX_SPANS_PER_FRAME);
        assert_eq!(frame.dropped_spans, 10);
        assert_eq!(
            frame.phase(Phase::Layout).unwrap().calls as usize,
            MAX_SPANS_PER_FRAME + 10
        );
    }

    #[test]
    fn causes_travel_with_the_frame() {
        start(false);
        {
            let _span = span(Phase::Render);
        }
        let cause = Cause {
            at: now(),
            component: Some("Counter"),
            kind: "event",
            event: Some(("click", 3)),
        };
        finish_frame(vec![cause]);
        let frame = last_frame().unwrap();
        stop();
        assert_eq!(frame.causes, [cause]);
    }

    #[test]
    fn start_discards_an_earlier_run() {
        start(false);
        {
            let _span = span(Phase::Raster);
        }
        finish_frame(Vec::new());
        start(false);
        let count = frames().len();
        stop();
        assert_eq!(count, 0);
    }
}
