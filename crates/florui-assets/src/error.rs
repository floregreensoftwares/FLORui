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
    Svg(resvg::usvg::Error),
    Png(image::ImageError),
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
            AssetError::Svg(err) => write!(f, "could not parse SVG: {err}"),
            AssetError::Png(err) => write!(f, "could not decode PNG: {err}"),
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
            AssetError::Png(err) => Some(err),
            AssetError::SourceTooLarge { .. }
            | AssetError::InvalidUtf8 { .. }
            | AssetError::RasterTooLarge { .. }
            | AssetError::RasterTargetEmpty { .. } => None,
        }
    }
}
