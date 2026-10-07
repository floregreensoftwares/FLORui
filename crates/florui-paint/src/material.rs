//! The glass material: an opt-in refraction of what is painted behind a
//! panel, with a rim light, for a node that also has a `backdrop-filter`.
//!
//! CSS properties keep their meaning. A node asks for the material with
//! `--florui-glass: refract` and tunes it with `--florui-glass-refraction`,
//! `-edge`, `-light-angle`, `-light-strength` and `-quality` (see
//! [`florui_style::GlassMaterial`]). Absent, or any other keyword, is no
//! material; a parameter that is present but invalid is reported and gives
//! no material. The properties inherit like any custom property; a child
//! that must not refract says `--florui-glass: none`.
//!
//! # Contract
//!
//! **Input and bounds.** The material reads the pixels already painted
//! behind the node, inside the node's border box grown by the displacement
//! limit. A read outside the surface clamps to the nearest edge pixel. It
//! never reads the node's own content, and it only writes inside the node's
//! rounded outline, clipped like any other paint.
//!
//! **Edge geometry.** The edge is the node's rounded outline, with each
//! corner treated as circular (an elliptical corner uses the smaller of its
//! two radii). Distance is measured inward from that outline, and the
//! displacement acts along its outward normal.
//!
//! **Distortion limits.** The peak displacement is the smallest of the
//! requested refraction, the width of the edge band, and [`MAX_REFRACTION`]
//! CSS pixels. A request above that is honored up to the limit and reported
//! as clamped; it is never silently exceeded. The displacement is zero at
//! the inner end of the band and grows toward the edge as the square of the
//! distance into the band, so the interior is untouched.
//!
//! **Effect order.** What is behind the node is sampled, refracted, then run
//! through the node's CSS `backdrop-filter`; then the node's own background,
//! border and shadow paint over it; then the rim light is added over those,
//! under the node's children. Refraction before blur means the blur softens
//! the lens rather than the other way round.
//!
//! **Lighting.** The rim light is white, scaled by the strength, falls off
//! over the same band, and is weighted by how much the outward normal faces
//! the light. It is added in premultiplied space and no channel exceeds the
//! pixel's alpha.
//!
//! **Alpha and color.** Premultiplied throughout. Refraction moves whole
//! premultiplied pixels (bilinearly) so it never changes a pixel's alpha
//! relation to its color. The result is deterministic, and mirror symmetric
//! for a symmetric node over a symmetric backdrop.
//!
//! **Quality.** `full` samples bilinearly. `reduced` samples the nearest
//! pixel, which is cheaper and visibly coarser. `off` is exactly the basic
//! glass (no refraction, no light). The effective mode is what the renderer
//! reports, never a silently lowered one.
//!
//! **Capability.** CPU only: nothing here needs a GPU, and no backend type
//! reaches the component API.
//!
//! # How the displacement is computed
//!
//! For each pixel of the band the signed distance of the outline is
//! evaluated every frame (an `OffsetMap` built once per outline and
//! parameters was prototyped and dropped). Measured in release on this
//! machine, for a panel of 300x200, 600x400 and 1200x800 pixels: the
//! per-frame refraction took 1.43, 3.99 and 10.9 ms; with the table 1.09, 3.30
//! and 9.01 ms (about 15 to 25% less), at 0.2 to 1.4 MiB per panel and a
//! table to invalidate whenever the size, radii, parameters or scale
//! change. Sampling dominates: nearest-pixel (`reduced`) took 0.58, 1.74 and
//! 5.4 ms. The saving did not pay for the cache.

use florui_style::{GlassMaterial, GlassQuality, RoundedRect};
use tiny_skia::{Pixmap, PremultipliedColorU8};

/// The largest displacement ever applied, in CSS pixels.
pub(crate) const MAX_REFRACTION: f32 = 32.0;

/// A material's parameters in painted (physical) pixels, with the limits of
/// the contract applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Params {
    /// Peak displacement.
    pub refraction: f32,
    /// Width of the lensing band.
    pub edge: f32,
    /// Unit vector from the panel toward the light.
    pub light: (f32, f32),
    pub light_strength: f32,
    pub quality: GlassQuality,
    /// The request was above a limit and was cut to it.
    pub clamped: bool,
}

impl Params {
    /// `None` when the quality is `off`: that is the basic glass.
    pub fn resolve(material: &GlassMaterial, scale_factor: f32) -> Option<Params> {
        if material.quality == GlassQuality::Off {
            return None;
        }
        let edge = material.edge * scale_factor;
        let requested = material.refraction * scale_factor;
        let limit = edge.min(MAX_REFRACTION * scale_factor);
        let angle = material.light_angle.to_radians();
        Some(Params {
            refraction: requested.min(limit),
            edge,
            light: (angle.sin(), -angle.cos()),
            light_strength: material.light_strength,
            quality: material.quality,
            clamped: requested > limit + 1e-3,
        })
    }

    /// How far a read can land outside the panel's own box.
    pub fn reach(&self) -> u32 {
        self.refraction.ceil() as u32 + 1
    }
}

/// The distance inward from the outline of a `width` x `height` box with
/// circular corner `radii` (top-left, top-right, bottom-right, bottom-left),
/// at the local point `(px, py)` (negative outside a rounded corner), and the
/// outward unit normal there.
pub(crate) fn edge_geometry(
    width: f32,
    height: f32,
    radii: &[f32; 4],
    px: f32,
    py: f32,
) -> (f32, (f32, f32)) {
    let corners = [
        (radii[0], radii[0], radii[0], px < radii[0] && py < radii[0]),
        (
            width - radii[1],
            radii[1],
            radii[1],
            px > width - radii[1] && py < radii[1],
        ),
        (
            width - radii[2],
            height - radii[2],
            radii[2],
            px > width - radii[2] && py > height - radii[2],
        ),
        (
            radii[3],
            height - radii[3],
            radii[3],
            px < radii[3] && py > height - radii[3],
        ),
    ];
    for (cx, cy, radius, inside_corner) in corners {
        if inside_corner {
            let (vx, vy) = (px - cx, py - cy);
            let length = (vx * vx + vy * vy).sqrt();
            let normal = if length > 1e-6 {
                (vx / length, vy / length)
            } else {
                (
                    -std::f32::consts::FRAC_1_SQRT_2,
                    -std::f32::consts::FRAC_1_SQRT_2,
                )
            };
            return (radius - length, normal);
        }
    }
    let (left, right, top, bottom) = (px, width - px, py, height - py);
    let nearest = left.min(right).min(top).min(bottom);
    let normal = if nearest == left {
        (-1.0, 0.0)
    } else if nearest == right {
        (1.0, 0.0)
    } else if nearest == top {
        (0.0, -1.0)
    } else {
        (0.0, 1.0)
    };
    (nearest, normal)
}

/// The outline's corner radii as circles.
pub(crate) fn circular_radii(outline: &RoundedRect) -> [f32; 4] {
    outline.radii.map(|(h, v)| h.min(v))
}

/// How far a point at `distance` inside the outline is pushed outward.
pub(crate) fn magnitude(params: &Params, distance: f32) -> f32 {
    if params.edge <= 0.0 || distance >= params.edge {
        return 0.0;
    }
    let t = 1.0 - distance.max(0.0) / params.edge;
    params.refraction * t * t
}

/// Where a region of the surface sits against the node's outline.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Region {
    pub width: u32,
    pub height: u32,
    /// The outline's own size.
    pub outline_width: f32,
    pub outline_height: f32,
    /// The region's top-left in the outline's local coordinates.
    pub offset: (f32, f32),
}

/// Calls `visit` for every pixel of `region` that can lie inside the lensing
/// band: all of them except the interior farther than the band from every
/// edge of the outline.
fn for_each_band_pixel(region: &Region, band: f32, mut visit: impl FnMut(u32, u32)) {
    let band = band.ceil() + 1.0;
    let (ox, oy) = region.offset;
    let left_end = ((band - ox - 0.5).ceil().max(0.0) as u32).min(region.width);
    let right_start = ((region.outline_width - band - ox - 0.5).floor() + 1.0)
        .clamp(left_end as f32, region.width as f32) as u32;
    for row in 0..region.height {
        let y = row as f32 + 0.5 + oy;
        if y < band || y > region.outline_height - band {
            for col in 0..region.width {
                visit(col, row);
            }
        } else {
            for col in (0..left_end).chain(right_start..region.width) {
                visit(col, row);
            }
        }
    }
}

/// Where `source` sits in the surface, so a read can land outside the panel.
/// `panel_x`/`panel_y` are the region's own top-left inside `source`.
pub(crate) struct Source<'a> {
    pub pixmap: &'a Pixmap,
    pub panel_x: u32,
    pub panel_y: u32,
}

/// `source`'s pixels inside `region`, refracted, and how many of them
/// moved.
pub(crate) fn refract(
    source: &Source<'_>,
    region: &Region,
    outline: &RoundedRect,
    params: &Params,
) -> Option<(Pixmap, u32)> {
    let (width, height) = (region.width, region.height);
    let mut out = Pixmap::new(width, height)?;
    let src_width = source.pixmap.width();
    let src_pixels = source.pixmap.pixels();
    // Start from the undisturbed pixels: only the band moves.
    for row in 0..height {
        let from = ((source.panel_y + row) * src_width + source.panel_x) as usize;
        out.pixels_mut()[(row * width) as usize..((row + 1) * width) as usize]
            .copy_from_slice(&src_pixels[from..from + width as usize]);
    }
    let sampler = Sampler {
        source,
        bilinear: params.quality == GlassQuality::Full,
    };
    let radii = circular_radii(outline);
    let (ox, oy) = region.offset;
    let mut moved = 0;
    for_each_band_pixel(region, params.edge, |col, row| {
        let (x, y) = (col as f32 + 0.5, row as f32 + 0.5);
        let (distance, normal) = edge_geometry(
            region.outline_width,
            region.outline_height,
            &radii,
            x + ox,
            y + oy,
        );
        let shift = magnitude(params, distance);
        if shift > 0.0 {
            out.pixels_mut()[(row * width + col) as usize] =
                sampler.at(x + normal.0 * shift, y + normal.1 * shift);
            moved += 1;
        }
    });
    Some((out, moved))
}

/// Adds the rim light over `pixels`, the surface's pixels of `region` (row
/// by row, `stride` pixels wide). `coverage` weights each pixel by the
/// outline's own antialiased edge and any clip.
pub(crate) fn add_rim_light(
    pixels: &mut [PremultipliedColorU8],
    stride: usize,
    region: &Region,
    outline: &RoundedRect,
    params: &Params,
    clip_coverage: impl Fn(u32, u32) -> u8,
) {
    if params.light_strength <= 0.0 || params.edge <= 0.0 {
        return;
    }
    let radii = circular_radii(outline);
    let (ox, oy) = region.offset;
    for_each_band_pixel(region, params.edge, |col, row| {
        let (distance, normal) = edge_geometry(
            region.outline_width,
            region.outline_height,
            &radii,
            col as f32 + 0.5 + ox,
            row as f32 + 0.5 + oy,
        );
        let coverage = (distance + 0.5).clamp(0.0, 1.0) * (clip_coverage(col, row) as f32 / 255.0);
        if coverage <= 0.0 || distance >= params.edge {
            return;
        }
        let facing = (normal.0 * params.light.0 + normal.1 * params.light.1).max(0.0);
        let t = 1.0 - distance.max(0.0) / params.edge;
        let light = params.light_strength * t * t * facing * coverage;
        if light <= 0.0 {
            return;
        }
        let pixel = &mut pixels[row as usize * stride + col as usize];
        let alpha = pixel.alpha();
        let add = (light * alpha as f32).round() as u16;
        let lit = |channel: u8| (channel as u16 + add).min(alpha as u16) as u8;
        if let Some(lit) = PremultipliedColorU8::from_rgba(
            lit(pixel.red()),
            lit(pixel.green()),
            lit(pixel.blue()),
            alpha,
        ) {
            *pixel = lit;
        }
    });
}

struct Sampler<'a, 'b> {
    source: &'a Source<'b>,
    bilinear: bool,
}

impl Sampler<'_, '_> {
    /// The pixel at the panel-local point `(x, y)`, clamping at the surface's
    /// edge.
    fn at(&self, x: f32, y: f32) -> PremultipliedColorU8 {
        let pixmap = self.source.pixmap;
        let (width, height) = (pixmap.width() as i64, pixmap.height() as i64);
        let sx = x + self.source.panel_x as f32 - 0.5;
        let sy = y + self.source.panel_y as f32 - 0.5;
        let pixel = |col: i64, row: i64| {
            pixmap.pixels()[(row.clamp(0, height - 1) * width + col.clamp(0, width - 1)) as usize]
        };
        if !self.bilinear {
            return pixel((sx + 0.5).floor() as i64, (sy + 0.5).floor() as i64);
        }
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let taps = [
            (pixel(x0, y0), (1.0 - fx) * (1.0 - fy)),
            (pixel(x0 + 1, y0), fx * (1.0 - fy)),
            (pixel(x0, y0 + 1), (1.0 - fx) * fy),
            (pixel(x0 + 1, y0 + 1), fx * fy),
        ];
        let mix = |channel: fn(&PremultipliedColorU8) -> u8| -> f32 {
            taps.iter().map(|(p, w)| channel(p) as f32 * w).sum()
        };
        let alpha = mix(|p| p.alpha()).round().clamp(0.0, 255.0) as u8;
        let channel = |value: f32| (value.round().clamp(0.0, 255.0) as u8).min(alpha);
        PremultipliedColorU8::from_rgba(
            channel(mix(|p| p.red())),
            channel(mix(|p| p.green())),
            channel(mix(|p| p.blue())),
            alpha,
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(refraction: f32, edge: f32) -> Params {
        Params::resolve(
            &GlassMaterial {
                refraction,
                edge,
                light_angle: 315.0,
                light_strength: 0.0,
                quality: GlassQuality::Full,
            },
            1.0,
        )
        .expect("not off")
    }

    #[test]
    fn the_peak_is_the_smallest_of_the_request_the_band_and_the_limit() {
        assert_eq!(params(8.0, 20.0).refraction, 8.0);
        assert!(!params(8.0, 20.0).clamped);
        assert_eq!(params(30.0, 12.0).refraction, 12.0, "cut to the band");
        assert!(params(30.0, 12.0).clamped);
        assert_eq!(
            params(60.0, 80.0).refraction,
            MAX_REFRACTION,
            "cut to the limit"
        );
        assert!(params(60.0, 80.0).clamped);
    }

    #[test]
    fn off_is_no_material_and_the_limits_scale_with_the_canvas() {
        let off = GlassMaterial {
            refraction: 8.0,
            edge: 20.0,
            light_angle: 0.0,
            light_strength: 0.0,
            quality: GlassQuality::Off,
        };
        assert_eq!(Params::resolve(&off, 1.0), None);
        let scaled = Params::resolve(
            &GlassMaterial {
                quality: GlassQuality::Full,
                ..off
            },
            2.0,
        )
        .expect("not off");
        assert_eq!((scaled.refraction, scaled.edge), (16.0, 40.0));
        let limit = Params::resolve(
            &GlassMaterial {
                refraction: 100.0,
                edge: 100.0,
                quality: GlassQuality::Full,
                ..off
            },
            2.0,
        )
        .expect("not off");
        assert_eq!(limit.refraction, MAX_REFRACTION * 2.0);
    }

    #[test]
    fn the_magnitude_is_zero_at_the_end_of_the_band_and_peaks_at_the_edge() {
        let p = params(10.0, 20.0);
        assert_eq!(magnitude(&p, 20.0), 0.0);
        assert_eq!(magnitude(&p, 35.0), 0.0);
        assert_eq!(magnitude(&p, 0.0), 10.0);
        assert!(magnitude(&p, 5.0) > magnitude(&p, 10.0));
    }

    #[test]
    fn the_edge_geometry_follows_a_rounded_outline() {
        let radii = [10.0; 4];
        let (distance, normal) = edge_geometry(100.0, 60.0, &radii, 50.0, 3.0);
        assert_eq!((distance, normal), (3.0, (0.0, -1.0)), "top edge");
        let (distance, normal) = edge_geometry(100.0, 60.0, &radii, 97.0, 30.0);
        assert_eq!((distance, normal), (3.0, (1.0, 0.0)), "right edge");
        // On the diagonal of the top-left corner arc.
        let on_arc = 10.0 - 10.0 / std::f32::consts::SQRT_2;
        let (distance, normal) = edge_geometry(100.0, 60.0, &radii, on_arc + 1.0, on_arc + 1.0);
        assert!(distance < 1.5 && distance > 0.0, "{distance}");
        assert!(normal.0 < 0.0 && normal.1 < 0.0);
        // The square corner point is outside the arc.
        let (distance, _) = edge_geometry(100.0, 60.0, &radii, 0.5, 0.5);
        assert!(distance < 0.0, "{distance}");
    }

    /// A test backdrop: vertical stripes, so any displacement is visible.
    fn stripes(width: u32, height: u32) -> Pixmap {
        let mut pixmap = Pixmap::new(width, height).expect("size");
        for row in 0..height {
            for col in 0..width {
                let v = if (col / 4) % 2 == 0 { 230 } else { 20 };
                pixmap.pixels_mut()[(row * width + col) as usize] =
                    PremultipliedColorU8::from_rgba(v, v / 2, 255 - v, 255).expect("opaque");
            }
        }
        pixmap
    }

    fn outline(width: f32, height: f32, radius: f32) -> RoundedRect {
        RoundedRect {
            x: 0.0,
            y: 0.0,
            width,
            height,
            radii: [(radius, radius); 4],
        }
    }

    fn full(width: u32, height: u32) -> Region {
        Region {
            width,
            height,
            outline_width: width as f32,
            outline_height: height as f32,
            offset: (0.0, 0.0),
        }
    }

    fn run(p: &Params, quality_nearest: bool) -> Pixmap {
        let (margin, w, h) = (p.reach(), 120u32, 80u32);
        let source = stripes(w + 2 * margin, h + 2 * margin);
        let mut p = *p;
        if quality_nearest {
            p.quality = GlassQuality::Reduced;
        }
        refract(
            &Source {
                pixmap: &source,
                panel_x: margin,
                panel_y: margin,
            },
            &full(w, h),
            &outline(w as f32, h as f32, 16.0),
            &p,
        )
        .expect("size")
        .0
    }

    #[test]
    fn zero_refraction_leaves_the_backdrop_byte_identical() {
        let p = params(0.0, 20.0);
        let out = run(&p, false);
        let margin = p.reach();
        let source = stripes(120 + 2 * margin, 80 + 2 * margin);
        for row in 0..80 {
            for col in 0..120 {
                assert_eq!(
                    out.pixels()[(row * 120 + col) as usize],
                    source.pixels()[((row + margin) * (120 + 2 * margin) + col + margin) as usize]
                );
            }
        }
    }

    #[test]
    fn the_interior_is_untouched_and_the_band_moves() {
        let p = params(10.0, 20.0);
        let out = run(&p, false);
        let margin = p.reach();
        let source = stripes(120 + 2 * margin, 80 + 2 * margin);
        let src = |col: u32, row: u32| {
            source.pixels()[((row + margin) * (120 + 2 * margin) + col + margin) as usize]
        };
        for row in 25..55 {
            for col in 25..95 {
                assert_eq!(
                    out.pixels()[(row * 120 + col) as usize],
                    src(col, row),
                    "({col}, {row})"
                );
            }
        }
        let moved = (0..80)
            .flat_map(|row| (0..120).map(move |col| (col, row)))
            .filter(|&(col, row)| out.pixels()[(row * 120 + col) as usize] != src(col, row))
            .count();
        assert!(moved > 100, "only {moved} pixels moved");
    }

    #[test]
    fn no_pixel_exceeds_its_alpha() {
        let p = params(12.0, 20.0);
        let out = run(&p, false);
        for pixel in out.pixels() {
            assert!(pixel.red() <= pixel.alpha());
            assert!(pixel.green() <= pixel.alpha());
            assert!(pixel.blue() <= pixel.alpha());
        }
    }

    #[test]
    fn a_symmetric_panel_over_a_symmetric_backdrop_refracts_symmetrically() {
        // Left-right mirror: an even stripe pattern centered on the panel.
        let (margin, w, h) = (13u32, 120u32, 80u32);
        let total = w + 2 * margin;
        let mut source = Pixmap::new(total, h + 2 * margin).expect("size");
        for row in 0..h + 2 * margin {
            for col in 0..total {
                let mirrored = col.min(total - 1 - col);
                let v = if (mirrored / 4) % 2 == 0 { 230 } else { 20 };
                source.pixels_mut()[(row * total + col) as usize] =
                    PremultipliedColorU8::from_rgba(v, v / 2, 255 - v, 255).expect("opaque");
            }
        }
        let p = params(12.0, 20.0);
        let (out, _) = refract(
            &Source {
                pixmap: &source,
                panel_x: margin,
                panel_y: margin,
            },
            &full(w, h),
            &outline(w as f32, h as f32, 16.0),
            &p,
        )
        .expect("size");
        for row in 0..h {
            for col in 0..w / 2 {
                let left = out.pixels()[(row * w + col) as usize];
                let right = out.pixels()[(row * w + (w - 1 - col)) as usize];
                let close = |a: u8, b: u8| a.abs_diff(b) <= 1;
                assert!(
                    close(left.red(), right.red()) && close(left.blue(), right.blue()),
                    "({col}, {row}): {left:?} against {right:?}"
                );
            }
        }
    }

    #[test]
    fn the_output_is_deterministic() {
        let p = params(10.0, 20.0);
        let a = run(&p, false);
        let b = run(&p, false);
        assert_eq!(a.data(), b.data());
    }

    /// A measurement, not a check: `cargo test -p florui-paint --release
    /// refraction_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn refraction_cost() {
        use std::time::Instant;
        for (w, h, radius, refraction, edge) in [
            (300u32, 200u32, 16.0f32, 10.0f32, 20.0f32),
            (600, 400, 24.0, 12.0, 24.0),
            (1200, 800, 28.0, 16.0, 32.0),
        ] {
            let p = params(refraction, edge);
            let margin = p.reach();
            let source = stripes(w + 2 * margin, h + 2 * margin);
            let rect = outline(w as f32, h as f32, radius);
            let src = Source {
                pixmap: &source,
                panel_x: margin,
                panel_y: margin,
            };
            let rounds = 40;
            let time = |p: &Params| {
                let t = Instant::now();
                for _ in 0..rounds {
                    std::hint::black_box(refract(&src, &full(w, h), &rect, p));
                }
                t.elapsed() / rounds
            };
            let mut reduced = p;
            reduced.quality = GlassQuality::Reduced;
            println!(
                "{w}x{h} r={radius} R={refraction} E={edge}: full {:?}, reduced {:?}",
                time(&p),
                time(&reduced)
            );
        }
    }
}
