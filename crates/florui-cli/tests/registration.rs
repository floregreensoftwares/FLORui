//! `florui register`, `unregister` and `doctor --registration` against the
//! real binary. The cases that must write nothing run everywhere; the one
//! that registers with the real per-user registry and launches the staged
//! executable through the shell is `#[ignore]`d and run on request:
//! `cargo test -p florui-cli --test registration -- --ignored`.

#![cfg(windows)]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const MAIN: &str = r#"use std::io::Write;
fn main() {
    let exe = std::env::current_exe().unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exe.with_file_name("launched.txt"))
        .unwrap();
    writeln!(log, "{}", args.join("|")).unwrap();
}
"#;

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn florui(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .args(args)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {}", text(output)))
}

struct Project {
    _holder: tempfile::TempDir,
    dir: PathBuf,
}

impl Project {
    fn staged(&self) -> PathBuf {
        self.dir.join("target/florui-build/garden/native")
    }
}

/// A built project with a space and an accented letter in its path, whose
/// executable appends what it was launched with to `launched.txt`.
fn built(config: &str) -> Project {
    let holder = tempfile::tempdir().unwrap();
    let dir = holder.path().join("meu jardim ação");
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "Cargo.toml",
        "[package]\nname = \"garden\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
    );
    write(&dir, "src/main.rs", MAIN);
    write(&dir, "florui.config.toml", config);
    let output = florui(&dir, &["build"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    Project {
        _holder: holder,
        dir,
    }
}

fn config(identifier: &str, schemes: &str, extra: &str) -> String {
    format!(
        "schema_version = 1\n[app]\nname = \"Garden\"\nidentifier = \"{identifier}\"\n\n\
         [app.activation]\nurl_schemes = [{schemes}]\n{extra}"
    )
}

fn reg(args: &[&str]) -> Output {
    Command::new("reg").args(args).output().unwrap()
}

fn key_exists(path: &str) -> bool {
    reg(&["query", path]).status.success()
}

#[test]
fn a_dry_run_says_what_it_would_register_and_writes_nothing() {
    let id = format!("com.floruitest.dry{}", std::process::id());
    let scheme = format!("floruitestdry{}", std::process::id());
    let project = built(&config(
        &id,
        &format!("\"{scheme}\""),
        "request_default = [\"%S\"]\n"
            .replace("%S", &scheme)
            .as_str(),
    ));

    let output = florui(&project.dir, &["register", "--dry-run", "--json"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let report = json(&output);
    assert_eq!(report["outcome"], "dry_run");
    assert_eq!(report["url_schemes"][0], scheme.as_str());
    assert_eq!(report["default_requests"][0], scheme.as_str());
    assert!(
        report["executable"]
            .as_str()
            .unwrap()
            .ends_with("garden.exe")
    );
    assert!(!key_exists(&format!("HKCU\\Software\\Classes\\{scheme}")));
    assert!(!key_exists(&format!("HKCU\\Software\\Florui\\{id}")));
}

#[test]
fn a_reserved_scheme_is_refused_and_nothing_is_written() {
    let id = format!("com.floruitest.reserved{}", std::process::id());
    let project = built(&config(&id, "\"mailto\"", ""));

    let output = florui(&project.dir, &["register", "--json"]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report = json(&output);
    assert_eq!(report["outcome"], "conflict");
    assert!(
        report["conflicts"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("reserved"),
        "{report}"
    );
    assert!(!key_exists(&format!("HKCU\\Software\\Florui\\{id}")));
    let (_, doctor) = (
        (),
        json(&florui(
            &project.dir,
            &["doctor", "--registration", "--json"],
        )),
    );
    let conflicts = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "registration.conflicts")
        .unwrap();
    assert_eq!(conflicts["status"], "fail");
}

#[test]
fn an_executable_that_is_not_the_one_the_build_wrote_is_refused() {
    let id = format!("com.floruitest.tamper{}", std::process::id());
    let project = built(&config(&id, "\"floruitesttamper\"", ""));
    let exe = project.staged().join("garden.exe");
    let mut bytes = std::fs::read(&exe).unwrap();
    bytes.push(0);
    std::fs::write(&exe, bytes).unwrap();

    let output = florui(&project.dir, &["register"]);

    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        text(&output).contains("not the file the build wrote"),
        "{}",
        text(&output)
    );
    assert!(!key_exists(&format!("HKCU\\Software\\Florui\\{id}")));
}

#[test]
fn a_project_that_declares_nothing_has_nothing_to_register() {
    let project = built("schema_version = 1\n[app]\nidentifier = \"com.floruitest.none\"\n");

    let output = florui(&project.dir, &["register", "--json"]);

    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(json(&output)["outcome"], "nothing_declared");
    let doctor = json(&florui(
        &project.dir,
        &["doctor", "--registration", "--json"],
    ));
    let declared = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "registration.declared")
        .unwrap();
    assert_eq!(declared["status"], "not_applicable");
}

#[test]
fn two_environments_that_share_a_scheme_are_reported_as_contending() {
    let id = "com.floruitest.iso";
    let project = built(&format!(
        "{}[environments.development.app]\nidentifier = \"{id}.dev\"\n",
        config(id, "\"floruitestiso\"", "")
    ));

    let doctor = json(&florui(
        &project.dir,
        &["doctor", "--registration", "--json"],
    ));
    let isolation = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "registration.isolation")
        .unwrap();

    assert_eq!(isolation["status"], "warning", "{isolation}");
    assert!(
        isolation["evidence"]
            .as_str()
            .unwrap()
            .contains("floruitestiso://")
    );

    // Giving the environment its own scheme separates them.
    let separate = built(&format!(
        "{}[environments.development.app]\nidentifier = \"{id}.dev\"\n\
         [environments.development.app.activation]\nurl_schemes = [\"floruitestisodev\"]\n",
        config(id, "\"floruitestiso\"", "")
    ));
    let doctor = json(&florui(
        &separate.dir,
        &["doctor", "--registration", "--json"],
    ));
    let isolation = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "registration.isolation")
        .unwrap();
    assert_eq!(isolation["status"], "pass", "{isolation}");
}

/// Removes whatever a test registered, even when it fails halfway.
struct Cleanup {
    dir: PathBuf,
    keys: Vec<String>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = florui(&self.dir, &["unregister"]);
        for key in &self.keys {
            let _ = reg(&["delete", key, "/f"]);
        }
    }
}

fn wait_for_launch(log: &Path, expected: &str) -> bool {
    for _ in 0..50 {
        if std::fs::read_to_string(log).is_ok_and(|text| text.lines().any(|l| l == expected)) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

#[test]
#[ignore = "writes to the real per-user registry and launches a process through the shell"]
fn registering_makes_a_link_launch_the_executable_and_unregistering_removes_it() {
    let pid = std::process::id();
    let id = format!("com.floruitest.e2e{pid}");
    let scheme = format!("floruitest{pid}");
    let extension = format!("floruitest{pid}");
    let project = built(&config(
        &id,
        &format!("\"{scheme}\""),
        &format!(
            "request_default = [\"{scheme}\", \".{extension}\"]\n\n\
             [[app.activation.file_associations]]\nextension = \"{extension}\"\n\
             identity = \"doc\"\ndescription = \"A test file\"\n"
        ),
    ));
    let _cleanup = Cleanup {
        dir: project.dir.clone(),
        keys: vec![
            format!("HKCU\\Software\\Classes\\{scheme}"),
            format!("HKCU\\Software\\Classes\\{id}.url.{scheme}"),
            format!("HKCU\\Software\\Classes\\{id}.doc"),
            format!("HKCU\\Software\\Classes\\.{extension}"),
            format!("HKCU\\Software\\Florui\\{id}"),
        ],
    };
    let registered_apps_before = reg(&["query", "HKCU\\Software\\RegisteredApplications"]);

    let output = florui(&project.dir, &["register", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(json(&output)["outcome"], "registered");

    // The registry holds what the contract says, read by the system's own tool.
    let command = reg(&[
        "query",
        &format!("HKCU\\Software\\Classes\\{scheme}\\shell\\open\\command"),
    ]);
    assert!(text(&command).contains("garden.exe"), "{}", text(&command));
    assert!(text(&command).contains("\"%1\""), "{}", text(&command));
    assert!(key_exists(&format!(
        "HKCU\\Software\\Classes\\.{extension}\\OpenWithProgids"
    )));
    assert!(
        text(&reg(&[
            "query",
            "HKCU\\Software\\RegisteredApplications",
            "/v",
            &id
        ]))
        .contains("Capabilities")
    );
    // No default was set for the file type.
    assert!(
        !text(&reg(&[
            "query",
            &format!("HKCU\\Software\\Classes\\.{extension}")
        ]))
        .contains("REG_SZ    "),
        "the extension must have no default value"
    );

    // A cold launch through the shell passes the URL, escapes and all, as one argument.
    let log = project.staged().join("launched.txt");
    let url = format!("{scheme}://open/a%20b?x=1&y=%C3%A7");
    // The line is quoted whole: `&` would otherwise end the command.
    let status = Command::new("cmd")
        .arg("/c")
        .raw_arg(format!("start \"\" \"{url}\""))
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        wait_for_launch(&log, &url),
        "the executable was not launched with the URL"
    );

    // The file type's command is the same quoted command: run it for a path with a space and an accent.
    let file = project.dir.join(format!("um arquivo ação.{extension}"));
    std::fs::write(&file, "x").unwrap();
    // Read through PowerShell as UTF-8: `reg.exe` prints in the OEM code page and
    // would garble the accent that the registry holds correctly.
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 (Get-Item 'HKCU:\\Software\\Classes\\{id}.doc\\shell\\open\\command').GetValue('')"
            ),
        ])
        .output()
        .unwrap();
    let command = String::from_utf8(output.stdout).unwrap();
    let template = command.trim().replace("%1", &file.display().to_string());
    let status = Command::new("cmd")
        .arg("/c")
        .raw_arg(format!("\"{template}\""))
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the file command {template:?} failed: {status:?}"
    );
    assert!(wait_for_launch(&log, &file.display().to_string()));

    // The doctor reads it back, and notices a key that went missing.
    let state = |dir: &Path| {
        let doctor = json(&florui(dir, &["doctor", "--registration", "--json"]));
        doctor["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "registration.state")
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(state(&project.dir), "pass");
    reg(&[
        "delete",
        &format!("HKCU\\Software\\Classes\\{scheme}"),
        "/f",
    ]);
    assert_eq!(state(&project.dir), "fail");

    // Registering again repairs it, and unregistering leaves nothing behind.
    assert_eq!(florui(&project.dir, &["register"]).status.code(), Some(0));
    assert_eq!(state(&project.dir), "pass");
    let output = florui(&project.dir, &["unregister", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(json(&output)["outcome"], "removed");
    for key in [
        format!("HKCU\\Software\\Classes\\{scheme}"),
        format!("HKCU\\Software\\Classes\\{id}.url.{scheme}"),
        format!("HKCU\\Software\\Classes\\{id}.doc"),
        format!("HKCU\\Software\\Classes\\.{extension}"),
        format!("HKCU\\Software\\Florui\\{id}"),
    ] {
        assert!(!key_exists(&key), "{key} is still there");
    }
    assert_eq!(
        text(&reg(&["query", "HKCU\\Software\\RegisteredApplications"])),
        text(&registered_apps_before),
        "the list of registered applications is as it was"
    );
}
