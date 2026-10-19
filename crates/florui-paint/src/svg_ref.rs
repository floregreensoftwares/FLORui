//! `url(#id)` references of `mask`, `clip-path` and `filter` to an SVG `<mask>`,
//! `<clipPath>` or `<filter>`: drawn by the SVG renderer (see `florui-svg`) for
//! the box and applied to its group.

use florui_layout::BoxLayout;
use florui_style::{FilterFunction, MaskMode, StyleMap, Tree};
use florui_svg::{Canvas, Reference};
use tiny_skia::{Pixmap, PremultipliedColorU8};

use crate::{Surface, mul_div_255};

/// The surface `group` is, for the box of `layout` painted at `node_abs`.
fn canvas(group: &Surface, node_abs: (f32, f32), layout: &BoxLayout, scale: f32) -> Canvas {
    Canvas {
        size: (group.pixmap.width(), group.pixmap.height()),
        scale,
        origin: (group.origin.0 as f32 / scale, group.origin.1 as f32 / scale),
        border_box: (
            node_abs.0 / scale,
            node_abs.1 / scale,
            layout.width,
            layout.height,
        ),
    }
}

/// How much of each pixel of `group` the mask `url` leaves; nothing when it names
/// no mask, which hides the box.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mask_coverage(
    arena: &Tree,
    styles: &StyleMap,
    url: &str,
    group: &Surface,
    node_abs: (f32, f32),
    layout: &BoxLayout,
    scale: f32,
    mode: MaskMode,
) -> Vec<u8> {
    // `match-source` is the mask's own type; the other two say how to read it.
    let kind = match mode {
        MaskMode::MatchSource => None,
        MaskMode::Alpha => Some(florui_svg::MaskKind::Alpha),
        MaskMode::Luminance => Some(florui_svg::MaskKind::Luminance),
    };
    let pixels = group.pixmap.pixels().len();
    // A mask that cannot be had hides the box; one that cannot be drawn is left out.
    let Some(reference) = florui_svg::reference(arena, styles, url) else {
        return vec![0; pixels];
    };
    match florui_svg::mask_coverage(&reference, &canvas(group, node_abs, layout, scale), kind) {
        Some(coverage) => coverage.to_vec(),
        None => vec![255; pixels],
    }
}

/// Multiplies `group` by the clip `url`; a reference that is no clip, or cannot be
/// drawn, clips nothing.
pub(crate) fn clip(
    arena: &Tree,
    styles: &StyleMap,
    url: &str,
    group: &mut Surface,
    node_abs: (f32, f32),
    layout: &BoxLayout,
    scale: f32,
) {
    let Some(reference) = florui_svg::reference(arena, styles, url) else {
        return;
    };
    let Some(coverage) =
        florui_svg::clip_coverage(&reference, &canvas(group, node_abs, layout, scale))
    else {
        return;
    };
    for (pixel, &cover) in group.pixmap.pixels_mut().iter_mut().zip(coverage.iter()) {
        if cover == 255 {
            continue;
        }
        let scaled = |channel: u8| mul_div_255(channel, cover);
        *pixel = PremultipliedColorU8::from_rgba(
            scaled(pixel.red()),
            scaled(pixel.green()),
            scaled(pixel.blue()),
            scaled(pixel.alpha()),
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
}

/// `group`'s pixels put through the filter `url`; left as they were when it names
/// no filter or cannot be drawn.
pub(crate) fn filter(
    arena: &Tree,
    styles: &StyleMap,
    url: &str,
    group: &mut Surface,
    node_abs: (f32, f32),
    layout: &BoxLayout,
    scale: f32,
) {
    let Some(reference) = florui_svg::reference(arena, styles, url) else {
        return;
    };
    // A reference that is no `<filter>` filters nothing.
    if !reference.defines("filter") {
        return;
    }
    let canvas = canvas(group, node_abs, layout, scale);
    let Some(filtered) = florui_svg::filter_pixels(&reference, &canvas, group.pixmap.data()) else {
        return;
    };
    let (width, height) = (group.pixmap.width(), group.pixmap.height());
    if let Some(mut pixmap) = Pixmap::new(width, height) {
        pixmap.data_mut().copy_from_slice(&filtered);
        group.pixmap = pixmap;
    }
}

/// How far the filter `url` reaches past the border box of `size` (CSS pixels):
/// the most its region extends beyond any side.
pub(crate) fn filter_reach(arena: &Tree, styles: &StyleMap, url: &str, size: (f32, f32)) -> f32 {
    let Some(reference): Option<Reference> = florui_svg::reference(arena, styles, url) else {
        return 0.0;
    };
    let Some((left, top, right, bottom)) = florui_svg::filter_region(&reference, size.0, size.1)
    else {
        return 0.0;
    };
    (-left)
        .max(-top)
        .max(right - size.0)
        .max(bottom - size.1)
        .max(0.0)
}

/// A filter list's own native stretches and its `url()` items, in order: each
/// native stretch goes through `native` and each `url()` through the SVG renderer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_chain(
    arena: &Tree,
    styles: &StyleMap,
    filters: &[FilterFunction],
    group: &mut Surface,
    node_abs: (f32, f32),
    layout: Option<&BoxLayout>,
    scale: f32,
    native: &mut dyn FnMut(&mut Pixmap, &[FilterFunction]),
) {
    let mut start = 0;
    for (index, function) in filters.iter().enumerate() {
        let FilterFunction::Url(url) = function else {
            continue;
        };
        native(&mut group.pixmap, &filters[start..index]);
        start = index + 1;
        if let Some(layout) = layout {
            filter(arena, styles, url, group, node_abs, layout, scale);
        }
    }
    native(&mut group.pixmap, &filters[start..]);
}

/// `pixmap` (a part of the surface, whose top left is at `origin` in device
/// pixels) put through the filter `url` for the border box `box_px` (`x, y,
/// width, height`, device pixels); as it was when the filter names nothing.
pub(crate) fn filter_pixmap(
    arena: &Tree,
    styles: &StyleMap,
    url: &str,
    pixmap: Pixmap,
    origin: (i32, i32),
    box_px: (f32, f32, f32, f32),
    scale: f32,
) -> Pixmap {
    let Some(reference) = florui_svg::reference(arena, styles, url) else {
        return pixmap;
    };
    if !reference.defines("filter") {
        return pixmap;
    }
    let canvas = Canvas {
        size: (pixmap.width(), pixmap.height()),
        scale,
        origin: (origin.0 as f32 / scale, origin.1 as f32 / scale),
        border_box: (
            box_px.0 / scale,
            box_px.1 / scale,
            box_px.2 / scale,
            box_px.3 / scale,
        ),
    };
    let Some(filtered) = florui_svg::filter_pixels(&reference, &canvas, pixmap.data()) else {
        return pixmap;
    };
    let mut out = pixmap;
    out.data_mut().copy_from_slice(&filtered);
    out
}
