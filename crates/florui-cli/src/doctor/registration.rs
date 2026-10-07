//! `florui doctor --registration`: what the operating system knows of the
//! application's URL schemes and file types, read without changing anything.
//! Reports whether the application is registered and still as it was
//! registered, whether registering it would take something over, and whether
//! two environments of the same project would contend for a scheme or a file
//! type.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use florui_config::{EnvironmentSelection, Target};

use super::{Check, Status};
use crate::register_cmd::{declaration_from, load_project};
use crate::registration::{self, Declaration, OWNER_VALUE, Registry};

const CATEGORY: &str = "registration";

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

pub(super) fn checks(
    package: Option<&str>,
    environment: Option<&str>,
    registry: &dyn Registry,
) -> Vec<Check> {
    let (facts, config) = match load_project(package, environment) {
        Ok(project) => project,
        Err(reason) => {
            return vec![check(
                "registration.project",
                false,
                Status::NotApplicable,
                format!("there is no application to check: {reason}"),
                None,
            )];
        }
    };
    let declaration = match declaration_from(&config) {
        Ok(declaration) => declaration,
        Err(reason) => {
            return vec![check(
                "registration.project",
                false,
                Status::Unknown,
                reason,
                Some("set app.identifier in florui.config.toml"),
            )];
        }
    };
    if declaration.schemes.is_empty() && declaration.associations.is_empty() {
        return vec![check(
            "registration.declared",
            false,
            Status::NotApplicable,
            "[app.activation] declares no URL scheme and no file type, so there is nothing to register"
                .to_string(),
            None,
        )];
    }
    let staged = facts
        .target_dir
        .join("florui-build")
        .join(&facts.package_name)
        .join("native")
        .join(format!(
            "{}{}",
            facts.package_name,
            std::env::consts::EXE_SUFFIX
        ));
    vec![
        state_check(&declaration, &staged, registry),
        conflicts_check(&declaration, &staged, registry),
        isolation_check(package, environment, &facts),
        default_requests_check(&config, &declaration, registry),
    ]
}

fn state_check(declaration: &Declaration, staged: &Path, registry: &dyn Registry) -> Check {
    let identifier = &declaration.identifier;
    let Some(record) = registration::read_record(registry, identifier) else {
        return check(
            "registration.state",
            false,
            Status::Skipped,
            format!("{identifier} is not registered with the system"),
            Some("run `florui build` and then `florui register`"),
        );
    };
    let mut problems = Vec::new();
    if !std::path::Path::new(&record.exe).is_file() {
        problems.push(format!("the registered executable {} is gone", record.exe));
    }
    for key in &record.trees {
        if !registry.key_exists(key) {
            problems.push(format!("HKCU\\{key} is missing"));
        } else if key.starts_with("Software\\Classes")
            && registry.get(key, OWNER_VALUE).as_deref() != Some(identifier.as_str())
        {
            problems.push(format!("HKCU\\{key} is no longer owned by {identifier}"));
        }
    }
    let command = registration::command_for(std::path::Path::new(&record.exe)).ok();
    for scheme in &record.schemes {
        let at = format!("Software\\Classes\\{scheme}\\shell\\open\\command");
        if registry.get(&at, "") != command {
            problems.push(format!(
                "the command of {scheme}:// no longer runs the registered executable"
            ));
        }
    }
    let declared: Vec<&String> = declaration.schemes.iter().collect();
    for scheme in &declared {
        if !record
            .schemes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(scheme))
        {
            problems.push(format!("{scheme}:// is declared but was not registered"));
        }
    }
    for association in &declaration.associations {
        if !record.extensions.contains(&association.extension) {
            problems.push(format!(
                ".{} is declared but was not registered",
                association.extension
            ));
        }
    }
    if staged.is_file() && std::path::Path::new(&record.exe) != staged {
        problems.push(format!(
            "it is registered for {} but the project's staged build is {}",
            record.exe,
            staged.display()
        ));
    }
    if problems.is_empty() {
        check(
            "registration.state",
            false,
            Status::Pass,
            format!(
                "{identifier} is registered for {} and still as it was registered",
                describe(declaration)
            ),
            None,
        )
    } else {
        check(
            "registration.state",
            false,
            Status::Fail,
            problems.join("; "),
            Some("run `florui unregister` and `florui register` again"),
        )
    }
}

fn describe(declaration: &Declaration) -> String {
    let mut parts: Vec<String> = declaration
        .schemes
        .iter()
        .map(|s| format!("{s}://"))
        .collect();
    parts.extend(
        declaration
            .associations
            .iter()
            .map(|a| format!(".{}", a.extension)),
    );
    parts.join(", ")
}

fn conflicts_check(declaration: &Declaration, staged: &Path, registry: &dyn Registry) -> Check {
    // The executable only matters to the command that gets written, so a
    // project that has not been built yet is still checked.
    let exe = if staged.is_file() {
        staged.to_path_buf()
    } else {
        PathBuf::from("C:\\app\\app.exe")
    };
    let plan = registration::plan(declaration, &exe, registry);
    if plan.conflicts.is_empty() {
        check(
            "registration.conflicts",
            false,
            Status::Pass,
            "registering would take nothing over: no reserved scheme, and nothing of another application's is in the way"
                .to_string(),
            None,
        )
    } else {
        let text: Vec<String> = plan
            .conflicts
            .iter()
            .map(|c| format!("{}: {}", c.what, c.reason))
            .collect();
        check(
            "registration.conflicts",
            false,
            Status::Fail,
            text.join("; "),
            Some("choose another scheme or association identity in [app.activation]"),
        )
    }
}

/// Resolves every environment the configuration declares and reports a URL
/// scheme or file type that two of them would both register.
fn isolation_check(
    package: Option<&str>,
    environment: Option<&str>,
    facts: &florui_config::CargoProjectFacts,
) -> Check {
    let names = match florui_config::resolve(
        facts,
        Some(Target::Native),
        environment.map(|name| EnvironmentSelection {
            name,
            explicit: true,
        }),
    ) {
        Ok(resolution) => resolution.environment.declared,
        Err(_) => Vec::new(),
    };
    let _ = package;
    if names.is_empty() {
        return check(
            "registration.isolation",
            false,
            Status::NotApplicable,
            "no [environments] are declared, so only one configuration can register".to_string(),
            None,
        );
    }
    // The base configuration is an environment too: it is what `production`
    // resolves to when no overlay is named.
    let mut candidates: Vec<(String, Option<&str>)> =
        vec![("the base configuration".to_string(), None)];
    candidates.extend(names.iter().map(|name| (name.clone(), Some(name.as_str()))));
    let mut owners: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (label, selected) in &candidates {
        let selection = selected.map(|name| EnvironmentSelection {
            name,
            explicit: true,
        });
        let Ok(resolution) = florui_config::resolve(facts, Some(Target::Native), selection) else {
            continue;
        };
        let Ok(declaration) = declaration_from(&resolution.config) else {
            continue;
        };
        for scheme in &declaration.schemes {
            owners
                .entry(format!("the URL scheme {}://", scheme.to_ascii_lowercase()))
                .or_default()
                .push(label.clone());
        }
        for association in &declaration.associations {
            owners
                .entry(format!(
                    "the file type .{}",
                    association.extension.to_ascii_lowercase()
                ))
                .or_default()
                .push(label.clone());
        }
    }
    let shared: Vec<String> = owners
        .into_iter()
        .filter(|(_, environments)| environments.len() > 1)
        .map(|(what, environments)| {
            format!("{what} is registered by {}", environments.join(" and "))
        })
        .collect();
    if shared.is_empty() {
        check(
            "registration.isolation",
            false,
            Status::Pass,
            format!(
                "no URL scheme or file type is shared by two of {} environments",
                names.len()
            ),
            None,
        )
    } else {
        check(
            "registration.isolation",
            false,
            Status::Warning,
            format!(
                "{}: environments installed side by side would contend for it",
                shared.join("; ")
            ),
            Some(
                "give each environment its own scheme or file association in [environments.<name>.app.activation]",
            ),
        )
    }
}

fn default_requests_check(
    config: &florui_config::ResolvedConfig,
    declaration: &Declaration,
    registry: &dyn Registry,
) -> Check {
    let requests = &config.app.activation.request_default;
    if requests.is_empty() {
        return check(
            "registration.default_requests",
            false,
            Status::NotApplicable,
            "the application does not ask to be a default handler".to_string(),
            None,
        );
    }
    let registered = registration::read_record(registry, &declaration.identifier).is_some();
    let list: Vec<String> = requests
        .iter()
        .map(|request| match request {
            florui_config::DefaultRequest::UrlScheme(scheme) => format!("{scheme}://"),
            florui_config::DefaultRequest::FileExtension(extension) => format!(".{extension}"),
        })
        .collect();
    if registered {
        check(
            "registration.default_requests",
            false,
            Status::Pass,
            format!(
                "the application may ask to be the default for {}; the user decides in Settings",
                list.join(", ")
            ),
            None,
        )
    } else {
        check(
            "registration.default_requests",
            false,
            Status::Warning,
            format!(
                "the application may ask to be the default for {}, but it is not registered, so the request would be refused",
                list.join(", ")
            ),
            Some("run `florui register`"),
        )
    }
}
