//! Measuring one workload in this process, and running a whole suite as one
//! child process per workload and repetition so no run inherits another's
//! caches, heap or fonts.

use std::hint::black_box;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::alloc;
use crate::report::{HeapRun, ProcessRun, Report, WorkloadReport, capture_environment};
use crate::workloads::{self, Workload};

/// Operations run and thrown away after the cold one, before sampling.
const WARM_UP: usize = 3;
const MIN_SAMPLES: usize = 10;
const TARGET_SAMPLES: usize = 100;
/// Sampling stops here even short of the target, so a slow workload cannot
/// run for minutes.
const SAMPLING_BUDGET: Duration = Duration::from_secs(8);
/// A child that outlives this is killed and reported, not waited for.
const CHILD_TIMEOUT: Duration = Duration::from_secs(180);

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Measures `workload` in this process. With `heap`, counts allocations and
/// does not report timings that should be used.
pub fn measure(workload: &Workload, heap: bool) -> ProcessRun {
    if heap {
        alloc::enable();
    }
    let started = Instant::now();
    let mut op = (workload.build)();
    let setup_ms = elapsed_ms(started);

    let cold_started = Instant::now();
    op();
    let cold_ms = elapsed_ms(cold_started);

    for _ in 0..WARM_UP {
        op();
    }

    let before = alloc::snapshot();
    let sampling = Instant::now();
    let mut warm_ms = Vec::new();
    while warm_ms.len() < TARGET_SAMPLES
        && (warm_ms.len() < MIN_SAMPLES || sampling.elapsed() < SAMPLING_BUDGET)
    {
        let sample = Instant::now();
        op();
        warm_ms.push(elapsed_ms(sample));
    }
    black_box(&warm_ms);
    let after = alloc::snapshot();

    let heap = heap.then(|| {
        let ops = warm_ms.len().max(1) as f64;
        HeapRun {
            peak_bytes: after.peak,
            allocations_per_op: (after.allocations - before.allocations) as f64 / ops,
            bytes_per_op: (after.bytes - before.bytes) as f64 / ops,
        }
    });
    ProcessRun {
        setup_ms,
        cold_ms,
        warm_ms,
        heap,
    }
}

fn run_child(exe: &Path, name: &str, heap: bool) -> Result<ProcessRun, String> {
    let mut command = Command::new(exe);
    command.arg("measure").arg(name);
    if heap {
        command.arg("--heap");
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                let mut stdout = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if !status.success() {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    return Err(format!("{name} exited with {status}: {}", stderr.trim()));
                }
                let line = stdout.lines().last().unwrap_or_default();
                return serde_json::from_str(line)
                    .map_err(|e| format!("{name} printed something unreadable: {e}"));
            }
            None if started.elapsed() > CHILD_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{name} ran past {} s and was stopped",
                    CHILD_TIMEOUT.as_secs()
                ));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Runs every workload whose name contains `filter` as `processes` timed
/// child processes plus one heap run, through the binary at `exe`. Rounds run
/// the workloads in alternating order so position in the run does not favor
/// any of them. A workload the binary does not know is skipped.
pub fn run_suite(
    exe: &Path,
    label: &str,
    processes: usize,
    filter: &str,
    heap: bool,
    profile: &str,
    progress: &mut dyn FnMut(&str),
) -> Report {
    let selected: Vec<Workload> = workloads::all()
        .into_iter()
        .filter(|w| w.name.contains(filter))
        .collect();
    let mut reports: Vec<WorkloadReport> = selected
        .iter()
        .map(|w| WorkloadReport {
            name: w.name.into(),
            description: w.description.into(),
            exercises: w.exercises.into(),
            runs: Vec::new(),
        })
        .collect();
    for round in 0..processes {
        let order: Vec<usize> = if round % 2 == 0 {
            (0..reports.len()).collect()
        } else {
            (0..reports.len()).rev().collect()
        };
        for index in order {
            let name = reports[index].name.clone();
            progress(&format!("{name}  (process {} of {processes})", round + 1));
            match run_child(exe, &name, false) {
                Ok(run) => reports[index].runs.push(run),
                Err(message) => progress(&format!("  skipped: {message}")),
            }
        }
    }
    if heap {
        for report in &mut reports {
            let name = report.name.clone();
            progress(&format!("{name}  (heap)"));
            match run_child(exe, &name, true) {
                Ok(run) => report.runs.push(run),
                Err(message) => progress(&format!("  skipped: {message}")),
            }
        }
    }
    reports.retain(|r| !r.runs.is_empty());
    Report {
        label: label.into(),
        environment: capture_environment(profile),
        workloads: reports,
    }
}
