//! `florui doctor` must leave the project as it found it, a missing
//! `Cargo.lock` included, and must not report a lockfile it did not find.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
}

fn doctor(dir: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(["doctor", "--json"])
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn lockfile_status(report: &Value) -> String {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "project.lockfile_readable")
        .map(|check| check["status"].as_str().unwrap().to_string())
        .expect("no lockfile check")
}

#[test]
fn doctor_does_not_create_a_missing_lockfile_and_says_there_is_none() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path());

    let report = doctor(&dir.path().join("src"));

    assert!(
        !dir.path().join("Cargo.lock").exists(),
        "doctor created a Cargo.lock"
    );
    assert_eq!(lockfile_status(&report), "warning");
}

#[test]
fn doctor_leaves_an_existing_lockfile_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path());
    let lock = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(lock.status.success());
    let before = std::fs::read(dir.path().join("Cargo.lock")).unwrap();

    let report = doctor(dir.path());

    assert_eq!(
        std::fs::read(dir.path().join("Cargo.lock")).unwrap(),
        before
    );
    assert_eq!(lockfile_status(&report), "pass");
}
