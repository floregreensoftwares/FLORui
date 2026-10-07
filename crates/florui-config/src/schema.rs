//! Typed `florui.config.toml` shape. These structs are the one definition of
//! the file: they are what the parser deserializes into, and the JSON Schema
//! editors read (see `json_schema.rs`) is derived from them, field
//! documentation included. The `///` lines on a field are therefore written
//! for the person editing the configuration; notes for maintainers are plain
//! `//` comments.
//!
//! Fields that need a source location for later validation (see
//! `resolve.rs`) are wrapped in [`Spanned`]; everything else is a plain
//! `Option<T>`, already validated by `toml`'s own type checking.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

pub const SUPPORTED_SCHEMA_VERSION: i64 = 1;

/// A value that remembers where in the file it was written. A thin wrapper
/// over [`toml::Spanned`] that also describes itself to the JSON Schema as
/// the plain value it wraps, so a field's location tracking never shows up
/// in the schema.
#[derive(Debug)]
pub(crate) struct Spanned<T>(toml::Spanned<T>);

impl<T> Spanned<T> {
    pub(crate) fn span(&self) -> Range<usize> {
        self.0.span()
    }

    pub(crate) fn get_ref(&self) -> &T {
        self.0.get_ref()
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Spanned<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        toml::Spanned::<T>::deserialize(deserializer).map(Spanned)
    }
}

impl<T: JsonSchema> JsonSchema for Spanned<T> {
    fn inline_schema() -> bool {
        T::inline_schema()
    }

    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn schema_id() -> Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
}

/// First step of the two-step parse (see `resolve.rs`): reads only
/// `schema_version`, ignoring every other key — deliberately not
/// `deny_unknown_fields`, so a newer schema's added fields don't get in the
/// way of first checking whether this build can even understand the file.
#[derive(Deserialize)]
pub(crate) struct SchemaVersionProbe {
    pub(crate) schema_version: Spanned<i64>,
}

fn schema_version_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({
        "type": "integer",
        "const": SUPPORTED_SCHEMA_VERSION,
        "description": "The version of this file format. Always 1 for now; a different value is rejected before anything else is read."
    })
}

/// The whole `florui.config.toml`: one application's identity, window
/// defaults, distribution metadata and per-environment overrides.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
#[schemars(title = "florui.config.toml")]
pub(crate) struct RawConfig {
    // Already consumed by `SchemaVersionProbe` before this struct is ever
    // parsed; kept (rather than dropped) so `deny_unknown_fields` still
    // recognizes the key instead of rejecting it as unknown.
    #[serde(rename = "schema_version")]
    #[schemars(schema_with = "schema_version_schema")]
    pub(crate) _schema_version: i64,
    /// The application's identity: name, version, icons, activation and
    /// translated names.
    pub(crate) app: Option<RawApp>,
    /// Defaults for the application's primary window.
    pub(crate) window: Option<RawWindow>,
    /// Distribution metadata written into a build.
    pub(crate) bundle: Option<RawBundle>,
    /// What `florui dev` runs.
    pub(crate) dev: Option<RawDev>,
    /// Browser metadata for the Web target, independent of the native
    /// application's window and icons.
    pub(crate) web: Option<RawWeb>,
    /// Named overrides of the application's identity, chosen with
    /// `--environment <name>`: for example a `development` environment with
    /// its own identifier so it installs next to the production one.
    // Spanned around the whole table -- "unknown environment" and
    // "duplicate identifier" are properties of the declared set, not one
    // entry, so both errors cite this table's own location.
    pub(crate) environments: Option<Spanned<BTreeMap<String, RawEnvironmentOverlay>>>,
}

/// One environment's overrides. Only what scopes an installation's identity
/// can be overridden: `app.identifier`, `app.name`, `app.description` and
/// `app.icons`.
// Not `window`, `bundle`, `dev`, `web`, `app.activation`,
// `app.locales`/`default_locale` -- an environment only ever overlays the
// fields that scope installation identity, instance coordination, and
// window persistence per-environment.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEnvironmentOverlay {
    /// The identity fields this environment overrides.
    pub(crate) app: Option<RawEnvironmentOverlayApp>,
}

/// The `[app]` fields an environment can override.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawEnvironmentOverlayApp {
    /// Replaces `app.identifier` in this environment. Environments that
    /// should install side by side need distinct identifiers.
    pub(crate) identifier: Option<String>,
    /// Replaces `app.name` in this environment.
    pub(crate) name: Option<String>,
    /// Replaces `app.description` in this environment.
    pub(crate) description: Option<String>,
    /// Replaces the icons in this environment, field by field.
    pub(crate) icons: Option<RawIcons>,
    /// Replaces the activation fields it names in this environment, field by
    /// field. Environments that are installed side by side need distinct URL
    /// schemes and file association identities, or they contend for them.
    pub(crate) activation: Option<RawActivation>,
}

/// Browser metadata for the Web target. It never inherits the native
/// window's title or the native icons.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWeb {
    /// The page title. Defaults to `app.name`.
    pub(crate) title: Option<String>,
    /// The page description. Defaults to `app.description`.
    pub(crate) description: Option<String>,
    /// The URL path the site is deployed under, starting with `/`. Defaults
    /// to `/`.
    pub(crate) base_path: Option<Spanned<String>>,
    /// The icons browsers show for the site.
    pub(crate) icons: Option<RawWebIcons>,
}

/// The icons of the Web target. Neither falls back to the native icon.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWebIcons {
    /// Path, relative to this file, of the favicon: an SVG, PNG or ICO that
    /// is copied as it is.
    pub(crate) favicon: Option<Spanned<String>>,
    /// Path, relative to this file, of the touch icon: a PNG.
    pub(crate) apple_touch_icon: Option<Spanned<String>>,
}

/// The application's identity.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawApp {
    /// A stable reverse-domain identity such as `com.example.app`. It scopes
    /// the application's data, instance coordination and window
    /// persistence, and is required for a distributable application.
    /// Changing it changes the installation's identity.
    pub(crate) identifier: Option<String>,
    /// The product name shown to users. Defaults to the Cargo package name.
    pub(crate) name: Option<String>,
    /// A one-line description of the application.
    pub(crate) description: Option<String>,
    /// The application's version: a version string, or `{ workspace = true }`
    /// to take the workspace package's version. Defaults to the Cargo
    /// package's version.
    pub(crate) version: Option<RawVersion>,
    /// The application icon, per platform.
    pub(crate) icons: Option<RawIcons>,
    /// Single-instance behavior, URL schemes and file associations.
    pub(crate) activation: Option<RawActivation>,
    /// The application's name and description in other languages, keyed by
    /// locale tag such as `pt-BR`. Mutually exclusive with `locales_file`.
    // Spanned around the whole table -- an invalid tag or a
    // `default_locale` mismatch is a property of the declared set, not one
    // key, so every locale error cites the table's own location.
    pub(crate) locales: Option<Spanned<BTreeMap<String, RawLocale>>>,
    /// The locale used when the requested one is not declared. Defaults to
    /// `en`. It must be one of the declared locales.
    pub(crate) default_locale: Option<Spanned<String>>,
    /// Path, relative to this file, of a TOML file that supplies the locales
    /// instead of declaring them here. A `florui.locales.toml` next to this
    /// file is used when this is omitted. Mutually exclusive with `locales`.
    pub(crate) locales_file: Option<Spanned<String>>,
}

/// The application's name and description in one language.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLocale {
    /// The product name in this language. Falls back to `app.name`.
    pub(crate) name: Option<String>,
    /// The description in this language. Falls back to `app.description`.
    pub(crate) description: Option<String>,
}

/// How the application is started again by the system: a second launch, a
/// URL, or a file.
// Typed schema only -- no OS registration, no single-instance IPC, no
// activation events. `florui-platform` has no `florui-config` consumer at
// all yet, so there is no runtime to wire this into; the shape is defined
// ahead of the runtime that will eventually consume it, the same
// schema-before-behavior treatment `[window]`/`[app.icons]` already got.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawActivation {
    /// Whether a second launch is handed to the running instance instead of
    /// starting another.
    pub(crate) single_instance: Option<bool>,
    /// URL schemes the application handles, without the `://`, for example
    /// `garden`.
    // Spanned around the whole array: a per-scheme problem (empty,
    // invalid characters, duplicate) is a property of one entry, but
    // there's no existing convention for spanning one element of a TOML
    // array here, so every activation error cites the array's own
    // location, same coarseness as `[environments]`'s table span.
    pub(crate) url_schemes: Option<Spanned<Vec<String>>>,
    /// File types the application opens.
    pub(crate) file_associations: Option<Vec<RawFileAssociation>>,
    /// The schemes and file types the application may ask to be the default
    /// handler of: a scheme as declared above (`garden`), a file type as its
    /// extension with a dot (`.garden`). Each must be declared. This is a
    /// statement of intent: registering the application changes no default,
    /// and when the application asks, the user chooses in the operating
    /// system's own settings.
    pub(crate) request_default: Option<Spanned<Vec<String>>>,
}

/// One file type the application opens.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawFileAssociation {
    /// The file extension, without the dot. Each extension may be declared
    /// once.
    pub(crate) extension: Spanned<String>,
    /// The MIME type of these files.
    pub(crate) mime_type: Option<String>,
    /// What these files are, as a person would say it.
    pub(crate) description: Option<String>,
    /// A stable name for this association, distinct from the extension, that
    /// survives a renamed extension or description across releases. Each
    /// identity may be declared once.
    pub(crate) identity: Spanned<String>,
}

/// Icon sources. SVG and PNG are supported; paths are relative to this
/// file.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawIcons {
    /// The icon used on every platform that has no override of its own.
    pub(crate) source: Option<Spanned<String>>,
    /// The icon for Windows, overriding `source`.
    pub(crate) windows: Option<Spanned<String>>,
    /// The icon for macOS, overriding `source`.
    pub(crate) macos: Option<Spanned<String>>,
    /// The icon for Linux, overriding `source`.
    pub(crate) linux: Option<Spanned<String>>,
}

/// Defaults for the primary window. A window created in code can override
/// them.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWindow {
    /// The window title. Defaults to `app.name`.
    pub(crate) title: Option<String>,
    /// The initial width in logical pixels. Must be positive.
    pub(crate) width: Option<Spanned<f64>>,
    /// The initial height in logical pixels. Must be positive.
    pub(crate) height: Option<Spanned<f64>>,
    /// The smallest width the window can be resized to, in logical pixels.
    /// Must not exceed `width`.
    pub(crate) min_width: Option<Spanned<f64>>,
    /// The smallest height the window can be resized to, in logical pixels.
    /// Must not exceed `height`.
    pub(crate) min_height: Option<Spanned<f64>>,
    /// Who draws the window frame. Defaults to `system`.
    pub(crate) decorations: Option<Spanned<RawDecorations>>,
    /// Whether the window can be see-through. Defaults to `false`.
    pub(crate) transparent: Option<bool>,
    /// Remembering the window's size and position between runs.
    pub(crate) persistence: Option<RawWindowPersistence>,
}

/// Remembering the window between runs.
// Typed schema only -- no bounds save/restore, no monitor revalidation, no
// `florui-platform` consumer yet (see `RawActivation`'s own note).
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawWindowPersistence {
    /// Whether the window's size and position are saved and restored.
    /// Defaults to `false`.
    pub(crate) enabled: Option<bool>,
    /// The name this window's saved state is stored under. Defaults to
    /// `main`.
    pub(crate) key: Option<String>,
}

/// Who draws the window frame.
#[derive(Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RawDecorations {
    /// The operating system's own frame.
    System,
    /// A frame the application draws itself.
    Custom,
}

/// Distribution metadata written into a native build.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawBundle {
    /// The company or person that publishes the application, shown in the
    /// executable's properties.
    pub(crate) publisher: Option<String>,
    /// A one-line copyright notice, written into the executable's version
    /// information.
    pub(crate) copyright: Option<Spanned<String>>,
    /// An SPDX license expression such as `MIT OR Apache-2.0`. Falls back to
    /// the package's Cargo `license`.
    pub(crate) license: Option<Spanned<String>>,
    /// Path, relative to this file and inside the package, of the file with
    /// the license text. It is staged beside the executable as the notice.
    /// Falls back to the package's Cargo `license-file`.
    pub(crate) license_file: Option<Spanned<String>>,
    /// A lowercase category token such as `productivity`.
    pub(crate) category: Option<Spanned<String>>,
    /// The application's home page: an absolute http or https URL without
    /// credentials. Falls back to the package's Cargo `homepage`.
    pub(crate) homepage: Option<Spanned<String>>,
}

/// What `florui dev` runs.
#[derive(Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawDev {
    /// The Cargo example `florui dev` builds and runs.
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
        let spanned: toml::Spanned<toml::Value> = Deserialize::deserialize(deserializer)?;
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

// The editor schema is stricter than the deserializer here: a shape the
// deserializer maps to `Invalid` (reported later with a location) is not
// offered, and `{ workspace = false }` is not offered either.
impl JsonSchema for RawVersion {
    fn schema_name() -> Cow<'static, str> {
        "AppVersion".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "A version string, or { workspace = true } to take the workspace package's version.",
            "oneOf": [
                { "type": "string" },
                {
                    "type": "object",
                    "properties": { "workspace": { "const": true } },
                    "required": ["workspace"],
                    "additionalProperties": false
                }
            ]
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
