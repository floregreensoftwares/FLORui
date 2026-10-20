//! The images a `background-image: url(...)` names, loaded by `florui_assets::url_image`,
//! and a tile of one drawn at the size a layer needs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use florui_assets::RasterFit;
pub(crate) use florui_assets::url_image::{Loaded, Source, identity, load};
use tiny_skia::Pixmap;

/// The largest tile, in pixels on a side and in pixels altogether, that is
/// rendered; a layer that asks for more shows nothing.
const MAX_SIDE: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

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

/// `source` (premultiplied RGBA, `sw x sh`) resampled to `width x height`: an
/// average over whole blocks first when it is being made much smaller, then
/// bilinear, so a large texture shrunk to a small tile is smooth and not a
/// scatter of the pixels that happened to land on a sample.
fn resample(source: Vec<u8>, (sw, sh): (usize, usize), (width, height): (usize, usize)) -> Vec<u8> {
    let (mut data, mut w, mut h) = (source, sw, sh);
    let factor = (w / width).min(h / height);
    if factor >= 2 {
        data = crate::blur::shrink_rgba(&data, w, h, factor);
        (w, h) = crate::blur::shrunk_size(w, h, factor);
    }
    if (w, h) == (width, height) {
        return data;
    }
    let taps = |size: usize, small: usize| -> Vec<(usize, usize, u32)> {
        (0..size)
            .map(|position| {
                let at = ((position as f32 + 0.5) * small as f32 / size as f32 - 0.5)
                    .clamp(0.0, (small - 1) as f32);
                let first = at.floor() as usize;
                (
                    first,
                    (first + 1).min(small - 1),
                    (((at - first as f32) * 256.0 + 0.5) as u32).min(256),
                )
            })
            .collect()
    };
    let columns = taps(width, w);
    let mut out = vec![0u8; width * height * 4];
    let mut blended = vec![0u32; w * 4];
    for (row, (above, below, weight)) in out.chunks_exact_mut(width * 4).zip(taps(height, h)) {
        let top = &data[above * w * 4..][..w * 4];
        let bottom = &data[below * w * 4..][..w * 4];
        for ((cell, &a), &b) in blended.iter_mut().zip(top).zip(bottom) {
            *cell = u32::from(a) * (256 - weight) + u32::from(b) * weight;
        }
        for (pixel, &(left, right, across)) in row.chunks_exact_mut(4).zip(&columns) {
            for channel in 0..4 {
                let mixed = blended[left * 4 + channel] * (256 - across)
                    + blended[right * 4 + channel] * across;
                pixel[channel] = ((mixed + 32768) >> 16) as u8;
            }
        }
    }
    out
}

/// One tile of `loaded` drawn at `width x height` pixels, premultiplied: an SVG
/// is drawn at that size, a PNG is resampled to it. `None` when the size is
/// empty or absurd.
pub(crate) fn tile(url: &str, loaded: &Loaded, width: u32, height: u32) -> Option<Arc<Pixmap>> {
    // Drawing an SVG is the costly part, a resized box asks for the same tile
    // again, and so does every thread painting a band of the frame: it is kept
    // for all of them, drawn once by whichever asks first.
    let key = format!("{}|{width}x{height}", identity(url));
    let cell = {
        let mut tiles = TILES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let tiles = tiles.get_or_insert_with(HashMap::new);
        if tiles.len() >= MOST_TILES {
            tiles.clear();
        }
        Arc::clone(tiles.entry(key).or_default())
    };
    cell.get_or_init(|| draw_tile(loaded, width, height).map(Arc::new))
        .clone()
}

type TileCell = Arc<OnceLock<Option<Arc<Pixmap>>>>;

static TILES: Mutex<Option<HashMap<String, TileCell>>> = Mutex::new(None);

/// How many tiles are kept before they start over.
const MOST_TILES: usize = 256;

fn draw_tile(loaded: &Loaded, width: u32, height: u32) -> Option<Pixmap> {
    if width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return None;
    }
    let mut pixmap = Pixmap::new(width, height)?;
    match &loaded.source {
        Source::Raster(image) => {
            let data = premultiplied(&image.rgba);
            let data = resample(
                data,
                (image.width as usize, image.height as usize),
                (width as usize, height as usize),
            );
            pixmap.data_mut().copy_from_slice(&data);
        }
        Source::Vector(image) => {
            let raster =
                florui_assets::rasterize_svg(image, RasterFit::Stretch { width, height }).ok()?;
            pixmap
                .data_mut()
                .copy_from_slice(&premultiplied(&raster.rgba));
        }
    }
    Some(pixmap)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    use super::*;

    /// A PNG of one flat color, encoded with the `png` crate.
    fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("a header");
            let data: Vec<u8> = (0..width * height).flat_map(|_| color).collect();
            writer.write_image_data(&data).expect("pixels");
        }
        bytes
    }

    fn base64_url(bytes: &[u8]) -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn a_png_tile_is_premultiplied_and_resampled_to_the_size_asked() {
        let url = base64_url(&png(2, 2, [200, 100, 50, 128]));
        let loaded = load(&url).expect("a PNG");
        let same = draw_tile(&loaded, 2, 2).expect("a tile");
        let pixel = same.pixel(0, 0).expect("a pixel");
        assert_eq!(pixel.alpha(), 128);
        assert_eq!(pixel.red(), 100, "200 * 128 / 255, premultiplied");
        let larger = draw_tile(&loaded, 8, 6).expect("a tile");
        assert_eq!((larger.width(), larger.height()), (8, 6));
        let corner = larger.pixel(7, 5).expect("a pixel");
        assert_eq!(
            (corner.red(), corner.alpha()),
            (100, 128),
            "a flat image stays flat when resampled"
        );
        let smaller = draw_tile(&loaded, 1, 1).expect("a tile");
        assert_eq!(smaller.pixel(0, 0).map(|p| p.alpha()), Some(128));
    }

    #[test]
    fn a_big_png_shrunk_a_lot_averages_instead_of_picking_pixels() {
        // In every block of 8 columns only the first is white, so each block
        // averages to 32 of 255; picking the pixel at a sample point would give
        // black.
        let width = 64u32;
        let mut data = Vec::new();
        for _ in 0..8 {
            for x in 0..width {
                let value = if x % 8 == 0 { 255 } else { 0 };
                data.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, 8);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .expect("a header")
                .write_image_data(&data)
                .expect("pixels");
        }
        let loaded = load(&base64_url(&bytes)).expect("a PNG");
        let small = draw_tile(&loaded, 8, 1).expect("a tile");
        for x in 0..8 {
            let value = small.pixel(x, 0).expect("a pixel").red();
            assert!((28..=36).contains(&value), "column {x}: {value}");
        }
    }

    #[test]
    fn an_svg_tile_is_drawn_at_the_size_asked() {
        let svg = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10' fill='#0000ff'/></svg>";
        let loaded = load(svg).expect("an SVG");
        let big = draw_tile(&loaded, 40, 20).expect("a tile");
        assert_eq!((big.width(), big.height()), (40, 20));
        let pixel = big.pixel(39, 19).expect("a pixel");
        assert_eq!(
            (pixel.blue(), pixel.alpha()),
            (255, 255),
            "stretched to fill the tile"
        );
    }

    #[test]
    fn the_same_tile_is_drawn_once() {
        let svg = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10' fill='#00ff00'/></svg>";
        let loaded = load(svg).expect("an SVG");
        let first = tile(svg, &loaded, 12, 7).expect("a tile");
        let again = tile(svg, &loaded, 12, 7).expect("a tile");
        assert!(Arc::ptr_eq(&first, &again), "asked twice, drawn once");
        let other = tile(svg, &loaded, 12, 8).expect("a tile");
        assert!(!Arc::ptr_eq(&first, &other), "another size is another tile");
    }

    #[test]
    fn an_empty_or_absurd_tile_is_none() {
        let loaded = load(&base64_url(&png(2, 2, [0, 0, 0, 255]))).expect("a PNG");
        assert!(draw_tile(&loaded, 0, 5).is_none());
        assert!(draw_tile(&loaded, 9000, 2).is_none());
        assert!(draw_tile(&loaded, 8000, 8000).is_none());
    }
}
