//! SVG parsing and rasterization, via `resvg`/`usvg`/`tiny-skia` -- see
//! this crate's own top-level doc for why that dependency was kept rather
//! than replaced. Parsing (this module's [`parse_svg`]) is kept separate
//! from rasterizing (`rasterize_svg`) so a caller can read a source's
//! intrinsic size (needed for CSS layout, before any pixel buffer exists)
//! without deciding a raster target size first.

use crate::error::AssetError;
use crate::limits::check_raster_size;
use crate::raster::RasterImage;

/// A parsed, not-yet-rasterized SVG document plus its intrinsic size (the
/// size an `<img>`-like consumer would use for layout before any raster
/// target is chosen).
pub struct VectorImage {
    tree: resvg::usvg::Tree,
    pub intrinsic_size: Size2D,
}

// `usvg::Tree` has no `Debug` impl of its own; report just the field a
// caller actually cares about inspecting.
impl std::fmt::Debug for VectorImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorImage")
            .field("intrinsic_size", &self.intrinsic_size)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size2D {
    pub width: f32,
    pub height: f32,
}

/// Parses `svg_text` and reads its intrinsic size. Pure -- takes no path,
/// touches no disk.
pub fn parse_svg(svg_text: &str) -> Result<VectorImage, AssetError> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_str(svg_text, &options).map_err(AssetError::Svg)?;
    let size = tree.size();
    Ok(VectorImage {
        tree,
        intrinsic_size: Size2D {
            width: size.width(),
            height: size.height(),
        },
    })
}

/// How a vector image's own intrinsic aspect ratio relates to the pixel
/// buffer it is rasterized into. Kept as an explicit, separate choice
/// from the raster dimensions themselves: requesting a non-square target
/// must never silently distort the source image, so a caller has to
/// opt into [`RasterFit::Stretch`] to get that -- [`RasterFit::Contain`]
/// always preserves the source's own proportions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RasterFit {
    /// Uniform scale to fit within `width`x`height`, preserving the
    /// source's own aspect ratio, centered on any leftover axis.
    Contain { width: u32, height: u32 },
    /// Ignore the source's own aspect ratio; stretch to fill exactly
    /// `width`x`height`.
    Stretch { width: u32, height: u32 },
}

impl RasterFit {
    pub fn target(self) -> (u32, u32) {
        match self {
            RasterFit::Contain { width, height } | RasterFit::Stretch { width, height } => {
                (width, height)
            }
        }
    }
}

/// Rasterizes `image` into a pixel buffer per `fit`. Straight (non-
/// premultiplied) alpha -- `tiny-skia`/`resvg` produce premultiplied
/// pixels internally, unpremultiplied here so [`RasterImage`] is always
/// in the same format regardless of source format (PNG decode is
/// straight-alpha natively; see `raster.rs`). A consumer whose own
/// compositor wants premultiplied pixels (e.g. blitting straight into a
/// `tiny-skia` canvas) converts at that point, not here.
///
/// `image.intrinsic_size` is never zero on either axis here: `usvg`
/// represents a tree's size as its own `NonZeroPositiveF32` type and
/// rejects an explicitly zero-sized document as `Err(InvalidSize)` at
/// [`parse_svg`] time (measured directly against `usvg` 0.48.1, not
/// assumed) -- so [`VectorImage`], only ever constructed by `parse_svg`,
/// cannot carry one. A requested raster `fit` of zero on either axis is a
/// real, distinct condition instead (e.g. layout momentarily producing a
/// zero-size box), reported as [`AssetError::RasterTargetEmpty`].
pub fn rasterize_svg(image: &VectorImage, fit: RasterFit) -> Result<RasterImage, AssetError> {
    let (target_width, target_height) = fit.target();
    check_raster_size(target_width, target_height)?;
    if target_width == 0 || target_height == 0 {
        return Err(AssetError::RasterTargetEmpty {
            width: target_width,
            height: target_height,
        });
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(target_width, target_height)
        .expect("target size is checked nonzero above");
    let intrinsic = image.intrinsic_size;
    let transform = match fit {
        RasterFit::Contain { .. } => {
            let scale = (target_width as f32 / intrinsic.width)
                .min(target_height as f32 / intrinsic.height);
            let offset_x = (target_width as f32 - intrinsic.width * scale) / 2.0;
            let offset_y = (target_height as f32 - intrinsic.height * scale) / 2.0;
            resvg::tiny_skia::Transform::from_translate(offset_x, offset_y).pre_scale(scale, scale)
        }
        RasterFit::Stretch { .. } => resvg::tiny_skia::Transform::from_scale(
            target_width as f32 / intrinsic.width,
            target_height as f32 / intrinsic.height,
        ),
    };
    resvg::render(&image.tree, transform, &mut pixmap.as_mut());

    let mut rgba = pixmap.data().to_vec();
    unpremultiply(&mut rgba);
    Ok(RasterImage {
        rgba,
        width: target_width,
        height: target_height,
    })
}

/// Straight = premultiplied * 255 / alpha, per channel. `tiny-skia`'s own
/// premultiplied invariant guarantees `premultiplied <= alpha`, so this
/// never exceeds 255 and never needs clamping -- but the multiply itself
/// overflows `u8` (max 65025), so it runs in `u16`. At `alpha == 0` the
/// pixel is fully transparent and whatever RGB `tiny-skia` already wrote
/// there (typically 0,0,0) is invisible regardless -- skip the division
/// rather than divide by zero.
fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = pixel[3] as u16;
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u16) * 255 / alpha) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLUE_CIRCLE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <circle cx="16" cy="16" r="14" fill="#3366ff"/>
</svg>"##;

    const WIDE_RECT_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 50">
  <rect width="100" height="50" fill="#ff0000"/>
</svg>"##;

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        rgba[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn parse_svg_reads_the_intrinsic_size() {
        let image = parse_svg(WIDE_RECT_SVG).unwrap();
        assert_eq!(
            image.intrinsic_size,
            Size2D {
                width: 100.0,
                height: 50.0
            }
        );
    }

    #[test]
    fn parse_svg_rejects_unparseable_markup() {
        let err = parse_svg("not an svg at all").unwrap_err();
        assert!(matches!(err, AssetError::Svg(_)));
    }

    #[test]
    fn rasterize_svg_contain_produces_the_requested_square_size() {
        let image = parse_svg(BLUE_CIRCLE_SVG).unwrap();
        let raster = rasterize_svg(
            &image,
            RasterFit::Contain {
                width: 64,
                height: 64,
            },
        )
        .unwrap();
        assert_eq!((raster.width, raster.height), (64, 64));
        assert_eq!(raster.rgba.len(), 64 * 64 * 4);
    }

    #[test]
    fn rasterize_svg_contain_unpremultiplies_a_known_fill_color_at_center() {
        let image = parse_svg(BLUE_CIRCLE_SVG).unwrap();
        let raster = rasterize_svg(
            &image,
            RasterFit::Contain {
                width: 64,
                height: 64,
            },
        )
        .unwrap();
        let [r, g, b, a] = pixel(&raster.rgba, 64, 32, 32);
        assert_eq!(a, 255, "center of the circle should be fully opaque");
        assert!(
            b > r && b > g,
            "center should be blue-dominant, got {r},{g},{b}"
        );
    }

    #[test]
    fn rasterize_svg_contain_leaves_a_fully_transparent_pixel_at_a_corner() {
        let image = parse_svg(BLUE_CIRCLE_SVG).unwrap();
        let raster = rasterize_svg(
            &image,
            RasterFit::Contain {
                width: 64,
                height: 64,
            },
        )
        .unwrap();
        let [_, _, _, a] = pixel(&raster.rgba, 64, 0, 0);
        assert_eq!(a, 0, "outside the circle should be transparent");
    }

    #[test]
    fn rasterize_svg_contain_into_a_non_matching_aspect_ratio_letterboxes_instead_of_stretching() {
        // A 100x50 (2:1) source into a 100x100 target under `Contain`
        // must scale to 100x50 and center vertically, leaving transparent
        // bands top and bottom -- never distort to fill the square.
        let image = parse_svg(WIDE_RECT_SVG).unwrap();
        let raster = rasterize_svg(
            &image,
            RasterFit::Contain {
                width: 100,
                height: 100,
            },
        )
        .unwrap();
        let [_, _, _, top_alpha] = pixel(&raster.rgba, 100, 50, 5);
        let [_, _, _, mid_alpha] = pixel(&raster.rgba, 100, 50, 50);
        assert_eq!(top_alpha, 0, "letterbox band should be transparent");
        assert_eq!(mid_alpha, 255, "rect should be opaque at vertical center");
    }

    #[test]
    fn rasterize_svg_stretch_fills_every_axis_independently() {
        // The same 100x50 source under `Stretch` into 100x100 must be
        // opaque everywhere -- confirming this mode really does distort
        // to fill, unlike `Contain` above.
        let image = parse_svg(WIDE_RECT_SVG).unwrap();
        let raster = rasterize_svg(
            &image,
            RasterFit::Stretch {
                width: 100,
                height: 100,
            },
        )
        .unwrap();
        let [_, _, _, top_alpha] = pixel(&raster.rgba, 100, 50, 5);
        assert_eq!(top_alpha, 255, "stretch should fill the whole square");
    }

    #[test]
    fn rasterize_svg_rejects_a_target_over_the_raster_size_limit() {
        let image = parse_svg(BLUE_CIRCLE_SVG).unwrap();
        let err = rasterize_svg(
            &image,
            RasterFit::Contain {
                width: crate::limits::MAX_RASTER_DIMENSION + 1,
                height: 64,
            },
        )
        .unwrap_err();
        assert!(matches!(err, AssetError::RasterTooLarge { .. }));
    }

    #[test]
    fn parse_svg_rejects_an_explicitly_zero_sized_document() {
        // Measured, not assumed: `usvg` represents a tree's own size as
        // `NonZeroPositiveF32` and rejects this at parse time, so
        // `rasterize_svg` never has to handle a zero-size `VectorImage`
        // (see its own doc comment).
        let err = parse_svg(r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#)
            .unwrap_err();
        assert!(matches!(err, AssetError::Svg(_)));
    }

    #[test]
    fn rasterize_svg_rejects_a_zero_sized_raster_target() {
        let image = parse_svg(BLUE_CIRCLE_SVG).unwrap();
        let err = rasterize_svg(
            &image,
            RasterFit::Contain {
                width: 0,
                height: 32,
            },
        )
        .unwrap_err();
        assert!(matches!(err, AssetError::RasterTargetEmpty { .. }));
    }
}
