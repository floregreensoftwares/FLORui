//! Raster (non-vector) image decoding, via the `image` crate: PNG, JPEG, WebP
//! and GIF (the first frame of an animated one). Adding another format is a
//! `Cargo.toml` feature flag plus a name in [`Format`], not a new architecture.
//!
//! A decoded image keeps what painting needs next to its pixels, so that a
//! page with a great many of them draws each without work per frame: the
//! premultiplied pixels (shared with the straight ones when nothing is
//! translucent, which is what a photograph is) and a chain of smaller
//! copies, each half the size of the one before, from which a picture drawn
//! much smaller than it is takes the nearest one (see [`RasterImage::level_for`]).

use std::io::Cursor;
use std::sync::OnceLock;

use image::{ImageDecoder, ImageReader};

use crate::error::AssetError;

/// The raster formats that are decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl Format {
    /// The format a file name's extension names (case-insensitive).
    pub fn of_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" | "jpe" | "jfif" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "gif" => Some(Self::Gif),
            _ => None,
        }
    }

    /// The format a media type (`image/jpeg`, `image/webp`...) names.
    pub fn of_media_type(media: &str) -> Option<Self> {
        let media = media.to_ascii_lowercase();
        if media.contains("png") {
            Some(Self::Png)
        } else if media.contains("jpeg") || media.contains("jpg") {
            Some(Self::Jpeg)
        } else if media.contains("webp") {
            Some(Self::Webp)
        } else if media.contains("gif") {
            Some(Self::Gif)
        } else {
            None
        }
    }

    /// The format the first bytes of a file say it is.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }
}

/// Decoded, straight (non-premultiplied) 32bpp RGBA pixels, tightly
/// packed row-major -- `rgba.len() == (width * height * 4) as usize`.
/// The common output shape both raster (PNG, JPEG, WebP, GIF) and
/// rasterized-vector (SVG, see `svg.rs`) sources produce.
#[derive(Clone)]
pub struct RasterImage {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// What painting reads, made the first time it is asked for.
    paint: OnceLock<PaintLevels>,
}

impl std::fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl PartialEq for RasterImage {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.rgba == other.rgba
    }
}

/// The pixels of an image as painting wants them.
#[derive(Clone)]
struct PaintLevels {
    /// The premultiplied pixels of the image itself, when some of them are
    /// translucent; `None` when none are, and the straight pixels already are the
    /// premultiplied ones.
    base: Option<Vec<u8>>,
    /// Each copy half the size of the one before (rounded up), down to one pixel.
    smaller: Vec<(u32, u32, Vec<u8>)>,
}

/// One size of an image, premultiplied, for painting.
#[derive(Debug, Clone, Copy)]
pub struct Level<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

/// Images smaller than this on both sides have no smaller copies: they are
/// icons, drawn near their own size or not much smaller.
const MIPS_FROM: u32 = 96;

impl RasterImage {
    /// An image of `width x height` pixels, `rgba` being straight, row-major.
    pub fn new(rgba: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            rgba,
            width,
            height,
            paint: OnceLock::new(),
        }
    }

    /// Makes what painting will read now, off the thread that paints. Calling
    /// it is never needed for correctness: painting makes it on first use.
    pub fn prepare(&self) {
        self.paint_levels();
    }

    fn paint_levels(&self) -> &PaintLevels {
        self.paint.get_or_init(|| {
            let opaque = self.rgba.chunks_exact(4).all(|pixel| pixel[3] == 255);
            let base = (!opaque).then(|| premultiplied(&self.rgba));
            let mut smaller: Vec<(u32, u32, Vec<u8>)> = Vec::new();
            if self.width >= MIPS_FROM || self.height >= MIPS_FROM {
                let mut next = halved(
                    base.as_deref().unwrap_or(&self.rgba),
                    self.width,
                    self.height,
                );
                loop {
                    let (pixels, width, height) = next;
                    let done = width <= 1 && height <= 1;
                    next = if done {
                        smaller.push((width, height, pixels));
                        break;
                    } else {
                        let following = halved(&pixels, width, height);
                        smaller.push((width, height, pixels));
                        following
                    };
                }
            }
            PaintLevels { base, smaller }
        })
    }

    /// The size of this image to paint it at `width x height` pixels: the
    /// smallest copy that is not smaller than that, so that what is left to
    /// scale is less than a half, which a bilinear blit does without losing
    /// anything.
    pub fn level_for(&self, width: u32, height: u32) -> Level<'_> {
        let levels = self.paint_levels();
        let base = Level {
            width: self.width,
            height: self.height,
            pixels: levels.base.as_deref().unwrap_or(&self.rgba),
        };
        if width == 0 || height == 0 {
            return base;
        }
        let mut chosen = base;
        for (level_width, level_height, pixels) in &levels.smaller {
            if *level_width < width || *level_height < height {
                break;
            }
            chosen = Level {
                width: *level_width,
                height: *level_height,
                pixels,
            };
        }
        chosen
    }
}

/// `rgba`, straight, as premultiplied pixels.
fn premultiplied(rgba: &[u8]) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for pixel in out.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    out
}

/// `pixels` (premultiplied, `width x height`) at half the size, each pixel the
/// average of the up to four it replaces.
fn halved(pixels: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let (new_width, new_height) = (width.div_ceil(2).max(1), height.div_ceil(2).max(1));
    let (width, height) = (width as usize, height as usize);
    let mut out = vec![0u8; new_width as usize * new_height as usize * 4];
    for y in 0..new_height as usize {
        let rows = [y * 2, (y * 2 + 1).min(height - 1)];
        for x in 0..new_width as usize {
            let columns = [x * 2, (x * 2 + 1).min(width - 1)];
            let mut sum = [0u32; 4];
            for row in rows {
                for column in columns {
                    let at = (row * width + column) * 4;
                    for (channel, total) in sum.iter_mut().enumerate() {
                        *total += u32::from(pixels[at + channel]);
                    }
                }
            }
            let at = (y * new_width as usize + x) * 4;
            for (channel, total) in sum.iter().enumerate() {
                out[at + channel] = ((total + 2) / 4) as u8;
            }
        }
    }
    (out, new_width, new_height)
}

/// Decodes `bytes` -- a PNG, a JPEG, a WebP or a GIF, told by what they start with --
/// at its own native resolution: a raster format has no analogue to an SVG's
/// arbitrary-size rasterization, so there is no target-size parameter here. Pure --
/// takes no path, touches no disk. The size is read from the header and checked before
/// any pixel is, so an image that is too large costs nothing; a JPEG is turned the way its
/// Exif orientation says, as a browser does; an animated GIF or WebP gives its first frame.
pub fn decode_raster(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| AssetError::Raster(image::ImageError::IoError(error)))?;
    let mut decoder = reader.into_decoder().map_err(AssetError::Raster)?;
    let (width, height) = decoder.dimensions();
    crate::limits::check_raster_size(width, height)?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(crate::limits::MAX_DECODE_BYTES);
    decoder.set_limits(limits).map_err(AssetError::Raster)?;
    let mut decoded = image::DynamicImage::from_decoder(decoder).map_err(AssetError::Raster)?;
    decoded.apply_orientation(orientation);
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(RasterImage::new(rgba.into_raw(), width, height))
}

/// [`decode_raster`], under the name it had when PNG was the only format.
pub fn decode_png(bytes: &[u8]) -> Result<RasterImage, AssetError> {
    decode_raster(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_test_image(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let image = image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([x as u8, y as u8, 128, 255])
        });
        let mut bytes = Vec::new();
        let dynamic = image::DynamicImage::ImageRgba8(image);
        let dynamic = match format {
            // JPEG has no alpha.
            image::ImageFormat::Jpeg => image::DynamicImage::ImageRgb8(dynamic.to_rgb8()),
            _ => dynamic,
        };
        dynamic
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    fn encode_test_png(width: u32, height: u32) -> Vec<u8> {
        encode_test_image(width, height, image::ImageFormat::Png)
    }

    #[test]
    fn decode_png_reports_the_real_dimensions() {
        let bytes = encode_test_png(17, 9);
        let image = decode_raster(&bytes).unwrap();
        assert_eq!((image.width, image.height), (17, 9));
        assert_eq!(image.rgba.len(), 17 * 9 * 4);
    }

    #[test]
    fn decode_rejects_corrupt_bytes() {
        let err = decode_raster(&[0, 1, 2, 3, 4]).unwrap_err();
        assert!(matches!(err, AssetError::Raster(_)));
    }

    #[test]
    fn decode_png_round_trips_pixel_values() {
        let bytes = encode_test_png(4, 4);
        let image = decode_raster(&bytes).unwrap();
        let offset = ((2 * 4 + 3) * 4) as usize;
        assert_eq!(&image.rgba[offset..offset + 4], &[3, 2, 128, 255]);
    }

    #[test]
    fn a_jpeg_decodes_to_its_size_and_close_to_its_colors() {
        let bytes = encode_test_image(32, 24, image::ImageFormat::Jpeg);
        assert_eq!(Format::sniff(&bytes), Some(Format::Jpeg));
        let image = decode_raster(&bytes).unwrap();
        assert_eq!((image.width, image.height), (32, 24));
        let offset = ((12 * 32 + 16) * 4) as usize;
        let pixel = &image.rgba[offset..offset + 4];
        assert!(pixel[0].abs_diff(16) < 24, "{pixel:?}");
        assert!(pixel[1].abs_diff(12) < 24, "{pixel:?}");
        assert!(pixel[2].abs_diff(128) < 24, "{pixel:?}");
        assert_eq!(pixel[3], 255);
    }

    #[test]
    fn a_webp_decodes_to_the_pixels_it_was_made_from() {
        let bytes = encode_test_image(10, 6, image::ImageFormat::WebP);
        assert_eq!(Format::sniff(&bytes), Some(Format::Webp));
        let image = decode_raster(&bytes).unwrap();
        assert_eq!((image.width, image.height), (10, 6));
        let offset = ((3 * 10 + 7) * 4) as usize;
        assert_eq!(&image.rgba[offset..offset + 4], &[7, 3, 128, 255]);
    }

    #[test]
    fn a_gif_decodes_its_first_frame() {
        let bytes = encode_test_image(8, 8, image::ImageFormat::Gif);
        assert_eq!(Format::sniff(&bytes), Some(Format::Gif));
        let image = decode_raster(&bytes).unwrap();
        assert_eq!((image.width, image.height), (8, 8));
        assert_eq!(image.rgba[3], 255);
    }

    #[test]
    fn the_format_is_told_by_the_name_the_media_type_and_the_first_bytes() {
        assert_eq!(Format::of_extension("JPG"), Some(Format::Jpeg));
        assert_eq!(Format::of_extension("jpeg"), Some(Format::Jpeg));
        assert_eq!(Format::of_extension("webp"), Some(Format::Webp));
        assert_eq!(Format::of_extension("gif"), Some(Format::Gif));
        assert_eq!(Format::of_extension("bmp"), None);
        assert_eq!(Format::of_media_type("image/jpeg"), Some(Format::Jpeg));
        assert_eq!(Format::of_media_type("image/webp"), Some(Format::Webp));
        assert_eq!(Format::sniff(b"not an image"), None);
    }

    #[test]
    fn an_image_too_large_is_refused_from_its_header() {
        // A 1x20000 PNG header with no pixel data after it: the size says no
        // before anything is allocated or decoded.
        let mut bytes = encode_test_png(1, 1);
        let height = 20_000u32.to_be_bytes();
        bytes[20..24].copy_from_slice(&height);
        let error = decode_raster(&bytes).unwrap_err();
        assert!(
            matches!(
                error,
                AssetError::RasterTooLarge { .. } | AssetError::Raster(_)
            ),
            "{error}"
        );
    }

    #[test]
    fn an_opaque_image_paints_from_its_own_pixels_and_a_translucent_one_from_a_copy() {
        let opaque = RasterImage::new([10u8, 20, 30, 255].repeat(4), 2, 2);
        assert_eq!(opaque.level_for(2, 2).pixels.as_ptr(), opaque.rgba.as_ptr());
        let half = RasterImage::new([200, 100, 50, 128].repeat(4), 2, 2);
        let level = half.level_for(2, 2);
        assert_ne!(level.pixels.as_ptr(), half.rgba.as_ptr());
        assert_eq!(&level.pixels[..4], &[100, 50, 25, 128]);
    }

    #[test]
    fn a_large_image_is_painted_from_the_smallest_copy_that_is_not_smaller_than_the_target() {
        let image = RasterImage::new([90u8, 90, 90, 255].repeat(400 * 300), 400, 300);
        let size = |w, h| {
            let level = image.level_for(w, h);
            (level.width, level.height)
        };
        assert_eq!(size(400, 300), (400, 300));
        assert_eq!(
            size(399, 299),
            (400, 300),
            "no copy between the two is large enough"
        );
        assert_eq!(size(200, 150), (200, 150));
        assert_eq!(size(120, 90), (200, 150));
        assert_eq!(size(100, 75), (100, 75));
        assert_eq!(size(1, 1), (1, 1));
        assert_eq!(size(800, 600), (400, 300), "never larger than the image");
        let small = image.level_for(100, 75);
        assert_eq!(small.pixels.len(), 100 * 75 * 4);
        assert_eq!(
            &small.pixels[..4],
            &[90, 90, 90, 255],
            "an average of the same value"
        );
    }

    #[test]
    fn an_icon_has_no_smaller_copies() {
        let icon = RasterImage::new(vec![1; 32 * 32 * 4], 32, 32);
        let level = icon.level_for(4, 4);
        assert_eq!((level.width, level.height), (32, 32));
    }

    #[test]
    fn halving_averages_whole_blocks_and_keeps_an_odd_edge() {
        let pixels = [
            [0u8, 0, 0, 255],
            [100, 100, 100, 255],
            [200, 200, 200, 255],
            [255, 255, 255, 255],
        ]
        .concat();
        let (half, w, h) = halved(&pixels, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(&half[..4], &[139, 139, 139, 255]);
        let (odd, w, h) = halved(&[10; 12], 3, 1);
        assert_eq!((w, h), (2, 1));
        assert_eq!(&odd[..4], &[10, 10, 10, 10]);
    }
}
