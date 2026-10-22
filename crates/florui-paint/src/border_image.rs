//! `border-image`: the source image cut into nine parts by `border-image-slice`
//! and laid around the border box, the corners at the border's width, the
//! edges stretched, repeated, rounded or spaced between them.

use florui_style::{
    BackgroundImage, BorderImage, BorderImageOutset, BorderImageRepeat, BorderImageSlice,
    BorderImageWidth, BorderSide, Edges, RoundedRect,
};
use tiny_skia::{FilterQuality, Mask, Paint, Pattern, Pixmap, Rect, SpreadMode, Transform};

use crate::gradient::{self, Shader};
use crate::image_source;
use crate::{Surface, surface_transform};

/// A part of the source: `x, y, width, height` in source pixels.
type Part = (f32, f32, f32, f32);

/// Where the image is laid: the border box grown by the outset, in painted
/// pixels.
pub(crate) fn area(
    border_box: &RoundedRect,
    image: &BorderImage,
    border: &Edges<BorderSide>,
    scale: f32,
) -> (f32, f32, f32, f32) {
    let outset = |side: BorderImageOutset, width: f32| match side {
        BorderImageOutset::Number(n) => n * width,
        BorderImageOutset::Length(px) => px * scale,
    };
    let top = outset(image.outset[0], border.top.width);
    let right = outset(image.outset[1], border.right.width);
    let bottom = outset(image.outset[2], border.bottom.width);
    let left = outset(image.outset[3], border.left.width);
    (
        border_box.x - left,
        border_box.y - top,
        border_box.width + left + right,
        border_box.height + top + bottom,
    )
}

/// How far the image reaches past the border box, the most on any side.
pub(crate) fn reach(image: &BorderImage, border: &Edges<BorderSide>, scale: f32) -> f32 {
    let widths = [
        border.top.width,
        border.right.width,
        border.bottom.width,
        border.left.width,
    ];
    image
        .outset
        .iter()
        .zip(widths)
        .map(|(side, width)| match side {
            BorderImageOutset::Number(n) => n * width,
            BorderImageOutset::Length(px) => px * scale,
        })
        .fold(0.0, f32::max)
}

thread_local! {
    /// The last few sources drawn, so a box that is painted again unchanged does
    /// not draw its gradient again.
    static DRAWN: std::cell::RefCell<Vec<(String, std::rc::Rc<Pixmap>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

const KEPT: usize = 6;

fn source_pixels(
    image: &BackgroundImage,
    area: (f32, f32),
    scale: f32,
) -> Option<std::rc::Rc<Pixmap>> {
    // A picture is drawn at its own size, whatever the area: boxes of any size
    // that share it share the one drawing.
    // The picture is loaded first and keyed by itself, so that the key cannot describe a
    // newer picture than the pixels drawn from it.
    let loaded = match image {
        BackgroundImage::Url(url) => Some(image_source::load(url)?),
        _ => None,
    };
    let (identity, size) = match (image, &loaded) {
        (BackgroundImage::Url(url), Some(loaded)) => {
            (image_source::identity_of(url, loaded), (0, 0))
        }
        _ => (String::new(), (area.0.to_bits(), area.1.to_bits())),
    };
    let key = format!(
        "{image:?}|{identity}|{}x{}|{}",
        size.0,
        size.1,
        scale.to_bits()
    );
    // A hit moves to the end, so the entries in use are the last to go.
    if let Some(found) = DRAWN.with(|drawn| {
        let mut drawn = drawn.borrow_mut();
        let at = drawn.iter().position(|(kept, _)| *kept == key)?;
        let entry = drawn.remove(at);
        let pixels = std::rc::Rc::clone(&entry.1);
        drawn.push(entry);
        Some(pixels)
    }) {
        return Some(found);
    }
    let pixels = std::rc::Rc::new(draw_source(image, loaded.as_deref(), area, scale)?);
    DRAWN.with(|drawn| {
        let mut drawn = drawn.borrow_mut();
        if drawn.len() >= KEPT {
            drawn.remove(0);
        }
        drawn.push((key, std::rc::Rc::clone(&pixels)));
    });
    Some(pixels)
}

/// The source drawn at the size it is sliced at: a gradient over the area, a
/// picture at its own size (both in painted pixels).
fn draw_source(
    image: &BackgroundImage,
    loaded: Option<&image_source::Loaded>,
    area: (f32, f32),
    scale: f32,
) -> Option<Pixmap> {
    let shader = match image {
        BackgroundImage::Url(url) => {
            let loaded = loaded?;
            let (width, height) = loaded.intrinsic;
            let tile = image_source::tile(
                url,
                loaded,
                (width * scale).round().max(1.0) as u32,
                (height * scale).round().max(1.0) as u32,
            )?;
            return Some((*tile).clone());
        }
        BackgroundImage::Linear(g) => Shader::linear(g, area.0, area.1, scale),
        BackgroundImage::Radial(g) => Shader::radial(g, area.0, area.1, scale),
        BackgroundImage::Conic(g) => Shader::conic(g, area.0, area.1, scale),
    };
    let (width, height) = (
        area.0.round().max(1.0) as u32,
        area.1.round().max(1.0) as u32,
    );
    let mut pixels = Pixmap::new(width, height)?;
    gradient::fill(
        pixels.data_mut(),
        width as usize,
        (0, 0, width as usize, height as usize),
        (0.0, 0.0, width as f32, height as f32),
        &shader,
    );
    Some(pixels)
}

/// Draws the border image; `false` when its source cannot be had, so the
/// border is drawn as its style says instead.
pub(crate) fn paint(
    buffer: &mut Surface,
    border_box: &RoundedRect,
    border: &Edges<BorderSide>,
    image: &BorderImage,
    scale: f32,
    clip: Option<&Mask>,
) -> bool {
    let (ax, ay, aw, ah) = area(border_box, image, border, scale);
    if aw <= 0.0 || ah <= 0.0 {
        return true;
    }
    let Some(source) = source_pixels(&image.source, (aw, ah), scale) else {
        return false;
    };
    let (sw, sh) = (source.width() as f32, source.height() as f32);

    // The slices, in source pixels, no deeper than the image.
    let slice = |side: BorderImageSlice, dimension: f32| match side {
        BorderImageSlice::Number(n) => (n * scale).min(dimension),
        BorderImageSlice::Percent(p) => (p * dimension).min(dimension),
    };
    let (st, sr, sb, sl) = (
        slice(image.slice[0], sh),
        slice(image.slice[1], sw),
        slice(image.slice[2], sh),
        slice(image.slice[3], sw),
    );

    // The widths the slices are drawn at.
    let width_of =
        |side: BorderImageWidth, border_width: f32, own_slice: f32, whole: f32| match side {
            BorderImageWidth::Number(n) => n * border_width,
            BorderImageWidth::Length(lp) => lp.length * scale + lp.percentage * whole,
            BorderImageWidth::Auto => own_slice,
        };
    let (mut wt, mut wr, mut wb, mut wl) = (
        width_of(image.width[0], border.top.width, st, ah),
        width_of(image.width[1], border.right.width, sr, aw),
        width_of(image.width[2], border.bottom.width, sb, ah),
        width_of(image.width[3], border.left.width, sl, aw),
    );
    let shrink = (aw / (wl + wr).max(f32::EPSILON))
        .min(ah / (wt + wb).max(f32::EPSILON))
        .min(1.0);
    if shrink < 1.0 {
        (wt, wr, wb, wl) = (wt * shrink, wr * shrink, wb * shrink, wl * shrink);
    }

    let target = Target {
        source: &source,
        transform: surface_transform(buffer),
        clip,
    };
    let (mid_w, mid_h) = (sw - sl - sr, sh - st - sb);
    let (dest_w, dest_h) = (aw - wl - wr, ah - wt - wb);
    let columns = [
        (0.0, sl, ax, wl),
        (sl, mid_w, ax + wl, dest_w),
        (sw - sr, sr, ax + aw - wr, wr),
    ];
    let rows = [
        (0.0, st, ay, wt),
        (st, mid_h, ay + wt, dest_h),
        (sh - sb, sb, ay + ah - wb, wb),
    ];
    for (row_index, &(src_y, src_h, dst_y, dst_h)) in rows.iter().enumerate() {
        for (column_index, &(src_x, src_w, dst_x, dst_w)) in columns.iter().enumerate() {
            let middle = row_index == 1 && column_index == 1;
            if middle && !image.fill {
                continue;
            }
            if src_w <= 0.0 || src_h <= 0.0 || dst_w <= 0.0 || dst_h <= 0.0 {
                continue;
            }
            // The edges keep their proportions along their own length: a top
            // edge is as much taller as the border is thicker than its slice.
            let across_x = if st > 0.0 {
                wt / st
            } else if sb > 0.0 {
                wb / sb
            } else {
                1.0
            };
            let across_y = if sl > 0.0 {
                wl / sl
            } else if sr > 0.0 {
                wr / sr
            } else {
                1.0
            };
            let (fx, fy) = match (row_index, column_index) {
                (1, 1) => (across_x, across_y),
                (_, 1) => (dst_h / src_h, dst_h / src_h),
                (1, _) => (dst_w / src_w, dst_w / src_w),
                _ => (dst_w / src_w, dst_h / src_h),
            };
            let repeat_x = if column_index == 1 {
                image.repeat.0
            } else {
                BorderImageRepeat::Stretch
            };
            let repeat_y = if row_index == 1 {
                image.repeat.1
            } else {
                BorderImageRepeat::Stretch
            };
            draw_region(
                buffer,
                &target,
                (src_x, src_y, src_w, src_h),
                (dst_x, dst_y, dst_w, dst_h),
                (fx, fy),
                (repeat_x, repeat_y),
            );
        }
    }
    true
}

struct Target<'a> {
    source: &'a Pixmap,
    transform: Transform,
    clip: Option<&'a Mask>,
}

/// Where the tiles of one axis go: `(offset, length)` from the region's start.
fn tiles(repeat: BorderImageRepeat, region: f32, tile: f32) -> Vec<(f32, f32)> {
    if tile <= 0.0 {
        return Vec::new();
    }
    match repeat {
        BorderImageRepeat::Stretch => vec![(0.0, region)],
        BorderImageRepeat::Repeat => {
            // An odd count, so one tile sits in the middle of the region.
            let count = ((region - tile) / (2.0 * tile)).ceil().max(0.0) * 2.0 + 1.0;
            let start = (region - count * tile) / 2.0;
            (0..count as usize)
                .map(|i| (start + i as f32 * tile, tile))
                .collect()
        }
        BorderImageRepeat::Round => {
            let count = (region / tile).round().max(1.0);
            let length = region / count;
            (0..count as usize)
                .map(|i| (i as f32 * length, length))
                .collect()
        }
        BorderImageRepeat::Space => {
            let count = (region / tile).floor();
            if count < 1.0 {
                return Vec::new();
            }
            let gap = (region - count * tile) / (count + 1.0);
            (0..count as usize)
                .map(|i| (gap + i as f32 * (tile + gap), tile))
                .collect()
        }
    }
}

/// One part of the source, at `scale` per axis, tiled over a region.
fn draw_region(
    buffer: &mut Surface,
    target: &Target<'_>,
    (src_x, src_y, src_w, src_h): Part,
    (dst_x, dst_y, dst_w, dst_h): Part,
    (fx, fy): (f32, f32),
    (repeat_x, repeat_y): (BorderImageRepeat, BorderImageRepeat),
) {
    let Some(rect) = tiny_skia::IntRect::from_xywh(
        src_x.round() as i32,
        src_y.round() as i32,
        src_w.round().max(1.0) as u32,
        src_h.round().max(1.0) as u32,
    ) else {
        return;
    };
    let Some(part) = target.source.clone_rect(rect) else {
        return;
    };
    let (part_w, part_h) = (part.width() as f32, part.height() as f32);
    let xs = tiles(repeat_x, dst_w, part_w * fx);
    let ys = tiles(repeat_y, dst_h, part_h * fy);
    for &(oy, tile_h) in &ys {
        for &(ox, tile_w) in &xs {
            let (tx, ty) = (dst_x + ox, dst_y + oy);
            // Only what falls inside the region is drawn.
            let x0 = tx.max(dst_x);
            let y0 = ty.max(dst_y);
            let x1 = (tx + tile_w).min(dst_x + dst_w);
            let y1 = (ty + tile_h).min(dst_y + dst_h);
            let Some(covered) = Rect::from_ltrb(x0, y0, x1, y1) else {
                continue;
            };
            let paint = Paint {
                anti_alias: true,
                shader: Pattern::new(
                    part.as_ref(),
                    SpreadMode::Pad,
                    FilterQuality::Bilinear,
                    1.0,
                    Transform::from_row(tile_w / part_w, 0.0, 0.0, tile_h / part_h, tx, ty),
                ),
                ..Paint::default()
            };
            buffer
                .pixmap
                .fill_rect(covered, &paint, target.transform, target.clip);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_puts_one_tile_in_the_middle_and_clips_the_ends() {
        let placed = tiles(BorderImageRepeat::Repeat, 100.0, 30.0);
        assert_eq!(placed.len(), 5);
        assert_eq!(placed[0], (-25.0, 30.0));
        assert_eq!(placed[2], (35.0, 30.0));
    }

    #[test]
    fn round_resizes_the_tiles_to_fit_whole() {
        let placed = tiles(BorderImageRepeat::Round, 100.0, 30.0);
        assert_eq!(placed.len(), 3);
        assert!((placed[0].1 - 100.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn space_spreads_whole_tiles_with_equal_gaps() {
        let placed = tiles(BorderImageRepeat::Space, 100.0, 30.0);
        assert_eq!(placed.len(), 3);
        let gap = 10.0 / 4.0;
        assert!((placed[0].0 - gap).abs() < 1e-4);
        assert!((placed[1].0 - (gap * 2.0 + 30.0)).abs() < 1e-4);
        assert!(tiles(BorderImageRepeat::Space, 20.0, 30.0).is_empty());
    }

    #[test]
    fn stretch_is_one_tile_over_the_region() {
        assert_eq!(
            tiles(BorderImageRepeat::Stretch, 70.0, 30.0),
            vec![(0.0, 70.0)]
        );
    }
}
