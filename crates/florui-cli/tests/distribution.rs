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

// ------------------------------------------------------------------- doctor

fn doctor(dir: &Path, args: &[&str]) -> (Output, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(["doctor", "--json"])
        .args(args)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    let json = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {}", text(&output)));
    (output, json)
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .unwrap_or_else(|| panic!("no check {id} in {report}"))
}

fn status(report: &Value, id: &str) -> String {
    check(report, id)["status"].as_str().unwrap().to_string()
}

fn distribution_ids(report: &Value) -> Vec<String> {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["category"] == "distribution")
        .map(|check| check["id"].as_str().unwrap().to_string())
        .collect()
}

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><circle cx="32" cy="32" r="30" fill="#c81e3c"/></svg>"##;

const COMPLETE: &str = "schema_version = 1\n\n[app]\nname = \"Garden\"\nidentifier = \"com.example.garden\"\n\n[app.icons]\nsource = \"icon.svg\"\n\n[bundle]\npublisher = \"Floregreen\"\ncopyright = \"Copyright 2026 Garden\"\nlicense = \"MIT OR Apache-2.0\"\nlicense_file = \"LICENSE-MIT\"\ncategory = \"productivity\"\nhomepage = \"https://example.com/garden\"\n";

fn complete_project(dir: &Path) {
    project(dir, "", Some(COMPLETE));
    write(dir, "LICENSE-MIT", "The MIT License\n");
    write(dir, "icon.svg", SVG);
}

#[test]
fn a_complete_project_passes_every_distribution_check_and_the_installer_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    complete_project(dir.path());

    let (output, report) = doctor(dir.path(), &["--distribution", "--strict"]);

    assert_eq!(output.status.code(), Some(0), "{report}");
    for id in [
        "distribution.identifier",
        "distribution.version",
        "distribution.publisher",
        "distribution.license",
        "distribution.license_file",
        "distribution.copyright",
        "distribution.category",
        "distribution.homepage",
        "distribution.icon",
    ] {
        assert_eq!(status(&report, id), "pass", "{id}: {}", check(&report, id));
    }
    assert_eq!(status(&report, "distribution.installer"), "skipped");
    assert!(!distribution_ids(&report).contains(&"distribution.packaging".to_string()));
}

#[test]
fn nothing_is_written_and_nothing_is_reported_without_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    complete_project(dir.path());
    // `cargo metadata`, which resolves the project, writes a missing lockfile
    // by itself; start from one so what is checked is doctor's own writing.
    let lock = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock));
    let before: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();

    let (_, with) = doctor(dir.path(), &["--distribution"]);
    let (_, without) = doctor(dir.path(), &[]);

    assert!(!distribution_ids(&with).is_empty());
    assert!(distribution_ids(&without).is_empty(), "{without}");
    let after: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after, "doctor created something");
}

#[test]
fn a_bare_project_fails_on_the_identifier_and_warns_on_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    project(dir.path(), "", None);

    let (output, report) = doctor(dir.path(), &["--distribution"]);

    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(status(&report, "distribution.identifier"), "fail");
    for id in [
        "distribution.publisher",
        "distribution.license",
        "distribution.license_file",
        "distribution.copyright",
        "distribution.icon",
    ] {
        assert_eq!(status(&report, id), "warning", "{id}");
    }
    assert_eq!(status(&report, "distribution.category"), "not_applicable");
    assert_eq!(status(&report, "distribution.version"), "pass");
}

#[test]
fn warnings_alone_pass_and_strict_turns_them_into_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "",
        Some("schema_version = 1\n[app]\nidentifier = \"com.example.garden\"\n"),
    );

    let (lenient, _) = doctor(dir.path(), &["--distribution"]);
    let (strict, _) = doctor(dir.path(), &["--distribution", "--strict"]);

    assert_eq!(lenient.status.code(), Some(0));
    assert_eq!(strict.status.code(), Some(1));
}

#[test]
fn an_invalid_identifier_a_missing_license_file_and_a_broken_icon_fail() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "",
        Some(
            &COMPLETE
                .replace("com.example.garden", "garden")
                .replace("LICENSE-MIT", "MISSING"),
        ),
    );
    write(dir.path(), "icon.svg", "<svg");

    let (output, report) = doctor(dir.path(), &["--distribution"]);

    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(status(&report, "distribution.identifier"), "fail");
    assert_eq!(status(&report, "distribution.license_file"), "fail");
    assert_eq!(status(&report, "distribution.icon"), "fail");
}

#[test]
fn a_version_that_does_not_map_to_a_windows_version_fails() {
    let dir = tempfile::tempdir().unwrap();
    project(
        dir.path(),
        "",
        Some(&COMPLETE.replace(
            "name = \"Garden\"\n",
            "name = \"Garden\"\nversion = \"1.70000.0\"\n",
        )),
    );
    write(dir.path(), "icon.svg", SVG);
    write(dir.path(), "LICENSE-MIT", "x");

    let (output, report) = doctor(dir.path(), &["--distribution"]);

    assert_eq!(status(&report, "distribution.version"), "fail", "{report}");
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn outside_a_project_the_distribution_checks_are_not_applicable() {
    let dir = tempfile::tempdir().unwrap();

    let (_, report) = doctor(dir.path(), &["--distribution"]);

    assert_eq!(status(&report, "distribution.project"), "not_applicable");
}

#[test]
fn the_web_target_keeps_its_unsupported_report() {
    let dir = tempfile::tempdir().unwrap();
    complete_project(dir.path());

    let (_, report) = doctor(dir.path(), &["--target", "web", "--distribution"]);

    assert_eq!(
        status(&report, "distribution.unsupported_target"),
        "skipped"
    );
}
