//! Summary statistics for timing samples, and a comparison of two sample sets
//! that says whether a difference is larger than the noise in either.

use serde::{Deserialize, Serialize};

/// Tail percentiles from a handful of samples are not stable, so they are only
/// reported above these counts.
const MIN_SAMPLES_P95: usize = 20;
const MIN_SAMPLES_P99: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub samples: usize,
    pub min: f64,
    pub median: f64,
    pub mean: f64,
    pub p95: Option<f64>,
    pub p99: Option<f64>,
    /// Median absolute deviation: how far a typical sample sits from the
    /// median, without one slow outlier dominating it.
    pub mad: f64,
}

impl Summary {
    /// The spread relative to the median, as a fraction.
    pub fn relative_mad(&self) -> f64 {
        if self.median == 0.0 {
            0.0
        } else {
            self.mad / self.median
        }
    }
}

pub fn summarize(samples: &[f64]) -> Option<Summary> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = percentile(&sorted, 50.0);
    let mut deviations: Vec<f64> = sorted.iter().map(|s| (s - median).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    Some(Summary {
        samples: sorted.len(),
        min: sorted[0],
        median,
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
        p95: (sorted.len() >= MIN_SAMPLES_P95).then(|| percentile(&sorted, 95.0)),
        p99: (sorted.len() >= MIN_SAMPLES_P99).then(|| percentile(&sorted, 99.0)),
        mad: percentile(&deviations, 50.0),
    })
}

/// Linear interpolation between the two nearest ranks; `sorted` is ascending.
pub fn percentile(sorted: &[f64], percent: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = percent / 100.0 * (sorted.len() - 1) as f64;
    let low = rank.floor() as usize;
    let high = rank.ceil() as usize;
    sorted[low] + (sorted[high] - sorted[low]) * (rank - low as f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Faster,
    Slower,
    /// The median moved, but not by more than the threshold, or not by more
    /// than the noise in the samples.
    NoDifference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comparison {
    pub baseline_median: f64,
    pub candidate_median: f64,
    /// `(candidate - baseline) / baseline`; negative is faster.
    pub relative_change: f64,
    /// 95% bootstrap interval of `relative_change`.
    pub interval: (f64, f64),
    pub verdict: Verdict,
}

/// A difference counts only when it exceeds `threshold` (a fraction, 0.05 for
/// 5%) and its whole 95% interval lies on one side of zero. The interval comes
/// from resampling both sets, so it widens with the noise in either.
pub fn compare(baseline: &[f64], candidate: &[f64], threshold: f64) -> Option<Comparison> {
    let base = summarize(baseline)?;
    let cand = summarize(candidate)?;
    if base.median == 0.0 {
        return None;
    }
    let relative_change = (cand.median - base.median) / base.median;
    let interval = bootstrap_interval(baseline, candidate);
    let verdict = if relative_change.abs() > threshold && (interval.0 > 0.0 || interval.1 < 0.0) {
        if relative_change < 0.0 {
            Verdict::Faster
        } else {
            Verdict::Slower
        }
    } else {
        Verdict::NoDifference
    };
    Some(Comparison {
        baseline_median: base.median,
        candidate_median: cand.median,
        relative_change,
        interval,
        verdict,
    })
}

const RESAMPLES: usize = 2000;

fn bootstrap_interval(baseline: &[f64], candidate: &[f64]) -> (f64, f64) {
    // A fixed seed keeps a report reproducible from the same samples.
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut changes = Vec::with_capacity(RESAMPLES);
    for _ in 0..RESAMPLES {
        let base = resampled_median(baseline, &mut rng);
        let cand = resampled_median(candidate, &mut rng);
        if base != 0.0 {
            changes.push((cand - base) / base);
        }
    }
    changes.sort_by(f64::total_cmp);
    if changes.is_empty() {
        return (0.0, 0.0);
    }
    (percentile(&changes, 2.5), percentile(&changes, 97.5))
}

fn resampled_median(samples: &[f64], rng: &mut u64) -> f64 {
    let mut picked: Vec<f64> = (0..samples.len())
        .map(|_| samples[(next(rng) % samples.len() as u64) as usize])
        .collect();
    picked.sort_by(f64::total_cmp);
    percentile(&picked, 50.0)
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_reports_median_and_spread() {
        let s = summarize(&[1.0, 2.0, 3.0, 4.0, 100.0]).unwrap();
        assert_eq!(s.median, 3.0);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.mad, 1.0);
        assert_eq!(s.p95, None, "too few samples for a tail percentile");
    }

    #[test]
    fn tail_percentiles_need_enough_samples() {
        let twenty: Vec<f64> = (0..20).map(f64::from).collect();
        let hundred: Vec<f64> = (0..100).map(f64::from).collect();
        assert!(summarize(&twenty).unwrap().p95.is_some());
        assert!(summarize(&twenty).unwrap().p99.is_none());
        assert!(summarize(&hundred).unwrap().p99.is_some());
    }

    #[test]
    fn percentiles_interpolate() {
        assert_eq!(percentile(&[0.0, 10.0], 50.0), 5.0);
        assert_eq!(percentile(&[7.0], 99.0), 7.0);
    }

    #[test]
    fn an_empty_set_has_no_summary() {
        assert!(summarize(&[]).is_none());
    }

    fn jittered(center: f64, amplitude: f64, n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| center + amplitude * (((i * 37) % 11) as f64 / 10.0 - 0.5))
            .collect()
    }

    #[test]
    fn a_clear_slowdown_is_reported() {
        let base = jittered(10.0, 0.4, 60);
        let cand = jittered(12.0, 0.4, 60);
        let c = compare(&base, &cand, 0.05).unwrap();
        assert_eq!(c.verdict, Verdict::Slower);
        assert!(c.interval.0 > 0.0);
    }

    #[test]
    fn a_clear_speedup_is_reported() {
        let c = compare(&jittered(10.0, 0.4, 60), &jittered(5.0, 0.4, 60), 0.05).unwrap();
        assert_eq!(c.verdict, Verdict::Faster);
    }

    #[test]
    fn a_shift_inside_the_noise_is_no_difference() {
        let c = compare(&jittered(10.0, 6.0, 40), &jittered(10.3, 6.0, 40), 0.02).unwrap();
        assert_eq!(c.verdict, Verdict::NoDifference);
    }

    #[test]
    fn a_small_but_certain_shift_below_the_threshold_is_no_difference() {
        let c = compare(&jittered(10.0, 0.01, 60), &jittered(10.2, 0.01, 60), 0.05).unwrap();
        assert_eq!(c.verdict, Verdict::NoDifference);
    }

    #[test]
    fn the_same_samples_give_the_same_interval() {
        let base = jittered(10.0, 1.0, 30);
        let cand = jittered(11.0, 1.0, 30);
        assert_eq!(compare(&base, &cand, 0.05), compare(&base, &cand, 0.05));
    }
}
