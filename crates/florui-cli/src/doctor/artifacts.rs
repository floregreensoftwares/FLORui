//! `florui doctor --artifacts <dir>`: what an existing native build output
//! actually is, against what its `report.json` says and against the project
//! as it is now. It never builds, writes, runs the executable or uses the
//! network, and anything it cannot establish is reported as unknown, not as a
//! pass.
//!
//! The report is read tolerantly (unknown fields are ignored), so a doctor
//! older than the report still reads it.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use super::{Check, Status, display_redacted};
use crate::build::hash_file;
use crate::resources;

const CATEGORY: &str = "artifacts";
/// A report is a small file; anything bigger is not one.
const MAX_REPORT_BYTES: u64 = 16 * 1024 * 1024;

fn check(
    id: &'static str,
    required: bool,
    status: Status,
    evidence: String,
    remediation: Option<String>,
) -> Check {
    Check {
        id,
        category: CATEGORY,
        status,
        required,
        observed: None,
        expected: None,
        evidence,
        reason: None,
        remediation,
    }
}

/// The checks of one staged directory. `cwd` and `package` select the project
/// the output is compared with, the way `doctor` selects it elsewhere.
pub(super) fn checks(
    dir: &Path,
    cwd: &Path,
    package: Option<&str>,
    environment: Option<&str>,
) -> Vec<Check> {
    let shown = display_redacted(dir);
    let build_hint = Some(
        "run `florui build --target native` to stage a build, or point --artifacts at the \
         directory it printed"
            .to_string(),
    );
    let report_path = dir.join("report.json");
    let report = match read_report(&report_path) {
        Ok(report) => report,
        Err(reason) => {
            let no_report = format!("no readable report in {shown}");
            return vec![
                check(
                    "artifacts.report",
                    true,
                    Status::Fail,
                    format!("{shown}: {reason}"),
                    build_hint,
                ),
                check(
                    "artifacts.files",
                    true,
                    Status::Unknown,
                    no_report.clone(),
                    None,
                ),
                check(
                    "artifacts.exposure",
                    true,
                    Status::Unknown,
                    no_report.clone(),
                    None,
                ),
                check(
                    "artifacts.resources",
                    true,
                    Status::Unknown,
                    no_report.clone(),
                    None,
                ),
                check(
                    "artifacts.identity",
                    false,
                    Status::Unknown,
                    no_report.clone(),
                    None,
                ),
                check(
                    "artifacts.provenance",
                    false,
                    Status::Unknown,
                    no_report,
                    None,
                ),
            ];
        }
    };

    let project = florui_config::resolve_cargo_project(cwd, package).ok();
    vec![
        report_check(&report, &shown),
        files_check(&report, dir, &shown),
        exposure_check(&report),
        resources_check(&report, dir),
        identity_check(&report, project.as_ref(), environment),
        provenance_check(&report, project.as_ref()),
    ]
}

pub(super) fn read_report(path: &Path) -> Result<Value, String> {
    let metadata = fs::metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "there is no report.json".to_string(),
        _ => format!("could not read report.json: {error}"),
    })?;
    if metadata.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "report.json is {} bytes, too large to be a build report",
            metadata.len()
        ));
    }
    let text =
        fs::read_to_string(path).map_err(|error| format!("could not read report.json: {error}"))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| format!("report.json is not valid JSON: {error}"))?;
    if !value.is_object() {
        return Err("report.json is not a JSON object".to_string());
    }
    Ok(value)
}

fn text<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut at = value;
    for key in path {
        at = at.get(key)?;
    }
    at.as_str()
}

fn report_check(report: &Value, shown: &str) -> Check {
    let schema = report.get("schema_version").and_then(Value::as_u64);
    let outcome = text(report, &["outcome"]);
    let rebuild = Some("run `florui build --target native` again".to_string());
    match (schema, outcome) {
        (None, _) => check(
            "artifacts.report",
            true,
            Status::Fail,
            format!(
                "{shown}: report.json has no schema_version, so it is not a Florui build report"
            ),
            rebuild,
        ),
        (Some(version), _) if version > 2 => check(
            "artifacts.report",
            true,
            Status::Unknown,
            format!(
                "report.json is schema {version}, newer than this doctor understands (2): \
                 nothing in it can be verified"
            ),
            Some("use a florui at least as new as the one that built this".to_string()),
        ),
        (_, Some("rejected")) => check(
            "artifacts.report",
            true,
            Status::Fail,
            "the build was rejected under --strict; its output is not a release".to_string(),
            rebuild,
        ),
        (Some(1), outcome) => check(
            "artifacts.report",
            true,
            Status::Warning,
            format!(
                "report.json is schema 1 (outcome {}): it has no record of the resources",
                outcome.unwrap_or("missing")
            ),
            rebuild,
        ),
        (Some(version), None) => check(
            "artifacts.report",
            true,
            Status::Fail,
            format!("report.json (schema {version}) has no outcome"),
            rebuild,
        ),
        (Some(version), Some(outcome)) => check(
            "artifacts.report",
            true,
            Status::Pass,
            format!("report.json is schema {version}, outcome {outcome}"),
            None,
        ),
    }
}

/// `name` as one plain file name inside the directory, or why not.
fn plain_name(name: &str) -> Result<&str, String> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(name),
        _ => Err(format!(
            "\"{name}\" is not a plain file name inside the directory"
        )),
    }
}

/// Everything wrong with the files of `dir` against the report's list.
fn file_problems(report: &Value, dir: &Path) -> Result<Vec<String>, String> {
    let listed = report
        .get("files")
        .and_then(Value::as_array)
        .ok_or("the report lists no files")?;
    let root = fs::canonicalize(dir)
        .map_err(|error| format!("could not resolve the directory: {error}"))?;
    let mut problems = Vec::new();
    let mut known: BTreeSet<String> = BTreeSet::from(["report.json".to_string()]);
    for entry in listed {
        let Some(name) = entry.get("file").and_then(Value::as_str) else {
            problems.push("a file entry in the report has no name".to_string());
            continue;
        };
        let name = match plain_name(name) {
            Ok(name) => name,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        known.insert(name.to_string());
        let path = dir.join(name);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            problems.push(format!("{name} is listed in the report but is missing"));
            continue;
        };
        if metadata.file_type().is_symlink() {
            problems.push(format!("{name} is a symbolic link, not a file staged here"));
            continue;
        }
        match fs::canonicalize(&path) {
            Ok(real) if real.starts_with(&root) => {}
            _ => {
                problems.push(format!("{name} resolves outside the directory"));
                continue;
            }
        }
        let actual = match hash_file(&path) {
            Ok(actual) => actual,
            Err(error) => {
                problems.push(format!("could not read {name}: {error}"));
                continue;
            }
        };
        let expected_bytes = entry.get("bytes").and_then(Value::as_u64);
        let expected_sha = entry.get("sha256").and_then(Value::as_str);
        let expected_blake = entry.get("blake3").and_then(Value::as_str);
        if expected_bytes.is_none() || expected_sha.is_none() || expected_blake.is_none() {
            problems.push(format!(
                "the report has no complete size and hashes for {name}"
            ));
        } else if expected_bytes != Some(actual.bytes) {
            problems.push(format!(
                "{name} is {} bytes, the report says {}",
                actual.bytes,
                expected_bytes.unwrap_or(0)
            ));
        } else if expected_sha != Some(actual.sha256.as_str())
            || expected_blake != Some(actual.blake3.as_str())
        {
            problems.push(format!("{name} does not match the hash in the report"));
        }
    }
    let entries =
        fs::read_dir(dir).map_err(|error| format!("could not list the directory: {error}"))?;
    let mut extra: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !known.contains(name))
        .collect();
    extra.sort();
    for name in extra {
        problems.push(format!("{name} is in the directory but not in the report"));
    }
    Ok(problems)
}

fn files_check(report: &Value, dir: &Path, shown: &str) -> Check {
    let rebuild =
        Some("run `florui build --target native` again to stage a clean output".to_string());
    match file_problems(report, dir) {
        Err(reason) => check(
            "artifacts.files",
            true,
            Status::Unknown,
            format!("{shown}: {reason}"),
            None,
        ),
        Ok(problems) if problems.is_empty() => {
            let count = report
                .get("files")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            check(
                "artifacts.files",
                true,
                Status::Pass,
                format!(
                    "{count} file(s) match the report's sizes and hashes and nothing else is there"
                ),
                None,
            )
        }
        Ok(problems) => check(
            "artifacts.files",
            true,
            Status::Fail,
            problems.join("; "),
            rebuild,
        ),
    }
}

fn exposure_check(report: &Value) -> Check {
    let note = "dependency evidence from the build, not a scan of the executable's contents";
    let evidence: Vec<&str> = report
        .get("exposure")
        .and_then(|exposure| exposure.get("evidence"))
        .and_then(Value::as_array)
        .map(|lines| lines.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match text(report, &["exposure", "status"]) {
        Some("excluded") => check(
            "artifacts.exposure",
            true,
            Status::Pass,
            format!("developer tooling was not part of this build ({note})"),
            None,
        ),
        Some("included") => check(
            "artifacts.exposure",
            true,
            Status::Warning,
            format!(
                "developer tooling is included: {} ({note})",
                evidence.join("; ")
            ),
            Some(
                "build without the inspector crate and the profiling and source-locations features"
                    .to_string(),
            ),
        ),
        Some(other) => check(
            "artifacts.exposure",
            true,
            Status::Unknown,
            format!("the report's exposure is \"{other}\" ({note})"),
            None,
        ),
        None => check(
            "artifacts.exposure",
            true,
            Status::Unknown,
            "the report records no exposure result".to_string(),
            None,
        ),
    }
}

/// What the report says was written into the executable `file`.
fn expected_strings(report: &Value, file: &str) -> Option<Vec<(&'static str, Option<String>)>> {
    let entries = report.get("resources")?.get("version_info")?.as_array()?;
    let entry = entries
        .iter()
        .find(|entry| text(entry, &["original_filename"]) == Some(file))?;
    let get = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string);
    Some(vec![
        ("ProductName", get("product_name")),
        ("CompanyName", get("company_name")),
        ("FileDescription", get("file_description")),
        ("FileVersion", get("file_version")),
        ("ProductVersion", get("product_version")),
        ("OriginalFilename", get("original_filename")),
        ("InternalName", get("internal_name")),
        ("LegalCopyright", get("legal_copyright")),
    ])
}

/// The (language, name, description) of each string table the build wrote
/// into `file`: the default locale's first, then every other embedded one.
fn expected_locales<'a>(report: &'a Value, file: &str) -> Vec<(&'a str, &'a str, &'a str)> {
    let Some(entry) = report
        .get("resources")
        .and_then(|resources| resources.get("version_info"))
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| text(entry, &["original_filename"]) == Some(file))
        })
    else {
        return Vec::new();
    };
    entry
        .get("localized")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|locale| locale.get("embedded").and_then(Value::as_bool) == Some(true))
        .filter_map(|locale| {
            Some((
                locale.get("language_id")?.as_str()?,
                locale.get("product_name")?.as_str()?,
                locale.get("file_description")?.as_str()?,
            ))
        })
        .collect()
}

fn resources_check(report: &Value, dir: &Path) -> Check {
    let status = text(report, &["resources", "status"]);
    match status {
        Some("applied") => {}
        Some(other) => {
            return check(
                "artifacts.resources",
                true,
                Status::Unknown,
                format!("the build wrote no executable resources (resources: {other})"),
                None,
            );
        }
        None => {
            return check(
                "artifacts.resources",
                true,
                Status::Unknown,
                "the report has no record of the executable resources (an older report, or a \
                 build that predates them)"
                    .to_string(),
                Some("run `florui build --target native` again".to_string()),
            );
        }
    }
    let executables: Vec<&str> = report
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("built").is_some_and(|built| !built.is_null()))
        .filter_map(|entry| entry.get("file").and_then(Value::as_str))
        .collect();
    if executables.is_empty() {
        return check(
            "artifacts.resources",
            true,
            Status::Unknown,
            "the report lists no executable".to_string(),
            None,
        );
    }
    let expected_icon_entries = report
        .get("resources")
        .and_then(|resources| resources.get("icon"))
        .and_then(|icon| icon.get("sizes"))
        .and_then(Value::as_array)
        .map(Vec::len);

    let mut problems = Vec::new();
    for file in &executables {
        let Ok(name) = plain_name(file) else { continue };
        let found = match resources::read_back(&dir.join(name)) {
            Ok(found) => found,
            Err(reason) => {
                return check("artifacts.resources", true, Status::Unknown, reason, None);
            }
        };
        let Some(expected) = expected_strings(report, name) else {
            problems.push(format!("the report has no version information for {name}"));
            continue;
        };
        for (key, wanted) in expected {
            let actual = found.version_strings.get(key).map(String::as_str);
            if actual != wanted.as_deref() {
                problems.push(format!(
                    "{name}: {key} is {} but the build wrote {}",
                    actual.map_or("absent".to_string(), |value| format!("\"{value}\"")),
                    wanted.map_or("nothing".to_string(), |value| format!("\"{value}\"")),
                ));
            }
        }
        // Every locale the build says it embedded is read back from its own
        // string table, in the order the build wrote them.
        let written: Vec<(&str, &str, &str)> = expected_locales(report, name);
        for (language, product, description) in &written {
            match found.localized.iter().find(|(id, ..)| id == language) {
                None => problems.push(format!(
                    "{name}: the string table for language {language} is missing"
                )),
                Some((_, found_product, found_description)) => {
                    for (what, wanted, actual) in [
                        ("ProductName", product, found_product),
                        ("FileDescription", description, found_description),
                    ] {
                        if wanted != actual {
                            problems.push(format!(
                                "{name}: {what} for language {language} is \"{actual}\" but the build wrote \"{wanted}\""
                            ));
                        }
                    }
                }
            }
        }
        if !written.is_empty() && found.localized.len() != written.len() {
            problems.push(format!(
                "{name}: the executable has {} string table(s) but the build wrote {}",
                found.localized.len(),
                written.len()
            ));
        }
        if found.icon_entries != expected_icon_entries {
            problems.push(format!(
                "{name}: the icon has {} image(s) but the build wrote {}",
                found
                    .icon_entries
                    .map_or("no".to_string(), |count| count.to_string()),
                expected_icon_entries.map_or("none".to_string(), |count| count.to_string()),
            ));
        }
    }
    if problems.is_empty() {
        check(
            "artifacts.resources",
            true,
            Status::Pass,
            format!(
                "the version information and icon read back from {} executable(s) are what the \
                 build wrote",
                executables.len()
            ),
            None,
        )
    } else {
        check(
            "artifacts.resources",
            true,
            Status::Fail,
            problems.join("; "),
            Some("the executable was changed after the build: run `florui build --target native` again".to_string()),
        )
    }
}

fn identity_check(
    report: &Value,
    project: Option<&florui_config::CargoProjectFacts>,
    environment: Option<&str>,
) -> Check {
    let Some(facts) = project else {
        return check(
            "artifacts.identity",
            false,
            Status::NotApplicable,
            "not invoked inside a resolvable Cargo project, so there is no configuration to \
             compare the build with"
                .to_string(),
            None,
        );
    };
    let built_environment = environment
        .or_else(|| text(report, &["application", "environment"]))
        .unwrap_or("production");
    let selection = florui_config::EnvironmentSelection {
        name: built_environment,
        explicit: false,
    };
    let resolution = match florui_config::resolve(facts, None, Some(selection)) {
        Ok(resolution) => resolution,
        Err(error) => {
            return check(
                "artifacts.identity",
                false,
                Status::Unknown,
                format!("the current configuration could not be resolved: {error}"),
                None,
            );
        }
    };
    let config = &resolution.config.app;
    let mut differences = Vec::new();
    let mut compare = |what: &str, built: Option<&str>, now: Option<&str>| {
        if built != now {
            differences.push(format!(
                "{what}: built as {} but the configuration now says {}",
                built.map_or("nothing".to_string(), |value| format!("\"{value}\"")),
                now.map_or("nothing".to_string(), |value| format!("\"{value}\"")),
            ));
        }
    };
    compare(
        "application name",
        text(report, &["application", "name"]),
        Some(&config.name),
    );
    compare(
        "identifier",
        text(report, &["application", "identifier"]),
        config.identifier.as_deref(),
    );
    let built_version = report
        .get("resources")
        .and_then(|resources| resources.get("version_info"))
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(|entry| text(entry, &["file_version"]))
        .or_else(|| text(report, &["package", "version"]));
    compare("version", built_version, Some(&config.version));
    if differences.is_empty() {
        check(
            "artifacts.identity",
            false,
            Status::Pass,
            format!(
                "name, identifier and version match the current configuration (environment {built_environment})"
            ),
            None,
        )
    } else {
        check(
            "artifacts.identity",
            false,
            Status::Warning,
            format!("the output predates a configuration change: {}", differences.join("; ")),
            Some("run `florui build --target native` again so the output carries the current identity".to_string()),
        )
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn provenance_check(report: &Value, project: Option<&florui_config::CargoProjectFacts>) -> Check {
    let Some(facts) = project else {
        return check(
            "artifacts.provenance",
            false,
            Status::NotApplicable,
            "not invoked inside a resolvable Cargo project, so there is no source to compare \
             the build with"
                .to_string(),
            None,
        );
    };
    let mut differences = Vec::new();
    let mut missing = Vec::new();

    match (
        text(report, &["source", "git_commit"]),
        git(&facts.package_root, &["rev-parse", "HEAD"]),
    ) {
        (Some(built), Some(now)) if built != now => differences.push(format!(
            "built from commit {} but the source is now at {}",
            &built[..built.len().min(12)],
            &now[..now.len().min(12)]
        )),
        (Some(_), Some(_)) => {}
        (None, _) => missing.push("the build recorded no git commit"),
        (_, None) => missing.push("the current commit could not be read"),
    }
    if report
        .get("source")
        .and_then(|source| source.get("git_dirty"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        differences.push("built from a tree with uncommitted changes".to_string());
    }

    let lockfile = facts.workspace_root.join("Cargo.lock");
    match (text(report, &["lockfile", "sha256"]), hash_file(&lockfile)) {
        (Some(built), Ok(now)) if built != now.sha256 => {
            differences.push("Cargo.lock changed since the build".to_string());
        }
        (Some(_), Ok(_)) => {}
        (None, _) => missing.push("the build recorded no lockfile hash"),
        (_, Err(_)) => missing.push("the current Cargo.lock could not be read"),
    }

    if !differences.is_empty() {
        check(
            "artifacts.provenance",
            false,
            Status::Warning,
            differences.join("; "),
            Some("run `florui build --target native` again from the current source".to_string()),
        )
    } else if !missing.is_empty() {
        check(
            "artifacts.provenance",
            false,
            Status::Unknown,
            format!("not enough evidence: {}", missing.join("; ")),
            None,
        )
    } else {
        check(
            "artifacts.provenance",
            false,
            Status::Pass,
            "the build's commit, clean tree and lockfile match the project now".to_string(),
            None,
        )
    }
}

/// The directory a relative `--artifacts` refers to.
pub(super) fn resolve_dir(cwd: &Path, given: &Path) -> PathBuf {
    if given.is_absolute() {
        given.to_path_buf()
    } else {
        cwd.join(given)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn staged(dir: &Path, report: &Value, files: &[(&str, &[u8])]) {
        fs::write(dir.join("report.json"), report.to_string()).unwrap();
        for (name, bytes) in files {
            fs::write(dir.join(name), bytes).unwrap();
        }
    }

    fn entry(name: &str, bytes: &[u8]) -> Value {
        let hashes = crate::build::hash_reader(bytes).unwrap();
        json!({"file": name, "bytes": hashes.bytes, "sha256": hashes.sha256, "blake3": hashes.blake3})
    }

    #[test]
    fn a_missing_a_truncated_and_a_foreign_report_are_failures_not_crashes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            read_report(&dir.path().join("report.json"))
                .unwrap_err()
                .contains("no report.json")
        );
        fs::write(dir.path().join("report.json"), "{\"schema_ver").unwrap();
        assert!(
            read_report(&dir.path().join("report.json"))
                .unwrap_err()
                .contains("not valid JSON")
        );
        fs::write(dir.path().join("report.json"), "[1]").unwrap();
        assert!(
            read_report(&dir.path().join("report.json"))
                .unwrap_err()
                .contains("not a JSON object")
        );
        let report = json!({"outcome": "built"});
        assert_eq!(report_check(&report, "d").status, Status::Fail);
    }

    #[test]
    fn the_report_check_tells_schemas_and_outcomes_apart() {
        let status = |report: Value| report_check(&report, "d").status;
        assert_eq!(
            status(json!({"schema_version": 2, "outcome": "built"})),
            Status::Pass
        );
        assert_eq!(
            status(json!({"schema_version": 2, "outcome": "built_with_warnings"})),
            Status::Pass
        );
        assert_eq!(
            status(json!({"schema_version": 2, "outcome": "rejected"})),
            Status::Fail
        );
        assert_eq!(
            status(json!({"schema_version": 1, "outcome": "built"})),
            Status::Warning
        );
        assert_eq!(
            status(json!({"schema_version": 3, "outcome": "built"})),
            Status::Unknown
        );
        assert_eq!(status(json!({"schema_version": 2})), Status::Fail);
    }

    #[test]
    fn an_unknown_field_in_the_report_is_ignored() {
        let report = json!({"schema_version": 2, "outcome": "built", "added_later": {"x": 1}});
        assert_eq!(report_check(&report, "d").status, Status::Pass);
    }

    #[test]
    fn matching_files_pass_and_each_kind_of_difference_is_named() {
        let dir = tempfile::tempdir().unwrap();
        let report = json!({"files": [entry("app.exe", b"binary")]});
        staged(dir.path(), &report, &[("app.exe", b"binary")]);
        assert_eq!(
            file_problems(&report, dir.path()).unwrap(),
            Vec::<String>::new()
        );

        fs::write(dir.path().join("app.exe"), b"binarX").unwrap();
        let problems = file_problems(&report, dir.path()).unwrap();
        assert_eq!(problems, ["app.exe does not match the hash in the report"]);

        fs::write(dir.path().join("app.exe"), b"binary and more").unwrap();
        let problems = file_problems(&report, dir.path()).unwrap();
        assert!(
            problems[0].contains("bytes, the report says 6"),
            "{problems:?}"
        );

        fs::write(dir.path().join("app.exe"), b"binary").unwrap();
        fs::write(dir.path().join("stray.txt"), b"left over").unwrap();
        let problems = file_problems(&report, dir.path()).unwrap();
        assert_eq!(
            problems,
            ["stray.txt is in the directory but not in the report"]
        );

        fs::remove_file(dir.path().join("stray.txt")).unwrap();
        fs::remove_file(dir.path().join("app.exe")).unwrap();
        let problems = file_problems(&report, dir.path()).unwrap();
        assert_eq!(problems, ["app.exe is listed in the report but is missing"]);
    }

    #[test]
    fn a_file_name_that_leaves_the_directory_is_refused_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"secret").unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        let report = json!({"files": [entry("../outside.txt", b"secret"), entry("sub/x", b"1"), entry("/abs", b"1")]});
        fs::write(inner.join("report.json"), "{}").unwrap();

        let problems = file_problems(&report, &inner).unwrap();

        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems.iter().all(|p| p.contains("not a plain file name")),
            "{problems:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_is_not_followed_out_of_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(dir.path().join("outside.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside.txt"), inner.join("app.exe")).unwrap();
        let report = json!({"files": [entry("app.exe", b"secret")]});
        fs::write(inner.join("report.json"), "{}").unwrap();

        let problems = file_problems(&report, &inner).unwrap();

        assert!(problems[0].contains("symbolic link"), "{problems:?}");
    }

    #[test]
    fn an_entry_without_hashes_is_not_taken_as_verified() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("report.json"), "{}").unwrap();
        fs::write(dir.path().join("app.exe"), b"x").unwrap();
        let report = json!({"files": [{"file": "app.exe"}]});

        let problems = file_problems(&report, dir.path()).unwrap();

        assert!(
            problems[0].contains("no complete size and hashes"),
            "{problems:?}"
        );
    }

    #[test]
    fn exposure_is_a_pass_a_warning_or_unknown_and_never_a_silent_pass() {
        let status = |report: Value| exposure_check(&report).status;
        assert_eq!(
            status(json!({"exposure": {"status": "excluded"}})),
            Status::Pass
        );
        let included =
            json!({"exposure": {"status": "included", "evidence": ["florui_devtools is linked"]}});
        assert_eq!(status(included.clone()), Status::Warning);
        assert!(
            exposure_check(&included)
                .evidence
                .contains("florui_devtools")
        );
        assert_eq!(
            status(json!({"exposure": {"status": "unknown"}})),
            Status::Unknown
        );
        assert_eq!(status(json!({})), Status::Unknown);
    }

    #[test]
    fn without_a_record_of_the_resources_nothing_is_claimed_about_them() {
        let dir = tempfile::tempdir().unwrap();
        let missing = resources_check(&json!({"schema_version": 1}), dir.path());
        let none_written = resources_check(
            &json!({"resources": {"status": "unsupported_host"}}),
            dir.path(),
        );
        assert_eq!(missing.status, Status::Unknown);
        assert_eq!(none_written.status, Status::Unknown);
    }

    #[test]
    fn the_directory_argument_is_relative_to_the_working_directory() {
        assert_eq!(
            resolve_dir(Path::new("/work"), Path::new("out dir/native")),
            Path::new("/work").join("out dir/native")
        );
    }
}
