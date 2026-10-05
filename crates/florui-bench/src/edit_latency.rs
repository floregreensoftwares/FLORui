//! `florui-bench edit-latency`: how long from saving a stylesheet to the changed
//! frame being composed on the screen, split into the parts that add up to it.
//!
//! The benchmark opens a real window in a child process, rewrites that window's
//! stylesheet, and watches one pixel of the window through desktop duplication.
//! The child reports each frame's watcher stamp, start and end; duplication
//! reports when the compositor presented the frame that showed the new color.
//! All of it is on the QPC clock. Windows only, release build with the
//! `profiling` feature, and a desktop session nobody is disturbing.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use florui::Element;
use florui_platform::{WindowOptions, WindowSpec};
use florui_profile::FrameProfile;
use florui_style::Rgba;

use crate::composition::{
    Composition, find_window, keep_on_top, make_process_dpi_aware, qpc, qpc_frequency,
};
use crate::edit_chain::{
    ChildFrame, EditLatencyReport, Rgb, chain, color_for_round, format_frame_line,
    parse_frame_line, render, same_color, stylesheet, summarize_samples,
};
use crate::report::{BuildInfo, capture_environment};

const TITLE: &str = "florui-bench edit latency";

pub const NEEDS_PROFILING: &str =
    "this build cannot measure frames: build florui-bench with --release --features profiling";

// ---------------------------------------------------------------- the window

static ANCHOR: OnceLock<(Duration, i64)> = OnceLock::new();

/// A profiler time on the QPC clock, from the pair read when the window opened.
fn to_qpc(at: Duration) -> i64 {
    let (anchor_at, anchor_qpc) = ANCHOR.get().copied().unwrap_or_default();
    let delta = at.as_nanos() as i128 - anchor_at.as_nanos() as i128;
    anchor_qpc + (delta * i128::from(qpc_frequency()) / 1_000_000_000) as i64
}

fn print_frame(frame: &FrameProfile) {
    println!(
        "{}",
        format_frame_line(&ChildFrame {
            reload: frame.stylesheet_reload.map(to_qpc),
            start: to_qpc(frame.start),
            end: to_qpc(frame.end),
        })
    );
}

fn class(name: &str) -> Vec<(String, String)> {
    vec![("class".into(), name.into())]
}

fn page(rows: usize) -> Element {
    Element::node(
        "div",
        class("page"),
        (0..rows)
            .map(|i| {
                Element::node(
                    "div",
                    class("row"),
                    vec![Element::text(format!("Row number {i}"))],
                )
            })
            .collect(),
    )
}

/// The child: a window that reloads `css_path` and prints one line per frame.
pub fn run_window(css_path: &Path, rows: usize) -> Result<(), String> {
    if !florui_profile::ENABLED {
        return Err(NEEDS_PROFILING.into());
    }
    // The benchmark holds this pipe open for as long as it runs, so the window
    // goes away with it even when it dies without cleaning up.
    std::thread::spawn(|| {
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new());
        std::process::exit(0);
    });
    let _ = ANCHOR.set((florui_profile::now(), qpc()));
    florui_profile::start(false);
    florui_profile::set_frame_sink(Some(print_frame));
    let spec = WindowSpec::with_css_reload(
        TITLE,
        css_path,
        Rgba::opaque(0x10, 0x10, 0x14),
        WindowOptions::default(),
        move || page(rows),
    )
    .map_err(|e| e.to_string())?;
    florui_platform::run_windows(vec![spec]).map_err(|e| e.to_string())
}

// ------------------------------------------------------------- the benchmark

pub struct Options {
    pub rows: usize,
    /// Edits measured, after the first one, which is reported apart.
    pub rounds: usize,
    /// Write through a temporary file and a rename, as many editors do.
    pub atomic: bool,
    /// Rewrite the same stylesheet while waiting for a color that never comes,
    /// to show every edit then times out instead of reporting a latency.
    pub unchanged: bool,
    pub out_dir: Option<PathBuf>,
}

/// The window process and the folder its stylesheet lives in, removed on drop.
struct Session {
    child: Child,
    dir: PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_stylesheet(css: &Path, text: &str, atomic: bool) -> Result<(), String> {
    let result = if atomic {
        let temp = css.with_extension("css.tmp");
        std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, css))
    } else {
        std::fs::write(css, text)
    };
    result.map_err(|e| format!("could not write {}: {e}", css.display()))
}

fn drain(frames: &Receiver<ChildFrame>, into: &mut Vec<ChildFrame>) {
    while let Ok(frame) = frames.try_recv() {
        into.push(frame);
    }
}

/// Lets the window and the compositor go quiet.
fn settle(
    screen: &Composition,
    frames: &Receiver<ChildFrame>,
    time: Duration,
) -> Result<(), String> {
    let until = Instant::now() + time;
    let mut ignored = Vec::new();
    while Instant::now() < until {
        screen.next(10)?;
        drain(frames, &mut ignored);
    }
    Ok(())
}

struct Waited {
    composed: Option<i64>,
    intermediate: bool,
}

/// Watches the pixel until a frame presented at or after `t0` shows `target`.
fn wait_for_color(
    screen: &Composition,
    t0: i64,
    old: Rgb,
    target: Rgb,
    timeout: Duration,
    frames: &Receiver<ChildFrame>,
    seen: &mut Vec<ChildFrame>,
) -> Result<Waited, String> {
    let deadline = Instant::now() + timeout;
    let mut intermediate = false;
    while Instant::now() < deadline {
        let after_the_write = screen.next(2)?.filter(|presented| presented.qpc >= t0);
        if let Some(presented) = after_the_write {
            if same_color(presented.color, target) {
                drain(frames, seen);
                return Ok(Waited {
                    composed: Some(presented.qpc),
                    intermediate,
                });
            }
            if !same_color(presented.color, old) {
                intermediate = true;
            }
        }
        drain(frames, seen);
    }
    Ok(Waited {
        composed: None,
        intermediate,
    })
}

pub fn run(options: &Options) -> Result<(), String> {
    if !florui_profile::ENABLED {
        return Err(NEEDS_PROFILING.into());
    }
    if cfg!(debug_assertions) {
        return Err(
            "a debug build says nothing about latency; build with --release --features profiling"
                .into(),
        );
    }
    make_process_dpi_aware();

    let dir = std::env::temp_dir().join(format!("florui-edit-latency-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let css = dir.join("app.css");
    write_stylesheet(&css, &stylesheet(color_for_round(0)), false)?;

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg("edit-latency-window")
        .arg(&css)
        .args(["--rows", &options.rows.to_string()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start the window process: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("the window process has no stdout")?;
    let pid = child.id();
    let _session = Session { child, dir };

    let (sender, frames) = mpsc::channel();
    std::thread::spawn(move || {
        let reported = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| parse_frame_line(&line));
        for frame in reported {
            if sender.send(frame).is_err() {
                break;
            }
        }
    });

    let started = Instant::now();
    let window = loop {
        if let Some(window) = find_window(pid, TITLE) {
            break window;
        }
        if started.elapsed() > Duration::from_secs(30) {
            return Err("the window never opened".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    keep_on_top(&window);
    std::thread::sleep(Duration::from_millis(1500));

    // Right of the rows' text and below the title bar: only the page background.
    let x = window.rect.right - 24;
    let y = (window.rect.top + window.rect.bottom) / 2;
    let screen = Composition::new(x, y)?;
    settle(&screen, &frames, Duration::from_millis(500))?;

    let frequency = qpc_frequency();
    let mut cold = None;
    let mut samples = Vec::new();
    let mut timeouts = 0;
    let mut with_intermediate = 0;
    for round in 0..=options.rounds {
        settle(&screen, &frames, Duration::from_millis(400))?;
        let old = color_for_round(round);
        let target = color_for_round(round + 1);
        let text = stylesheet(if options.unchanged { old } else { target });
        let mut seen = Vec::new();

        let t0 = qpc();
        write_stylesheet(&css, &text, options.atomic)?;
        let waited = wait_for_color(
            &screen,
            t0,
            old,
            target,
            Duration::from_secs(2),
            &frames,
            &mut seen,
        )?;
        if waited.intermediate {
            with_intermediate += 1;
        }
        let Some(composed) = waited.composed else {
            timeouts += 1;
            continue;
        };
        let settled_at = Instant::now();
        let sample = loop {
            drain(&frames, &mut seen);
            if let Some(sample) = chain(t0, &seen, composed, frequency) {
                break Some(sample);
            }
            if settled_at.elapsed() > Duration::from_millis(400) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        match sample {
            Some(sample) if round == 0 => cold = Some(sample),
            Some(sample) => samples.push(sample),
            None => timeouts += 1,
        }
    }

    if cold.is_none() && samples.is_empty() && !options.unchanged {
        return Err(format!(
            "no edit showed on the watched pixel ({x}, {y}): is something covering the window?"
        ));
    }

    let report = EditLatencyReport {
        environment: capture_environment(&BuildInfo::this_build()),
        rows: options.rows,
        write_mode: if options.atomic { "atomic" } else { "in-place" }.into(),
        refresh_hz: screen.refresh_hz,
        cold,
        intervals: summarize_samples(&samples),
        edits_with_an_intermediate_frame: with_intermediate,
        timeouts,
        samples,
    };
    let markdown = render(&report);
    println!("{markdown}");
    if let Some(out_dir) = &options.out_dir {
        std::fs::create_dir_all(out_dir)
            .map_err(|e| format!("could not create {}: {e}", out_dir.display()))?;
        let stem = format!("edit-latency-{}-rows-{}", options.rows, report.write_mode);
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(out_dir.join(format!("{stem}.json")), json)
            .map_err(|e| format!("could not write the report: {e}"))?;
        std::fs::write(out_dir.join(format!("{stem}.md")), &markdown)
            .map_err(|e| format!("could not write the report: {e}"))?;
    }
    Ok(())
}
