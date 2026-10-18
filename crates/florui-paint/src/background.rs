//! `background-image` layers: where each one sits (origin box, size, position,
//! repeat), which part of the box it is cut to, and the composite of its
//! tiles under that cut. The colors of a gradient come from [`crate::gradient`].

use florui_style::{
    BackgroundBox, BackgroundImage, BackgroundLayer, BackgroundRepeat, BackgroundSize, BorderSide,
    Edges, LengthPercentage, RoundedRect,
};
use tiny_skia::{FillRule, Mask, Pixmap, PixmapPaint, Transform};

use crate::gradient::{self, Shader};
use crate::image_source;
use crate::layer_cache;
use crate::rounded::RoundedRectPath;
use crate::{Surface, draw_shifted, mul_div_255};

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
            // Never an origin; a clip of it is handled where the layer is cut.
            BackgroundBox::NoClip => &self.border,
        }
    }
}

fn resolve(value: LengthPercentage, basis: f32, scale: f32) -> f32 {
    value.length * scale + value.percentage * basis
}

/// The size of one tile of the image. A gradient has no size or ratio of its
/// own, so `auto`, `cover` and `contain` all mean the positioning area; an
/// image has its intrinsic size (CSS pixels, so it scales with the display) and
/// ratio: an `auto` axis follows the other one, `cover` and `contain` scale
/// it to just fill or just fit the area.
fn tile_size(
    size: BackgroundSize,
    area: (f32, f32),
    scale: f32,
    intrinsic: Option<(f32, f32)>,
) -> (f32, f32) {
    let Some((iw, ih)) = intrinsic.filter(|(w, h)| *w > 0.0 && *h > 0.0) else {
        return match size {
            BackgroundSize::Cover | BackgroundSize::Contain => area,
            BackgroundSize::Explicit(width, height) => (
                width.map_or(area.0, |w| resolve(w, area.0, scale)),
                height.map_or(area.1, |h| resolve(h, area.1, scale)),
            ),
        };
    };
    match size {
        BackgroundSize::Cover | BackgroundSize::Contain => {
            let (across, down) = (area.0 / iw, area.1 / ih);
            let factor = if size == BackgroundSize::Cover {
                across.max(down)
            } else {
                across.min(down)
            };
            (iw * factor, ih * factor)
        }
        BackgroundSize::Explicit(width, height) => {
            let width = width.map(|w| resolve(w, area.0, scale));
            let height = height.map(|h| resolve(h, area.1, scale));
            match (width, height) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ih / iw),
                (None, Some(h)) => (h * iw / ih, h),
                (None, None) => (iw * scale, ih * scale),
            }
        }
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
    // `mask-clip: no-clip` cuts nothing: the whole surface is where it may paint.
    let everywhere = RoundedRect::square(
        buffer.origin.0 as f32,
        buffer.origin.1 as f32,
        buffer.pixmap.width() as f32,
        buffer.pixmap.height() as f32,
    );
    let cut = if layer.clip == BackgroundBox::NoClip {
        &everywhere
    } else {
        boxes.get(layer.clip)
    };
    if cut.width <= 0.0 || cut.height <= 0.0 || origin.width <= 0.0 || origin.height <= 0.0 {
        return;
    }
    // An image is read (or found already read) before anything is sized by it.
    let loaded = match &layer.image {
        BackgroundImage::Url(url) => match image_source::load(url) {
            Some(loaded) => Some(loaded),
            None => return,
        },
        _ => None,
    };
    let (mut tile_w, mut tile_h) = tile_size(
        layer.size,
        (origin.width, origin.height),
        scale,
        loaded.as_ref().map(|loaded| loaded.intrinsic),
    );
    if loaded.is_some() {
        // An image is drawn at whole pixels, so its edges stay sharp.
        (tile_w, tile_h) = (tile_w.round(), tile_h.round());
    }
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
    let (xs, ys, tile_w, tile_h) = if loaded.is_some() {
        let whole = |values: Vec<f32>| values.into_iter().map(f32::round).collect::<Vec<_>>();
        (
            whole(xs),
            whole(ys),
            tile_w.round().max(1.0),
            tile_h.round().max(1.0),
        )
    } else {
        (xs, ys, tile_w, tile_h)
    };

    // Only the part the tiles cover is drawn and kept: a strip along one edge of
    // a large box is a strip, not a box-sized layer of clear pixels.
    let (Some(&first_x), Some(&last_x), Some(&first_y), Some(&last_y)) = (
        xs.iter().min_by(|a, b| a.total_cmp(b)),
        xs.iter().max_by(|a, b| a.total_cmp(b)),
        ys.iter().min_by(|a, b| a.total_cmp(b)),
        ys.iter().max_by(|a, b| a.total_cmp(b)),
    ) else {
        return;
    };
    let x0 = x0.max(first_x.floor() as i32);
    let y0 = y0.max(first_y.floor() as i32);
    let x1 = x1.min((last_x + tile_w).ceil() as i32);
    let y1 = y1.min((last_y + tile_h).ceil() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (width, height) = ((x1 - x0) as usize, (y1 - y0) as usize);

    // The pixels of the layer depend on nothing but these, so a layer that is
    // painted again unchanged is not rendered again.
    let (keep_x0, keep_x1) = center_range(cut_x, cut_x + cut.width, x0, x1);
    let (keep_y0, keep_y1) = center_range(cut_y, cut_y + cut.height, y0, y1);
    let bits = |values: &[f32]| values.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    let source = match &layer.image {
        BackgroundImage::Url(url) => image_source::identity(url),
        _ => String::new(),
    };
    let key = format!(
        "{:?}|{source}|{}|{width}x{height}|{:?}|{:?}|{:?}|{:?}|{keep_x0},{keep_x1},{keep_y0},{keep_y1}|{:?}",
        layer.image,
        scale.to_bits(),
        bits(&xs),
        bits(&ys),
        bits(&[tile_w, tile_h]),
        bits(&[x0 as f32, y0 as f32]),
        (layer.repeat, layer.origin, layer.clip),
    );
    let pixels = match layer_cache::peek(&key) {
        Some(pixels) => pixels,
        None => {
            // A layer nothing cuts or clips, not asked for before, goes straight
            // onto the frame: drawing it into a layer of its own and putting that
            // over the frame is twice the work, and a resize asks for each size once.
            let loose = clip.is_none_or(|mask| {
                crate::mask_is_full(mask, buffer.pixmap.width(), (x0, y0, x1, y1))
            });
            if loose && !layer_cache::seen_before(&key) {
                florui_profile::count(florui_profile::Counter::LayersDrawnDirect, 1);
                draw_straight(
                    buffer,
                    cut,
                    (x0, y0),
                    (width, height),
                    (keep_x0, keep_x1, keep_y0, keep_y1),
                    &|pixels, origin, keep, over| {
                        render_layer(
                            pixels,
                            &layer.image,
                            loaded.as_deref(),
                            scale,
                            (tile_w, tile_h),
                            (&xs, &ys),
                            origin,
                            keep,
                            over,
                        );
                    },
                );
                return;
            }
            let Some(pixels) = layer_cache::get_or_render(key, || {
                let mut pixels = Pixmap::new(width as u32, height as u32)?;
                render_layer(
                    &mut pixels,
                    &layer.image,
                    loaded.as_deref(),
                    scale,
                    (tile_w, tile_h),
                    (&xs, &ys),
                    (x0, y0),
                    (keep_x0, keep_x1, keep_y0, keep_y1),
                    false,
                );
                Some(pixels)
            }) else {
                return;
            };
            pixels
        }
    };
    composite_layer(buffer, &pixels, (x0, y0), cut, clip);
}

/// Draws every tile of the layer into `pixels` (whose top-left pixel is at
/// `(x0, y0)` of the surface), within the pixels the cut keeps. `loaded` is the
/// image of a `url()` layer.
#[allow(clippy::too_many_arguments)]
fn render_layer(
    pixels: &mut Pixmap,
    image: &BackgroundImage,
    loaded: Option<&image_source::Loaded>,
    scale: f32,
    (tile_w, tile_h): (f32, f32),
    (xs, ys): (&[f32], &[f32]),
    (x0, y0): (i32, i32),
    (keep_x0, keep_x1, keep_y0, keep_y1): (i32, i32, i32, i32),
    over: bool,
) {
    let width = pixels.width() as usize;
    let shader = match image {
        BackgroundImage::Linear(g) => Shader::linear(g, tile_w, tile_h, scale),
        BackgroundImage::Radial(g) => Shader::radial(g, tile_w, tile_h, scale),
        BackgroundImage::Conic(g) => Shader::conic(g, tile_w, tile_h, scale),
        BackgroundImage::Url(url) => {
            let Some(tile) = loaded
                .and_then(|loaded| image_source::tile(url, loaded, tile_w as u32, tile_h as u32))
            else {
                return;
            };
            let (limit_w, limit_h) = (pixels.width() as i32, pixels.height() as i32);
            for &ty in ys {
                for &tx in xs {
                    let (px, py) = (tx as i32 - x0, ty as i32 - y0);
                    let bounds = (
                        px.max(keep_x0 - x0).max(0),
                        py.max(keep_y0 - y0).max(0),
                        (px + tile.width() as i32).min(keep_x1 - x0).min(limit_w),
                        (py + tile.height() as i32).min(keep_y1 - y0).min(limit_h),
                    );
                    if bounds.2 > bounds.0 && bounds.3 > bounds.1 {
                        crate::blit_over(pixels, &tile, (px, py), bounds);
                    }
                }
            }
            return;
        }
    };
    let data = pixels.data_mut();
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
            let fill = if over {
                gradient::fill_over
            } else {
                gradient::fill
            };
            fill(
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

/// Puts `layer` (whose top-left pixel is at `(x0, y0)` of the surface) over the
/// buffer, through the rounded corners of `cut` and the ancestors' clip. The
/// cut and the clip are applied to the layer's own pixels, over the area it
/// covers only, so a small rounded panel costs a small mask and not one the size
/// of the window; a layer that nothing cuts is copied as it is.
fn composite_layer(
    buffer: &mut Surface,
    layer: &Pixmap,
    (x0, y0): (i32, i32),
    cut: &RoundedRect,
    clip: Option<&Mask>,
) {
    let (width, height) = (layer.width() as usize, layer.height() as usize);
    let surface_width_now = buffer.pixmap.width();
    let unclipped = clip.is_none_or(|mask| {
        crate::mask_is_full(
            mask,
            surface_width_now,
            (x0, y0, x0 + width as i32, y0 + height as i32),
        )
    });
    if unclipped && !cut.is_square() {
        composite_rounded(buffer, layer, (x0, y0), cut);
        return;
    }
    let rounded = (!cut.is_square())
        .then(|| corner_coverage(buffer, cut, (x0, y0), (width, height)))
        .flatten();
    let surface_width = buffer.pixmap.width();
    let clip = clip.filter(|mask| {
        !crate::mask_is_full(
            mask,
            surface_width,
            (x0, y0, x0 + width as i32, y0 + height as i32),
        )
    });
    if rounded.is_none() && clip.is_none() {
        draw_shifted(
            &mut buffer.pixmap,
            layer,
            (x0, y0),
            &PixmapPaint::default(),
            None,
        );
        return;
    }
    let mut shaped = layer.clone();
    for (row, pixels) in shaped.data_mut().chunks_exact_mut(width * 4).enumerate() {
        for (column, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            if pixel[3] == 0 {
                continue;
            }
            let mut keep = rounded
                .as_ref()
                .map_or(255, |cover| cover[row * width + column]);
            if let Some(clip) = clip {
                let index = (y0 as usize + row) * surface_width as usize + x0 as usize + column;
                keep = mul_div_255(keep, clip.data().get(index).copied().unwrap_or(0));
            }
            if keep != 255 {
                for channel in pixel.iter_mut() {
                    *channel = mul_div_255(*channel, keep);
                }
            }
        }
    }
    draw_shifted(
        &mut buffer.pixmap,
        &shaped,
        (x0, y0),
        &PixmapPaint::default(),
        None,
    );
}

/// A rectangle `(x0, y0, x1, y1)` of an area, in its own pixels.
type Area = (usize, usize, usize, usize);

/// The part of a `width x height` area that a rounded cut can touch: a frame as
/// thick as the corners are round, as rectangles `(x0, y0, x1, y1)` of the area,
/// and what is inside it, where the cut covers every pixel.
fn rim_rects(width: usize, height: usize, cut: &RoundedRect) -> (Option<Area>, Vec<Area>) {
    let reach = cut
        .radii
        .iter()
        .fold(0.0f32, |most, &(h, v)| most.max(h).max(v));
    let pad = reach.ceil() as usize + 1;
    if width <= 2 * pad || height <= 2 * pad {
        return (None, vec![(0, 0, width, height)]);
    }
    (
        Some((pad, pad, width - pad, height - pad)),
        vec![
            (0, 0, width, pad),
            (0, height - pad, width, height),
            (0, pad, pad, height - pad),
            (width - pad, pad, width, height - pad),
        ],
    )
}

/// `layer` over the buffer through the rounded corners of `cut`: the inside is
/// copied as it is, and only the frame where the corners are is shaped.
fn composite_rounded(
    buffer: &mut Surface,
    layer: &Pixmap,
    (x0, y0): (i32, i32),
    cut: &RoundedRect,
) {
    let (width, height) = (layer.width() as usize, layer.height() as usize);
    let (inside, bands) = rim_rects(width, height, cut);
    if let Some((ix0, iy0, ix1, iy1)) = inside {
        crate::blit_over(
            &mut buffer.pixmap,
            layer,
            (x0, y0),
            (
                x0 + ix0 as i32,
                y0 + iy0 as i32,
                x0 + ix1 as i32,
                y0 + iy1 as i32,
            ),
        );
    }
    for band in bands {
        shape_band(buffer, cut, (x0, y0), band, layer, (band.0, band.1));
    }
}

/// Draws a layer straight onto the frame: what the cut leaves whole as it is, and
/// the frame along its rounded corners through a small layer of its own, shaped
/// and put over the frame the way a kept layer is, so that both come out alike.
/// `draw` renders the layer into a pixmap whose top left is at the given point
/// of the surface, within the given pixels (`x0, x1, y0, y1`), over what is there
/// or not.
type DrawLayer<'a> = &'a dyn Fn(&mut Pixmap, (i32, i32), (i32, i32, i32, i32), bool);

fn draw_straight(
    buffer: &mut Surface,
    cut: &RoundedRect,
    (x0, y0): (i32, i32),
    (width, height): (usize, usize),
    keep: (i32, i32, i32, i32),
    draw: DrawLayer<'_>,
) {
    if cut.is_square() {
        draw(&mut buffer.pixmap, (0, 0), keep, true);
        return;
    }
    let (inside, bands) = rim_rects(width, height, cut);
    if let Some((ix0, iy0, ix1, iy1)) = inside {
        let within = (
            keep.0.max(x0 + ix0 as i32),
            keep.1.min(x0 + ix1 as i32),
            keep.2.max(y0 + iy0 as i32),
            keep.3.min(y0 + iy1 as i32),
        );
        if within.1 > within.0 && within.3 > within.2 {
            draw(&mut buffer.pixmap, (0, 0), within, true);
        }
    }
    for band in bands {
        let (bw, bh) = (band.2 - band.0, band.3 - band.1);
        let Some(mut piece) = Pixmap::new(bw as u32, bh as u32) else {
            continue;
        };
        let origin = (x0 + band.0 as i32, y0 + band.1 as i32);
        let within = (
            keep.0.max(origin.0),
            keep.1.min(origin.0 + bw as i32),
            keep.2.max(origin.1),
            keep.3.min(origin.1 + bh as i32),
        );
        if within.1 > within.0 && within.3 > within.2 {
            draw(&mut piece, origin, within, false);
            shape_band(buffer, cut, (x0, y0), band, &piece, (0, 0));
        }
    }
}

/// The part `band` of the area at `(x0, y0)`, read from `source` (where the
/// band's top left is at `from`), put over the buffer through the cut's corners.
fn shape_band(
    buffer: &mut Surface,
    cut: &RoundedRect,
    (x0, y0): (i32, i32),
    (bx0, by0, bx1, by1): Area,
    source: &Pixmap,
    (from_x, from_y): (usize, usize),
) {
    let (bw, bh) = (bx1 - bx0, by1 - by0);
    let Some(cover) = corner_coverage(buffer, cut, (x0 + bx0 as i32, y0 + by0 as i32), (bw, bh))
    else {
        return;
    };
    let surface_width = buffer.pixmap.width() as usize;
    let source_width = source.width() as usize;
    let pixels = source.data();
    let target = buffer.pixmap.data_mut();
    for row in 0..bh {
        for column in 0..bw {
            let keep = cover[row * bw + column];
            let at = ((from_y + row) * source_width + from_x + column) * 4;
            let pixel = &pixels[at..at + 4];
            if keep == 0 || pixel[3] == 0 {
                continue;
            }
            let scaled = if keep == 255 {
                [pixel[0], pixel[1], pixel[2], pixel[3]]
            } else {
                [
                    mul_div_255(pixel[0], keep),
                    mul_div_255(pixel[1], keep),
                    mul_div_255(pixel[2], keep),
                    mul_div_255(pixel[3], keep),
                ]
            };
            let to = ((y0 as usize + by0 + row) * surface_width + x0 as usize + bx0 + column) * 4;
            gradient::over(&mut target[to..to + 4], scaled);
        }
    }
}

/// How much of each pixel of the `width x height` area at `(x0, y0)` of the
/// surface the rounded box `cut` covers, anti-aliased.
fn corner_coverage(
    buffer: &Surface,
    cut: &RoundedRect,
    (x0, y0): (i32, i32),
    (width, height): (usize, usize),
) -> Option<Vec<u8>> {
    let path = cut.path()?;
    let mut mask = Mask::new(width as u32, height as u32)?;
    let to_area = Transform::from_translate(
        -(buffer.origin.0 + x0) as f32,
        -(buffer.origin.1 + y0) as f32,
    );
    mask.fill_path(&path, FillRule::Winding, true, to_area);
    Some(mask.data().to_vec())
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
            tile_size(BackgroundSize::Explicit(None, None), area, 1.0, None),
            area
        );
        assert_eq!(tile_size(BackgroundSize::Cover, area, 1.0, None), area);
        assert_eq!(tile_size(BackgroundSize::Contain, area, 1.0, None), area);
        assert_eq!(
            tile_size(
                BackgroundSize::Explicit(Some(lp(20.0, 0.0)), Some(lp(0.0, 0.5))),
                area,
                2.0,
                None
            ),
            (40.0, 50.0),
            "lengths scale, percentages are of the area"
        );
        assert_eq!(
            tile_size(
                BackgroundSize::Explicit(Some(lp(10.0, 0.0)), None),
                area,
                1.0,
                None
            ),
            (10.0, 100.0),
            "an auto axis is the area on that axis"
        );
    }

    #[test]
    fn an_image_tile_is_its_own_size_scaled_by_the_display_and_keeps_its_ratio() {
        let area = (200.0, 100.0);
        let image = Some((40.0, 20.0));
        let auto = BackgroundSize::Explicit(None, None);
        assert_eq!(tile_size(auto, area, 1.0, image), (40.0, 20.0));
        assert_eq!(
            tile_size(auto, area, 2.0, image),
            (80.0, 40.0),
            "css pixels scale"
        );
        // One axis given: the other follows the ratio.
        let width = BackgroundSize::Explicit(Some(lp(60.0, 0.0)), None);
        assert_eq!(tile_size(width, area, 1.0, image), (60.0, 30.0));
        let height = BackgroundSize::Explicit(None, Some(lp(0.0, 0.5)));
        assert_eq!(tile_size(height, area, 1.0, image), (100.0, 50.0));
        // Both given: stretched.
        let both = BackgroundSize::Explicit(Some(lp(30.0, 0.0)), Some(lp(30.0, 0.0)));
        assert_eq!(tile_size(both, area, 1.0, image), (30.0, 30.0));
        // 200 x 100 for a 2:1 image fits exactly; a 1:1 one covers or is contained.
        assert_eq!(
            tile_size(BackgroundSize::Cover, area, 1.0, Some((10.0, 10.0))),
            (200.0, 200.0)
        );
        assert_eq!(
            tile_size(BackgroundSize::Contain, area, 1.0, Some((10.0, 10.0))),
            (100.0, 100.0)
        );
        assert_eq!(
            tile_size(BackgroundSize::Cover, area, 1.0, image),
            (200.0, 100.0)
        );
        // An image with no size is sized like a gradient.
        assert_eq!(
            tile_size(BackgroundSize::Cover, area, 1.0, Some((0.0, 0.0))),
            area
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

    #[test]
    fn a_strip_along_the_edge_of_a_large_box_keeps_a_strip() {
        let before = layer_cache::bytes_on_this_thread();
        // Four tiles of a 2px strip each, on a 400x400 box.
        let tree: florui::prelude::Element = florui::prelude::view! { <div class="box"></div> };
        let arena = florui_style::Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".box { background-image: repeating-linear-gradient(90deg, #f00 0 6px, transparent 6px 11px); \
             background-size: 100% 2px; background-position: left bottom; background-repeat: no-repeat; }",
        )
        .expect("a stylesheet");
        let styles = florui_style::compute(
            &arena,
            &rules,
            &florui_style::InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let node = arena.roots()[0];
        let mut layouts = std::collections::HashMap::new();
        layouts.insert(
            node,
            florui_layout::BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 400.0,
            },
        );
        let mut font = florui_text::Font::load_embedded();
        let page = crate::paint_to_buffer(
            &mut font,
            400,
            400,
            florui_style::Rgba::TRANSPARENT,
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        let kept = layer_cache::bytes_on_this_thread() - before;
        assert!(kept <= 400 * 2 * 4, "{kept} bytes kept for a 400x2 strip");
        assert!(
            page.pixel(2, 399).expect("inside").alpha() > 0,
            "it is drawn at the bottom"
        );
        assert_eq!(
            page.pixel(2, 390).expect("inside").alpha(),
            0,
            "and only there"
        );
    }
}
