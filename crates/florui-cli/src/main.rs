use std::path::{Path, PathBuf};
use std::process::{Command as ChildCommand, ExitCode, ExitStatus};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use florui_conformance::driver::{ChromiumDriver, ChromiumOptions};
use florui_conformance::engine::render_fixture;
use florui_conformance::geometry::{GeometryReport, compare_geometry};
use florui_conformance::pixels::{
    PixelDiffOptions, PixelSummary, anaglyph_overlay, compare_pixels, overlay_images,
};
use florui_conformance::reference_fixture::load_reference_fixture;
use florui_conformance::regions::culprit_lines;
use florui_conformance::report::{ArtifactPaths, Outcome, Report, classify, write_report};
use florui_conformance::run_history::{
    self, BaselineSelector, DeltaStatus, FixtureOutcome, RunManifest, RunReport, RunStatus,
};
use florui_devtools::diagnostics::{dim_text, failure, success};
use test_cmd::{Suite, SuiteOutcome};

mod build;
mod compare_select;
mod distribution;
mod doctor;
mod fmt;
mod new;
mod resources;
mod schema_cmd;
mod test_cmd;

#[derive(Parser)]
#[command(name = "florui", about = "Florui project CLI")]
struct Cli {
    /// Selects a specific workspace package by name when the current
    /// directory doesn't resolve to exactly one on its own (an invocation
    /// from a virtual workspace root with more than one candidate member).
    #[arg(long, global = true)]
    package: Option<String>,
    /// Selects a named `[environments.<name>]` overlay from
    /// `florui.config.toml`. Defaults to "development" for `dev` and
    /// "production" for `doctor`; naming an environment that isn't
    /// declared is only an error when this flag is given explicitly --
    /// the command's own default silently falls back to base
    /// configuration when undeclared.
    #[arg(long, global = true)]
    environment: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a minimal compiling application.
    New { name: String },
    /// Resolves the current project via `cargo metadata` (works from any
    /// directory inside it, the same way `cargo build` does), builds and
    /// runs the cargo example declared in its `Cargo.toml`
    /// (`[package.metadata.florui.dev] example = "..."`), and restarts it
    /// whenever its Rust source changes — with an explicit report that its
    /// in-memory state was just reset, since a fresh process has none of
    /// the old one's. CSS reload for that running example is its own
    /// concern (see `florui_platform::run_with_css_reload`) — this only
    /// watches `.rs` files.
    ///
    /// `--fixture` instead runs the original, more primitive engine-only
    /// preview (one drawable rectangle driven directly by a CSS file, no
    /// real components) that this loop bootstrapped from — still useful
    /// for the conformance/geometry harness, but not a real project.
    Dev {
        /// Run the primitive CSS-fixture engine preview at this path
        /// instead of resolving and running the current project.
        #[arg(long, conflicts_with = "example")]
        fixture: Option<PathBuf>,
        /// Run this cargo example instead of the one declared in the
        /// resolved project's `[package.metadata.florui.dev]`.
        #[arg(long, conflicts_with = "fixture")]
        example: Option<String>,
    },
    /// Runs the selected package's Cargo tests (never the whole workspace)
    /// and the reference fixtures against Chromium, then prints which suites
    /// passed, failed or were skipped. Arguments after `--` go to
    /// `cargo test`.
    Test {
        /// Which suites to run. Asking for `visual` by name fails when its
        /// fixtures or Chromium are missing; under `all` it is reported as
        /// skipped.
        #[arg(long, value_enum, default_value = "all")]
        suite: Suite,
        /// Path to a Chromium-family binary; without it the pinned Chromium
        /// is used.
        #[arg(long, env = "FLORUI_CHROMIUM")]
        chromium: Option<PathBuf>,
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Compares reference fixtures' Chromium captures against Florui's own
    /// render and writes the images and a report for each. Only reads the
    /// fixtures: nothing here ever updates an expected result.
    Compare {
        /// A fixture to compare: its name under `fixtures/reference` at the
        /// workspace root, or the path of a directory with a manifest.json.
        /// Repeatable; every fixture when none is given.
        #[arg(long)]
        fixture: Vec<String>,
        /// Path to a Chromium-family binary (Chrome, Chromium, or Edge).
        #[arg(long, env = "FLORUI_CHROMIUM")]
        chromium: Option<PathBuf>,
        /// Directory to write report.json and image artifacts under
        /// (default: `florui-conformance` in the project's target directory).
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// Open the artifacts of each failing fixture (or of the only
        /// fixture selected) in the file manager.
        #[arg(long)]
        open: bool,
        /// Run Chromium with a visible window instead of headless.
        #[arg(long)]
        headed: bool,
        /// Keep the isolated Chromium profile directory after the run.
        #[arg(long)]
        keep_profile: bool,
    },
    /// Compares every reference fixture against a real Chromium binary in
    /// one batch, recording the whole run as a new numbered entry in a
    /// persistent run history — diffed against the most recent prior run
    /// by default, so a regression across a code change is a concrete
    /// `delta.md`, not something to re-derive from raw pixel numbers by
    /// hand. See `florui_conformance::run_history`'s own module doc.
    CompareAll {
        /// Root directory containing one subdirectory per reference fixture.
        #[arg(long, default_value = "fixtures/reference")]
        fixtures_root: PathBuf,
        /// Path to a Chromium-family binary (Chrome, Chromium, or Edge).
        #[arg(long, env = "FLORUI_CHROMIUM")]
        chromium: Option<PathBuf>,
        /// Persistent run-history root — deliberately outside `target/`,
        /// so old runs survive `cargo clean` instead of being ephemeral
        /// build output.
        #[arg(long, default_value = "output/florui-conformance")]
        output: PathBuf,
        /// Run id to diff this run against ("auto" for the most recent
        /// completed run, "none" to record this as baseline-less).
        #[arg(long, default_value = "auto")]
        baseline: String,
    },
    /// Builds the selected package in release mode, stages its executables in
    /// `target/florui-build/<package>/native/` and writes `report.json` there:
    /// the toolchain, source, lockfile, each file's hashes and which developer
    /// tooling the artifact was built with. Exits 2 under `--strict` when it
    /// includes any.
    Build {
        #[arg(long, default_value = "native")]
        target: String,
        /// Reject an artifact that includes developer tooling instead of
        /// reporting it as a warning.
        #[arg(long)]
        strict: bool,
    },
    /// Report real, observed evidence about the local environment and
    /// (when resolvable) the current project — see `doctor`'s own module
    /// doc for what each flag actually probes and how honestly it's
    /// allowed to report what it found.
    Doctor {
        #[arg(long, default_value = "native")]
        target: String,
        /// Probe for a real graphics adapter and device, in a separate
        /// bounded process.
        #[arg(long)]
        graphics: bool,
        /// Probe real window presentation (implies --graphics).
        #[arg(long)]
        presentation: bool,
        /// Report packaging/distribution diagnostics.
        #[arg(long)]
        distribution: bool,
        /// Check an existing native build output (the directory `florui build`
        /// staged) against its report and the current project, without building
        /// or changing anything.
        #[arg(long, value_name = "DIR")]
        artifacts: Option<PathBuf>,
        /// Emit a single JSON document on stdout instead of human-readable
        /// lines.
        #[arg(long)]
        json: bool,
        /// Also fail on applicable warning/unknown results, not just
        /// required failures.
        #[arg(long)]
        strict: bool,
    },
    /// Prints the JSON Schema editors read for `florui.config.toml`, or
    /// writes it next to the project's configuration so an editor can be
    /// pointed at it with `#:schema ./florui.config.schema.json`.
    Schema {
        /// Write the schema instead of printing it: into the project's
        /// directory, or to PATH when one is given.
        #[arg(long, value_name = "PATH", num_args = 0..=1)]
        write: Option<Option<PathBuf>>,
    },
    /// Reformats `view!`-containing `.rs` files — see `fmt`'s own module
    /// doc for exactly what this does and does not reformat.
    Fmt {
        /// Files or directories to format. Defaults to the whole
        /// resolved workspace when none are given.
        paths: Vec<PathBuf>,
        /// Report which files would change without writing anything;
        /// exits 1 if any would.
        #[arg(long)]
        check: bool,
        /// Format source read from stdin as if it were this file and print
        /// only the result to stdout; diagnostics go to stderr. The path
        /// selects the `rustfmt` configuration and edition.
        #[arg(long, value_name = "PATH", conflicts_with = "paths")]
        stdin_filepath: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    // Bypasses `clap` entirely -- these exist only so `doctor`'s own
    // `--graphics`/`--presentation` probes can re-invoke this exact
    // binary as a separate, bounded child process (see `doctor`'s own
    // module doc for why); they are not part of this CLI's real surface
    // and must never appear in its own `--help` output.
    match std::env::args().nth(1).as_deref() {
        Some(doctor::INTERNAL_GRAPHICS_PROBE_FLAG) => {
            return doctor::run_internal_graphics_probe();
        }
        Some(doctor::INTERNAL_PRESENTATION_PROBE_FLAG) => {
            return doctor::run_internal_presentation_probe();
        }
        _ => {}
    }

    let cli = Cli::parse();
    match cli.command {
        Command::Dev { fixture, example } => match fixture {
            Some(fixture) => run_dev(fixture),
            None => run_dev_example(cli.package, example, cli.environment),
        },
        Command::New { name } => new::run(&name),
        Command::Test {
            suite,
            chromium,
            args,
        } => run_test(cli.package, suite, chromium, args),
        Command::Compare {
            fixture,
            chromium,
            out_dir,
            open,
            headed,
            keep_profile,
        } => run_compare(CompareRequest {
            package: cli.package,
            fixtures: fixture,
            chromium,
            out_dir,
            open,
            headed,
            keep_profile,
        }),
        Command::CompareAll {
            fixtures_root,
            chromium,
            output,
            baseline,
        } => run_compare_all(fixtures_root, chromium, output, baseline),
        Command::Build { target, strict } => {
            if target != "native" {
                eprintln!(
                    "{}",
                    failure(&format!("target \"{target}\" is not supported yet"))
                );
                eprintln!(
                    "only \"native\" is currently supported; the web target has its own separate milestone"
                );
                ExitCode::FAILURE
            } else {
                build::run(build::Options {
                    package: cli.package,
                    environment: cli.environment,
                    strict,
                })
            }
        }
        Command::Doctor {
            target,
            graphics,
            presentation,
            distribution,
            artifacts,
            json,
            strict,
        } => doctor::run(doctor::Options {
            target,
            package: cli.package,
            environment: cli.environment,
            graphics,
            presentation,
            distribution,
            artifacts,
            json,
            strict,
        }),
        Command::Schema { write } => schema_cmd::run(schema_cmd::Options {
            package: cli.package,
            write,
        }),
        Command::Fmt {
            paths,
            check,
            stdin_filepath,
        } => fmt::run(fmt::Options {
            paths,
            check,
            stdin_filepath,
        }),
    }
}

fn run_dev(fixture: PathBuf) -> ExitCode {
    if !fixture.exists() {
        eprintln!("fixture not found: {}", fixture.display());
        return ExitCode::FAILURE;
    }
    match florui_devtools::preview::run(fixture) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// `target/<debug>/examples/<example><EXE_SUFFIX>` under `target_dir` —
/// cargo's own, undocumented-but-stable convention for where `cargo build
/// --example <name>` places its binary.
fn example_exe_path(target_dir: &Path, example: &str) -> PathBuf {
    target_dir
        .join("debug")
        .join("examples")
        .join(format!("{example}{}", std::env::consts::EXE_SUFFIX))
}

/// Runs `cargo build --example <example>`, inheriting stdio so cargo's own
/// compiler diagnostics reach the terminal directly rather than being
/// re-parsed and re-rendered here.
fn cargo_build_example(example: &str) -> std::io::Result<ExitStatus> {
    ChildCommand::new("cargo")
        .args(["build", "--example", example])
        .status()
}

/// Watches `package_root`'s `src` and `examples` directories (whichever
/// exist) for `.rs` file changes, sending on `tx` for each — never the
/// whole package root, since that would also see `cargo build`'s own
/// writes under `target/` and rebuild forever in response to its own
/// output. Returns every watcher created; a caller must keep them alive
/// for as long as it wants to keep watching.
fn watch_rust_sources(
    package_root: &Path,
    tx: mpsc::Sender<()>,
) -> notify::Result<Vec<RecommendedWatcher>> {
    let mut watchers = Vec::new();
    for subdir in ["src", "examples"] {
        let dir = package_root.join(subdir);
        if !dir.is_dir() {
            continue;
        }
        let tx = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let Ok(event) = result else { return };
                let touches_rust = event
                    .paths
                    .iter()
                    .any(|p| p.extension().is_some_and(|ext| ext == "rs"));
                if touches_rust {
                    // The loop may already have stopped reading; nothing to
                    // do if so.
                    let _ = tx.send(());
                }
            })?;
        watcher.watch(&dir, RecursiveMode::Recursive)?;
        watchers.push(watcher);
    }
    Ok(watchers)
}

fn exit_code_from_status(status: ExitStatus) -> ExitCode {
    if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Resolves the current project via `florui_config` (the one resolver
/// shared with `florui doctor`) and builds and runs its declared example —
/// `example_override`, if given, takes precedence over the resolved
/// `[dev].example`/legacy `[package.metadata.florui.dev]` the same way
/// `cargo run --example` overrides a crate's own `default-run` —
/// restarting it with an explicit report that its in-memory state was just
/// reset, since a fresh process has none of the old one's, whenever a
/// `.rs` file under the resolved package's `src`/`examples` changes. A
/// build that fails leaves whichever version last built successfully
/// running untouched, the same "stale but working, plus an actionable
/// diagnostic" recovery contract the CSS-fixture preview already
/// established; cargo's own compiler output is that diagnostic here,
/// printed directly rather than re-parsed.
///
/// Exits once the running example's own window closes on its own (not as
/// a result of a restart this loop performed), returning its exit code.
fn run_dev_example(
    package: Option<String>,
    example_override: Option<String>,
    environment: Option<String>,
) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => return fail(format!("could not determine the current directory: {err}")),
    };
    let facts = match florui_config::resolve_cargo_project(&cwd, package.as_deref()) {
        Ok(facts) => facts,
        Err(err) => return fail(err.to_string()),
    };
    let selection = florui_config::EnvironmentSelection {
        name: environment.as_deref().unwrap_or("development"),
        explicit: environment.is_some(),
    };
    let resolved_config = match florui_config::resolve(&facts, None, Some(selection)) {
        Ok(resolution) => resolution,
        Err(err) => return fail(err.to_string()),
    };
    let example = match example_override.or(resolved_config.config.dev.example) {
        Some(example) => example,
        None => {
            return fail(
                "no dev entry point configured for this package — add\n\n    \
                 [package.metadata.florui.dev]\n    example = \"<name>\"\n\n\
                 (or [dev]\n    example = \"<name>\"\n    in florui.config.toml) to its \
                 Cargo.toml, or pass --example <name> explicitly",
            );
        }
    };
    let package_root = facts.package_root;
    let exe_path = example_exe_path(&facts.target_dir, &example);

    println!(
        "{}",
        dim_text(&format!("building example \"{example}\"..."))
    );
    match cargo_build_example(&example) {
        Ok(status) if status.success() => {}
        Ok(status) => return fail(format!("cargo build exited with {status}")),
        Err(err) => return fail(format!("could not run cargo build: {err}")),
    }

    // `None` means "no window is currently running" — true after a rebuild
    // whose linker step failed (the old process was already killed to free
    // the .exe for that attempt), not just before the very first spawn.
    // The timeout arm below must only treat a dead child as "the user
    // closed it" when this is `Some`; otherwise it would mistake "still
    // waiting for the developer to fix a build error" for the window
    // having closed on its own and exit the whole loop.
    let mut child = match ChildCommand::new(&exe_path).spawn() {
        Ok(child) => Some(child),
        Err(err) => {
            return fail(format!(
                "could not run built example at {}: {err}",
                exe_path.display()
            ));
        }
    };
    println!("{}", success(&format!("✔ running \"{example}\"")));

    let (tx, rx) = mpsc::channel();
    let _watchers = match watch_rust_sources(&package_root, tx) {
        Ok(watchers) => watchers,
        Err(err) => return fail(format!("could not watch Rust sources: {err}")),
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(()) => {
                // A single save can fire several filesystem events (and an
                // editor writing multiple files at once fires more); drain
                // whatever else arrives in a short window so one save
                // triggers exactly one rebuild, not a burst of them.
                let debounce_until = std::time::Instant::now() + Duration::from_millis(150);
                while let Some(remaining) = debounce_until
                    .checked_duration_since(std::time::Instant::now())
                    .filter(|remaining| !remaining.is_zero())
                {
                    if rx.recv_timeout(remaining).is_err() {
                        break;
                    }
                }

                // The running .exe is locked on Windows — linking a new one
                // over it fails outright while the old process still holds
                // it open, so the old process must die *before* the build
                // is even attempted, not after it succeeds. That also means
                // a failed rebuild here cannot leave a stale-but-working
                // window the way a CSS-only reload failure can: there is
                // no window at all until the next successful build. `child`
                // stays `None` for the rest of this iteration until (and
                // unless) a rebuild actually succeeds below.
                if let Some(mut old) = child.take() {
                    let _ = old.kill();
                    let _ = old.wait();
                }
                println!(
                    "{}",
                    dim_text(&format!(
                        "Rust source changed — rebuilding \"{example}\" (window closed; \
                         in-memory UI state was reset)..."
                    ))
                );
                match cargo_build_example(&example) {
                    Ok(status) if status.success() => match ChildCommand::new(&exe_path).spawn() {
                        Ok(new_child) => {
                            child = Some(new_child);
                            println!(
                                "{}",
                                success(&format!("✔ rebuilt and restarted \"{example}\""))
                            );
                        }
                        Err(err) => {
                            return fail(format!(
                                "rebuild succeeded but could not restart {}: {err}",
                                exe_path.display()
                            ));
                        }
                    },
                    Ok(status) => {
                        println!(
                            "{}",
                            failure(&format!("✘ build failed (cargo exited with {status})"))
                        );
                        println!(
                            "{}",
                            dim_text(
                                "no window is open — fix the error above and save again to retry"
                            )
                        );
                    }
                    Err(err) => {
                        println!(
                            "{}",
                            failure(&format!("✘ could not run cargo build: {err}"))
                        );
                        println!(
                            "{}",
                            dim_text(
                                "no window is open — fix the error above and save again to retry"
                            )
                        );
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(current) = &mut child
                    && let Ok(Some(status)) = current.try_wait()
                {
                    return exit_code_from_status(status);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                eprintln!(
                    "{}",
                    failure("the source watcher stopped unexpectedly; exiting")
                );
                if let Some(mut current) = child.take() {
                    let _ = current.kill();
                }
                return ExitCode::FAILURE;
            }
        }
    }
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{}", failure(&message.to_string()));
    ExitCode::FAILURE
}

fn cleanup_profile(dir: &Path, keep: bool) {
    if !keep {
        std::fs::remove_dir_all(dir).ok();
    }
}

/// Resolves a Chromium binary: the explicit path/env value if given, else
/// the pinned build fetched by `scripts/fetch-chromium.ps1`, if present.
/// Never falls back to guessing a system install — callers decide how to
/// report an unresolved binary, since a missing one means different things
/// to `compare` (a hard error) and `test` (a skipped suite).
fn resolve_chromium(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path);
    }
    let pinned = florui_conformance::pin::default_executable_path();
    if pinned.exists() {
        println!(
            "{}",
            dim_text(&format!(
                "using pinned Chromium {} at {}",
                florui_conformance::pin::pin().version,
                pinned.display()
            ))
        );
        Some(pinned)
    } else {
        None
    }
}

fn discover_reference_fixtures(fixtures_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(fixtures_root) else {
        return Vec::new();
    };
    let mut fixtures: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path.join("manifest.json").is_file())
        .collect();
    fixtures.sort();
    fixtures
}

struct CompareResult {
    fixture_id: String,
    outcome: Outcome,
    pixels: PixelSummary,
    geometry: GeometryReport,
    artifacts_dir: PathBuf,
    /// The elements drawn where the pixels differ, innermost first.
    culprits: Vec<String>,
}

/// Runs one fixture's capture/compare/report cycle against an
/// already-launched `driver`, so a batch of fixtures (see `run_test`) can
/// share a single Chromium instance instead of relaunching it per fixture.
fn compare_fixture(
    driver: &ChromiumDriver,
    fixture_path: &Path,
    out_dir: &Path,
    chromium_executable: &Path,
) -> Result<CompareResult, String> {
    let fixture = load_reference_fixture(fixture_path)
        .map_err(|err| format!("could not load fixture: {err}"))?;

    let capture = driver
        .capture(&fixture)
        .map_err(|err| format!("capture failed: {err}"))?;

    let viewport = fixture.manifest.viewport;
    let engine = render_fixture(
        &fixture.manifest.florui,
        &fixture.manifest.florui.css,
        &fixture.manifest.canvas_color,
        viewport.width_css_px,
        viewport.height_css_px,
        viewport.device_pixel_ratio,
    )
    .map_err(|err| format!("engine render failed: {err}"))?;

    let pixel_report = compare_pixels(&capture.image, &engine.image, &PixelDiffOptions::default())
        .map_err(|err| err.to_string())?;
    let geometry_report = compare_geometry(
        capture.element_box_css_px,
        engine.element_box_css_px,
        fixture.manifest.classification.geometry_tolerance_px(),
    );

    let outcome = classify(
        &fixture.manifest.classification,
        &pixel_report.summary,
        &geometry_report,
    );
    let culprits = pixel_report
        .diff_region
        .map(|region| culprit_lines(&engine.elements, region, viewport.device_pixel_ratio, 5))
        .unwrap_or_default();

    let fixture_out_dir = out_dir.join(&fixture.manifest.id);
    std::fs::create_dir_all(&fixture_out_dir)
        .map_err(|err| format!("could not create {}: {err}", fixture_out_dir.display()))?;

    let reference_path = fixture_out_dir.join("reference.png");
    let result_path = fixture_out_dir.join("result.png");
    let diff_path = fixture_out_dir.join("diff.png");
    let overlay_path = fixture_out_dir.join("overlay.png");
    let anaglyph_path = fixture_out_dir.join("florui_vs_chromium.png");
    let overlay = overlay_images(&capture.image, &engine.image);
    let anaglyph = anaglyph_overlay(&capture.image, &engine.image);

    for (image, path) in [
        (&capture.image, &reference_path),
        (&engine.image, &result_path),
        (&pixel_report.diff_image, &diff_path),
        (&overlay, &overlay_path),
        (&anaglyph, &anaglyph_path),
    ] {
        image
            .save(path)
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }

    let captured_at_unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let report = Report {
        fixture_id: fixture.manifest.id.clone(),
        captured_at_unix_seconds,
        chromium_executable: chromium_executable.to_owned(),
        pixels: pixel_report.summary,
        geometry: geometry_report,
        artifacts: ArtifactPaths {
            reference: reference_path,
            result: result_path,
            diff: diff_path,
            overlay: overlay_path,
            anaglyph: anaglyph_path,
        },
        outcome,
        culprits: culprits.clone(),
    };
    write_report(&report, &fixture_out_dir).map_err(|err| err.to_string())?;

    Ok(CompareResult {
        fixture_id: fixture.manifest.id,
        outcome,
        pixels: pixel_report.summary,
        geometry: geometry_report,
        artifacts_dir: fixture_out_dir,
        culprits,
    })
}

fn print_compare_result(result: &CompareResult) {
    match result.outcome {
        Outcome::Pass => {
            println!(
                "{}",
                success(&format!("✔ {} — exact match", result.fixture_id))
            );
        }
        Outcome::Fail => {
            println!(
                "{}",
                failure(&format!(
                    "✘ {} — {} differing pixels ({:.2}%), max geometry delta {:.1}px",
                    result.fixture_id,
                    result.pixels.differing_pixels,
                    result.pixels.percent_different,
                    result.geometry.max_axis_delta_px
                ))
            );
            if !result.culprits.is_empty() {
                println!(
                    "{}",
                    dim_text("elements behind the difference, innermost first:")
                );
                for line in &result.culprits {
                    println!("{}", dim_text(&format!("  {line}")));
                }
            }
            println!(
                "{}",
                dim_text(&format!(
                    "artifacts written to {}",
                    result.artifacts_dir.display()
                ))
            );
        }
    }
}

struct CompareRequest {
    package: Option<String>,
    fixtures: Vec<String>,
    chromium: Option<PathBuf>,
    out_dir: Option<PathBuf>,
    open: bool,
    headed: bool,
    keep_profile: bool,
}

fn run_compare(request: CompareRequest) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => return fail(format!("could not read the current directory: {err}")),
    };
    // Fixtures and output belong to the project, not to wherever the command
    // was started. A fixture given as a path works outside any project.
    let (workspace_root, target_dir) =
        match florui_config::resolve_cargo_project(&cwd, request.package.as_deref()) {
            Ok(facts) => (facts.workspace_root, facts.target_dir),
            Err(_) if !request.fixtures.is_empty() => (cwd.clone(), cwd.join("target")),
            Err(err) => return fail(err.to_string()),
        };
    let fixtures_root = workspace_root.join("fixtures").join("reference");
    let available = discover_reference_fixtures(&fixtures_root);
    let fixtures = match compare_select::select(&fixtures_root, &available, &request.fixtures, &cwd)
    {
        Ok(fixtures) => fixtures,
        Err(err) => return fail(err),
    };

    let Some(chromium) = resolve_chromium(request.chromium) else {
        eprintln!("{}", failure("no Chromium binary configured"));
        eprintln!(
            "run scripts/fetch-chromium.ps1 to fetch the pinned build, or pass --chromium <path> / set FLORUI_CHROMIUM, e.g.:"
        );
        eprintln!(r#"  --chromium "C:\Program Files\Google\Chrome\Application\chrome.exe""#);
        eprintln!(r#"  --chromium "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe""#);
        return ExitCode::FAILURE;
    };

    // Chrome resolves a relative --user-data-dir against its own executable
    // directory, not the caller's cwd. Inside a write-protected install
    // directory (e.g. Program Files) that silently fails and Chrome falls
    // back to the real default profile, which then refuses remote
    // debugging outright ("non-default data directory" required). An
    // absolute path avoids that entirely.
    let out_dir = request
        .out_dir
        .unwrap_or_else(|| target_dir.join("florui-conformance"));
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        return fail(format!("could not create {}: {err}", out_dir.display()));
    }
    let out_dir = match out_dir.canonicalize() {
        Ok(path) => compare_select::plain(path),
        Err(err) => return fail(format!("could not resolve {}: {err}", out_dir.display())),
    };
    let profile_dir = out_dir.join(format!("chrome-profile-{}", std::process::id()));

    let driver = match ChromiumDriver::launch(ChromiumOptions {
        executable: chromium.clone(),
        user_data_dir: profile_dir.clone(),
        headless: !request.headed,
        launch_timeout: Duration::from_secs(30),
    }) {
        Ok(driver) => driver,
        Err(err) => return fail(format!("could not launch Chromium: {err}")),
    };

    let mut results = Vec::new();
    for fixture_path in &fixtures {
        results.push((
            fixture_path,
            compare_fixture(&driver, fixture_path, &out_dir, &chromium),
        ));
    }
    // Chrome holds the profile directory open until the process exits, so
    // the driver (and the browser it owns) must be dropped before any
    // attempt to remove that directory, or the removal silently fails.
    drop(driver);
    cleanup_profile(&profile_dir, request.keep_profile);

    let mut failed = 0;
    let only_one = results.len() == 1;
    for (fixture_path, result) in &results {
        match result {
            Ok(result) => {
                print_compare_result(result);
                let differs = result.outcome == Outcome::Fail;
                if differs {
                    failed += 1;
                } else {
                    println!(
                        "{}",
                        dim_text(&format!("artifacts in {}", result.artifacts_dir.display()))
                    );
                }
                if request.open
                    && (differs || only_one)
                    && let Err(err) = compare_select::open_directory(&result.artifacts_dir)
                {
                    eprintln!("{}", failure(&err));
                }
            }
            Err(err) => {
                println!(
                    "{}",
                    failure(&format!("✘ {}: {err}", fixture_path.display()))
                );
                failed += 1;
            }
        }
    }
    if results.len() > 1 {
        println!(
            "{} of {} fixtures match",
            results.len() - failed,
            results.len()
        );
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Parses `--baseline`'s three accepted forms — `clap` validates it's
/// present but not its shape, since the valid values depend on nothing
/// `clap` itself knows (a 6-digit run id isn't a fixed enum).
fn parse_baseline_selector(value: &str) -> Result<BaselineSelector, String> {
    match value {
        "auto" => Ok(BaselineSelector::Auto),
        "none" => Ok(BaselineSelector::None),
        other if other.len() == 6 && other.bytes().all(|b| b.is_ascii_digit()) => {
            Ok(BaselineSelector::Explicit(other.to_owned()))
        }
        other => Err(format!(
            "--baseline must be \"auto\", \"none\", or a 6-digit run id, got {other:?}"
        )),
    }
}

/// Runs every fixture under `fixtures_root` against `chromium`, recording
/// the batch as a new entry in the run history at `output` — see
/// `florui_conformance::run_history`'s own module doc for the on-disk
/// shape this produces (`runs/NNNNNN/`, `index.md`, per-run `delta.md`).
fn run_compare_all(
    fixtures_root: PathBuf,
    chromium: Option<PathBuf>,
    output: PathBuf,
    baseline: String,
) -> ExitCode {
    let selector = match parse_baseline_selector(&baseline) {
        Ok(selector) => selector,
        Err(err) => return fail(err),
    };

    let fixtures = discover_reference_fixtures(&fixtures_root);
    if fixtures.is_empty() {
        return fail(format!(
            "no reference fixtures found under {}",
            fixtures_root.display()
        ));
    }

    let Some(chromium) = resolve_chromium(chromium) else {
        eprintln!("{}", failure("no Chromium binary configured"));
        eprintln!(
            "run scripts/fetch-chromium.ps1 to fetch the pinned build, or pass --chromium <path> / set FLORUI_CHROMIUM, e.g.:"
        );
        eprintln!(r#"  --chromium "C:\Program Files\Google\Chrome\Application\chrome.exe""#);
        eprintln!(r#"  --chromium "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe""#);
        return ExitCode::FAILURE;
    };

    if let Err(err) = std::fs::create_dir_all(&output) {
        return fail(format!("could not create {}: {err}", output.display()));
    }
    let output = match output.canonicalize() {
        Ok(path) => path,
        Err(err) => return fail(format!("could not resolve {}: {err}", output.display())),
    };

    let existing = match run_history::list_run_ids(&output) {
        Ok(ids) => ids,
        Err(err) => return fail(err.to_string()),
    };
    let resolved_baseline = match run_history::select_baseline(&output, &existing, &selector) {
        Ok(baseline) => baseline,
        Err(err) => return fail(err.to_string()),
    };
    let baseline_run_id = resolved_baseline.as_ref().map(|(id, _)| id.clone());
    let (run_id, run_dir) = match run_history::allocate_run(&output, &existing) {
        Ok(allocated) => allocated,
        Err(err) => return fail(err.to_string()),
    };
    println!(
        "{}",
        dim_text(&format!(
            "run {run_id} — baseline {}",
            baseline_run_id.as_deref().unwrap_or("none")
        ))
    );

    let mut manifest = RunManifest {
        version: 1,
        run_id: run_id.clone(),
        baseline_run: baseline_run_id.clone(),
        fixture_count: fixtures.len(),
        status: RunStatus::Running,
    };
    if let Err(err) = run_history::write_manifest(&run_dir, &manifest) {
        return fail(err.to_string());
    }

    // Chrome's own scratch profile (crash reporter state, GPU cache, ...)
    // is not a run artifact worth keeping — deliberately outside `run_dir`
    // so the persistent run history stays just the fixtures' own
    // comparison output, even on a platform where Chrome still holds a
    // file handle open briefly after exit and `cleanup_profile` silently
    // can't remove it.
    let profile_dir = std::env::temp_dir().join(format!(
        "florui-conformance-chrome-profile-{}",
        std::process::id()
    ));
    let driver = match ChromiumDriver::launch(ChromiumOptions {
        executable: chromium.clone(),
        user_data_dir: profile_dir.clone(),
        headless: true,
        launch_timeout: Duration::from_secs(30),
    }) {
        Ok(driver) => driver,
        Err(err) => return fail(format!("could not launch Chromium: {err}")),
    };

    let mut fixture_outcomes = Vec::with_capacity(fixtures.len());
    let mut all_ok = true;
    for fixture_path in &fixtures {
        match compare_fixture(&driver, fixture_path, &run_dir, &chromium) {
            Ok(result) => {
                if result.outcome == Outcome::Fail {
                    all_ok = false;
                }
                print_compare_result(&result);
                fixture_outcomes.push(FixtureOutcome {
                    fixture_id: result.fixture_id,
                    outcome: result.outcome,
                    pixels: result.pixels,
                    geometry: result.geometry,
                });
            }
            Err(err) => {
                println!(
                    "{}",
                    failure(&format!("✘ {}: {err}", fixture_path.display()))
                );
                all_ok = false;
            }
        }
    }
    drop(driver);
    cleanup_profile(&profile_dir, false);

    let report = RunReport {
        version: 1,
        run_id: run_id.clone(),
        baseline_run: baseline_run_id,
        fixtures: fixture_outcomes,
    };
    if let Err(err) = run_history::write_run_report(&run_dir, &report) {
        return fail(err.to_string());
    }
    if let Err(err) = std::fs::write(
        run_dir.join("report.md"),
        run_history::render_report_markdown(&report),
    ) {
        return fail(format!("could not write report.md: {err}"));
    }

    let delta = run_history::build_delta(run_id.clone(), resolved_baseline.as_ref(), &report);
    if let Err(err) = run_history::write_run_delta(&run_dir, &delta) {
        return fail(err.to_string());
    }
    if let Err(err) = std::fs::write(
        run_dir.join("delta.md"),
        run_history::render_delta_markdown(&delta),
    ) {
        return fail(format!("could not write delta.md: {err}"));
    }

    manifest.status = RunStatus::Complete;
    if let Err(err) = run_history::write_manifest(&run_dir, &manifest) {
        return fail(err.to_string());
    }
    if let Err(err) = run_history::write_index(&output) {
        return fail(err.to_string());
    }

    println!();
    println!(
        "{}",
        dim_text(&format!(
            "run {run_id} report: {}",
            run_dir.join("report.md").display()
        ))
    );
    if delta.status == DeltaStatus::Changed {
        println!(
            "{}",
            dim_text(&format!("delta: {}", run_dir.join("delta.md").display()))
        );
    }
    println!(
        "{}",
        dim_text(&format!("index: {}", output.join("index.md").display()))
    );

    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_test(
    package: Option<String>,
    suite: Suite,
    chromium: Option<PathBuf>,
    extra: Vec<String>,
) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => return fail(format!("could not read the current directory: {err}")),
    };
    let facts = match florui_config::resolve_cargo_project(&cwd, package.as_deref()) {
        Ok(facts) => facts,
        Err(err) => return fail(err.to_string()),
    };

    let mut results = Vec::new();
    if suite.includes_cargo() {
        results.push(("cargo", run_cargo_suite(&facts, &extra)));
    }
    if suite.includes_visual() {
        results.push(("visual", run_visual_suite(&facts, suite, chromium)));
    }
    test_cmd::summarize(&results)
}

fn run_cargo_suite(facts: &florui_config::CargoProjectFacts, extra: &[String]) -> SuiteOutcome {
    let args = test_cmd::cargo_args(&facts.package_name, extra);
    println!("{}", dim_text(&format!("running cargo {}", args.join(" "))));
    match ChildCommand::new("cargo")
        .args(&args)
        .current_dir(&facts.package_root)
        .status()
    {
        Ok(status) if status.success() => SuiteOutcome::Passed,
        Ok(status) => SuiteOutcome::Failed(format!("cargo test {status}")),
        Err(err) => SuiteOutcome::Failed(format!("could not run cargo test: {err}")),
    }
}

fn run_visual_suite(
    facts: &florui_config::CargoProjectFacts,
    requested: Suite,
    chromium: Option<PathBuf>,
) -> SuiteOutcome {
    let unavailable = |reason: String| SuiteOutcome::unavailable(requested, Suite::Visual, reason);
    let fixtures_root = test_cmd::fixtures_root(&facts.workspace_root);
    let fixtures = discover_reference_fixtures(&fixtures_root);
    if fixtures.is_empty() {
        return unavailable(format!(
            "no reference fixtures in {}",
            fixtures_root.display()
        ));
    }
    let Some(chromium) = resolve_chromium(chromium) else {
        return unavailable(
            "no Chromium found: run scripts/fetch-chromium.ps1 or set FLORUI_CHROMIUM".to_string(),
        );
    };

    println!();
    let out_dir = facts.target_dir.join("florui-conformance");
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        return SuiteOutcome::Failed(format!("could not create {}: {err}", out_dir.display()));
    }
    let out_dir = match out_dir.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            return SuiteOutcome::Failed(format!("could not resolve {}: {err}", out_dir.display()));
        }
    };
    let profile_dir = out_dir.join(format!("chrome-profile-{}", std::process::id()));

    let driver = match ChromiumDriver::launch(ChromiumOptions {
        executable: chromium.clone(),
        user_data_dir: profile_dir.clone(),
        headless: true,
        launch_timeout: Duration::from_secs(30),
    }) {
        Ok(driver) => driver,
        Err(err) => return SuiteOutcome::Failed(format!("could not launch Chromium: {err}")),
    };

    let mut failed = 0;
    for fixture_path in &fixtures {
        match compare_fixture(&driver, fixture_path, &out_dir, &chromium) {
            Ok(result) => {
                if result.outcome == Outcome::Fail {
                    failed += 1;
                }
                print_compare_result(&result);
            }
            Err(err) => {
                println!(
                    "{}",
                    failure(&format!("✘ {}: {err}", fixture_path.display()))
                );
                failed += 1;
            }
        }
    }
    drop(driver);
    cleanup_profile(&profile_dir, false);

    if failed == 0 {
        SuiteOutcome::Passed
    } else {
        SuiteOutcome::Failed(format!("{failed} of {} fixtures", fixtures.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_exe_path_joins_target_debug_examples_and_the_platform_exe_suffix() {
        let path = example_exe_path(Path::new("target"), "counter");
        assert_eq!(
            path,
            Path::new("target")
                .join("debug")
                .join("examples")
                .join(format!("counter{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn example_exe_path_respects_a_non_default_target_dir() {
        let path = example_exe_path(Path::new("/custom/target"), "counter");
        assert_eq!(
            path,
            Path::new("/custom/target")
                .join("debug")
                .join("examples")
                .join(format!("counter{}", std::env::consts::EXE_SUFFIX))
        );
    }

    #[test]
    fn exit_code_from_status_maps_a_successful_child_to_success() {
        // A trivial command that always succeeds, portable across the
        // platforms `std::process` supports.
        let status = ChildCommand::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/C", "exit 0"][..]
            } else {
                &[][..]
            })
            .status()
            .expect("a trivial command should always be spawnable");
        assert_eq!(exit_code_from_status(status), ExitCode::SUCCESS);
    }

    #[test]
    fn exit_code_from_status_maps_a_failing_child_to_failure() {
        let status = ChildCommand::new(if cfg!(windows) { "cmd" } else { "false" })
            .args(if cfg!(windows) {
                &["/C", "exit 1"][..]
            } else {
                &[][..]
            })
            .status()
            .expect("a trivial command should always be spawnable");
        assert_eq!(exit_code_from_status(status), ExitCode::FAILURE);
    }
}
