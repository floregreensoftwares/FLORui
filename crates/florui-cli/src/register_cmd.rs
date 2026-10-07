//! `florui register` and `florui unregister`: the explicit steps that make
//! the operating system know the application's URL schemes and file types, and
//! forget them again. See [`crate::registration`] for what is written and the
//! rules it follows; nothing else in this CLI ever changes an association.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use florui_config::{CargoProjectFacts, EnvironmentSelection, ResolvedConfig, Target};
use florui_devtools::diagnostics::{dim_text, failure, success};
use serde_json::{Value, json};

use crate::registration::{self, Association, Declaration, Registry};

pub struct Options {
    pub package: Option<String>,
    pub environment: Option<String>,
    /// The staged build output; `target/florui-build/<package>/native` when
    /// not given.
    pub artifacts: Option<PathBuf>,
    /// Which staged executable, when the build produced more than one.
    pub exe: Option<String>,
    pub dry_run: bool,
    pub json: bool,
}

/// The project in the current directory, resolved for the selected
/// environment (`production` unless one is named).
pub(crate) fn load_project(
    package: Option<&str>,
    environment: Option<&str>,
) -> Result<(CargoProjectFacts, ResolvedConfig), String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("could not determine the current directory: {error}"))?;
    let facts =
        florui_config::resolve_cargo_project(&cwd, package).map_err(|error| error.to_string())?;
    let selection = EnvironmentSelection {
        name: environment.unwrap_or("production"),
        explicit: environment.is_some(),
    };
    let resolution = florui_config::resolve(&facts, Some(Target::Native), Some(selection))
        .map_err(|error| error.to_string())?;
    Ok((facts, resolution.config))
}

/// What the configuration asks to be registered, or why it cannot be.
pub(crate) fn declaration_from(config: &ResolvedConfig) -> Result<Declaration, String> {
    let app = &config.app;
    let identifier = app.identifier.clone().ok_or_else(|| {
        "app.identifier is not set, and a registration is named after it".to_string()
    })?;
    let shown = app.localized_identity(&app.locales.default_locale);
    Ok(Declaration {
        identifier,
        display_name: shown.name.to_string(),
        description: shown.description.map(str::to_string),
        schemes: app.activation.url_schemes.clone(),
        associations: app
            .activation
            .file_associations
            .iter()
            .map(|association| Association {
                extension: association.extension.clone(),
                identity: association.identity.clone(),
                description: association.description.clone(),
            })
            .collect(),
    })
}

/// The staged executable to register: named by the build's report, and only
/// if it is still the file the build wrote.
fn staged_executable(
    facts: &CargoProjectFacts,
    artifacts: Option<&Path>,
    wanted: Option<&str>,
) -> Result<PathBuf, String> {
    let dir = artifacts.map_or_else(
        || {
            facts
                .target_dir
                .join("florui-build")
                .join(&facts.package_name)
                .join("native")
        },
        Path::to_path_buf,
    );
    let report = crate::doctor::read_build_report(&dir.join("report.json")).map_err(|reason| {
        format!(
            "{reason} in {} (run `florui build` first, or name the directory with --artifacts)",
            dir.display()
        )
    })?;
    let executables: Vec<&Value> = report
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("built").is_some_and(|built| !built.is_null()))
        .collect();
    let name_of = |entry: &Value| {
        entry
            .get("file")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let chosen = match wanted {
        Some(name) => executables
            .iter()
            .find(|entry| name_of(entry).as_deref() == Some(name)),
        None if executables.len() == 1 => executables.first(),
        None => {
            let own = format!("{}{}", facts.package_name, std::env::consts::EXE_SUFFIX);
            executables
                .iter()
                .find(|entry| name_of(entry).as_deref() == Some(own.as_str()))
        }
    };
    let Some(entry) = chosen else {
        let names: Vec<String> = executables.iter().filter_map(|e| name_of(e)).collect();
        return Err(format!(
            "which executable to register is not clear: the build staged {names:?}; name one with --exe"
        ));
    };
    let name = name_of(entry).ok_or_else(|| "the report names no file".to_string())?;
    if Path::new(&name).components().count() != 1 {
        return Err(format!("\"{name}\" is not a plain file name"));
    }
    let path = dir.join(&name);
    let recorded = entry.get("sha256").and_then(Value::as_str).unwrap_or("");
    let actual = crate::build::hash_file(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?
        .sha256;
    if actual != recorded {
        return Err(format!(
            "{} is not the file the build wrote (its SHA-256 differs from report.json); run `florui build` again, or `florui doctor --artifacts` to see what changed",
            path.display()
        ));
    }
    let absolute = std::fs::canonicalize(&path)
        .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
    // The verbatim prefix `\\?\` is not something a launcher should see.
    let text = absolute.to_string_lossy();
    Ok(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text)))
}

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("{}", failure(&message.to_string()));
    ExitCode::from(2)
}

#[cfg(not(windows))]
pub fn register(_options: Options) -> ExitCode {
    fail("florui register is only supported on Windows so far")
}

#[cfg(not(windows))]
pub fn unregister(_package: Option<String>, _environment: Option<String>, _json: bool) -> ExitCode {
    fail("florui unregister is only supported on Windows so far")
}

#[cfg(windows)]
pub fn register(options: Options) -> ExitCode {
    let mut registry = registration::CurrentUser;
    register_in(&options, &mut registry)
}

#[cfg(windows)]
pub fn unregister(package: Option<String>, environment: Option<String>, json: bool) -> ExitCode {
    let mut registry = registration::CurrentUser;
    unregister_in(
        package.as_deref(),
        environment.as_deref(),
        json,
        &mut registry,
    )
}

pub(crate) fn register_in(options: &Options, registry: &mut dyn Registry) -> ExitCode {
    let (facts, config) =
        match load_project(options.package.as_deref(), options.environment.as_deref()) {
            Ok(project) => project,
            Err(error) => return fail(error),
        };
    let declaration = match declaration_from(&config) {
        Ok(declaration) => declaration,
        Err(error) => return fail(error),
    };
    if declaration.schemes.is_empty() && declaration.associations.is_empty() {
        let message =
            "nothing to register: [app.activation] declares no URL scheme and no file type";
        if options.json {
            println!(
                "{}",
                json!({ "outcome": "nothing_declared", "message": message })
            );
        } else {
            println!("{message}");
        }
        return ExitCode::SUCCESS;
    }
    let exe = match staged_executable(&facts, options.artifacts.as_deref(), options.exe.as_deref())
    {
        Ok(exe) => exe,
        Err(error) => return fail(error),
    };
    let plan = registration::plan(&declaration, &exe, registry);
    if !plan.conflicts.is_empty() {
        if options.json {
            println!(
                "{}",
                json!({ "outcome": "conflict", "identifier": declaration.identifier,
                        "conflicts": plan.conflicts })
            );
        } else {
            eprintln!(
                "{}",
                failure(
                    "nothing was registered: it would take over something that is not this application's"
                )
            );
            for conflict in &plan.conflicts {
                eprintln!("  {}: {}", conflict.what, conflict.reason);
            }
        }
        return ExitCode::FAILURE;
    }
    if options.dry_run {
        report(options.json, "dry_run", &declaration, &exe, &config, None);
        return ExitCode::SUCCESS;
    }
    match registration::apply(&plan, &exe, registry) {
        Ok(record) => {
            report(
                options.json,
                "registered",
                &declaration,
                &exe,
                &config,
                Some(&record),
            );
            ExitCode::SUCCESS
        }
        Err(error) => fail(error),
    }
}

pub(crate) fn unregister_in(
    package: Option<&str>,
    environment: Option<&str>,
    json: bool,
    registry: &mut dyn Registry,
) -> ExitCode {
    let (_, config) = match load_project(package, environment) {
        Ok(project) => project,
        Err(error) => return fail(error),
    };
    let Some(identifier) = config.app.identifier.clone() else {
        return fail("app.identifier is not set, and a registration is named after it");
    };
    let Some(record) = registration::read_record(registry, &identifier) else {
        let message = format!("{identifier} is not registered: nothing to remove");
        if json {
            println!(
                "{}",
                json!({ "outcome": "not_registered", "identifier": identifier })
            );
        } else {
            println!("{message}");
        }
        return ExitCode::SUCCESS;
    };
    let removal = registration::remove(&record, registry);
    if json {
        println!(
            "{}",
            json!({ "outcome": "removed", "identifier": identifier,
                    "removed": removal.removed, "left_alone": removal.left })
        );
    } else {
        println!("{} {identifier}", success("unregistered"));
        for key in &removal.removed {
            println!("{}", dim_text(&format!("  removed {key}")));
        }
        for key in &removal.left {
            println!("  left alone {key}");
        }
    }
    ExitCode::SUCCESS
}

fn report(
    as_json: bool,
    outcome: &str,
    declaration: &Declaration,
    exe: &Path,
    config: &ResolvedConfig,
    record: Option<&registration::Record>,
) {
    let requests: Vec<String> = config
        .app
        .activation
        .request_default
        .iter()
        .map(|request| match request {
            florui_config::DefaultRequest::UrlScheme(scheme) => scheme.clone(),
            florui_config::DefaultRequest::FileExtension(extension) => format!(".{extension}"),
        })
        .collect();
    if as_json {
        println!(
            "{}",
            json!({
                "outcome": outcome,
                "identifier": declaration.identifier,
                "executable": exe.display().to_string(),
                "url_schemes": declaration.schemes,
                "file_types": declaration.associations.iter()
                    .map(|a| format!(".{}", a.extension)).collect::<Vec<_>>(),
                "default_requests": requests,
                "record": record,
            })
        );
        return;
    }
    let verb = if outcome == "dry_run" {
        "would register"
    } else {
        "registered"
    };
    println!("{} {}", success(verb), declaration.identifier);
    println!("  executable: {}", exe.display());
    for scheme in &declaration.schemes {
        println!("  URL scheme: {scheme}://");
    }
    for association in &declaration.associations {
        println!("  file type:  .{}", association.extension);
    }
    println!(
        "{}",
        dim_text(
            "  no default was changed: the application is a candidate (\"Open with\", Settings > Apps > Default apps), and the user chooses"
        )
    );
    if !requests.is_empty() {
        println!(
            "{}",
            dim_text(&format!(
                "  the application may ask to be the default for: {}",
                requests.join(", ")
            ))
        );
    }
}
