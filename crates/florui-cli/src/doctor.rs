//! `florui doctor`: real, observed evidence about the local environment
//! and (when invoked inside one) the current project — never a readiness
//! claim for a capability nothing here actually probed. Every check
//! carries a stable id, so a script or CI job can key off one specific
//! result instead of parsing human prose.
//!
//! Graphics and window-presentation probing (`--graphics`/`--presentation`)
//! run in a separate, bounded child process (this same `florui` binary,
//! re-invoked with a hidden internal flag) rather than in this one: a
//! crashed or hung graphics driver then only takes down that child, and a
//! probe that never responds is killed after a fixed timeout instead of
//! hanging the whole report. `--presentation` implies `--graphics` (it
//! cannot mean anything without a real adapter first) and, on success,
//! opens one clearly-titled temporary window just long enough to ask
//! `florui_platform::gpu::GpuPresenter` what it actually negotiated
//! before closing it again — this reports which presentation path came
//! up (and its real surface format/alpha mode), not a claim that a human
//! has visually confirmed the result; visual confirmation is a stronger,
//! separate kind of evidence this command does not produce.

use std::env;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command as ChildCommand, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

mod artifacts;

const SCHEMA_VERSION: u32 = 1;
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// The hidden flag this binary re-invokes itself with to run each bounded
/// probe as a real child process — see this module's own doc.
pub const INTERNAL_GRAPHICS_PROBE_FLAG: &str = "--internal-graphics-probe";
pub const INTERNAL_PRESENTATION_PROBE_FLAG: &str = "--internal-presentation-probe";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Warning,
    Fail,
    Unknown,
    Skipped,
    NotApplicable,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warning => "warning",
            Status::Fail => "fail",
            Status::Unknown => "unknown",
            Status::Skipped => "skipped",
            Status::NotApplicable => "not_applicable",
        }
    }

    /// Whether this status counts as "applicable" for strict mode's own
    /// extended failure condition — a check that never ran for a real
    /// reason (skipped, not applicable) is not the same as one that ran
    /// and came back uncertain.
    fn is_applicable(self) -> bool {
        !matches!(self, Status::Skipped | Status::NotApplicable)
    }
}

#[derive(Debug)]
struct Check {
    id: &'static str,
    category: &'static str,
    status: Status,
    required: bool,
    observed: Option<String>,
    expected: Option<String>,
    evidence: String,
    reason: Option<String>,
    remediation: Option<String>,
}

impl Check {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "category": self.category,
            "status": self.status.as_str(),
            "required": self.required,
            "observed": self.observed,
            "expected": self.expected,
            "evidence": self.evidence,
            "reason": self.reason,
            "remediation": self.remediation,
        })
    }
}

pub struct Options {
    pub target: String,
    pub package: Option<String>,
    pub environment: Option<String>,
    pub graphics: bool,
    pub presentation: bool,
    pub distribution: bool,
    /// A staged native build output to check, never to build.
    pub artifacts: Option<std::path::PathBuf>,
    pub json: bool,
    pub strict: bool,
}

pub fn run(options: Options) -> ExitCode {
    let started = Instant::now();
    let graphics = options.graphics || options.presentation;
    let mut checks = Vec::new();

    checks.extend(environment_checks());
    checks.extend(project_checks(
        options.package.as_deref(),
        options.environment.as_deref(),
        &options.target,
    ));

    match options.target.as_str() {
        "native" => {
            if graphics {
                let probe = run_bounded_probe(INTERNAL_GRAPHICS_PROBE_FLAG);
                checks.push(graphics_check(&probe));
                if options.presentation {
                    checks.push(presentation_check(
                        &probe,
                        &run_bounded_probe(INTERNAL_PRESENTATION_PROBE_FLAG),
                    ));
                }
            }
            if let Some(given) = &options.artifacts {
                match env::current_dir() {
                    Ok(cwd) => checks.extend(artifacts::checks(
                        &artifacts::resolve_dir(&cwd, given),
                        &cwd,
                        options.package.as_deref(),
                        options.environment.as_deref(),
                    )),
                    Err(error) => checks.push(check_unknown(
                        "artifacts.report",
                        format!("could not determine the current directory: {error}"),
                    )),
                }
            }
            if options.distribution {
                checks.push(Check {
                    id: "distribution.packaging",
                    category: "distribution",
                    status: Status::Unknown,
                    required: false,
                    observed: None,
                    expected: None,
                    evidence: "no packaging/signing pipeline exists in this CLI yet".to_string(),
                    reason: Some(
                        "distribution diagnostics are not implemented yet, not a property of \
                         this environment"
                            .to_string(),
                    ),
                    remediation: None,
                });
            }
        }
        "web" => {
            checks.push(Check {
                id: "target.web",
                category: "target",
                status: Status::Unknown,
                required: false,
                observed: None,
                expected: None,
                evidence: "the Web target has no build pipeline in this CLI yet".to_string(),
                reason: Some("the Web target is not implemented yet".to_string()),
                remediation: None,
            });
            if graphics {
                checks.push(Check {
                    id: "graphics.unsupported_target",
                    category: "graphics",
                    status: Status::NotApplicable,
                    required: false,
                    observed: None,
                    expected: None,
                    evidence: "graphics/presentation probing is only implemented for --target \
                                native"
                        .to_string(),
                    reason: None,
                    remediation: None,
                });
            }
            if options.artifacts.is_some() {
                checks.push(Check {
                    id: "artifacts.unsupported_target",
                    category: "artifacts",
                    status: Status::Skipped,
                    required: false,
                    observed: None,
                    expected: None,
                    evidence: "output checks are only implemented for --target native; Web output \
                               validation arrives with the Web target"
                        .to_string(),
                    reason: None,
                    remediation: None,
                });
            }
            if options.distribution {
                // A real request this build genuinely cannot honor yet for
                // this target, distinct from `NotApplicable` (which means
                // the check itself makes no sense here) -- distribution
                // diagnostics do make sense for a Web target eventually,
                // this CLI just doesn't implement them yet.
                checks.push(Check {
                    id: "distribution.unsupported_target",
                    category: "distribution",
                    status: Status::Skipped,
                    required: false,
                    observed: None,
                    expected: None,
                    evidence: "distribution diagnostics are only implemented for --target native"
                        .to_string(),
                    reason: None,
                    remediation: None,
                });
            }
        }
        other => {
            checks.push(Check {
                id: "target.unknown",
                category: "target",
                status: Status::Fail,
                required: true,
                observed: Some(other.to_string()),
                expected: Some("native or web".to_string()),
                evidence: format!("--target {other} is not a recognized target selector"),
                reason: None,
                remediation: Some("pass --target native or --target web".to_string()),
            });
        }
    }

    report(
        checks,
        options.target,
        started,
        options.json,
        options.strict,
    )
}

fn environment_checks() -> Vec<Check> {
    vec![
        tool_version_check("rust.cargo", "cargo", true),
        tool_version_check("rust.rustc", "rustc", true),
        tool_version_check("rust.rustfmt", "rustfmt", false),
        reduced_motion_check(),
        single_instance_capability_check(),
        window_persistence_capability_check(),
    ]
}

/// A dedicated, throwaway identifier -- never a real project's own -- so
/// this probe can never collide with (or interfere with) an actually
/// running instance of anything.
const CAPABILITY_PROBE_IDENTIFIER: &str = "florui-doctor-probe";

/// Real evidence (a real named mutex plus a security-descriptor-restricted
/// named pipe, both created and released) that single-instance activation
/// could actually work on this machine -- not an assumption from the OS
/// merely being Windows. Machine-level, like [`reduced_motion_check`]:
/// independent of whether the current project even opts into it.
fn single_instance_capability_check() -> Check {
    if !cfg!(target_os = "windows") {
        return Check {
            id: "environment.single_instance_capability",
            category: "environment",
            status: Status::NotApplicable,
            required: false,
            observed: None,
            expected: None,
            evidence: "no equivalent single-instance IPC mechanism exists for this platform yet"
                .to_string(),
            reason: None,
            remediation: None,
        };
    }
    if florui_platform::probe_single_instance_capability() {
        Check {
            id: "environment.single_instance_capability",
            category: "environment",
            status: Status::Pass,
            required: false,
            observed: Some("true".to_string()),
            expected: None,
            evidence: "a real named mutex and a security-descriptor-restricted named pipe were \
                       both created and released successfully"
                .to_string(),
            reason: None,
            remediation: None,
        }
    } else {
        Check {
            id: "environment.single_instance_capability",
            category: "environment",
            status: Status::Fail,
            required: false,
            observed: Some("false".to_string()),
            expected: None,
            evidence: "could not create the real named mutex and/or security-descriptor-\
                       restricted named pipe single-instance activation depends on"
                .to_string(),
            reason: None,
            remediation: Some(
                "check local Windows security policy/permissions for named-object creation"
                    .to_string(),
            ),
        }
    }
}

/// Real evidence (a window-state file actually written to and removed
/// from the OS application-data location) that window persistence could
/// actually work on this machine -- not an assumption from `LOCALAPPDATA`
/// merely being set. Machine-level, like [`reduced_motion_check`]:
/// independent of whether the current project even opts into it.
fn window_persistence_capability_check() -> Check {
    if !cfg!(target_os = "windows") {
        return Check {
            id: "environment.window_persistence_capability",
            category: "environment",
            status: Status::NotApplicable,
            required: false,
            observed: None,
            expected: None,
            evidence: "no OS application-data location convention is wired up for this platform \
                       yet"
            .to_string(),
            reason: None,
            remediation: None,
        };
    }
    if florui_platform::probe_persistence_capability(CAPABILITY_PROBE_IDENTIFIER) {
        Check {
            id: "environment.window_persistence_capability",
            category: "environment",
            status: Status::Pass,
            required: false,
            observed: Some("true".to_string()),
            expected: None,
            evidence: "a window-state file was written to and removed from the OS \
                       application-data location successfully"
                .to_string(),
            reason: None,
            remediation: None,
        }
    } else {
        Check {
            id: "environment.window_persistence_capability",
            category: "environment",
            status: Status::Fail,
            required: false,
            observed: Some("false".to_string()),
            expected: None,
            evidence: "could not write a window-state file to the OS application-data location \
                       (LOCALAPPDATA) window persistence depends on"
                .to_string(),
            reason: None,
            remediation: Some(
                "check that LOCALAPPDATA is set and writable for the current user".to_string(),
            ),
        }
    }
}

/// A benign, synchronous, side-effect-free global-state read (unlike
/// `graphics_check`/`presentation_check`, which run a real GPU/window
/// creation probe in a bounded subprocess because a bad driver can crash
/// the whole process) -- reported in-process, no subprocess needed.
fn reduced_motion_check() -> Check {
    if !cfg!(target_os = "windows") {
        return Check {
            id: "environment.reduced_motion",
            category: "environment",
            status: Status::NotApplicable,
            required: false,
            observed: None,
            expected: None,
            evidence: "no OS reduced-motion integration exists for this platform yet".to_string(),
            reason: None,
            remediation: None,
        };
    }
    let prefers_reduced = florui_platform::accessibility::prefers_reduced_motion();
    Check {
        id: "environment.reduced_motion",
        category: "environment",
        status: Status::Pass,
        required: false,
        observed: Some(prefers_reduced.to_string()),
        expected: None,
        evidence: format!(
            "Windows' \"Show animations in Windows\" setting reports reduced motion: \
             {prefers_reduced}"
        ),
        reason: None,
        remediation: None,
    }
}

fn tool_version_check(id: &'static str, program: &str, required: bool) -> Check {
    match ChildCommand::new(program).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check {
                id,
                category: "toolchain",
                status: Status::Pass,
                required,
                observed: Some(version.clone()),
                expected: None,
                evidence: format!("`{program} --version` succeeded: {version}"),
                reason: None,
                remediation: None,
            }
        }
        Ok(output) => Check {
            id,
            category: "toolchain",
            status: if required {
                Status::Fail
            } else {
                Status::Warning
            },
            required,
            observed: None,
            expected: None,
            evidence: format!(
                "`{program} --version` exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            reason: None,
            remediation: Some(format!("install or repair {program}")),
        },
        Err(error) => Check {
            id,
            category: "toolchain",
            status: if required {
                Status::Fail
            } else {
                Status::Warning
            },
            required,
            observed: None,
            expected: None,
            evidence: format!("could not run `{program}`: {error}"),
            reason: None,
            remediation: Some(format!("install {program} and ensure it is on PATH")),
        },
    }
}

/// Resolves the current project via `florui_config` (the one resolver
/// shared with `florui dev`, replacing what used to be two independent
/// `cargo metadata` parsers here and in `main.rs`) and turns both the
/// Cargo-level facts and the `florui.config.toml` resolution into doctor
/// checks. Outside a resolvable project, returns a single
/// `NotApplicable` check rather than failing outright -- a doctor run
/// should still report environment checks in that case.
fn project_checks(package: Option<&str>, environment: Option<&str>, target: &str) -> Vec<Check> {
    match env::current_dir() {
        Ok(cwd) => project_checks_at(&cwd, package, environment, target),
        Err(error) => vec![check_unknown(
            "project.resolved",
            format!("could not determine the current directory: {error}"),
        )],
    }
}

/// The actual logic behind [`project_checks`], with `cwd` explicit rather
/// than inherited from the process -- so it's testable against a real
/// temporary Cargo workspace without mutating global process state, the
/// same reasoning `florui_config::resolve_cargo_project` itself already
/// applies.
fn project_checks_at(
    cwd: &Path,
    package: Option<&str>,
    environment: Option<&str>,
    target: &str,
) -> Vec<Check> {
    let facts = match florui_config::resolve_cargo_project(cwd, package) {
        Ok(facts) => facts,
        // An explicit --package that doesn't exist is a real user mistake,
        // worth surfacing as a failure rather than folded into "outside a
        // project".
        Err(error @ florui_config::ProjectResolutionError::UnknownPackage { .. }) => {
            return vec![Check {
                id: "project.resolved",
                category: "project",
                status: Status::Fail,
                required: false,
                observed: None,
                expected: None,
                evidence: error.to_string(),
                reason: None,
                remediation: Some(
                    "check --package against the workspace's real package names".to_string(),
                ),
            }];
        }
        // Everything else -- an ambiguous/unresolvable root, or `cargo
        // metadata` itself failing because no Cargo.toml exists anywhere
        // above `cwd` -- is the same "not inside a resolvable Cargo
        // project" case the doctor documents: report environment checks and
        // mark project checks not applicable, not a failure.
        Err(_) => {
            return vec![Check {
                id: "project.resolved",
                category: "project",
                status: Status::NotApplicable,
                required: false,
                observed: None,
                expected: None,
                evidence: "not invoked inside a resolvable Cargo project".to_string(),
                reason: None,
                remediation: None,
            }];
        }
    };

    let mut checks = vec![Check {
        id: "project.manifest_readable",
        category: "project",
        status: Status::Pass,
        required: false,
        observed: Some(display_redacted(&facts.manifest_path)),
        expected: None,
        evidence: "cargo metadata resolved this project's own Cargo.toml".to_string(),
        reason: None,
        remediation: None,
    }];

    let lockfile = facts.workspace_root.join("Cargo.lock");
    checks.push(if lockfile.is_file() {
        Check {
            id: "project.lockfile_readable",
            category: "project",
            status: Status::Pass,
            required: false,
            observed: Some(display_redacted(&lockfile)),
            expected: None,
            evidence: "Cargo.lock exists at the workspace root".to_string(),
            reason: None,
            remediation: None,
        }
    } else {
        Check {
            id: "project.lockfile_readable",
            category: "project",
            status: Status::Warning,
            required: false,
            observed: None,
            expected: Some(display_redacted(&lockfile)),
            evidence: "no Cargo.lock at the workspace root".to_string(),
            reason: None,
            remediation: Some(
                "run `cargo metadata` or `cargo build` once to generate it".to_string(),
            ),
        }
    });

    let config_target = match target {
        "native" => Some(florui_config::Target::Native),
        "web" => Some(florui_config::Target::Web),
        _ => None,
    };
    let config_exists = florui_config::config_file_path(&facts.package_root).exists();
    let selection = florui_config::EnvironmentSelection {
        name: environment.unwrap_or("production"),
        explicit: environment.is_some(),
    };
    let resolution = florui_config::resolve(&facts, config_target, Some(selection));

    checks.push(dev_example_target_check(&facts, &resolution));
    checks.push(config_discovered_check(config_exists));
    checks.push(config_schema_valid_check(config_exists, &resolution));
    checks.push(config_schema_version_check(config_exists, &resolution));
    checks.push(config_legacy_migration_check(config_exists, &resolution));
    checks.push(config_identity_check(config_exists, &resolution));
    checks.push(config_window_size_check(config_exists, &resolution));
    checks.push(config_environment_selected_check(
        Some(selection),
        &resolution,
    ));
    checks.push(config_environment_known_check(&resolution));
    checks.push(config_identity_collision_check(config_exists, &resolution));
    checks.push(config_window_persistence_identity_check(
        config_exists,
        &resolution,
    ));
    checks.push(config_single_instance_identity_check(
        config_exists,
        &resolution,
    ));
    checks.extend(config_icon_asset_checks(
        config_exists,
        config_target,
        &resolution,
    ));

    checks
}

fn check_unknown(id: &'static str, evidence: String) -> Check {
    Check {
        id,
        category: "config",
        status: Status::Unknown,
        required: false,
        observed: None,
        expected: None,
        evidence,
        reason: None,
        remediation: None,
    }
}

fn check_not_applicable(id: &'static str, evidence: &str) -> Check {
    Check {
        id,
        category: "config",
        status: Status::NotApplicable,
        required: false,
        observed: None,
        expected: None,
        evidence: evidence.to_string(),
        reason: None,
        remediation: None,
    }
}

fn check_pass(id: &'static str, evidence: String) -> Check {
    Check {
        id,
        category: "config",
        status: Status::Pass,
        required: false,
        observed: None,
        expected: None,
        evidence,
        reason: None,
        remediation: None,
    }
}

/// Whether `florui.config.toml` failed to even parse -- everything that
/// depends on its content downstream is `Unknown`, not `Fail`, in that
/// case (see this module's own doc for why: a schema-shape problem and an
/// independent semantic one, like an invalid window size, must not be
/// conflated into the same failure).
fn parse_failed(
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> bool {
    matches!(resolution, Err(florui_config::ConfigError::Toml { .. }))
}

fn semantic_errors(
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> &[florui_config::SemanticConfigError] {
    match resolution {
        Err(florui_config::ConfigError::Semantic(errors)) => errors,
        _ => &[],
    }
}

fn dev_example_target_check(
    facts: &florui_config::CargoProjectFacts,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    // Independent of the rest of config validity -- a broken
    // florui.config.toml elsewhere must not hide an otherwise-checkable
    // legacy dev example, so this falls back to the raw legacy value
    // when resolution as a whole failed.
    let example = match resolution {
        Ok(resolution) => resolution.config.dev.example.clone(),
        Err(_) => facts
            .legacy_dev_example
            .as_ref()
            .map(|located| located.value.clone()),
    };
    match example {
        None => check_not_applicable(
            "project.dev_example_target_exists",
            "no dev example declared in [package.metadata.florui.dev] or florui.config.toml [dev]",
        ),
        Some(example) if facts.example_targets.contains(&example) => check_pass(
            "project.dev_example_target_exists",
            format!("cargo example target `{example}` exists"),
        ),
        Some(example) => Check {
            id: "project.dev_example_target_exists",
            category: "project",
            status: Status::Fail,
            required: false,
            observed: Some(example.clone()),
            expected: Some("a matching [[example]] target".to_string()),
            evidence: format!(
                "the declared dev example \"{example}\" has no matching [[example]] target"
            ),
            reason: None,
            remediation: Some(format!(
                "add an examples/{example}.rs (or fix the declared name)"
            )),
        },
    }
}

fn config_discovered_check(config_exists: bool) -> Check {
    check_pass(
        "config.discovered",
        if config_exists {
            "florui.config.toml found at the resolved package root".to_string()
        } else {
            "no florui.config.toml at the resolved package root -- using Cargo/legacy/built-in \
             defaults"
                .to_string()
        },
    )
}

fn config_schema_valid_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if !config_exists {
        return check_not_applicable("config.schema_valid", "no florui.config.toml to validate");
    }
    match resolution {
        Err(florui_config::ConfigError::Toml { .. }) => {
            let error = resolution.as_ref().unwrap_err();
            Check {
                id: "config.schema_valid",
                category: "config",
                status: Status::Fail,
                required: false,
                observed: None,
                expected: None,
                evidence: error.to_string(),
                reason: None,
                remediation: Some("fix the reported location in florui.config.toml".to_string()),
            }
        }
        _ => check_pass(
            "config.schema_valid",
            "florui.config.toml parses as valid TOML matching the schema shape".to_string(),
        ),
    }
}

fn config_schema_version_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if !config_exists {
        return check_not_applicable(
            "config.schema_version_supported",
            "no florui.config.toml to check",
        );
    }
    if parse_failed(resolution) {
        return check_unknown(
            "config.schema_version_supported",
            "not evaluated: florui.config.toml failed to parse".to_string(),
        );
    }
    match semantic_errors(resolution).iter().find(|error| {
        matches!(
            error,
            florui_config::SemanticConfigError::UnsupportedSchemaVersion { .. }
        )
    }) {
        Some(error) => Check {
            id: "config.schema_version_supported",
            category: "config",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.to_string(),
            reason: None,
            remediation: None,
        },
        None => check_pass(
            "config.schema_version_supported",
            "schema_version is supported by this build".to_string(),
        ),
    }
}

fn config_legacy_migration_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if parse_failed(resolution) {
        return check_unknown(
            "config.legacy_migration_conflict",
            "not evaluated: florui.config.toml failed to parse".to_string(),
        );
    }
    match semantic_errors(resolution).iter().find(|error| {
        matches!(
            error,
            florui_config::SemanticConfigError::LegacyNewDuplicate { .. }
        )
    }) {
        Some(error) => Check {
            id: "config.legacy_migration_conflict",
            category: "config",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.to_string(),
            reason: None,
            remediation: Some(
                "remove the legacy [package.metadata.florui.dev] key once migrated".to_string(),
            ),
        },
        None if !config_exists => check_not_applicable(
            "config.legacy_migration_conflict",
            "no florui.config.toml to conflict with the legacy key",
        ),
        None => check_pass(
            "config.legacy_migration_conflict",
            "no conflicting legacy and new definitions".to_string(),
        ),
    }
}

fn config_identity_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if !config_exists {
        return check_not_applicable("config.identity_resolved", "no florui.config.toml to check");
    }
    if parse_failed(resolution) {
        return check_unknown(
            "config.identity_resolved",
            "not evaluated: florui.config.toml failed to parse".to_string(),
        );
    }
    let identity_errors: Vec<&florui_config::SemanticConfigError> = semantic_errors(resolution)
        .iter()
        .filter(|error| {
            matches!(
                error,
                florui_config::SemanticConfigError::InvalidAppVersion { .. }
                    | florui_config::SemanticConfigError::WorkspaceVersionNotInherited { .. }
            )
        })
        .collect();
    match identity_errors.first() {
        Some(error) => Check {
            id: "config.identity_resolved",
            category: "config",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.to_string(),
            reason: None,
            remediation: None,
        },
        None => check_pass(
            "config.identity_resolved",
            "app identity (name/version) resolved".to_string(),
        ),
    }
}

fn config_window_size_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if !config_exists {
        return check_not_applicable("config.window_size_valid", "no florui.config.toml to check");
    }
    if parse_failed(resolution) {
        return check_unknown(
            "config.window_size_valid",
            "not evaluated: florui.config.toml failed to parse".to_string(),
        );
    }
    match semantic_errors(resolution).iter().find(|error| {
        matches!(
            error,
            florui_config::SemanticConfigError::InvalidWindowSize { .. }
        )
    }) {
        Some(error) => Check {
            id: "config.window_size_valid",
            category: "config",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.to_string(),
            reason: None,
            remediation: None,
        },
        None => check_pass(
            "config.window_size_valid",
            "window sizes (if declared) are finite, positive, and min/max-consistent".to_string(),
        ),
    }
}

/// Always Pass -- purely informational, reporting what environment
/// selection was actually requested and (when resolution succeeded)
/// whether a declared overlay actually applied. `config.environment_known`
/// and `config.identity_collision` carry the pass/fail verdicts.
fn config_environment_selected_check(
    selection: Option<florui_config::EnvironmentSelection<'_>>,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    let evidence = match resolution {
        Ok(resolution) => match &resolution.environment.selected {
            Some(name) if resolution.environment.overlay_applied => {
                format!("environment \"{name}\" selected ([environments.{name}] applied)")
            }
            Some(name) => format!(
                "environment \"{name}\" selected (using base configuration -- not declared, \
                 or no app overlay)"
            ),
            None => "no environment selected -- using base configuration".to_string(),
        },
        Err(_) => match selection {
            Some(selection) => format!("environment \"{}\" requested", selection.name),
            None => "no environment selected".to_string(),
        },
    };
    check_pass("config.environment_selected", evidence)
}

fn config_environment_known_check(
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if parse_failed(resolution) {
        return check_unknown(
            "config.environment_known",
            "not evaluated: florui.config.toml failed to parse".to_string(),
        );
    }
    match semantic_errors(resolution).iter().find(|error| {
        matches!(
            error,
            florui_config::SemanticConfigError::UnknownEnvironment { .. }
        )
    }) {
        Some(error) => Check {
            id: "config.environment_known",
            category: "config",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.to_string(),
            reason: None,
            remediation: Some(
                "pass --environment with a name declared under [environments], or omit it"
                    .to_string(),
            ),
        },
        None => check_pass(
            "config.environment_known",
            "the requested environment (if any) is declared".to_string(),
        ),
    }
}

fn config_identity_collision_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    if !config_exists {
        return check_not_applicable(
            "config.identity_collision",
            "no florui.config.toml to check",
        );
    }
    if parse_failed(resolution) {
        return check_unknown(
            "config.identity_collision",
            "not evaluated: florui.config.toml failed to parse".to_string(),
        );
    }
    match semantic_errors(resolution).iter().find(|error| {
        matches!(
            error,
            florui_config::SemanticConfigError::DuplicateEnvironmentIdentifier { .. }
        )
    }) {
        Some(error) => Check {
            id: "config.identity_collision",
            category: "config",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.to_string(),
            reason: None,
            remediation: Some(
                "give each declared environment (and base configuration) a distinct \
                 app.identifier"
                    .to_string(),
            ),
        },
        None => check_pass(
            "config.identity_collision",
            "no declared environment shares app.identifier with another or with base \
             configuration"
                .to_string(),
        ),
    }
}

/// `[window.persistence]`/`[app.activation].single_instance` both key
/// their real OS-side state (a `LOCALAPPDATA` path, a named mutex/pipe)
/// off `app.identifier` -- enabling either one without also setting an
/// identifier is not a schema-level error (both fields are independently
/// optional), but it does mean the feature silently never does anything,
/// exactly how `WindowPersistence`/`SingleInstance`'s own app-side
/// resolution helpers already degrade (`None`, no diagnostic). This is
/// the one place that silent gap becomes visible.
fn config_window_persistence_identity_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    const ID: &str = "config.window_persistence_identity";
    if !config_exists {
        return check_not_applicable(ID, "no florui.config.toml to check");
    }
    let resolution = match resolution {
        Ok(resolution) => resolution,
        Err(_) => {
            return check_unknown(
                ID,
                "not evaluated: florui.config.toml failed to resolve".to_string(),
            );
        }
    };
    if !resolution.config.window.persistence.enabled {
        return check_not_applicable(ID, "[window.persistence] is not enabled");
    }
    match &resolution.config.app.identifier {
        Some(identifier) => check_pass(
            ID,
            format!("window persistence is enabled with app.identifier \"{identifier}\""),
        ),
        None => Check {
            id: ID,
            category: "config",
            status: Status::Warning,
            required: false,
            observed: None,
            expected: Some("app.identifier set".to_string()),
            evidence: "[window.persistence] is enabled but app.identifier is unset -- \
                       persistence silently never saves or restores anything without it"
                .to_string(),
            reason: None,
            remediation: Some("set app.identifier in florui.config.toml".to_string()),
        },
    }
}

/// See [`config_window_persistence_identity_check`]'s own doc -- the same
/// silent-no-op gap, for `[app.activation].single_instance` instead.
fn config_single_instance_identity_check(
    config_exists: bool,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Check {
    const ID: &str = "config.single_instance_identity";
    if !config_exists {
        return check_not_applicable(ID, "no florui.config.toml to check");
    }
    let resolution = match resolution {
        Ok(resolution) => resolution,
        Err(_) => {
            return check_unknown(
                ID,
                "not evaluated: florui.config.toml failed to resolve".to_string(),
            );
        }
    };
    if !resolution.config.app.activation.single_instance {
        return check_not_applicable(ID, "[app.activation].single_instance is not enabled");
    }
    match &resolution.config.app.identifier {
        Some(identifier) => check_pass(
            ID,
            format!("single-instance activation is enabled with app.identifier \"{identifier}\""),
        ),
        None => Check {
            id: ID,
            category: "config",
            status: Status::Warning,
            required: false,
            observed: None,
            expected: Some("app.identifier set".to_string()),
            evidence: "[app.activation].single_instance is enabled but app.identifier is unset \
                       -- single-instance enforcement cannot derive a mutex/pipe name without it"
                .to_string(),
            reason: None,
            remediation: Some("set app.identifier in florui.config.toml".to_string()),
        },
    }
}

/// One check per icon field, scoped to the selected target -- `resolve()`
/// itself already skips the filesystem check entirely outside
/// `Target::Native` (see `florui_config`'s own doc), so this only ever
/// reports something other than not-applicable/unknown under `--target
/// native`.
fn config_icon_asset_checks(
    config_exists: bool,
    target: Option<florui_config::Target>,
    resolution: &Result<florui_config::Resolution, florui_config::ConfigError>,
) -> Vec<Check> {
    const FIELDS: [(&str, &str); 4] = [
        ("source", "config.icon_asset_present.source"),
        ("windows", "config.icon_asset_present.windows"),
        ("macos", "config.icon_asset_present.macos"),
        ("linux", "config.icon_asset_present.linux"),
    ];

    FIELDS
        .into_iter()
        .map(|(field_name, id)| {
            if !config_exists {
                return check_not_applicable(id, "no florui.config.toml to check");
            }
            if parse_failed(resolution) {
                return check_unknown(
                    id,
                    "not evaluated: florui.config.toml failed to parse".to_string(),
                );
            }
            if !semantic_errors(resolution).is_empty() {
                return check_unknown(
                    id,
                    "not evaluated: florui.config.toml has unrelated semantic errors".to_string(),
                );
            }
            let Ok(resolution) = resolution else {
                return check_unknown(id, "not evaluated".to_string());
            };
            if !matches!(target, Some(florui_config::Target::Native)) {
                return check_not_applicable(
                    id,
                    "asset presence is only checked for --target native",
                );
            }
            let path = match field_name {
                "source" => &resolution.config.app.icons.source,
                "windows" => &resolution.config.app.icons.windows,
                "macos" => &resolution.config.app.icons.macos,
                _ => &resolution.config.app.icons.linux,
            };
            let Some(path) = path else {
                return check_not_applicable(id, "not declared in [app.icons]");
            };
            match resolution
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.code == id)
            {
                Some(diagnostic) => Check {
                    id,
                    category: "config",
                    status: Status::Warning,
                    required: false,
                    observed: Some(display_redacted(path)),
                    expected: None,
                    evidence: diagnostic.message.clone(),
                    reason: None,
                    remediation: Some(format!("add the missing asset at {}", path.display())),
                },
                None => check_pass(id, format!("{} exists", display_redacted(path))),
            }
        })
        .collect()
}

/// Replaces the user's home directory prefix with `~` — this crate's own
/// small share of the redaction contract every check's evidence and
/// observed/expected fields follow, so a doctor report is safe to paste
/// into an issue without hand-editing paths first.
fn display_redacted(path: &Path) -> String {
    let rendered = path.display().to_string();
    match dirs_home() {
        Some(home) if rendered.starts_with(&home) => {
            format!("~{}", &rendered[home.len()..])
        }
        _ => rendered,
    }
}

fn dirs_home() -> Option<String> {
    env::var("USERPROFILE").or_else(|_| env::var("HOME")).ok()
}

/// Runs this same binary again with `flag`, bounded to [`PROBE_TIMEOUT`] —
/// killed and reported as timed out rather than left to hang the whole
/// report. Returns the probe's own parsed JSON on a clean, successful
/// exit; `Err` carries a human-readable reason otherwise (missing
/// adapter, probe panic, timeout), which the caller turns into real
/// check evidence rather than a raw process error.
fn run_bounded_probe(flag: &str) -> Result<Value, String> {
    let exe =
        env::current_exe().map_err(|error| format!("could not find own executable: {error}"))?;
    let mut child = ChildCommand::new(exe)
        .arg(flag)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not spawn probe process: {error}"))?;

    let (tx, rx) = mpsc::channel();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    thread::spawn(move || {
        use std::io::Read;
        let mut out = String::new();
        let mut err = String::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_string(&mut out);
        }
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_string(&mut err);
        }
        let _ = tx.send((out, err));
    });

    let status = match child.try_wait() {
        Ok(Some(status)) => Some(status),
        Ok(None) => wait_bounded(&mut child, PROBE_TIMEOUT),
        Err(error) => return Err(format!("could not check probe process status: {error}")),
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "probe process did not finish within {PROBE_TIMEOUT:?} and was killed"
        ));
    };

    let (stdout, stderr) = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    if !status.success() {
        return Err(format!(
            "probe process exited with {status}: {}",
            stderr.trim()
        ));
    }
    serde_json::from_str(stdout.trim())
        .map_err(|error| format!("probe process produced unparsable output: {error}"))
}

fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
}

fn graphics_check(probe: &Result<Value, String>) -> Check {
    match probe {
        Ok(evidence) => Check {
            id: "graphics.adapter",
            category: "graphics",
            status: Status::Pass,
            required: false,
            observed: evidence
                .get("adapter_name")
                .and_then(Value::as_str)
                .map(str::to_owned),
            expected: None,
            evidence: format!(
                "backend={} adapter={} device_type={} driver={} driver_info={}",
                evidence
                    .get("backend")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                evidence
                    .get("adapter_name")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                evidence
                    .get("device_type")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                evidence
                    .get("driver")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                evidence
                    .get("driver_info")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
            ),
            reason: None,
            remediation: None,
        },
        Err(error) => Check {
            id: "graphics.adapter",
            category: "graphics",
            status: Status::Fail,
            required: false,
            observed: None,
            expected: None,
            evidence: error.clone(),
            reason: Some(
                "no usable GPU adapter came up -- florui_platform's own softbuffer CPU path \
                 remains the fallback"
                    .to_string(),
            ),
            remediation: Some("install or update graphics drivers".to_string()),
        },
    }
}

fn presentation_check(
    graphics_probe: &Result<Value, String>,
    probe: &Result<Value, String>,
) -> Check {
    match probe {
        Ok(evidence) => {
            let capability = evidence
                .get("capability")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let format = evidence
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let alpha_mode = evidence
                .get("alpha_mode")
                .and_then(Value::as_str)
                .unwrap_or("?");
            // A real adapter existing but no presenter coming up for a real
            // window is worth flagging even though it is not, on its own,
            // an environment failure -- see this module's own doc.
            let status = if capability == "cpu" && graphics_probe.is_ok() {
                Status::Warning
            } else {
                Status::Pass
            };
            let reason = (capability == "cpu" && graphics_probe.is_ok()).then(|| {
                "a real graphics adapter exists, but no GPU presenter could be created for a \
                 real window; presentation falls back to the CPU/softbuffer path"
                    .to_string()
            });
            Check {
                id: "presentation.capability",
                category: "presentation",
                status,
                required: false,
                observed: Some(capability.to_string()),
                expected: None,
                evidence: format!(
                    "capability={capability} format={format} alpha_mode={alpha_mode} -- a \
                     successful negotiation, not a human-verified visual confirmation (see \
                     this command's own doc)"
                ),
                reason,
                remediation: None,
            }
        }
        Err(error) => Check {
            id: "presentation.capability",
            category: "presentation",
            status: Status::Unknown,
            required: false,
            observed: None,
            expected: None,
            evidence: error.clone(),
            reason: Some("could not evaluate real window presentation".to_string()),
            remediation: None,
        },
    }
}

/// Whether `checks` should make `florui doctor` exit non-zero: a required
/// check failed or came back unverified, or (only under `strict`) any
/// applicable check came back as a warning or unknown result — pulled
/// out as its own pure function so this decision is directly testable
/// against a hand-built check list, not just observable through a real
/// process's exit code.
fn should_fail(checks: &[Check], strict: bool) -> bool {
    let required_failed = checks
        .iter()
        .any(|check| check.required && check.status == Status::Fail);
    let required_unverified = checks
        .iter()
        .any(|check| check.required && check.status == Status::Unknown);
    let strict_triggered = strict
        && checks.iter().any(|check| {
            check.status.is_applicable()
                && matches!(check.status, Status::Warning | Status::Unknown)
        });
    required_failed || required_unverified || strict_triggered
}

fn report(
    checks: Vec<Check>,
    target: String,
    started: Instant,
    json: bool,
    strict: bool,
) -> ExitCode {
    let failed = should_fail(&checks, strict);

    if json {
        let generated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let document = json!({
            "schema_version": SCHEMA_VERSION,
            "florui_version": env!("CARGO_PKG_VERSION"),
            "target": target,
            "generated_at_unix": generated_at,
            "duration_ms": started.elapsed().as_millis(),
            "checks": checks.iter().map(Check::to_json).collect::<Vec<_>>(),
        });
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "{document}");
    } else {
        print_human(&checks);
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_human(checks: &[Check]) {
    for check in checks {
        let marker = match check.status {
            Status::Pass => florui_devtools::diagnostics::success("PASS"),
            Status::Warning => florui_devtools::diagnostics::warning("WARN"),
            Status::Fail => florui_devtools::diagnostics::failure("FAIL"),
            Status::Unknown => florui_devtools::diagnostics::dim_text("UNKNOWN"),
            Status::Skipped => florui_devtools::diagnostics::dim_text("SKIPPED"),
            Status::NotApplicable => florui_devtools::diagnostics::dim_text("N/A"),
        };
        println!("{marker} {} -- {}", check.id, check.evidence);
        if let Some(reason) = &check.reason {
            println!("       {}", florui_devtools::diagnostics::dim_text(reason));
        }
        if let Some(remediation) = &check.remediation {
            println!(
                "       {} {remediation}",
                florui_devtools::diagnostics::dim_text("fix:")
            );
        }
    }
}

/// Runs inside the child process spawned by [`run_bounded_probe`] for
/// [`INTERNAL_GRAPHICS_PROBE_FLAG`] -- prints one line of JSON to stdout
/// and returns the process exit code; never called from a normal
/// `florui doctor` invocation itself.
pub fn run_internal_graphics_probe() -> ExitCode {
    match florui_platform::gpu::probe_graphics() {
        Ok(probe) => {
            let document = json!({
                "backend": probe.backend,
                "adapter_name": probe.adapter_name,
                "device_type": probe.device_type,
                "driver": probe.driver,
                "driver_info": probe.driver_info,
            });
            println!("{document}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Runs inside the child process spawned by [`run_bounded_probe`] for
/// [`INTERNAL_PRESENTATION_PROBE_FLAG`] -- opens one real, clearly
/// titled temporary window, asks a real [`florui_platform::gpu::GpuPresenter`]
/// what it negotiated, prints one line of JSON, and exits. Never called
/// from a normal `florui doctor` invocation itself.
pub fn run_internal_presentation_probe() -> ExitCode {
    use std::sync::Arc;

    use winit::application::ApplicationHandler;
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::window::{Window, WindowId};

    struct Probe {
        result: Option<Result<String, String>>,
    }

    impl ApplicationHandler for Probe {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let attrs = florui_platform::gpu::transparent_capable_attributes(
                Window::default_attributes().with_title("florui doctor -- presentation probe"),
            );
            let outcome = match event_loop.create_window(attrs) {
                Ok(window) => {
                    let window = Arc::new(window);
                    match florui_platform::gpu::GpuPresenter::try_new(window) {
                        Some(presenter) => Ok(json!({
                            "capability": match presenter.capability() {
                                florui_platform::gpu::PresentationCapability::GpuTransparent => "gpu_transparent",
                                florui_platform::gpu::PresentationCapability::GpuOpaque => "gpu_opaque",
                                florui_platform::gpu::PresentationCapability::Cpu => "cpu",
                            },
                            "format": format!("{:?}", presenter.format()),
                            "alpha_mode": format!("{:?}", presenter.alpha_mode()),
                        })
                        .to_string()),
                        None => Ok(json!({
                            "capability": "cpu",
                            "format": null,
                            "alpha_mode": null,
                        })
                        .to_string()),
                    }
                }
                Err(error) => Err(format!("could not create a probe window: {error}")),
            };
            self.result = Some(outcome);
            event_loop.exit();
        }

        fn window_event(
            &mut self,
            _event_loop: &ActiveEventLoop,
            _window_id: WindowId,
            _event: winit::event::WindowEvent,
        ) {
        }
    }

    let Ok(event_loop) = EventLoop::new() else {
        eprintln!("could not create an event loop for the presentation probe");
        return ExitCode::FAILURE;
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut probe = Probe { result: None };
    if event_loop.run_app(&mut probe).is_err() {
        eprintln!("presentation probe event loop failed");
        return ExitCode::FAILURE;
    }

    match probe.result {
        Some(Ok(json)) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Some(Err(error)) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
        None => {
            eprintln!("presentation probe exited without producing a result");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &'static str, status: Status, required: bool) -> Check {
        Check {
            id,
            category: "test",
            status,
            required,
            observed: None,
            expected: None,
            evidence: String::new(),
            reason: None,
            remediation: None,
        }
    }

    #[test]
    fn skipped_and_not_applicable_are_not_applicable() {
        assert!(!Status::Skipped.is_applicable());
        assert!(!Status::NotApplicable.is_applicable());
    }

    #[test]
    fn pass_warning_fail_and_unknown_are_applicable() {
        assert!(Status::Pass.is_applicable());
        assert!(Status::Warning.is_applicable());
        assert!(Status::Fail.is_applicable());
        assert!(Status::Unknown.is_applicable());
    }

    #[test]
    fn a_required_failure_fails_even_without_strict() {
        let checks = vec![check("a", Status::Fail, true)];
        assert!(should_fail(&checks, false));
    }

    #[test]
    fn a_required_unknown_result_fails_even_without_strict() {
        let checks = vec![check("a", Status::Unknown, true)];
        assert!(
            should_fail(&checks, false),
            "an unverified required check must not be silently treated as passing"
        );
    }

    #[test]
    fn an_optional_failure_does_not_fail_without_strict() {
        let checks = vec![check("a", Status::Fail, false)];
        assert!(!should_fail(&checks, false));
    }

    #[test]
    fn strict_fails_on_an_optional_warning() {
        let checks = vec![check("a", Status::Warning, false)];
        assert!(!should_fail(&checks, false));
        assert!(should_fail(&checks, true));
    }

    #[test]
    fn strict_does_not_fail_on_a_not_applicable_or_skipped_result() {
        let checks = vec![
            check("a", Status::NotApplicable, false),
            check("b", Status::Skipped, false),
        ];
        assert!(
            !should_fail(&checks, true),
            "strict extends the failure condition to applicable results only"
        );
    }

    #[test]
    fn an_all_pass_report_never_fails() {
        let checks = vec![
            check("a", Status::Pass, true),
            check("b", Status::Pass, false),
        ];
        assert!(!should_fail(&checks, false));
        assert!(!should_fail(&checks, true));
    }

    /// A minimal real Cargo package `florui_config::resolve_cargo_project`
    /// can actually resolve -- these tests exercise `project_checks_at`
    /// against a real temporary workspace and a real `cargo metadata`
    /// shell-out, the same reasoning `florui-config`'s own integration
    /// tests use: faking this would misrepresent what's being verified.
    fn scaffold_project(root: &std::path::Path) {
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "").unwrap();
    }

    fn find<'a>(checks: &'a [Check], id: &str) -> &'a Check {
        checks
            .iter()
            .find(|check| check.id == id)
            .unwrap_or_else(|| panic!("no check with id {id} in {checks:#?}"))
    }

    #[test]
    fn project_checks_at_reports_not_applicable_outside_a_project() {
        let dir = tempfile::tempdir().unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "project.resolved").status,
            Status::NotApplicable
        );
    }

    #[test]
    fn project_checks_at_reports_config_discovered_and_defaults_without_a_config_file() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(find(&checks, "config.discovered").status, Status::Pass);
        assert_eq!(
            find(&checks, "config.schema_valid").status,
            Status::NotApplicable
        );
    }

    #[test]
    fn project_checks_at_flags_an_unsupported_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 2\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.schema_version_supported").status,
            Status::Fail
        );
    }

    #[test]
    fn project_checks_at_flags_a_legacy_and_new_dev_example_conflict() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [package.metadata.florui.dev]\nexample = \"counter\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[dev]\nexample = \"counter\"\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.legacy_migration_conflict").status,
            Status::Fail
        );
    }

    #[test]
    fn project_checks_at_flags_a_missing_icon_asset_under_native_target() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app.icons]\nsource = \"missing.svg\"\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.icon_asset_present.source").status,
            Status::Warning
        );
    }

    #[test]
    fn project_checks_at_skips_icon_asset_checks_outside_native_target() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app.icons]\nsource = \"missing.svg\"\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "web");
        assert_eq!(
            find(&checks, "config.icon_asset_present.source").status,
            Status::NotApplicable
        );
    }

    #[test]
    fn project_checks_at_reports_the_selected_environment() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[environments.development.app]\nidentifier = \"com.floregreen.garden.dev\"\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, Some("development"), "native");
        let check = find(&checks, "config.environment_selected");
        assert_eq!(check.status, Status::Pass);
        assert!(check.evidence.contains("development"));
        assert!(check.evidence.contains("applied"));
    }

    #[test]
    fn project_checks_at_flags_an_unknown_explicit_environment() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, Some("staging"), "native");
        assert_eq!(
            find(&checks, "config.environment_known").status,
            Status::Fail
        );
    }

    #[test]
    fn project_checks_at_flags_a_duplicate_environment_identifier() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n\
             [environments.development]\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.identity_collision").status,
            Status::Fail
        );
    }

    #[test]
    fn window_persistence_identity_check_is_not_applicable_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.window_persistence_identity").status,
            Status::NotApplicable
        );
    }

    #[test]
    fn window_persistence_identity_check_passes_with_an_identifier() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n\
             [window.persistence]\nenabled = true\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.window_persistence_identity").status,
            Status::Pass
        );
    }

    #[test]
    fn window_persistence_identity_check_warns_without_an_identifier() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[window.persistence]\nenabled = true\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.window_persistence_identity").status,
            Status::Warning
        );
    }

    #[test]
    fn single_instance_identity_check_is_not_applicable_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.single_instance_identity").status,
            Status::NotApplicable
        );
    }

    #[test]
    fn single_instance_identity_check_passes_with_an_identifier() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n\
             [app.activation]\nsingle_instance = true\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.single_instance_identity").status,
            Status::Pass
        );
    }

    #[test]
    fn single_instance_identity_check_warns_without_an_identifier() {
        let dir = tempfile::tempdir().unwrap();
        scaffold_project(dir.path());
        std::fs::write(
            dir.path().join("florui.config.toml"),
            "schema_version = 1\n[app.activation]\nsingle_instance = true\n",
        )
        .unwrap();
        let checks = project_checks_at(dir.path(), None, None, "native");
        assert_eq!(
            find(&checks, "config.single_instance_identity").status,
            Status::Warning
        );
    }

    #[test]
    fn environment_checks_include_the_new_capability_probes() {
        let checks = environment_checks();
        let single_instance = find(&checks, "environment.single_instance_capability");
        let persistence = find(&checks, "environment.window_persistence_capability");
        if cfg!(target_os = "windows") {
            assert_eq!(single_instance.status, Status::Pass);
            assert_eq!(persistence.status, Status::Pass);
        } else {
            assert_eq!(single_instance.status, Status::NotApplicable);
            assert_eq!(persistence.status, Status::NotApplicable);
        }
    }
}
