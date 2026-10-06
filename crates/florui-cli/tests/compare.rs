//! `florui compare` against the real binary, in a temporary workspace that
//! holds a copy of the repository's reference fixtures.
//!
//! Tests that need a browser use `FLORUI_CHROMIUM` or an installed Chrome and
//! say so on stderr when there is none, instead of passing without running.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A workspace with one crate and the reference fixtures `div-default` and
/// `p-default`.
fn workspace(dir: &Path) {
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/reference");
    for name in ["assets", "div-default", "p-default"] {
        copy_dir(
            &source.join(name),
            &dir.join("fixtures/reference").join(name),
        );
    }
}

fn chrome() -> Option<PathBuf> {
    let from_env = std::env::var_os("FLORUI_CHROMIUM").map(PathBuf::from);
    let installed = PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe");
    from_env.or(Some(installed)).filter(|path| path.is_file())
}

fn florui_compare(cwd: &Path, chromium: Option<&Path>, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_florui"));
    command
        .arg("compare")
        .args(args)
        .current_dir(cwd)
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("FLORUI_CHROMIUM");
    if let Some(chromium) = chromium {
        command.env("FLORUI_CHROMIUM", chromium);
    }
    command.output().unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Every file under `dir` with its bytes, for before/after comparison.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.push((path, bytes));
            }
        }
    }
    files.sort();
    files
}

macro_rules! need_chrome {
    () => {
        match chrome() {
            Some(path) => path,
            None => {
                eprintln!("skipped: no Chrome (set FLORUI_CHROMIUM)");
                return;
            }
        }
    };
}

#[test]
fn an_unknown_fixture_name_fails_and_lists_the_available_ones() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_compare(dir.path(), None, &["--fixture", "nope"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(
        all.contains("\"nope\"") && all.contains("div-default, p-default"),
        "{all}"
    );
    assert!(
        !all.contains("Chromium"),
        "must fail before looking for a browser: {all}"
    );
}

#[test]
fn a_name_selects_one_fixture_from_a_nested_directory_and_output_goes_to_the_project_target() {
    let chromium = need_chrome!();
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());
    let nested = dir.path().join("src");

    let output = florui_compare(&nested, Some(&chromium), &["--fixture", "div-default"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    assert!(
        all.contains("div-default") && !all.contains("p-default"),
        "{all}"
    );
    let artifacts = dir.path().join("target/florui-conformance/div-default");
    assert!(artifacts.join("report.json").is_file(), "{all}");
    assert!(
        !nested.join("target").exists(),
        "output followed the working directory"
    );
    assert!(
        all.contains("artifacts in"),
        "a pass must say where its artifacts are: {all}"
    );
}

#[test]
fn no_selection_compares_every_fixture_and_counts_them() {
    let chromium = need_chrome!();
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let output = florui_compare(dir.path(), Some(&chromium), &[]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    assert!(all.contains("2 of 2 fixtures match"), "{all}");
}

#[test]
fn a_failing_fixture_exits_1_and_the_fixture_is_left_untouched() {
    let chromium = need_chrome!();
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());
    // Chromium moves the box, Florui's render of the manifest does not.
    let html = dir.path().join("fixtures/reference/div-default/index.html");
    let text_html = std::fs::read_to_string(&html).unwrap();
    std::fs::write(
        &html,
        text_html.replace("<div id=\"el\">", "<div id=\"el\" style=\"margin:60px\">"),
    )
    .unwrap();
    let before = snapshot(&dir.path().join("fixtures"));

    let output = florui_compare(dir.path(), Some(&chromium), &["--fixture", "div-default"]);

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("differing pixels"), "{all}");
    assert_eq!(
        snapshot(&dir.path().join("fixtures")),
        before,
        "compare changed a fixture"
    );
}
