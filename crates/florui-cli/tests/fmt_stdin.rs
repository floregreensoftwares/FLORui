//! `florui fmt --stdin-filepath` against the real binary: what goes to
//! stdout, what stays off it, and the exit codes an editor integration relies on.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const DIRTY: &str = "fn root() -> Element {\nview! {\n<div class=\"a\"><span>{\"x\"}</span><span>{\"y\"}</span></div>\n}\n}\n";

fn florui_fmt(args: &[&str], dir: &Path, stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("fmt")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn package(dir: &Path, edition: &str) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"{edition}\"\n"),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
}

#[test]
fn formats_stdin_to_stdout_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "2021");

    let first = florui_fmt(&["--stdin-filepath", "src/lib.rs"], dir.path(), DIRTY);

    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(stderr(&first), "");
    let formatted = stdout(&first);
    assert_ne!(formatted, DIRTY);
    assert!(formatted.contains("view!"));

    let second = florui_fmt(&["--stdin-filepath", "src/lib.rs"], dir.path(), &formatted);
    assert_eq!(stdout(&second), formatted, "formatting twice changed it");
}

#[test]
fn reads_the_rustfmt_configuration_next_to_the_given_path() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "2021");
    std::fs::write(dir.path().join("rustfmt.toml"), "hard_tabs = true\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();

    let path = dir.path().join("src/lib.rs");
    let output = florui_fmt(
        &["--stdin-filepath", path.to_str().unwrap()],
        elsewhere.path(),
        "fn f() {\n    let x = 1;\n}\n",
    );

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output), "fn f() {\n\tlet x = 1;\n}\n");
}

#[test]
fn takes_the_edition_of_the_package_that_owns_the_path() {
    let old = tempfile::tempdir().unwrap();
    package(old.path(), "2015");
    let current = tempfile::tempdir().unwrap();
    package(current.path(), "2021");
    let source = "async fn f() {}\n";

    let rejected = florui_fmt(&["--stdin-filepath", "src/lib.rs"], old.path(), source);
    let accepted = florui_fmt(&["--stdin-filepath", "src/lib.rs"], current.path(), source);

    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(stdout(&rejected), "");
    assert_eq!(accepted.status.code(), Some(0));
    assert_eq!(stdout(&accepted), source);
}

#[test]
fn a_parse_failure_leaves_stdout_empty_and_names_the_path() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "2021");

    let output = florui_fmt(
        &["--stdin-filepath", "src/lib.rs"],
        dir.path(),
        "fn broken( {\n",
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).contains("lib.rs"), "{}", stderr(&output));
}

#[test]
fn check_reports_a_difference_through_the_exit_code_only() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "2021");

    let dirty = florui_fmt(
        &["--check", "--stdin-filepath", "src/lib.rs"],
        dir.path(),
        DIRTY,
    );
    let clean = florui_fmt(
        &["--check", "--stdin-filepath", "src/lib.rs"],
        dir.path(),
        "fn f() {}\n",
    );

    assert_eq!(dirty.status.code(), Some(1));
    assert_eq!(stdout(&dirty), "");
    assert_eq!(clean.status.code(), Some(0));
    assert_eq!(stdout(&clean), "");
}

#[test]
fn rejects_a_path_that_is_not_rust_and_a_path_list_beside_stdin() {
    let dir = tempfile::tempdir().unwrap();

    let not_rust = florui_fmt(&["--stdin-filepath", "notes.txt"], dir.path(), "x");
    let both = florui_fmt(
        &["--stdin-filepath", "a.rs", "b.rs"],
        dir.path(),
        "fn f() {}\n",
    );

    assert_eq!(not_rust.status.code(), Some(2));
    assert_eq!(stdout(&not_rust), "");
    assert!(!both.status.success());
    assert_eq!(stdout(&both), "");
}

#[test]
fn file_mode_reads_the_configuration_beside_a_relative_path() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "2021");
    std::fs::write(dir.path().join("rustfmt.toml"), "hard_tabs = true\n").unwrap();
    std::fs::write(dir.path().join("a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();

    let output = florui_fmt(&["a.rs"], dir.path(), "");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
        "fn f() {\n\tlet x = 1;\n}\n"
    );
}
