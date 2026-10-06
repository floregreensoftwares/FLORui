//! `florui schema` against the real binary.

use std::path::Path;
use std::process::{Command, Output};

fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join("src/nested")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"garden\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
}

fn schema(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("schema")
        .args(args)
        .current_dir(cwd)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn printing_the_schema_gives_exactly_what_the_library_generates() {
    let dir = tempfile::tempdir().unwrap();

    let output = schema(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        florui_config::json_schema_pretty()
    );
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn writing_from_a_nested_directory_puts_the_file_in_the_package_and_a_second_run_changes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("my project");
    project(&dir);
    let target = dir.join(florui_config::SCHEMA_FILE_NAME);

    let first = schema(&dir.join("src/nested"), &["--write"]);
    let second = schema(&dir.join("src/nested"), &["--write"]);

    assert_eq!(first.status.code(), Some(0), "{}", text(&first));
    assert!(text(&first).contains("created"), "{}", text(&first));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        florui_config::json_schema_pretty()
    );
    assert!(
        text(&second).contains("already current"),
        "{}",
        text(&second)
    );
    assert!(
        !dir.join("src/nested")
            .join(florui_config::SCHEMA_FILE_NAME)
            .exists()
    );
}

#[test]
fn a_stale_copy_is_reported_as_updated_and_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path());
    let target = dir.path().join(florui_config::SCHEMA_FILE_NAME);
    std::fs::write(&target, "{}").unwrap();

    let output = schema(dir.path(), &["--write"]);

    assert!(text(&output).contains("updated"), "{}", text(&output));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        florui_config::json_schema_pretty()
    );
}

#[test]
fn an_explicit_path_works_outside_any_project() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("elsewhere.json");

    let output = schema(dir.path(), &["--write", target.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        florui_config::json_schema_pretty()
    );
}

#[test]
fn writing_outside_a_project_without_a_path_says_what_to_do() {
    let dir = tempfile::tempdir().unwrap();

    let output = schema(dir.path(), &["--write"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output).contains("--write <PATH>"),
        "{}",
        text(&output)
    );
}

#[test]
fn the_locales_schema_prints_as_the_library_generates_it() {
    let dir = tempfile::tempdir().unwrap();

    let output = schema(dir.path(), &["--locales"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        florui_config::locales_json_schema_pretty()
    );
}

#[test]
fn writing_the_locales_schema_uses_its_own_file_name_beside_the_other() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path());

    let locales = schema(dir.path(), &["--locales", "--write"]);
    let config = schema(dir.path(), &["--write"]);

    assert_eq!(locales.status.code(), Some(0), "{}", text(&locales));
    assert!(
        text(&locales).contains("florui.locales.toml"),
        "{}",
        text(&locales)
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(florui_config::LOCALES_SCHEMA_FILE_NAME)).unwrap(),
        florui_config::locales_json_schema_pretty()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(florui_config::SCHEMA_FILE_NAME)).unwrap(),
        florui_config::json_schema_pretty()
    );
    assert_eq!(config.status.code(), Some(0));
}
