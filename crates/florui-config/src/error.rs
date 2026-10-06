//! `resolve()`'s error and diagnostic types — three tiers, not two:
//!
//! 1. [`ConfigError::Toml`]: `florui.config.toml` doesn't even produce a
//!    valid [`crate::schema::RawConfig`] (malformed TOML, an unknown key, a
//!    wrong type). Fail-fast — nothing downstream is evaluable.
//! 2. [`ConfigError::Semantic`]: `RawConfig` parsed fine; independent
//!    semantic checks over it (plus the Cargo project's own facts) found
//!    one or more problems. Accumulated, not fail-fast, since each check is
//!    independently evaluable against already-valid data — this is what
//!    lets `florui doctor` report a real pass/fail per check instead of
//!    `unknown` for everything after the first problem.
//! 3. [`Diagnostic`]: resolution still succeeds; a non-fatal fact worth
//!    surfacing (this slice: a referenced icon asset file that doesn't
//!    exist on disk — nothing consumes it yet, so it's a filesystem fact,
//!    not a schema-validity one).

use crate::location::SourceLocation;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Toml {
        path: PathBuf,
        message: String,
        location: Option<SourceLocation>,
    },
    Semantic(Vec<SemanticConfigError>),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            ConfigError::Toml {
                path,
                message,
                location: Some(location),
            } => write!(f, "{}:{location}: {message}", path.display()),
            ConfigError::Toml {
                path,
                message,
                location: None,
            } => write!(f, "{}: {message}", path.display()),
            ConfigError::Semantic(errors) => {
                for (i, error) in errors.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "{error}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Debug)]
pub enum SemanticConfigError {
    UnsupportedSchemaVersion {
        config_path: PathBuf,
        found: i64,
        supported: i64,
        location: SourceLocation,
    },
    InvalidAppVersion {
        config_path: PathBuf,
        location: SourceLocation,
    },
    LegacyNewDuplicate {
        field: &'static str,
        config_path: PathBuf,
        new_location: SourceLocation,
        legacy_manifest_path: PathBuf,
        legacy_location: SourceLocation,
    },
    InvalidWindowSize {
        config_path: PathBuf,
        field: &'static str,
        location: SourceLocation,
        reason: WindowSizeError,
    },
    WorkspaceVersionNotInherited {
        manifest_path: PathBuf,
        location: SourceLocation,
        reason: WorkspaceInheritanceError,
    },
    InvalidWebBasePath {
        config_path: PathBuf,
        location: SourceLocation,
    },
    InvalidWebIconFormat {
        config_path: PathBuf,
        field: &'static str,
        location: SourceLocation,
        allowed: &'static [&'static str],
    },
    InvalidActivation {
        config_path: PathBuf,
        location: SourceLocation,
        reason: ActivationError,
    },
    InvalidFileAssociation {
        config_path: PathBuf,
        location: SourceLocation,
        reason: FileAssociationError,
    },
    InvalidBundleField {
        config_path: PathBuf,
        field: &'static str,
        location: SourceLocation,
        reason: String,
    },
    InvalidDefaultLocale {
        config_path: PathBuf,
        requested: String,
        available: Vec<String>,
        location: SourceLocation,
    },
    InvalidLocaleTag {
        config_path: PathBuf,
        tag: String,
        location: SourceLocation,
    },
    /// `app.locales_file` names a file that does not exist -- distinct
    /// from a malformed file at a path that does exist, which is as
    /// fundamental as the main config's own malformed TOML and fails
    /// immediately rather than joining this batch.
    LocalesFileNotFound {
        config_path: PathBuf,
        referenced_path: PathBuf,
        location: SourceLocation,
    },
    /// Both the inline `[app.locales]` table and an external locales file
    /// (explicit or auto-discovered) declared entries -- exactly one
    /// source is allowed, so it's never ambiguous which one a reader
    /// should trust.
    ConflictingLocalesSource {
        config_path: PathBuf,
        external_path: PathBuf,
        location: SourceLocation,
    },
    UnknownEnvironment {
        config_path: PathBuf,
        requested: String,
        available: Vec<String>,
        /// `None` only when no `[environments]` table exists at all.
        location: Option<SourceLocation>,
    },
    DuplicateEnvironmentIdentifier {
        config_path: PathBuf,
        identifier: String,
        /// Every environment (and `"(base configuration)"` when it's one
        /// of the colliding entries) that resolves to `identifier`.
        environments: Vec<String>,
        location: SourceLocation,
    },
}

impl fmt::Display for SemanticConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticConfigError::UnsupportedSchemaVersion {
                config_path,
                found,
                supported,
                location,
            } => write!(
                f,
                "{}:{location}: unsupported schema_version {found} (this build supports {supported})",
                config_path.display()
            ),
            SemanticConfigError::InvalidAppVersion {
                config_path,
                location,
            } => write!(
                f,
                "{}:{location}: app.version must be a version string or `{{ workspace = true }}`",
                config_path.display()
            ),
            SemanticConfigError::LegacyNewDuplicate {
                field,
                config_path,
                new_location,
                legacy_manifest_path,
                legacy_location,
            } => write!(
                f,
                "{}:{new_location}: declares [{field}], but {}:{legacy_location} still declares the legacy [package.metadata.florui.dev] equivalent -- remove the legacy key once migrated",
                config_path.display(),
                legacy_manifest_path.display()
            ),
            SemanticConfigError::InvalidWindowSize {
                config_path,
                field,
                location,
                reason,
            } => write!(
                f,
                "{}:{location}: window.{field} is invalid: {reason}",
                config_path.display()
            ),
            SemanticConfigError::WorkspaceVersionNotInherited {
                manifest_path,
                location,
                reason,
            } => write!(
                f,
                "{}:{location}: app.version requests {{ workspace = true }}, but {}",
                manifest_path.display(),
                reason
            ),
            SemanticConfigError::InvalidWebBasePath {
                config_path,
                location,
            } => write!(
                f,
                "{}:{location}: web.base_path must start with \"/\"",
                config_path.display()
            ),
            SemanticConfigError::InvalidWebIconFormat {
                config_path,
                field,
                location,
                allowed,
            } => write!(
                f,
                "{}:{location}: web.icons.{field} must be one of: {}",
                config_path.display(),
                allowed.join(", ")
            ),
            SemanticConfigError::InvalidActivation {
                config_path,
                location,
                reason,
            } => write!(
                f,
                "{}:{location}: app.activation.url_schemes is invalid: {reason}",
                config_path.display()
            ),
            SemanticConfigError::InvalidFileAssociation {
                config_path,
                location,
                reason,
            } => write!(
                f,
                "{}:{location}: app.activation.file_associations entry is invalid: {reason}",
                config_path.display()
            ),
            SemanticConfigError::InvalidBundleField {
                config_path,
                field,
                location,
                reason,
            } => write!(
                f,
                "{}:{location}: bundle.{field} {reason}",
                config_path.display()
            ),
            SemanticConfigError::InvalidDefaultLocale {
                config_path,
                requested,
                available,
                location,
            } => {
                let available = if available.is_empty() {
                    "no locales are declared".to_owned()
                } else {
                    format!("available: {}", available.join(", "))
                };
                write!(
                    f,
                    "{}:{location}: app.default_locale \"{requested}\" is not declared in [app.locales] ({available})",
                    config_path.display()
                )
            }
            SemanticConfigError::InvalidLocaleTag {
                config_path,
                tag,
                location,
            } => write!(
                f,
                "{}:{location}: \"{tag}\" is not a valid locale tag",
                config_path.display()
            ),
            SemanticConfigError::LocalesFileNotFound {
                config_path,
                referenced_path,
                location,
            } => write!(
                f,
                "{}:{location}: app.locales_file points to {}, which does not exist",
                config_path.display(),
                referenced_path.display()
            ),
            SemanticConfigError::ConflictingLocalesSource {
                config_path,
                external_path,
                location,
            } => write!(
                f,
                "{}:{location}: [app.locales] is declared here, but {} also declares locales -- \
                 declare them in exactly one place",
                config_path.display(),
                external_path.display()
            ),
            SemanticConfigError::UnknownEnvironment {
                config_path,
                requested,
                available,
                location: Some(location),
            } => {
                let available = if available.is_empty() {
                    "none declared".to_owned()
                } else {
                    available.join(", ")
                };
                write!(
                    f,
                    "{}:{location}: --environment {requested} is not declared in [environments] (available: {available})",
                    config_path.display()
                )
            }
            SemanticConfigError::UnknownEnvironment {
                config_path,
                requested,
                location: None,
                ..
            } => write!(
                f,
                "{}: --environment {requested} is not declared (no [environments] table exists)",
                config_path.display()
            ),
            SemanticConfigError::DuplicateEnvironmentIdentifier {
                config_path,
                identifier,
                environments,
                location,
            } => write!(
                f,
                "{}:{location}: app.identifier \"{identifier}\" is shared by {} -- side-by-side environments must use distinct identifiers",
                config_path.display(),
                environments.join(", ")
            ),
        }
    }
}

#[derive(Debug)]
pub enum ActivationError {
    EmptyUrlScheme,
    InvalidUrlSchemeCharacters { scheme: String },
    DuplicateUrlScheme { scheme: String },
}

impl fmt::Display for ActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActivationError::EmptyUrlScheme => write!(f, "a URL scheme cannot be empty"),
            ActivationError::InvalidUrlSchemeCharacters { scheme } => write!(
                f,
                "\"{scheme}\" is not a valid URL scheme (must start with a letter, followed by letters, digits, `+`, `-`, or `.`)"
            ),
            ActivationError::DuplicateUrlScheme { scheme } => {
                write!(f, "\"{scheme}\" is declared more than once")
            }
        }
    }
}

#[derive(Debug)]
pub enum FileAssociationError {
    EmptyExtension,
    EmptyIdentity,
    DuplicateExtension { extension: String },
    DuplicateIdentity { identity: String },
}

impl fmt::Display for FileAssociationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileAssociationError::EmptyExtension => write!(f, "extension cannot be empty"),
            FileAssociationError::EmptyIdentity => write!(f, "identity cannot be empty"),
            FileAssociationError::DuplicateExtension { extension } => {
                write!(f, "extension \"{extension}\" is declared more than once")
            }
            FileAssociationError::DuplicateIdentity { identity } => {
                write!(f, "identity \"{identity}\" is declared more than once")
            }
        }
    }
}

#[derive(Debug)]
pub enum WindowSizeError {
    NotFinite,
    NotPositive,
    MinExceedsMax {
        min_field: &'static str,
        max_field: &'static str,
    },
}

impl fmt::Display for WindowSizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WindowSizeError::NotFinite => write!(f, "must be a finite number"),
            WindowSizeError::NotPositive => write!(f, "must be positive"),
            WindowSizeError::MinExceedsMax {
                min_field,
                max_field,
            } => write!(f, "{min_field} exceeds {max_field}"),
        }
    }
}

#[derive(Debug)]
pub enum WorkspaceInheritanceError {
    MissingVersionKey,
    NotInherited { found: String },
}

impl fmt::Display for WorkspaceInheritanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkspaceInheritanceError::MissingVersionKey => {
                write!(f, "its own [package] has no version key at all")
            }
            WorkspaceInheritanceError::NotInherited { found } => write!(
                f,
                "its own [package] does not inherit it from the workspace (found {found})"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub location: Option<SourceLocation>,
    pub field: &'static str,
}
