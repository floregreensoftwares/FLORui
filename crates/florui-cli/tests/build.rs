//! `florui build` against the real binary, in temporary projects of trivial
//! crates: what it builds, stages and reports, and what it rejects under
//! `--strict`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::{Digest, Sha256};

const EXE: &str = std::env::consts::EXE_SUFFIX;

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn package(name: &str, extra: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{extra}\n")
}

fn florui_build(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("build")
        .args(args)
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn staged(dir: &Path, package: &str) -> PathBuf {
    dir.join("target/florui-build").join(package).join("native")
}

fn report(dir: &Path, package: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(staged(dir, package).join("report.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// One binary crate, `app`.
fn single_app(dir: &Path) {
    write(dir, "Cargo.toml", &package("app", ""));
    write(dir, "src/main.rs", "fn main() {}\n");
}

/// A crate that stands in for a Florui one: same name, so the build sees what
/// it would see.
fn stand_in(dir: &Path, name: &str, features: &str) {
    write(dir, &format!("{name}/Cargo.toml"), &package(name, features));
    write(dir, &format!("{name}/src/lib.rs"), "");
}

#[test]
fn builds_the_package_stages_the_executable_and_reports_hashes_that_match_the_file() {
    let dir = tempfile::tempdir().unwrap();
    single_app(dir.path());

    let output = florui_build(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));
    let staged_exe = staged(dir.path(), "app").join(format!("app{EXE}"));
    let built = std::fs::read(dir.path().join("target/release").join(format!("app{EXE}"))).unwrap();
    let copy = std::fs::read(&staged_exe).unwrap();
    // On Windows the copy also carries the application's version information;
    // what cargo produced is recorded separately and is never touched.
    if cfg!(windows) {
        assert_ne!(copy, built, "the copy carries no resources");
    } else {
        assert_eq!(copy, built, "the staged file is not what cargo built");
    }

    let report = report(dir.path(), "app");
    assert_eq!(report["schema_version"], 2);
    let built_sha: String = Sha256::digest(&built)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(report["files"][0]["built"]["sha256"], built_sha.as_str());
    assert_eq!(report["outcome"], "built");
    assert_eq!(report["package"]["name"], "app");
    assert_eq!(report["application"]["environment"], "production");
    assert_eq!(report["exposure"]["status"], "excluded");
    let file = &report["files"][0];
    assert_eq!(file["file"], format!("app{EXE}"));
    assert_eq!(file["bytes"], copy.len());
    let sha: String = Sha256::digest(&copy)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(file["sha256"], sha.as_str());
    assert_eq!(file["blake3"], blake3::hash(&copy).to_hex().as_str());
    assert!(
        report["toolchain"]["rustc"]
            .as_str()
            .unwrap()
            .starts_with("rustc ")
    );
    assert!(!report["toolchain"]["host"].as_str().unwrap().is_empty());
}

#[test]
fn a_file_left_by_an_earlier_build_does_not_survive_the_next() {
    let dir = tempfile::tempdir().unwrap();
    single_app(dir.path());
    assert_eq!(florui_build(dir.path(), &[]).status.code(), Some(0));
    let stale = staged(dir.path(), "app").join("stale.txt");
    std::fs::write(&stale, "from an earlier build").unwrap();

    assert_eq!(florui_build(dir.path(), &[]).status.code(), Some(0));

    assert!(!stale.exists(), "the staging directory was not cleared");
}

#[test]
fn a_failing_build_exits_1_and_leaves_no_executable_staged() {
    let dir = tempfile::tempdir().unwrap();
    single_app(dir.path());
    assert_eq!(florui_build(dir.path(), &[]).status.code(), Some(0));
    write(dir.path(), "src/main.rs", "fn main() { not rust }\n");

    let output = florui_build(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(1));
    assert!(
        !staged(dir.path(), "app").join(format!("app{EXE}")).exists(),
        "an executable from the earlier build is still staged"
    );
}

#[test]
fn only_the_selected_package_is_built() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        "[workspace]\nmembers = [\"one\", \"two\"]\nresolver = \"2\"\n",
    );
    for name in ["one", "two"] {
        write(
            dir.path(),
            &format!("{name}/Cargo.toml"),
            &package(name, ""),
        );
        write(dir.path(), &format!("{name}/src/main.rs"), "fn main() {}\n");
    }

    let ambiguous = florui_build(dir.path(), &[]);
    let selected = florui_build(dir.path(), &["--package", "one"]);

    assert_eq!(ambiguous.status.code(), Some(1), "{}", stderr(&ambiguous));
    assert_eq!(selected.status.code(), Some(0), "{}", stderr(&selected));
    assert!(staged(dir.path(), "one").join(format!("one{EXE}")).exists());
    assert!(
        !dir.path()
            .join("target/release")
            .join(format!("two{EXE}"))
            .exists(),
        "the other member was built too"
    );
}

#[test]
fn linked_developer_tooling_is_a_warning_and_strict_rejects_it_and_still_writes_the_report() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "{}\n[dependencies]\nflorui-devtools = {{ path = \"florui-devtools\" }}\n",
            package("app", "")
        ),
    );
    write(dir.path(), "src/main.rs", "fn main() {}\n");
    stand_in(dir.path(), "florui-devtools", "");

    let lenient = florui_build(dir.path(), &[]);
    let lenient_report = report(dir.path(), "app");
    let strict = florui_build(dir.path(), &["--strict"]);
    let strict_report = report(dir.path(), "app");

    assert_eq!(lenient.status.code(), Some(0), "{}", stderr(&lenient));
    assert_eq!(lenient_report["outcome"], "built_with_warnings");
    assert_eq!(lenient_report["exposure"]["status"], "included");
    assert!(
        lenient_report["exposure"]["evidence"][0]
            .as_str()
            .unwrap()
            .contains("florui_devtools")
    );
    assert_eq!(strict.status.code(), Some(2), "{}", stderr(&strict));
    assert_eq!(strict_report["outcome"], "rejected");
    assert!(staged(dir.path(), "app").join(format!("app{EXE}")).exists());
}

#[test]
fn the_profiling_and_source_location_features_are_reported_with_the_crate_that_has_them() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "{}\n[dependencies]\nflorui = {{ path = \"florui\", features = [\"profiling\", \"source-locations\"] }}\n",
            package("app", "")
        ),
    );
    write(dir.path(), "src/main.rs", "fn main() {}\n");
    stand_in(
        dir.path(),
        "florui",
        "[features]\nprofiling = []\nsource-locations = []\n",
    );

    let output = florui_build(dir.path(), &[]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report = report(dir.path(), "app");
    let evidence: Vec<&str> = report["exposure"]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|line| line.as_str().unwrap())
        .collect();
    assert_eq!(
        evidence,
        [
            "florui was built with `profiling`",
            "florui was built with `source-locations`"
        ]
    );
}

#[test]
fn tooling_that_is_only_a_dev_dependency_is_not_in_the_artifact() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        "Cargo.toml",
        &format!(
            "{}\n[dev-dependencies]\nflorui-devtools = {{ path = \"florui-devtools\" }}\n",
            package("app", "")
        ),
    );
    write(dir.path(), "src/main.rs", "fn main() {}\n");
    stand_in(dir.path(), "florui-devtools", "");

    let output = florui_build(dir.path(), &["--strict"]);

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(report(dir.path(), "app")["exposure"]["status"], "excluded");
}
