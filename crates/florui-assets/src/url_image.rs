//! The images a `url(...)` names in a stylesheet: where their bytes come from (a file path
//! read against the working directory, as `<img src>` is, or a `data:` URL carried in the
//! stylesheet itself) and what they decode to (a raster image or an SVG), with the size a box that
//! has no size of its own is given.
//!
//! Loading happens on the thread that needs the image first, and the result is kept for
//! every thread; a file that changes on disk is read again. That keeps every way of laying
//! out and painting (a window, a test, a comparison) free of a registry to feed, at the price
//! of a short pause for a large file: an image that big belongs in an `<img>`, which loads in
//! the background.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use base64::Engine as _;

use crate::{RasterImage, VectorImage};

/// A decoded image: its pixels, or the vector document they are drawn from.
pub enum Source {
    Raster(RasterImage),
    Vector(Box<VectorImage>),
}

/// What an image says of its own size: a side it has, and the ratio of its sides when it
/// has one. A raster image has both sides; an SVG has the ones it declares, and a ratio from them or
/// from its `viewBox`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Natural {
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// Width over height.
    pub ratio: Option<f32>,
}

pub struct Loaded {
    pub source: Source,
    /// The size the image has in CSS pixels: its pixels for a PNG, its
    /// declared size for an SVG (the size its `viewBox` or its content gives when it
    /// declares none).
    pub intrinsic: (f32, f32),
    /// What the image itself says of its size, which `intrinsic` fills in.
    pub natural: Natural,
}

/// What identifies the bytes behind a path: when and how big. A `data:` URL
/// is its own identity.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Stamp {
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
pub fn identity(url: &str) -> String {
    match stamp(url) {
        Stamp::Data => format!("data:{}", url.len()),
        Stamp::Missing => format!("missing:{url}"),
        Stamp::File(modified, length) => format!("{url}@{modified:?}/{length}"),
    }
}

/// What was found when an image was last read, and the file's stamp then.
type Kept = (Stamp, Option<Arc<Loaded>>);

/// Every thread reads the same images, so what was read is kept for all of them, not once
/// for each.
static SOURCES: Mutex<Option<HashMap<String, Kept>>> = Mutex::new(None);

/// The image `url` names, loaded now if it has not been (or its file has
/// changed since), or `None` when it cannot be read or decoded; that is said
/// once, on standard error, and not again until the file changes.
pub fn load(url: &str) -> Option<Arc<Loaded>> {
    let now = stamp(url);
    {
        let sources = SOURCES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((kept, loaded)) = sources.as_ref().and_then(|sources| sources.get(url))
            && *kept == now
        {
            return loaded.clone();
        }
    }
    let loaded = match read(url) {
        Ok(loaded) => Some(Arc::new(loaded)),
        Err(reason) => {
            let shown: String = url.chars().take(80).collect();
            eprintln!("florui-assets: the image url({shown}) cannot be used: {reason}");
            None
        }
    };
    SOURCES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(url.to_string(), (now, loaded.clone()));
    loaded
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    /// A PNG, JPEG, WebP or GIF: the decoder tells them apart by their first bytes.
    Raster,
    Svg,
}

fn sniff(bytes: &[u8]) -> Option<Format> {
    if crate::raster::Format::sniff(bytes).is_some() {
        return Some(Format::Raster);
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
            Some(ext) if ext == "svg" => Some(Format::Svg),
            Some(ext) if crate::raster::Format::of_extension(&ext).is_some() => {
                Some(Format::Raster)
            }
            _ => None,
        };
        (bytes, hinted)
    };
    match hinted.or_else(|| sniff(&bytes)) {
        Some(Format::Raster) => {
            let image = crate::decode_raster(&bytes).map_err(|e| e.to_string())?;
            let (width, height) = (image.width as f32, image.height as f32);
            Ok(Loaded {
                source: Source::Raster(image),
                intrinsic: (width, height),
                natural: Natural {
                    width: Some(width),
                    height: Some(height),
                    ratio: Some(width / height),
                },
            })
        }
        Some(Format::Svg) => {
            let text = String::from_utf8(bytes).map_err(|_| "the SVG is not UTF-8".to_string())?;
            let image = crate::parse_svg(&text).map_err(|e| e.to_string())?;
            let intrinsic = (image.intrinsic_size.width, image.intrinsic_size.height);
            Ok(Loaded {
                natural: svg_natural(&text, intrinsic),
                source: Source::Vector(Box::new(image)),
                intrinsic,
            })
        }
        None => Err("only PNG, JPEG, WebP, GIF and SVG images are supported".to_string()),
    }
}

/// What the root element of the SVG `text` says of its size, with `size` what it resolves
/// to: a side it declares (not as a percentage), and the ratio of its `viewBox` or else of
/// both its sides.
fn svg_natural(text: &str, size: (f32, f32)) -> Natural {
    // A length in pixels is what it says; one in another unit is what the SVG parser makes
    // of it.
    let declared = |name: &str, resolved: f32| {
        let value = root_attribute(text, name)?;
        let value = value.trim();
        if value.is_empty() || value.ends_with('%') {
            return None;
        }
        let pixels = value.strip_suffix("px").unwrap_or(value).trim();
        Some(pixels.parse::<f32>().unwrap_or(resolved)).filter(|side| *side > 0.0)
    };
    let width = declared("width", size.0);
    let height = declared("height", size.1);
    let view_box = root_attribute(text, "viewBox").and_then(|value| {
        let numbers: Vec<f32> = value
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse().ok())
            .collect();
        match numbers[..] {
            [_, _, w, h] if w > 0.0 && h > 0.0 => Some(w / h),
            _ => None,
        }
    });
    let ratio = view_box.or_else(|| width.zip(height).map(|(w, h)| w / h));
    Natural {
        width,
        height,
        ratio,
    }
}

/// The value of attribute `name` on the first `<svg>` element of `text`, which is the first
/// one that is not in a comment.
fn root_attribute(text: &str, name: &str) -> Option<String> {
    let mut at = 0;
    let start = loop {
        let relative = text[at..].find(['<'])?;
        let tag = at + relative;
        let rest = &text[tag..];
        if rest.starts_with("<!--") {
            at = tag + rest.find("-->").map_or(rest.len(), |end| end + 3);
        } else if rest.starts_with("<svg")
            && rest[4..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
        {
            break tag + 4;
        } else {
            at = tag + 1;
        }
    };
    let mut rest = &text[start..];
    // The attributes, up to the `>` that is not in a quoted value.
    while let Some(first) = rest.find(|c: char| !c.is_whitespace()) {
        rest = &rest[first..];
        if rest.starts_with('>') || rest.starts_with("/>") {
            return None;
        }
        let key_end = rest.find(|c: char| c.is_whitespace() || matches!(c, '=' | '>' | '/'))?;
        let key = &rest[..key_end];
        let after = rest[key_end..].trim_start();
        let Some(value) = after.strip_prefix('=') else {
            // An attribute with no value.
            rest = after;
            continue;
        };
        let value = value.trim_start();
        let quote = value.chars().next().filter(|c| matches!(c, '"' | '\''))?;
        let close = value[1..].find(quote)? + 1;
        if key == name {
            return Some(value[1..close].to_owned());
        }
        rest = &value[close + 1..];
    }
    None
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
    } else if crate::raster::Format::of_media_type(&media).is_some() {
        Some(Format::Raster)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("an encoded PNG");
        bytes
    }

    fn base64_url(bytes: &[u8]) -> String {
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    fn svg_url(attributes: &str) -> String {
        format!(
            "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' {attributes}><rect width='30' height='20' fill='red'/></svg>"
        )
    }

    fn encoded(format: image::ImageFormat, media: &str) -> String {
        let image = image::RgbImage::from_pixel(10, 5, image::Rgb([20, 120, 220]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .expect("an encoded image");
        format!(
            "data:{media};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        )
    }

    #[test]
    fn a_data_url_jpeg_webp_or_gif_loads_with_its_own_size() {
        for (format, media) in [
            (image::ImageFormat::Jpeg, "image/jpeg"),
            (image::ImageFormat::WebP, "image/webp"),
            (image::ImageFormat::Gif, "image/gif"),
            // A media type that names nothing: the bytes do.
            (image::ImageFormat::Jpeg, "application/octet-stream"),
        ] {
            let loaded = load(&encoded(format, media)).unwrap_or_else(|| panic!("{media}"));
            assert_eq!(loaded.intrinsic, (10.0, 5.0), "{media}");
            assert!(matches!(loaded.source, Source::Raster(_)));
        }
    }

    #[test]
    fn a_file_with_a_jpeg_extension_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cover.JPG");
        let image = image::RgbImage::from_pixel(12, 8, image::Rgb([200, 30, 30]));
        image::DynamicImage::ImageRgb8(image)
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();
        let loaded = load(path.to_str().unwrap()).expect("a JPEG file");
        assert_eq!(loaded.intrinsic, (12.0, 8.0));
    }

    #[test]
    fn a_data_url_png_loads_with_its_own_size() {
        let url = base64_url(&png(6, 4, [200, 100, 50, 255]));
        let loaded = load(&url).expect("a PNG");
        assert_eq!(loaded.intrinsic, (6.0, 4.0));
        assert_eq!(
            loaded.natural,
            Natural {
                width: Some(6.0),
                height: Some(4.0),
                ratio: Some(1.5)
            }
        );
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
    fn the_root_of_an_svg_is_the_first_svg_that_is_not_in_a_comment() {
        let svg = "data:image/svg+xml;utf8,<!-- <svg width='10' height='10'> --><svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 20 10'><rect width='20' height='10'/></svg>";
        let loaded = load(svg).expect("an SVG");
        assert_eq!(
            loaded.natural,
            Natural {
                width: None,
                height: None,
                ratio: Some(2.0)
            }
        );
    }
    #[test]
    fn an_svg_says_the_sides_it_declares_and_the_ratio_it_has() {
        let natural = |attributes: &str| {
            load(&svg_url(attributes))
                .unwrap_or_else(|| panic!("an SVG: {attributes}"))
                .natural
        };
        let sides = |width, height, ratio| Natural {
            width,
            height,
            ratio,
        };
        assert_eq!(
            natural("width='30' height='20'"),
            sides(Some(30.0), Some(20.0), Some(1.5))
        );
        assert_eq!(natural("viewBox='0 0 20 10'"), sides(None, None, Some(2.0)));
        assert_eq!(
            natural("width='30' viewBox='0 0 20 10'"),
            sides(Some(30.0), None, Some(2.0))
        );
        assert_eq!(natural("height='30'"), sides(None, Some(30.0), None));
        assert_eq!(natural(""), sides(None, None, None));
        // A side that is a percentage of nothing is not a size.
        assert_eq!(
            natural("width='100%' height='50%'"),
            sides(None, None, None)
        );
        assert_eq!(
            natural("height=\"30\" viewBox=\"0 0 10 20\""),
            sides(None, Some(30.0), Some(0.5))
        );
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
        let dir = std::env::temp_dir().join(format!("florui-url-image-{}", std::process::id()));
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
}
