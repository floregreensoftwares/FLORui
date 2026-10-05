//! `florui fmt`: a thin CLI wrapper over [`florui_fmt::format_source`] —
//! file discovery, `--check`, and safe writes live here; the actual
//! reformatting decisions live in that crate (see its own module doc).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ChildCommand, ExitCode};

use florui_devtools::diagnostics::{dim_text, failure, success, warning};

pub struct Options {
    /// Explicit files/directories to format — an empty list means
    /// "resolve the workspace root and walk it," matching `florui fmt`'s
    /// own no-argument form.
    pub paths: Vec<PathBuf>,
    pub check: bool,
    /// Format stdin as if it were this file: its directory selects the
    /// `rustfmt` configuration and its package the edition.
    pub stdin_filepath: Option<PathBuf>,
}

pub fn run(options: Options) -> ExitCode {
    if let Some(path) = &options.stdin_filepath {
        return run_stdin(path, options.check);
    }
    let workspace = workspace_metadata(None);
    let edition = workspace
        .as_ref()
        .map(|meta| meta.edition.clone())
        // A real default (Rust's own stable one), not a guess specific
        // to this workspace — only used when `cargo metadata` itself
        // could not be resolved at all (formatting an explicit path
        // outside any project).
        .unwrap_or_else(|| "2021".to_string());

    let roots = if options.paths.is_empty() {
        match workspace.map(|meta| meta.root) {
            Some(root) => vec![root],
            None => {
                eprintln!(
                    "florui fmt: could not resolve a workspace root (not inside a Cargo \
                     project, and no path was given)"
                );
                return ExitCode::from(2);
            }
        }
    } else {
        options.paths
    };

    let mut files = Vec::new();
    for root in &roots {
        if root.is_file() {
            if florui_fmt::is_formattable(root) {
                files.push(root.clone());
            }
            continue;
        }
        if let Err(error) = collect_rust_files(root, &mut files) {
            eprintln!("florui fmt: could not walk {}: {error}", root.display());
            return ExitCode::from(2);
        }
    }
    files.sort();
    files.dedup();

    // Compute every file's own outcome before writing anything — a parse
    // or tool failure on one file must leave every selected file
    // unchanged, not just the ones processed before it.
    struct Planned {
        path: PathBuf,
        original: String,
        outcome: florui_fmt::FormatOutcome,
    }

    let mut planned = Vec::new();
    for path in &files {
        let original = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) => {
                eprintln!("florui fmt: could not read {}: {error}", path.display());
                return ExitCode::from(2);
            }
        };
        match florui_fmt::format_source(
            &original,
            &edition,
            path.parent().filter(|dir| !dir.as_os_str().is_empty()),
        ) {
            Ok(outcome) => planned.push(Planned {
                path: path.clone(),
                original,
                outcome,
            }),
            Err(error) => {
                eprintln!("florui fmt: {}: {error}", path.display());
                return ExitCode::from(2);
            }
        }
    }

    let mut any_changed = false;
    for plan in &planned {
        for skipped in &plan.outcome.skipped {
            eprintln!(
                "{} {}:{}: {}",
                warning("skipped"),
                plan.path.display(),
                skipped.line,
                skipped.reason
            );
        }
        if plan.outcome.output != plan.original {
            any_changed = true;
        }
    }

    if options.check {
        for plan in &planned {
            if plan.outcome.output != plan.original {
                println!("{} {}", failure("would reformat"), plan.path.display());
            }
        }
        if !any_changed {
            println!("{}", success("already formatted"));
        }
        return if any_changed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }

    for plan in &planned {
        if plan.outcome.output == plan.original {
            continue;
        }
        // A concurrent edit between the read above and now is reported,
        // not silently overwritten.
        match fs::read_to_string(&plan.path) {
            Ok(current) if current == plan.original => {}
            Ok(_) => {
                eprintln!(
                    "florui fmt: {} changed on disk since it was read — not overwriting",
                    plan.path.display()
                );
                continue;
            }
            Err(error) => {
                eprintln!(
                    "florui fmt: could not re-read {}: {error}",
                    plan.path.display()
                );
                continue;
            }
        }
        if let Err(error) = write_atomic(&plan.path, &plan.outcome.output) {
            eprintln!(
                "florui fmt: could not write {}: {error}",
                plan.path.display()
            );
            return ExitCode::from(2);
        }
        println!("{} {}", success("formatted"), plan.path.display());
    }
    if !any_changed {
        println!("{}", dim_text("nothing to format"));
    }

    ExitCode::SUCCESS
}

/// Writes `contents` via a temp file in the same directory, then renames
/// it over `path` — a rename is atomic on the same filesystem, so a
/// reader never observes a half-written file.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = dir.join(format!(
        ".{}.florui-fmt-tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
    ));
    fs::write(&temp, contents)?;
    fs::rename(&temp, path)
}

fn collect_rust_files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if name == "target" || name.starts_with('.') {
                continue;
            }
            collect_rust_files(&path, out)?;
        } else if florui_fmt::is_formattable(&path) {
            out.push(path);
        }
    }
    Ok(())
}

struct WorkspaceMetadata {
    root: PathBuf,
    /// Any resolved package's own `edition` field — every member of
    /// this workspace shares one edition via `edition.workspace = true`,
    /// so the first package found is as good as asking for a specific
    /// one. `rustfmt` needs this explicitly: run with no edition it
    /// defaults differently than `cargo fmt` does (which reads it from
    /// the crate's own `Cargo.toml` automatically), producing
    /// spurious reformatting this crate would otherwise misreport as
    /// this tool's own decision rather than an edition mismatch.
    edition: String,
    /// Each package's directory and edition, for resolving a single file.
    packages: Vec<(PathBuf, String)>,
}

impl WorkspaceMetadata {
    fn edition_for(&self, file: &Path) -> &str {
        self.packages
            .iter()
            .filter(|(dir, _)| file.starts_with(dir))
            .max_by_key(|(dir, _)| dir.components().count())
            .map_or(self.edition.as_str(), |(_, edition)| edition.as_str())
    }
}

/// Resolved from `dir` when given, otherwise from the working directory.
fn workspace_metadata(dir: Option<&Path>) -> Option<WorkspaceMetadata> {
    let mut command = ChildCommand::new("cargo");
    command.args(["metadata", "--format-version", "1", "--no-deps"]);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let root = metadata
        .get("workspace_root")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)?;
    let package_list = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array);
    let edition_of = |package: &serde_json::Value| {
        package
            .get("edition")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let edition = package_list
        .and_then(|packages| packages.first())
        .and_then(edition_of)
        .unwrap_or_else(|| "2021".to_string());
    let packages = package_list
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let manifest = package.get("manifest_path")?.as_str()?;
            let dir = Path::new(manifest).parent()?.to_path_buf();
            Some((dir, edition_of(package)?))
        })
        .collect();
    Some(WorkspaceMetadata {
        root,
        edition,
        packages,
    })
}

/// Formats stdin as `path` and writes only the formatted source to stdout, so
/// an editor can replace the buffer with it; every diagnostic goes to stderr.
fn run_stdin(path: &Path, check: bool) -> ExitCode {
    use std::io::{Read, Write};

    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if !florui_fmt::is_formattable(&path) {
        eprintln!("florui fmt: {} is not a Rust source file", path.display());
        return ExitCode::from(2);
    }
    let mut source = String::new();
    if let Err(error) = std::io::stdin().read_to_string(&mut source) {
        eprintln!("florui fmt: could not read stdin as UTF-8 text: {error}");
        return ExitCode::from(2);
    }

    // The file may be unsaved, so resolve from the nearest directory that exists.
    let config_dir = path.ancestors().skip(1).find(|dir| dir.is_dir());
    let edition = workspace_metadata(config_dir)
        .map(|meta| meta.edition_for(&path).to_string())
        .unwrap_or_else(|| "2021".to_string());

    let outcome = match florui_fmt::format_source(&source, &edition, config_dir) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("florui fmt: {}: {error}", path.display());
            return ExitCode::from(2);
        }
    };
    for skipped in &outcome.skipped {
        eprintln!(
            "skipped {}:{}: {}",
            path.display(),
            skipped.line,
            skipped.reason
        );
    }
    if check {
        return if outcome.output == source {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(outcome.output.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("florui fmt: could not write stdout: {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_rust_files_finds_rs_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/b.rs"), "fn b() {}").unwrap();
        std::fs::write(dir.path().join("c.txt"), "not rust").unwrap();

        let mut files = Vec::new();
        collect_rust_files(dir.path(), &mut files).unwrap();
        files.sort();

        assert_eq!(files.len(), 2, "found: {files:?}");
        assert!(files.iter().any(|f| f.ends_with("a.rs")));
        assert!(
            files
                .iter()
                .any(|f| f.ends_with("nested/b.rs") || f.ends_with(r"nested\b.rs"))
        );
    }

    #[test]
    fn collect_rust_files_skips_target_and_hidden_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/generated.rs"), "fn g() {}").unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/hook.rs"), "fn h() {}").unwrap();
        std::fs::write(dir.path().join("real.rs"), "fn r() {}").unwrap();

        let mut files = Vec::new();
        collect_rust_files(dir.path(), &mut files).unwrap();

        assert_eq!(files.len(), 1, "found: {files:?}");
        assert!(files[0].ends_with("real.rs"));
    }

    #[test]
    fn write_atomic_replaces_the_files_own_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file.rs");
        std::fs::write(&path, "old content").unwrap();

        write_atomic(&path, "new content").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
        // The temp file must not be left behind alongside the real one.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            leftovers,
            vec!["file.rs".to_string()],
            "leftovers: {leftovers:?}"
        );
    }
}
