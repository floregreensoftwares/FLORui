//! `florui new` against the real binary: what it writes, and that it never
//! overwrites or half-creates a project.

use std::path::Path;
use std::process::{Command, Output};

fn florui_new(dir: &Path, name: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(["new", name])
        .current_dir(dir)
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn read(dir: &Path, relative: &str) -> String {
    std::fs::read_to_string(dir.join(relative)).unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn creates_the_project_with_the_name_and_version_filled_in() {
    let dir = tempfile::tempdir().unwrap();

    let output = florui_new(dir.path(), "my-app");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let project = dir.path().join("my-app");
    for file in [
        "Cargo.toml",
        "florui.config.toml",
        ".gitignore",
        "src/lib.rs",
        "src/app.rs",
        "src/app.css",
        "src/main.rs",
        "examples/dev.rs",
        "tests/app.rs",
    ] {
        let contents = read(&project, file);
        assert!(!contents.contains("{{"), "{file} keeps a placeholder");
    }

    let manifest = read(&project, "Cargo.toml");
    assert!(manifest.contains("name = \"my-app\""));
    assert!(manifest.contains("name = \"my_app\""));
    assert!(manifest.contains(&format!("florui = \"{}\"", env!("CARGO_PKG_VERSION"))));
    assert!(read(&project, "src/main.rs").contains("use my_app::app::"));
    assert!(read(&project, "florui.config.toml").contains("example = \"dev\""));
}

#[test]
fn a_bad_name_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();

    let output = florui_new(dir.path(), "1app");

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("1app"), "{}", stderr(&output));
    assert!(entries(dir.path()).is_empty());
}

#[test]
fn an_existing_project_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(florui_new(dir.path(), "my-app").status.code(), Some(0));
    let project = dir.path().join("my-app");
    std::fs::write(project.join("src/app.rs"), "// my own work\n").unwrap();
    std::fs::remove_file(project.join("tests/app.rs")).unwrap();

    let output = florui_new(dir.path(), "my-app");

    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("app.rs"), "{}", stderr(&output));
    assert_eq!(read(&project, "src/app.rs"), "// my own work\n");
    assert!(
        !project.join("tests/app.rs").exists(),
        "a refused run must not create the files that were free"
    );
}

#[test]
fn an_empty_directory_is_accepted_and_other_files_in_it_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("my-app");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("NOTES.md"), "keep me\n").unwrap();

    let output = florui_new(dir.path(), "my-app");

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    assert_eq!(read(&project, "NOTES.md"), "keep me\n");
    assert!(project.join("src/app.rs").exists());
}

/// The generated project depends on published versions, which do not exist
/// yet, so this builds it against this checkout through `[patch.crates-io]`
/// and the workspace's own lockfile (offline, same third-party versions).
/// Slow: run with `cargo test -p florui-cli --test new -- --ignored`.
#[test]
#[ignore = "builds a generated project against this checkout"]
fn the_generated_project_builds_and_passes_its_own_test() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .to_path_buf();
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(florui_new(dir.path(), "my-app").status.code(), Some(0));
    let project = dir.path().join("my-app");

    let mut patches = String::from("[patch.crates-io]\n");
    for entry in std::fs::read_dir(workspace.join("crates")).unwrap() {
        let path = entry.unwrap().path();
        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            let path = path.to_string_lossy().replace('\\', "/");
            patches.push_str(&format!("{name} = {{ path = \"{path}\" }}\n"));
        }
    }
    std::fs::create_dir(project.join(".cargo")).unwrap();
    std::fs::write(project.join(".cargo/config.toml"), patches).unwrap();
    std::fs::copy(workspace.join("Cargo.lock"), project.join("Cargo.lock")).unwrap();

    let output = Command::new("cargo")
        .args(["test", "--offline"])
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .output()
        .unwrap();

    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        stderr(&output)
    );
    assert!(output.status.success(), "{log}");
    assert!(log.contains("clicking_the_button_counts ... ok"), "{log}");

    let build = Command::new("cargo")
        .args(["build", "--offline", "--bin", "my-app", "--example", "dev"])
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", workspace.join("target"))
        .output()
        .unwrap();
    assert!(build.status.success(), "{}", stderr(&build));
}
