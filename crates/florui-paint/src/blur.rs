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

/// Below this standard deviation the three-box approximation is visibly
/// off, and the exact kernel is both cheap (few taps) and the right answer.
const BOX_APPROXIMATION_FROM_SIGMA: f32 = 2.0;

/// How many box passes approximate the Gaussian. More passes follow it more
/// closely (the central limit theorem at work) at one more pass of cost each;
/// four keep the difference to the exact kernel small enough to match what
/// browsers draw, where three leave a visible step.
const BOX_PASSES: usize = 4;

/// The radius (cells before and after a center cell) of each of the
/// `BOX_PASSES` centered boxes whose combined variance is that of a Gaussian of
/// standard deviation `sigma_px`: boxes of two odd widths, `w` and `w + 2`, in the
/// proportion that makes the variances add up exactly (a box of width `w` has
/// variance `(w * w - 1) / 12`). `None` when the blur is too small to be one box.
fn box_radii(sigma_px: f32) -> Option<[usize; BOX_PASSES]> {
    let n = BOX_PASSES as f32;
    let variance = sigma_px * sigma_px;
    let mut narrow = (12.0 * variance / n + 1.0).sqrt().floor() as i64;
    if narrow % 2 == 0 {
        narrow -= 1;
    }
    if narrow < 3 {
        return None;
    }
    let narrow_f = narrow as f32;
    let wide_count = ((12.0 * variance - n * narrow_f * narrow_f - 4.0 * n * narrow_f - 3.0 * n)
        / (-4.0 * narrow_f - 4.0))
        .round()
        .clamp(0.0, n) as usize;
    let mut radii = [0usize; BOX_PASSES];
    for (i, radius) in radii.iter_mut().enumerate() {
        let width = if i < wide_count { narrow } else { narrow + 2 };
        *radius = ((width - 1) / 2) as usize;
    }
    Some(radii)
}

/// Sums the `radius` cells before and after each pixel of `src` (and the pixel
/// itself) into `dst`, a pixel being four channels that move together, treating
/// what lies beyond either end as zero.
fn box_pass_row(src: &[u32], dst: &mut [u32], radius: usize) {
    let (src, _) = src.as_chunks::<4>();
    let (dst, _) = dst.as_chunks_mut::<4>();
    let pixels = src.len();
    let mut sum = [0u32; 4];
    for pixel in &src[..(radius + 1).min(pixels)] {
        for c in 0..4 {
            sum[c] += pixel[c];
        }
    }
    for p in 0..pixels {
        dst[p] = sum;
        // Slide to the next pixel: take in p + radius + 1, let go of p - radius.
        if let Some(entering) = src.get(p + radius + 1) {
            for c in 0..4 {
                sum[c] += entering[c];
            }
        }
        if p >= radius {
            let leaving = &src[p - radius];
            for c in 0..4 {
                sum[c] -= leaving[c];
            }
        }
    }
}

/// The same sums down the columns: `src` and `dst` are rows of `row` cells, and a
/// running row of sums moves down one row at a time, so each step is a plain
/// add and subtract over a whole row.
fn box_pass_columns(src: &[u32], dst: &mut [u32], row: usize, radius: usize) {
    let height = src.len() / row;
    let mut sum = vec![0u32; row];
    for y in 0..(radius + 1).min(height) {
        for (acc, &value) in sum.iter_mut().zip(&src[y * row..][..row]) {
            *acc += value;
        }
    }
    for y in 0..height {
        dst[y * row..][..row].copy_from_slice(&sum);
        let entering = y + radius + 1;
        if entering < height {
            for (acc, &value) in sum.iter_mut().zip(&src[entering * row..][..row]) {
                *acc += value;
            }
        }
        if y >= radius {
            let leaving = y - radius;
            for (acc, &value) in sum.iter_mut().zip(&src[leaving * row..][..row]) {
                *acc -= value;
            }
        }
    }
}

/// How many pixels wide a strip of columns is blurred at a time: narrow enough
/// that its running sums and both buffers stay in the processor's cache.
const STRIP_PIXELS: usize = 16;

/// Below this many pixels a blur is not worth the cost of starting threads.
const PARALLEL_FROM_PIXELS: usize = 40_000;

/// How many threads share `pixels` pixels of work. Rows and strips do not
/// depend on each other and the sums are integers, so the result is the same
/// bytes whatever this returns.
fn workers_for(pixels: usize) -> usize {
    if pixels < PARALLEL_FROM_PIXELS {
        return 1;
    }
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8)
}

/// Blurs the rows of `rows` (whole RGBA rows of `row_len` bytes) horizontally.
fn blur_rows(rows: &mut [u8], row_len: usize, radii: &[usize; BOX_PASSES], scale: f32) {
    let mut a = vec![0u32; row_len];
    let mut b = vec![0u32; row_len];
    for row in rows.chunks_exact_mut(row_len) {
        if row.iter().all(|&byte| byte == 0) {
            continue;
        }
        for (cell, &byte) in a.iter_mut().zip(row.iter()) {
            *cell = u32::from(byte);
        }
        let mut in_a = true;
        for &radius in radii {
            if in_a {
                box_pass_row(&a, &mut b, radius);
            } else {
                box_pass_row(&b, &mut a, radius);
            }
            in_a = !in_a;
        }
        let done = if in_a { &a } else { &b };
        for (byte, &sum) in row.iter_mut().zip(done) {
            *byte = ((sum as f32 * scale + 0.5) as u32).min(255) as u8;
        }
    }
}

/// One blurred strip of columns: where it starts (in pixels), how many cells
/// wide it is, and its `height` rows of that many bytes.
struct Strip {
    x: usize,
    cells: usize,
    bytes: Vec<u8>,
}

/// Blurs the strips of columns that start at each of `starts` (in pixels)
/// vertically, reading `pixels` and returning the results to be written back.
fn blur_strips(
    pixels: &[u8],
    (w, h): (usize, usize),
    starts: &[usize],
    radii: &[usize; BOX_PASSES],
    scale: f32,
) -> Vec<Strip> {
    let row_len = w * 4;
    let strip_cells = STRIP_PIXELS * 4;
    let mut first = vec![0u32; h * strip_cells];
    let mut second = vec![0u32; h * strip_cells];
    let mut out = Vec::with_capacity(starts.len());
    for &x in starts {
        let cells = (w - x).min(STRIP_PIXELS) * 4;
        for y in 0..h {
            let from = &pixels[y * row_len + x * 4..][..cells];
            for (cell, &byte) in first[y * cells..][..cells].iter_mut().zip(from) {
                *cell = u32::from(byte);
            }
        }
        let mut in_first = true;
        for &radius in radii {
            if in_first {
                box_pass_columns(&first[..h * cells], &mut second[..h * cells], cells, radius);
            } else {
                box_pass_columns(&second[..h * cells], &mut first[..h * cells], cells, radius);
            }
            in_first = !in_first;
        }
        let done = if in_first { &first } else { &second };
        let bytes = done[..h * cells]
            .iter()
            .map(|&sum| ((sum as f32 * scale + 0.5) as u32).min(255) as u8)
            .collect();
        out.push(Strip { x, cells, bytes });
    }
    out
}

/// Blurs four-channel 8-bit `pixels` (RGBA, row-major) in place by box passes
/// along each axis that approximate a Gaussian of standard deviation
/// `sigma_px`, at a cost per pixel that does not grow with the radius, shared
/// among threads when the image is large.
///
/// What lies outside the buffer counts as zero, as for
/// [`gaussian_blur_in_place`], so the same padding rule applies. The sums are
/// exact integers and divided once per axis (rounded), not once per pass.
/// Returns `false` and leaves `pixels` alone when the approximation does not
/// apply (a small sigma, or a window so wide the sums would overflow), and the
/// caller uses the exact blur.
pub(crate) fn box_blur_rgba_in_place(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    sigma_px: f32,
) -> bool {
    box_blur_with_workers(
        pixels,
        width,
        height,
        sigma_px,
        workers_for(width as usize * height as usize),
    )
}

/// [`box_blur_rgba_in_place`] on exactly `workers` threads (one runs it on the
/// calling thread).
fn box_blur_with_workers(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    sigma_px: f32,
    workers: usize,
) -> bool {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || sigma_px < BOX_APPROXIMATION_FROM_SIGMA {
        return false;
    }
    let Some(radii) = box_radii(sigma_px) else {
        return false;
    };
    let divisor: u64 = radii.iter().map(|radius| (2 * radius + 1) as u64).product();
    if divisor * 255 > u64::from(u32::MAX) {
        return false;
    }
    let scale = 1.0 / divisor as f32;
    let row_len = w * 4;

    // Horizontal: whole rows, a share of them to each thread.
    if workers <= 1 {
        blur_rows(pixels, row_len, &radii, scale);
    } else {
        let rows_per = h.div_ceil(workers);
        std::thread::scope(|scope| {
            for chunk in pixels.chunks_mut(rows_per * row_len) {
                let radii = &radii;
                scope.spawn(move || blur_rows(chunk, row_len, radii, scale));
            }
        });
    }

    // Vertical: strips of columns, a share of them to each thread, which hand
    // their results back to be written into place.
    let starts: Vec<usize> = (0..w).step_by(STRIP_PIXELS).collect();
    let strips: Vec<Strip> = if workers <= 1 {
        blur_strips(pixels, (w, h), &starts, &radii, scale)
    } else {
        let per = starts.len().div_ceil(workers);
        let shared: &[u8] = pixels;
        std::thread::scope(|scope| {
            let handles: Vec<_> = starts
                .chunks(per)
                .map(|share| {
                    let radii = &radii;
                    scope.spawn(move || blur_strips(shared, (w, h), share, radii, scale))
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().expect("a blur thread does not panic"))
                .collect()
        })
    };
    for strip in strips {
        for y in 0..h {
            pixels[y * row_len + strip.x * 4..][..strip.cells]
                .copy_from_slice(&strip.bytes[y * strip.cells..][..strip.cells]);
        }
    }
    true
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

    /// RGBA made of pseudo-random premultiplied pixels (color never above
    /// alpha), with empty rows and columns.
    fn noisy_rgba(width: u32, height: u32, seed: u32) -> Vec<u8> {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(7);
        let mut pixels = Vec::new();
        for row in 0..height {
            for col in 0..width {
                let mut next = || {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                };
                let (r, g, b, a) = (next(), next(), next(), next());
                if row % 9 < 2 || col % 13 == 0 {
                    pixels.extend([0, 0, 0, 0]);
                } else {
                    pixels.extend([r.min(a), g.min(a), b.min(a), a]);
                }
            }
        }
        pixels
    }

    fn exact_rgba(pixels: &[u8], width: u32, height: u32, sigma: f32) -> Vec<u8> {
        let mut out = pixels.to_vec();
        for channel in 0..4 {
            let mut plane: Vec<u8> = pixels.iter().skip(channel).step_by(4).copied().collect();
            gaussian_blur_in_place(&mut plane, width, height, sigma);
            for (i, value) in plane.into_iter().enumerate() {
                out[i * 4 + channel] = value;
            }
        }
        out
    }

    #[test]
    fn the_box_blur_stays_close_to_the_exact_gaussian_at_odd_and_even_widths() {
        // The widths come out odd and even across these sigmas.
        for sigma in [2.0f32, 2.6, 4.0, 5.5, 8.0, 11.0, 16.0] {
            let (width, height) = (96u32, 80u32);
            // A solid, smooth image gives the approximation its fair case: a
            // rectangle and a disc, padded by more than the kernel reaches.
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            for y in 0..height {
                for x in 0..width {
                    let inside_rect = (30..66).contains(&x) && (24..52).contains(&y);
                    let inside_disc = (x as i32 - 48).pow(2) + (y as i32 - 40).pow(2) < 14 * 14;
                    if inside_rect || inside_disc {
                        let i = ((y * width + x) * 4) as usize;
                        pixels[i..i + 4].copy_from_slice(&[200, 120, 40, 255]);
                    }
                }
            }
            let expected = exact_rgba(&pixels, width, height, sigma);
            let mut actual = pixels;
            assert!(
                box_blur_rgba_in_place(&mut actual, width, height, sigma),
                "sigma {sigma}"
            );
            let worst = actual
                .iter()
                .zip(&expected)
                .map(|(a, e)| a.abs_diff(*e))
                .max()
                .unwrap();
            assert!(
                worst <= 14,
                "sigma {sigma}: the largest difference is {worst} of 255"
            );
        }
    }

    #[test]
    fn the_box_blur_keeps_every_color_within_its_alpha_and_its_energy() {
        let (width, height) = (70u32, 60u32);
        let mut original = noisy_rgba(width, height, 5);
        // A margin of nothing, wider than the blur reaches, as a caller pads.
        for y in 0..height {
            for x in 0..width {
                if x < 14 || x >= width - 14 || y < 14 || y >= height - 14 {
                    let i = ((y * width + x) * 4) as usize;
                    original[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let mut blurred = original.clone();
        assert!(box_blur_rgba_in_place(&mut blurred, width, height, 3.5));
        // Energy is what is lost off the edges and to rounding: with this much
        // padding of noise it stays within a few percent.
        let total = |p: &[u8]| p.iter().map(|&b| u64::from(b)).sum::<u64>() as f64;
        assert!((total(&blurred) / total(&original) - 1.0).abs() < 0.08);
        for pixel in blurred.chunks_exact(4) {
            let a = pixel[3];
            assert!(
                pixel[0] <= a.saturating_add(1)
                    && pixel[1] <= a.saturating_add(1)
                    && pixel[2] <= a.saturating_add(1),
                "{pixel:?}"
            );
        }
    }

    #[test]
    fn a_box_blurred_impulse_is_symmetric_and_spreads_outward_only() {
        let (width, height) = (61u32, 61u32);
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        // A 7 x 7 block, large enough that the blurred values survive 8-bit
        // rounding well away from its middle.
        for y in 27..34 {
            for x in 27..34 {
                let i = ((y * width + x) * 4) as usize;
                pixels[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        assert!(box_blur_rgba_in_place(&mut pixels, width, height, 4.0));
        let at = |x: u32, y: u32| pixels[((y * width + x) * 4 + 3) as usize];
        for d in 1..12u32 {
            assert_eq!(at(30 - d, 30), at(30 + d, 30), "left and right at {d}");
            assert_eq!(at(30, 30 - d), at(30, 30 + d), "up and down at {d}");
            assert_eq!(at(30 - d, 30), at(30, 30 - d), "the axes agree at {d}");
        }
        assert!(at(30, 30) > at(36, 30), "it fades away from the middle");
        assert!(at(36, 30) >= at(40, 30) && at(40, 30) > 0);
        assert_eq!(at(0, 0), 0, "far away stays empty");
    }

    #[test]
    fn the_result_is_the_same_bytes_on_one_thread_or_many() {
        // Large enough that the shares of rows and of strips differ, with a
        // width that is not a multiple of the strip, and a sigma at each parity.
        for (width, height, sigma) in [(211u32, 173u32, 3.0f32), (300, 130, 7.5), (64, 700, 12.0)] {
            let original = noisy_rgba(width, height, width + height);
            let mut one = original.clone();
            assert!(box_blur_with_workers(&mut one, width, height, sigma, 1));
            for workers in [2usize, 3, 5, 8] {
                let mut many = original.clone();
                assert!(box_blur_with_workers(
                    &mut many, width, height, sigma, workers
                ));
                assert_eq!(
                    many, one,
                    "{width}x{height} sigma {sigma} on {workers} threads"
                );
            }
        }
    }

    #[test]
    fn a_small_sigma_or_a_huge_one_is_left_to_the_exact_blur() {
        let mut pixels = noisy_rgba(20, 20, 3);
        let before = pixels.clone();
        assert!(
            !box_blur_rgba_in_place(&mut pixels, 20, 20, 1.5),
            "too small"
        );
        assert!(
            !box_blur_rgba_in_place(&mut pixels, 20, 20, 400.0),
            "the sums would overflow"
        );
        assert!(!box_blur_rgba_in_place(&mut [], 0, 0, 8.0));
        assert_eq!(pixels, before, "untouched when it does not apply");
    }

    #[test]
    fn the_box_radii_add_up_to_the_variance_of_the_gaussian() {
        for sigma in [2.0f32, 2.5, 3.7, 6.0, 9.3, 16.0, 28.0, 40.0] {
            let radii = box_radii(sigma).expect("a box blur applies");
            let variance: f32 = radii
                .iter()
                .map(|&r| {
                    let width = (2 * r + 1) as f32;
                    (width * width - 1.0) / 12.0
                })
                .sum();
            assert!(
                (variance - sigma * sigma).abs() < 0.12 * sigma * sigma,
                "sigma {sigma}: the boxes have variance {variance}, wanted {}",
                sigma * sigma
            );
            let widths: std::collections::BTreeSet<usize> =
                radii.iter().map(|r| 2 * r + 1).collect();
            assert!(widths.len() <= 2, "at most two widths: {widths:?}");
        }
        assert_eq!(box_radii(0.5), None, "too small to be one box");
    }

    /// A measurement, not a check: `cargo test -p florui-paint --release
    /// blur_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn blur_cost() {
        use std::time::Instant;
        for (w, h, sigma) in [
            (300u32, 200u32, 8.0f32),
            (650, 450, 8.0),
            (650, 450, 24.0),
            (1200, 800, 16.0),
        ] {
            let pixels = noisy_rgba(w, h, 9);
            let rounds = 20;
            let t = Instant::now();
            for _ in 0..rounds {
                let mut copy = pixels.clone();
                std::hint::black_box(box_blur_rgba_in_place(&mut copy, w, h, sigma));
            }
            let boxed = t.elapsed() / rounds;
            let t = Instant::now();
            for _ in 0..3 {
                let copy = pixels.clone();
                for channel in 0..4 {
                    let mut plane: Vec<u8> =
                        copy.iter().skip(channel).step_by(4).copied().collect();
                    gaussian_blur_in_place(&mut plane, w, h, sigma);
                    std::hint::black_box(&plane);
                }
            }
            let exact = t.elapsed() / 3;
            let row_len = (w * 4) as usize;
            let src = vec![7u32; row_len];
            let mut dst = vec![0u32; row_len];
            let t = Instant::now();
            for _ in 0..rounds {
                for _ in 0..h {
                    box_pass_row(&src, &mut dst, 5);
                }
                std::hint::black_box(&dst);
            }
            let one_row_pass = t.elapsed() / rounds;
            let cells = STRIP_PIXELS * 4;
            let strip = vec![7u32; h as usize * cells];
            let mut out = vec![0u32; strip.len()];
            let t = Instant::now();
            for _ in 0..rounds {
                for _ in 0..(w as usize).div_ceil(STRIP_PIXELS) {
                    box_pass_columns(&strip, &mut out, cells, 5);
                }
                std::hint::black_box(&out);
            }
            let one_column_pass = t.elapsed() / rounds;
            println!(
                "{w}x{h} sigma {sigma}: boxes {boxed:?}, exact {exact:?}; one row pass {one_row_pass:?}, one column pass {one_column_pass:?}"
            );
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
