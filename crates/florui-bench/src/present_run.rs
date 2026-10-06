//! `florui-bench present`: what showing a frame costs, by window size, in a real
//! window. A child process opens a window of the asked size and repaints it every
//! refresh with an animation; the profiler's frame sink prints the time of each
//! phase of presenting, and this process takes the medians once the window has
//! warmed up. Windows only, release with the `profiling` feature.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use florui::Element;
use florui_platform::{WindowOptions, WindowSpec};
use florui_profile::{FrameProfile, Phase};
use florui_style::Rgba;

use crate::composition::{client_size, find_window, keep_on_top, make_process_dpi_aware};
use crate::edit_latency::NEEDS_PROFILING;
use crate::present::{
    PresentFrame, PresentReport, SizeResult, format_present_line, parse_present_line, render,
    summarize_frames, used_the_gpu_presenter,
};
use crate::report::{BuildInfo, capture_environment};

const TITLE: &str = "florui-bench present";

/// A background that changes every frame, so every frame is painted and shown.
const CSS: &str = "\
    @keyframes pulse { from { background-color: #202030; } to { background-color: #304060; } } \
    .page { width: 100%; min-height: 100vh; animation: pulse 1s linear infinite alternate; }";

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn print_frame(frame: &FrameProfile) {
    println!(
        "{}",
        format_present_line(&PresentFrame {
            upload: ms(frame.total(Phase::Upload)),
            acquire: ms(frame.total(Phase::Acquire)),
            submit: ms(frame.total(Phase::Submit)),
            flip: ms(frame.total(Phase::Flip)),
            present: ms(frame.total(Phase::Present)),
            raster: ms(frame.total(Phase::Raster)),
            update: ms(frame.total(Phase::Update)),
            wall: ms(frame.wall()),
        })
    );
}

/// The child: a window of `width` x `height` logical pixels that repaints itself.
pub fn run_window(width: u32, height: u32) -> Result<(), String> {
    if !florui_profile::ENABLED {
        return Err(NEEDS_PROFILING.into());
    }
    // The benchmark holds this pipe open for as long as it runs, so the window
    // goes away with it even when it dies without cleaning up.
    std::thread::spawn(|| {
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new());
        std::process::exit(0);
    });
    florui_profile::start(false);
    florui_profile::set_frame_sink(Some(print_frame));
    let spec = WindowSpec::new(
        TITLE,
        CSS,
        Rgba::opaque(0x10, 0x10, 0x14),
        WindowOptions {
            size: Some((f64::from(width), f64::from(height))),
            respect_reduced_motion: false,
            ..WindowOptions::default()
        },
        || Element::node("div", vec![("class".into(), "page".into())], Vec::new()),
    )
    .map_err(|e| e.to_string())?;
    florui_platform::run_windows(vec![spec]).map_err(|e| e.to_string())
}

pub struct Options {
    pub sizes: Vec<(u32, u32)>,
    /// Frames measured after the warm-up.
    pub frames: usize,
    pub warmup: usize,
    pub out_dir: Option<PathBuf>,
}

/// The window process, killed when this goes away.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn measure(size: (u32, u32), options: &Options) -> Result<SizeResult, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg("present-window")
        .args(["--size", &format!("{}x{}", size.0, size.1)])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start the window process: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("the window process has no stdout")?;
    let pid = child.id();
    let _guard = ChildGuard(child);

    let (sender, frames) = mpsc::channel();
    std::thread::spawn(move || {
        let reported = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| parse_present_line(&line));
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

    let wanted = options.warmup + options.frames;
    let mut seen = Vec::with_capacity(wanted);
    let deadline = Instant::now() + Duration::from_secs(120);
    while seen.len() < wanted {
        match frames.recv_timeout(Duration::from_secs(10)) {
            Ok(frame) => seen.push(frame),
            Err(_) => {
                return Err(format!(
                    "the {}x{} window produced {} frames of the {wanted} wanted",
                    size.0,
                    size.1,
                    seen.len()
                ));
            }
        }
        if Instant::now() > deadline {
            return Err(format!(
                "the {}x{} window was too slow to finish",
                size.0, size.1
            ));
        }
    }
    let measured = &seen[options.warmup..];
    Ok(SizeResult {
        requested: size,
        actual: client_size(&window).unwrap_or((0, 0)),
        frames: measured.len(),
        gpu_presenter: used_the_gpu_presenter(measured),
        columns: summarize_frames(measured),
    })
}

pub fn run(options: &Options) -> Result<(), String> {
    if !florui_profile::ENABLED {
        return Err(NEEDS_PROFILING.into());
    }
    if cfg!(debug_assertions) {
        return Err(
            "a debug build says nothing about presenting; build with --release --features profiling"
                .into(),
        );
    }
    make_process_dpi_aware();
    let mut results = Vec::new();
    for &size in &options.sizes {
        eprintln!("measuring {}x{}", size.0, size.1);
        results.push(measure(size, options)?);
    }
    let report = PresentReport {
        environment: capture_environment(&BuildInfo::this_build()),
        results,
    };
    let markdown = render(&report);
    println!("{markdown}");
    if let Some(out_dir) = &options.out_dir {
        std::fs::create_dir_all(out_dir)
            .map_err(|e| format!("could not create {}: {e}", out_dir.display()))?;
        let json = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(out_dir.join("present.json"), json)
            .map_err(|e| format!("could not write the report: {e}"))?;
        std::fs::write(out_dir.join("present.md"), &markdown)
            .map_err(|e| format!("could not write the report: {e}"))?;
    }
    Ok(())
}
