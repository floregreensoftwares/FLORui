//! What `florui test` runs and how it reports it: the selection of suites,
//! the `cargo test` invocation (always one package, never the workspace) and
//! the summary that decides the exit code.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Suite {
    /// The selected package's Cargo tests.
    Cargo,
    /// The reference fixtures compared against Chromium.
    Visual,
    /// Both; the visual suite is skipped, and reported as skipped, when its
    /// prerequisites are missing.
    All,
}

impl Suite {
    pub fn includes_cargo(self) -> bool {
        matches!(self, Suite::Cargo | Suite::All)
    }

    pub fn includes_visual(self) -> bool {
        matches!(self, Suite::Visual | Suite::All)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SuiteOutcome {
    Passed,
    Failed(String),
    Skipped(String),
}

impl SuiteOutcome {
    /// A suite that could not start. Asked for by name it is a failure, since
    /// reporting it as passed would claim coverage that did not run; as part
    /// of `all` it is a visible skip.
    pub fn unavailable(requested: Suite, suite: Suite, reason: String) -> Self {
        if requested == suite {
            SuiteOutcome::Failed(reason)
        } else {
            SuiteOutcome::Skipped(reason)
        }
    }
}

pub fn cargo_args(package: &str, extra: &[String]) -> Vec<String> {
    let mut args = vec!["test".to_string(), "-p".to_string(), package.to_string()];
    args.extend(extra.iter().cloned());
    args
}

pub fn fixtures_root(workspace_root: &Path) -> PathBuf {
    workspace_root.join("fixtures").join("reference")
}

/// Prints one line per suite and returns the exit code: failure when any
/// suite failed, success otherwise.
pub fn summarize(results: &[(&str, SuiteOutcome)]) -> ExitCode {
    println!();
    println!("florui test:");
    let mut failed = false;
    for (name, outcome) in results {
        match outcome {
            SuiteOutcome::Passed => println!("  {name}: passed"),
            SuiteOutcome::Failed(reason) => {
                failed = true;
                println!("  {name}: failed ({reason})");
            }
            SuiteOutcome::Skipped(reason) => println!("  {name}: skipped ({reason})"),
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cargo_invocation_names_one_package_and_never_the_workspace() {
        let args = cargo_args("my-app", &["--".to_string(), "only_this".to_string()]);
        assert_eq!(args, ["test", "-p", "my-app", "--", "only_this"]);
        assert!(!args.iter().any(|arg| arg == "--workspace"));
    }

    #[test]
    fn an_unavailable_suite_fails_when_asked_for_by_name_and_is_skipped_inside_all() {
        let reason = || "no Chromium".to_string();
        assert_eq!(
            SuiteOutcome::unavailable(Suite::Visual, Suite::Visual, reason()),
            SuiteOutcome::Failed(reason())
        );
        assert_eq!(
            SuiteOutcome::unavailable(Suite::All, Suite::Visual, reason()),
            SuiteOutcome::Skipped(reason())
        );
    }

    #[test]
    fn the_fixtures_are_found_from_the_workspace_root_not_the_working_directory() {
        assert_eq!(
            fixtures_root(Path::new("root")),
            Path::new("root").join("fixtures").join("reference")
        );
    }

    #[test]
    fn suites_select_what_runs() {
        assert!(Suite::Cargo.includes_cargo() && !Suite::Cargo.includes_visual());
        assert!(!Suite::Visual.includes_cargo() && Suite::Visual.includes_visual());
        assert!(Suite::All.includes_cargo() && Suite::All.includes_visual());
    }
}
