//! The images a `background-image: url(...)` names: where their bytes come
//! from (a file path read against the working directory, as `<img src>` is,
//! or a `data:` URL carried in the stylesheet itself), what they decode to
//! (a PNG or an SVG), and a tile of one drawn at the size a layer needs.
//!
//! Loading happens on the painting thread, the first time a layer needs the
//! image, and the result is kept; a file that changes on disk is read again.
//! That keeps every way of painting (a window, a test, a comparison) free of a
//! registry to feed, at the price of a short pause for a large file: an
//! image that big belongs in an `<img>`, which loads in the background.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::SystemTime;

use base64::Engine as _;
use florui_assets::{RasterFit, RasterImage, VectorImage};
use tiny_skia::Pixmap;

/// The largest tile, in pixels on a side and in pixels altogether, that is
/// rendered; a layer that asks for more shows nothing.
const MAX_SIDE: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

/// A decoded image: its pixels, or the vector document they are drawn from.
pub(crate) enum Source {
    Raster(RasterImage),
    Vector(Box<VectorImage>),
}

pub(crate) struct Loaded {
    pub source: Source,
    /// The size the image has in CSS pixels: its pixels for a PNG, its
    /// declared size for an SVG.
    pub intrinsic: (f32, f32),
}

/// What identifies the bytes behind a path: when and how big. A `data:` URL
/// is its own identity.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Stamp {
    Data,
    File(Option<SystemTime>, u64),
    Missing,
}

fn stamp(url: &str) -> Stamp {
    if url.starts_with("data:") {
        return Stamp::Data;
    }
    match std::fs::metadata(Path::new(url)) {
        Ok(meta) => Stamp::File(meta.modified().ok(), meta.len()),
        Err(_) => Stamp::Missing,
    }
}

/// Text that stands for `url`'s current bytes, for keys of things made from
/// them: it changes when the file does.
pub(crate) fn identity(url: &str) -> String {
    match stamp(url) {
        Stamp::Data => format!("data:{}", url.len()),
        Stamp::Missing => format!("missing:{url}"),
        Stamp::File(modified, length) => format!("{url}@{modified:?}/{length}"),
    }
}

/// What was found when an image was last read, and the file's stamp then.
type Kept = (Stamp, Option<Rc<Loaded>>);

thread_local! {
    static SOURCES: RefCell<HashMap<String, Kept>> = RefCell::new(HashMap::new());
}

/// The image `url` names, loaded now if it has not been (or its file has
/// changed since), or `None` when it cannot be read or decoded; that is said
/// once, on standard error, and not again until the file changes.
pub(crate) fn load(url: &str) -> Option<Rc<Loaded>> {
    let now = stamp(url);
    SOURCES.with(|sources| {
        let mut sources = sources.borrow_mut();
        if let Some((kept, loaded)) = sources.get(url)
            && *kept == now
        {
            return loaded.clone();
        }
        let loaded = match read(url) {
            Ok(loaded) => Some(Rc::new(loaded)),
            Err(reason) => {
                let shown: String = url.chars().take(80).collect();
                eprintln!("florui-paint: background-image url({shown}) is not painted: {reason}");
                None
            }
        };
        sources.insert(url.to_string(), (now, loaded.clone()));
        loaded
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Png,
    Svg,
}

fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some(Format::Png);
    }
    let text = bytes
        .iter()
        .skip_while(|b| b.is_ascii_whitespace() || **b == 0xEF || **b == 0xBB || **b == 0xBF);
    let head: Vec<u8> = text.take(5).copied().collect();
    (head.first() == Some(&b'<')).then_some(Format::Svg)
}

fn read(url: &str) -> Result<Loaded, String> {
    let (bytes, hinted) = if url.starts_with("data:") {
        data_url(url)?
    } else {
        let path = Path::new(url);
        let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
        let hinted = match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
        {
            Some(ext) if ext == "png" => Some(Format::Png),
            Some(ext) if ext == "svg" => Some(Format::Svg),
            _ => None,
        };
        (bytes, hinted)
    };
    match hinted.or_else(|| sniff(&bytes)) {
        Some(Format::Png) => {
            let image = florui_assets::decode_png(&bytes).map_err(|e| e.to_string())?;
            let intrinsic = (image.width as f32, image.height as f32);
            Ok(Loaded {
                source: Source::Raster(image),
                intrinsic,
            })
        }
        Some(Format::Svg) => {
            let text = String::from_utf8(bytes).map_err(|_| "the SVG is not UTF-8".to_string())?;
            let image = florui_assets::parse_svg(&text).map_err(|e| e.to_string())?;
            let intrinsic = (image.intrinsic_size.width, image.intrinsic_size.height);
            Ok(Loaded {
                source: Source::Vector(Box::new(image)),
                intrinsic,
            })
        }
        None => Err("only PNG and SVG images are supported".to_string()),
    }
}

/// The bytes of a `data:` URL, and the format its media type names, if it
/// names one.
fn data_url(url: &str) -> Result<(Vec<u8>, Option<Format>), String> {
    let rest = &url["data:".len()..];
    let (header, payload) = rest
        .split_once(',')
        .ok_or_else(|| "a data URL has no comma".to_string())?;
    let mut parts = header.split(';');
    let media = parts.next().unwrap_or("").to_ascii_lowercase();
    let base64 = parts.any(|part| part.eq_ignore_ascii_case("base64"));
    let hinted = if media.contains("svg") {
        Some(Format::Svg)
    } else if media.contains("png") {
        Some(Format::Png)
    } else {
        None
    };
    let bytes = if base64 {
        let cleaned = percent_decode(payload);
        let text: Vec<u8> = cleaned
            .into_iter()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map_err(|e| format!("the base64 data is invalid: {e}"))?
    } else {
        percent_decode(payload)
    };
    Ok((bytes, hinted))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            out.push(high * 16 + low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

fn hex(digit: u8) -> Option<u8> {
    (digit as char).to_digit(16).map(|d| d as u8)
}

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
pub(crate) fn tile(loaded: &Loaded, width: u32, height: u32) -> Option<Pixmap> {
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
    fn a_data_url_png_loads_with_its_own_size() {
        let url = base64_url(&png(6, 4, [200, 100, 50, 255]));
        let loaded = load(&url).expect("a PNG");
        assert_eq!(loaded.intrinsic, (6.0, 4.0));
        assert!(matches!(loaded.source, Source::Raster(_)));
    }

    #[test]
    fn a_data_url_svg_loads_as_text_or_as_base64_or_percent_encoded() {
        let svg = "<svg xmlns='http://www.w3.org/2000/svg' width='30' height='20'><rect width='30' height='20' fill='red'/></svg>";
        let plain = format!("data:image/svg+xml;utf8,{svg}");
        let encoded = format!(
            "data:image/svg+xml,{}",
            svg.replace('<', "%3C").replace('>', "%3E")
        );
        let packed = format!(
            "data:image/svg+xml;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(svg)
        );
        for url in [plain, encoded, packed] {
            let loaded = load(&url).unwrap_or_else(|| panic!("an SVG: {url}"));
            assert_eq!(loaded.intrinsic, (30.0, 20.0), "{url}");
            assert!(matches!(loaded.source, Source::Vector(_)));
        }
    }

    #[test]
    fn a_data_url_with_no_media_type_is_told_by_its_first_bytes() {
        let url = format!(
            "data:;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png(2, 2, [1, 2, 3, 255]))
        );
        assert!(matches!(
            load(&url).expect("a PNG").source,
            Source::Raster(_)
        ));
    }

    #[test]
    fn what_cannot_be_loaded_is_none() {
        assert!(load("definitely/not/here.png").is_none());
        assert!(load("data:image/png;base64,@@@@").is_none());
        assert!(
            load("data:image/png;base64,").is_none(),
            "no bytes decode to nothing"
        );
        assert!(
            load("data:text/plain,hello").is_none(),
            "neither PNG nor SVG"
        );
        assert!(load("data:no-comma").is_none());
    }

    #[test]
    fn a_file_is_read_again_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("florui-image-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let path = dir.join("one.png");
        std::fs::write(&path, png(3, 3, [255, 0, 0, 255])).expect("written");
        let url = path.to_string_lossy().to_string();
        assert_eq!(load(&url).expect("a PNG").intrinsic, (3.0, 3.0));
        // A different size is a different file, whatever the clock says.
        std::fs::write(&path, png(5, 7, [0, 255, 0, 255])).expect("written");
        assert_eq!(load(&url).expect("a PNG").intrinsic, (5.0, 7.0));
        assert_ne!(identity(&url), format!("{url}@None/0"));
        std::fs::remove_file(&path).expect("removed");
        assert!(load(&url).is_none(), "gone");
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn a_png_tile_is_premultiplied_and_resampled_to_the_size_asked() {
        let url = base64_url(&png(2, 2, [200, 100, 50, 128]));
        let loaded = load(&url).expect("a PNG");
        let same = tile(&loaded, 2, 2).expect("a tile");
        let pixel = same.pixel(0, 0).expect("a pixel");
        assert_eq!(pixel.alpha(), 128);
        assert_eq!(pixel.red(), 100, "200 * 128 / 255, premultiplied");
        let larger = tile(&loaded, 8, 6).expect("a tile");
        assert_eq!((larger.width(), larger.height()), (8, 6));
        let corner = larger.pixel(7, 5).expect("a pixel");
        assert_eq!(
            (corner.red(), corner.alpha()),
            (100, 128),
            "a flat image stays flat when resampled"
        );
        let smaller = tile(&loaded, 1, 1).expect("a tile");
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
        let small = tile(&loaded, 8, 1).expect("a tile");
        for x in 0..8 {
            let value = small.pixel(x, 0).expect("a pixel").red();
            assert!((28..=36).contains(&value), "column {x}: {value}");
        }
    }

    #[test]
    fn an_svg_tile_is_drawn_at_the_size_asked() {
        let svg = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='10' height='10' fill='#0000ff'/></svg>";
        let loaded = load(svg).expect("an SVG");
        let big = tile(&loaded, 40, 20).expect("a tile");
        assert_eq!((big.width(), big.height()), (40, 20));
        let pixel = big.pixel(39, 19).expect("a pixel");
        assert_eq!(
            (pixel.blue(), pixel.alpha()),
            (255, 255),
            "stretched to fill the tile"
        );
    }

    #[test]
    fn an_empty_or_absurd_tile_is_none() {
        let loaded = load(&base64_url(&png(2, 2, [0, 0, 0, 255]))).expect("a PNG");
        assert!(tile(&loaded, 0, 5).is_none());
        assert!(tile(&loaded, 9000, 2).is_none());
        assert!(tile(&loaded, 8000, 8000).is_none());
    }
}
