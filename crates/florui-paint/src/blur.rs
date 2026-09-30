//! A hand-rolled Gaussian blur for an 8-bit alpha mask — see this crate's
//! own module doc for why: tiny-skia has no blur or mask-filter primitive
//! of its own to paint `box-shadow`'s `blur-radius` with.
//!
//! # Matching the CSS spec's blur radius
//!
//! CSS Backgrounds and Borders §7.2 doesn't mandate a specific blur
//! algorithm, but its own non-normative note gives the correspondence
//! real browsers use: a `blur-radius` of `X` corresponds to a Gaussian
//! blur with standard deviation `X / 2`. [`gaussian_blur_in_place`] takes
//! that already-converted `sigma_px`, not the raw CSS blur radius.
//!
//! A direct separable convolution (one horizontal pass, one vertical
//! pass), not a box-blur approximation of one: this runs once per
//! painted shadow, not per animation frame, so the simpler, obviously
//! correct implementation is worth more than the extra speed a box-blur
//! approximation would buy.

/// A normalized 1-D Gaussian kernel, truncated at `3 * sigma_px` — beyond
/// that the contribution is visually negligible (well under 1% of the
/// peak) and not worth the extra taps. The returned radius is how far the
/// kernel reaches on each side of its center tap.
pub(crate) fn gaussian_kernel(sigma_px: f32) -> Vec<f32> {
    let radius = (sigma_px * 3.0).ceil().max(1.0) as i32;
    let two_sigma_sq = 2.0 * sigma_px * sigma_px;
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let mut sum = 0.0f32;
    for offset in -radius..=radius {
        let weight = (-((offset * offset) as f32) / two_sigma_sq).exp();
        kernel.push(weight);
        sum += weight;
    }
    for weight in &mut kernel {
        *weight /= sum;
    }
    kernel
}

/// How far a [`gaussian_kernel`] built from `sigma_px` reaches on each
/// side of its center tap — the minimum margin a caller needs to pad an
/// alpha buffer by on every side so the convolution below never needs a
/// value it doesn't have.
pub(crate) fn kernel_radius(sigma_px: f32) -> u32 {
    (sigma_px * 3.0).ceil().max(1.0) as u32
}

/// Blurs `samples` (a `width x height`, row-major, row `0` at the top
/// alpha buffer) in place with a real separable Gaussian of standard
/// deviation `sigma_px`. Every value the kernel would need from outside
/// the buffer is treated as `0` — correct as long as the caller padded
/// the buffer by at least [`kernel_radius`] beyond whatever real content
/// it painted into it (see this module's own doc), not merely clamped to
/// an edge value.
///
/// Each output pixel still sums its taps in ascending order, so the result is
/// bit-identical to the plain per-pixel convolution; only the loop order
/// differs. Every pass accumulates whole rows tap by tap (`out += src *
/// weight` over a contiguous range), which needs no per-tap bounds test,
/// reads memory in order, and lets the compiler vectorize. A source row that
/// is all zero, common in the padding, contributes nothing and is skipped.
pub(crate) fn gaussian_blur_in_place(samples: &mut [u8], width: u32, height: u32, sigma_px: f32) {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 {
        return;
    }
    let kernel = gaussian_kernel(sigma_px);
    let radius = kernel.len() / 2;

    let mut source: Vec<f32> = samples.iter().map(|&byte| byte as f32).collect();
    let mut horizontal = vec![0.0f32; source.len()];
    for (src, out) in source.chunks_exact(w).zip(horizontal.chunks_exact_mut(w)) {
        if src.iter().all(|&value| value == 0.0) {
            continue;
        }
        for (tap, &weight) in kernel.iter().enumerate() {
            let shift = tap as isize - radius as isize;
            let first = (-shift).max(0) as usize;
            let end = (w as isize - shift.max(0)).max(first as isize) as usize;
            if end <= first {
                continue;
            }
            let src_from = (first as isize + shift) as usize;
            for (o, &s) in out[first..end].iter_mut().zip(&src[src_from..]) {
                *o += s * weight;
            }
        }
    }

    let row_has_signal: Vec<bool> = horizontal
        .chunks_exact(w)
        .map(|row| row.iter().any(|&value| value != 0.0))
        .collect();
    // `source` is no longer needed; reuse it for the vertical result.
    source.fill(0.0);
    for (row, out) in source.chunks_exact_mut(w).enumerate() {
        for (tap, &weight) in kernel.iter().enumerate() {
            let sample_row = row as isize + tap as isize - radius as isize;
            if sample_row < 0 || sample_row >= h as isize || !row_has_signal[sample_row as usize] {
                continue;
            }
            let src = &horizontal[sample_row as usize * w..][..w];
            for (o, &s) in out.iter_mut().zip(src) {
                *o += s * weight;
            }
        }
    }

    for (dest, value) in samples.iter_mut().zip(source) {
        *dest = value.round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plain per-pixel convolution [`gaussian_blur_in_place`] replaced,
    /// kept as the oracle its loop reordering must match exactly.
    fn reference_blur(samples: &mut [u8], width: u32, height: u32, sigma_px: f32) {
        let kernel = gaussian_kernel(sigma_px);
        let radius = (kernel.len() / 2) as i32;
        let (w, h) = (width as i32, height as i32);
        let source: Vec<f32> = samples.iter().map(|&byte| byte as f32).collect();
        let mut horizontal = vec![0.0f32; source.len()];
        for row in 0..h {
            for col in 0..w {
                let mut acc = 0.0f32;
                for (tap, &weight) in kernel.iter().enumerate() {
                    let sample_col = col + tap as i32 - radius;
                    if sample_col >= 0 && sample_col < w {
                        acc += source[(row * w + sample_col) as usize] * weight;
                    }
                }
                horizontal[(row * w + col) as usize] = acc;
            }
        }
        let mut vertical = vec![0.0f32; source.len()];
        for row in 0..h {
            for col in 0..w {
                let mut acc = 0.0f32;
                for (tap, &weight) in kernel.iter().enumerate() {
                    let sample_row = row + tap as i32 - radius;
                    if sample_row >= 0 && sample_row < h {
                        acc += horizontal[(sample_row * w + col) as usize] * weight;
                    }
                }
                vertical[(row * w + col) as usize] = acc;
            }
        }
        for (dest, value) in samples.iter_mut().zip(vertical) {
            *dest = value.round().clamp(0.0, 255.0) as u8;
        }
    }

    /// Deterministic pseudo-random bytes, with whole rows and columns left
    /// empty so the zero-row skipping is exercised too.
    fn noisy(width: u32, height: u32, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
        let mut samples = Vec::with_capacity((width * height) as usize);
        for row in 0..height {
            for col in 0..width {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let empty = row % 7 < 2 || col % 11 == 0;
                samples.push(if empty { 0 } else { (state >> 24) as u8 });
            }
        }
        samples
    }

    #[test]
    fn the_reordered_blur_is_bit_identical_to_the_per_pixel_convolution() {
        for (width, height, sigma) in [
            (1u32, 1u32, 2.0f32),
            (7, 5, 0.5),
            (40, 40, 2.0),
            (61, 23, 6.0),
            (23, 61, 6.0),
            (120, 80, 16.0),
            (9, 200, 32.0),
        ] {
            let original = noisy(width, height, width * 31 + height);
            let mut expected = original.clone();
            let mut actual = original;
            reference_blur(&mut expected, width, height, sigma);
            gaussian_blur_in_place(&mut actual, width, height, sigma);
            assert_eq!(actual, expected, "{width}x{height} sigma {sigma}");
        }
    }

    #[test]
    fn an_all_zero_buffer_and_an_empty_one_stay_as_they_are() {
        let mut zeros = vec![0u8; 50 * 30];
        gaussian_blur_in_place(&mut zeros, 50, 30, 8.0);
        assert!(zeros.iter().all(|&b| b == 0));
        let mut empty: Vec<u8> = Vec::new();
        gaussian_blur_in_place(&mut empty, 0, 0, 4.0);
        assert!(empty.is_empty());
    }

    #[test]
    fn kernel_weights_sum_to_one() {
        for sigma in [0.5, 1.0, 4.0, 12.5] {
            let kernel = gaussian_kernel(sigma);
            let sum: f32 = kernel.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-4,
                "sigma {sigma}: kernel sum {sum}, expected ~1.0"
            );
        }
    }

    #[test]
    fn kernel_is_symmetric_and_peaks_at_center() {
        let kernel = gaussian_kernel(3.0);
        let mid = kernel.len() / 2;
        for offset in 1..=mid {
            assert!(
                (kernel[mid - offset] - kernel[mid + offset]).abs() < 1e-6,
                "kernel should be symmetric around its center"
            );
            assert!(
                kernel[mid] >= kernel[mid - offset],
                "center weight should be the peak"
            );
        }
    }

    /// A solid rect stamped into a padded buffer and blurred: alpha must
    /// fall off monotonically moving away from the shape's own edge, not
    /// dip and recover — the exact defect class an inverted blend factor
    /// would produce.
    #[test]
    fn blurring_a_solid_rect_falls_off_monotonically_away_from_the_edge() {
        let width = 120u32;
        let height = 80u32;
        let mut samples = vec![0u8; (width as usize) * (height as usize)];
        // A 60x40 solid rect at (10,10)-(70,50).
        for row in 10..50u32 {
            for col in 10..70u32 {
                samples[(row * width + col) as usize] = 255;
            }
        }

        gaussian_blur_in_place(&mut samples, width, height, 6.0);

        let row = 30u32; // vertical middle of the shape, clear of corners
        let mut previous = 255u8;
        for col in 70..110u32 {
            let value = samples[(row * width + col) as usize];
            assert!(
                value <= previous,
                "alpha increased moving away from the shape at column {col}: {value} > {previous}"
            );
            previous = value;
        }
        assert!(
            previous < 40,
            "expected the mask to have faded well below full opacity by the far edge, got \
             {previous}"
        );
    }

    #[test]
    fn deep_inside_stays_opaque_and_far_outside_stays_transparent() {
        let width = 40u32;
        let height = 40u32;
        let mut samples = vec![0u8; (width as usize) * (height as usize)];
        for row in 5..35u32 {
            for col in 5..35u32 {
                samples[(row * width + col) as usize] = 255;
            }
        }

        gaussian_blur_in_place(&mut samples, width, height, 2.0);

        assert_eq!(samples[(20 * width + 20) as usize], 255, "deep inside");
        assert_eq!(samples[(width + 1) as usize], 0, "far outside");
    }

    #[test]
    fn kernel_radius_grows_with_sigma_and_is_never_zero() {
        assert_eq!(kernel_radius(0.01), 1);
        assert!(kernel_radius(10.0) > kernel_radius(2.0));
    }
}
