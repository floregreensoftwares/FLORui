//! What the profiler costs. A build without the `profiling` feature has no
//! profiler at all ("compiled out"); one with it is idle until started, then
//! measures a summary per frame, or every span as well ("detail"). The mode of
//! a benchmark run comes from the `FLORUI_BENCH_PROFILER` variable
//! (`summary` or `detail`); without it a build that has the profiler leaves it
//! idle.

use std::hint::black_box;
use std::time::Instant;

use florui_profile::{Counter, Phase};

pub fn profiler_mode() -> &'static str {
    if !florui_profile::ENABLED {
        return "compiled out";
    }
    match std::env::var("FLORUI_BENCH_PROFILER").as_deref() {
        Ok("summary") => "summary",
        Ok("detail") => "detail",
        _ => "idle",
    }
}

/// Starts the profiler the way the environment asks, before a workload builds.
pub fn start_from_environment() {
    match profiler_mode() {
        "summary" => florui_profile::start(false),
        "detail" => florui_profile::start(true),
        _ => {}
    }
}

/// What one span and one counter update cost in a mode, in nanoseconds.
#[derive(Debug, Clone)]
pub struct PrimitiveCost {
    pub mode: &'static str,
    pub span_ns: f64,
    pub count_ns: f64,
}

const ITERATIONS: usize = 2_000_000;
/// A frame is finished this often, so detail mode keeps its spans (up to the
/// per-frame cap) instead of only counting the dropped ones.
const FRAME_EVERY: usize = 1000;

fn per_call(mut body: impl FnMut(usize)) -> f64 {
    let started = Instant::now();
    for i in 0..ITERATIONS {
        body(i);
    }
    started.elapsed().as_nanos() as f64 / ITERATIONS as f64
}

/// The cost of an empty loop of the same shape, to subtract.
fn loop_floor() -> f64 {
    per_call(|i| {
        black_box(i);
    })
}

/// Measures a span and a counter update with the profiler idle, then started
/// for a summary, then started with detail. Empty in a build without the
/// profiler. Each figure has the empty loop's cost taken off.
pub fn primitive_costs() -> Vec<PrimitiveCost> {
    if !florui_profile::ENABLED {
        return Vec::new();
    }
    let floor = loop_floor();
    let mut costs = Vec::new();
    for (mode, start) in [
        ("idle", None),
        ("summary", Some(false)),
        ("detail", Some(true)),
    ] {
        florui_profile::stop();
        if let Some(detail) = start {
            florui_profile::start(detail);
        }
        let span_ns = per_call(|i| {
            let span = florui_profile::span(Phase::Layout);
            black_box(&span);
            drop(span);
            if i % FRAME_EVERY == FRAME_EVERY - 1 {
                florui_profile::finish_frame(Vec::new());
            }
        });
        let count_ns = per_call(|i| {
            florui_profile::count(Counter::NodesPainted, 1);
            if i % FRAME_EVERY == FRAME_EVERY - 1 {
                florui_profile::finish_frame(Vec::new());
            }
        });
        florui_profile::stop();
        costs.push(PrimitiveCost {
            mode,
            span_ns: (span_ns - floor).max(0.0),
            count_ns: (count_ns - floor).max(0.0),
        });
    }
    costs
}

pub fn render_primitive_costs(costs: &[PrimitiveCost]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "| Profiler | One span (ns) | One counter update (ns) |\n| --- | ---: | ---: |\n",
    );
    for c in costs {
        let _ = writeln!(out, "| {} | {:.1} | {:.1} |", c.mode, c.span_ns, c.count_ns);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_costs_cover_every_mode_when_the_profiler_exists() {
        let costs = primitive_costs();
        if florui_profile::ENABLED {
            let modes: Vec<_> = costs.iter().map(|c| c.mode).collect();
            assert_eq!(modes, ["idle", "summary", "detail"]);
            assert!(
                costs
                    .iter()
                    .all(|c| c.span_ns.is_finite() && c.count_ns.is_finite())
            );
        } else {
            assert!(costs.is_empty());
        }
    }

    #[test]
    fn a_table_lists_each_mode() {
        let text = render_primitive_costs(&[PrimitiveCost {
            mode: "idle",
            span_ns: 1.5,
            count_ns: 0.5,
        }]);
        assert!(text.contains("| idle | 1.5 | 0.5 |"));
    }
}
