//! What `florui build` does with `[bundle]`'s distribution metadata: stages
//! the license notice and says in the report what became of each field.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use sha2::{Digest, Sha256};

const EXE: &str = std::env::consts::EXE_SUFFIX;

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn project(dir: &Path, manifest_extra: &str, config: Option<&str>) {
    write(
        dir,
        "Cargo.toml",
        &format!(
            "[package]\nname = \"garden\"\nversion = \"1.0.0\"\nedition = \"2021\"\n{manifest_extra}\n"
        ),
    );
    write(dir, "src/main.rs", "fn main() {}\n");
    if let Some(config) = config {
        write(dir, "florui.config.toml", config);
    }
}

fn florui_build(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("build")
        .current_dir(dir)
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

fn staged(dir: &Path) -> PathBuf {
    dir.join("target/florui-build/garden/native")
}

fn report(dir: &Path) -> Value {
    let text = std::fs::read_to_string(staged(dir).join("report.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn field<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["distribution"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == name)
        .unwrap_or_else(|| panic!("no {name} in {report}"))
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

const FULL: &str = "schema_version = 1\n\n[app]\nname = \"Garden\"\nidentifier = \"com.example.garden\"\n\n[bundle]\npublisher = \"Floregreen\"\ncopyright = \"Copyright 2026 Garden\"\nlicense_file = \"LICENSE-MIT\"\ncategory = \"productivity\"\n";

#[test]
fn the_license_file_is_staged_as_a_notice_and_every_field_is_accounted_for() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "license = \"MIT\"\nhomepage = \"https://example.com/garden\"\n",
        Some(FULL),
    );
    write(dir.path(), "LICENSE-MIT", "The MIT License\n");

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    let notice = std::fs::read(staged(dir.path()).join("LICENSE-MIT")).unwrap();
    assert_eq!(notice, b"The MIT License\n");
    let report = report(dir.path());
    let listed = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["file"] == "LICENSE-MIT")
        .expect("the notice is not listed in the report");
    assert_eq!(listed["sha256"], sha(&notice).as_str());

    let copyright = field(&report, "bundle.copyright");
    assert_eq!(copyright["value"], "Copyright 2026 Garden");
    assert_eq!(
        (copyright["source"].as_str(), copyright["mapping"].as_str()),
        (Some("config"), Some("embedded"))
    );
    let license = field(&report, "bundle.license");
    assert_eq!(license["value"], "MIT");
    assert_eq!(license["source"], "cargo");
    assert_eq!(license["mapping"], "not_represented");
    assert_eq!(field(&report, "bundle.homepage")["source"], "cargo");
    assert_eq!(
        field(&report, "bundle.homepage")["mapping"],
        "not_represented"
    );
    assert_eq!(
        field(&report, "bundle.category")["mapping"],
        "not_represented"
    );
    assert_eq!(field(&report, "bundle.license_file")["mapping"], "staged");
    assert_eq!(
        field(&report, "bundle.license_file")["value"],
        "LICENSE-MIT"
    );
}

#[test]
fn a_build_without_a_bundle_still_works_and_reports_every_field_absent() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path(), "", None);

    let output = florui_build(dir.path());

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let report = report(dir.path());
    for name in [
        "bundle.publisher",
        "bundle.copyright",
        "bundle.license",
        "bundle.license_file",
        "bundle.category",
        "bundle.homepage",
    ] {
        assert_eq!(field(&report, name)["mapping"], "absent", "{name}");
        assert_eq!(field(&report, name)["source"], "none", "{name}");
    }
}

#[test]
fn a_declared_license_file_that_is_missing_fails_the_build_and_stages_nothing() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path(), "", Some(FULL));
    // An earlier staged build must not survive as if it were this one.
    write(
        dir.path(),
        "target/florui-build/garden/native/garden.old",
        "old",
    );

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("bundle.license_file"), "{all}");
    assert!(!staged(dir.path()).join("garden.old").exists());
    assert!(!staged(dir.path()).join("report.json").exists());
}

#[test]
fn a_notice_that_would_replace_the_executable_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let name = format!("garden{EXE}");
    project(dir.path(), "", Some(&FULL.replace("LICENSE-MIT", &name)));
    write(dir.path(), &name, "not an executable");

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("has the name of an executable"), "{all}");
}

#[test]
fn an_invalid_bundle_value_stops_the_build_before_cargo_runs() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "",
        Some("schema_version = 1\n[bundle]\nlicense = \"Aache-2.0\"\n"),
    );

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(
        all.contains("bundle.license") && all.contains("SPDX"),
        "{all}"
    );
    assert!(
        !dir.path().join("target/release").exists(),
        "cargo ran: {all}"
    );
}
