//! `florui doctor --artifacts` against the real binary: each case stages a
//! real build of a tiny project, changes one thing, and reads the report.

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><circle cx="32" cy="32" r="30" fill="#c81e3c"/></svg>"##;
const CONFIG: &str = "schema_version = 1\n\n[app]\nname = \"Garden\"\nidentifier = \"com.example.garden\"\ndescription = \"A workspace\"\n\n[app.icons]\nsource = \"icon.svg\"\n\n[bundle]\npublisher = \"Floregreen\"\ncopyright = \"Copyright 2026 Garden\"\n";

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn manifest(version: &str, extra: &str) -> String {
    format!("[package]\nname = \"garden\"\nversion = \"{version}\"\nedition = \"2021\"\n{extra}\n")
}

/// A project directory with a space in its name, built once.
struct Project {
    _holder: tempfile::TempDir,
    dir: PathBuf,
}

impl Project {
    fn staged(&self) -> PathBuf {
        self.dir.join("target/florui-build/garden/native")
    }
}

fn build(extra_manifest: &str, setup: impl FnOnce(&Path)) -> Project {
    let holder = tempfile::tempdir().unwrap();
    let dir = holder.path().join("my project");
    std::fs::create_dir_all(&dir).unwrap();
    write(&dir, "Cargo.toml", &manifest("1.0.0", extra_manifest));
    write(&dir, "src/main.rs", "fn main() {}\n");
    write(&dir, "florui.config.toml", CONFIG);
    write(&dir, "icon.svg", SVG);
    setup(&dir);
    let output = Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("build")
        .current_dir(&dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    Project {
        _holder: holder,
        dir,
    }
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn doctor(cwd: &Path, args: &[&str]) -> (Output, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(["doctor", "--json"])
        .args(args)
        .current_dir(cwd)
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

fn artifact_checks(report: &Value) -> Vec<(String, String)> {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["category"] == "artifacts")
        .map(|check| {
            (
                check["id"].as_str().unwrap().to_string(),
                check["status"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn status(report: &Value, id: &str) -> String {
    check(report, id)["status"].as_str().unwrap().to_string()
}

const STAGED: &str = "target/florui-build/garden/native";

/// Every file under `dir` with its bytes, to prove a run wrote nothing.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push((path.clone(), std::fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Deletes the icon group from an executable, as an edit after the build.
fn remove_icon_group(exe: &Path) {
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };
    // SAFETY: the path is NUL-terminated; a null data pointer with a zero size
    // deletes the resource; the update is finished before returning.
    unsafe {
        let handle = BeginUpdateResourceW(wide(exe).as_ptr(), 0);
        assert!(!handle.is_null());
        assert_ne!(
            UpdateResourceW(handle, 14 as _, 1 as _, 0, std::ptr::null(), 0),
            0
        );
        assert_ne!(EndUpdateResourceW(handle, 0), 0);
    }
}

#[test]
fn an_untouched_build_passes_every_check_from_a_nested_directory_and_writes_nothing() {
    let project = build("", committed);
    let nested = project.dir.join("src");
    let before = snapshot(&project.dir);

    let (output, report) = doctor(&nested, &["--artifacts", &format!("../{STAGED}")]);

    assert_eq!(output.status.code(), Some(0), "{report}");
    let checks = artifact_checks(&report);
    assert_eq!(checks.len(), 6, "{checks:?}");
    for (id, status) in &checks {
        assert_eq!(status, "pass", "{id}: {}", check(&report, id));
    }
    assert_eq!(snapshot(&project.dir), before, "doctor changed something");
}

#[test]
fn without_artifacts_no_artifact_check_is_reported_at_all() {
    let project = build("", |_| {});

    let (_, report) = doctor(&project.dir, &[]);

    assert!(artifact_checks(&report).is_empty(), "{report}");
}

#[test]
fn an_executable_changed_after_the_build_fails_its_hash() {
    let project = build("", |_| {});
    let exe = project.staged().join("garden.exe");
    let mut bytes = std::fs::read(&exe).unwrap();
    bytes.push(0);
    std::fs::write(&exe, bytes).unwrap();

    let (output, report) = doctor(&project.dir, &["--artifacts", STAGED]);

    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(status(&report, "artifacts.files"), "fail");
    assert!(
        check(&report, "artifacts.files")["evidence"]
            .as_str()
            .unwrap()
            .contains("garden.exe"),
        "{report}"
    );
}

#[test]
fn a_stray_file_is_named() {
    let project = build("", |_| {});
    std::fs::write(project.staged().join("leftover.txt"), "old").unwrap();

    let (output, report) = doctor(&project.dir, &["--artifacts", STAGED]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        check(&report, "artifacts.files")["evidence"]
            .as_str()
            .unwrap()
            .contains("leftover.txt is in the directory but not in the report")
    );
}

#[test]
fn a_missing_or_truncated_report_fails_with_the_command_that_stages_one() {
    let project = build("", |_| {});
    let path = project.staged().join("report.json");

    std::fs::write(&path, "{\"schema_vers").unwrap();
    let (truncated, truncated_report) = doctor(&project.dir, &["--artifacts", STAGED]);
    std::fs::remove_file(&path).unwrap();
    let (missing, missing_report) = doctor(&project.dir, &["--artifacts", STAGED]);
    let (absent, absent_report) = doctor(&project.dir, &["--artifacts", "target/nowhere"]);

    for (output, report) in [
        (truncated, truncated_report),
        (missing, missing_report),
        (absent, absent_report),
    ] {
        assert_eq!(output.status.code(), Some(1), "{report}");
        assert_eq!(status(&report, "artifacts.report"), "fail");
        assert!(
            check(&report, "artifacts.report")["remediation"]
                .as_str()
                .unwrap()
                .contains("florui build"),
            "{report}"
        );
        assert_eq!(status(&report, "artifacts.files"), "unknown");
    }
}

#[test]
fn linked_developer_tooling_is_a_warning_that_strict_turns_into_a_failure() {
    let project = build(
        "\n[dependencies]\nflorui-devtools = { path = \"florui-devtools\" }\n",
        |dir| {
            write(
                dir,
                "florui-devtools/Cargo.toml",
                "[package]\nname = \"florui-devtools\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            );
            write(dir, "florui-devtools/src/lib.rs", "");
        },
    );

    let (lenient, lenient_report) = doctor(&project.dir, &["--artifacts", STAGED]);
    let (strict, _) = doctor(&project.dir, &["--artifacts", STAGED, "--strict"]);

    assert_eq!(lenient.status.code(), Some(0), "{lenient_report}");
    assert_eq!(status(&lenient_report, "artifacts.exposure"), "warning");
    assert_eq!(strict.status.code(), Some(1));
}

#[test]
fn a_configuration_change_after_the_build_is_reported_as_stale_branding() {
    let project = build("", |_| {});
    write(&project.dir, "Cargo.toml", &manifest("2.0.0", ""));
    write(
        &project.dir,
        "florui.config.toml",
        &CONFIG.replace("name = \"Garden\"", "name = \"Garden Two\""),
    );

    let (lenient, report) = doctor(&project.dir, &["--artifacts", STAGED]);
    let (strict, _) = doctor(&project.dir, &["--artifacts", STAGED, "--strict"]);

    assert_eq!(lenient.status.code(), Some(0), "{report}");
    assert_eq!(status(&report, "artifacts.identity"), "warning");
    let evidence = check(&report, "artifacts.identity")["evidence"]
        .as_str()
        .unwrap();
    assert!(
        evidence.contains("Garden Two") && evidence.contains("2.0.0"),
        "{evidence}"
    );
    assert_eq!(strict.status.code(), Some(1));
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output));
}

/// Puts the project under git with everything committed, lockfile included,
/// so a build from it is a build of a clean tree.
fn committed(dir: &Path) {
    let lock = Command::new("cargo")
        .arg("generate-lockfile")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(lock.status.success(), "{}", text(&lock));
    write(dir, ".gitignore", "target\n");
    git(dir, &["init", "-q"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "first"]);
}

#[test]
fn a_different_commit_than_the_build_was_made_from_is_reported() {
    let project = build("", committed);
    let (_, before) = doctor(&project.dir, &["--artifacts", STAGED]);
    assert_eq!(status(&before, "artifacts.provenance"), "pass", "{before}");
    write(&project.dir, "src/main.rs", "fn main() { let _ = 1; }\n");
    git(&project.dir, &["commit", "-q", "-a", "-m", "second"]);

    let (_, after) = doctor(&project.dir, &["--artifacts", STAGED]);

    assert_eq!(status(&after, "artifacts.provenance"), "warning");
    assert!(
        check(&after, "artifacts.provenance")["evidence"]
            .as_str()
            .unwrap()
            .contains("built from commit"),
        "{after}"
    );
}

#[test]
fn a_build_that_recorded_no_git_commit_is_unknown_not_a_pass() {
    let project = build("", |_| {});

    let (lenient, report) = doctor(&project.dir, &["--artifacts", STAGED]);
    let (strict, _) = doctor(&project.dir, &["--artifacts", STAGED, "--strict"]);

    assert_eq!(
        status(&report, "artifacts.provenance"),
        "unknown",
        "{report}"
    );
    assert_eq!(lenient.status.code(), Some(0));
    assert_eq!(strict.status.code(), Some(1));
}

#[test]
fn the_executable_resources_are_read_from_the_file_not_taken_from_the_report() {
    let project = build("", |_| {});
    // Remove the icon, then make the file match its report again so only the
    // resource check can notice.
    let exe = project.staged().join("garden.exe");
    remove_icon_group(&exe);
    let report_path = project.staged().join("report.json");
    let mut report: Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    let hashes = {
        let bytes = std::fs::read(&exe).unwrap();
        let sha: String = {
            use sha2::{Digest, Sha256};
            Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        (bytes.len(), sha, blake3::hash(&bytes).to_hex().to_string())
    };
    for file in report["files"].as_array_mut().unwrap() {
        if file["file"] == "garden.exe" {
            file["bytes"] = hashes.0.into();
            file["sha256"] = hashes.1.clone().into();
            file["blake3"] = hashes.2.clone().into();
        }
    }
    std::fs::write(&report_path, report.to_string()).unwrap();

    let (output, report) = doctor(&project.dir, &["--artifacts", STAGED]);

    assert_eq!(status(&report, "artifacts.files"), "pass", "{report}");
    assert_eq!(status(&report, "artifacts.resources"), "fail", "{report}");
    assert!(
        check(&report, "artifacts.resources")["evidence"]
            .as_str()
            .unwrap()
            .contains("icon has no image(s) but the build wrote 7"),
        "{report}"
    );
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn the_web_target_reports_output_checks_as_not_yet_supported() {
    let project = build("", |_| {});

    let (_, report) = doctor(&project.dir, &["--target", "web", "--artifacts", STAGED]);

    assert_eq!(status(&report, "artifacts.unsupported_target"), "skipped");
    assert_eq!(artifact_checks(&report).len(), 1);
}

const LOCALIZED_CONFIG: &str = "schema_version = 1\n\n[app]\nname = \"Garden\"\nidentifier = \"com.example.garden\"\ndescription = \"A workspace\"\ndefault_locale = \"en\"\n\n[app.icons]\nsource = \"icon.svg\"\n\n[app.locales.en]\nname = \"Garden\"\ndescription = \"A workspace\"\n\n[app.locales.pt-BR]\nname = \"Jardim\"\ndescription = \"Um espaco para ideias\"\n\n[app.locales.tlh]\nname = \"Beq\"\n\n[bundle]\npublisher = \"Floregreen\"\ncopyright = \"Copyright 2026 Garden\"\n";

#[test]
fn each_locale_is_written_read_back_and_the_unmapped_one_is_reported() {
    let project = build("", |dir| write(dir, "florui.config.toml", LOCALIZED_CONFIG));

    let (output, report) = doctor(&project.dir, &["--artifacts", STAGED]);

    assert_eq!(status(&report, "artifacts.resources"), "pass", "{report}");
    assert_eq!(output.status.code(), Some(0), "{report}");
    let built: Value =
        serde_json::from_slice(&std::fs::read(project.staged().join("report.json")).unwrap())
            .unwrap();
    let info = &built["resources"]["version_info"][0];
    let localized = info["localized"].as_array().unwrap();
    let tag = |name: &str| localized.iter().find(|l| l["tag"] == name).unwrap();
    assert_eq!(tag("en")["language_id"], "0409");
    assert_eq!(tag("pt-BR")["language_id"], "0416");
    assert_eq!(tag("pt-BR")["product_name"], "Jardim");
    assert_eq!(tag("pt-BR")["embedded"], true);
    assert_eq!(tag("tlh")["embedded"], false);
    let warnings = built["resources"]["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w.as_str().unwrap().contains("tlh")
            && w.as_str().unwrap().contains("no Windows language")),
        "{warnings:?}"
    );
}

#[test]
fn a_localized_name_that_differs_from_the_executables_is_a_failure() {
    let project = build("", |dir| write(dir, "florui.config.toml", LOCALIZED_CONFIG));
    let path = project.staged().join("report.json");
    let tampered = std::fs::read_to_string(&path).unwrap().replace(
        "\"product_name\": \"Jardim\"",
        "\"product_name\": \"Quintal\"",
    );
    std::fs::write(&path, tampered).unwrap();

    let (output, report) = doctor(&project.dir, &["--artifacts", STAGED]);

    assert_eq!(output.status.code(), Some(1), "{report}");
    assert_eq!(status(&report, "artifacts.resources"), "fail");
    let evidence = check(&report, "artifacts.resources")["evidence"]
        .as_str()
        .unwrap();
    assert!(
        evidence.contains("0416") && evidence.contains("Quintal"),
        "{evidence}"
    );
}
