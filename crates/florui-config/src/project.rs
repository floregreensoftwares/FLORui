//! Resolves which Cargo package `florui` is acting on and what it needs to
//! know about it -- the one place this logic lives now, replacing what
//! used to be two independent implementations in `florui-cli` (`main.rs`'s
//! `resolve_project`/`parse_resolved_project` and `doctor.rs`'s
//! `resolve_project_facts`/`parse_project_facts`). Keeps the same split
//! each of those already used on its own: a pure, JSON-in half
//! ([`parse_cargo_project_facts`]) that's testable against a hand-built
//! payload, and a thin shell-out wrapper ([`resolve_cargo_project`]) around
//! it.

use crate::location::LocatedValue;
use crate::schema::RawVersion;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct CargoProjectFacts {
    pub workspace_root: PathBuf,
    pub package_root: PathBuf,
    pub manifest_path: PathBuf,
    /// Raw `Cargo.toml` text, read once and reused both for
    /// `legacy_dev_example` (below) and for `resolve()`'s on-demand
    /// `{ workspace = true }` inheritance check -- `cargo metadata`'s JSON
    /// carries neither spans nor the literal, undigested `version` key, so
    /// both need the manifest's own text, not the JSON.
    pub manifest_text: String,
    pub target_dir: PathBuf,
    pub package_name: String,
    /// Already flattened to a concrete version by cargo, regardless of
    /// whether the manifest declared it literally or via
    /// `version.workspace = true`.
    pub package_version: String,
    pub example_targets: Vec<String>,
    pub legacy_dev_example: Option<LocatedValue<String>>,
    /// The package's own Cargo `license` (an SPDX expression), if any.
    pub license: Option<String>,
    /// The package's own Cargo `license-file`, as written in its manifest.
    pub license_file: Option<PathBuf>,
    /// The package's own Cargo `homepage`, if any.
    pub homepage: Option<String>,
}

#[derive(Debug)]
pub enum ProjectResolutionError {
    Metadata(String),
    /// `cargo metadata` could not resolve a single owning package for the
    /// current directory, and no `--package` was given to disambiguate.
    NoResolvableRoot {
        candidates: Vec<String>,
    },
    UnknownPackage {
        requested: String,
        available: Vec<String>,
    },
}

impl std::fmt::Display for ProjectResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProjectResolutionError::Metadata(message) => write!(f, "{message}"),
            ProjectResolutionError::NoResolvableRoot { candidates } => {
                write!(
                    f,
                    "could not determine which package to resolve -- invoke from inside a \
                     specific package's own directory, or pass --package explicitly"
                )?;
                if !candidates.is_empty() {
                    write!(f, " (available packages: {})", candidates.join(", "))?;
                }
                Ok(())
            }
            ProjectResolutionError::UnknownPackage {
                requested,
                available,
            } => write!(
                f,
                "--package {requested} does not match any workspace package (available: {})",
                available.join(", ")
            ),
        }
    }
}

impl std::error::Error for ProjectResolutionError {}

/// Shells out to `cargo metadata --no-deps` from `cwd` and reads the
/// resolved package's own `Cargo.toml`, then resolves both into one
/// [`CargoProjectFacts`]. `cwd` is explicit rather than inherited from the
/// process so this is actually testable against a real temporary
/// workspace without mutating global process state.
///
/// `--no-deps` is deliberate: nothing here needs the dependency graph, and
/// without it cargo writes a missing `Cargo.lock` as a side effect of
/// resolving, which a command that must not change the project (`doctor`)
/// cannot afford, and it would also make this lookup depend on the registry
/// index. It leaves `resolve.root` null, so the package `cwd` belongs to is
/// found the way cargo itself finds it: the nearest `Cargo.toml` above `cwd`.
pub fn resolve_cargo_project(
    cwd: &Path,
    explicit_package: Option<&str>,
) -> Result<CargoProjectFacts, ProjectResolutionError> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(cwd)
        .output()
        .map_err(|err| {
            ProjectResolutionError::Metadata(format!("could not run cargo metadata: {err}"))
        })?;
    if !output.status.success() {
        return Err(ProjectResolutionError::Metadata(format!(
            "cargo metadata exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut metadata: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        ProjectResolutionError::Metadata(format!("could not parse cargo metadata output: {err}"))
    })?;
    if let Some(root) = package_id_at(&metadata, cwd) {
        metadata["resolve"] = serde_json::json!({ "root": root });
    }
    let partial = parse_cargo_project_facts(&metadata, explicit_package)?;

    let manifest_text = std::fs::read_to_string(&partial.manifest_path).map_err(|err| {
        ProjectResolutionError::Metadata(format!(
            "could not read {}: {err}",
            partial.manifest_path.display()
        ))
    })?;
    let legacy_dev_example = extract_legacy_dev_example(&manifest_text);

    Ok(CargoProjectFacts {
        manifest_text,
        legacy_dev_example,
        ..partial
    })
}

/// The pure, JSON-in half of [`resolve_cargo_project`] -- separated out so
/// it's testable against a hand-built `cargo metadata` payload without
/// shelling out. Leaves `manifest_text`/`legacy_dev_example` empty/`None`;
/// [`resolve_cargo_project`] fills those in from the real manifest file
/// after this succeeds.
pub fn parse_cargo_project_facts(
    metadata: &Value,
    explicit_package: Option<&str>,
) -> Result<CargoProjectFacts, ProjectResolutionError> {
    let workspace_root: PathBuf = metadata
        .get("workspace_root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(
                "cargo metadata output had no workspace_root field".to_owned(),
            )
        })?;
    let target_dir: PathBuf = metadata
        .get("target_directory")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(
                "cargo metadata output had no target_directory field".to_owned(),
            )
        })?;

    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let workspace_member_ids: Vec<&str> = metadata
        .get("workspace_members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let member_package = |id: &str| -> Option<&Value> {
        packages
            .iter()
            .find(|package| package.get("id").and_then(Value::as_str) == Some(id))
    };
    let available_names: Vec<String> = workspace_member_ids
        .iter()
        .filter_map(|id| member_package(id))
        .filter_map(|package| package.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();

    let root_id: String = match explicit_package {
        Some(requested) => workspace_member_ids
            .iter()
            .find(|&&id| {
                member_package(id)
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    == Some(requested)
            })
            .map(|&id| id.to_owned())
            .ok_or_else(|| ProjectResolutionError::UnknownPackage {
                requested: requested.to_owned(),
                available: available_names.clone(),
            })?,
        None => metadata
            .get("resolve")
            .and_then(|resolve| resolve.get("root"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| ProjectResolutionError::NoResolvableRoot {
                candidates: available_names.clone(),
            })?,
    };

    let package = member_package(&root_id)
        .or_else(|| {
            packages
                .iter()
                .find(|p| p.get("id").and_then(Value::as_str) == Some(root_id.as_str()))
        })
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(
                "cargo metadata's resolved package was not in its own packages list".to_owned(),
            )
        })?;

    let manifest_path: PathBuf = package
        .get("manifest_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(
                "cargo metadata's package entry had no manifest_path".to_owned(),
            )
        })?;
    let package_root = manifest_path
        .parent()
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(format!(
                "{} has no parent directory",
                manifest_path.display()
            ))
        })?
        .to_owned();
    let package_name = package
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(
                "cargo metadata's package entry had no name".to_owned(),
            )
        })?
        .to_owned();
    let package_version = package
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ProjectResolutionError::Metadata(
                "cargo metadata's package entry had no version".to_owned(),
            )
        })?
        .to_owned();

    let example_targets = package
        .get("targets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|target| {
            target
                .get("kind")
                .and_then(Value::as_array)
                .is_some_and(|kinds| kinds.iter().any(|kind| kind.as_str() == Some("example")))
        })
        .filter_map(|target| target.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();

    Ok(CargoProjectFacts {
        workspace_root,
        package_root,
        manifest_path,
        manifest_text: String::new(),
        target_dir,
        package_name,
        package_version,
        example_targets,
        legacy_dev_example: None,
        license: cargo_string(package, "license"),
        license_file: cargo_string(package, "license_file").map(PathBuf::from),
        homepage: cargo_string(package, "homepage"),
    })
}

#[derive(Deserialize)]
struct CargoManifestProbe {
    package: Option<CargoPackageProbe>,
}
#[derive(Deserialize)]
struct CargoPackageProbe {
    version: Option<RawVersion>,
    metadata: Option<CargoMetadataProbe>,
}
#[derive(Deserialize)]
struct CargoMetadataProbe {
    florui: Option<CargoFlorolMetadataProbe>,
}
#[derive(Deserialize)]
struct CargoFlorolMetadataProbe {
    dev: Option<CargoDevProbe>,
}
#[derive(Deserialize)]
struct CargoDevProbe {
    example: Option<toml::Spanned<String>>,
}

/// A non-empty string key of a `cargo metadata` package entry.
fn cargo_string(package: &Value, key: &str) -> Option<String> {
    package
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

/// The id of the workspace member whose manifest is the nearest `Cargo.toml`
/// at or above `cwd`, which is the package cargo itself would build from
/// there. `None` when that manifest is a virtual workspace root (it has no
/// package) or no member owns it.
fn package_id_at(metadata: &Value, cwd: &Path) -> Option<String> {
    let nearest = cwd
        .ancestors()
        .map(|dir| dir.join("Cargo.toml"))
        .find(|manifest| manifest.is_file())?;
    let nearest = std::fs::canonicalize(nearest).ok()?;
    let members: Vec<&str> = metadata
        .get("workspace_members")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    metadata
        .get("packages")?
        .as_array()?
        .iter()
        .filter(|package| {
            package
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| members.contains(&id))
        })
        .find(|package| {
            package
                .get("manifest_path")
                .and_then(Value::as_str)
                .and_then(|path| std::fs::canonicalize(path).ok())
                .is_some_and(|manifest| manifest == nearest)
        })
        .and_then(|package| package.get("id")?.as_str())
        .map(str::to_owned)
}

/// Pulls `[package.metadata.florui.dev].example` out of a `Cargo.toml`'s
/// raw text, with its span -- not from `cargo metadata`'s JSON, which
/// carries no spans. Not `deny_unknown_fields`: a real `Cargo.toml` has
/// many fields this probe doesn't care about. A manifest this probe can't
/// even parse (which would mean cargo itself couldn't have resolved this
/// project) yields `None` rather than propagating a parse error here --
/// [`crate::resolve::check_workspace_inheritance`] is the one that
/// actually needs to surface a manifest parse problem, and only when
/// `{ workspace = true }` is actually requested.
fn extract_legacy_dev_example(manifest_text: &str) -> Option<LocatedValue<String>> {
    let probe: CargoManifestProbe = toml::from_str(manifest_text).ok()?;
    let spanned = probe.package?.metadata?.florui?.dev?.example?;
    let span = spanned.span();
    Some(LocatedValue {
        value: spanned.into_inner(),
        span,
    })
}

/// Reads and parses just `[package].version` from `manifest_path`'s own
/// text, verifying it's actually `version.workspace = true` -- `cargo
/// metadata` always reports an already-flattened, concrete version
/// regardless of how it was declared, so this can't be checked from that
/// alone.
pub(crate) fn parse_package_version(
    manifest_text: &str,
) -> Result<Option<RawVersion>, toml::de::Error> {
    let probe: CargoManifestProbe = toml::from_str(manifest_text)?;
    Ok(probe.package.and_then(|package| package.version))
}

/// Where `florui.config.toml` would live for `package_root`, if it exists.
pub fn config_file_path(package_root: &Path) -> PathBuf {
    package_root.join("florui.config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata_with(root_id: &str, dev_example: Option<&str>, example_targets: &[&str]) -> Value {
        let metadata_field = match dev_example {
            Some(example) => json!({"florui": {"dev": {"example": example}}}),
            None => json!({}),
        };
        let targets: Vec<Value> = example_targets
            .iter()
            .map(|name| json!({"name": name, "kind": ["example"]}))
            .chain(std::iter::once(json!({"name": "main", "kind": ["bin"]})))
            .collect();
        json!({
            "workspace_root": "/workspace",
            "target_directory": "/workspace/target",
            "resolve": {"root": root_id},
            "workspace_members": [root_id],
            "packages": [
                {
                    "id": root_id,
                    "name": "app",
                    "version": "0.1.0",
                    "manifest_path": "/workspace/app/Cargo.toml",
                    "metadata": metadata_field,
                    "targets": targets,
                },
            ],
        })
    }

    #[test]
    fn resolves_workspace_root_target_dir_and_package_root() {
        let metadata = metadata_with("app#0.1.0", None, &[]);
        let facts = parse_cargo_project_facts(&metadata, None).unwrap();
        assert_eq!(facts.workspace_root, Path::new("/workspace"));
        assert_eq!(facts.target_dir, Path::new("/workspace/target"));
        assert_eq!(facts.package_root, Path::new("/workspace/app"));
        assert_eq!(facts.package_name, "app");
        assert_eq!(facts.package_version, "0.1.0");
    }

    #[test]
    fn excludes_non_example_targets() {
        let metadata = metadata_with("app#0.1.0", None, &["counter"]);
        let facts = parse_cargo_project_facts(&metadata, None).unwrap();
        assert!(facts.example_targets.iter().any(|n| n == "counter"));
        assert!(!facts.example_targets.iter().any(|n| n == "main"));
    }

    #[test]
    fn no_resolvable_root_lists_candidate_packages() {
        let metadata = json!({
            "workspace_root": "/workspace",
            "target_directory": "/workspace/target",
            "resolve": {"root": null},
            "workspace_members": ["a#0.1.0", "b#0.1.0"],
            "packages": [
                {"id": "a#0.1.0", "name": "a", "version": "0.1.0", "manifest_path": "/workspace/a/Cargo.toml", "targets": []},
                {"id": "b#0.1.0", "name": "b", "version": "0.1.0", "manifest_path": "/workspace/b/Cargo.toml", "targets": []},
            ],
        });
        let err = parse_cargo_project_facts(&metadata, None).unwrap_err();
        match err {
            ProjectResolutionError::NoResolvableRoot { candidates } => {
                assert_eq!(candidates, vec!["a".to_owned(), "b".to_owned()]);
            }
            other => panic!("expected NoResolvableRoot, got {other:?}"),
        }
    }

    #[test]
    fn explicit_package_selects_that_member_even_without_a_resolvable_root() {
        let metadata = json!({
            "workspace_root": "/workspace",
            "target_directory": "/workspace/target",
            "resolve": {"root": null},
            "workspace_members": ["a#0.1.0", "b#0.1.0"],
            "packages": [
                {"id": "a#0.1.0", "name": "a", "version": "0.1.0", "manifest_path": "/workspace/a/Cargo.toml", "targets": []},
                {"id": "b#0.1.0", "name": "b", "version": "0.1.0", "manifest_path": "/workspace/b/Cargo.toml", "targets": []},
            ],
        });
        let facts = parse_cargo_project_facts(&metadata, Some("b")).unwrap();
        assert_eq!(facts.package_name, "b");
    }

    #[test]
    fn unknown_explicit_package_lists_available_names() {
        let metadata = metadata_with("app#0.1.0", None, &[]);
        let err = parse_cargo_project_facts(&metadata, Some("nope")).unwrap_err();
        match err {
            ProjectResolutionError::UnknownPackage {
                requested,
                available,
            } => {
                assert_eq!(requested, "nope");
                assert_eq!(available, vec!["app".to_owned()]);
            }
            other => panic!("expected UnknownPackage, got {other:?}"),
        }
    }

    #[test]
    fn extract_legacy_dev_example_reads_the_declared_value_with_a_span() {
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[package.metadata.florui.dev]\nexample = \"counter\"\n";
        let located = extract_legacy_dev_example(manifest).unwrap();
        assert_eq!(located.value, "counter");
        assert_eq!(&manifest[located.span], "\"counter\"");
    }

    #[test]
    fn extract_legacy_dev_example_is_none_when_undeclared() {
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
        assert!(extract_legacy_dev_example(manifest).is_none());
    }

    #[test]
    fn parse_package_version_reads_a_literal_string() {
        let manifest = "[package]\nname = \"app\"\nversion = \"1.2.3\"\n";
        let version = parse_package_version(manifest).unwrap();
        assert!(matches!(version, Some(RawVersion::Literal { value, .. }) if value == "1.2.3"));
    }

    #[test]
    fn parse_package_version_reads_workspace_inheritance() {
        let manifest = "[package]\nname = \"app\"\nversion.workspace = true\n";
        let version = parse_package_version(manifest).unwrap();
        assert!(matches!(
            version,
            Some(RawVersion::Workspace {
                requested: true,
                ..
            })
        ));
    }
}
