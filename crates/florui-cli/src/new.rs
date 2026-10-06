//! `florui new`: writes a minimal application that compiles and tests, from
//! the files under `templates/new/`. It never overwrites: a file that already
//! exists is a conflict and nothing is written.
//!
//! The generated `Cargo.toml` depends on the Florui crates by version, the
//! same one as this CLI (the crates are meant to be released together), so it
//! resolves once they are published.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use florui_devtools::diagnostics::{dim_text, failure, success};

/// Must match the workspace's edition; a test checks it.
const EDITION: &str = "2024";

const FILES: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        include_str!("../templates/new/Cargo.toml.tmpl"),
    ),
    (
        "florui.config.toml",
        include_str!("../templates/new/florui.config.toml.tmpl"),
    ),
    (
        ".gitignore",
        include_str!("../templates/new/gitignore.tmpl"),
    ),
    (
        "src/lib.rs",
        include_str!("../templates/new/src/lib.rs.tmpl"),
    ),
    (
        "src/app.rs",
        include_str!("../templates/new/src/app.rs.tmpl"),
    ),
    (
        "src/app.css",
        include_str!("../templates/new/src/app.css.tmpl"),
    ),
    (
        "src/main.rs",
        include_str!("../templates/new/src/main.rs.tmpl"),
    ),
    (
        "examples/dev.rs",
        include_str!("../templates/new/examples/dev.rs.tmpl"),
    ),
    (
        "tests/app.rs",
        include_str!("../templates/new/tests/app.rs.tmpl"),
    ),
];

const STRICT_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "abstract", "become", "box", "do", "final", "macro", "override", "priv",
    "try", "typeof", "unsized", "virtual", "yield",
];

/// Names Cargo or the generated dependency list would make fail later.
const RESERVED_NAMES: &[&str] = &[
    "test",
    "core",
    "std",
    "alloc",
    "proc_macro",
    "florui",
    "florui-platform",
    "florui-style",
    "florui-test",
];

#[derive(Debug, PartialEq)]
pub enum NewError {
    InvalidName(String),
    /// The paths that already exist and would have been overwritten.
    Conflicts(Vec<PathBuf>),
    Io {
        path: PathBuf,
        message: String,
        /// Written before the failure; not rolled back.
        written: Vec<PathBuf>,
    },
}

impl std::fmt::Display for NewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NewError::InvalidName(reason) => write!(f, "{reason}"),
            NewError::Conflicts(paths) => {
                write!(f, "refusing to overwrite existing files:")?;
                for path in paths {
                    write!(f, "\n    {}", path.display())?;
                }
                Ok(())
            }
            NewError::Io {
                path,
                message,
                written,
            } => {
                write!(f, "could not write {}: {message}", path.display())?;
                if !written.is_empty() {
                    write!(f, "\nalready written (left in place):")?;
                    for path in written {
                        write!(f, "\n    {}", path.display())?;
                    }
                }
                Ok(())
            }
        }
    }
}

pub fn run(name: &str) -> ExitCode {
    let parent = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!(
                "{}",
                failure(&format!(
                    "could not determine the current directory: {error}"
                ))
            );
            return ExitCode::from(2);
        }
    };
    match scaffold(&parent, name) {
        Ok(files) => {
            println!("{} {name} ({} files)", success("created"), files.len());
            println!("{}", dim_text(&format!("cd {name} && florui dev")));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{}", failure(&format!("florui new: {error}")));
            ExitCode::from(2)
        }
    }
}

/// Every file of a new project, rendered: the templates, and the editor
/// schema for `florui.config.toml` (generated, so it is always the one this
/// `florui` understands; the generated configuration names it in its first
/// line).
fn files(name: &str) -> Vec<(&'static str, String)> {
    FILES
        .iter()
        .map(|(relative, template)| (*relative, render(template, name)))
        .chain(std::iter::once((
            florui_config::SCHEMA_FILE_NAME,
            florui_config::json_schema_pretty(),
        )))
        .collect()
}

/// Creates `parent/name` and returns the files written.
pub fn scaffold(parent: &Path, name: &str) -> Result<Vec<PathBuf>, NewError> {
    validate_name(name)?;
    let root = parent.join(name);
    let files = files(name);

    let conflicts: Vec<PathBuf> = files
        .iter()
        .map(|(relative, _)| root.join(relative))
        .filter(|path| path.symlink_metadata().is_ok())
        .collect();
    if !conflicts.is_empty() {
        return Err(NewError::Conflicts(conflicts));
    }
    if root.symlink_metadata().is_ok_and(|meta| !meta.is_dir()) {
        return Err(NewError::Conflicts(vec![root]));
    }

    let mut written = Vec::new();
    for (relative, contents) in &files {
        let path = root.join(relative);
        let result = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| {
                // `create_new` fails instead of replacing a file that appeared
                // after the check above.
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                file.write_all(contents.as_bytes())
            });
        match result {
            Ok(()) => written.push(path),
            Err(error) => {
                return Err(NewError::Io {
                    path,
                    message: error.to_string(),
                    written,
                });
            }
        }
    }
    Ok(written)
}

fn validate_name(name: &str) -> Result<(), NewError> {
    let invalid = |reason: &str| Err(NewError::InvalidName(format!("\"{name}\" {reason}")));
    if name.is_empty() {
        return Err(NewError::InvalidName(
            "the project name is empty".to_string(),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return invalid("may only contain ASCII letters, digits, '-' and '_'");
    }
    if name.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return invalid("must start with a letter or '_'");
    }
    let ident = crate_name(name);
    if STRICT_KEYWORDS.contains(&ident.as_str()) {
        return invalid("is a Rust keyword and cannot be a crate name");
    }
    if RESERVED_NAMES.contains(&name) || RESERVED_NAMES.contains(&ident.as_str()) {
        return invalid("would collide with a crate the project depends on or with std");
    }
    Ok(())
}

fn crate_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Always LF: a Windows checkout may have turned the template files into CRLF.
fn render(template: &str, name: &str) -> String {
    template
        .replace("\r\n", "\n")
        .replace("{{name}}", name)
        .replace("{{crate}}", &crate_name(name))
        .replace("{{version}}", env!("CARGO_PKG_VERSION"))
        .replace("{{edition}}", EDITION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placeholder_in_every_template_is_replaced() {
        for (path, template) in FILES {
            let rendered = render(template, "my-app");
            assert!(!rendered.contains("{{"), "{path} keeps a placeholder");
        }
    }

    #[test]
    fn output_is_lf_even_when_the_template_was_checked_out_as_crlf() {
        assert_eq!(render("a {{name}}\r\nb\r\n", "x"), "a x\nb\n");
    }

    #[test]
    fn the_template_edition_is_the_workspaces() {
        let manifest = include_str!("../../../Cargo.toml");
        assert!(
            manifest.contains(&format!("edition = \"{EDITION}\"")),
            "the workspace edition changed: update EDITION"
        );
    }

    #[test]
    fn names_are_checked_before_anything_is_written() {
        for bad in [
            "",
            "1app",
            "-app",
            "my app",
            "my/app",
            "fn",
            "self",
            "florui",
            "florui-test",
            "std",
        ] {
            assert!(
                matches!(validate_name(bad), Err(NewError::InvalidName(_))),
                "{bad:?} was accepted"
            );
        }
        for good in ["app", "my-app", "my_app", "_app", "App2"] {
            assert_eq!(validate_name(good), Ok(()), "{good:?} was refused");
        }
    }
}
