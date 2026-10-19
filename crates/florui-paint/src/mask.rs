//! `mask-image`: what a box and everything in it is multiplied by.
//!
//! Each layer is drawn by the background painter (a mask layer is placed,
//! sized, repeated and cut as a background layer is) into a surface of its own,
//! turned into a coverage (its alpha, or its luminance times its alpha),
//! and combined with the layers below it by `mask-composite`, the bottom layer
//! first. Where no layer paints, the coverage is nothing: a masked box shows
//! only inside the boxes its layers are cut to, which is why a shadow outside the
//! border box is cut away, and why an image that cannot be read hides the box.

use florui_layout::BoxLayout;
use florui_style::{BackgroundImage, ComputedStyle, MaskComposite, MaskMode, RoundedRect, Tree};
use tiny_skia::{Pixmap, PremultipliedColorU8};

use crate::{Surface, background, mul_div_255, scaled_border, scaled_padding, svg_ref};

/// Multiplies `group`'s pixels by the coverage of `style`'s mask layers.
pub(crate) fn apply_mask(
    group: &mut Surface,
    arena: &Tree,
    style: &ComputedStyle,
    layout: &BoxLayout,
    node_abs: (f32, f32),
    scale_factor: f32,
) {
    let box_image = style
        .mask_box_image
        .as_deref()
        .and_then(florui_style::resolve_mask_box_image);
    if style.mask.is_empty() && box_image.is_none() {
        return;
    }
    let (width, height) = (group.pixmap.width(), group.pixmap.height());
    let outline = RoundedRect::new(
        node_abs.0,
        node_abs.1,
        layout.width,
        layout.height,
        &style.border_radius,
        scale_factor,
    );
    let border = scaled_border(style.border, scale_factor);
    let padding = scaled_padding(style.padding, scale_factor);
    // The coverage of the layers painted so far, from the bottom one up.
    let mut below: Option<Vec<u8>> = None;
    for layer in style.mask.iter().rev() {
        // A `url(#id)` to an SVG `<mask>` is drawn by the SVG renderer, with the
        // mask's own type; the properties that place an image do not apply to it.
        let reference = match &layer.layer.image {
            BackgroundImage::Url(url) if florui_svg::is_reference(url) => Some(url),
            _ => None,
        };
        let source = if let Some(url) = reference {
            svg_ref::mask_coverage(arena, url, group, node_abs, layout, scale_factor)
        } else {
            let Some(mut surface) = Surface::new(width, height, group.origin) else {
                return;
            };
            background::paint_layers(
                &mut surface,
                &outline,
                border,
                padding,
                std::slice::from_ref(&layer.layer),
                scale_factor,
                None,
                None,
            );
            coverage(&surface.pixmap, layer.mode)
        };
        // The bottom layer has nothing to combine with: its operator is not used.
        below = Some(match below {
            None => source,
            Some(below) => combine(&source, &below, layer.composite),
        });
    }
    // `-webkit-mask-box-image` is painted over the layers, as one more of them.
    if let Some(image) = box_image {
        let Some(mut surface) = Surface::new(width, height, group.origin) else {
            return;
        };
        // An image that cannot be read paints nothing, and masks nothing.
        crate::border_image::paint(&mut surface, &outline, &border, &image, scale_factor, None);
        let source = coverage(&surface.pixmap, MaskMode::Alpha);
        below = Some(match below {
            None => source,
            Some(below) => combine(&source, &below, MaskComposite::Add),
        });
    }
    let Some(coverage) = below else {
        return;
    };
    for (pixel, &cover) in group.pixmap.pixels_mut().iter_mut().zip(&coverage) {
        if cover == 255 {
            continue;
        }
        let scale = |channel: u8| mul_div_255(channel, cover);
        *pixel = PremultipliedColorU8::from_rgba(
            scale(pixel.red()),
            scale(pixel.green()),
            scale(pixel.blue()),
            scale(pixel.alpha()),
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
}

/// What a layer's pixels mask by. A gradient or an image masks by its alpha
/// unless `mask-mode` says luminance, which is the sRGB luminance of the color
/// times its alpha; on premultiplied channels that is their weighted sum.
fn coverage(pixmap: &Pixmap, mode: MaskMode) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .map(|pixel| match mode {
            MaskMode::Luminance => {
                let luminance = 0.2126 * f32::from(pixel.red())
                    + 0.7152 * f32::from(pixel.green())
                    + 0.0722 * f32::from(pixel.blue());
                (luminance + 0.5).min(255.0) as u8
            }
            MaskMode::MatchSource | MaskMode::Alpha => pixel.alpha(),
        })
        .collect()
}

/// `source` (a layer) put over `below` (the layers under it) by `operator`:
/// Porter-Duff on the coverages.
fn combine(source: &[u8], below: &[u8], operator: MaskComposite) -> Vec<u8> {
    source
        .iter()
        .zip(below)
        .map(|(&s, &d)| match operator {
            MaskComposite::Add => s.saturating_add(mul_div_255(d, 255 - s)),
            MaskComposite::Subtract => mul_div_255(s, 255 - d),
            MaskComposite::Intersect => mul_div_255(s, d),
            MaskComposite::Exclude => {
                mul_div_255(s, 255 - d).saturating_add(mul_div_255(d, 255 - s))
            }
        })
        .collect()
}
