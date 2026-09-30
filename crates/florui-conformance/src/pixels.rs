//! Per-pixel comparison between a Chromium reference capture and Florui's
//! own render: dimension/alpha checks, percent/mean/max error, and a diff
//! image, per this project's visual-comparison criteria.

use std::fmt;

use image::{Rgba as ImageRgba, RgbaImage};
use serde::{Deserialize, Serialize};

/// Per-channel and alpha tolerance, in 0-255 units. `0`/`0` matches the rule
/// that fixtures classified as exact require zero differing pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelDiffOptions {
    pub channel_tolerance: u8,
    pub alpha_tolerance: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PixelSummary {
    pub differing_pixels: u64,
    pub total_pixels: u64,
    pub percent_different: f64,
    pub mean_error: f64,
    pub max_error: u8,
}

impl PixelSummary {
    pub fn is_exact_match(&self) -> bool {
        self.differing_pixels == 0
    }
}

/// The smallest box, in physical pixels, that holds every differing pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct PixelReport {
    pub summary: PixelSummary,
    /// Where the differing pixels are; `None` when nothing differs.
    pub diff_region: Option<PixelRegion>,
    /// Transparent where pixels match, opaque red where they differ.
    pub diff_image: RgbaImage,
}

#[derive(Debug)]
pub enum PixelCompareError {
    DimensionMismatch {
        reference: (u32, u32),
        result: (u32, u32),
    },
}

impl fmt::Display for PixelCompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PixelCompareError::DimensionMismatch { reference, result } => write!(
                f,
                "image dimensions differ: reference is {}x{}, result is {}x{}",
                reference.0, reference.1, result.0, result.1
            ),
        }
    }
}

impl std::error::Error for PixelCompareError {}

/// Compares `reference` (Chromium capture) against `result` (Florui's own
/// render) pixel by pixel. Dimensions and alpha are checked explicitly,
/// rather than folded silently into a single color distance metric.
///
/// Both images are treated as plain sRGB byte buffers with no color-profile
/// normalization — a documented bootstrap limitation. No fixture with a
/// non-default color profile should be added until that normalization
/// exists.
pub fn compare_pixels(
    reference: &RgbaImage,
    result: &RgbaImage,
    options: &PixelDiffOptions,
) -> Result<PixelReport, PixelCompareError> {
    if reference.dimensions() != result.dimensions() {
        return Err(PixelCompareError::DimensionMismatch {
            reference: reference.dimensions(),
            result: result.dimensions(),
        });
    }

    let (width, height) = reference.dimensions();
    let total_pixels = u64::from(width) * u64::from(height);
    let mut differing_pixels = 0u64;
    let mut error_sum = 0f64;
    let mut max_error = 0u8;
    let mut diff_image = RgbaImage::new(width, height);
    let mut bounds: Option<(u32, u32, u32, u32)> = None;

    for (x, y, reference_pixel) in reference.enumerate_pixels() {
        let result_pixel = result.get_pixel(x, y);
        let channel_error = reference_pixel.0[..3]
            .iter()
            .zip(result_pixel.0[..3].iter())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let alpha_error = reference_pixel.0[3].abs_diff(result_pixel.0[3]);

        let differs =
            channel_error > options.channel_tolerance || alpha_error > options.alpha_tolerance;
        if differs {
            differing_pixels += 1;
            diff_image.put_pixel(x, y, ImageRgba([255, 0, 0, 255]));
            bounds = Some(match bounds {
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                None => (x, y, x, y),
            });
        } else {
            diff_image.put_pixel(x, y, ImageRgba([0, 0, 0, 0]));
        }

        error_sum += f64::from(channel_error.max(alpha_error));
        max_error = max_error.max(channel_error).max(alpha_error);
    }

    let percent_different = if total_pixels == 0 {
        0.0
    } else {
        (differing_pixels as f64 / total_pixels as f64) * 100.0
    };
    let mean_error = if total_pixels == 0 {
        0.0
    } else {
        error_sum / total_pixels as f64
    };

    Ok(PixelReport {
        summary: PixelSummary {
            differing_pixels,
            total_pixels,
            percent_different,
            mean_error,
            max_error,
        },
        diff_region: bounds.map(|(x0, y0, x1, y1)| PixelRegion {
            x: x0,
            y: y0,
            width: x1 - x0 + 1,
            height: y1 - y0 + 1,
        }),
        diff_image,
    })
}

/// Composites `reference` and `result` at 50% opacity each, as the
/// "overlay" failure artifact — enough to spot a misplaced or misrendered
/// region at a glance.
///
/// Precondition: both images have the same dimensions. Callers only
/// produce an overlay after a successful [`compare_pixels`] call, which
/// already guarantees this.
pub fn overlay_images(reference: &RgbaImage, result: &RgbaImage) -> RgbaImage {
    let (width, height) = reference.dimensions();
    RgbaImage::from_fn(width, height, |x, y| {
        let a = reference.get_pixel(x, y).0;
        let b = result.get_pixel(x, y).0;
        let blend = |i: usize| ((u16::from(a[i]) + u16::from(b[i])) / 2) as u8;
        ImageRgba([blend(0), blend(1), blend(2), blend(3)])
    })
}

/// A red/cyan anaglyph of `reference` against `result` — `reference`'s own
/// luminance in the red channel, `result`'s in green+blue. Where the two
/// agree, red and cyan cancel out to a neutral gray; where they don't, the
/// mismatching region shows up as a colored fringe (reference brighter:
/// reddish; result brighter: cyan-ish) — a glance tells you *whether* two
/// renders line up, which [`overlay_images`]'s own 50/50 blend (still
/// useful for "what does the misplaced region actually look like") doesn't
/// make as immediately legible.
///
/// Precondition: both images have the same dimensions, the same as
/// [`overlay_images`].
pub fn anaglyph_overlay(reference: &RgbaImage, result: &RgbaImage) -> RgbaImage {
    let (width, height) = reference.dimensions();
    RgbaImage::from_fn(width, height, |x, y| {
        let reference_luminance = luminance(*reference.get_pixel(x, y));
        let result_luminance = luminance(*result.get_pixel(x, y));
        ImageRgba([reference_luminance, result_luminance, result_luminance, 255])
    })
}

/// Perceptual (ITU-R BT.601) luminance of one RGB pixel, ignoring alpha —
/// both this crate's own images are always fully opaque in practice (a
/// Chromium screenshot, or Florui's own canvas filled before painting).
fn luminance(pixel: ImageRgba<u8>) -> u8 {
    let [r, g, b, _a] = pixel.0;
    (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, color: [u8; 4]) -> RgbaImage {
        RgbaImage::from_fn(width, height, |_, _| ImageRgba(color))
    }

    #[test]
    fn the_differing_pixels_are_bounded_by_a_region() {
        let a = solid(20, 20, [0, 0, 0, 255]);
        let mut b = solid(20, 20, [0, 0, 0, 255]);
        for (x, y) in [(4, 6), (9, 6), (4, 11), (9, 11)] {
            b.put_pixel(x, y, ImageRgba([255, 255, 255, 255]));
        }
        let report = compare_pixels(&a, &b, &PixelDiffOptions::default()).unwrap();
        assert_eq!(
            report.diff_region,
            Some(PixelRegion {
                x: 4,
                y: 6,
                width: 6,
                height: 6
            })
        );
    }

    #[test]
    fn identical_images_have_no_region() {
        let a = solid(5, 5, [1, 2, 3, 255]);
        let report = compare_pixels(&a, &a.clone(), &PixelDiffOptions::default()).unwrap();
        assert_eq!(report.diff_region, None);
    }

    #[test]
    fn identical_images_have_zero_diff() {
        let a = solid(10, 10, [10, 20, 30, 255]);
        let b = solid(10, 10, [10, 20, 30, 255]);
        let report = compare_pixels(&a, &b, &PixelDiffOptions::default()).unwrap();
        assert!(report.summary.is_exact_match());
        assert_eq!(report.summary.percent_different, 0.0);
        assert_eq!(report.summary.mean_error, 0.0);
        assert_eq!(report.summary.max_error, 0);
    }

    #[test]
    fn a_single_differing_pixel_is_measured_correctly() {
        let mut a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(10, 10, [0, 0, 0, 255]);
        a.put_pixel(3, 4, ImageRgba([50, 0, 0, 255]));

        let report = compare_pixels(&a, &b, &PixelDiffOptions::default()).unwrap();
        assert_eq!(report.summary.differing_pixels, 1);
        assert_eq!(report.summary.total_pixels, 100);
        assert_eq!(report.summary.percent_different, 1.0);
        assert_eq!(report.summary.max_error, 50);
        // Only the differing pixel contributes error, spread across 100 pixels.
        assert!((report.summary.mean_error - 0.5).abs() < 1e-9);
        assert_eq!(
            *report.diff_image.get_pixel(3, 4),
            ImageRgba([255, 0, 0, 255])
        );
        assert_eq!(*report.diff_image.get_pixel(0, 0), ImageRgba([0, 0, 0, 0]));
    }

    #[test]
    fn alpha_mismatch_is_detected_even_with_matching_color() {
        let a = solid(4, 4, [10, 10, 10, 255]);
        let b = solid(4, 4, [10, 10, 10, 200]);
        let report = compare_pixels(&a, &b, &PixelDiffOptions::default()).unwrap();
        assert!(!report.summary.is_exact_match());
        assert_eq!(report.summary.differing_pixels, 16);
    }

    #[test]
    fn tolerance_absorbs_small_differences() {
        let a = solid(4, 4, [10, 10, 10, 255]);
        let b = solid(4, 4, [12, 10, 10, 255]);
        let options = PixelDiffOptions {
            channel_tolerance: 5,
            alpha_tolerance: 0,
        };
        let report = compare_pixels(&a, &b, &options).unwrap();
        assert!(report.summary.is_exact_match());
    }

    #[test]
    fn overlay_blends_both_images_evenly() {
        let a = solid(2, 2, [0, 0, 0, 255]);
        let b = solid(2, 2, [200, 100, 50, 255]);
        let overlay = overlay_images(&a, &b);
        assert_eq!(*overlay.get_pixel(0, 0), ImageRgba([100, 50, 25, 255]));
    }

    #[test]
    fn anaglyph_overlay_of_identical_images_is_neutral_gray() {
        // Same luminance on both sides means red == green == blue: a
        // perfect match reads as gray, with no colored fringe at all.
        let a = solid(2, 2, [128, 128, 128, 255]);
        let b = solid(2, 2, [128, 128, 128, 255]);
        let overlay = anaglyph_overlay(&a, &b);
        let pixel = overlay.get_pixel(0, 0).0;
        assert_eq!(pixel[0], pixel[1]);
        assert_eq!(pixel[1], pixel[2]);
    }

    #[test]
    fn anaglyph_overlay_of_a_mismatch_produces_a_colored_fringe() {
        // reference darker than result: red channel (reference) stays low,
        // green/blue (result) go high — a cyan-leaning pixel, not gray.
        let reference = solid(2, 2, [0, 0, 0, 255]);
        let result = solid(2, 2, [255, 255, 255, 255]);
        let overlay = anaglyph_overlay(&reference, &result);
        let pixel = overlay.get_pixel(0, 0).0;
        assert_eq!(pixel[0], 0, "reference's own luminance in the red channel");
        assert_eq!(pixel[1], 255, "result's own luminance in green and blue");
        assert_eq!(pixel[2], 255);
        assert_ne!(pixel[0], pixel[1], "a real mismatch must not read as gray");
    }

    #[test]
    fn anaglyph_overlay_reads_perceptual_luminance_not_a_flat_average() {
        // Pure green is perceptually much brighter than pure blue at equal
        // channel value — a flat (r+g+b)/3 average would treat them as
        // identical luminance; BT.601 weighting must not.
        let green = solid(1, 1, [0, 255, 0, 255]);
        let blue = solid(1, 1, [0, 0, 255, 255]);
        let overlay = anaglyph_overlay(&green, &blue);
        let pixel = overlay.get_pixel(0, 0).0;
        assert!(
            pixel[0] > pixel[1],
            "green's luminance (red channel here) must read brighter than blue's \
             (green/blue channels here): got {pixel:?}"
        );
    }

    #[test]
    fn dimension_mismatch_is_a_hard_error() {
        let a = solid(10, 10, [0, 0, 0, 255]);
        let b = solid(10, 20, [0, 0, 0, 255]);
        let error = compare_pixels(&a, &b, &PixelDiffOptions::default()).unwrap_err();
        assert!(matches!(
            error,
            PixelCompareError::DimensionMismatch {
                reference: (10, 10),
                result: (10, 20)
            }
        ));
    }
}
