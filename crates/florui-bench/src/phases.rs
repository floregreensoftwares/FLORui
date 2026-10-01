//! Where a frame's time goes: the profiler's per-phase times and counters for
//! one workload, in one process.
//!
//! A benchmark says how long a workload takes; this says in which phase. It
//! runs the workload with the profiler started, drops the first (cold) run and
//! a few warm-up runs, and reports the median of each phase over the frames
//! that remain. A phase can contain others (an update contains render,
//! cascade and layout), so the rows are not meant to add up to the frame.
//!
//! It needs a build with the profiler (`--features profiling`, or a debug
//! build), and a workload that ends a frame: the profiler summarizes a frame
//! when one is painted, so a workload that only updates produces none.

use florui_profile::{Counter, FrameProfile, Phase};

use crate::workloads::Workload;

/// Runs before the profiler starts, so caches are warm.
const WARM_UP: usize = 3;

/// One phase's median over the profiled frames.
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseRow {
    pub phase: Phase,
    /// Median of the phase's total in a frame, over the frames it appears in.
    pub median_ms: f64,
    /// Median of how many times it ran in a frame.
    pub calls: u64,
}

/// What the profiler saw over a workload's profiled runs.
#[derive(Debug)]
pub struct PhaseReport {
    pub workload: String,
    pub frames: usize,
    /// Median wall time of a frame, from its first measurement to its last.
    pub wall_ms: f64,
    pub rows: Vec<PhaseRow>,
    /// The counters of the last frame (they repeat frame to frame in a
    /// workload that does the same thing each time).
    pub counters: Vec<(Counter, u64)>,
}

fn median(mut values: Vec<f64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.partial_cmp(b).expect("times are numbers"));
    values[values.len() / 2]
}

/// The report for `frames`, as measured from `workload`.
pub fn report_of(workload: &str, frames: &[FrameProfile]) -> PhaseReport {
    let rows = Phase::ALL
        .iter()
        .filter_map(|&phase| {
            let seen: Vec<&FrameProfile> = frames
                .iter()
                .filter(|frame| frame.phase(phase).is_some())
                .collect();
            if seen.is_empty() {
                return None;
            }
            let times = seen
                .iter()
                .map(|frame| frame.total(phase).as_secs_f64() * 1e3)
                .collect();
            let calls = median(
                seen.iter()
                    .map(|frame| frame.phase(phase).map_or(0, |p| p.calls) as f64)
                    .collect(),
            ) as u64;
            Some(PhaseRow {
                phase,
                median_ms: median(times),
                calls,
            })
        })
        .collect();
    let counters = frames
        .last()
        .map(|frame| {
            Counter::ALL
                .iter()
                .map(|&counter| (counter, frame.counter(counter)))
                .filter(|&(_, value)| value > 0)
                .collect()
        })
        .unwrap_or_default();
    PhaseReport {
        workload: workload.to_string(),
        frames: frames.len(),
        wall_ms: median(
            frames
                .iter()
                .map(|frame| frame.wall().as_secs_f64() * 1e3)
                .collect(),
        ),
        rows,
        counters,
    }
}

/// Runs `workload` `ops` times under the profiler and reports its phases.
pub fn measure(workload: &Workload, ops: usize) -> Result<PhaseReport, String> {
    if !florui_profile::ENABLED {
        return Err("this build has no profiler; build with --features profiling".into());
    }
    let mut op = (workload.build)();
    op();
    for _ in 0..WARM_UP {
        op();
    }
    florui_profile::clear();
    florui_profile::start(false);
    for _ in 0..ops {
        op();
    }
    let frames = florui_profile::frames();
    florui_profile::stop();
    if frames.is_empty() {
        return Err(format!(
            "{} ended no frame, so the profiler has nothing to summarize; a workload that only \
             updates paints none, use one that paints (a frame workload)",
            workload.name
        ));
    }
    Ok(report_of(workload.name, &frames))
}

/// The report as a Markdown table.
pub fn render(report: &PhaseReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Phases of `{}`\n\n{} profiled frames, median wall time {:.2} ms. A phase can contain others, so the rows do not add up.\n\n",
        report.workload, report.frames, report.wall_ms
    ));
    out.push_str("| Phase | Median (ms) | Calls |\n| --- | ---: | ---: |\n");
    for row in &report.rows {
        out.push_str(&format!(
            "| {} | {:.2} | {} |\n",
            row.phase.name(),
            row.median_ms,
            row.calls
        ));
    }
    if !report.counters.is_empty() {
        out.push_str("\n| Counter (last frame) | Value |\n| --- | ---: |\n");
        for (counter, value) in &report.counters {
            out.push_str(&format!("| {} | {} |\n", counter.name(), value));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workloads;

    #[test]
    fn the_median_is_the_middle_value() {
        assert_eq!(median(vec![5.0, 1.0, 3.0]), 3.0);
        assert_eq!(median(vec![4.0]), 4.0);
        assert_eq!(median(Vec::new()), 0.0);
    }

    #[test]
    fn a_workload_that_paints_reports_its_phases_and_counters() {
        let workload = workloads::find("deep_100_levels").expect("the workload exists");
        let report = measure(&workload, 4).expect("it paints, so frames are profiled");
        assert!(report.frames >= 4, "{} frames", report.frames);
        let phases: Vec<&str> = report.rows.iter().map(|row| row.phase.name()).collect();
        for expected in ["cascade", "layout", "raster"] {
            assert!(phases.contains(&expected), "{expected} in {phases:?}");
        }
        assert!(report.rows.iter().all(|row| row.median_ms >= 0.0));
        assert!(
            report
                .counters
                .iter()
                .any(|&(counter, value)| counter == Counter::NodesLaidOut && value > 0),
            "nodes laid out are counted: {:?}",
            report.counters
        );
        let text = render(&report);
        assert!(text.contains("| layout |") && text.contains("nodes-laid-out"));
    }

    #[test]
    fn a_workload_that_only_updates_says_why_it_has_no_phases() {
        let workload = workloads::find("update_100_rows").expect("the workload exists");
        let error = measure(&workload, 3).expect_err("an update alone ends no frame");
        assert!(error.contains("ended no frame"), "{error}");
    }
}
