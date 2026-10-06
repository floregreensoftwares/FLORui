//! The top-level entry point: turns a [`CargoProjectFacts`] plus whatever
//! `florui.config.toml` (if any) sits at its `package_root` into a
//! [`Resolution`] -- validated, fully-defaulted config, field provenance,
//! and non-fatal diagnostics.

use crate::error::{
    ActivationError, ConfigError, Diagnostic, FileAssociationError, SemanticConfigError, Severity,
    WindowSizeError, WorkspaceInheritanceError,
};
use crate::location::{LineIndex, SourceLocation};
use crate::project::{CargoProjectFacts, config_file_path, parse_package_version};
use crate::resolved::{
    ActivationConfig, AppConfig, DecorationsSetting, DevConfig, FieldProvenance,
    FileAssociationConfig, IconsConfig, LocaleConfig, LocalesConfig, Provenance, ResolvedConfig,
    Target, WebConfig, WebIconsConfig, WindowConfig, WindowPersistenceConfig,
};
use crate::schema::{
    RawActivation, RawApp, RawConfig, RawDecorations, RawEnvironmentOverlay,
    RawEnvironmentOverlayApp, RawIcons, RawLocale, RawVersion, RawWeb, RawWindow,
    SUPPORTED_SCHEMA_VERSION, SchemaVersionProbe,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use toml::Spanned;

#[derive(Debug)]
pub struct Resolution {
    pub config: ResolvedConfig,
    pub provenance: Vec<FieldProvenance>,
    pub diagnostics: Vec<Diagnostic>,
    pub environment: EnvironmentResolution,
}

/// Selects a named `[environments.<name>]` overlay. `explicit` distinguishes
/// a user-typed `--environment <name>` (undeclared ⇒ a hard error) from a
/// command's own implicit default guess like `"development"`/`"production"`
/// (undeclared ⇒ silently resolves from base configuration).
#[derive(Debug, Clone, Copy)]
pub struct EnvironmentSelection<'a> {
    pub name: &'a str,
    pub explicit: bool,
}

/// What actually happened with environment selection, for `florui doctor`
/// (and any other caller) to report without re-deriving it from `Resolution`.
#[derive(Debug)]
pub struct EnvironmentResolution {
    pub selected: Option<String>,
    /// `false` when `selected` names a declared environment whose `app`
    /// table is absent or empty, same as when nothing was selected at all
    /// -- no overlay field ever actually took effect either way.
    pub overlay_applied: bool,
    /// Every name under `[environments]`, in sorted order.
    pub declared: Vec<String>,
}

/// Resolves `florui.config.toml` (if present) at `facts.package_root`
/// against `facts`, the selected `target`, and the selected `environment`.
/// A missing config file is not an error -- it resolves to
/// Cargo/legacy/built-in defaults, so an unmigrated project keeps working
/// without notice.
pub fn resolve(
    facts: &CargoProjectFacts,
    target: Option<Target>,
    environment: Option<EnvironmentSelection<'_>>,
) -> Result<Resolution, ConfigError> {
    let config_path = config_file_path(&facts.package_root);
    let source = match std::fs::read_to_string(&config_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(ConfigError::Io {
                path: config_path,
                source: err,
            });
        }
    };

    let raw = match &source {
        Some(text) => Some(parse_config(text, &config_path)?),
        None => None,
    };
    let lines = source.as_deref().map(LineIndex::new);
    let raw_environments = raw.as_ref().and_then(|c| c.environments.as_ref());

    let mut errors: Vec<SemanticConfigError> = Vec::new();

    // Resolved before any other validation -- see
    // `resolve_external_locales_file`'s own doc for why a missing file
    // joins `errors` below but malformed TOML in an existing one fails
    // immediately instead.
    let external_locales = resolve_external_locales_file(
        raw.as_ref().and_then(|c| c.app.as_ref()),
        &config_path,
        &facts.package_root,
        lines.as_ref(),
        &mut errors,
    )?;

    let new_dev_example = raw
        .as_ref()
        .and_then(|c| c.dev.as_ref())
        .and_then(|d| d.example.as_ref());
    if let (Some(legacy), Some(new_)) = (&facts.legacy_dev_example, new_dev_example) {
        let manifest_lines = LineIndex::new(&facts.manifest_text);
        errors.push(SemanticConfigError::LegacyNewDuplicate {
            field: "dev.example",
            config_path: config_path.clone(),
            new_location: lines.as_ref().unwrap().locate_start(new_.span()),
            legacy_manifest_path: facts.manifest_path.clone(),
            legacy_location: manifest_lines.locate_start(legacy.span.clone()),
        });
    }

    let app_version = raw
        .as_ref()
        .and_then(|c| c.app.as_ref())
        .and_then(|a| a.version.as_ref());
    if let Some(version) = app_version {
        match version {
            RawVersion::Invalid { span } => {
                errors.push(SemanticConfigError::InvalidAppVersion {
                    config_path: config_path.clone(),
                    location: lines.as_ref().unwrap().locate_start(span.clone()),
                });
            }
            RawVersion::Workspace {
                requested: true, ..
            } => {
                if let Err(error) =
                    check_workspace_inheritance(&facts.manifest_path, &facts.manifest_text)
                {
                    errors.push(error);
                }
            }
            RawVersion::Workspace {
                requested: false,
                span,
            } => {
                errors.push(SemanticConfigError::InvalidAppVersion {
                    config_path: config_path.clone(),
                    location: lines.as_ref().unwrap().locate_start(span.clone()),
                });
            }
            RawVersion::Literal { .. } => {}
        }
    }

    if let Some(window) = raw.as_ref().and_then(|c| c.window.as_ref()) {
        let config_lines = lines.as_ref().unwrap();
        validate_window_size(
            window.width.as_ref(),
            "width",
            &config_path,
            config_lines,
            &mut errors,
        );
        validate_window_size(
            window.height.as_ref(),
            "height",
            &config_path,
            config_lines,
            &mut errors,
        );
        validate_window_size(
            window.min_width.as_ref(),
            "min_width",
            &config_path,
            config_lines,
            &mut errors,
        );
        validate_window_size(
            window.min_height.as_ref(),
            "min_height",
            &config_path,
            config_lines,
            &mut errors,
        );
        if let (Some(width), Some(min_width)) = (&window.width, &window.min_width)
            && min_width.get_ref() > width.get_ref()
        {
            errors.push(SemanticConfigError::InvalidWindowSize {
                config_path: config_path.clone(),
                field: "min_width",
                location: config_lines.locate_start(min_width.span()),
                reason: WindowSizeError::MinExceedsMax {
                    min_field: "min_width",
                    max_field: "width",
                },
            });
        }
        if let (Some(height), Some(min_height)) = (&window.height, &window.min_height)
            && min_height.get_ref() > height.get_ref()
        {
            errors.push(SemanticConfigError::InvalidWindowSize {
                config_path: config_path.clone(),
                field: "min_height",
                location: config_lines.locate_start(min_height.span()),
                reason: WindowSizeError::MinExceedsMax {
                    min_field: "min_height",
                    max_field: "height",
                },
            });
        }
    }

    if let Some(bundle) = raw.as_ref().and_then(|c| c.bundle.as_ref()) {
        crate::bundle::validate(bundle, &config_path, lines.as_ref().unwrap(), &mut errors);
    }

    if let Some(web) = raw.as_ref().and_then(|c| c.web.as_ref()) {
        let config_lines = lines.as_ref().unwrap();
        validate_web_base_path(web, &config_path, config_lines, &mut errors);
        validate_web_icon_formats(web, &config_path, config_lines, &mut errors);
    }

    if let Some(app) = raw.as_ref().and_then(|c| c.app.as_ref()) {
        let config_lines = lines.as_ref().unwrap();
        if let Some(activation) = app.activation.as_ref() {
            validate_activation(activation, &config_path, config_lines, &mut errors);
        }
        validate_locales(
            app,
            &config_path,
            config_lines,
            external_locales.as_ref(),
            &mut errors,
        );
    }

    if let Some(selection) = environment.as_ref().filter(|s| s.explicit) {
        let declared = raw_environments.is_some_and(|e| e.get_ref().contains_key(selection.name));
        if !declared {
            let available = raw_environments
                .map(|e| e.get_ref().keys().cloned().collect())
                .unwrap_or_default();
            let location = raw_environments.map(|e| lines.as_ref().unwrap().locate_start(e.span()));
            errors.push(SemanticConfigError::UnknownEnvironment {
                config_path: config_path.clone(),
                requested: selection.name.to_owned(),
                available,
                location,
            });
        }
    }

    if let Some(environments) = raw_environments {
        let config_lines = lines.as_ref().unwrap();
        validate_environment_identity_collisions(
            raw.as_ref().and_then(|c| c.app.as_ref()),
            environments,
            &config_path,
            config_lines,
            &mut errors,
        );
    }

    if !errors.is_empty() {
        return Err(ConfigError::Semantic(errors));
    }

    // Every field below is now safe to resolve unconditionally -- nothing
    // that would have blocked resolution survived the validation pass
    // above.
    let raw_app = raw.as_ref().and_then(|c| c.app.as_ref());
    let raw_window = raw.as_ref().and_then(|c| c.window.as_ref());
    let raw_bundle = raw.as_ref().and_then(|c| c.bundle.as_ref());

    // Only ever `Some` for a *declared* environment with a non-empty `app`
    // overlay table -- an explicit selection that fell through the
    // now-passed validation above (undeclared-but-implicit) or a declared
    // environment with no `app` table both resolve as "no overlay."
    let selected_overlay: Option<(&str, &RawEnvironmentOverlayApp)> = environment.and_then(|sel| {
        raw_environments
            .and_then(|envs| envs.get_ref().get(sel.name))
            .and_then(|overlay| overlay.app.as_ref())
            .map(|app_overlay| (sel.name, app_overlay))
    });

    let mut provenance = Vec::new();

    let name = match selected_overlay.and_then(|(_, o)| o.name.as_deref()) {
        Some(name) => {
            provenance.push(field(
                "app.name",
                Provenance::Environment(selected_overlay.unwrap().0.to_owned()),
            ));
            name.to_owned()
        }
        None => match raw_app.and_then(|a| a.name.as_deref()) {
            Some(name) => {
                provenance.push(field("app.name", Provenance::ConfigFile(None)));
                name.to_owned()
            }
            None => {
                provenance.push(field("app.name", Provenance::CargoManifest));
                facts.package_name.clone()
            }
        },
    };

    let version = match raw_app.and_then(|a| a.version.as_ref()) {
        Some(RawVersion::Literal { value, span }) => {
            provenance.push(field(
                "app.version",
                Provenance::ConfigFile(Some(lines.as_ref().unwrap().locate_start(span.clone()))),
            ));
            value.clone()
        }
        Some(RawVersion::Workspace { .. }) => {
            provenance.push(field("app.version", Provenance::CargoManifest));
            facts.package_version.clone()
        }
        Some(RawVersion::Invalid { .. }) => {
            unreachable!("invalid app.version would have errored above")
        }
        None => {
            provenance.push(field("app.version", Provenance::CargoManifest));
            facts.package_version.clone()
        }
    };

    let identifier = environment_optional_string_field(
        selected_overlay.and_then(|(_, o)| o.identifier.as_deref()),
        raw_app.and_then(|a| a.identifier.as_deref()),
        "app.identifier",
        selected_overlay.map(|(name, _)| name),
        &mut provenance,
    );
    let description = environment_optional_string_field(
        selected_overlay.and_then(|(_, o)| o.description.as_deref()),
        raw_app.and_then(|a| a.description.as_deref()),
        "app.description",
        selected_overlay.map(|(name, _)| name),
        &mut provenance,
    );

    let icons_dir = config_path.parent().unwrap_or(&facts.package_root);
    let mut diagnostics = Vec::new();
    let icons = resolve_icons(
        raw_app.and_then(|a| a.icons.as_ref()),
        selected_overlay.and_then(|(_, o)| o.icons.as_ref()),
        selected_overlay.map(|(name, _)| name),
        icons_dir,
        lines.as_ref(),
        target,
        &mut provenance,
        &mut diagnostics,
    );

    let activation = resolve_activation(
        raw_app.and_then(|a| a.activation.as_ref()),
        lines.as_ref(),
        &mut provenance,
    );
    let locales = resolve_locales(
        raw_app,
        lines.as_ref(),
        external_locales.as_ref(),
        &mut provenance,
    );

    let app = AppConfig {
        identifier,
        name,
        description,
        version,
        icons,
        activation,
        locales,
    };

    let web = resolve_web(
        raw.as_ref().and_then(|c| c.web.as_ref()),
        &app.name,
        app.description.as_deref(),
        icons_dir,
        lines.as_ref(),
        target,
        &mut provenance,
        &mut diagnostics,
    );

    let title = match raw_window.and_then(|w| w.title.as_deref()) {
        Some(title) => {
            provenance.push(field("window.title", Provenance::ConfigFile(None)));
            title.to_owned()
        }
        None => {
            provenance.push(field("window.title", Provenance::BuiltinDefault));
            app.name.clone()
        }
    };
    let width = spanned_size_field(
        raw_window.and_then(|w| w.width.as_ref()),
        "window.width",
        lines.as_ref(),
        &mut provenance,
    );
    let height = spanned_size_field(
        raw_window.and_then(|w| w.height.as_ref()),
        "window.height",
        lines.as_ref(),
        &mut provenance,
    );
    let min_width = spanned_size_field(
        raw_window.and_then(|w| w.min_width.as_ref()),
        "window.min_width",
        lines.as_ref(),
        &mut provenance,
    );
    let min_height = spanned_size_field(
        raw_window.and_then(|w| w.min_height.as_ref()),
        "window.min_height",
        lines.as_ref(),
        &mut provenance,
    );
    let decorations = match raw_window.and_then(|w| w.decorations.as_ref()) {
        Some(spanned) => {
            provenance.push(field(
                "window.decorations",
                Provenance::ConfigFile(Some(lines.as_ref().unwrap().locate_start(spanned.span()))),
            ));
            match spanned.get_ref() {
                RawDecorations::System => DecorationsSetting::System,
                RawDecorations::Custom => DecorationsSetting::Custom,
            }
        }
        None => {
            provenance.push(field("window.decorations", Provenance::BuiltinDefault));
            DecorationsSetting::System
        }
    };
    let transparent = match raw_window.and_then(|w| w.transparent) {
        Some(value) => {
            provenance.push(field("window.transparent", Provenance::ConfigFile(None)));
            value
        }
        None => {
            provenance.push(field("window.transparent", Provenance::BuiltinDefault));
            false
        }
    };

    let persistence = resolve_persistence(raw_window, &mut provenance);

    let window = WindowConfig {
        title,
        width,
        height,
        min_width,
        min_height,
        decorations,
        transparent,
        persistence,
    };

    let publisher = optional_string_field(
        raw_bundle.and_then(|b| b.publisher.as_deref()),
        "bundle.publisher",
        &mut provenance,
    );
    let bundle = crate::bundle::resolve(
        raw_bundle,
        publisher,
        facts,
        icons_dir,
        lines.as_ref(),
        &mut provenance,
        &mut diagnostics,
    );

    let example = match new_dev_example {
        Some(spanned) => {
            provenance.push(field(
                "dev.example",
                Provenance::ConfigFile(Some(lines.as_ref().unwrap().locate_start(spanned.span()))),
            ));
            Some(spanned.get_ref().clone())
        }
        None => match &facts.legacy_dev_example {
            Some(located) => {
                provenance.push(field("dev.example", Provenance::LegacyMetadata));
                Some(located.value.clone())
            }
            None => {
                provenance.push(field("dev.example", Provenance::BuiltinDefault));
                None
            }
        },
    };
    let dev = DevConfig { example };

    let environment_resolution = EnvironmentResolution {
        selected: environment.map(|sel| sel.name.to_owned()),
        overlay_applied: selected_overlay.is_some(),
        declared: raw_environments
            .map(|e| e.get_ref().keys().cloned().collect())
            .unwrap_or_default(),
    };

    Ok(Resolution {
        config: ResolvedConfig {
            app,
            window,
            bundle,
            dev,
            web,
        },
        provenance,
        diagnostics,
        environment: environment_resolution,
    })
}

fn field(name: &'static str, provenance: Provenance) -> FieldProvenance {
    FieldProvenance {
        field: name,
        provenance,
    }
}

fn optional_string_field(
    raw: Option<&str>,
    name: &'static str,
    provenance: &mut Vec<FieldProvenance>,
) -> Option<String> {
    match raw {
        Some(value) => {
            provenance.push(field(name, Provenance::ConfigFile(None)));
            Some(value.to_owned())
        }
        None => {
            provenance.push(field(name, Provenance::BuiltinDefault));
            None
        }
    }
}

/// Like `optional_string_field`, but tries the selected environment's
/// overlay value first -- `Provenance::Environment` outranks `ConfigFile`
/// for the same field, per the documented resolution order.
fn environment_optional_string_field(
    overlay: Option<&str>,
    base: Option<&str>,
    name: &'static str,
    environment_name: Option<&str>,
    provenance: &mut Vec<FieldProvenance>,
) -> Option<String> {
    match overlay {
        Some(value) => {
            provenance.push(field(
                name,
                Provenance::Environment(environment_name.unwrap().to_owned()),
            ));
            Some(value.to_owned())
        }
        None => optional_string_field(base, name, provenance),
    }
}

fn spanned_size_field(
    raw: Option<&toml::Spanned<f64>>,
    name: &'static str,
    lines: Option<&LineIndex<'_>>,
    provenance: &mut Vec<FieldProvenance>,
) -> Option<f64> {
    match raw {
        Some(spanned) => {
            provenance.push(field(
                name,
                Provenance::ConfigFile(Some(lines.unwrap().locate_start(spanned.span()))),
            ));
            Some(*spanned.get_ref())
        }
        None => {
            provenance.push(field(name, Provenance::BuiltinDefault));
            None
        }
    }
}

fn validate_window_size(
    raw: Option<&toml::Spanned<f64>>,
    field_name: &'static str,
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let Some(spanned) = raw else { return };
    let value = *spanned.get_ref();
    let reason = if !value.is_finite() {
        Some(WindowSizeError::NotFinite)
    } else if value <= 0.0 {
        Some(WindowSizeError::NotPositive)
    } else {
        None
    };
    if let Some(reason) = reason {
        errors.push(SemanticConfigError::InvalidWindowSize {
            config_path: config_path.to_owned(),
            field: field_name,
            location: lines.locate_start(spanned.span()),
            reason,
        });
    }
}

/// `overlay` is the selected environment's `app.icons` table, if any --
/// tried before `raw` field-by-field (an environment can override just
/// `source` while inheriting `windows`/`macos`/`linux` from base), matching
/// "overlay tables merge by field."
#[allow(clippy::too_many_arguments)]
fn resolve_icons(
    raw: Option<&RawIcons>,
    overlay: Option<&RawIcons>,
    environment_name: Option<&str>,
    icons_dir: &Path,
    lines: Option<&LineIndex<'_>>,
    target: Option<Target>,
    provenance: &mut Vec<FieldProvenance>,
    diagnostics: &mut Vec<Diagnostic>,
) -> IconsConfig {
    let checks_assets = matches!(target, Some(Target::Native));
    let mut resolve_one = |overlay_raw: Option<&toml::Spanned<String>>,
                           base_raw: Option<&toml::Spanned<String>>,
                           name: &'static str,
                           code: &'static str| {
        let (spanned, from_environment) = match overlay_raw {
            Some(spanned) => (Some(spanned), true),
            None => (base_raw, false),
        };
        match spanned {
            Some(spanned) => {
                let location = lines.unwrap().locate_start(spanned.span());
                let provenance_kind = if from_environment {
                    Provenance::Environment(environment_name.unwrap().to_owned())
                } else {
                    Provenance::ConfigFile(Some(location))
                };
                provenance.push(field(name, provenance_kind));
                let path = icons_dir.join(spanned.get_ref());
                if checks_assets && !path.exists() {
                    diagnostics.push(Diagnostic {
                        severity: Severity::Warning,
                        code,
                        message: format!(
                            "{name} points to {}, which does not exist",
                            path.display()
                        ),
                        location: Some(location),
                        field: name,
                    });
                }
                Some(path)
            }
            None => {
                provenance.push(field(name, Provenance::BuiltinDefault));
                None
            }
        }
    };

    IconsConfig {
        source: resolve_one(
            overlay.and_then(|i| i.source.as_ref()),
            raw.and_then(|i| i.source.as_ref()),
            "app.icons.source",
            "config.icon_asset_present.source",
        ),
        windows: resolve_one(
            overlay.and_then(|i| i.windows.as_ref()),
            raw.and_then(|i| i.windows.as_ref()),
            "app.icons.windows",
            "config.icon_asset_present.windows",
        ),
        macos: resolve_one(
            overlay.and_then(|i| i.macos.as_ref()),
            raw.and_then(|i| i.macos.as_ref()),
            "app.icons.macos",
            "config.icon_asset_present.macos",
        ),
        linux: resolve_one(
            overlay.and_then(|i| i.linux.as_ref()),
            raw.and_then(|i| i.linux.as_ref()),
            "app.icons.linux",
            "config.icon_asset_present.linux",
        ),
    }
}

fn validate_web_base_path(
    web: &RawWeb,
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let Some(base_path) = web.base_path.as_ref() else {
        return;
    };
    if !base_path.get_ref().starts_with('/') {
        errors.push(SemanticConfigError::InvalidWebBasePath {
            config_path: config_path.to_owned(),
            location: lines.locate_start(base_path.span()),
        });
    }
}

fn validate_web_icon_formats(
    web: &RawWeb,
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let Some(icons) = web.icons.as_ref() else {
        return;
    };
    check_web_icon_extension(
        icons.favicon.as_ref(),
        "favicon",
        &["svg", "png", "ico"],
        config_path,
        lines,
        errors,
    );
    check_web_icon_extension(
        icons.apple_touch_icon.as_ref(),
        "apple_touch_icon",
        &["png"],
        config_path,
        lines,
        errors,
    );
}

fn check_web_icon_extension(
    raw: Option<&Spanned<String>>,
    field_name: &'static str,
    allowed: &'static [&'static str],
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let Some(spanned) = raw else {
        return;
    };
    let lower = spanned.get_ref().to_ascii_lowercase();
    let ok = allowed
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")));
    if !ok {
        errors.push(SemanticConfigError::InvalidWebIconFormat {
            config_path: config_path.to_owned(),
            field: field_name,
            location: lines.locate_start(spanned.span()),
            allowed,
        });
    }
}

/// `web.title`/`web.description` fall back to `app.name`/`app.description`
/// (never native `window.title`) with `Provenance::BuiltinDefault`, the same
/// treatment `window.title`'s own fallback to `app.name` already gets above
/// -- a field inheriting another resolved field's value isn't a distinct
/// provenance category in this model. `web.icons` never inherits
/// `app.icons`: an absent field simply resolves to `None`.
#[allow(clippy::too_many_arguments)]
fn resolve_web(
    raw: Option<&RawWeb>,
    app_name: &str,
    app_description: Option<&str>,
    icons_dir: &Path,
    lines: Option<&LineIndex<'_>>,
    target: Option<Target>,
    provenance: &mut Vec<FieldProvenance>,
    diagnostics: &mut Vec<Diagnostic>,
) -> WebConfig {
    let title = match raw.and_then(|w| w.title.as_deref()) {
        Some(title) => {
            provenance.push(field("web.title", Provenance::ConfigFile(None)));
            title.to_owned()
        }
        None => {
            provenance.push(field("web.title", Provenance::BuiltinDefault));
            app_name.to_owned()
        }
    };
    let description = match raw.and_then(|w| w.description.as_deref()) {
        Some(description) => {
            provenance.push(field("web.description", Provenance::ConfigFile(None)));
            Some(description.to_owned())
        }
        None => {
            provenance.push(field("web.description", Provenance::BuiltinDefault));
            app_description.map(str::to_owned)
        }
    };
    let base_path = match raw.and_then(|w| w.base_path.as_ref()) {
        Some(spanned) => {
            provenance.push(field(
                "web.base_path",
                Provenance::ConfigFile(Some(lines.unwrap().locate_start(spanned.span()))),
            ));
            spanned.get_ref().clone()
        }
        None => {
            provenance.push(field("web.base_path", Provenance::BuiltinDefault));
            "/".to_owned()
        }
    };

    let checks_assets = matches!(target, Some(Target::Web));
    let raw_icons = raw.and_then(|w| w.icons.as_ref());
    let mut resolve_one = |raw: Option<&toml::Spanned<String>>,
                           name: &'static str,
                           code: &'static str| match raw {
        Some(spanned) => {
            let location = lines.unwrap().locate_start(spanned.span());
            provenance.push(field(name, Provenance::ConfigFile(Some(location))));
            let path = icons_dir.join(spanned.get_ref());
            if checks_assets && !path.exists() {
                diagnostics.push(Diagnostic {
                    severity: Severity::Warning,
                    code,
                    message: format!("{name} points to {}, which does not exist", path.display()),
                    location: Some(location),
                    field: name,
                });
            }
            Some(path)
        }
        None => {
            provenance.push(field(name, Provenance::BuiltinDefault));
            None
        }
    };
    let icons = WebIconsConfig {
        favicon: resolve_one(
            raw_icons.and_then(|i| i.favicon.as_ref()),
            "web.icons.favicon",
            "config.web_icon_asset_present.favicon",
        ),
        apple_touch_icon: resolve_one(
            raw_icons.and_then(|i| i.apple_touch_icon.as_ref()),
            "web.icons.apple_touch_icon",
            "config.web_icon_asset_present.apple_touch_icon",
        ),
    };

    WebConfig {
        title,
        description,
        base_path,
        icons,
    }
}

fn validate_activation(
    activation: &RawActivation,
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    if let Some(schemes) = activation.url_schemes.as_ref() {
        let location = lines.locate_start(schemes.span());
        let mut seen = BTreeSet::new();
        for scheme in schemes.get_ref() {
            if scheme.is_empty() {
                errors.push(SemanticConfigError::InvalidActivation {
                    config_path: config_path.to_owned(),
                    location,
                    reason: ActivationError::EmptyUrlScheme,
                });
            } else if !is_valid_url_scheme(scheme) {
                errors.push(SemanticConfigError::InvalidActivation {
                    config_path: config_path.to_owned(),
                    location,
                    reason: ActivationError::InvalidUrlSchemeCharacters {
                        scheme: scheme.clone(),
                    },
                });
            } else if !seen.insert(scheme.to_ascii_lowercase()) {
                errors.push(SemanticConfigError::InvalidActivation {
                    config_path: config_path.to_owned(),
                    location,
                    reason: ActivationError::DuplicateUrlScheme {
                        scheme: scheme.clone(),
                    },
                });
            }
        }
    }

    let Some(associations) = activation.file_associations.as_ref() else {
        return;
    };
    let mut seen_extensions = BTreeSet::new();
    let mut seen_identities = BTreeSet::new();
    for association in associations {
        let extension = association.extension.get_ref();
        let extension_location = lines.locate_start(association.extension.span());
        if extension.is_empty() {
            errors.push(SemanticConfigError::InvalidFileAssociation {
                config_path: config_path.to_owned(),
                location: extension_location,
                reason: FileAssociationError::EmptyExtension,
            });
        } else if !seen_extensions.insert(extension.to_ascii_lowercase()) {
            errors.push(SemanticConfigError::InvalidFileAssociation {
                config_path: config_path.to_owned(),
                location: extension_location,
                reason: FileAssociationError::DuplicateExtension {
                    extension: extension.clone(),
                },
            });
        }

        let identity = association.identity.get_ref();
        let identity_location = lines.locate_start(association.identity.span());
        if identity.is_empty() {
            errors.push(SemanticConfigError::InvalidFileAssociation {
                config_path: config_path.to_owned(),
                location: identity_location,
                reason: FileAssociationError::EmptyIdentity,
            });
        } else if !seen_identities.insert(identity.clone()) {
            errors.push(SemanticConfigError::InvalidFileAssociation {
                config_path: config_path.to_owned(),
                location: identity_location,
                reason: FileAssociationError::DuplicateIdentity {
                    identity: identity.clone(),
                },
            });
        }
    }
}

/// A URI scheme per RFC 3986: a letter, then letters/digits/`+`/`-`/`.`.
fn is_valid_url_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// No OS registration, no single-instance IPC -- see
/// `crate::schema::RawActivation`'s own doc comment.
fn resolve_activation(
    raw: Option<&RawActivation>,
    lines: Option<&LineIndex<'_>>,
    provenance: &mut Vec<FieldProvenance>,
) -> ActivationConfig {
    let single_instance = match raw.and_then(|a| a.single_instance) {
        Some(value) => {
            provenance.push(field(
                "app.activation.single_instance",
                Provenance::ConfigFile(None),
            ));
            value
        }
        None => {
            provenance.push(field(
                "app.activation.single_instance",
                Provenance::BuiltinDefault,
            ));
            false
        }
    };
    let url_schemes = match raw.and_then(|a| a.url_schemes.as_ref()) {
        Some(spanned) => {
            provenance.push(field(
                "app.activation.url_schemes",
                Provenance::ConfigFile(Some(lines.unwrap().locate_start(spanned.span()))),
            ));
            spanned.get_ref().clone()
        }
        None => {
            provenance.push(field(
                "app.activation.url_schemes",
                Provenance::BuiltinDefault,
            ));
            Vec::new()
        }
    };
    let file_associations = match raw.and_then(|a| a.file_associations.as_ref()) {
        Some(list) => {
            provenance.push(field(
                "app.activation.file_associations",
                Provenance::ConfigFile(None),
            ));
            list.iter()
                .map(|association| FileAssociationConfig {
                    extension: association.extension.get_ref().clone(),
                    mime_type: association.mime_type.clone(),
                    description: association.description.clone(),
                    identity: association.identity.get_ref().clone(),
                })
                .collect()
        }
        None => {
            provenance.push(field(
                "app.activation.file_associations",
                Provenance::BuiltinDefault,
            ));
            Vec::new()
        }
    };

    ActivationConfig {
        single_instance,
        url_schemes,
        file_associations,
    }
}

/// No bounds save/restore, no monitor revalidation -- see
/// `crate::schema::RawWindowPersistence`'s own doc comment. `key` defaults
/// to `"main"` (not `String::default()`'s empty string) so an
/// enabled-without-a-key persistence declaration still has a usable key.
fn resolve_persistence(
    raw_window: Option<&RawWindow>,
    provenance: &mut Vec<FieldProvenance>,
) -> WindowPersistenceConfig {
    let raw_persistence = raw_window.and_then(|w| w.persistence.as_ref());
    let enabled = match raw_persistence.and_then(|p| p.enabled) {
        Some(value) => {
            provenance.push(field(
                "window.persistence.enabled",
                Provenance::ConfigFile(None),
            ));
            value
        }
        None => {
            provenance.push(field(
                "window.persistence.enabled",
                Provenance::BuiltinDefault,
            ));
            false
        }
    };
    let key = match raw_persistence.and_then(|p| p.key.as_deref()) {
        Some(key) => {
            provenance.push(field(
                "window.persistence.key",
                Provenance::ConfigFile(None),
            ));
            key.to_owned()
        }
        None => {
            provenance.push(field("window.persistence.key", Provenance::BuiltinDefault));
            "main".to_owned()
        }
    };
    WindowPersistenceConfig { enabled, key }
}

/// The conventional filename an external locales file is auto-discovered
/// under (sitting next to `florui.config.toml`) when `app.locales_file`
/// doesn't explicitly name a different one.
const LOCALES_FILE_NAME: &str = "florui.locales.toml";

/// An external locales file's own path plus its parsed, not-yet-validated
/// top-level table.
type ExternalLocales = (PathBuf, BTreeMap<String, RawLocale>);

/// A locale-set validated against exactly one source: either the inline
/// `[app.locales]` table, or an external file's own top-level table --
/// never both, and never neither once at least one is present. Carries
/// whichever location every entry's own errors should cite, and (for
/// `External`) the file's own path, since that's what actually declared
/// the offending tag.
enum LocaleSource<'a> {
    Inline {
        entries: &'a BTreeMap<String, RawLocale>,
        location: SourceLocation,
    },
    External {
        entries: &'a BTreeMap<String, RawLocale>,
        path: &'a Path,
    },
    None,
}

/// Determines whether `app.locales_file` (explicit) or the conventional
/// [`LOCALES_FILE_NAME`] (auto-discovered, only when `locales_file` isn't
/// set) supplies this app's locale entries instead of the inline
/// `[app.locales]` table -- and if so, reads and parses it immediately.
///
/// A referenced-but-missing file is an ordinary semantic mistake (pushed
/// into `errors`, alongside everything else this config might also get
/// wrong); malformed TOML in a file that does exist is as fundamental as
/// the main config's own malformed TOML and fails immediately, before any
/// other check even runs -- `Ok(None)` from the `NotFound` branch below is
/// never actually used to build a real `Resolution`, since `resolve`
/// bails as soon as `errors` is non-empty regardless.
///
/// `Ok(None)` with no error at all means neither `locales_file` nor the
/// conventional filename applies: `[app.locales]` (if any) is the only
/// source, exactly today's existing behavior.
fn resolve_external_locales_file(
    raw_app: Option<&RawApp>,
    config_path: &Path,
    package_root: &Path,
    lines: Option<&LineIndex<'_>>,
    errors: &mut Vec<SemanticConfigError>,
) -> Result<Option<ExternalLocales>, ConfigError> {
    let explicit = raw_app.and_then(|a| a.locales_file.as_ref());
    let path = match explicit {
        Some(spanned) => package_root.join(spanned.get_ref()),
        None => {
            let conventional = package_root.join(LOCALES_FILE_NAME);
            if !conventional.is_file() {
                return Ok(None);
            }
            conventional
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Only reachable for an *explicit* reference -- the
            // auto-discovery branch above already checked existence.
            let spanned = explicit.expect("only an explicit reference reaches this branch");
            errors.push(SemanticConfigError::LocalesFileNotFound {
                config_path: config_path.to_owned(),
                referenced_path: path,
                location: lines.unwrap().locate_start(spanned.span()),
            });
            return Ok(None);
        }
        Err(err) => return Err(ConfigError::Io { path, source: err }),
    };
    let file_lines = LineIndex::new(&text);
    let entries: BTreeMap<String, RawLocale> =
        toml::from_str(&text).map_err(|err| toml_error(&path, &file_lines, err))?;
    Ok(Some((path, entries)))
}

fn validate_locales(
    app: &RawApp,
    config_path: &Path,
    lines: &LineIndex<'_>,
    external: Option<&ExternalLocales>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let inline = app
        .locales
        .as_ref()
        .filter(|spanned| !spanned.get_ref().is_empty());
    let source = match (inline, external) {
        (Some(inline), Some((external_path, _))) => {
            errors.push(SemanticConfigError::ConflictingLocalesSource {
                config_path: config_path.to_owned(),
                external_path: external_path.clone(),
                location: lines.locate_start(inline.span()),
            });
            return;
        }
        (Some(inline), None) => LocaleSource::Inline {
            entries: inline.get_ref(),
            location: lines.locate_start(inline.span()),
        },
        (None, Some((path, entries))) => LocaleSource::External { entries, path },
        (None, None) => LocaleSource::None,
    };
    let (entries, entries_path, entries_location) = match &source {
        LocaleSource::Inline { entries, location } => (*entries, config_path, *location),
        // A generic "start of file" location: the inline table only ever
        // reports table-level precision too (see `LocaleSource::Inline`'s
        // own construction above), so this loses nothing an external file
        // would otherwise have offered.
        LocaleSource::External { entries, path } => {
            (*entries, *path, SourceLocation { line: 1, column: 1 })
        }
        LocaleSource::None => return,
    };
    for tag in entries.keys() {
        if !is_valid_locale_tag(tag) {
            errors.push(SemanticConfigError::InvalidLocaleTag {
                config_path: entries_path.to_owned(),
                tag: tag.clone(),
                location: entries_location,
            });
        }
    }

    let Some(default_locale) = app.default_locale.as_ref() else {
        return;
    };
    // Only validated against a declared, non-empty locale set -- an
    // undeclared default_locale is just an inert string otherwise.
    if entries.is_empty() {
        return;
    }
    if !entries.contains_key(default_locale.get_ref()) {
        errors.push(SemanticConfigError::InvalidDefaultLocale {
            config_path: config_path.to_owned(),
            requested: default_locale.get_ref().clone(),
            available: entries.keys().cloned().collect(),
            location: lines.locate_start(default_locale.span()),
        });
    }
}

/// A light structural heuristic, not full BCP-47/CLDR validation: subtags
/// separated by `-`, the primary subtag 2-8 ASCII letters, every later
/// subtag 1-8 ASCII alphanumerics.
fn is_valid_locale_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    let valid_primary =
        (2..=8).contains(&primary.len()) && primary.chars().all(|c| c.is_ascii_alphabetic());
    if !valid_primary {
        return false;
    }
    parts.all(|part| {
        (1..=8).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// No runtime locale switching -- `default_locale` only scopes the
/// identity locale fallback contract; nothing here consumes a "current
/// locale." `external`'s entries (already validated by [`validate_locales`]
/// against exactly the same rules as the inline table) take priority over
/// `[app.locales]` -- `validate_locales` already rejected declaring both,
/// so at most one is ever actually populated here.
fn resolve_locales(
    raw_app: Option<&RawApp>,
    lines: Option<&LineIndex<'_>>,
    external: Option<&ExternalLocales>,
    provenance: &mut Vec<FieldProvenance>,
) -> LocalesConfig {
    let to_locales_config =
        |entries: &BTreeMap<String, RawLocale>| -> BTreeMap<String, LocaleConfig> {
            entries
                .iter()
                .map(|(tag, locale)| {
                    (
                        tag.clone(),
                        LocaleConfig {
                            name: locale.name.clone(),
                            description: locale.description.clone(),
                        },
                    )
                })
                .collect()
        };
    let locales = match (external, raw_app.and_then(|a| a.locales.as_ref())) {
        (Some((_path, entries)), _) => {
            provenance.push(field(
                "app.locales",
                Provenance::ConfigFile(Some(SourceLocation { line: 1, column: 1 })),
            ));
            to_locales_config(entries)
        }
        (None, Some(spanned)) => {
            provenance.push(field(
                "app.locales",
                Provenance::ConfigFile(Some(lines.unwrap().locate_start(spanned.span()))),
            ));
            to_locales_config(spanned.get_ref())
        }
        (None, None) => {
            provenance.push(field("app.locales", Provenance::BuiltinDefault));
            BTreeMap::new()
        }
    };
    let default_locale = match raw_app.and_then(|a| a.default_locale.as_ref()) {
        Some(spanned) => {
            provenance.push(field(
                "app.default_locale",
                Provenance::ConfigFile(Some(lines.unwrap().locate_start(spanned.span()))),
            ));
            spanned.get_ref().clone()
        }
        None => {
            provenance.push(field("app.default_locale", Provenance::BuiltinDefault));
            "en".to_owned()
        }
    };
    LocalesConfig {
        default_locale,
        locales,
    }
}

/// Resolves every declared environment's effective `app.identifier`
/// simultaneously (base configuration plus each overlay's own
/// field-merge rule: overlay value, or the base value) and flags any
/// value shared by more than one -- independent of which environment is
/// actually selected at this invocation, since the mistake exists in the
/// file regardless. `None` identifiers never collide with each other:
/// this crate never invents an identifier, so there's no identity to share.
fn validate_environment_identity_collisions(
    raw_app: Option<&RawApp>,
    environments: &Spanned<BTreeMap<String, RawEnvironmentOverlay>>,
    config_path: &Path,
    lines: &LineIndex<'_>,
    errors: &mut Vec<SemanticConfigError>,
) {
    let location = lines.locate_start(environments.span());
    let base_identifier = raw_app.and_then(|a| a.identifier.as_deref());

    let mut labels_by_identifier: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    if let Some(identifier) = base_identifier {
        labels_by_identifier
            .entry(identifier)
            .or_default()
            .push("(base configuration)".to_owned());
    }
    for (name, overlay) in environments.get_ref() {
        let effective = overlay
            .app
            .as_ref()
            .and_then(|a| a.identifier.as_deref())
            .or(base_identifier);
        if let Some(identifier) = effective {
            labels_by_identifier
                .entry(identifier)
                .or_default()
                .push(name.clone());
        }
    }

    for (identifier, labels) in labels_by_identifier {
        if labels.len() > 1 {
            errors.push(SemanticConfigError::DuplicateEnvironmentIdentifier {
                config_path: config_path.to_owned(),
                identifier: identifier.to_owned(),
                environments: labels,
                location,
            });
        }
    }
}

/// Two-step parse: peeks `schema_version` first, so a genuinely newer or
/// older schema shape reports a clean "unsupported schema_version" error
/// rather than "unknown field" noise for every field this build doesn't
/// recognize yet. Two `toml::from_str` calls over the same short string is
/// simpler and more honest than trying to reuse one permissive pass.
fn parse_config(text: &str, config_path: &Path) -> Result<RawConfig, ConfigError> {
    let lines = LineIndex::new(text);
    let probe: SchemaVersionProbe =
        toml::from_str(text).map_err(|err| toml_error(config_path, &lines, err))?;
    let version = *probe.schema_version.get_ref();
    if version != SUPPORTED_SCHEMA_VERSION {
        return Err(ConfigError::Semantic(vec![
            SemanticConfigError::UnsupportedSchemaVersion {
                config_path: config_path.to_owned(),
                found: version,
                supported: SUPPORTED_SCHEMA_VERSION,
                location: lines.locate_start(probe.schema_version.span()),
            },
        ]));
    }
    toml::from_str::<RawConfig>(text).map_err(|err| toml_error(config_path, &lines, err))
}

fn toml_error(config_path: &Path, lines: &LineIndex<'_>, err: toml::de::Error) -> ConfigError {
    let location = err.span().map(|span| lines.locate_start(span));
    ConfigError::Toml {
        path: config_path.to_owned(),
        message: err.message().to_owned(),
        location,
    }
}

/// Verifies `manifest_path`'s own `[package]` table actually declares
/// `version.workspace = true` -- `cargo metadata` always reports an
/// already-flattened, concrete version regardless of how it was declared,
/// so this can't be checked from that JSON alone.
pub(crate) fn check_workspace_inheritance(
    manifest_path: &Path,
    manifest_text: &str,
) -> Result<(), SemanticConfigError> {
    let lines = LineIndex::new(manifest_text);
    let not_inherited = |found: String, span: std::ops::Range<usize>| {
        SemanticConfigError::WorkspaceVersionNotInherited {
            manifest_path: manifest_path.to_owned(),
            location: lines.locate_start(span),
            reason: WorkspaceInheritanceError::NotInherited { found },
        }
    };
    match parse_package_version(manifest_text) {
        Ok(Some(RawVersion::Workspace {
            requested: true, ..
        })) => Ok(()),
        Ok(Some(RawVersion::Workspace {
            requested: false,
            span,
        })) => Err(not_inherited("workspace = false".to_owned(), span)),
        Ok(Some(RawVersion::Literal { value, span })) => {
            Err(not_inherited(format!("version = \"{value}\""), span))
        }
        Ok(Some(RawVersion::Invalid { span })) => Err(not_inherited(
            "an unrecognized version value".to_owned(),
            span,
        )),
        Ok(None) | Err(_) => Err(SemanticConfigError::WorkspaceVersionNotInherited {
            manifest_path: manifest_path.to_owned(),
            location: lines.locate(0),
            reason: WorkspaceInheritanceError::MissingVersionKey,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::CargoProjectFacts;

    fn facts(package_root: &Path, manifest_text: &str) -> CargoProjectFacts {
        CargoProjectFacts {
            workspace_root: package_root.to_owned(),
            package_root: package_root.to_owned(),
            manifest_path: package_root.join("Cargo.toml"),
            manifest_text: manifest_text.to_owned(),
            target_dir: package_root.join("target"),
            package_name: "app".to_owned(),
            package_version: "0.1.0".to_owned(),
            example_targets: Vec::new(),
            legacy_dev_example: None,
            license: None,
            license_file: None,
            homepage: None,
        }
    }

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn missing_config_file_falls_back_to_defaults_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.app.name, "app");
        assert_eq!(resolution.config.app.version, "0.1.0");
        assert_eq!(resolution.config.window.title, "app");
        assert_eq!(
            resolution.config.window.decorations,
            DecorationsSetting::System
        );
        assert!(!resolution.config.window.transparent);
    }

    #[test]
    fn unsupported_schema_version_is_rejected_with_a_location() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 2\n");
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert_eq!(errors.len(), 1);
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::UnsupportedSchemaVersion {
                        found: 2,
                        supported: 1,
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_key_is_a_hard_toml_error_with_a_location() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\nbogus = true\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Toml {
                location: Some(_), ..
            } => {}
            other => panic!("expected Toml with a location, got {other:?}"),
        }
    }

    #[test]
    fn app_name_defaults_to_the_cargo_package_name() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.app.name, "app");
        assert!(
            resolution
                .provenance
                .iter()
                .any(|p| p.field == "app.name" && p.provenance == Provenance::CargoManifest)
        );
    }

    #[test]
    fn app_name_from_config_takes_precedence_with_config_file_provenance() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nname = \"Garden\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.app.name, "Garden");
        assert!(
            resolution
                .provenance
                .iter()
                .any(|p| p.field == "app.name" && p.provenance == Provenance::ConfigFile(None))
        );
    }

    #[test]
    fn app_version_literal_string_is_used_as_is() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nversion = \"9.9.9\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.app.version, "9.9.9");
    }

    #[test]
    fn app_version_wrong_shape_is_a_semantic_error_with_a_location() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nversion = 123\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidAppVersion { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn app_version_workspace_true_resolves_to_the_cargo_version_when_manifest_inherits() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nversion = { workspace = true }\n",
        );
        let manifest = "[package]\nname = \"app\"\nversion.workspace = true\n";
        let resolution = resolve(&facts(dir.path(), manifest), None, None).unwrap();
        assert_eq!(resolution.config.app.version, "0.1.0");
    }

    #[test]
    fn app_version_workspace_true_errors_when_the_manifest_uses_a_literal_version() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nversion = { workspace = true }\n",
        );
        // The same shape examples/florui-example-app/Cargo.toml has today.
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
        let err = resolve(&facts(dir.path(), manifest), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::WorkspaceVersionNotInherited { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn window_size_rejects_non_finite() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[window]\nwidth = nan\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidWindowSize {
                        reason: WindowSizeError::NotFinite,
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn window_size_rejects_non_positive() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[window]\nwidth = 0\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidWindowSize {
                        reason: WindowSizeError::NotPositive,
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn window_size_rejects_min_exceeding_max() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[window]\nwidth = 400\nmin_width = 800\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidWindowSize {
                        reason: WindowSizeError::MinExceedsMax { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn legacy_and_new_dev_example_both_declared_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[dev]\nexample = \"counter\"\n",
        );
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[package.metadata.florui.dev]\nexample = \"counter\"\n";
        let mut f = facts(dir.path(), manifest);
        let located = crate::location::LocatedValue {
            value: "counter".to_owned(),
            span: 0..7,
        };
        f.legacy_dev_example = Some(located);
        let err = resolve(&f, None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::LegacyNewDuplicate { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn legacy_only_dev_example_resolves_with_legacy_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = facts(dir.path(), "");
        f.legacy_dev_example = Some(crate::location::LocatedValue {
            value: "counter".to_owned(),
            span: 0..7,
        });
        let resolution = resolve(&f, None, None).unwrap();
        assert_eq!(resolution.config.dev.example.as_deref(), Some("counter"));
        assert!(
            resolution
                .provenance
                .iter()
                .any(|p| p.field == "dev.example" && p.provenance == Provenance::LegacyMetadata)
        );
    }

    #[test]
    fn new_only_dev_example_resolves_with_config_file_provenance() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[dev]\nexample = \"counter\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.dev.example.as_deref(), Some("counter"));
    }

    #[test]
    fn icon_asset_missing_is_a_warning_diagnostic_not_an_error_under_native() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.icons]\nsource = \"missing.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), Some(Target::Native), None).unwrap();
        assert_eq!(resolution.diagnostics.len(), 1);
        assert_eq!(
            resolution.diagnostics[0].code,
            "config.icon_asset_present.source"
        );
    }

    #[test]
    fn icon_asset_check_is_skipped_under_target_web() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.icons]\nsource = \"missing.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), Some(Target::Web), None).unwrap();
        assert!(resolution.diagnostics.is_empty());
    }

    #[test]
    fn icon_asset_check_is_skipped_when_target_is_none() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.icons]\nsource = \"missing.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert!(resolution.diagnostics.is_empty());
    }

    #[test]
    fn icon_asset_present_produces_no_diagnostic() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "icon.svg", "<svg/>");
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.icons]\nsource = \"icon.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), Some(Target::Native), None).unwrap();
        assert!(resolution.diagnostics.is_empty());
        assert_eq!(
            resolution.config.app.icons.source,
            Some(dir.path().join("icon.svg"))
        );
    }

    #[test]
    fn check_workspace_inheritance_confirms_a_real_workspace_true_manifest() {
        let manifest = "[package]\nname = \"app\"\nversion.workspace = true\n";
        assert!(check_workspace_inheritance(Path::new("Cargo.toml"), manifest).is_ok());
    }

    #[test]
    fn check_workspace_inheritance_rejects_a_literal_version_string() {
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
        let err = check_workspace_inheritance(Path::new("Cargo.toml"), manifest).unwrap_err();
        assert!(matches!(
            err,
            SemanticConfigError::WorkspaceVersionNotInherited {
                reason: WorkspaceInheritanceError::NotInherited { .. },
                ..
            }
        ));
    }

    #[test]
    fn web_title_falls_back_to_app_name_not_window_title() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nname = \"Garden\"\n[window]\ntitle = \"A Different Title\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.web.title, "Garden");
        assert_eq!(resolution.config.window.title, "A Different Title");
    }

    #[test]
    fn web_description_falls_back_to_app_description() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndescription = \"A workspace for your ideas\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(
            resolution.config.web.description.as_deref(),
            Some("A workspace for your ideas")
        );
    }

    #[test]
    fn web_base_path_defaults_to_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.web.base_path, "/");
    }

    fn bundle_provenance(resolution: &Resolution, name: &str) -> Provenance {
        resolution
            .provenance
            .iter()
            .find(|p| p.field == name)
            .unwrap_or_else(|| panic!("no provenance for {name}"))
            .provenance
            .clone()
    }

    fn bundle_errors(dir: &Path, bundle: &str) -> Vec<String> {
        write(
            dir,
            "florui.config.toml",
            &format!("schema_version = 1\n[bundle]\n{bundle}"),
        );
        match resolve(&facts(dir, ""), None, None).unwrap_err() {
            ConfigError::Semantic(errors) => errors.iter().map(ToString::to_string).collect(),
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn every_bundle_field_resolves_with_its_location() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "LICENSE-MIT", "MIT");
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[bundle]\npublisher = \"Floregreen\"\ncopyright = \"Copyright 2026 Garden\"\nlicense = \"MIT OR Apache-2.0\"\nlicense_file = \"LICENSE-MIT\"\ncategory = \"productivity\"\nhomepage = \"https://example.com/garden\"\n",
        );

        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();

        let bundle = &resolution.config.bundle;
        assert_eq!(bundle.copyright.as_deref(), Some("Copyright 2026 Garden"));
        assert_eq!(bundle.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert_eq!(bundle.license_file, Some(dir.path().join("LICENSE-MIT")));
        assert_eq!(bundle.category.as_deref(), Some("productivity"));
        assert_eq!(
            bundle.homepage.as_deref(),
            Some("https://example.com/garden")
        );
        match bundle_provenance(&resolution, "bundle.license") {
            Provenance::ConfigFile(Some(location)) => assert_eq!(location.line, 5),
            other => panic!("expected a config location, got {other:?}"),
        }
        assert!(
            resolution.diagnostics.is_empty(),
            "{:?}",
            resolution.diagnostics
        );
    }

    #[test]
    fn a_silent_configuration_inherits_cargo_license_file_and_homepage() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "NOTICE", "notice");
        let mut package = facts(dir.path(), "");
        package.license = Some("Apache-2.0".to_owned());
        package.license_file = Some(PathBuf::from("NOTICE"));
        package.homepage = Some("https://cargo.example".to_owned());

        let resolution = resolve(&package, None, None).unwrap();

        let bundle = &resolution.config.bundle;
        assert_eq!(bundle.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(bundle.license_file, Some(dir.path().join("NOTICE")));
        assert_eq!(bundle.homepage.as_deref(), Some("https://cargo.example"));
        for name in ["bundle.license", "bundle.license_file", "bundle.homepage"] {
            assert_eq!(
                bundle_provenance(&resolution, name),
                Provenance::CargoManifest
            );
        }
        assert_eq!(
            bundle_provenance(&resolution, "bundle.copyright"),
            Provenance::BuiltinDefault
        );
    }

    #[test]
    fn a_configured_value_wins_over_cargo_and_nothing_else_is_inherited() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[bundle]\nlicense = \"MIT\"\nhomepage = \"https://config.example\"\n",
        );
        let mut package = facts(dir.path(), "");
        package.license = Some("Apache-2.0".to_owned());
        package.homepage = Some("https://cargo.example".to_owned());

        let resolution = resolve(&package, None, None).unwrap();

        let bundle = &resolution.config.bundle;
        assert_eq!(bundle.license.as_deref(), Some("MIT"));
        assert_eq!(bundle.homepage.as_deref(), Some("https://config.example"));
        assert!(matches!(
            bundle_provenance(&resolution, "bundle.license"),
            Provenance::ConfigFile(Some(_))
        ));
        assert_eq!(bundle.copyright, None);
        assert_eq!(bundle.category, None);
    }

    #[test]
    fn an_invalid_cargo_value_is_a_warning_and_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let mut package = facts(dir.path(), "");
        package.license = Some("MIT/Apache-2.0".to_owned());
        package.homepage = Some("not a url".to_owned());

        let resolution = resolve(&package, None, None).unwrap();

        assert_eq!(resolution.config.bundle.license, None);
        assert_eq!(resolution.config.bundle.homepage, None);
        let fields: Vec<&str> = resolution.diagnostics.iter().map(|d| d.field).collect();
        assert!(fields.contains(&"bundle.license") && fields.contains(&"bundle.homepage"));
    }

    #[test]
    fn an_invalid_license_names_the_field_and_the_line() {
        let dir = tempfile::tempdir().unwrap();

        let errors = bundle_errors(dir.path(), "license = \"Aache-2.0\"\n");

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("bundle.license") && errors[0].contains("SPDX"),
            "{errors:?}"
        );
        assert!(errors[0].contains(":3:"), "{errors:?}");
    }

    #[test]
    fn each_bad_bundle_value_is_reported_with_its_own_error() {
        let dir = tempfile::tempdir().unwrap();

        let errors = bundle_errors(
            dir.path(),
            "copyright = \"\"\ncategory = \"Productivity\"\nhomepage = \"https://u:p@example.com\"\nlicense_file = \"../LICENSE\"\nlicense = \"MIT OR\"\n",
        );

        assert_eq!(errors.len(), 5, "{errors:?}");
        for field in [
            "copyright",
            "category",
            "homepage",
            "license_file",
            "license",
        ] {
            assert!(
                errors
                    .iter()
                    .any(|e| e.contains(&format!("bundle.{field} "))),
                "{field}: {errors:?}"
            );
        }
    }

    #[test]
    fn a_missing_license_file_is_a_warning_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[bundle]\nlicense_file = \"LICENSE-MIT\"\n",
        );

        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();

        assert!(resolution.diagnostics.iter().any(|d| {
            d.code == "config.bundle_license_file_present" && d.message.contains("LICENSE-MIT")
        }));
    }

    #[test]
    fn an_unknown_bundle_key_is_still_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[bundle]\nrepository = \"https://example.com\"\n",
        );

        let error = resolve(&facts(dir.path(), ""), None, None).unwrap_err();

        assert!(error.to_string().contains("repository"), "{error}");
    }

    #[test]
    fn web_base_path_missing_leading_slash_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[web]\nbase_path = \"garden/\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidWebBasePath { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn web_icon_asset_missing_is_a_warning_diagnostic_under_target_web() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[web.icons]\nfavicon = \"missing.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), Some(Target::Web), None).unwrap();
        assert_eq!(resolution.diagnostics.len(), 1);
        assert_eq!(
            resolution.diagnostics[0].code,
            "config.web_icon_asset_present.favicon"
        );
    }

    #[test]
    fn web_icon_asset_check_is_skipped_outside_target_web() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[web.icons]\nfavicon = \"missing.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), Some(Target::Native), None).unwrap();
        assert!(resolution.diagnostics.is_empty());
    }

    #[test]
    fn invalid_favicon_extension_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[web.icons]\nfavicon = \"favicon.gif\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidWebIconFormat {
                        field: "favicon",
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn invalid_apple_touch_icon_extension_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[web.icons]\napple_touch_icon = \"icon.svg\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidWebIconFormat {
                        field: "apple_touch_icon",
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn absent_web_icons_resolve_to_none_with_no_native_icon_inheritance() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.icons]\nsource = \"assets/app.svg\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.web.icons.favicon, None);
        assert_eq!(resolution.config.web.icons.apple_touch_icon, None);
    }

    #[test]
    fn activation_defaults_to_disabled_single_instance_and_empty_schemes_and_associations() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let activation = &resolution.config.app.activation;
        assert!(!activation.single_instance);
        assert!(activation.url_schemes.is_empty());
        assert!(activation.file_associations.is_empty());
    }

    #[test]
    fn activation_resolves_single_instance_and_url_schemes() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.activation]\nsingle_instance = true\nurl_schemes = [\"garden\"]\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let activation = &resolution.config.app.activation;
        assert!(activation.single_instance);
        assert_eq!(activation.url_schemes, vec!["garden".to_owned()]);
    }

    #[test]
    fn empty_url_scheme_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.activation]\nurl_schemes = [\"\"]\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidActivation {
                        reason: ActivationError::EmptyUrlScheme,
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn url_scheme_with_invalid_characters_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.activation]\nurl_schemes = [\"1garden\"]\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidActivation {
                        reason: ActivationError::InvalidUrlSchemeCharacters { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_url_scheme_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.activation]\nurl_schemes = [\"garden\", \"Garden\"]\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidActivation {
                        reason: ActivationError::DuplicateUrlScheme { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn file_association_resolves_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[[app.activation.file_associations]]\nextension = \"garden\"\nmime_type = \"application/x-garden\"\ndescription = \"Garden document\"\nidentity = \"com.floregreen.garden.document\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let association = &resolution.config.app.activation.file_associations[0];
        assert_eq!(association.extension, "garden");
        assert_eq!(
            association.mime_type.as_deref(),
            Some("application/x-garden")
        );
        assert_eq!(association.description.as_deref(), Some("Garden document"));
        assert_eq!(association.identity, "com.floregreen.garden.document");
    }

    #[test]
    fn duplicate_file_association_identity_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n\
             [[app.activation.file_associations]]\n\
             extension = \"garden\"\n\
             identity = \"com.floregreen.garden.document\"\n\
             [[app.activation.file_associations]]\n\
             extension = \"gdn\"\n\
             identity = \"com.floregreen.garden.document\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidFileAssociation {
                        reason: FileAssociationError::DuplicateIdentity { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_file_association_extension_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n\
             [[app.activation.file_associations]]\n\
             extension = \"garden\"\n\
             identity = \"com.floregreen.garden.document\"\n\
             [[app.activation.file_associations]]\n\
             extension = \"Garden\"\n\
             identity = \"com.floregreen.garden.other\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidFileAssociation {
                        reason: FileAssociationError::DuplicateExtension { .. },
                        ..
                    }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn window_persistence_defaults_disabled_with_key_main() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let persistence = &resolution.config.window.persistence;
        assert!(!persistence.enabled);
        assert_eq!(persistence.key, "main");
    }

    #[test]
    fn window_persistence_enabled_with_explicit_key_resolves() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[window.persistence]\nenabled = true\nkey = \"editor\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let persistence = &resolution.config.window.persistence;
        assert!(persistence.enabled);
        assert_eq!(persistence.key, "editor");
    }

    #[test]
    fn default_locale_defaults_to_en_without_any_locales_declared() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.app.locales.default_locale, "en");
        assert!(resolution.config.app.locales.locales.is_empty());
    }

    #[test]
    fn declared_locales_resolve_with_their_fields() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.locales.en]\nname = \"Garden\"\ndescription = \"A workspace for your ideas\"\n[app.locales.pt-BR]\nname = \"Garden\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let locales = &resolution.config.app.locales.locales;
        assert_eq!(locales.len(), 2);
        assert_eq!(locales["en"].name.as_deref(), Some("Garden"));
        assert_eq!(
            locales["en"].description.as_deref(),
            Some("A workspace for your ideas")
        );
        assert_eq!(locales["pt-BR"].name.as_deref(), Some("Garden"));
    }

    #[test]
    fn default_locale_not_present_in_declared_locales_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndefault_locale = \"fr\"\n[app.locales.en]\nname = \"Garden\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidDefaultLocale { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn default_locale_without_any_declared_locales_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndefault_locale = \"fr\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(resolution.config.app.locales.default_locale, "fr");
    }

    #[test]
    fn invalid_locale_tag_shape_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.locales.x]\nname = \"Garden\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidLocaleTag { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn localized_identity_matches_the_requested_locale_exactly() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.locales.en]\nname = \"Garden\"\ndescription = \"A workspace for your ideas\"\n[app.locales.pt-BR]\nname = \"Jardim\"\ndescription = \"Um espaco para suas ideias\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let identity = resolution.config.app.localized_identity("pt-BR");
        assert_eq!(identity.name, "Jardim");
        assert_eq!(identity.description, Some("Um espaco para suas ideias"));
    }

    #[test]
    fn localized_identity_matching_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.locales.pt-BR]\nname = \"Jardim\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(
            resolution.config.app.localized_identity("PT-br").name,
            "Jardim"
        );
    }

    #[test]
    fn localized_identity_falls_back_to_default_locale_when_requested_is_undeclared() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndefault_locale = \"en\"\n[app.locales.en]\nname = \"Garden\"\ndescription = \"A workspace for your ideas\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let identity = resolution.config.app.localized_identity("fr");
        assert_eq!(identity.name, "Garden");
        assert_eq!(identity.description, Some("A workspace for your ideas"));
    }

    #[test]
    fn localized_identity_falls_back_to_the_base_identity_with_no_locales_declared() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let identity = resolution.config.app.localized_identity("fr");
        assert_eq!(identity.name, "app");
        assert_eq!(identity.description, None);
    }

    #[test]
    fn localized_identity_falls_back_to_default_locale_per_missing_field() {
        // pt-BR declares only `name` -- its own missing `description`
        // falls back to en's (the implicit default_locale), not straight
        // to the base app.description, proving the fallback is per field
        // within a locale that did partially match, not all-or-nothing.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndescription = \"Base description\"\n[app.locales.en]\nname = \"Garden\"\ndescription = \"English description\"\n[app.locales.pt-BR]\nname = \"Jardim\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let identity = resolution.config.app.localized_identity("pt-BR");
        assert_eq!(identity.name, "Jardim");
        assert_eq!(identity.description, Some("English description"));
    }

    #[test]
    fn localized_identity_falls_back_to_the_base_identity_when_default_locale_also_lacks_the_field()
    {
        // Neither pt-BR nor en (the default_locale) declares a
        // description -- falls all the way through to the base
        // app.description.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndescription = \"Base description\"\n[app.locales.en]\nname = \"Garden\"\n[app.locales.pt-BR]\nname = \"Jardim\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let identity = resolution.config.app.localized_identity("pt-BR");
        assert_eq!(identity.name, "Jardim");
        assert_eq!(identity.description, Some("Base description"));
    }

    #[test]
    fn a_conventional_locales_file_is_auto_discovered_with_no_main_config_change() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        write(
            dir.path(),
            "florui.locales.toml",
            "[en]\nname = \"Garden\"\ndescription = \"A workspace for your ideas\"\n[pt-BR]\nname = \"Jardim\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        let locales = &resolution.config.app.locales.locales;
        assert_eq!(locales.len(), 2);
        assert_eq!(locales["en"].name.as_deref(), Some("Garden"));
        assert_eq!(
            resolution.config.app.localized_identity("pt-BR").name,
            "Jardim"
        );
    }

    #[test]
    fn an_explicit_locales_file_reference_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nlocales_file = \"translations/app.toml\"\n",
        );
        std::fs::create_dir_all(dir.path().join("translations")).unwrap();
        write(
            &dir.path().join("translations"),
            "app.toml",
            "[en]\nname = \"Garden\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(
            resolution.config.app.locales.locales["en"].name.as_deref(),
            Some("Garden")
        );
    }

    #[test]
    fn an_explicit_locales_file_that_does_not_exist_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nlocales_file = \"missing.toml\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::LocalesFileNotFound { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn malformed_toml_in_an_external_locales_file_fails_immediately() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        write(dir.path(), "florui.locales.toml", "not valid toml [[[");
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        assert!(matches!(err, ConfigError::Toml { .. }));
    }

    #[test]
    fn declaring_locales_both_inline_and_in_an_external_file_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app.locales.en]\nname = \"Garden\"\n",
        );
        write(
            dir.path(),
            "florui.locales.toml",
            "[en]\nname = \"Garden\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::ConflictingLocalesSource { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn default_locale_is_validated_against_an_external_file_too() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\ndefault_locale = \"fr\"\n",
        );
        write(
            dir.path(),
            "florui.locales.toml",
            "[en]\nname = \"Garden\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::InvalidDefaultLocale { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn environment_overlay_merges_identifier_and_icons_by_field() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n\
             [app]\n\
             identifier = \"com.floregreen.garden\"\n\
             [app.icons]\n\
             windows = \"assets/app.ico\"\n\
             [environments.development.app]\n\
             identifier = \"com.floregreen.garden.dev\"\n\
             [environments.development.app.icons]\n\
             source = \"assets/app-dev.svg\"\n",
        );
        let resolution = resolve(
            &facts(dir.path(), ""),
            None,
            Some(EnvironmentSelection {
                name: "development",
                explicit: true,
            }),
        )
        .unwrap();
        assert_eq!(
            resolution.config.app.identifier.as_deref(),
            Some("com.floregreen.garden.dev")
        );
        assert_eq!(
            resolution.config.app.icons.source,
            Some(dir.path().join("assets/app-dev.svg"))
        );
        assert_eq!(
            resolution.config.app.icons.windows,
            Some(dir.path().join("assets/app.ico"))
        );
        assert!(resolution.environment.overlay_applied);
        assert_eq!(
            resolution.environment.selected.as_deref(),
            Some("development")
        );
    }

    #[test]
    fn unselected_environment_falls_back_to_base_configuration() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n\
             [environments.development.app]\nidentifier = \"com.floregreen.garden.dev\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(
            resolution.config.app.identifier.as_deref(),
            Some("com.floregreen.garden")
        );
        assert!(!resolution.environment.overlay_applied);
        assert_eq!(resolution.environment.selected, None);
        assert_eq!(
            resolution.environment.declared,
            vec!["development".to_owned()]
        );
    }

    #[test]
    fn implicit_default_environment_missing_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let resolution = resolve(
            &facts(dir.path(), ""),
            None,
            Some(EnvironmentSelection {
                name: "production",
                explicit: false,
            }),
        )
        .unwrap();
        assert!(!resolution.environment.overlay_applied);
        assert_eq!(
            resolution.environment.selected.as_deref(),
            Some("production")
        );
    }

    #[test]
    fn explicit_unknown_environment_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "florui.config.toml", "schema_version = 1\n");
        let err = resolve(
            &facts(dir.path(), ""),
            None,
            Some(EnvironmentSelection {
                name: "staging",
                explicit: true,
            }),
        )
        .unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::UnknownEnvironment { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_identifier_between_declared_environment_and_base_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n\
             [environments.development]\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::DuplicateEnvironmentIdentifier { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_identifier_between_two_declared_environments_is_a_semantic_error() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n\
             [environments.development.app]\nidentifier = \"com.floregreen.garden.shared\"\n\
             [environments.staging.app]\nidentifier = \"com.floregreen.garden.shared\"\n",
        );
        let err = resolve(&facts(dir.path(), ""), None, None).unwrap_err();
        match err {
            ConfigError::Semantic(errors) => {
                assert!(matches!(
                    errors[0],
                    SemanticConfigError::DuplicateEnvironmentIdentifier { .. }
                ));
            }
            other => panic!("expected Semantic, got {other:?}"),
        }
    }

    #[test]
    fn distinct_identifiers_across_environments_pass_validation() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "florui.config.toml",
            "schema_version = 1\n[app]\nidentifier = \"com.floregreen.garden\"\n\
             [environments.development.app]\nidentifier = \"com.floregreen.garden.dev\"\n\
             [environments.staging.app]\nidentifier = \"com.floregreen.garden.staging\"\n",
        );
        let resolution = resolve(&facts(dir.path(), ""), None, None).unwrap();
        assert_eq!(
            resolution.config.app.identifier.as_deref(),
            Some("com.floregreen.garden")
        );
    }
}
