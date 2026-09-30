//! Smooth-scroll curves: a wheel notch and a programmatic `scrollTo` animate
//! differently. Curve shapes and durations are fitted to offsets measured in
//! Edge (a real OS wheel notch, and `scrollTo({behavior: 'smooth'})`), not
//! copied from Chromium's source. Both durations grow with the square root
//! of the distance. A retarget mid-animation keeps the current velocity, so
//! a burst of notches accelerates instead of restarting from rest.

/// Time is seconds on any monotonic clock the caller picks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Bezier {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl Bezier {
    fn axis(a: f64, b: f64, t: f64) -> f64 {
        let u = 1.0 - t;
        3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
    }

    /// Progress (0 to 1) after the fraction `x` of the duration.
    fn ease(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..40 {
            let mid = (low + high) / 2.0;
            if Self::axis(self.x1, self.x2, mid) < x {
                low = mid;
            } else {
                high = mid;
            }
        }
        Self::axis(self.y1, self.y2, (low + high) / 2.0)
    }

    /// The same curve starting with normalized slope `slope` (progress per
    /// unit of duration), so a retarget continues the current speed.
    fn with_initial_slope(self, slope: f64) -> Self {
        Self {
            y1: (slope * self.x1).clamp(0.0, 1.0),
            ..self
        }
    }
}

/// Which measured behavior an animation reproduces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ScrollKind {
    /// A mouse-wheel notch: ease-in-out, 100 px in about 180 ms.
    Wheel,
    /// `scrollTo` with smooth behavior: fast start, long tail.
    Programmatic,
}

impl ScrollKind {
    fn curve(self) -> Bezier {
        match self {
            Self::Wheel => Bezier {
                x1: 0.45,
                y1: 0.15,
                x2: 0.60,
                y2: 0.95,
            },
            Self::Programmatic => Bezier {
                x1: 0.40,
                y1: 0.10,
                x2: 0.00,
                y2: 1.00,
            },
        }
    }

    fn duration(self, distance: f64) -> f64 {
        match self {
            Self::Wheel => distance.sqrt() * 0.018,
            Self::Programmatic => distance.sqrt() / 60.0,
        }
    }
}

fn length(v: (f64, f64)) -> f64 {
    v.0.hypot(v.1)
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ScrollAnimation {
    from: (f32, f32),
    to: (f32, f32),
    start: f64,
    duration: f64,
    curve: Bezier,
    kind: ScrollKind,
}

impl ScrollAnimation {
    pub(crate) fn new(from: (f32, f32), to: (f32, f32), now: f64, kind: ScrollKind) -> Self {
        let distance = length((f64::from(to.0 - from.0), f64::from(to.1 - from.1)));
        Self {
            from,
            to,
            start: now,
            duration: kind.duration(distance),
            curve: kind.curve(),
            kind,
        }
    }

    pub(crate) fn target(&self) -> (f32, f32) {
        self.to
    }

    pub(crate) fn is_done(&self, now: f64) -> bool {
        now >= self.start + self.duration
    }

    pub(crate) fn offset_at(&self, now: f64) -> (f32, f32) {
        if self.duration <= 0.0 || self.is_done(now) {
            return self.to;
        }
        let progress = self.curve.ease((now - self.start) / self.duration) as f32;
        (
            self.from.0 + (self.to.0 - self.from.0) * progress,
            self.from.1 + (self.to.1 - self.from.1) * progress,
        )
    }

    /// Moves the target to `to`, continuing from where the animation is now
    /// at its current velocity. The new segment's duration comes from how far
    /// the target moved, not from the distance still to cover.
    pub(crate) fn retarget(&mut self, to: (f32, f32), now: f64) {
        const SAMPLE: f64 = 0.001;
        let here = self.offset_at(now);
        let ahead = self.offset_at(now + SAMPLE);
        let velocity = (
            f64::from(ahead.0 - here.0) / SAMPLE,
            f64::from(ahead.1 - here.1) / SAMPLE,
        );
        let remaining = (f64::from(to.0 - here.0), f64::from(to.1 - here.1));
        let moved = length((f64::from(to.0 - self.to.0), f64::from(to.1 - self.to.1)));
        let duration = self.kind.duration(moved);
        let remaining_sq = remaining.0 * remaining.0 + remaining.1 * remaining.1;
        let slope = if remaining_sq > 0.0 {
            (velocity.0 * remaining.0 + velocity.1 * remaining.1) / remaining_sq * duration
        } else {
            0.0
        };
        *self = Self {
            from: here,
            to,
            start: now,
            duration,
            curve: self.kind.curve().with_initial_slope(slope),
            kind: self.kind,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offsets measured in Edge for one real wheel notch (100 px): seconds
    /// since the animation started, and scrollTop.
    const MEASURED_NOTCH: [(f64, f32); 10] = [
        (0.0130, 2.0),
        (0.0306, 8.0),
        (0.0468, 16.0),
        (0.0638, 28.0),
        (0.0804, 41.0),
        (0.0975, 55.0),
        (0.1132, 69.0),
        (0.1311, 81.0),
        (0.1478, 90.0),
        (0.1644, 97.0),
    ];

    #[test]
    fn a_curve_runs_from_zero_to_one() {
        for kind in [ScrollKind::Wheel, ScrollKind::Programmatic] {
            let curve = kind.curve();
            assert!(curve.ease(0.0).abs() < 1e-9);
            assert!((curve.ease(1.0) - 1.0).abs() < 1e-9);
            let mut last = 0.0;
            for i in 0..=100 {
                let value = curve.ease(f64::from(i) / 100.0);
                assert!(value >= last - 1e-9, "monotonic");
                last = value;
            }
        }
    }

    #[test]
    fn one_wheel_notch_follows_the_measured_edge_curve() {
        let animation = ScrollAnimation::new((0.0, 0.0), (0.0, 100.0), 0.0, ScrollKind::Wheel);
        for (seconds, measured) in MEASURED_NOTCH {
            let got = animation.offset_at(seconds).1;
            assert!(
                (got - measured).abs() < 3.0,
                "at {seconds}s: got {got}, measured {measured}"
            );
        }
        assert!((animation.duration - 0.18).abs() < 1e-9);
        assert_eq!(animation.offset_at(1.0), (0.0, 100.0));
    }

    #[test]
    fn a_longer_programmatic_scroll_takes_longer() {
        let short = ScrollAnimation::new((0.0, 0.0), (0.0, 200.0), 0.0, ScrollKind::Programmatic);
        let long = ScrollAnimation::new((0.0, 0.0), (0.0, 4000.0), 0.0, ScrollKind::Programmatic);
        assert!(short.duration < 0.25 && long.duration > 1.0);
        // Measured: 4000 px reached 2930 px by 0.387 s.
        let long_at = long.offset_at(0.387).1;
        assert!((long_at - 2930.0).abs() < 150.0, "{long_at}");
        // Measured: 800 px reached 632 px by 0.193 s.
        let mid = ScrollAnimation::new((0.0, 0.0), (0.0, 800.0), 0.0, ScrollKind::Programmatic);
        assert!((mid.offset_at(0.193).1 - 632.0).abs() < 50.0);
    }

    #[test]
    fn a_burst_of_notches_accelerates_and_lands_on_the_summed_target() {
        // Five notches about 47 ms apart, as sent in the measurement.
        let times = [0.0, 0.047, 0.094, 0.141, 0.188];
        let mut animation =
            ScrollAnimation::new((0.0, 0.0), (0.0, 100.0), times[0], ScrollKind::Wheel);
        for (index, &time) in times.iter().enumerate().skip(1) {
            animation.retarget((0.0, 100.0 * (index + 1) as f32), time);
        }
        assert_eq!(animation.target(), (0.0, 500.0));
        // Measured in Edge: 264 px (of 500) by 0.215 s, settled by 0.365 s.
        let mid = animation.offset_at(0.215).1;
        assert!((150.0..=320.0).contains(&mid), "mid-burst offset {mid}");
        let end = animation.start + animation.duration;
        assert!((0.30..=0.45).contains(&end), "burst ends at {end}s");
        assert_eq!(animation.offset_at(end + 0.01), (0.0, 500.0));
    }

    #[test]
    fn reversing_direction_starts_from_rest() {
        let mut animation = ScrollAnimation::new((0.0, 0.0), (0.0, 100.0), 0.0, ScrollKind::Wheel);
        animation.retarget((0.0, 0.0), 0.05);
        let here = animation.offset_at(0.05).1;
        let soon = animation.offset_at(0.06).1;
        assert!(
            soon <= here + 0.01,
            "no further forward push: {here} then {soon}"
        );
        assert_eq!(animation.offset_at(10.0), (0.0, 0.0));
    }

    #[test]
    fn a_retarget_to_where_it_already_is_finishes_at_once() {
        let mut animation = ScrollAnimation::new((0.0, 0.0), (0.0, 100.0), 0.0, ScrollKind::Wheel);
        assert_eq!(animation.offset_at(0.2), (0.0, 100.0));
        animation.retarget((0.0, 100.0), 0.2);
        assert!(animation.is_done(0.2));
        assert_eq!(animation.offset_at(0.2), (0.0, 100.0));
    }
}
