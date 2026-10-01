use std::path::PathBuf;
use std::process::ExitCode;

use florui_bench::alloc::CountingAllocator;
use florui_bench::report::{self, Report};
use florui_bench::{runner, workloads};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const USAGE: &str = "\
florui-bench: reproducible performance baselines

  florui-bench list
  florui-bench run [--label L] [--out FILE.json] [--processes N] [--filter A,B] [--no-heap] [--allow-debug]
  florui-bench compare BASELINE.json CANDIDATE.json [--threshold 0.05] [--out FILE.md]
  florui-bench ab --a EXE --b EXE [--rounds N] [--filter A,B] [--no-heap] [--threshold 0.05] [--out-dir DIR]
  florui-bench overhead [--out FILE.md]   (what one profiler span and counter cost in each mode)
  florui-bench mode                       (this build's profile and profiler mode)
  florui-bench measure NAME [--heap]      (one process's measurement; used by the commands above)

`run` measures every workload in separate processes, alternating their order,
and writes the raw samples as JSON plus a Markdown table next to it. `ab`
alternates two builds round by round so thermal drift and ordering affect both
the same way, then compares them. Use release builds.";

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn number<T: std::str::FromStr>(args: &[String], name: &str, default: T) -> Result<T, String> {
    match option(args, name) {
        Some(text) => text.parse().map_err(|_| format!("{name} needs a number")),
        None => Ok(default),
    }
}

fn profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

fn load(path: &str) -> Result<Report, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("could not read {path}: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("{path} is not a baseline report: {e}"))
}

fn write(path: &PathBuf, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("could not write {}: {e}", path.display()))
}

fn run_command(args: &[String]) -> Result<(), String> {
    if cfg!(debug_assertions) && !flag(args, "--allow-debug") {
        return Err("a debug build says nothing about performance; build with --release (or pass --allow-debug to try the tool)".into());
    }
    let label = option(args, "--label").unwrap_or_else(|| "baseline".into());
    let processes: usize = number(args, "--processes", 5)?;
    let filter = option(args, "--filter").unwrap_or_default();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut progress = |line: &str| eprintln!("{line}");
    let report = runner::run_suite(
        &exe,
        &label,
        processes,
        &filter,
        !flag(args, "--no-heap"),
        &mut progress,
    );
    let json_path = PathBuf::from(option(args, "--out").unwrap_or_else(|| format!("{label}.json")));
    write(
        &json_path,
        &serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?,
    )?;
    let markdown_path = json_path.with_extension("md");
    let markdown = report::render(&report);
    write(&markdown_path, &markdown)?;
    println!("{markdown}");
    eprintln!(
        "wrote {} and {}",
        json_path.display(),
        markdown_path.display()
    );
    Ok(())
}

fn compare_command(args: &[String]) -> Result<(), String> {
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[i - 1].starts_with("--")))
        .map(|(_, a)| a)
        .collect();
    let [baseline, candidate] = positional[..] else {
        return Err("compare needs a baseline and a candidate report".into());
    };
    let threshold: f64 = number(args, "--threshold", 0.05)?;
    let text = report::render_comparison(&load(baseline)?, &load(candidate)?, threshold);
    if let Some(path) = option(args, "--out") {
        write(&PathBuf::from(path), &text)?;
    }
    println!("{text}");
    Ok(())
}

fn ab_command(args: &[String]) -> Result<(), String> {
    let a = PathBuf::from(option(args, "--a").ok_or("ab needs --a EXE")?);
    let b = PathBuf::from(option(args, "--b").ok_or("ab needs --b EXE")?);
    let rounds: usize = number(args, "--rounds", 5)?;
    let threshold: f64 = number(args, "--threshold", 0.05)?;
    let filter = option(args, "--filter").unwrap_or_default();
    let out_dir = PathBuf::from(option(args, "--out-dir").unwrap_or_else(|| "ab".into()));
    let mut progress = |line: &str| eprintln!("{line}");
    let mut baseline: Option<Report> = None;
    let mut candidate: Option<Report> = None;
    for round in 0..rounds {
        eprintln!("== round {} of {rounds} ==", round + 1);
        let order: [(&PathBuf, &str); 2] = if round % 2 == 0 {
            [(&a, "baseline"), (&b, "candidate")]
        } else {
            [(&b, "candidate"), (&a, "baseline")]
        };
        for (exe, label) in order {
            let part = runner::run_suite(
                exe,
                label,
                1,
                &filter,
                round == 0 && !flag(args, "--no-heap"),
                &mut progress,
            );
            let slot = if label == "baseline" {
                &mut baseline
            } else {
                &mut candidate
            };
            match slot {
                Some(existing) => existing.absorb(part),
                None => *slot = Some(part),
            }
        }
    }
    let (baseline, candidate) = (
        baseline.ok_or("no rounds ran")?,
        candidate.ok_or("no rounds ran")?,
    );
    write(
        &out_dir.join("baseline.json"),
        &serde_json::to_string_pretty(&baseline).map_err(|e| e.to_string())?,
    )?;
    write(
        &out_dir.join("candidate.json"),
        &serde_json::to_string_pretty(&candidate).map_err(|e| e.to_string())?,
    )?;
    let text = report::render_comparison(&baseline, &candidate, threshold);
    write(&out_dir.join("comparison.md"), &text)?;
    println!("{text}");
    Ok(())
}

fn overhead_command(args: &[String]) -> Result<(), String> {
    let costs = florui_bench::overhead::primitive_costs();
    if costs.is_empty() {
        return Err("this build has no profiler; build with --features profiling".into());
    }
    let text = format!(
        "# Cost of one profiler primitive

- Build: {} profile
- Each figure is nanoseconds per call over {} calls with the cost of an empty loop taken off; detail mode finishes a frame every 1,000 spans.

{}",
        profile(),
        2_000_000,
        florui_bench::overhead::render_primitive_costs(&costs)
    );
    if let Some(path) = option(args, "--out") {
        write(&PathBuf::from(path), &text)?;
    }
    println!("{text}");
    Ok(())
}

fn measure_command(args: &[String]) -> Result<(), String> {
    let name = args.first().ok_or("measure needs a workload name")?;
    let workload = workloads::find(name).ok_or_else(|| format!("no workload named {name}"))?;
    let run = runner::measure(&workload, flag(args, "--heap"));
    println!(
        "{}",
        serde_json::to_string(&run).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("list") => {
            for w in workloads::all() {
                println!("{:28} {}", w.name, w.description);
            }
            Ok(())
        }
        Some("run") => run_command(&args[1..]),
        Some("compare") => compare_command(&args[1..]),
        Some("ab") => ab_command(&args[1..]),
        Some("measure") => measure_command(&args[1..]),
        Some("overhead") => overhead_command(&args[1..]),
        Some("mode") => {
            println!("{}", florui_bench::report::BuildInfo::this_build().line());
            Ok(())
        }
        _ => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
