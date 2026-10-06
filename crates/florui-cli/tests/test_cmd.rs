//! `florui test` against the real binary, in temporary workspaces of trivial
//! crates: which package's tests run, what is reported and the exit codes.

use std::path::Path;
use std::process::{Command, Output};

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn member(dir: &Path, name: &str, tests: &str) {
    write(
        dir,
        &format!("{name}/Cargo.toml"),
        &format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    );
    write(dir, &format!("{name}/src/lib.rs"), tests);
}

/// A workspace of two members: `good` whose tests pass, `bad` whose test fails.
fn workspace(dir: &Path) {
    write(
        dir,
        "Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"good\", \"bad\"]\n",
    );
    member(
        dir,
        "good",
        "#[test] fn alpha() {}\n#[test] fn beta() { panic!(\"beta ran\"); }\n",
    );
    member(dir, "bad", "#[test] fn broken() { panic!(\"bad ran\"); }\n");
}

fn florui_test(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("test")
        .args(args)
        .current_dir(cwd)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("FLORUI_CHROMIUM")
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
fn only_the_selected_packages_tests_run() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_test(
        dir.path(),
        &["--package", "good", "--suite", "cargo", "--", "alpha"],
    );

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    assert!(all.contains("test alpha ... ok"), "{all}");
    assert!(!all.contains("bad ran") && !all.contains("broken"), "{all}");
    assert!(all.contains("cargo: passed"), "{all}");
}

#[test]
fn a_failing_test_fails_the_command_and_its_output_is_not_hidden() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_test(dir.path(), &["--package", "bad", "--suite", "cargo"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("bad ran"), "{all}");
    assert!(all.contains("cargo: failed"), "{all}");
}

#[test]
fn arguments_after_the_separator_reach_cargo_test() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    // `beta` panics; filtering to `alpha` must not run it.
    let filtered = florui_test(
        dir.path(),
        &["--package", "good", "--suite", "cargo", "--", "alpha"],
    );
    assert_eq!(filtered.status.code(), Some(0), "{}", text(&filtered));
    let unfiltered = florui_test(dir.path(), &["--package", "good", "--suite", "cargo"]);
    assert_eq!(unfiltered.status.code(), Some(1), "{}", text(&unfiltered));
}

#[test]
fn runs_the_same_package_from_a_nested_directory() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());
    let nested = dir.path().join("good/src");

    let output = florui_test(&nested, &["--suite", "cargo", "--", "alpha"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    assert!(all.contains("test alpha ... ok"), "{all}");
    assert!(!all.contains("broken"), "{all}");
}

#[test]
fn an_ambiguous_workspace_is_refused_without_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_test(dir.path(), &["--suite", "cargo"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(
        !all.contains("running 1 test") && !all.contains("running 2 tests"),
        "{all}"
    );
}

#[test]
fn the_visual_suite_asked_for_by_name_fails_when_it_cannot_run() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_test(dir.path(), &["--package", "good", "--suite", "visual"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(
        all.contains("visual: failed (no reference fixtures"),
        "{all}"
    );
    assert!(
        !all.contains("cargo:"),
        "the cargo suite must not run: {all}"
    );
}

#[test]
fn under_all_a_visual_suite_that_cannot_run_is_reported_as_skipped() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_test(dir.path(), &["--package", "good", "--", "alpha"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    assert!(all.contains("cargo: passed"), "{all}");
    assert!(
        all.contains("visual: skipped (no reference fixtures"),
        "{all}"
    );
}

#[test]
fn a_cargo_failure_still_lets_the_other_suite_report() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_test(dir.path(), &["--package", "bad"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("cargo: failed"), "{all}");
    assert!(all.contains("visual: skipped"), "{all}");
}

#[test]
fn the_chromium_environment_variable_reaches_the_visual_suite() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());
    write(dir.path(), "fixtures/reference/one/manifest.json", "{}");

    let output = Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(["test", "--package", "good", "--suite", "visual"])
        .current_dir(dir.path())
        .env_remove("CARGO_TARGET_DIR")
        .env("FLORUI_CHROMIUM", dir.path().join("no-such-chromium"))
        .output()
        .unwrap();

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("could not launch Chromium"), "{all}");
    assert!(!all.contains("no Chromium found"), "{all}");
}
