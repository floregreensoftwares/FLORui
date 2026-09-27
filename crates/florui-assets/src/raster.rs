//! Raster (non-vector) image decoding, via the `image` crate. Only PNG is
//! enabled today (see this crate's own top-level doc on format scope) --
//! adding another raster format is a `Cargo.toml` feature flag plus a
//! dispatch arm here, not a new architecture.

use crate::error::AssetError;

/// Decoded, straight (non-premultiplied) 32bpp RGBA pixels, tightly
/// packed row-major -- `rgba.len() == (width * height * 4) as usize`.
/// The common output shape both raster (PNG) and rasterized-vector (SVG,
/// see `svg.rs`) sources produce.
#[derive(Debug, Clone, PartialEq)]
pub struct RasterImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Decodes `bytes` as a PNG at its own native resolution -- a raster
/// format has no analogue to an SVG's arbitrary-size rasterization, so
/// there is no target-size parameter here. Pure -- takes no path, touches
/// no disk.
pub fn decode_png(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let image = image::load_from_memory(bytes)
        .map_err(AssetError::Png)?
        .to_rgba8();
    let (width, height) = image.dimensions();
    crate::limits::check_raster_size(width, height)?;
    Ok(RasterImage {
        rgba: image.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let image = decode_png(&bytes).unwrap();
        assert_eq!((image.width, image.height), (17, 9));
        assert_eq!(image.rgba.len(), 17 * 9 * 4);
    }

    #[test]
    fn decode_png_rejects_corrupt_bytes() {
        let err = decode_png(&[0, 1, 2, 3, 4]).unwrap_err();
        assert!(matches!(err, AssetError::Png(_)));
    }

    #[test]
    fn decode_png_round_trips_pixel_values() {
        let bytes = encode_test_png(4, 4);
        let image = decode_png(&bytes).unwrap();
        let offset = ((2 * 4 + 3) * 4) as usize;
        assert_eq!(&image.rgba[offset..offset + 4], &[3, 2, 128, 255]);
    }
}
