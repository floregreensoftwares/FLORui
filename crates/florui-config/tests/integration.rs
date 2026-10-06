//! Tests against a real, temporary Cargo workspace and a real `cargo
//! metadata` shell-out -- faking cwd-based resolution or workspace-version
//! inheritance would misrepresent what's actually being verified for
//! those two, since both depend on real cargo semantics this crate itself
//! does not implement.

use florui_config::{ProjectResolutionError, resolve, resolve_cargo_project};
use std::path::{Path, PathBuf};

fn scaffold_workspace_root(root: &Path, members: &[&str], workspace_version: Option<&str>) {
    let member_list = members
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut manifest = format!("[workspace]\nmembers = [{member_list}]\n");
    if let Some(version) = workspace_version {
        manifest.push_str(&format!("\n[workspace.package]\nversion = \"{version}\"\n"));
    }
    std::fs::write(root.join("Cargo.toml"), manifest).unwrap();
}

fn scaffold_package(root: &Path, name: &str, version_decl: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\n{version_decl}\nedition = \"2021\"\n"),
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    dir
}

#[test]
fn nested_directory_invocation_resolves_the_owning_package() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["app"], None);
    let package_dir = scaffold_package(root.path(), "app", "version = \"0.1.0\"");
    std::fs::create_dir_all(package_dir.join("src/nested")).unwrap();

    let facts = resolve_cargo_project(&package_dir.join("src/nested"), None).unwrap();
    assert_eq!(facts.package_root, package_dir);
    assert_eq!(facts.package_name, "app");
}

#[test]
fn nested_directory_invocation_from_a_path_containing_spaces() {
    let root = tempfile::tempdir().unwrap();
    let spaced_root = root.path().join("has spaces in it");
    std::fs::create_dir_all(&spaced_root).unwrap();
    scaffold_workspace_root(&spaced_root, &["app"], None);
    let package_dir = scaffold_package(&spaced_root, "app", "version = \"0.1.0\"");
    std::fs::create_dir_all(package_dir.join("src/nested")).unwrap();

    let facts = resolve_cargo_project(&package_dir.join("src/nested"), None).unwrap();
    assert_eq!(facts.package_root, package_dir);
}

#[test]
fn ambiguous_workspace_root_invocation_without_package_flag_lists_candidates() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["a", "b"], None);
    scaffold_package(root.path(), "a", "version = \"0.1.0\"");
    scaffold_package(root.path(), "b", "version = \"0.1.0\"");

    let err = resolve_cargo_project(root.path(), None).unwrap_err();
    match err {
        ProjectResolutionError::NoResolvableRoot { candidates } => {
            assert!(candidates.contains(&"a".to_owned()));
            assert!(candidates.contains(&"b".to_owned()));
        }
        other => panic!("expected NoResolvableRoot, got {other:?}"),
    }
}

#[test]
fn explicit_package_flag_disambiguates_from_the_workspace_root() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["a", "b"], None);
    scaffold_package(root.path(), "a", "version = \"0.1.0\"");
    scaffold_package(root.path(), "b", "version = \"0.1.0\"");

    let facts = resolve_cargo_project(root.path(), Some("b")).unwrap();
    assert_eq!(facts.package_name, "b");
}

#[test]
fn unknown_package_flag_is_a_clear_error() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["a"], None);
    scaffold_package(root.path(), "a", "version = \"0.1.0\"");

    let err = resolve_cargo_project(root.path(), Some("nope")).unwrap_err();
    match err {
        ProjectResolutionError::UnknownPackage {
            requested,
            available,
        } => {
            assert_eq!(requested, "nope");
            assert_eq!(available, vec!["a".to_owned()]);
        }
        other => panic!("expected UnknownPackage, got {other:?}"),
    }
}

#[test]
fn workspace_version_inheritance_confirmed_end_to_end() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["app"], Some("1.2.3"));
    let package_dir = scaffold_package(root.path(), "app", "version.workspace = true");
    std::fs::write(
        package_dir.join("florui.config.toml"),
        "schema_version = 1\n[app]\nversion = { workspace = true }\n",
    )
    .unwrap();

    let facts = resolve_cargo_project(&package_dir, None).unwrap();
    assert_eq!(facts.package_version, "1.2.3");
    let resolution = resolve(&facts, None, None).unwrap();
    assert_eq!(resolution.config.app.version, "1.2.3");
}

#[test]
fn workspace_version_inheritance_missing_is_a_hard_error_end_to_end() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["app"], Some("1.2.3"));
    // The same shape examples/florui-example-app/Cargo.toml has today: a
    // literal version, not workspace inheritance.
    let package_dir = scaffold_package(root.path(), "app", "version = \"0.1.0\"");
    std::fs::write(
        package_dir.join("florui.config.toml"),
        "schema_version = 1\n[app]\nversion = { workspace = true }\n",
    )
    .unwrap();

    let facts = resolve_cargo_project(&package_dir, None).unwrap();
    let err = resolve(&facts, None, None).unwrap_err();
    match err {
        florui_config::ConfigError::Semantic(errors) => {
            assert!(matches!(
                errors[0],
                florui_config::SemanticConfigError::WorkspaceVersionNotInherited { .. }
            ));
        }
        other => panic!("expected Semantic, got {other:?}"),
    }
}

#[test]
fn resolving_a_workspace_member_does_not_create_a_lockfile() {
    let root = tempfile::tempdir().unwrap();
    scaffold_workspace_root(root.path(), &["a", "b"], None);
    scaffold_package(root.path(), "a", "version = \"0.1.0\"");
    let b = scaffold_package(root.path(), "b", "version = \"0.1.0\"");

    resolve_cargo_project(&b, None).unwrap();
    resolve_cargo_project(root.path(), Some("a")).unwrap();

    assert!(
        !root.path().join("Cargo.lock").exists(),
        "resolving the project wrote a Cargo.lock"
    );
}

#[test]
fn resolving_a_single_package_does_not_create_a_lockfile() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "").unwrap();

    let facts = resolve_cargo_project(&root.path().join("src"), None).unwrap();

    assert_eq!(facts.package_name, "solo");
    assert!(
        !root.path().join("Cargo.lock").exists(),
        "resolving the project wrote a Cargo.lock"
    );
}

#[test]
fn a_package_that_is_also_the_workspace_root_resolves_from_a_nested_directory() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src/nested")).unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"rootpkg\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\nmembers = [\"member\"]\n",
    )
    .unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "").unwrap();
    scaffold_package(root.path(), "member", "version = \"0.1.0\"");

    let from_root_package = resolve_cargo_project(&root.path().join("src/nested"), None).unwrap();
    let from_member = resolve_cargo_project(&root.path().join("member/src"), None).unwrap();

    assert_eq!(from_root_package.package_name, "rootpkg");
    assert_eq!(from_member.package_name, "member");
}
