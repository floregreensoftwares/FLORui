//! `[bundle]`'s distribution metadata: `copyright`, an SPDX `license`, a
//! `license_file`, a `category` and a `homepage`.
//!
//! `license`, `license_file` and `homepage` fall back to the package's Cargo
//! `license`, `license-file` and `homepage` when the configuration does not
//! set them (nothing else from Cargo is inherited: there is no documented
//! rule for it). A configured value always wins, and provenance says which
//! one was used. Nothing here touches the network or the file system beyond
//! checking that a license file exists.

use std::path::{Component, Path, PathBuf};

use crate::schema::Spanned;

use crate::error::{Diagnostic, SemanticConfigError, Severity};
use crate::location::LineIndex;
use crate::project::CargoProjectFacts;
use crate::resolved::{BundleConfig, FieldProvenance, Provenance};
use crate::schema::RawBundle;

const MAX_COPYRIGHT_CHARS: usize = 256;
const MAX_CATEGORY_CHARS: usize = 64;
const MAX_URL_CHARS: usize = 2048;

/// Why `value` is not a usable copyright line.
pub(crate) fn copyright_problem(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        return Some("must not be empty".to_string());
    }
    if value.chars().any(char::is_control) {
        return Some("must be a single line of text".to_string());
    }
    if value.chars().count() > MAX_COPYRIGHT_CHARS {
        return Some(format!("must be at most {MAX_COPYRIGHT_CHARS} characters"));
    }
    None
}

/// Why `value` is not a usable category token. The spelling list is
/// provisional until platform mappings are validated, so only the shape is
/// checked.
pub(crate) fn category_problem(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let shaped = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !shaped {
        return Some(
            "must be a lowercase token: letters, digits and hyphens, starting with a letter"
                .to_string(),
        );
    }
    if value.len() > MAX_CATEGORY_CHARS {
        return Some(format!("must be at most {MAX_CATEGORY_CHARS} characters"));
    }
    None
}

/// Why `value` is not a usable homepage: an absolute `http` or `https` URL
/// with a host and no credentials. It is only parsed, never fetched.
pub(crate) fn homepage_problem(value: &str) -> Option<String> {
    if value.len() > MAX_URL_CHARS {
        return Some(format!("must be at most {MAX_URL_CHARS} characters"));
    }
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Some("must not contain spaces or control characters".to_string());
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return Some("must be an absolute http or https URL".to_string());
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Some("must use the http or https scheme".to_string());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Some("must not contain credentials".to_string());
    }
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                authority
            }
        });
    if host.is_empty() {
        return Some("must have a host".to_string());
    }
    None
}

/// Why `value` is not a usable SPDX license expression: valid syntax and
/// known license and exception identifiers (or `LicenseRef-` forms).
pub(crate) fn license_problem(value: &str) -> Option<String> {
    match spdx::Expression::parse(value) {
        Ok(_) => None,
        Err(error) => Some(format!(
            "is not a valid SPDX license expression: {}",
            error.to_string().lines().last().unwrap_or("").trim()
        )),
    }
}

/// Why `value` cannot name a license file inside the package: it must be a
/// relative path that does not climb out of it.
pub(crate) fn license_file_problem(value: &str) -> Option<String> {
    let path = Path::new(value);
    if value.trim().is_empty() {
        return Some("must not be empty".to_string());
    }
    let escapes = path.is_absolute()
        || path.has_root()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)));
    escapes.then(|| "must be a relative path inside the package, without \"..\"".to_string())
}

fn invalid(
    config_path: &Path,
    lines: &LineIndex<'_>,
    field: &'static str,
    value: &Spanned<String>,
    problem: String,
) -> SemanticConfigError {
    SemanticConfigError::InvalidBundleField {
        config_path: config_path.to_owned(),
        field,
        location: lines.locate_start(value.span()),
        reason: problem,
    }
}

/// The configured fields' errors, each with its location.
pub(crate) fn validate(
    raw: &RawBundle,
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let mut check = |field: &'static str,
                     value: &Option<Spanned<String>>,
                     problem: fn(&str) -> Option<String>| {
        if let Some(value) = value
            && let Some(problem) = problem(value.get_ref())
        {
            errors.push(invalid(config_path, lines, field, value, problem));
        }
    };
    check("copyright", &raw.copyright, copyright_problem);
    check("category", &raw.category, category_problem);
    check("homepage", &raw.homepage, homepage_problem);
    check("license", &raw.license, license_problem);
    check("license_file", &raw.license_file, license_file_problem);
}

fn warn(
    diagnostics: &mut Vec<Diagnostic>,
    code: &'static str,
    field: &'static str,
    message: String,
) {
    diagnostics.push(Diagnostic {
        severity: Severity::Warning,
        code,
        message,
        location: None,
        field,
    });
}

/// Resolves the bundle fields. Called only after [`validate`] found nothing
/// wrong with the configured values.
pub(crate) fn resolve(
    raw: Option<&RawBundle>,
    publisher: Option<String>,
    facts: &CargoProjectFacts,
    config_dir: &Path,
    lines: Option<&LineIndex<'_>>,
    provenance: &mut Vec<FieldProvenance>,
    diagnostics: &mut Vec<Diagnostic>,
) -> BundleConfig {
    let configured = |value: Option<&Spanned<String>>, name: &'static str| {
        value.map(|spanned| {
            let location = lines.map(|lines| lines.locate_start(spanned.span()));
            (
                spanned.get_ref().clone(),
                Provenance::ConfigFile(location),
                name,
            )
        })
    };
    let mut record =
        |picked: Option<(String, Provenance, &'static str)>, name: &'static str| match picked {
            Some((value, source, _)) => {
                provenance.push(FieldProvenance {
                    field: name,
                    provenance: source,
                });
                Some(value)
            }
            None => {
                provenance.push(FieldProvenance {
                    field: name,
                    provenance: Provenance::BuiltinDefault,
                });
                None
            }
        };

    let copyright = record(
        configured(raw.and_then(|b| b.copyright.as_ref()), "bundle.copyright"),
        "bundle.copyright",
    );
    let category = record(
        configured(raw.and_then(|b| b.category.as_ref()), "bundle.category"),
        "bundle.category",
    );

    let inherited_homepage =
        facts
            .homepage
            .clone()
            .and_then(|value| match homepage_problem(&value) {
                None => Some((value, Provenance::CargoManifest, "bundle.homepage")),
                Some(problem) => {
                    warn(
                        diagnostics,
                        "config.bundle_inherited_invalid",
                        "bundle.homepage",
                        format!("the package's Cargo homepage {problem}, so it is not used"),
                    );
                    None
                }
            });
    let homepage = record(
        configured(raw.and_then(|b| b.homepage.as_ref()), "bundle.homepage").or(inherited_homepage),
        "bundle.homepage",
    );

    let inherited_license = facts
        .license
        .clone()
        .and_then(|value| match license_problem(&value) {
            None => Some((value, Provenance::CargoManifest, "bundle.license")),
            Some(problem) => {
                warn(
                    diagnostics,
                    "config.bundle_inherited_invalid",
                    "bundle.license",
                    format!("the package's Cargo license {problem}, so it is not used"),
                );
                None
            }
        });
    let license = record(
        configured(raw.and_then(|b| b.license.as_ref()), "bundle.license").or(inherited_license),
        "bundle.license",
    );

    let inherited_file = facts.license_file.as_ref().and_then(|path| {
        let text = path.to_string_lossy().into_owned();
        match license_file_problem(&text) {
            None => Some((text, Provenance::CargoManifest, "bundle.license_file")),
            Some(problem) => {
                warn(
                    diagnostics,
                    "config.bundle_inherited_invalid",
                    "bundle.license_file",
                    format!("the package's Cargo license-file {problem}, so it is not used"),
                );
                None
            }
        }
    });
    let license_file = record(
        configured(
            raw.and_then(|b| b.license_file.as_ref()),
            "bundle.license_file",
        )
        .or(inherited_file),
        "bundle.license_file",
    )
    .map(|relative| -> PathBuf { config_dir.join(relative) });
    if let Some(path) = &license_file
        && !path.is_file()
    {
        warn(
            diagnostics,
            "config.bundle_license_file_present",
            "bundle.license_file",
            format!(
                "bundle.license_file points to {}, which is not a file",
                path.display()
            ),
        );
    }

    BundleConfig {
        publisher,
        copyright,
        license,
        license_file,
        category,
        homepage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copyright_must_be_one_non_empty_line() {
        assert!(copyright_problem("Copyright 2026 Garden contributors").is_none());
        assert!(copyright_problem("  ").unwrap().contains("empty"));
        assert!(copyright_problem("a\nb").unwrap().contains("single line"));
        assert!(copyright_problem(&"x".repeat(300)).unwrap().contains("256"));
    }

    #[test]
    fn category_is_a_lowercase_token() {
        for good in ["productivity", "developer-tools", "a1"] {
            assert!(category_problem(good).is_none(), "{good}");
        }
        for bad in ["", "Productivity", "1abc", "two words", "under_score"] {
            assert!(category_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn homepage_is_an_absolute_http_url_without_credentials() {
        for good in [
            "https://example.com",
            "http://example.com/garden?x=1#top",
            "https://example.com:8443/path",
            "HTTPS://example.com",
        ] {
            assert!(homepage_problem(good).is_none(), "{good}");
        }
        for (bad, why) in [
            ("example.com", "absolute"),
            ("javascript://alert(1)", "http or https"),
            ("ftp://example.com", "http or https"),
            ("https://user:pw@example.com", "credentials"),
            ("https://user@example.com", "credentials"),
            ("https:///path", "host"),
            ("https://", "host"),
            ("https://exa mple.com", "spaces"),
        ] {
            let problem = homepage_problem(bad).unwrap_or_else(|| panic!("{bad} was accepted"));
            assert!(problem.contains(why), "{bad}: {problem}");
        }
    }

    #[test]
    fn license_is_a_real_spdx_expression() {
        for good in [
            "MIT OR Apache-2.0",
            "Apache-2.0 WITH LLVM-exception",
            "GPL-3.0-or-later",
            "MIT AND (Apache-2.0 OR BSD-3-Clause)",
            "LicenseRef-Proprietary",
        ] {
            assert!(license_problem(good).is_none(), "{good}");
        }
        for bad in [
            "MIT OR",
            "MIT OR (Apache-2.0",
            "Aache-2.0",
            "mit",
            "MIT/Apache-2.0",
            "",
        ] {
            assert!(
                license_problem(bad).unwrap().contains("SPDX"),
                "{bad} was accepted"
            );
        }
    }

    #[test]
    fn a_license_file_stays_inside_the_package() {
        assert!(license_file_problem("LICENSE-MIT").is_none());
        assert!(license_file_problem("legal/NOTICE.txt").is_none());
        for bad in [
            "../LICENSE",
            "a/../../LICENSE",
            "/etc/passwd",
            "",
            "C:\\x\\LICENSE",
        ] {
            assert!(license_file_problem(bad).is_some(), "{bad}");
        }
    }
}
