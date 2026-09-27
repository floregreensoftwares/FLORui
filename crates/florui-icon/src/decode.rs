use crate::RawIcon;
use std::fmt;
use std::path::{Path, PathBuf};

/// `resvg`/`image` stay direct dependencies of this crate only so their
/// error types (`Svg`/`Png` below) are nameable here -- the actual
/// parsing/decoding/rasterization logic lives in `florui-assets` and is
/// not duplicated in this crate; see that crate's own doc for why `resvg`
/// was kept rather than re-evaluated.
#[derive(Debug)]
pub enum IconError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    UnsupportedFormat {
        path: PathBuf,
        extension: Option<String>,
    },
    Svg(resvg::usvg::Error),
    /// Preserved for API compatibility with earlier versions of this
    /// crate; never constructed today. `florui_assets::rasterize_svg`'s
    /// own doc explains why: `usvg` represents a tree's size as
    /// `NonZeroPositiveF32` and rejects an explicitly zero-sized document
    /// at parse time, so no successfully parsed SVG can reach this crate
    /// with a zero intrinsic size.
    SvgEmptySize {
        path: Option<PathBuf>,
    },
    Png(image::ImageError),
    /// A source file exceeded `florui_assets::limits::MAX_SOURCE_BYTES`.
    SourceTooLarge {
        path: PathBuf,
        byte_len: u64,
        limit: u64,
    },
    /// An `.svg` file's bytes were not valid UTF-8.
    InvalidUtf8 {
        path: PathBuf,
    },
    /// The requested `size` exceeds
    /// `florui_assets::limits::MAX_RASTER_DIMENSION`. `path` is `None` for
    /// [`decode_svg`] (no file involved).
    SizeTooLarge {
        path: Option<PathBuf>,
        size: u32,
        limit: u32,
    },
    /// The requested `size` is zero -- nothing to rasterize into. `path`
    /// is `None` for [`decode_svg`] (no file involved).
    SizeZero {
        path: Option<PathBuf>,
    },
}

impl fmt::Display for IconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IconError::Io { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            IconError::UnsupportedFormat { path, extension } => write!(
                f,
                "{} has an unsupported icon extension ({}) -- only .svg and .png are supported",
                path.display(),
                extension.as_deref().unwrap_or("none")
            ),
            IconError::Svg(err) => write!(f, "could not parse SVG: {err}"),
            IconError::SvgEmptySize { path: Some(path) } => {
                write!(f, "{} has no visible content to rasterize", path.display())
            }
            IconError::SvgEmptySize { path: None } => {
                write!(f, "SVG has no visible content to rasterize")
            }
            IconError::Png(err) => write!(f, "could not decode PNG: {err}"),
            IconError::SourceTooLarge {
                path,
                byte_len,
                limit,
            } => write!(
                f,
                "{} is {byte_len} bytes, over the {limit}-byte limit",
                path.display()
            ),
            IconError::InvalidUtf8 { path } => {
                write!(f, "{} is not valid UTF-8 text", path.display())
            }
            IconError::SizeTooLarge { path, size, limit } => write!(
                f,
                "requested icon size {size}px ({}) exceeds the {limit}px limit",
                path.as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "in-memory SVG".to_string())
            ),
            IconError::SizeZero { path } => write!(
                f,
                "requested icon size is zero ({})",
                path.as_deref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "in-memory SVG".to_string())
            ),
        }
    }
}

impl std::error::Error for IconError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IconError::Io { source, .. } => Some(source),
            IconError::Svg(err) => Some(err),
            IconError::Png(err) => Some(err),
            IconError::UnsupportedFormat { .. }
            | IconError::SvgEmptySize { .. }
            | IconError::SourceTooLarge { .. }
            | IconError::InvalidUtf8 { .. }
            | IconError::SizeTooLarge { .. }
            | IconError::SizeZero { .. } => None,
        }
    }
}

/// Converts a `florui-assets` failure into this crate's own error type.
/// `path` is `None` for the pure, no-disk functions ([`decode_svg`],
/// [`decode_png`]) and `Some` for [`load_icon_at_size`].
fn from_asset_error(err: florui_assets::AssetError, path: Option<&Path>) -> IconError {
    let owned_path = || path.map(Path::to_owned);
    match err {
        florui_assets::AssetError::Io { path, source } => IconError::Io { path, source },
        florui_assets::AssetError::Svg(err) => IconError::Svg(err),
        florui_assets::AssetError::Png(err) => IconError::Png(err),
        florui_assets::AssetError::SourceTooLarge {
            byte_len, limit, ..
        } => IconError::SourceTooLarge {
            // Only reachable via `load_icon_at_size`, which always passes
            // `Some(path)` -- `decode_svg`/`decode_png` never call a
            // `read_bytes`/`read_text` path that can produce this.
            path: owned_path().expect("SourceTooLarge only originates from a real file read"),
            byte_len,
            limit,
        },
        florui_assets::AssetError::InvalidUtf8 { .. } => IconError::InvalidUtf8 {
            path: owned_path().expect("InvalidUtf8 only originates from a real file read"),
        },
        florui_assets::AssetError::RasterTooLarge { width, limit, .. } => IconError::SizeTooLarge {
            path: owned_path(),
            size: width,
            limit,
        },
        florui_assets::AssetError::RasterTargetEmpty { .. } => {
            IconError::SizeZero { path: owned_path() }
        }
    }
}

/// Rasterizes `svg_text` to exactly `size` x `size` RGBA pixels, scaled
/// uniformly to fit the SVG's own intrinsic size (aspect-preserving,
/// centered within the square). Pure -- takes no path, touches no disk.
pub fn decode_svg(svg_text: &str, size: u32) -> Result<RawIcon, IconError> {
    let image = florui_assets::parse_svg(svg_text).map_err(|err| from_asset_error(err, None))?;
    let raster = florui_assets::rasterize_svg(
        &image,
        florui_assets::RasterFit::Contain {
            width: size,
            height: size,
        },
    )
    .map_err(|err| from_asset_error(err, None))?;
    Ok(RawIcon {
        rgba: raster.rgba,
        width: raster.width,
        height: raster.height,
    })
}

/// Decodes `bytes` as a PNG at its own native resolution -- `size` has no
/// PNG analogue (no vector content to rasterize at an arbitrary size).
/// Pure -- takes no path, touches no disk.
pub fn decode_png(bytes: &[u8]) -> Result<RawIcon, IconError> {
    let raster = florui_assets::decode_png(bytes).map_err(|err| from_asset_error(err, None))?;
    Ok(RawIcon {
        rgba: raster.rgba,
        width: raster.width,
        height: raster.height,
    })
}

/// Reads and decodes `path`, dispatching on its extension (`.svg`/`.png`,
/// case-insensitive). `size` only affects SVG rasterization.
pub fn load_icon_at_size(path: &Path, size: u32) -> Result<RawIcon, IconError> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("svg") => {
            let source = florui_assets::AssetSource::Path(path);
            let text = source
                .read_text()
                .map_err(|err| from_asset_error(err, Some(path)))?;
            let image =
                florui_assets::parse_svg(&text).map_err(|err| from_asset_error(err, Some(path)))?;
            let raster = florui_assets::rasterize_svg(
                &image,
                florui_assets::RasterFit::Contain {
                    width: size,
                    height: size,
                },
            )
            .map_err(|err| from_asset_error(err, Some(path)))?;
            Ok(RawIcon {
                rgba: raster.rgba,
                width: raster.width,
                height: raster.height,
            })
        }
        Some("png") => {
            let source = florui_assets::AssetSource::Path(path);
            let bytes = source
                .read_bytes()
                .map_err(|err| from_asset_error(err, Some(path)))?;
            let raster = florui_assets::decode_png(&bytes)
                .map_err(|err| from_asset_error(err, Some(path)))?;
            Ok(RawIcon {
                rgba: raster.rgba,
                width: raster.width,
                height: raster.height,
            })
        }
        _ => Err(IconError::UnsupportedFormat {
            path: path.to_owned(),
            extension,
        }),
    }
}

/// [`load_icon_at_size`] at [`crate::DEFAULT_ICON_SIZE`].
pub fn load_icon(path: &Path) -> Result<RawIcon, IconError> {
    load_icon_at_size(path, crate::DEFAULT_ICON_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUE_CIRCLE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <circle cx="16" cy="16" r="14" fill="#3366ff"/>
</svg>"##;

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        rgba[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn decode_svg_rasterizes_to_the_requested_size() {
        let icon = decode_svg(BLUE_CIRCLE_SVG, 64).unwrap();
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
    }

    #[test]
    fn decode_svg_unpremultiplies_a_known_fill_color_at_center() {
        let icon = decode_svg(BLUE_CIRCLE_SVG, 64).unwrap();
        let [r, g, b, a] = pixel(&icon.rgba, 64, 32, 32);
        assert_eq!(a, 255, "center of the circle should be fully opaque");
        assert!(
            b > r && b > g,
            "center should be blue-dominant, got {r},{g},{b}"
        );
    }

    #[test]
    fn decode_svg_leaves_a_fully_transparent_pixel_at_alpha_zero() {
        let icon = decode_svg(BLUE_CIRCLE_SVG, 64).unwrap();
        let [_, _, _, a] = pixel(&icon.rgba, 64, 0, 0);
        assert_eq!(a, 0, "outside the circle should be transparent");
    }

    #[test]
    fn decode_svg_rejects_unparseable_markup() {
        let err = decode_svg("not an svg at all", 64).unwrap_err();
        assert!(matches!(err, IconError::Svg(_)));
    }

    #[test]
    fn decode_svg_rejects_a_zero_size() {
        let err = decode_svg(BLUE_CIRCLE_SVG, 0).unwrap_err();
        assert!(matches!(err, IconError::SizeZero { path: None }));
    }

    fn encode_test_png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([x as u8, y as u8, 128, 255])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn decode_png_reports_the_real_dimensions() {
        let bytes = encode_test_png(17, 9);
        let icon = decode_png(&bytes).unwrap();
        assert_eq!((icon.width, icon.height), (17, 9));
        assert_eq!(icon.rgba.len(), 17 * 9 * 4);
    }

    #[test]
    fn decode_png_rejects_corrupt_bytes() {
        let err = decode_png(&[0, 1, 2, 3, 4]).unwrap_err();
        assert!(matches!(err, IconError::Png(_)));
    }

    #[test]
    fn load_icon_at_size_dispatches_svg_by_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, BLUE_CIRCLE_SVG).unwrap();
        let icon = load_icon_at_size(&path, 48).unwrap();
        assert_eq!((icon.width, icon.height), (48, 48));
    }

    #[test]
    fn load_icon_at_size_dispatches_png_by_extension_and_ignores_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.PNG"); // case-insensitive extension
        std::fs::write(&path, encode_test_png(10, 10)).unwrap();
        let icon = load_icon_at_size(&path, 999).unwrap();
        assert_eq!((icon.width, icon.height), (10, 10));
    }

    #[test]
    fn load_icon_at_size_rejects_an_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.bmp");
        std::fs::write(&path, b"whatever").unwrap();
        let err = load_icon_at_size(&path, 64).unwrap_err();
        assert!(matches!(err, IconError::UnsupportedFormat { .. }));
    }

    #[test]
    fn load_icon_at_size_reports_a_missing_file_as_io_error() {
        let err = load_icon_at_size(Path::new("does/not/exist.svg"), 64).unwrap_err();
        assert!(matches!(err, IconError::Io { .. }));
    }

    #[test]
    fn load_icon_defaults_to_the_standard_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, BLUE_CIRCLE_SVG).unwrap();
        let icon = load_icon(&path).unwrap();
        assert_eq!(
            (icon.width, icon.height),
            (crate::DEFAULT_ICON_SIZE, crate::DEFAULT_ICON_SIZE)
        );
    }

    #[test]
    fn load_icon_at_size_rejects_a_source_file_over_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.svg");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(florui_assets::limits::MAX_SOURCE_BYTES + 1)
            .unwrap();
        let err = load_icon_at_size(&path, 64).unwrap_err();
        assert!(matches!(err, IconError::SourceTooLarge { .. }));
    }
}
