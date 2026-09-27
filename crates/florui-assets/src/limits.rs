//! Explicit ceilings applied before an untrusted source is decoded or
//! rasterized, so a corrupt or hostile asset (a huge file, an SVG whose
//! declared size implies an unreasonable pixel buffer) fails with a
//! diagnostic instead of exhausting memory. These are deliberately
//! generous defaults for real UI assets, not tuned against a specific
//! deployment -- a consumer with tighter requirements can check its own
//! bound before calling into this crate at all.

use std::path::Path;

use crate::AssetError;

/// Ceiling on a single source asset's byte size, checked before it is
/// read into memory. 64 MiB comfortably covers any real UI image or icon
/// asset; nothing in this project's own fixtures or examples comes close.
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Ceiling on a single rasterized dimension (width or height, in pixels).
/// 8192 matches a common conservative GPU 2D-texture size limit and is far
/// beyond any on-screen control or content image this engine paints
/// today.
pub const MAX_RASTER_DIMENSION: u32 = 8192;

pub(crate) fn check_source_len(byte_len: u64, path: Option<&Path>) -> Result<(), AssetError> {
    if byte_len > MAX_SOURCE_BYTES {
        return Err(AssetError::SourceTooLarge {
            path: path.map(Path::to_owned),
            byte_len,
            limit: MAX_SOURCE_BYTES,
        });
    }
    Ok(())
}

pub(crate) fn check_raster_size(width: u32, height: u32) -> Result<(), AssetError> {
    if width > MAX_RASTER_DIMENSION || height > MAX_RASTER_DIMENSION {
        return Err(AssetError::RasterTooLarge {
            width,
            height,
            limit: MAX_RASTER_DIMENSION,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_source_len_accepts_a_size_at_the_limit() {
        assert!(check_source_len(MAX_SOURCE_BYTES, None).is_ok());
    }

    #[test]
    fn check_source_len_rejects_one_byte_over_the_limit() {
        let err = check_source_len(MAX_SOURCE_BYTES + 1, None).unwrap_err();
        assert!(matches!(err, AssetError::SourceTooLarge { .. }));
    }

    #[test]
    fn check_raster_size_accepts_a_dimension_at_the_limit() {
        assert!(check_raster_size(MAX_RASTER_DIMENSION, MAX_RASTER_DIMENSION).is_ok());
    }

    #[test]
    fn check_raster_size_rejects_one_axis_over_the_limit() {
        let err = check_raster_size(MAX_RASTER_DIMENSION + 1, 64).unwrap_err();
        assert!(matches!(err, AssetError::RasterTooLarge { .. }));
    }
}
