//! `text-shadow`: the glyphs of a run again, in the shadow's color, moved by
//! its offsets and blurred, painted behind the text. The layers go bottom to
//! top, the first one listed on top, as CSS says.

use florui_style::TextShadow;
use florui_text::ShapedRun;
use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use crate::{Surface, append_glyphs, blur, draw_shifted};

/// The most pixels of blurred shadow rendered for one run of text; a shadow
/// larger than this (a huge text with a huge blur) is not painted.
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

/// Paints `shadows` for the glyphs of `runs`, whose block's top-left corner is
/// `(x, y)`, behind where the text itself will be drawn. Lengths in the shadows
/// are CSS pixels and scale with `scale_factor`.
pub(crate) fn paint(
    buffer: &mut Surface,
    runs: &[ShapedRun],
    (x, y): (f32, f32),
    shadows: &[TextShadow],
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    if shadows.is_empty() {
        return;
    }
    let (x, y) = buffer.local(x, y);
    for shadow in shadows.iter().rev() {
        if shadow.color.a == 0 {
            continue;
        }
        let mut builder = PathBuilder::new();
        append_glyphs(
            &mut builder,
            runs,
            x + shadow.offset_x * scale_factor,
            y + shadow.offset_y * scale_factor,
            scale_factor,
        );
        let Some(path) = builder.finish() else {
            continue;
        };
        let sigma = shadow.blur_radius * scale_factor / 2.0;
        if sigma <= 0.0 {
            let mut paint = Paint::default();
            paint.set_color_rgba8(
                shadow.color.r,
                shadow.color.g,
                shadow.color.b,
                shadow.color.a,
            );
            paint.anti_alias = true;
            buffer.pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                clip,
            );
            continue;
        }

        // The part of the surface the blur can reach, and the margin the
        // kernel needs beyond the glyphs so the edge of the buffer is not felt.
        let pad = blur::kernel_radius(sigma) as f32;
        let bounds = path.bounds();
        let reach = pad.ceil() as i32;
        let x0 = ((bounds.left() - pad).floor() as i32).max(-reach);
        let y0 = ((bounds.top() - pad).floor() as i32).max(-reach);
        let x1 = ((bounds.right() + pad).ceil() as i32).min(buffer.pixmap.width() as i32 + reach);
        let y1 = ((bounds.bottom() + pad).ceil() as i32).min(buffer.pixmap.height() as i32 + reach);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let (width, height) = ((x1 - x0) as u32, (y1 - y0) as u32);
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            continue;
        }
        let (Some(mut mask), Some(mut layer)) =
            (Mask::new(width, height), Pixmap::new(width, height))
        else {
            continue;
        };
        mask.fill_path(
            &path,
            FillRule::Winding,
            true,
            Transform::from_translate(-(x0 as f32), -(y0 as f32)),
        );
        let mut coverage = mask.data().to_vec();
        if !blur::box_blur_plane_in_place(&mut coverage, width, height, sigma) {
            blur::gaussian_blur_in_place(&mut coverage, width, height, sigma);
        }
        let color = shadow.color;
        for (pixel, &cover) in layer.data_mut().chunks_exact_mut(4).zip(&coverage) {
            if cover == 0 {
                continue;
            }
            let alpha = (u32::from(cover) * u32::from(color.a) + 127) / 255;
            let premultiply = |channel: u8| ((u32::from(channel) * alpha + 127) / 255) as u8;
            pixel.copy_from_slice(&[
                premultiply(color.r),
                premultiply(color.g),
                premultiply(color.b),
                alpha as u8,
            ]);
        }
        draw_shifted(
            &mut buffer.pixmap,
            &layer,
            (x0, y0),
            &PixmapPaint::default(),
            clip,
        );
    }
}
