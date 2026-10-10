//! `background-image` layers: where each one sits (origin box, size, position,
//! repeat), which part of the box it is cut to, and the composite of its
//! tiles under that cut. The colors of a gradient come from [`crate::gradient`].

use florui_style::{
    BackgroundBox, BackgroundImage, BackgroundLayer, BackgroundRepeat, BackgroundSize, BorderSide,
    Edges, LengthPercentage, RoundedRect,
};
use tiny_skia::{FillRule, Mask, Pixmap, PixmapPaint};

use crate::gradient::{self, Shader};
use crate::layer_cache;
use crate::rounded::RoundedRectPath;
use crate::{Surface, draw_shifted, mul_div_255, surface_transform};

/// More tiles than this in one layer are not drawn: a pattern that fine is a
/// texture, and a texture is an image (see the limits in the README).
const MAX_TILES: usize = 20_000;

/// The three boxes a background can be placed in or cut to.
struct Boxes {
    border: RoundedRect,
    padding: RoundedRect,
    content: RoundedRect,
}

impl Boxes {
    fn new(outline: &RoundedRect, border: Edges<BorderSide>, padding: Edges<f32>) -> Self {
        let padding_box = outline.inset(
            border.top.width,
            border.right.width,
            border.bottom.width,
            border.left.width,
        );
        let content = padding_box.inset(padding.top, padding.right, padding.bottom, padding.left);
        Self {
            border: *outline,
            padding: padding_box,
            content,
        }
    }

    fn get(&self, which: BackgroundBox) -> &RoundedRect {
        match which {
            BackgroundBox::Border => &self.border,
            BackgroundBox::Padding => &self.padding,
            BackgroundBox::Content => &self.content,
        }
    }
}

fn resolve(value: LengthPercentage, basis: f32, scale: f32) -> f32 {
    value.length * scale + value.percentage * basis
}

/// The size of one tile of the image: gradients have no size or ratio of their
/// own, so `auto`, `cover` and `contain` all mean the positioning area.
fn tile_size(size: BackgroundSize, area: (f32, f32), scale: f32) -> (f32, f32) {
    match size {
        BackgroundSize::Cover | BackgroundSize::Contain => area,
        BackgroundSize::Explicit(width, height) => (
            width.map_or(area.0, |w| resolve(w, area.0, scale)),
            height.map_or(area.1, |h| resolve(h, area.1, scale)),
        ),
    }
}

/// Where the tiles start along one axis, and how long each is.
///
/// `area_start`/`area_len` are the positioning area, `offset` the resolved
/// `background-position`, and `visible_start`/`visible_end` the part of the
/// axis the layer is cut to, which bounds how far a repeat has to reach.
fn axis_tiles(
    repeat: BackgroundRepeat,
    area_start: f32,
    area_len: f32,
    tile: f32,
    offset: f32,
    (visible_start, visible_end): (f32, f32),
) -> (Vec<f32>, f32) {
    if tile <= 0.0 {
        return (Vec::new(), tile);
    }
    match repeat {
        BackgroundRepeat::NoRepeat => (vec![area_start + offset], tile),
        BackgroundRepeat::Repeat => {
            let first = area_start + offset;
            let back = ((first - visible_start) / tile).ceil();
            let mut at = first - back * tile;
            let mut starts = Vec::new();
            while at < visible_end && starts.len() < MAX_TILES {
                starts.push(at);
                at += tile;
            }
            (starts, tile)
        }
        BackgroundRepeat::Round => {
            let count = (area_len / tile).round().max(1.0);
            let rounded = area_len / count;
            let first = area_start + offset;
            let back = ((first - visible_start) / rounded).ceil();
            let mut at = first - back * rounded;
            let mut starts = Vec::new();
            while at < visible_end && starts.len() < MAX_TILES {
                starts.push(at);
                at += rounded;
            }
            (starts, rounded)
        }
        BackgroundRepeat::Space => {
            let count = (area_len / tile).floor();
            if count < 2.0 {
                return (vec![area_start + offset], tile);
            }
            let gap = (area_len - count * tile) / (count - 1.0);
            let starts = (0..count as usize)
                .map(|i| area_start + i as f32 * (tile + gap))
                .collect();
            (starts, tile)
        }
    }
}

/// Paints `layers` of one box, the first on top, over what is already there.
/// Lengths in the layers are CSS pixels and are multiplied by `scale`; the
/// boxes are already in painted pixels.
pub(crate) fn paint_layers(
    buffer: &mut Surface,
    outline: &RoundedRect,
    border: Edges<BorderSide>,
    padding: Edges<f32>,
    layers: &[BackgroundLayer],
    scale: f32,
    clip: Option<&Mask>,
) {
    let boxes = Boxes::new(outline, border, padding);
    for layer in layers.iter().rev() {
        paint_layer(buffer, &boxes, layer, scale, clip);
    }
}

fn paint_layer(
    buffer: &mut Surface,
    boxes: &Boxes,
    layer: &BackgroundLayer,
    scale: f32,
    clip: Option<&Mask>,
) {
    let origin = boxes.get(layer.origin);
    let cut = boxes.get(layer.clip);
    if cut.width <= 0.0 || cut.height <= 0.0 || origin.width <= 0.0 || origin.height <= 0.0 {
        return;
    }
    let (tile_w, tile_h) = tile_size(layer.size, (origin.width, origin.height), scale);
    if tile_w <= 0.0 || tile_h <= 0.0 {
        return;
    }

    // The part of the surface the layer can reach, in whole local pixels.
    let (cut_x, cut_y) = buffer.local(cut.x, cut.y);
    let x0 = cut_x.floor().max(0.0) as i32;
    let y0 = cut_y.floor().max(0.0) as i32;
    let x1 = ((cut_x + cut.width).ceil() as i32).min(buffer.pixmap.width() as i32);
    let y1 = ((cut_y + cut.height).ceil() as i32).min(buffer.pixmap.height() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (width, height) = ((x1 - x0) as usize, (y1 - y0) as usize);
    // Free space decides the percentages of `background-position`.
    let (origin_x, origin_y) = buffer.local(origin.x, origin.y);
    let offset_x = resolve(layer.position.0, origin.width - tile_w, scale);
    let offset_y = resolve(layer.position.1, origin.height - tile_h, scale);
    let (xs, tile_w) = axis_tiles(
        layer.repeat.0,
        origin_x,
        origin.width,
        tile_w,
        offset_x,
        (cut_x, cut_x + cut.width),
    );
    let (ys, tile_h) = axis_tiles(
        layer.repeat.1,
        origin_y,
        origin.height,
        tile_h,
        offset_y,
        (cut_y, cut_y + cut.height),
    );

    // The pixels of the layer depend on nothing but these, so a layer that is
    // painted again unchanged is not rendered again.
    let (keep_x0, keep_x1) = center_range(cut_x, cut_x + cut.width, x0, x1);
    let (keep_y0, keep_y1) = center_range(cut_y, cut_y + cut.height, y0, y1);
    let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let key = format!(
        "{:?}|{}|{width}x{height}|{:?}|{:?}|{:?}|{:?}|{keep_x0},{keep_x1},{keep_y0},{keep_y1}|{:?}",
        layer.image,
        scale.to_bits(),
        bits(&xs),
        bits(&ys),
        bits(&[tile_w, tile_h]),
        bits(&[x0 as f32, y0 as f32]),
        (layer.repeat, layer.origin, layer.clip),
    );
    let Some(pixels) = layer_cache::get_or_render(key, || {
        let mut pixels = Pixmap::new(width as u32, height as u32)?;
        render_layer(
            pixels.data_mut(),
            width,
            &layer.image,
            scale,
            (tile_w, tile_h),
            (&xs, &ys),
            (x0, y0),
            (keep_x0, keep_x1, keep_y0, keep_y1),
        );
        Some(pixels)
    }) else {
        return;
    };

    let mask = cut_mask(buffer, cut, clip);
    draw_shifted(
        &mut buffer.pixmap,
        &pixels,
        (x0, y0),
        &PixmapPaint::default(),
        mask.as_ref().or(clip),
    );
}

/// Draws every tile of the layer into `data` (`width` pixels wide, whose
/// top-left pixel is at `(x0, y0)` of the surface), within the pixels the cut
/// keeps.
#[allow(clippy::too_many_arguments)]
fn render_layer(
    data: &mut [u8],
    width: usize,
    image: &BackgroundImage,
    scale: f32,
    (tile_w, tile_h): (f32, f32),
    (xs, ys): (&[f32], &[f32]),
    (x0, y0): (i32, i32),
    (keep_x0, keep_x1, keep_y0, keep_y1): (i32, i32, i32, i32),
) {
    let shader = match image {
        BackgroundImage::Linear(g) => Shader::linear(g, tile_w, tile_h, scale),
        BackgroundImage::Radial(g) => Shader::radial(g, tile_w, tile_h, scale),
        BackgroundImage::Conic(g) => Shader::conic(g, tile_w, tile_h, scale),
    };
    // A tile that does not start on a whole pixel covers its edge pixels only
    // in part, which `gradient::fill` blends in.
    let (shift_x, shift_y) = (x0 as f32, y0 as f32);
    for &ty in ys {
        let row0 = (ty.floor() as i32).max(keep_y0);
        let row1 = ((ty + tile_h).ceil() as i32).min(keep_y1);
        for &tx in xs {
            let col0 = (tx.floor() as i32).max(keep_x0);
            let col1 = ((tx + tile_w).ceil() as i32).min(keep_x1);
            if col1 <= col0 || row1 <= row0 {
                continue;
            }
            gradient::fill(
                data,
                width,
                (
                    (col0 - x0) as usize,
                    (row0 - y0) as usize,
                    (col1 - x0) as usize,
                    (row1 - y0) as usize,
                ),
                (
                    tx - shift_x,
                    ty - shift_y,
                    tx + tile_w - shift_x,
                    ty + tile_h - shift_y,
                ),
                &shader,
            );
        }
    }
}

/// The whole pixels of `low..high` whose centers lie in `start..end`.
fn center_range(start: f32, end: f32, low: i32, high: i32) -> (i32, i32) {
    let first = ((start - 0.5).ceil() as i32).max(low);
    let last = ((end - 0.5).ceil() as i32).min(high);
    (first, last)
}

/// The mask that cuts a layer to `cut` when it has rounded corners: its
/// anti-aliased shape times the ancestors' clip. A square cut needs none.
fn cut_mask(buffer: &Surface, cut: &RoundedRect, clip: Option<&Mask>) -> Option<Mask> {
    if cut.is_square() {
        return None;
    }
    let path = cut.path()?;
    let mut mask = Mask::new(buffer.pixmap.width(), buffer.pixmap.height())?;
    mask.fill_path(&path, FillRule::Winding, true, surface_transform(buffer));
    if let Some(clip) = clip {
        for (own, ancestor) in mask.data_mut().iter_mut().zip(clip.data()) {
            *own = mul_div_255(*own, *ancestor);
        }
    }
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(length: f32, percentage: f32) -> LengthPercentage {
        LengthPercentage { length, percentage }
    }

    #[test]
    fn auto_cover_and_contain_are_the_whole_area_for_a_gradient() {
        let area = (200.0, 100.0);
        assert_eq!(
            tile_size(BackgroundSize::Explicit(None, None), area, 1.0),
            area
        );
        assert_eq!(tile_size(BackgroundSize::Cover, area, 1.0), area);
        assert_eq!(tile_size(BackgroundSize::Contain, area, 1.0), area);
        assert_eq!(
            tile_size(
                BackgroundSize::Explicit(Some(lp(20.0, 0.0)), Some(lp(0.0, 0.5))),
                area,
                2.0
            ),
            (40.0, 50.0),
            "lengths scale, percentages are of the area"
        );
        assert_eq!(
            tile_size(
                BackgroundSize::Explicit(Some(lp(10.0, 0.0)), None),
                area,
                1.0
            ),
            (10.0, 100.0),
            "an auto axis is the area on that axis"
        );
    }

    #[test]
    fn a_repeat_reaches_back_to_cover_the_visible_start() {
        // Tiles of 30 starting at 50, visible 0..100: 20, then 50, 80, ... and
        // one before 0 (-10) because it still covers 0..20.
        let (starts, tile) = axis_tiles(
            BackgroundRepeat::Repeat,
            0.0,
            100.0,
            30.0,
            50.0,
            (0.0, 100.0),
        );
        assert_eq!(tile, 30.0);
        assert_eq!(starts, vec![-10.0, 20.0, 50.0, 80.0]);
    }

    #[test]
    fn no_repeat_is_one_tile_at_the_position() {
        let (starts, _) = axis_tiles(
            BackgroundRepeat::NoRepeat,
            10.0,
            100.0,
            30.0,
            25.0,
            (10.0, 110.0),
        );
        assert_eq!(starts, vec![35.0]);
    }

    #[test]
    fn round_scales_the_tile_to_fit_a_whole_number() {
        // 100 / 30 = 3.33 rounds to 3 tiles of 33.33.
        let (starts, tile) =
            axis_tiles(BackgroundRepeat::Round, 0.0, 100.0, 30.0, 0.0, (0.0, 100.0));
        assert!((tile - 100.0 / 3.0).abs() < 1.0e-4);
        assert_eq!(starts.len(), 3);
        // Rounds to the nearest whole number, up as well as down: 100 / 28 = 3.57 is 4.
        let (starts, tile) =
            axis_tiles(BackgroundRepeat::Round, 0.0, 100.0, 28.0, 0.0, (0.0, 100.0));
        assert_eq!((starts.len(), tile), (4, 25.0));
        // Never fewer than one, however large the tile.
        let (starts, tile) = axis_tiles(
            BackgroundRepeat::Round,
            0.0,
            100.0,
            400.0,
            0.0,
            (0.0, 100.0),
        );
        assert_eq!((starts.len(), tile), (1, 100.0));
    }

    #[test]
    fn space_spreads_whole_tiles_across_the_area() {
        // 100 / 30 fits 3; the 10 left over is two gaps of 5.
        let (starts, tile) = axis_tiles(
            BackgroundRepeat::Space,
            0.0,
            100.0,
            30.0,
            17.0,
            (0.0, 100.0),
        );
        assert_eq!((starts, tile), (vec![0.0, 35.0, 70.0], 30.0));
        // Fewer than two do not space: one at the position.
        let (starts, _) = axis_tiles(
            BackgroundRepeat::Space,
            0.0,
            100.0,
            60.0,
            12.0,
            (0.0, 100.0),
        );
        assert_eq!(starts, vec![12.0]);
    }

    #[test]
    fn a_pixel_belongs_to_a_range_when_its_center_is_inside() {
        assert_eq!(center_range(1.4, 3.6, 0, 10), (1, 4), "centers 1.5 .. 3.5");
        assert_eq!(center_range(1.6, 3.4, 0, 10), (2, 3), "only center 2.5");
        assert_eq!(
            center_range(-5.0, 20.0, 0, 10),
            (0, 10),
            "bounded by the pixmap"
        );
        assert_eq!(
            center_range(4.0, 4.0, 0, 10).1 - center_range(4.0, 4.0, 0, 10).0,
            0,
            "empty"
        );
    }
}
