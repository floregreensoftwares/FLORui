//! Typed `florui.config.toml` schema, discovery, and resolution.
//!
//! [`resolve`] turns a [`CargoProjectFacts`] (from [`resolve_cargo_project`])
//! plus whatever `florui.config.toml` sits at its package root, the
//! selected [`Target`], and the selected [`EnvironmentSelection`] into a
//! fully defaulted [`ResolvedConfig`], with per-field [`Provenance`] and
//! non-fatal [`Diagnostic`]s. Covers `schema_version`, `[app]`,
//! `[app.icons]`, `[app.activation]`, `[app.locales]`, `[window]`,
//! `[window.persistence]`, `[bundle]`, `[dev]` (the last only to the extent
//! needed to detect a conflict with the legacy
//! `[package.metadata.florui.dev]`), the independent `[web]`/`[web.icons]`,
//! and named `[environments.<name>]` overlays (scoped to `app.identifier`,
//! `app.name`, `app.description`, and `app.icons.*`).

mod bundle;
mod error;
mod json_schema;
mod location;
mod project;
mod resolve;
mod resolved;
mod schema;

pub use error::{
    ActivationError, ConfigError, Diagnostic, FileAssociationError, SemanticConfigError, Severity,
    WindowSizeError, WorkspaceInheritanceError,
};
pub use json_schema::{SCHEMA_FILE_NAME, json_schema, json_schema_pretty};
pub use location::SourceLocation;
pub use project::{
    CargoProjectFacts, ProjectResolutionError, config_file_path, parse_cargo_project_facts,
    resolve_cargo_project,
};
pub use resolve::{EnvironmentResolution, EnvironmentSelection, Resolution, resolve};
pub use resolved::{
    ActivationConfig, AppConfig, BundleConfig, DecorationsSetting, DevConfig, FieldProvenance,
    FileAssociationConfig, IconsConfig, LocaleConfig, LocalesConfig, LocalizedIdentity, Provenance,
    ResolvedConfig, Target, WebConfig, WebIconsConfig, WindowConfig, WindowPersistenceConfig,
};
