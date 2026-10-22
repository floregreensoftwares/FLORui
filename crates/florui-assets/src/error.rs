use std::fmt;
use std::path::PathBuf;

/// Every failure mode this crate's resolution/decode primitives can
/// produce. Callers that need a domain-specific error type (e.g.
/// `florui-icon`'s `IconError`) convert into their own shape rather than
/// exposing this one directly, the same way `std::io::Error` is wrapped
/// rather than re-exported by most crates that use it.
#[derive(Debug)]
pub enum AssetError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// `path` is `None` for an embedded source, which has no filesystem
    /// location to report.
    SourceTooLarge {
        path: Option<PathBuf>,
        byte_len: u64,
        limit: u64,
    },
    /// SVG text was not valid UTF-8 -- reported like any other read
    /// failure since it prevents the source from being used at all.
    InvalidUtf8 {
        path: Option<PathBuf>,
    },
    /// A network source (`http://` or `https://`) that could not be loaded.
    Fetch {
        url: String,
        error: FetchError,
    },
    Svg(resvg::usvg::Error),
    /// A raster image (PNG, JPEG, WebP or GIF) that could not be decoded.
    Raster(image::ImageError),
    /// Holding the decoded image would take what a cache retains past its budget.
    MemoryBudget {
        needed: u64,
        held: u64,
        limit: u64,
    },
    /// A requested raster target exceeds [`crate::limits::MAX_RASTER_DIMENSION`]
    /// on at least one axis.
    RasterTooLarge {
        width: u32,
        height: u32,
        limit: u32,
    },
    /// A requested raster target is zero on at least one axis -- nothing
    /// to rasterize into. Not a limit violation (see `RasterTooLarge`);
    /// this is a distinct, real caller condition (e.g. layout momentarily
    /// producing a zero-size box) rather than a hypothetical guard.
    RasterTargetEmpty {
        width: u32,
        height: u32,
    },
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssetError::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            AssetError::SourceTooLarge {
                path,
                byte_len,
                limit,
            } => write!(
                f,
                "{} is {byte_len} bytes, over the {limit}-byte limit",
                path.as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "embedded asset".to_string()),
            ),
            AssetError::InvalidUtf8 { path: Some(path) } => {
                write!(f, "{} is not valid UTF-8 text", path.display())
            }
            AssetError::InvalidUtf8 { path: None } => write!(f, "asset is not valid UTF-8 text"),
            AssetError::Fetch { url, error } => write!(f, "could not load {url}: {error}"),
            AssetError::Svg(err) => write!(f, "could not parse SVG: {err}"),
            AssetError::Raster(err) => write!(f, "could not decode the image: {err}"),
            AssetError::MemoryBudget {
                needed,
                held,
                limit,
            } => write!(
                f,
                "the image needs {needed} bytes and the cache already holds {held} of its {limit}"
            ),
            AssetError::RasterTooLarge {
                width,
                height,
                limit,
            } => write!(
                f,
                "requested raster size {width}x{height} exceeds the {limit}px-per-side limit"
            ),
            AssetError::RasterTargetEmpty { width, height } => write!(
                f,
                "requested raster size {width}x{height} has no area to rasterize into"
            ),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AssetError::Io { source, .. } => Some(source),
            AssetError::Svg(err) => Some(err),
            AssetError::Raster(err) => Some(err),
            AssetError::Fetch { error, .. } => Some(error),
            AssetError::SourceTooLarge { .. }
            | AssetError::InvalidUtf8 { .. }
            | AssetError::MemoryBudget { .. }
            | AssetError::RasterTooLarge { .. }
            | AssetError::RasterTargetEmpty { .. } => None,
        }
    }
}

/// Why a network source did not load. Plain data and cheap to clone, because one
/// request can answer several waiters; it exists with or without the `net` feature, so
/// a consumer can name it without a conditional build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// This build has no network loading (the `net` feature of the crate is off).
    Unavailable,
    /// Not a URL this crate can request: it does not parse, has no host, or carries a
    /// user name or password.
    InvalidUrl(String),
    /// A scheme other than `http` and `https`, in the URL or in a redirect.
    UnsupportedScheme(String),
    /// The server answered with a status that is not a success.
    Status(u16),
    /// More redirects than the configured limit.
    TooManyRedirects { limit: u32 },
    /// A redirect from `https` to `http`, refused.
    InsecureRedirect { to: String },
    /// A connection, a response or a body took longer than its limit.
    Timeout,
    /// The body is, or grew past, the size limit. Checked from the declared length and
    /// again while the body streams in, so a server that lies about it is cut off.
    TooLarge { limit: u64 },
    /// The response is not an image this crate decodes: its media type is not an image
    /// type, or it is an image type with no decoder here, or the bytes are none of them.
    NotAnImage { content_type: String },
    /// The media type says one format and the bytes are another.
    ContentTypeMismatch {
        declared: String,
        found: &'static str,
    },
    /// The request was cancelled before it finished.
    Cancelled,
    /// Resolving the host, connecting, the TLS handshake or reading failed.
    Transport(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Unavailable => {
                write!(
                    f,
                    "this build cannot load from the network (the `net` feature is off)"
                )
            }
            FetchError::InvalidUrl(reason) => write!(f, "not a usable URL: {reason}"),
            FetchError::UnsupportedScheme(scheme) => {
                write!(
                    f,
                    "the scheme `{scheme}` is not loaded, only http and https"
                )
            }
            FetchError::Status(code) => write!(f, "the server answered with status {code}"),
            FetchError::TooManyRedirects { limit } => {
                write!(f, "more than {limit} redirects")
            }
            FetchError::InsecureRedirect { to } => {
                write!(f, "a redirect from https to {to} was refused")
            }
            FetchError::Timeout => write!(f, "the request timed out"),
            FetchError::TooLarge { limit } => {
                write!(f, "the response is over the {limit}-byte limit")
            }
            FetchError::NotAnImage { content_type } => {
                write!(f, "the response is not a supported image ({content_type})")
            }
            FetchError::ContentTypeMismatch { declared, found } => {
                write!(f, "the response says {declared} but its bytes are {found}")
            }
            FetchError::Cancelled => write!(f, "the request was cancelled"),
            FetchError::Transport(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for FetchError {}
