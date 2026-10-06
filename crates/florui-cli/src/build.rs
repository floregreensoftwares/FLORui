//! `florui build --target native`: builds the selected package, stages its
//! executables in a fresh directory and writes a report of what was built from
//! and what the artifact exposes.
//!
//! What the artifact was linked from is read from the messages `cargo` prints for
//! every unit it compiled (the crate, its kinds and the features it was built
//! with), not from a workspace-wide `cargo metadata`, which unifies features
//! across members. That is evidence about the dependency graph of this build,
//! not a scan of the binary's contents, and the report says so.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

use florui_devtools::diagnostics::{dim_text, failure, success, warning};

pub struct Options {
    pub package: Option<String>,
    pub environment: Option<String>,
    /// Reject an artifact that includes developer tooling.
    pub strict: bool,
}

/// The crate whose presence means the inspector is in the artifact.
const DEVTOOLS_CRATE: &str = "florui_devtools";
/// The Florui crates that carry the profiling and source-location features.
const FEATURE_CRATES: &[&str] = &[
    "florui",
    "florui_reactive",
    "florui_platform",
    "florui_profile",
];
const DEVELOPER_FEATURES: &[&str] = &["profiling", "source-locations"];

// ------------------------------------------------------------ cargo's output

/// One unit `cargo` compiled.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Unit {
    pub crate_name: String,
    pub kinds: Vec<String>,
    pub features: Vec<String>,
    pub manifest_path: String,
    pub executable: Option<String>,
}

impl Unit {
    /// Whether the unit's code ends up in an executable: not a build script, a
    /// procedural macro, a test, a bench or an example.
    fn is_linked(&self) -> bool {
        self.kinds.iter().any(|kind| {
            matches!(
                kind.as_str(),
                "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "bin"
            )
        })
    }
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct BuildFacts {
    pub units: Vec<Unit>,
    pub succeeded: Option<bool>,
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// Reads `cargo build --message-format=json` output. Lines that are not JSON, or
/// are a message this does not use, are skipped.
pub(crate) fn parse_cargo_messages(output: &str) -> BuildFacts {
    let mut facts = BuildFacts::default();
    for line in output.lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match message["reason"].as_str() {
            Some("compiler-artifact") => facts.units.push(Unit {
                crate_name: message["target"]["name"].as_str().unwrap_or("").to_string(),
                kinds: strings(&message["target"]["kind"]),
                features: strings(&message["features"]),
                manifest_path: message["manifest_path"].as_str().unwrap_or("").to_string(),
                executable: message["executable"].as_str().map(str::to_string),
            }),
            Some("build-finished") => facts.succeeded = message["success"].as_bool(),
            _ => {}
        }
    }
    facts
}

// -------------------------------------------------------------------- exposure

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Exposure {
    /// `excluded`, `included` or `unknown`.
    pub status: &'static str,
    pub evidence: Vec<String>,
}

/// What developer tooling the build linked in, from the units it compiled.
pub(crate) fn exposure(units: &[Unit]) -> Exposure {
    let linked: Vec<&Unit> = units.iter().filter(|unit| unit.is_linked()).collect();
    if linked.is_empty() {
        return Exposure {
            status: "unknown",
            evidence: vec!["the build reported no unit that is linked into the artifact".into()],
        };
    }
    let mut evidence = Vec::new();
    for unit in &linked {
        if unit.crate_name == DEVTOOLS_CRATE {
            evidence.push(format!("{DEVTOOLS_CRATE} is linked (the inspector)"));
        }
        if FEATURE_CRATES.contains(&unit.crate_name.as_str()) {
            for feature in DEVELOPER_FEATURES {
                if unit.features.iter().any(|f| f == feature) {
                    evidence.push(format!("{} was built with `{feature}`", unit.crate_name));
                }
            }
        }
    }
    evidence.sort();
    evidence.dedup();
    Exposure {
        status: if evidence.is_empty() {
            "excluded"
        } else {
            "included"
        },
        evidence,
    }
}

// --------------------------------------------------------------------- hashes

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct FileHashes {
    pub bytes: u64,
    pub sha256: String,
    pub blake3: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn hash_reader(mut reader: impl Read) -> std::io::Result<FileHashes> {
    let mut sha = Sha256::new();
    let mut fast = blake3::Hasher::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut bytes = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha.update(&buffer[..read]);
        fast.update(&buffer[..read]);
        bytes += read as u64;
    }
    Ok(FileHashes {
        bytes,
        sha256: hex(&sha.finalize()),
        blake3: fast.finalize().to_hex().to_string(),
    })
}

fn hash_file(path: &Path) -> std::io::Result<FileHashes> {
    hash_reader(fs::File::open(path)?)
}

// -------------------------------------------------------------------- staging

/// The directory the executables of `package` are staged in: inside the
/// project's target directory, so it cannot point anywhere else.
fn staging_dir(target_dir: &Path, package: &str) -> Result<PathBuf, String> {
    let mut components = Path::new(package).components();
    let is_one_plain_name = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !is_one_plain_name {
        return Err(format!(
            "\"{package}\" is not a name that can be a directory under {}",
            target_dir.display()
        ));
    }
    Ok(target_dir.join("florui-build").join(package).join("native"))
}

/// Removes whatever an earlier build left and creates the directory again.
fn fresh_directory(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::create_dir_all(dir)
}

#[derive(Debug, Serialize)]
struct StagedFile {
    file: String,
    #[serde(flatten)]
    hashes: FileHashes,
}

fn stage(executables: &[PathBuf], dir: &Path) -> std::io::Result<Vec<StagedFile>> {
    let mut staged = Vec::new();
    for source in executables {
        let name = source
            .file_name()
            .ok_or_else(|| std::io::Error::other("an executable has no file name"))?;
        let destination = dir.join(name);
        fs::copy(source, &destination)?;
        staged.push(StagedFile {
            file: name.to_string_lossy().into_owned(),
            hashes: hash_file(&destination)?,
        });
    }
    Ok(staged)
}

// --------------------------------------------------------------------- report

#[derive(Serialize)]
struct Report {
    schema_version: u32,
    /// `built`, `built_with_warnings` (developer tooling included, not strict)
    /// or `rejected` (developer tooling included under `--strict`).
    outcome: &'static str,
    florui_target: &'static str,
    package: PackageInfo,
    application: ApplicationInfo,
    toolchain: Toolchain,
    profile: &'static str,
    /// The features the package itself was built with.
    package_features: Vec<String>,
    source: SourceInfo,
    lockfile: Option<FileHashes>,
    florui_cli_version: &'static str,
    built_at_unix_seconds: u64,
    exposure: Exposure,
    files: Vec<StagedFile>,
    notes: Vec<&'static str>,
}

#[derive(Serialize)]
struct PackageInfo {
    name: String,
    version: String,
}

#[derive(Serialize)]
struct ApplicationInfo {
    name: String,
    identifier: Option<String>,
    environment: String,
}

#[derive(Serialize)]
struct Toolchain {
    rustc: String,
    cargo: String,
    host: String,
}

#[derive(Serialize)]
struct SourceInfo {
    git_commit: Option<String>,
    git_dirty: Option<bool>,
}

const NOTES: &[&str] = &[
    "exposure is evidence from the dependency graph of this build, not a scan of the executable's contents; \
     an executable stays inspectable whatever was left out",
    "this is not a reproducible-build claim: the executable embeds paths and the report a timestamp",
];

fn run_tool(program: &str, args: &[&str], dir: Option<&Path>) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn host_triple(rustc_verbose: &str) -> String {
    rustc_verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("unknown")
        .to_string()
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{}", failure(&message.to_string()));
    ExitCode::FAILURE
}

fn same_file(a: &str, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => Path::new(a) == b,
    }
}

pub fn run(options: Options) -> ExitCode {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            return fail(format!(
                "could not determine the current directory: {error}"
            ));
        }
    };
    let facts = match florui_config::resolve_cargo_project(&cwd, options.package.as_deref()) {
        Ok(facts) => facts,
        Err(error) => return fail(error),
    };
    let selection = florui_config::EnvironmentSelection {
        name: options.environment.as_deref().unwrap_or("production"),
        explicit: options.environment.is_some(),
    };
    let resolution = match florui_config::resolve(&facts, None, Some(selection)) {
        Ok(resolution) => resolution,
        Err(error) => return fail(error),
    };

    let staging = match staging_dir(&facts.target_dir, &facts.package_name) {
        Ok(dir) => dir,
        Err(error) => return fail(error),
    };
    // Cleared before building, so a failed build cannot leave an earlier
    // executable that looks like this one's.
    if let Err(error) = fresh_directory(&staging) {
        return fail(format!("could not prepare {}: {error}", staging.display()));
    }

    println!(
        "{}",
        dim_text(&format!(
            "building {} (cargo build --release -p {})...",
            facts.package_name, facts.package_name
        ))
    );
    let output = match Command::new("cargo")
        .args([
            "build",
            "--release",
            "-p",
            &facts.package_name,
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(output) => output,
        Err(error) => return fail(format!("could not run cargo build: {error}")),
    };
    if !output.status.success() {
        return fail(format!("cargo build exited with {}", output.status));
    }
    let built = parse_cargo_messages(&String::from_utf8_lossy(&output.stdout));

    let executables: Vec<PathBuf> = built
        .units
        .iter()
        .filter(|unit| same_file(&unit.manifest_path, &facts.manifest_path))
        .filter_map(|unit| unit.executable.as_ref().map(PathBuf::from))
        .collect();
    if executables.is_empty() {
        return fail(format!(
            "{} has no binary target, so there is nothing to stage",
            facts.package_name
        ));
    }
    let files = match stage(&executables, &staging) {
        Ok(files) => files,
        Err(error) => return fail(format!("could not stage the executables: {error}")),
    };

    let exposure = exposure(&built.units);
    let mut package_features: Vec<String> = built
        .units
        .iter()
        .filter(|unit| same_file(&unit.manifest_path, &facts.manifest_path))
        .flat_map(|unit| unit.features.clone())
        .collect();
    package_features.sort();
    package_features.dedup();

    let rustc = run_tool("rustc", &["-vV"], None).unwrap_or_default();
    let included = exposure.status == "included";
    let outcome = match (included, options.strict) {
        (false, _) => "built",
        (true, false) => "built_with_warnings",
        (true, true) => "rejected",
    };
    let git_commit = run_tool("git", &["rev-parse", "HEAD"], Some(&facts.package_root));
    let report = Report {
        schema_version: 1,
        outcome,
        florui_target: "native",
        package: PackageInfo {
            name: facts.package_name.clone(),
            version: facts.package_version.clone(),
        },
        application: ApplicationInfo {
            name: resolution.config.app.name.clone(),
            identifier: resolution.config.app.identifier.clone(),
            environment: selection.name.to_string(),
        },
        toolchain: Toolchain {
            host: host_triple(&rustc),
            rustc: rustc.lines().next().unwrap_or("").to_string(),
            cargo: run_tool("cargo", &["-V"], None).unwrap_or_default(),
        },
        profile: "release",
        package_features,
        source: SourceInfo {
            git_dirty: git_commit.as_ref().and_then(|_| {
                run_tool("git", &["status", "--porcelain"], Some(&facts.package_root))
                    .map(|status| !status.is_empty())
            }),
            git_commit,
        },
        lockfile: hash_file(&facts.workspace_root.join("Cargo.lock")).ok(),
        florui_cli_version: env!("CARGO_PKG_VERSION"),
        built_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
        exposure,
        files,
        notes: NOTES.to_vec(),
    };
    let report_path = staging.join("report.json");
    let json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => return fail(format!("could not encode the build report: {error}")),
    };
    if let Err(error) = fs::write(&report_path, json) {
        return fail(format!(
            "could not write {}: {error}",
            report_path.display()
        ));
    }

    for file in &report.files {
        println!(
            "  {} ({} bytes, sha256 {})",
            file.file, file.hashes.bytes, file.hashes.sha256
        );
    }
    println!(
        "{}",
        dim_text(&format!("report: {}", report_path.display()))
    );
    match outcome {
        "built" => {
            println!(
                "{}",
                success(&format!("built {} (native, release)", facts.package_name))
            );
            ExitCode::SUCCESS
        }
        "built_with_warnings" => {
            for line in &report.exposure.evidence {
                println!(
                    "{}",
                    warning(&format!("developer tooling included: {line}"))
                );
            }
            println!(
                "{}",
                success(&format!(
                    "built {} (native, release) with developer tooling included",
                    facts.package_name
                ))
            );
            ExitCode::SUCCESS
        }
        _ => {
            for line in &report.exposure.evidence {
                eprintln!(
                    "{}",
                    failure(&format!("developer tooling included: {line}"))
                );
            }
            eprintln!(
                "{}",
                failure("rejected by --strict: the artifact includes developer tooling")
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGES: &str = r#"
{"reason":"compiler-artifact","package_id":"a","manifest_path":"/w/florui-platform/Cargo.toml","target":{"kind":["lib"],"name":"florui_platform"},"features":["default","desktop","profiling"],"executable":null}
{"reason":"compiler-artifact","package_id":"b","manifest_path":"/w/florui-macros/Cargo.toml","target":{"kind":["proc-macro"],"name":"florui_macros"},"features":["profiling"],"executable":null}
{"reason":"compiler-artifact","package_id":"c","manifest_path":"/w/app/build.rs","target":{"kind":["custom-build"],"name":"build_script_build"},"features":[],"executable":null}
{"reason":"compiler-artifact","package_id":"d","manifest_path":"/w/app/Cargo.toml","target":{"kind":["bin"],"name":"app"},"features":["extra"],"executable":"/w/target/release/app.exe"}
this line is not json
{"reason":"compiler-message","message":{}}
{"reason":"build-finished","success":true}
"#;

    fn unit(name: &str, kind: &str, features: &[&str]) -> Unit {
        Unit {
            crate_name: name.into(),
            kinds: vec![kind.into()],
            features: features.iter().map(|f| (*f).to_string()).collect(),
            manifest_path: String::new(),
            executable: None,
        }
    }

    #[test]
    fn cargo_messages_give_units_and_executables_and_skip_what_is_not_json() {
        let facts = parse_cargo_messages(MESSAGES);

        assert_eq!(facts.units.len(), 4);
        assert_eq!(facts.succeeded, Some(true));
        let app = &facts.units[3];
        assert_eq!(app.executable.as_deref(), Some("/w/target/release/app.exe"));
        assert_eq!(app.features, ["extra"]);
        assert_eq!(parse_cargo_messages("").units, []);
    }

    #[test]
    fn a_unit_counts_only_when_its_code_is_linked_into_the_artifact() {
        let facts = parse_cargo_messages(MESSAGES);

        let linked: Vec<&str> = facts
            .units
            .iter()
            .filter(|unit| unit.is_linked())
            .map(|unit| unit.crate_name.as_str())
            .collect();

        assert_eq!(
            linked,
            ["florui_platform", "app"],
            "a proc macro and a build script are not linked"
        );
    }

    #[test]
    fn the_inspector_crate_and_each_developer_feature_are_found_and_nothing_else_is() {
        assert_eq!(exposure(&[unit("app", "bin", &[])]).status, "excluded");

        let with_devtools =
            exposure(&[unit("app", "bin", &[]), unit("florui_devtools", "lib", &[])]);
        assert_eq!(with_devtools.status, "included");
        assert!(with_devtools.evidence[0].contains("florui_devtools"));

        let profiling = exposure(&[unit("florui_platform", "lib", &["desktop", "profiling"])]);
        assert_eq!(
            profiling.evidence,
            ["florui_platform was built with `profiling`"]
        );

        let locations = exposure(&[unit("florui", "lib", &["source-locations"])]);
        assert_eq!(
            locations.evidence,
            ["florui was built with `source-locations`"]
        );

        let not_florui = exposure(&[unit("serde", "lib", &["profiling"])]);
        assert_eq!(
            not_florui.status, "excluded",
            "a feature of another crate is not ours"
        );
    }

    #[test]
    fn tooling_that_is_not_linked_is_not_counted() {
        let host_only = exposure(&[
            unit("app", "bin", &[]),
            unit("florui_devtools", "custom-build", &[]),
            unit("florui_devtools", "proc-macro", &[]),
            unit("florui_devtools", "test", &[]),
        ]);

        assert_eq!(host_only.status, "excluded");
    }

    #[test]
    fn a_build_that_reports_nothing_linked_is_unknown_not_a_pass() {
        assert_eq!(exposure(&[]).status, "unknown");
        assert_eq!(
            exposure(&[unit("florui_devtools", "test", &[])]).status,
            "unknown"
        );
    }

    #[test]
    fn hashes_match_the_published_vectors_and_do_not_depend_on_how_the_input_is_read() {
        let abc = hash_reader(&b"abc"[..]).unwrap();
        assert_eq!(
            abc.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(abc.bytes, 3);
        assert_eq!(
            hash_reader(&b""[..]).unwrap().blake3,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );

        let big = vec![0x5a_u8; 300_000];
        let whole = hash_reader(&big[..]).unwrap();
        let one_byte_at_a_time = hash_reader(OneByte(&big[..])).unwrap();
        assert_eq!(whole, one_byte_at_a_time);
    }

    struct OneByte<'a>(&'a [u8]);

    impl Read for OneByte<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some((first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            buffer[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn the_staging_directory_stays_inside_the_target_directory() {
        let target = Path::new("/project/target");
        assert_eq!(
            staging_dir(target, "app").unwrap(),
            Path::new("/project/target/florui-build/app/native")
        );
        assert!(staging_dir(target, "../../elsewhere").is_err());
    }

    #[test]
    fn the_host_is_read_from_rustc_verbose_output() {
        let verbose =
            "rustc 1.94.0 (x)\nbinary: rustc\nhost: x86_64-pc-windows-msvc\nrelease: 1.94.0\n";
        assert_eq!(host_triple(verbose), "x86_64-pc-windows-msvc");
        assert_eq!(host_triple(""), "unknown");
    }
}
