//! A report on standard error, every few seconds, of what the process holds and
//! what keeps it busy: memory by owner, why the event loop ran, how many frames
//! it drew and what is animating. It is for finding out why an app that looks
//! idle uses the processor or holds memory, and it costs nothing unless asked.
//!
//! Set `FLORUI_REPORT=<seconds>` (any other value means five) in a build that
//! can measure: a debug build, or a release build with the `profiling` feature.
//! In another build the variable is ignored, with one line saying so.
//!
//! The report names kinds of work (the loop's wake reasons, the component that
//! wrote a signal, an animation's name), never values from the application.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use florui_profile::gauge::{self, Reading, Unit};
use florui_profile::wake::Activity;
use florui_style::{RunningAnimation, RunningKind};

const DEFAULT_SECONDS: u64 = 5;

/// What the operating system says the process holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessMemory {
    /// Pages resident in memory, shared ones included.
    pub working_set: u64,
    /// Memory only this process uses: private bytes on Windows, anonymous
    /// resident pages on Linux.
    pub private: u64,
    pub threads: Option<u32>,
}

#[cfg(windows)]
pub(crate) fn process_memory() -> Option<ProcessMemory> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: `counters` is a plain struct of integers that the call fills; `cb` tells it
    // which of the two layouts it has, and the pseudo handle needs no closing.
    unsafe {
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let filled = K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        );
        (filled != 0).then_some(ProcessMemory {
            working_set: counters.WorkingSetSize as u64,
            private: counters.PrivateUsage as u64,
            threads: None,
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_memory() -> Option<ProcessMemory> {
    parse_proc_status(&std::fs::read_to_string("/proc/self/status").ok()?)
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(crate) fn process_memory() -> Option<ProcessMemory> {
    None
}

/// Reads the memory lines of `/proc/<pid>/status`, which count in kibibytes.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_status(text: &str) -> Option<ProcessMemory> {
    let field = |name: &str| -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    Some(ProcessMemory {
        working_set: field("VmRSS")? * 1024,
        private: field("RssAnon")? * 1024,
        threads: field("Threads").and_then(|n| u32::try_from(n).ok()),
    })
}

/// What a window tells the report about itself.
pub(crate) struct WindowReport {
    pub nodes: usize,
    pub decoded_images: u64,
    pub animations: Vec<RunningAnimation>,
}

/// Counts of the signal writes (and other updates) that happened, by the
/// component that was running, since the last report.
type Writes = Rc<RefCell<BTreeMap<String, u64>>>;

pub(crate) struct ActivityReporter {
    every: Duration,
    next: Instant,
    writes: Writes,
    _trace: florui_reactive::trace::Subscription,
}

impl ActivityReporter {
    /// A reporter if `FLORUI_REPORT` asks for one. Must be made on the thread
    /// that runs the components, since that is where their writes are traced.
    pub(crate) fn from_env() -> Option<Self> {
        let value = std::env::var("FLORUI_REPORT").ok()?;
        if !florui_profile::ENABLED {
            eprintln!(
                "florui-platform: FLORUI_REPORT needs a debug build or the `profiling` feature"
            );
            return None;
        }
        let seconds = value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|seconds| *seconds > 0)
            .unwrap_or(DEFAULT_SECONDS);
        let writes: Writes = Rc::default();
        let sink = Rc::clone(&writes);
        let trace = florui_reactive::trace::subscribe(move |trace| {
            *sink.borrow_mut().entry(describe_trace(&trace)).or_default() += 1;
        });
        // Marks where the loop's counting begins.
        let _ = florui_profile::wake::take();
        let every = Duration::from_secs(seconds);
        eprintln!("florui report: every {seconds} s (FLORUI_REPORT)");
        Some(Self {
            every,
            next: Instant::now() + every,
            writes,
            _trace: trace,
        })
    }

    /// When the loop must run next for the report.
    pub(crate) fn deadline(&self) -> Instant {
        self.next
    }

    pub(crate) fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// Says what the last stretch was and sets the next report.
    pub(crate) fn emit(&mut self, now: Instant, windows: &[WindowReport]) {
        self.next = now + self.every;
        let writes = std::mem::take(&mut *self.writes.borrow_mut());
        let text = format_report(
            process_memory(),
            &gauge::read(),
            florui_profile::wake::take(),
            &writes,
            windows,
        );
        eprintln!("{text}");
    }
}

fn describe_trace(trace: &florui_reactive::trace::UpdateTrace) -> String {
    use florui_reactive::trace::TraceKind;
    let kind = match trace.kind {
        TraceKind::SignalWrite => "signal write",
        TraceKind::EventDispatched => "event",
        TraceKind::ResourceStarted => "resource started",
        TraceKind::ResourceCompleted => "resource completed",
    };
    format!(
        "{} ({kind})",
        trace.component.unwrap_or("outside a component")
    )
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn amount(reading: &Reading, value: usize) -> String {
    match reading.gauge.unit() {
        Unit::Bytes => mib(value as u64),
        Unit::Entries => format!("{value}"),
    }
}

/// The text of one report. Pure, so what it says is tested without a window.
pub(crate) fn format_report(
    process: Option<ProcessMemory>,
    readings: &[Reading],
    activity: Option<Activity>,
    writes: &BTreeMap<String, u64>,
    windows: &[WindowReport],
) -> String {
    let mut out = String::from("florui report");
    if let Some(activity) = &activity {
        out.push_str(&format!(" ({:.1} s)", activity.span.as_secs_f64()));
    }
    if let Some(process) = process {
        out.push_str(&format!(
            "\n  process    working set {}, private {}",
            mib(process.working_set),
            mib(process.private)
        ));
        if let Some(threads) = process.threads {
            out.push_str(&format!(", {threads} threads"));
        }
    }
    let held: Vec<String> = readings
        .iter()
        .filter(|reading| reading.peak > 0)
        .map(|reading| {
            format!(
                "{} {} (peak {})",
                reading.gauge.name(),
                amount(reading, reading.current),
                amount(reading, reading.peak)
            )
        })
        .collect();
    if !held.is_empty() {
        out.push_str(&format!("\n  held       {}", held.join(", ")));
    }
    for (index, window) in windows.iter().enumerate() {
        out.push_str(&format!(
            "\n  window {}   {} nodes, {} of decoded pictures",
            index + 1,
            window.nodes,
            mib(window.decoded_images)
        ));
    }
    if let Some(activity) = &activity {
        let wakes: Vec<String> = activity
            .wakes
            .iter()
            .filter(|(_, count)| *count > 0)
            .map(|(reason, count)| format!("{} {count}", reason.name()))
            .collect();
        if wakes.is_empty() && activity.frames == 0 {
            out.push_str("\n  loop       idle: no wakes, no frames");
        } else {
            out.push_str(&format!(
                "\n  loop       {} wakes ({}); {} frames, {:.1}/s",
                activity.total_wakes(),
                if wakes.is_empty() {
                    "none besides frames".to_owned()
                } else {
                    wakes.join(", ")
                },
                activity.frames,
                activity.frames_per_second()
            ));
            if activity.frames > 0 {
                out.push_str(&format!(
                    ", {:.2} ms each, drawing {:.1}% of the time",
                    activity.frame_time.as_secs_f64() * 1000.0 / activity.frames as f64,
                    activity.busy_share() * 100.0
                ));
            }
        }
    }
    for (index, window) in windows.iter().enumerate() {
        if window.animations.is_empty() {
            continue;
        }
        let list: Vec<String> = window
            .animations
            .iter()
            .map(|running| match &running.kind {
                RunningKind::Keyframes { name, infinite } => format!(
                    "@keyframes {name}{} on {}",
                    if *infinite { " (repeats for ever)" } else { "" },
                    elements(running.elements)
                ),
                RunningKind::Transition { property } => {
                    format!("transition of {property} on {}", elements(running.elements))
                }
            })
            .collect();
        out.push_str(&format!(
            "\n  animating  window {}: {}",
            index + 1,
            list.join("; ")
        ));
    }
    if !writes.is_empty() {
        let seconds = activity
            .as_ref()
            .map_or(1.0, |a| a.span.as_secs_f64().max(f64::EPSILON));
        let mut ranked: Vec<(&String, &u64)> = writes.iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        let list: Vec<String> = ranked
            .iter()
            .take(8)
            .map(|(what, count)| format!("{what} {count} ({:.1}/s)", **count as f64 / seconds))
            .collect();
        out.push_str(&format!("\n  updates    {}", list.join(", ")));
    }
    out
}

fn elements(count: usize) -> String {
    if count == 1 {
        "1 element".to_owned()
    } else {
        format!("{count} elements")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use florui_profile::gauge::Gauge;
    use florui_profile::wake::Wake;

    fn activity(frames: u64, animation: u64) -> Activity {
        Activity {
            span: Duration::from_secs(5),
            wakes: Wake::ALL
                .iter()
                .map(|&reason| {
                    (
                        reason,
                        if reason == Wake::Animation {
                            animation
                        } else {
                            0
                        },
                    )
                })
                .collect(),
            frames,
            frame_time: Duration::from_millis(2) * frames as u32,
        }
    }

    #[test]
    fn proc_status_is_read_in_bytes() {
        let status = "Name:\tapp\nVmRSS:\t  2048 kB\nRssAnon:\t 1024 kB\nThreads:\t12\n";
        assert_eq!(
            parse_proc_status(status),
            Some(ProcessMemory {
                working_set: 2048 * 1024,
                private: 1024 * 1024,
                threads: Some(12),
            })
        );
        assert_eq!(parse_proc_status("Name:\tapp\n"), None);
    }

    #[test]
    fn a_busy_loop_names_what_woke_it_and_what_animates() {
        let readings = [Reading {
            gauge: Gauge::LayerCache,
            current: 3 * 1024 * 1024,
            peak: 9 * 1024 * 1024,
        }];
        let mut writes = BTreeMap::new();
        writes.insert("Clock (signal write)".to_owned(), 5);
        let windows = [WindowReport {
            nodes: 120,
            decoded_images: 4 * 1024 * 1024,
            animations: vec![RunningAnimation {
                kind: RunningKind::Keyframes {
                    name: "pulse".to_owned(),
                    infinite: true,
                },
                elements: 2,
            }],
        }];
        let text = format_report(None, &readings, Some(activity(300, 300)), &writes, &windows);
        assert!(
            text.contains("layer-cache 3.0 MiB (peak 9.0 MiB)"),
            "{text}"
        );
        assert!(
            text.contains("300 wakes (animation 300); 300 frames, 60.0/s"),
            "{text}"
        );
        assert!(text.contains("2.00 ms each"), "{text}");
        assert!(
            text.contains("@keyframes pulse (repeats for ever) on 2 elements"),
            "{text}"
        );
        assert!(text.contains("Clock (signal write) 5 (1.0/s)"), "{text}");
    }

    #[test]
    fn a_quiet_loop_says_so() {
        let text = format_report(None, &[], Some(activity(0, 0)), &BTreeMap::new(), &[]);
        assert!(text.contains("idle: no wakes, no frames"), "{text}");
    }
}
