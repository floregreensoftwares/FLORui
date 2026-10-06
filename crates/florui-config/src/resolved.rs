//! The public, fully-resolved shape `resolve()` produces: every field
//! populated (no `Option` where a default exists), plus where each value
//! actually came from.

use crate::location::SourceLocation;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Which native/build target `resolve()` should check assets against —
/// asset-existence checks are target-scoped ("resolve assets... for the
/// selected target"), unlike schema validation, which always runs in full
/// regardless of target. `None` (schema-only mode) skips asset checks
/// entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Native,
    Web,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub app: AppConfig,
    pub window: WindowConfig,
    pub bundle: BundleConfig,
    pub dev: DevConfig,
    pub web: WebConfig,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub identifier: Option<String>,
    /// The Cargo package name when `app.name` is unset.
    pub name: String,
    pub description: Option<String>,
    /// The Cargo-resolved concrete version when `app.version` is unset or
    /// requests `{ workspace = true }`.
    pub version: String,
    pub icons: IconsConfig,
    pub activation: ActivationConfig,
    pub locales: LocalesConfig,
}

impl AppConfig {
    /// Resolves the effective product name/description for `requested` (a
    /// locale tag; matching is case-insensitive, since a caller and a
    /// config author might reasonably disagree on casing) -- the identity
    /// locale fallback contract `app.default_locale` configures. Falls
    /// back per field, not per locale entry: a declared locale missing
    /// one field (e.g. `[app.locales.pt-BR]` with a `name` but no
    /// `description`) still contributes its own `name` while falling
    /// back to a less-specific value for the missing field, rather than
    /// discarding the whole match. The chain for each field is:
    /// `requested`'s own entry, then `default_locale`'s entry, then the
    /// base, locale-independent `app.name`/`app.description` -- never an
    /// error, since requesting a locale nobody declared is an ordinary,
    /// expected case, not a misconfiguration.
    pub fn localized_identity(&self, requested: &str) -> LocalizedIdentity<'_> {
        let requested_locale = self.locales.find(requested);
        let default_locale = self.locales.find(&self.locales.default_locale);
        let name = requested_locale
            .and_then(|locale| locale.name.as_deref())
            .or_else(|| default_locale.and_then(|locale| locale.name.as_deref()))
            .unwrap_or(&self.name);
        let description = requested_locale
            .and_then(|locale| locale.description.as_deref())
            .or_else(|| default_locale.and_then(|locale| locale.description.as_deref()))
            .or(self.description.as_deref());
        LocalizedIdentity { name, description }
    }
}

/// The product name/description [`AppConfig::localized_identity`]
/// resolved for one specific requested locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalizedIdentity<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
}

/// `default_locale` is always populated, defaulting to `"en"` even with no
/// `[app.locales]` declared at all.
#[derive(Debug, Clone)]
pub struct LocalesConfig {
    pub default_locale: String,
    pub locales: BTreeMap<String, LocaleConfig>,
}

impl LocalesConfig {
    /// Case-insensitive exact match against a declared locale tag -- not
    /// a language-only ("pt" matching a declared "pt-BR") or CLDR-style
    /// fallback; the contract this crate implements is deliberately just
    /// "requested, else default_locale, else the base identity," per
    /// the locale fallback contract.
    fn find(&self, tag: &str) -> Option<&LocaleConfig> {
        self.locales
            .iter()
            .find(|(declared, _)| declared.eq_ignore_ascii_case(tag))
            .map(|(_, locale)| locale)
    }
}

#[derive(Debug, Clone)]
pub struct LocaleConfig {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Typed declaration only -- no OS registration, no single-instance IPC.
/// See `crate::schema::RawActivation`'s doc comment for why.
#[derive(Debug, Clone, Default)]
pub struct ActivationConfig {
    pub single_instance: bool,
    pub url_schemes: Vec<String>,
    pub file_associations: Vec<FileAssociationConfig>,
}

#[derive(Debug, Clone)]
pub struct FileAssociationConfig {
    pub extension: String,
    pub mime_type: Option<String>,
    pub description: Option<String>,
    pub identity: String,
}

#[derive(Debug, Clone, Default)]
pub struct IconsConfig {
    /// Resolved relative to `florui.config.toml`'s own directory, never
    /// the process's current directory.
    pub source: Option<PathBuf>,
    pub windows: Option<PathBuf>,
    pub macos: Option<PathBuf>,
    pub linux: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// `app.name` when `window.title` is unset.
    pub title: String,
    /// `None` means "no opinion" -- this crate does not invent a
    /// framework-wide default window size.
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub min_width: Option<f64>,
    pub min_height: Option<f64>,
    pub decorations: DecorationsSetting,
    pub transparent: bool,
    pub persistence: WindowPersistenceConfig,
}

/// Typed declaration only -- no bounds save/restore, no monitor
/// revalidation. See `crate::schema::RawWindowPersistence`'s doc comment.
#[derive(Debug, Clone)]
pub struct WindowPersistenceConfig {
    pub enabled: bool,
    /// Defaults to `"main"` when `persistence` is declared without a key.
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorationsSetting {
    System,
    Custom,
}

#[derive(Debug, Clone, Default)]
pub struct BundleConfig {
    pub publisher: Option<String>,
    pub copyright: Option<String>,
    /// A valid SPDX expression: configured, else the package's Cargo
    /// `license`.
    pub license: Option<String>,
    /// Resolved relative to `florui.config.toml`'s own directory: configured,
    /// else the package's Cargo `license-file`.
    pub license_file: Option<PathBuf>,
    pub category: Option<String>,
    /// An absolute http(s) URL: configured, else the package's Cargo
    /// `homepage`.
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DevConfig {
    /// The merged legacy-or-new dev example -- see `resolve.rs` for the
    /// duplicate-definition check that runs before this is populated.
    pub example: Option<String>,
}

/// Independent of native `[app]`/`[window]` -- `title`/`description` fall
/// back to `app.name`/`app.description`, never to native `window.title`,
/// and there is no implicit inheritance of `app.icons` into `icons`.
#[derive(Debug, Clone)]
pub struct WebConfig {
    pub title: String,
    pub description: Option<String>,
    pub base_path: String,
    pub icons: WebIconsConfig,
}

#[derive(Debug, Clone, Default)]
pub struct WebIconsConfig {
    /// Resolved relative to `florui.config.toml`'s own directory, same as
    /// `IconsConfig`'s fields.
    pub favicon: Option<PathBuf>,
    pub apple_touch_icon: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Provenance {
    /// From `florui.config.toml` itself; carries an exact location for the
    /// subset of fields that get post-parse semantic validation (window
    /// sizes, decorations, app.version, icon paths, dev.example) and `None`
    /// for plain fields that are validated for free by `toml`'s own type
    /// checking.
    ConfigFile(Option<SourceLocation>),
    /// Resolved from `cargo metadata` (e.g. `app.version` defaulting to the
    /// package's own Cargo-resolved version).
    CargoManifest,
    /// From `[package.metadata.florui.dev]`.
    LegacyMetadata,
    /// From the named `[environments.<name>]` overlay's own `app` table --
    /// outranks `ConfigFile` for the same field, per the documented
    /// resolution order (defaults/presets, base configuration, selected
    /// environment, then supported CLI flags).
    Environment(String),
    BuiltinDefault,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldProvenance {
    pub field: &'static str,
    pub provenance: Provenance,
}
