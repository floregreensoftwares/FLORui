//! Drawing a reference: its coverage over a box, or the pixels it filters.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use florui_assets::{RasterFit, SvgFonts, parse_svg_with_fonts, rasterize_svg};
use florui_style::{NodeId, StyleMap, Tree};

use crate::document::Reference;

/// Where a reference is drawn: a surface of `size` device pixels whose top left
/// is at `origin` (CSS pixels, in the page), for the box at `border_box`
/// (`x, y, width, height`, CSS pixels, in the page), at this `scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Canvas {
    pub size: (u32, u32),
    pub scale: f32,
    pub origin: (f32, f32),
    pub border_box: (f32, f32, f32, f32),
}

/// The fonts the text in an SVG is drawn with: the engine's own.
fn fonts() -> SvgFonts<'static> {
    const FACES: [&[u8]; 3] = [
        florui_text::EMBEDDED_SANS_SERIF_FONT,
        florui_text::EMBEDDED_SANS_SERIF_ITALIC_FONT,
        florui_text::EMBEDDED_MONOSPACE_FONT,
    ];
    SvgFonts {
        faces: &FACES,
        sans_serif: "Open Sans",
        monospace: "Fira Mono",
    }
}

const ELEMENTS_KEPT: usize = 16;

thread_local! {
    static ELEMENTS: RefCell<Vec<(u64, Rc<Vec<u8>>)>> = const { RefCell::new(Vec::new()) };
}

/// The inline `<svg>` `node`, drawn as `size` device pixels (`css_size` CSS
/// pixels): straight RGBA. Kept for the next time while nothing about it changes.
pub fn render_svg(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    css_size: (f32, f32),
    size: (u32, u32),
) -> Option<Rc<Vec<u8>>> {
    use std::hash::{Hash, Hasher};
    let text = crate::document::svg_document(arena, styles, node, css_size.0, css_size.1)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    size.hash(&mut hasher);
    let key = hasher.finish();
    if let Some(found) = ELEMENTS.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(kept, _)| *kept == key)
            .map(|(_, pixels)| Rc::clone(pixels))
    }) {
        return Some(found);
    }
    let pixels = Rc::new(raster(&text, size)?);
    ELEMENTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= ELEMENTS_KEPT {
            cache.remove(0);
        }
        cache.push((key, Rc::clone(&pixels)));
    });
    Some(pixels)
}

const KEPT: usize = 24;

const FILTERED_KEPT: usize = 4;

thread_local! {
    static DRAWN: RefCell<HashMap<u64, Rc<Vec<u8>>>> = RefCell::new(HashMap::new());
    static FILTERED: RefCell<Vec<(u64, Rc<Vec<u8>>)>> = const { RefCell::new(Vec::new()) };
}

/// The document a reference is drawn in. Its user space has the box's top left as
/// its origin, as an SVG reference on an HTML element has it.
fn document(canvas: &Canvas, reference: &Reference, body: &str) -> String {
    let (width, height) = (
        canvas.size.0 as f32 / canvas.scale,
        canvas.size.1 as f32 / canvas.scale,
    );
    let (left, top) = (
        canvas.origin.0 - canvas.border_box.0,
        canvas.origin.1 - canvas.border_box.1,
    );
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{width}\" height=\"{height}\" viewBox=\"{left} {top} {width} {height}\">\
         <defs>{}</defs>{body}</svg>",
        reference.defs
    )
}

fn raster(text: &str, size: (u32, u32)) -> Option<Vec<u8>> {
    let image = parse_svg_with_fonts(text, fonts()).ok()?;
    let drawn = rasterize_svg(
        &image,
        RasterFit::Stretch {
            width: size.0,
            height: size.1,
        },
    )
    .ok()?;
    Some(drawn.rgba)
}

/// The alpha of the document, kept for the next time.
fn alpha_of(text: String, size: (u32, u32)) -> Option<Rc<Vec<u8>>> {
    // The document is named by a hash of it: the text itself is not kept.
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        size.hash(&mut hasher);
        text.hash(&mut hasher);
        hasher.finish()
    };
    if let Some(found) = DRAWN.with(|cache| cache.borrow().get(&key).cloned()) {
        return Some(found);
    }
    let rgba = raster(&text, size)?;
    let alpha: Rc<Vec<u8>> = Rc::new(rgba.chunks_exact(4).map(|pixel| pixel[3]).collect());
    DRAWN.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= KEPT {
            cache.clear();
        }
        cache.insert(key, Rc::clone(&alpha));
    });
    Some(alpha)
}

fn box_size(canvas: &Canvas) -> (f32, f32) {
    (canvas.border_box.2, canvas.border_box.3)
}

/// What a mask is read as, when the layer's `mask-mode` says (`match-source`, the
/// default, is the mask's own `mask-type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskKind {
    Luminance,
    Alpha,
}

/// How much of each pixel the `<mask>` `reference` leaves of the box: the alpha
/// of an opaque box drawn through the mask, which carries the mask's own type
/// (luminance, or alpha with `mask-type`).
pub fn mask_coverage(
    reference: &Reference,
    canvas: &Canvas,
    kind: Option<MaskKind>,
) -> Option<Rc<Vec<u8>>> {
    // A reference that is no `<mask>` is a transparent black one: nothing shows.
    if !reference.defines("mask") {
        return Some(Rc::new(vec![0; (canvas.size.0 * canvas.size.1) as usize]));
    }
    let typed;
    let reference = match kind {
        Some(kind) => {
            typed = crate::filter::with_mask_type(
                reference,
                match kind {
                    MaskKind::Alpha => "alpha",
                    MaskKind::Luminance => "luminance",
                },
            );
            &typed
        }
        None => reference,
    };
    let (w, h) = box_size(canvas);
    let body = format!(
        "<rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"#000\" mask=\"url(#{})\"/>",
        reference.id
    );
    alpha_of(document(canvas, reference, &body), canvas.size)
}

/// How much of each pixel the `<clipPath>` `reference` leaves of the box.
pub fn clip_coverage(reference: &Reference, canvas: &Canvas) -> Option<Rc<Vec<u8>>> {
    let (w, h) = box_size(canvas);
    let body = format!(
        "<rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"#000\" clip-path=\"url(#{})\"/>",
        reference.id
    );
    alpha_of(document(canvas, reference, &body), canvas.size)
}

/// `premultiplied` (RGBA, the size of the canvas) put through the `<filter>`
/// `reference`; premultiplied too.
pub fn filter_pixels(
    reference: &Reference,
    canvas: &Canvas,
    premultiplied: &[u8],
) -> Option<Vec<u8>> {
    // The same picture through the same filter, as when nothing moves, is the
    // result of the last time.
    let key = filter_key(reference, canvas, premultiplied);
    if let Some(found) = FILTERED.with(|cache| {
        cache
            .borrow()
            .iter()
            .find(|(kept, _)| *kept == key)
            .map(|(_, pixels)| Rc::clone(pixels))
    }) {
        return Some(found.to_vec());
    }
    let filtered = filter_pixels_uncached(reference, canvas, premultiplied)?;
    FILTERED.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= FILTERED_KEPT {
            cache.remove(0);
        }
        cache.push((key, Rc::new(filtered.clone())));
    });
    Some(filtered)
}

/// What identifies a filter run: the reference, where it is drawn and the pixels.
fn filter_key(reference: &Reference, canvas: &Canvas, pixels: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    reference.hash(&mut hasher);
    canvas.size.hash(&mut hasher);
    for value in [
        canvas.scale,
        canvas.origin.0,
        canvas.origin.1,
        canvas.border_box.0,
        canvas.border_box.1,
        canvas.border_box.2,
        canvas.border_box.3,
    ] {
        value.to_bits().hash(&mut hasher);
    }
    // The pixels, eight bytes at a time.
    let mut digest: u64 = 0xcbf2_9ce4_8422_2325;
    let mut chunks = pixels.chunks_exact(8);
    for chunk in &mut chunks {
        let word = u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
        digest = (digest ^ word)
            .wrapping_mul(0x0000_0100_0000_01b3)
            .rotate_left(5);
    }
    for &byte in chunks.remainder() {
        digest = (digest ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    digest.hash(&mut hasher);
    hasher.finish()
}

fn filter_pixels_uncached(
    reference: &Reference,
    canvas: &Canvas,
    premultiplied: &[u8],
) -> Option<Vec<u8>> {
    let (width, height) = canvas.size;
    let mut straight = premultiplied.to_vec();
    for pixel in straight.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&straight).ok()?;
    }
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(&encoded);
    let (css_w, css_h) = (width as f32 / canvas.scale, height as f32 / canvas.scale);
    let (x, y) = (
        canvas.origin.0 - canvas.border_box.0,
        canvas.origin.1 - canvas.border_box.1,
    );
    let (box_w, box_h) = box_size(canvas);
    let reference = &crate::filter::with_resolved_region(reference, box_w, box_h);
    let body = format!(
        "<image x=\"{x}\" y=\"{y}\" width=\"{css_w}\" height=\"{css_h}\" preserveAspectRatio=\"none\" \
         href=\"data:image/png;base64,{data}\" filter=\"url(#{})\"/>",
        reference.id
    );
    let mut out = raster(&document(canvas, reference, &body), canvas.size)?;
    for pixel in out.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(size: u32, scale: f32) -> Canvas {
        let css = size as f32 / scale;
        Canvas {
            size: (size, size),
            scale,
            origin: (100.0, 50.0),
            border_box: (100.0, 50.0, css, css),
        }
    }

    fn reference(defs: &str, id: &str) -> Reference {
        Reference {
            id: id.to_owned(),
            defs: defs.to_owned(),
        }
    }

    fn at(pixels: &[u8], size: u32, x: u32, y: u32) -> u8 {
        pixels[(y * size + x) as usize]
    }

    #[test]
    fn a_mask_leaves_what_its_content_covers_in_the_boxs_user_space() {
        let mask = reference(
            "<mask id=\"m\" maskUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"20\" height=\"20\">\
             <rect x=\"2\" y=\"2\" width=\"6\" height=\"6\" fill=\"#fff\"/></mask>",
            "m",
        );
        let coverage = mask_coverage(&mask, &canvas(20, 1.0), None).expect("a coverage");
        assert_eq!(at(&coverage, 20, 4, 4), 255);
        assert_eq!(at(&coverage, 20, 12, 12), 0);
        assert_eq!(at(&coverage, 20, 1, 4), 0);
        // At twice the scale the same box covers twice the pixels.
        let coverage = mask_coverage(&mask, &canvas(40, 2.0), None).expect("a coverage");
        assert_eq!(at(&coverage, 40, 8, 8), 255);
        assert_eq!(at(&coverage, 40, 3, 8), 0);
        // A luminance mask follows the colour; `mask-type: alpha` follows the alpha.
        let grey = reference(
            "<mask id=\"m\" maskUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" width=\"20\" height=\"20\">\
             <rect width=\"20\" height=\"20\" fill=\"#000\"/></mask>",
            "m",
        );
        let coverage = mask_coverage(&grey, &canvas(20, 1.0), None).expect("a coverage");
        assert_eq!(at(&coverage, 20, 4, 4), 0);
        let alpha = reference(
            "<mask id=\"m\" mask-type=\"alpha\" maskUnits=\"userSpaceOnUse\" x=\"0\" y=\"0\" \
             width=\"20\" height=\"20\"><rect width=\"20\" height=\"20\" fill=\"#000\"/></mask>",
            "m",
        );
        let coverage = mask_coverage(&alpha, &canvas(20, 1.0), None).expect("a coverage");
        assert_eq!(at(&coverage, 20, 4, 4), 255);
    }

    #[test]
    fn what_cannot_be_drawn_is_none_and_a_missing_definition_is_not_drawn() {
        let broken = reference("<mask id=\"m\"><rect width=\"2\" height=\"2\"", "m");
        let size = (9000, 4);
        let huge = Canvas {
            size,
            scale: 1.0,
            origin: (0.0, 0.0),
            border_box: (0.0, 0.0, 9000.0, 4.0),
        };
        // Over the raster size limit, or an SVG that does not parse, there is nothing.
        let mask = reference("<mask id=\"m\"/>", "m");
        assert!(mask_coverage(&mask, &huge, None).is_none());
        assert!(clip_coverage(&broken, &canvas(20, 1.0)).is_none());
        // A mask the definitions do not hold is a transparent one; a clip or filter is
        // simply not found.
        let none = reference("<g/>", "nope");
        let coverage = mask_coverage(&none, &canvas(20, 1.0), None).expect("a coverage");
        assert!(coverage.iter().all(|&cover| cover == 0));
        assert!(!none.defines("filter"));
        let pixels = vec![0u8; 20 * 20 * 4];
        assert!(filter_pixels(&broken, &canvas(20, 1.0), &pixels).is_none());
    }

    #[test]
    fn the_same_picture_through_the_same_filter_is_answered_again() {
        let filter = reference("<filter id=\"f\"><feOffset dx=\"1\"/></filter>", "f");
        let mut pixels = vec![0u8; 8 * 8 * 4];
        pixels[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let first = filter_pixels(&filter, &canvas(8, 1.0), &pixels).expect("pixels");
        let second = filter_pixels(&filter, &canvas(8, 1.0), &pixels).expect("pixels");
        assert_eq!(first, second);
        // Another picture is another answer.
        pixels[4..8].copy_from_slice(&[0, 255, 0, 255]);
        let other = filter_pixels(&filter, &canvas(8, 1.0), &pixels).expect("pixels");
        assert_ne!(first, other);
    }

    #[test]
    fn a_clip_path_leaves_what_its_shape_covers() {
        let clip = reference(
            "<clipPath id=\"c\"><circle cx=\"10\" cy=\"10\" r=\"6\"/></clipPath>",
            "c",
        );
        let coverage = clip_coverage(&clip, &canvas(20, 1.0)).expect("a coverage");
        assert_eq!(at(&coverage, 20, 10, 10), 255);
        assert_eq!(at(&coverage, 20, 1, 1), 0);
        // In units of the box: half of it.
        let units = reference(
            "<clipPath id=\"c\" clipPathUnits=\"objectBoundingBox\">\
             <rect width=\"0.5\" height=\"1\"/></clipPath>",
            "c",
        );
        let coverage = clip_coverage(&units, &canvas(20, 1.0)).expect("a coverage");
        assert_eq!(at(&coverage, 20, 4, 10), 255);
        assert_eq!(at(&coverage, 20, 15, 10), 0);
    }

    #[test]
    fn a_filter_is_applied_to_the_pixels_it_is_given() {
        let size = 20u32;
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        // An opaque red 4x4 square at (2, 2), premultiplied.
        for y in 2..6 {
            for x in 2..6 {
                let at = ((y * size + x) * 4) as usize;
                pixels[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let offset = reference(
            "<filter id=\"f\" x=\"-1\" y=\"-1\" width=\"3\" height=\"3\">\
             <feOffset dx=\"6\" dy=\"0\"/></filter>",
            "f",
        );
        let moved = filter_pixels(&offset, &canvas(size, 1.0), &pixels).expect("pixels");
        let alpha = |x: u32, y: u32| moved[((y * size + x) * 4 + 3) as usize];
        assert_eq!(alpha(9, 3), 255);
        assert_eq!(alpha(3, 3), 0);
        assert_eq!(moved[((3 * size + 9) * 4) as usize], 255);
    }
}
