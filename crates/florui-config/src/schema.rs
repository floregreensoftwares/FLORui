//! Typed `florui.config.toml` shape, deserialized straight off `toml`'s
//! own [`toml::Spanned`] where a field needs a source location for later
//! semantic validation (see `resolve.rs`) — everything else is a plain
//! `Option<T>`, already validated for free by `toml`'s own type checking.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::ops::Range;
use toml::Spanned;

pub const SUPPORTED_SCHEMA_VERSION: i64 = 1;

/// First step of the two-step parse (see `resolve.rs`): reads only
/// `schema_version`, ignoring every other key — deliberately not
/// `deny_unknown_fields`, so a newer schema's added fields don't get in the
/// way of first checking whether this build can even understand the file.
#[derive(Deserialize)]
pub(crate) struct SchemaVersionProbe {
    pub(crate) schema_version: Spanned<i64>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawConfig {
    // Already consumed by `SchemaVersionProbe` before this struct is ever
    // parsed; kept (rather than dropped) so `deny_unknown_fields` still
    // recognizes the key instead of rejecting it as unknown.
    #[serde(rename = "schema_version")]
    pub(crate) _schema_version: i64,
    pub(crate) app: Option<RawApp>,
    pub(crate) window: Option<RawWindow>,
    pub(crate) bundle: Option<RawBundle>,
    pub(crate) dev: Option<RawDev>,
    pub(crate) web: Option<RawWeb>,
    /// Spanned around the whole table -- "unknown environment" and
    /// "duplicate identifier" are properties of the declared set, not one
    /// entry, so both errors cite this table's own location.
    pub(crate) environments: Option<Spanned<BTreeMap<String, RawEnvironmentOverlay>>>,
}

/// Scoped to exactly what overlays application identity: `app.identifier`,
/// `app.name`, `app.description`, `app.icons.*`. Not `window`, `bundle`,
/// `dev`, `web`, `app.activation`, `app.locales`/`default_locale` -- an
/// environment only ever overlays the fields that scope installation
/// identity, instance coordination, and window persistence per-environment.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEnvironmentOverlay {
    pub(crate) app: Option<RawEnvironmentOverlayApp>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEnvironmentOverlayApp {
    pub(crate) identifier: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) icons: Option<RawIcons>,
}

/// `[web]` -- independent browser metadata/assets, never inheriting native
/// `[window]`/`app.icons` (see `resolve.rs`'s `resolve_web`).
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWeb {
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) base_path: Option<Spanned<String>>,
    pub(crate) icons: Option<RawWebIcons>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWebIcons {
    pub(crate) favicon: Option<Spanned<String>>,
    pub(crate) apple_touch_icon: Option<Spanned<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawApp {
    pub(crate) identifier: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) version: Option<RawVersion>,
    pub(crate) icons: Option<RawIcons>,
    pub(crate) activation: Option<RawActivation>,
    /// Spanned around the whole table -- an invalid tag or a
    /// `default_locale` mismatch is a property of the declared set, not one
    /// key, so every locale error cites the table's own location.
    pub(crate) locales: Option<Spanned<BTreeMap<String, RawLocale>>>,
    pub(crate) default_locale: Option<Spanned<String>>,
    /// An explicit reference to an external TOML file (relative to this
    /// config's own directory, like every other path here) supplying
    /// `[app.locales]`'s entries instead of declaring them inline --
    /// mutually exclusive with `locales`. See
    /// `resolve::resolve_external_locales_file`'s own doc for the
    /// conventional filename (`florui.locales.toml`) this can be omitted
    /// in favor of, auto-discovered when present.
    pub(crate) locales_file: Option<Spanned<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLocale {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
}

/// Typed schema only -- no OS registration, no single-instance IPC, no
/// activation events. `florui-platform` has no `florui-config` consumer at
/// all yet, so there is no runtime to wire this into; the shape is defined
/// ahead of the runtime that will eventually consume it, the same
/// schema-before-behavior treatment `[window]`/`[app.icons]` already got.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawActivation {
    pub(crate) single_instance: Option<bool>,
    /// Spanned around the whole array: a per-scheme problem (empty,
    /// invalid characters, duplicate) is a property of one entry, but
    /// there's no existing convention for spanning one element of a TOML
    /// array here, so every activation error cites the array's own
    /// location, same coarseness as `[environments]`'s table span.
    pub(crate) url_schemes: Option<Spanned<Vec<String>>>,
    pub(crate) file_associations: Option<Vec<RawFileAssociation>>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFileAssociation {
    pub(crate) extension: Spanned<String>,
    pub(crate) mime_type: Option<String>,
    pub(crate) description: Option<String>,
    /// A stable identity for this association, distinct from `extension`,
    /// meant to survive a renamed extension or description across releases.
    pub(crate) identity: Spanned<String>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawIcons {
    pub(crate) source: Option<Spanned<String>>,
    pub(crate) windows: Option<Spanned<String>>,
    pub(crate) macos: Option<Spanned<String>>,
    pub(crate) linux: Option<Spanned<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWindow {
    pub(crate) title: Option<String>,
    pub(crate) width: Option<Spanned<f64>>,
    pub(crate) height: Option<Spanned<f64>>,
    pub(crate) min_width: Option<Spanned<f64>>,
    pub(crate) min_height: Option<Spanned<f64>>,
    pub(crate) decorations: Option<Spanned<RawDecorations>>,
    pub(crate) transparent: Option<bool>,
    pub(crate) persistence: Option<RawWindowPersistence>,
}

/// Typed schema only -- no bounds save/restore, no monitor revalidation, no
/// `florui-platform` consumer yet (see `RawActivation`'s own doc comment).
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWindowPersistence {
    pub(crate) enabled: Option<bool>,
    pub(crate) key: Option<String>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RawDecorations {
    System,
    Custom,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawBundle {
    pub(crate) publisher: Option<String>,
    pub(crate) copyright: Option<Spanned<String>>,
    /// An SPDX expression. Falls back to the package's Cargo `license`.
    pub(crate) license: Option<Spanned<String>>,
    /// Relative to the configuration file. Falls back to the package's
    /// Cargo `license-file`.
    pub(crate) license_file: Option<Spanned<String>>,
    pub(crate) category: Option<Spanned<String>>,
    /// Falls back to the package's Cargo `homepage`.
    pub(crate) homepage: Option<Spanned<String>>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDev {
    pub(crate) example: Option<Spanned<String>>,
}

/// `app.version`'s two accepted shapes (a literal version string, or
/// `{ workspace = true }`). NOT `#[serde(untagged)]`: an untagged enum
/// whose variants wrap `toml::Spanned<T>` fails to deserialize even the
/// valid cases (confirmed with a standalone experiment before adopting
/// `toml` — every variant's span-capturing `Deserialize` impl reports "did
/// not match any variant" instead of the specific problem). Deserializing
/// into `Spanned<toml::Value>` and dispatching on the value's shape here
/// sidesteps that entirely. `Invalid` is never a deserialize error — a
/// shape this doesn't recognize becomes a semantic error in `resolve.rs`
/// with the same one-place-cites-a-location treatment as every other
/// semantic problem, rather than a raw TOML type error.
#[derive(Debug)]
pub(crate) enum RawVersion {
    Literal { value: String, span: Range<usize> },
    Workspace { requested: bool, span: Range<usize> },
    Invalid { span: Range<usize> },
}

impl<'de> Deserialize<'de> for RawVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let spanned: Spanned<toml::Value> = Deserialize::deserialize(deserializer)?;
        let span = spanned.span();
        Ok(match spanned.into_inner() {
            toml::Value::String(value) => RawVersion::Literal { value, span },
            toml::Value::Table(table) if table.len() == 1 => match table.get("workspace") {
                Some(toml::Value::Boolean(requested)) => RawVersion::Workspace {
                    requested: *requested,
                    span,
                },
                _ => RawVersion::Invalid { span },
            },
            _ => RawVersion::Invalid { span },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_probe_ignores_unrelated_keys() {
        let probe: SchemaVersionProbe =
            toml::from_str("schema_version = 1\n[app]\nname = \"Garden\"\n").unwrap();
        assert_eq!(*probe.schema_version.get_ref(), 1);
    }

    #[test]
    fn raw_config_rejects_an_unknown_top_level_key() {
        let err = toml::from_str::<RawConfig>("schema_version = 1\nbogus = true\n").unwrap_err();
        assert!(err.message().contains("bogus"));
    }

    #[test]
    fn raw_config_rejects_an_unknown_key_in_a_nested_table() {
        let err =
            toml::from_str::<RawConfig>("schema_version = 1\n[window]\nbogus = 1\n").unwrap_err();
        assert!(err.message().contains("bogus"));
    }

    #[test]
    fn raw_version_accepts_a_literal_string() {
        #[derive(Deserialize)]
        struct Probe {
            version: RawVersion,
        }
        let probe: Probe = toml::from_str("version = \"1.2.3\"\n").unwrap();
        match probe.version {
            RawVersion::Literal { value, .. } => assert_eq!(value, "1.2.3"),
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn raw_version_accepts_a_workspace_table() {
        #[derive(Deserialize)]
        struct Probe {
            version: RawVersion,
        }
        let probe: Probe = toml::from_str("version = { workspace = true }\n").unwrap();
        match probe.version {
            RawVersion::Workspace { requested, .. } => assert!(requested),
            _ => panic!("expected Workspace"),
        }
    }

    #[test]
    fn raw_version_marks_an_unrecognized_shape_invalid_rather_than_erroring() {
        #[derive(Deserialize)]
        struct Probe {
            version: RawVersion,
        }
        let probe: Probe = toml::from_str("version = 123\n").unwrap();
        assert!(matches!(probe.version, RawVersion::Invalid { .. }));
    }

    #[test]
    fn invalid_decorations_value_fails_to_parse() {
        let err = toml::from_str::<RawConfig>(
            "schema_version = 1\n[window]\ndecorations = \"diagonal\"\n",
        )
        .unwrap_err();
        assert!(err.span().is_some());
    }
}
