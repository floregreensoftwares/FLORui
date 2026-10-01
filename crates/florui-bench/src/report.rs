//! What a run records, and how it reads: the environment, every raw sample,
//! and tables with medians, spread and tail percentiles.

use std::fmt::Write as _;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::stats::{self, Verdict};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub commit: String,
    pub uncommitted_changes: bool,
    pub rustc: String,
    pub os: String,
    pub cpu: String,
    pub logical_cores: usize,
    pub power_plan: String,
    pub profile: String,
    /// `compiled out`, `idle`, `summary` or `detail`; reports from before the
    /// profiler existed have none.
    #[serde(default)]
    pub profiler: String,
    pub captured_at_unix_seconds: u64,
}

/// Heap figures, measured in a run of their own so the counting allocator does
/// not touch the timings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeapRun {
    /// Most bytes live at once, setup included.
    pub peak_bytes: usize,
    pub allocations_per_op: f64,
    pub bytes_per_op: f64,
}

/// One process's measurements of one workload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessRun {
    /// Building the workload (window, stylesheet, first render).
    pub setup_ms: f64,
    /// The first operation in a fresh process: caches and fonts are cold.
    pub cold_ms: f64,
    /// Operations after the warm-up, in order, kept raw.
    pub warm_ms: Vec<f64>,
    pub heap: Option<HeapRun>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadReport {
    pub name: String,
    pub description: String,
    pub exercises: String,
    pub runs: Vec<ProcessRun>,
}

impl WorkloadReport {
    pub fn timed_runs(&self) -> impl Iterator<Item = &ProcessRun> {
        self.runs.iter().filter(|r| r.heap.is_none())
    }

    /// Every warm sample from every timed process.
    pub fn warm_samples(&self) -> Vec<f64> {
        self.timed_runs()
            .flat_map(|r| r.warm_ms.iter().copied())
            .collect()
    }

    pub fn cold_samples(&self) -> Vec<f64> {
        self.timed_runs().map(|r| r.cold_ms).collect()
    }

    pub fn heap(&self) -> Option<&HeapRun> {
        self.runs.iter().find_map(|r| r.heap.as_ref())
    }

    /// How far apart the processes' own medians are, relative to the overall
    /// median: a measure of whether a run reproduces itself.
    pub fn process_spread(&self) -> Option<f64> {
        let medians: Vec<f64> = self
            .timed_runs()
            .filter_map(|r| stats::summarize(&r.warm_ms).map(|s| s.median))
            .collect();
        if medians.len() < 2 {
            return None;
        }
        let overall = stats::summarize(&medians)?.median;
        let (low, high) = medians
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, h), m| (l.min(*m), h.max(*m)));
        (overall > 0.0).then(|| (high - low) / overall)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub label: String,
    pub environment: Environment,
    pub workloads: Vec<WorkloadReport>,
}

impl Report {
    pub fn workload(&self, name: &str) -> Option<&WorkloadReport> {
        self.workloads.iter().find(|w| w.name == name)
    }

    /// Adds another report's runs, for a comparison that alternates rounds.
    pub fn absorb(&mut self, other: Report) {
        for incoming in other.workloads {
            match self.workloads.iter_mut().find(|w| w.name == incoming.name) {
                Some(existing) => existing.runs.extend(incoming.runs),
                None => self.workloads.push(incoming),
            }
        }
    }
}

pub fn capture_environment(profile: &str, profiler: &str) -> Environment {
    let output = |program: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(program).args(args).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!text.is_empty()).then_some(text)
    };
    Environment {
        commit: output("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into()),
        uncommitted_changes: output("git", &["status", "--porcelain"]).is_some(),
        rustc: output("rustc", &["-V"]).unwrap_or_else(|| "unknown".into()),
        os: output("cmd", &["/C", "ver"]).unwrap_or_else(|| std::env::consts::OS.into()),
        cpu: std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".into()),
        logical_cores: std::thread::available_parallelism().map_or(0, |n| n.get()),
        power_plan: output("powercfg", &["/getactivescheme"]).unwrap_or_else(|| "unknown".into()),
        profile: profile.into(),
        profiler: profiler.into(),
        captured_at_unix_seconds: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    }
}

fn ms(value: f64) -> String {
    if value >= 100.0 {
        format!("{value:.0}")
    } else if value >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// The report as Markdown, with the environment first so a number is never
/// read without its conditions.
pub fn render(report: &Report) -> String {
    let env = &report.environment;
    let mut out = String::new();
    let _ = writeln!(out, "# Performance baseline: {}\n", report.label);
    let _ = writeln!(
        out,
        "- Commit: `{}`{}",
        env.commit,
        if env.uncommitted_changes {
            " (with uncommitted changes)"
        } else {
            ""
        }
    );
    let _ = writeln!(out, "- Build: {} profile, {}", env.profile, env.rustc);
    if !env.profiler.is_empty() {
        let _ = writeln!(out, "- Profiler: {}", env.profiler);
    }
    let _ = writeln!(out, "- OS: {}", env.os);
    let _ = writeln!(
        out,
        "- CPU: {} ({} logical cores)",
        env.cpu, env.logical_cores
    );
    let _ = writeln!(out, "- Power: {}", env.power_plan);
    let _ = writeln!(
        out,
        "- Headless: no window and no GPU, so every figure is CPU work (update, paint, accessibility tree); presentation is not included. Scale factor 1.0, 800x600.\n"
    );
    let _ = writeln!(
        out,
        "| Workload | Processes | Samples | Cold (ms) | Warm median (ms) | p95 (ms) | Spread (MAD) | Between processes | Allocs/op | Bytes/op | Peak heap |"
    );
    let _ = writeln!(
        out,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for workload in &report.workloads {
        let warm = workload.warm_samples();
        let Some(summary) = stats::summarize(&warm) else {
            let _ = writeln!(out, "| {} | no samples | | | | | | | | | |", workload.name);
            continue;
        };
        let cold = stats::summarize(&workload.cold_samples()).map_or("-".into(), |s| ms(s.median));
        let heap = workload.heap();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {:.1}% | {} | {} | {} | {} |",
            workload.name,
            workload.timed_runs().count(),
            summary.samples,
            cold,
            ms(summary.median),
            summary.p95.map_or("-".into(), ms),
            summary.relative_mad() * 100.0,
            workload
                .process_spread()
                .map_or("-".into(), |s| format!("{:.1}%", s * 100.0)),
            heap.map_or("-".into(), |h| format!("{:.0}", h.allocations_per_op)),
            heap.map_or("-".into(), |h| format!(
                "{:.0} KiB",
                h.bytes_per_op / 1024.0
            )),
            heap.map_or("-".into(), |h| format!(
                "{:.1} MiB",
                h.peak_bytes as f64 / 1048576.0
            )),
        );
    }
    let _ = writeln!(
        out,
        "\nCold is the first operation in a fresh process (median over processes); warm is every operation after three discarded warm-up runs, pooled over processes. A `-` for p95 means too few samples for a stable tail. Heap figures come from separate runs with a counting allocator.\n"
    );
    let _ = writeln!(out, "## What each workload exercises\n");
    for workload in &report.workloads {
        let _ = writeln!(
            out,
            "- `{}`: {}. Exercises {}.",
            workload.name, workload.description, workload.exercises
        );
    }
    out
}

/// A candidate against a baseline, workload by workload.
pub fn render_comparison(baseline: &Report, candidate: &Report, threshold: f64) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Comparison: {} against {}\n",
        candidate.label, baseline.label
    );
    let describe = |e: &Environment| {
        if e.profiler.is_empty() {
            format!("{} profile", e.profile)
        } else {
            format!("{} profile, profiler {}", e.profile, e.profiler)
        }
    };
    let _ = writeln!(
        out,
        "- Baseline: `{}` ({})",
        baseline.environment.commit,
        describe(&baseline.environment)
    );
    let _ = writeln!(
        out,
        "- Candidate: `{}` ({})",
        candidate.environment.commit,
        describe(&candidate.environment)
    );
    if baseline.environment.cpu != candidate.environment.cpu
        || baseline.environment.os != candidate.environment.os
    {
        let _ = writeln!(
            out,
            "- **The two runs are from different machines or systems; the comparison is not valid.**"
        );
    }
    let _ = writeln!(
        out,
        "- A change counts when it exceeds {:.0}% and its 95% bootstrap interval excludes zero.\n",
        threshold * 100.0
    );
    let _ = writeln!(
        out,
        "| Workload | Baseline (ms) | Candidate (ms) | Change | 95% interval | Verdict | Cold change |"
    );
    let _ = writeln!(out, "| --- | ---: | ---: | ---: | ---: | --- | ---: |");
    for base in &baseline.workloads {
        let Some(cand) = candidate.workload(&base.name) else {
            continue;
        };
        let Some(c) = stats::compare(&base.warm_samples(), &cand.warm_samples(), threshold) else {
            continue;
        };
        let cold = stats::compare(&base.cold_samples(), &cand.cold_samples(), threshold)
            .map_or("-".into(), |c| {
                format!("{:+.1}%", c.relative_change * 100.0)
            });
        let verdict = match c.verdict {
            Verdict::Faster => "faster",
            Verdict::Slower => "**slower**",
            Verdict::NoDifference => "no difference",
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {:+.1}% | {:+.1}% to {:+.1}% | {} | {} |",
            base.name,
            ms(c.baseline_median),
            ms(c.candidate_median),
            c.relative_change * 100.0,
            c.interval.0 * 100.0,
            c.interval.1 * 100.0,
            verdict,
            cold,
        );
    }
    let _ = writeln!(
        out,
        "\nCold change compares the first operation of each fresh process; with few processes it is indicative only."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(warm: &[f64]) -> ProcessRun {
        ProcessRun {
            setup_ms: 1.0,
            cold_ms: warm[0] * 3.0,
            warm_ms: warm.to_vec(),
            heap: None,
        }
    }

    fn report(label: &str, warm: &[f64]) -> Report {
        Report {
            label: label.into(),
            environment: capture_environment("test", "idle"),
            workloads: vec![WorkloadReport {
                name: "w".into(),
                description: "d".into(),
                exercises: "e".into(),
                runs: vec![run(warm), run(warm)],
            }],
        }
    }

    #[test]
    fn a_report_reads_back_from_json() {
        let original = report("a", &[1.0, 2.0, 3.0]);
        let json = serde_json::to_string(&original).unwrap();
        let back: Report = serde_json::from_str(&json).unwrap();
        assert_eq!(back.workloads[0].warm_samples().len(), 6);
    }

    #[test]
    fn runs_from_another_round_are_added_to_the_same_workload() {
        let mut a = report("a", &[1.0, 2.0]);
        a.absorb(report("b", &[3.0, 4.0]));
        assert_eq!(a.workloads.len(), 1);
        assert_eq!(a.workloads[0].runs.len(), 4);
    }

    #[test]
    fn the_spread_between_processes_shows_a_run_that_does_not_reproduce() {
        let mut r = report("a", &[10.0, 10.0, 10.0]);
        r.workloads[0].runs[1] = run(&[20.0, 20.0, 20.0]);
        let spread = r.workloads[0].process_spread().unwrap();
        assert!(spread > 0.5, "{spread}");
    }

    #[test]
    fn the_rendered_report_names_its_conditions_and_workloads() {
        let text = render(&report("label", &[1.0, 2.0, 3.0]));
        assert!(text.contains("# Performance baseline: label"));
        assert!(text.contains("| w |"));
        assert!(text.contains("CPU work"));
    }

    #[test]
    fn a_comparison_marks_a_clear_regression() {
        let base = report("base", &[10.0, 10.1, 9.9, 10.05, 9.95, 10.0]);
        let cand = report("cand", &[14.0, 14.1, 13.9, 14.05, 13.95, 14.0]);
        let text = render_comparison(&base, &cand, 0.05);
        assert!(text.contains("**slower**"), "{text}");
    }
}
