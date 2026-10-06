//! `florui schema`: the JSON Schema editors read for `florui.config.toml`,
//! printed or written next to the project's configuration so an editor can
//! be pointed at it with a relative path.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use florui_devtools::diagnostics::{dim_text, failure, success};

pub struct Options {
    pub package: Option<String>,
    /// `None` prints the schema; `Some(None)` writes it into the project;
    /// `Some(Some(path))` writes it to `path`.
    pub write: Option<Option<PathBuf>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Written {
    Created,
    Changed,
    Unchanged,
}

/// Writes `content` to `path`, leaving the file alone when it already holds
/// exactly that, and says which of the three happened.
pub fn write_if_different(path: &Path, content: &str) -> Result<Written, String> {
    let existing = std::fs::read_to_string(path);
    let outcome = match &existing {
        Ok(current) if current == content => return Ok(Written::Unchanged),
        Ok(_) => Written::Changed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Written::Created,
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    std::fs::write(path, content)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    Ok(outcome)
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{}", failure(&message.to_string()));
    ExitCode::FAILURE
}

pub fn run(options: Options) -> ExitCode {
    let schema = florui_config::json_schema_pretty();
    let Some(target) = options.write else {
        print!("{schema}");
        return ExitCode::SUCCESS;
    };
    let path = match target {
        Some(path) => path,
        None => {
            let cwd = match std::env::current_dir() {
                Ok(cwd) => cwd,
                Err(error) => {
                    return fail(format!("could not read the current directory: {error}"));
                }
            };
            match florui_config::resolve_cargo_project(&cwd, options.package.as_deref()) {
                Ok(facts) => facts.package_root.join(florui_config::SCHEMA_FILE_NAME),
                Err(error) => {
                    return fail(format!(
                        "{error}; run this inside a project, or name the file with --write <PATH>"
                    ));
                }
            }
        }
    };
    match write_if_different(&path, &schema) {
        Ok(Written::Created) => println!("{}", success(&format!("created {}", path.display()))),
        Ok(Written::Changed) => println!("{}", success(&format!("updated {}", path.display()))),
        Ok(Written::Unchanged) => {
            println!(
                "{}",
                dim_text(&format!("{} is already current", path.display()))
            );
        }
        Err(error) => return fail(error),
    }
    println!(
        "{}",
        dim_text(&format!(
            "point an editor at it with `#:schema ./{}` on the first line of florui.config.toml",
            path.file_name().map_or_else(
                || florui_config::SCHEMA_FILE_NAME.to_string(),
                |name| name.to_string_lossy().into_owned()
            )
        ))
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_created_changed_or_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.json");

        assert_eq!(write_if_different(&path, "one").unwrap(), Written::Created);
        assert_eq!(
            write_if_different(&path, "one").unwrap(),
            Written::Unchanged
        );
        assert_eq!(write_if_different(&path, "two").unwrap(), Written::Changed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
    }

    #[test]
    fn a_file_that_cannot_be_written_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let error =
            write_if_different(&dir.path().join("missing").join("s.json"), "x").unwrap_err();
        assert!(
            error.contains("could not write") && error.contains("s.json"),
            "{error}"
        );
    }
}
