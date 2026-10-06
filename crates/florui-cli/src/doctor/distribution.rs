//! `florui doctor --distribution` for a native build: whether the
//! application's distribution metadata is declared and usable, read from the
//! configuration and the source tree. It never builds, signs, packages or
//! touches the network.
//!
//! No installer or signing format is supported yet; the last check says so
//! explicitly instead of reporting readiness for something that was not
//! looked at.

use std::path::Path;

use florui_config::{CargoProjectFacts, Resolution};

use super::{Check, Status, display_redacted};
use crate::distribution::{read_notice, source_of};
use crate::resources;

const CATEGORY: &str = "distribution";

fn check(
    id: &'static str,
    required: bool,
    status: Status,
    evidence: String,
    remediation: Option<&str>,
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
        remediation: remediation.map(str::to_string),
    }
}

/// Why `value` is not a usable reverse-domain application identifier: at
/// least two dot-separated labels of letters, digits and hyphens, none
/// empty and none starting or ending with a hyphen.
pub(super) fn identifier_problem(value: &str) -> Option<String> {
    let labels: Vec<&str> = value.split('.').collect();
    if labels.len() < 2 {
        return Some("needs at least two dot-separated labels, like com.example.app".to_string());
    }
    for label in labels {
        let valid = !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !valid {
            return Some(format!(
                "has the label \"{label}\", which must be letters, digits and hyphens, not empty \
                 and not starting or ending with a hyphen"
            ));
        }
    }
    None
}

pub(super) fn checks(cwd: &Path, package: Option<&str>, environment: Option<&str>) -> Vec<Check> {
    let facts = match florui_config::resolve_cargo_project(cwd, package) {
        Ok(facts) => facts,
        Err(_) => {
            return vec![check(
                "distribution.project",
                false,
                Status::NotApplicable,
                "not invoked inside a resolvable Cargo project, so there is no application to \
                 check"
                    .to_string(),
                None,
            )];
        }
    };
    let selection = florui_config::EnvironmentSelection {
        name: environment.unwrap_or("production"),
        explicit: environment.is_some(),
    };
    let resolution = match florui_config::resolve(
        &facts,
        Some(florui_config::Target::Native),
        Some(selection),
    ) {
        Ok(resolution) => resolution,
        Err(error) => {
            return vec![check(
                "distribution.project",
                false,
                Status::Unknown,
                format!("the configuration could not be resolved: {error}"),
                Some("fix florui.config.toml first; the config.* checks say what is wrong"),
            )];
        }
    };

    let mut checks = vec![
        identifier_check(&resolution),
        version_check(&resolution),
        publisher_check(&resolution),
        license_check(&resolution),
        license_file_check(&resolution),
        copyright_check(&resolution),
        optional_check(
            "distribution.category",
            "bundle.category",
            resolution.config.bundle.category.as_deref(),
            &resolution,
        ),
        optional_check(
            "distribution.homepage",
            "bundle.homepage",
            resolution.config.bundle.homepage.as_deref(),
            &resolution,
        ),
    ];
    checks.push(icon_check(&resolution, &facts));
    checks.push(check(
        "distribution.installer",
        false,
        Status::Skipped,
        "no installer or signing format is supported yet: build, package and sign are separate \
         operations, and none of the last two is available"
            .to_string(),
        None,
    ));
    checks
}

fn identifier_check(resolution: &Resolution) -> Check {
    match resolution.config.app.identifier.as_deref() {
        None => check(
            "distribution.identifier",
            true,
            Status::Fail,
            "app.identifier is not set: a distributable application needs a stable reverse-domain \
             identity"
                .to_string(),
            Some("set app.identifier in florui.config.toml, for example \"com.example.app\""),
        ),
        Some(identifier) => match identifier_problem(identifier) {
            Some(problem) => check(
                "distribution.identifier",
                true,
                Status::Fail,
                format!("app.identifier \"{identifier}\" {problem}"),
                Some("use a reverse-domain identifier such as com.example.app"),
            ),
            None => check(
                "distribution.identifier",
                true,
                Status::Pass,
                format!("app.identifier is \"{identifier}\""),
                None,
            ),
        },
    }
}

fn version_check(resolution: &Resolution) -> Check {
    let version = &resolution.config.app.version;
    match resources::numeric_version(version) {
        Ok([major, minor, patch, _]) => check(
            "distribution.version",
            true,
            Status::Pass,
            format!(
                "app.version \"{version}\" maps to the Windows version {major}.{minor}.{patch}.0; \
                 the whole string is kept in the version information"
            ),
            None,
        ),
        Err(problem) => check(
            "distribution.version",
            true,
            Status::Fail,
            problem,
            Some("use a version whose major, minor and patch are numbers from 0 to 65535"),
        ),
    }
}

fn publisher_check(resolution: &Resolution) -> Check {
    match resolution.config.bundle.publisher.as_deref() {
        Some(publisher) => check(
            "distribution.publisher",
            false,
            Status::Pass,
            format!("bundle.publisher is \"{publisher}\""),
            None,
        ),
        None => check(
            "distribution.publisher",
            false,
            Status::Warning,
            "bundle.publisher is not set: the executable will show no company".to_string(),
            Some("set bundle.publisher in florui.config.toml"),
        ),
    }
}

fn license_check(resolution: &Resolution) -> Check {
    match resolution.config.bundle.license.as_deref() {
        Some(license) => check(
            "distribution.license",
            false,
            Status::Pass,
            format!(
                "the license is \"{license}\" (from the {})",
                source_label(source_of(&resolution.provenance, "bundle.license"))
            ),
            None,
        ),
        None => check(
            "distribution.license",
            false,
            Status::Warning,
            "no license is declared in bundle.license or in Cargo's license".to_string(),
            Some("set bundle.license to an SPDX expression such as \"MIT OR Apache-2.0\""),
        ),
    }
}

fn source_label(source: &str) -> &'static str {
    match source {
        "config" => "configuration",
        "cargo" => "Cargo manifest",
        _ => "defaults",
    }
}

fn license_file_check(resolution: &Resolution) -> Check {
    match resolution.config.bundle.license_file.as_deref() {
        None => check(
            "distribution.license_file",
            false,
            Status::Warning,
            "no license file is declared, so no license notice will be staged with the build"
                .to_string(),
            Some("set bundle.license_file to the file holding your license text"),
        ),
        Some(path) => match read_notice(path) {
            Ok(notice) => check(
                "distribution.license_file",
                true,
                Status::Pass,
                format!(
                    "{} ({} bytes) will be staged as the license notice",
                    display_redacted(path),
                    notice.bytes.len()
                ),
                None,
            ),
            Err(problem) => check(
                "distribution.license_file",
                true,
                Status::Fail,
                format!("{problem}: the build would be refused"),
                Some("point bundle.license_file at a readable text file inside the package"),
            ),
        },
    }
}

fn copyright_check(resolution: &Resolution) -> Check {
    match resolution.config.bundle.copyright.as_deref() {
        Some(copyright) => check(
            "distribution.copyright",
            false,
            Status::Pass,
            format!("bundle.copyright is \"{copyright}\""),
            None,
        ),
        None => check(
            "distribution.copyright",
            false,
            Status::Warning,
            "bundle.copyright is not set: the executable will show no copyright".to_string(),
            Some("set bundle.copyright in florui.config.toml"),
        ),
    }
}

/// A field a Windows executable has no place for: declared is a pass that
/// says where it is recorded instead, not declared is not applicable.
fn optional_check(
    id: &'static str,
    field: &'static str,
    value: Option<&str>,
    resolution: &Resolution,
) -> Check {
    match value {
        Some(value) => check(
            id,
            false,
            Status::Pass,
            format!(
                "{field} is \"{value}\" (from the {}); a Windows executable has no field for it, \
                 so the build report records it instead",
                source_label(source_of(&resolution.provenance, field))
            ),
            None,
        ),
        None => check(
            id,
            false,
            Status::NotApplicable,
            format!("{field} is not declared; it is optional"),
            None,
        ),
    }
}

fn icon_check(resolution: &Resolution, facts: &CargoProjectFacts) -> Check {
    let Some((declared_as, path)) = resources::declared_icon(&resolution.config.app.icons) else {
        return check(
            "distribution.icon",
            false,
            Status::Warning,
            "no icon is declared in app.icons.windows or app.icons.source: the executable will \
             have the default icon"
                .to_string(),
            Some("set app.icons.source to an SVG or a PNG of at least 256 pixels"),
        );
    };
    match resources::generate_icon(declared_as, path, &facts.package_root) {
        Err(problem) => check(
            "distribution.icon",
            true,
            Status::Fail,
            format!("{problem}: the build would be refused"),
            Some("declare a square SVG or PNG of at least 16 pixels"),
        ),
        Ok(icon) if !icon.warnings.is_empty() => check(
            "distribution.icon",
            false,
            Status::Warning,
            icon.warnings.join("; "),
            Some("use an SVG, or a PNG of at least 256 pixels"),
        ),
        Ok(icon) => check(
            "distribution.icon",
            true,
            Status::Pass,
            format!(
                "{declared_as} converts to {} sizes ({:?})",
                icon.asset.sizes.len(),
                icon.asset.sizes
            ),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identifier_is_a_reverse_domain_name() {
        for good in ["com.example.app", "com.floregreen.garden-dev", "a.b"] {
            assert!(identifier_problem(good).is_none(), "{good}");
        }
        for bad in [
            "app",
            "com..app",
            "com.-x.app",
            "com.x-.app",
            "com.exa mple.app",
            "com.é.app",
            "",
        ] {
            assert!(identifier_problem(bad).is_some(), "{bad}");
        }
    }
}
