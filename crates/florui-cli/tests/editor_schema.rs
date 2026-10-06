//! The editor schema a project points at: `florui new` writes it, and
//! `florui doctor` says when the copy is not the one this florui generates.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(args)
        .current_dir(cwd)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn editor_schema_check(cwd: &Path) -> Value {
    let output = run(cwd, &["doctor", "--json"]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "config.editor_schema")
        .unwrap_or_else(|| panic!("no config.editor_schema in {report}"))
        .clone()
}

fn status(cwd: &Path) -> String {
    editor_schema_check(cwd)["status"]
        .as_str()
        .unwrap()
        .to_string()
}

fn project(dir: &Path, config: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"garden\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("florui.config.toml"), config).unwrap();
}

const SCHEMA: &str = florui_config::SCHEMA_FILE_NAME;

#[test]
fn a_new_project_comes_with_the_schema_and_the_line_that_names_it() {
    let dir = tempfile::tempdir().unwrap();

    let output = run(dir.path(), &["new", "my-app"]);

    assert_eq!(output.status.code(), Some(0));
    let project = dir.path().join("my-app");
    let config = std::fs::read_to_string(project.join("florui.config.toml")).unwrap();
    assert_eq!(
        config.lines().next(),
        Some("#:schema ./florui.config.schema.json")
    );
    assert_eq!(
        std::fs::read_to_string(project.join(SCHEMA)).unwrap(),
        florui_config::json_schema_pretty()
    );
}

#[test]
fn a_new_project_passes_the_check_straight_away() {
    let dir = tempfile::tempdir().unwrap();
    run(dir.path(), &["new", "my-app"]);

    assert_eq!(status(&dir.path().join("my-app")), "pass");
}

#[test]
fn an_existing_schema_file_stops_florui_new_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("my-app")).unwrap();
    std::fs::write(dir.path().join("my-app").join(SCHEMA), "{}").unwrap();

    let output = run(dir.path(), &["new", "my-app"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(!dir.path().join("my-app/Cargo.toml").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("my-app").join(SCHEMA)).unwrap(),
        "{}"
    );
}

#[test]
fn a_current_copy_passes_from_a_nested_directory() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "#:schema ./florui.config.schema.json\nschema_version = 1\n",
    );
    run(dir.path(), &["schema", "--write"]);

    assert_eq!(status(&dir.path().join("src")), "pass");
}

#[test]
fn a_copy_written_by_another_version_or_edited_is_a_warning_with_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "#:schema ./florui.config.schema.json\nschema_version = 1\n",
    );
    std::fs::write(dir.path().join(SCHEMA), "{\"type\": \"object\"}").unwrap();

    let check = editor_schema_check(dir.path());

    assert_eq!(check["status"], "warning");
    assert!(
        check["remediation"]
            .as_str()
            .unwrap()
            .contains("florui schema --write")
    );
    // The command the check names makes it pass.
    run(dir.path(), &["schema", "--write"]);
    assert_eq!(status(dir.path()), "pass");
}

#[test]
fn a_line_that_points_at_a_missing_file_is_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "#:schema ./florui.config.schema.json\nschema_version = 1\n",
    );

    assert_eq!(status(dir.path()), "warning");
}

#[test]
fn a_url_is_not_read_and_no_line_at_all_is_not_applicable() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "#:schema https://example.com/florui.schema.json\nschema_version = 1\n",
    );
    assert_eq!(status(dir.path()), "not_applicable");

    std::fs::write(
        dir.path().join("florui.config.toml"),
        "schema_version = 1\n",
    )
    .unwrap();
    assert_eq!(status(dir.path()), "not_applicable");
}

#[test]
fn without_a_configuration_file_the_check_is_not_applicable() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path(), "");
    std::fs::remove_file(dir.path().join("florui.config.toml")).unwrap();

    assert_eq!(status(dir.path()), "not_applicable");
}
