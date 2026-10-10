//! CSS gradients as pixels: the stop list CSS describes (positions that may be
//! missing, hints between stops), the table of colors it makes, and the three
//! shapes (linear, radial, conic) that turn a position into a place in that
//! table. Colors are interpolated premultiplied, as CSS says.
//!
//! Everything here works in one tile's own pixel space, with the origin at its
//! top-left corner and lengths already in device pixels; the caller decides
//! where the tile sits and how often it repeats.

use florui_style::{
    ConicGradient, ConicItem, GradientItem, LengthPercentage, LinearDirection, LinearGradient,
    RadialExtent, RadialGradient, RadialSize, Rgba,
};

/// How many colors the table holds across one gradient. The table is read
/// with the position rounded to the nearest entry.
const TABLE_SIZE: usize = 2048;

/// How many extra stops stand in for the curve a hint asks for.
const HINT_STEPS: usize = 16;

/// A stop once its position is known: along the gradient line, in pixels for
/// a linear or radial gradient and in turns for a conic one. The color is
/// premultiplied, each channel `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Stop {
    pub position: f32,
    pub color: [f32; 4],
}

/// An item with its position still optional, in the unit of the gradient.
#[derive(Debug, Clone, Copy)]
enum Item {
    Stop { color: Rgba, position: Option<f32> },
    Hint(f32),
}

fn premultiplied(color: Rgba) -> [f32; 4] {
    let alpha = f32::from(color.a) / 255.0;
    [
        f32::from(color.r) / 255.0 * alpha,
        f32::from(color.g) / 255.0 * alpha,
        f32::from(color.b) / 255.0 * alpha,
        alpha,
    ]
}

fn mix(from: [f32; 4], to: [f32; 4], t: f32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for ((out, a), b) in out.iter_mut().zip(from).zip(to) {
        *out = a + (b - a) * t;
    }
    out
}

/// CSS's color-stop fix-up: a first or last stop with no position sits at the
/// start or end, a position before an earlier one is raised to it, and a run
/// of stops with no position is spread evenly. Hints are kept between the stops
/// they sit among, raised or lowered into that span, then each becomes a few
/// extra stops that follow the curve it describes.
///
/// \`end\` is where an unpositioned last stop goes: the gradient line's length
/// in pixels, or one turn.
fn resolve(items: &[Item], end: f32) -> Vec<Stop> {
    // Stops first: only they take part in the fix-up of positions.
    let mut stops: Vec<(Rgba, Option<f32>)> = Vec::new();
    let mut hints: Vec<(usize, f32)> = Vec::new();
    for item in items {
        match *item {
            Item::Stop { color, position } => stops.push((color, position)),
            // A hint belongs between the stop before it and the one after.
            Item::Hint(position) => hints.push((stops.len(), position)),
        }
    }
    if stops.is_empty() {
        return Vec::new();
    }
    let last = stops.len() - 1;
    if stops[0].1.is_none() {
        stops[0].1 = Some(0.0);
    }
    if stops[last].1.is_none() {
        stops[last].1 = Some(end);
    }
    // No stop may sit before one that came earlier.
    let mut highest = f32::NEG_INFINITY;
    for stop in stops.iter_mut() {
        if let Some(position) = stop.1 {
            let raised = position.max(highest);
            stop.1 = Some(raised);
            highest = raised;
        }
    }
    // A run with no positions is spread between its neighbors.
    let mut index = 0;
    while index <= last {
        if stops[index].1.is_some() {
            index += 1;
            continue;
        }
        let run_start = index;
        while stops[index].1.is_none() {
            index += 1;
        }
        let before = stops[run_start - 1].1.unwrap_or(0.0);
        let after = stops[index].1.unwrap_or(end);
        let count = (index - run_start + 1) as f32;
        for (offset, stop) in stops[run_start..index].iter_mut().enumerate() {
            stop.1 = Some(before + (after - before) * (offset + 1) as f32 / count);
        }
    }

    let mut out: Vec<Stop> = Vec::with_capacity(stops.len() + hints.len() * HINT_STEPS);
    for (position, &(color, at)) in stops.iter().enumerate() {
        let at = at.unwrap_or(0.0);
        if position > 0 {
            let before = out.last().copied();
            if let (Some(before), Some(&(_, hint))) =
                (before, hints.iter().find(|(after, _)| *after == position))
            {
                let span = at - before.position;
                if span > 0.0 {
                    // The hint is where the blend reaches the middle; clamped
                    // into the span it cannot be at either end.
                    let fraction = ((hint - before.position) / span).clamp(0.001, 0.999);
                    let exponent = 0.5f32.ln() / fraction.ln();
                    for step in 1..HINT_STEPS {
                        let t = step as f32 / HINT_STEPS as f32;
                        out.push(Stop {
                            position: before.position + span * t,
                            color: mix(before.color, premultiplied(color), t.powf(exponent)),
                        });
                    }
                }
            }
        }
        out.push(Stop {
            position: at,
            color: premultiplied(color),
        });
    }
    out
}

/// The colors of a gradient from its first stop to its last, \`TABLE_SIZE\`
/// entries of premultiplied RGBA bytes.
#[derive(Debug, Clone)]
pub(crate) struct Table {
    entries: Vec<[u8; 4]>,
    /// Where the first and last stops are, in the gradient's unit.
    first: f32,
    last: f32,
    repeating: bool,
}

impl Table {
    fn new(stops: &[Stop], repeating: bool) -> Option<Self> {
        let (first, last) = (stops.first()?.position, stops.last()?.position);
        let span = (last - first).max(1.0e-6);
        let mut entries = Vec::with_capacity(TABLE_SIZE);
        let mut segment = 0;
        for index in 0..TABLE_SIZE {
            let position = first + span * index as f32 / (TABLE_SIZE - 1) as f32;
            while segment + 1 < stops.len() - 1 && stops[segment + 1].position <= position {
                segment += 1;
            }
            let color = if stops.len() == 1 {
                stops[0].color
            } else {
                let (a, b) = (stops[segment], stops[segment + 1]);
                let width = b.position - a.position;
                // A hard stop: at or past the later one, that one's color.
                if position >= b.position && width <= 0.0 {
                    b.color
                } else if width <= 0.0 {
                    a.color
                } else {
                    mix(
                        a.color,
                        b.color,
                        ((position - a.position) / width).clamp(0.0, 1.0),
                    )
                }
            };
            entries.push(color.map(|channel| (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
        }
        Some(Self {
            entries,
            first,
            last,
            repeating,
        })
    }

    /// The color at \`position\` (in the gradient's unit), padded past the ends
    /// or repeated.
    #[inline]
    fn at(&self, position: f32) -> [u8; 4] {
        let span = (self.last - self.first).max(1.0e-6);
        let mut t = (position - self.first) / span;
        if self.repeating {
            t -= t.floor();
        }
        let index = (t.clamp(0.0, 1.0) * (TABLE_SIZE - 1) as f32 + 0.5) as usize;
        self.entries[index.min(TABLE_SIZE - 1)]
    }

    /// For a repeating gradient whose stops all sit at one place, CSS paints
    /// the average color.
    fn averaged(stops: &[Stop]) -> [u8; 4] {
        let mut sum = [0.0f32; 4];
        for stop in stops {
            for (total, channel) in sum.iter_mut().zip(stop.color) {
                *total += channel;
            }
        }
        sum.map(|total| (total / stops.len() as f32 * 255.0 + 0.5) as u8)
    }
}

/// A gradient ready to be read at a pixel of its tile.
pub(crate) enum Shader {
    Solid([u8; 4]),
    Linear {
        table: Table,
        /// The gradient line's center and unit direction, and its length.
        center: (f32, f32),
        direction: (f32, f32),
        length: f32,
    },
    Radial {
        table: Table,
        center: (f32, f32),
        /// The ellipse's radii; a circle has them equal. Distances are
        /// measured in units of \`radius_x\`.
        radius_x: f32,
        radius_y: f32,
    },
    Conic {
        table: Table,
        center: (f32, f32),
        /// Degrees clockwise from the top.
        from: f32,
    },
}

fn px(value: LengthPercentage, basis: f32, scale: f32) -> f32 {
    value.length * scale + value.percentage * basis
}

fn linear_items(items: &[GradientItem], line: f32, scale: f32) -> Vec<Item> {
    items
        .iter()
        .map(|item| match *item {
            GradientItem::Stop { color, position } => Item::Stop {
                color,
                position: position.map(|p| px(p, line, scale)),
            },
            GradientItem::Hint(position) => Item::Hint(px(position, line, scale)),
        })
        .collect()
}

fn table_or_solid(stops: &[Stop], repeating: bool) -> Result<Table, [u8; 4]> {
    let (Some(first), Some(last)) = (stops.first(), stops.last()) else {
        return Err([0; 4]);
    };
    if repeating && last.position - first.position <= 1.0e-6 {
        return Err(Table::averaged(stops));
    }
    Table::new(stops, repeating).ok_or([0; 4])
}

impl Shader {
    /// A linear gradient painted into a \`width x height\` tile.
    pub(crate) fn linear(gradient: &LinearGradient, width: f32, height: f32, scale: f32) -> Self {
        let (sin, cos, direction) = match gradient.direction {
            LinearDirection::Angle(degrees) => {
                let radians = degrees.to_radians();
                (
                    radians.sin(),
                    radians.cos(),
                    (radians.sin(), -radians.cos()),
                )
            }
            LinearDirection::Corner { right, bottom } => {
                // The line is perpendicular to the diagonal between the two
                // corners it does not point at, so the 50% color passes through
                // both.
                let x = if right { height } else { -height };
                let y = if bottom { width } else { -width };
                let norm = x.hypot(y).max(1.0e-6);
                let direction = (x / norm, y / norm);
                (direction.0, -direction.1, direction)
            }
        };
        let length = (width * sin).abs() + (height * cos).abs();
        let stops = resolve(&linear_items(&gradient.items, length, scale), length);
        match table_or_solid(&stops, gradient.repeating) {
            Ok(table) => Self::Linear {
                table,
                center: (width / 2.0, height / 2.0),
                direction,
                length,
            },
            Err(color) => Self::Solid(color),
        }
    }

    /// A radial gradient painted into a \`width x height\` tile.
    pub(crate) fn radial(gradient: &RadialGradient, width: f32, height: f32, scale: f32) -> Self {
        let center = (
            px(gradient.center.0, width, scale),
            px(gradient.center.1, height, scale),
        );
        let (radius_x, radius_y) = radial_radii(gradient.size, center, (width, height), scale);
        let (radius_x, radius_y) = (radius_x.max(1.0e-3), radius_y.max(1.0e-3));
        let stops = resolve(&linear_items(&gradient.items, radius_x, scale), radius_x);
        match table_or_solid(&stops, gradient.repeating) {
            Ok(table) => Self::Radial {
                table,
                center,
                radius_x,
                radius_y,
            },
            Err(color) => Self::Solid(color),
        }
    }

    /// A conic gradient painted into a \`width x height\` tile.
    pub(crate) fn conic(gradient: &ConicGradient, width: f32, height: f32, scale: f32) -> Self {
        let items: Vec<Item> = gradient
            .items
            .iter()
            .map(|item| match *item {
                ConicItem::Stop { color, position } => Item::Stop { color, position },
                ConicItem::Hint(position) => Item::Hint(position),
            })
            .collect();
        let stops = resolve(&items, 1.0);
        match table_or_solid(&stops, gradient.repeating) {
            Ok(table) => Self::Conic {
                table,
                center: (
                    px(gradient.center.0, width, scale),
                    px(gradient.center.1, height, scale),
                ),
                from: gradient.from,
            },
            Err(color) => Self::Solid(color),
        }
    }

    /// The premultiplied color at the center of the pixel at \`(x, y)\` of the tile.
    #[inline]
    pub(crate) fn color_at(&self, x: f32, y: f32) -> [u8; 4] {
        match self {
            Self::Solid(color) => *color,
            Self::Linear {
                table,
                center,
                direction,
                length,
            } => {
                let along = (x - center.0) * direction.0 + (y - center.1) * direction.1;
                table.at(along + length / 2.0)
            }
            Self::Radial {
                table,
                center,
                radius_x,
                radius_y,
            } => {
                let dx = x - center.0;
                let dy = (y - center.1) * (radius_x / radius_y);
                table.at(dx.hypot(dy))
            }
            Self::Conic {
                table,
                center,
                from,
            } => {
                let degrees = (x - center.0).atan2(-(y - center.1)).to_degrees();
                let turns = (degrees - from).rem_euclid(360.0) / 360.0;
                table.at(turns)
            }
        }
    }
}

/// The radii of a radial gradient's ending shape in a box of the given size
/// with the center at \`center\`.
fn radial_radii(
    size: RadialSize,
    center: (f32, f32),
    (width, height): (f32, f32),
    scale: f32,
) -> (f32, f32) {
    let (left, right) = (center.0, width - center.0);
    let (top, bottom) = (center.1, height - center.1);
    match size {
        RadialSize::Circle(radius) => (radius * scale, radius * scale),
        RadialSize::Ellipse(rx, ry) => (px(rx, width, scale), px(ry, height, scale)),
        RadialSize::Extent { circle, extent } => {
            let closest_side = (left.abs().min(right.abs()), top.abs().min(bottom.abs()));
            let farthest_side = (left.abs().max(right.abs()), top.abs().max(bottom.abs()));
            let corner = |far: bool| {
                let xs = [left.abs(), right.abs()];
                let ys = [top.abs(), bottom.abs()];
                let pick = |values: [f32; 2]| {
                    if far {
                        values[0].max(values[1])
                    } else {
                        values[0].min(values[1])
                    }
                };
                (pick(xs), pick(ys))
            };
            match (extent, circle) {
                (RadialExtent::ClosestSide, true) => {
                    let r = closest_side.0.min(closest_side.1);
                    (r, r)
                }
                (RadialExtent::FarthestSide, true) => {
                    let r = farthest_side.0.max(farthest_side.1);
                    (r, r)
                }
                (RadialExtent::ClosestSide, false) => closest_side,
                (RadialExtent::FarthestSide, false) => farthest_side,
                (RadialExtent::ClosestCorner, true) => {
                    let (dx, dy) = corner(false);
                    let r = dx.hypot(dy);
                    (r, r)
                }
                (RadialExtent::FarthestCorner, true) => {
                    let (dx, dy) = corner(true);
                    let r = dx.hypot(dy);
                    (r, r)
                }
                (RadialExtent::ClosestCorner, false) => {
                    scaled_through_corner(closest_side, corner(false))
                }
                (RadialExtent::FarthestCorner, false) => {
                    scaled_through_corner(farthest_side, corner(true))
                }
            }
        }
    }
}

/// An ellipse with the proportions of \`radii\` made just large enough to pass
/// through \`corner\` (measured from the center).
fn scaled_through_corner(radii: (f32, f32), corner: (f32, f32)) -> (f32, f32) {
    let (rx, ry) = (radii.0.max(1.0e-3), radii.1.max(1.0e-3));
    let factor = ((corner.0 / rx).powi(2) + (corner.1 / ry).powi(2)).sqrt();
    (rx * factor, ry * factor)
}

/// How much of the pixel `[index, index + 1)` the span `[start, end)` covers.
#[inline]
fn coverage(index: usize, start: f32, end: f32) -> f32 {
    ((index + 1) as f32).min(end).sub_floor(index as f32, start)
}

trait SubFloor {
    fn sub_floor(self, low: f32, start: f32) -> f32;
}

impl SubFloor for f32 {
    /// `self - max(low, start)`, never below zero.
    #[inline]
    fn sub_floor(self, low: f32, start: f32) -> f32 {
        (self - low.max(start)).clamp(0.0, 1.0)
    }
}

/// Writes the gradient into the pixels of `target` (`width` pixels wide, four
/// premultiplied bytes each, row-major) in `rect` (`x0, y0, x1, y1` in whole
/// pixels), for a tile whose edges are at `tile` (`left, top, right, bottom`,
/// in the same pixels, possibly fractional). The tile's own origin is its
/// top-left corner. A pixel the tile only partly covers is blended in by that
/// share, over what is already there, so a tile that does not start on a pixel
/// has soft edges like the browser's.
pub(crate) fn fill(
    target: &mut [u8],
    width: usize,
    rect: (usize, usize, usize, usize),
    tile: (f32, f32, f32, f32),
    shader: &Shader,
) {
    let (x0, y0, x1, y1) = rect;
    let (left, top, right, bottom) = tile;
    for y in y0..y1 {
        let row_share = coverage(y, top, bottom);
        if row_share <= 0.0 {
            continue;
        }
        let local_y = y as f32 + 0.5 - top;
        let row = &mut target[(y * width + x0) * 4..(y * width + x1) * 4];
        for (offset, pixel) in row.chunks_exact_mut(4).enumerate() {
            let x = x0 + offset;
            let share = row_share * coverage(x, left, right);
            if share <= 0.0 {
                continue;
            }
            let color = shader.color_at(x as f32 + 0.5 - left, local_y);
            if share >= 1.0 {
                pixel.copy_from_slice(&color);
            } else {
                // Source over: the part of what is there that the tile does
                // not cover stays.
                let alpha = f32::from(color[3]) / 255.0 * share;
                for (out, &channel) in pixel.iter_mut().zip(&color) {
                    let blended = f32::from(channel) * share + f32::from(*out) * (1.0 - alpha);
                    *out = (blended + 0.5).min(255.0) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(r: u8, g: u8, b: u8) -> Rgba {
        Rgba::opaque(r, g, b)
    }

    fn stop(color: Rgba, position: Option<f32>) -> Item {
        Item::Stop { color, position }
    }

    fn positions(stops: &[Stop]) -> Vec<f32> {
        stops.iter().map(|s| s.position).collect()
    }

    #[test]
    fn missing_positions_are_spread_evenly_between_the_ends() {
        let red = color(255, 0, 0);
        let stops = resolve(
            &[
                stop(red, None),
                stop(red, None),
                stop(red, None),
                stop(red, None),
                stop(red, None),
            ],
            100.0,
        );
        assert_eq!(positions(&stops), vec![0.0, 25.0, 50.0, 75.0, 100.0]);
    }

    #[test]
    fn a_position_before_an_earlier_one_is_raised_to_it() {
        let red = color(255, 0, 0);
        let stops = resolve(
            &[
                stop(red, Some(40.0)),
                stop(red, Some(10.0)),
                stop(red, Some(60.0)),
            ],
            100.0,
        );
        assert_eq!(positions(&stops), vec![40.0, 40.0, 60.0]);
    }

    #[test]
    fn a_run_without_positions_is_spread_between_the_stops_that_have_them() {
        let red = color(255, 0, 0);
        let stops = resolve(
            &[
                stop(red, Some(10.0)),
                stop(red, None),
                stop(red, None),
                stop(red, Some(70.0)),
            ],
            100.0,
        );
        assert_eq!(positions(&stops), vec![10.0, 30.0, 50.0, 70.0]);
    }

    #[test]
    fn a_hint_bends_the_blend_so_the_middle_color_is_at_the_hint() {
        let black = color(0, 0, 0);
        let white = color(255, 255, 255);
        let items = [
            stop(black, Some(0.0)),
            Item::Hint(25.0),
            stop(white, Some(100.0)),
        ];
        let table = Table::new(&resolve(&items, 100.0), false).expect("a table");
        // The halfway color sits at the hint, a quarter of the way along.
        let middle = table.at(25.0)[0];
        assert!((i32::from(middle) - 128).abs() <= 4, "{middle}");
        // Without the hint the same position is a quarter of the way to white.
        let plain = Table::new(
            &resolve(&[stop(black, Some(0.0)), stop(white, Some(100.0))], 100.0),
            false,
        )
        .expect("a table");
        assert!((i32::from(plain.at(25.0)[0]) - 64).abs() <= 2);
    }

    #[test]
    fn a_hard_stop_changes_color_at_once() {
        let items = [
            stop(color(255, 0, 0), Some(0.0)),
            stop(color(255, 0, 0), Some(50.0)),
            stop(color(0, 0, 255), Some(50.0)),
            stop(color(0, 0, 255), Some(100.0)),
        ];
        let table = Table::new(&resolve(&items, 100.0), false).expect("a table");
        assert_eq!(table.at(49.0), [255, 0, 0, 255]);
        assert_eq!(table.at(51.0), [0, 0, 255, 255]);
    }

    #[test]
    fn colors_are_interpolated_premultiplied() {
        // Red to transparent white: halfway is half-transparent red. Mixing the
        // straight colors would turn it pink, because the white that is not
        // there would still count.
        let items = [
            stop(color(255, 0, 0), Some(0.0)),
            stop(
                Rgba {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 0,
                },
                Some(100.0),
            ),
        ];
        let table = Table::new(&resolve(&items, 100.0), false).expect("a table");
        let [r, g, b, a] = table.at(50.0);
        assert!((i32::from(a) - 128).abs() <= 2, "{a}");
        assert!(
            (i32::from(r) - 128).abs() <= 2,
            "{r}: premultiplied red follows the alpha"
        );
        assert_eq!((g, b), (0, 0), "no white leaks in");
    }

    #[test]
    fn a_table_pads_past_the_ends_and_a_repeating_one_repeats() {
        let items = [
            stop(color(255, 0, 0), Some(20.0)),
            stop(color(0, 0, 255), Some(40.0)),
        ];
        let stops = resolve(&items, 100.0);
        let pad = Table::new(&stops, false).expect("a table");
        assert_eq!(pad.at(-50.0), [255, 0, 0, 255]);
        assert_eq!(pad.at(500.0), [0, 0, 255, 255]);
        let repeat = Table::new(&stops, true).expect("a table");
        // One period later is the same color.
        let a = repeat.at(25.0);
        let b = repeat.at(45.0);
        assert!(
            a[0].abs_diff(b[0]) <= 1 && a[2].abs_diff(b[2]) <= 1,
            "{a:?} {b:?}"
        );
    }

    #[test]
    fn a_repeating_gradient_with_no_width_paints_the_average() {
        let stops = resolve(
            &[
                stop(color(255, 0, 0), Some(10.0)),
                stop(color(0, 0, 255), Some(10.0)),
            ],
            100.0,
        );
        assert_eq!(table_or_solid(&stops, true).err(), Some([128, 0, 128, 255]));
    }

    fn linear(direction: LinearDirection, width: f32, height: f32) -> Shader {
        let gradient = LinearGradient {
            direction,
            items: vec![
                GradientItem::Stop {
                    color: color(0, 0, 0),
                    position: None,
                },
                GradientItem::Stop {
                    color: color(255, 255, 255),
                    position: None,
                },
            ],
            repeating: false,
        };
        Shader::linear(&gradient, width, height, 1.0)
    }

    #[test]
    fn a_linear_gradient_runs_from_one_side_to_the_other() {
        let shader = linear(LinearDirection::Angle(180.0), 100.0, 200.0);
        assert!(shader.color_at(50.0, 0.5)[0] <= 2);
        assert!((i32::from(shader.color_at(50.0, 100.0)[0]) - 128).abs() <= 2);
        assert!(shader.color_at(50.0, 199.5)[0] >= 253);
        // A sideways step changes nothing.
        assert_eq!(shader.color_at(5.0, 120.0), shader.color_at(95.0, 120.0));
        // To the right is the other axis.
        let shader = linear(LinearDirection::Angle(90.0), 100.0, 200.0);
        assert!(shader.color_at(0.5, 100.0)[0] <= 2 && shader.color_at(99.5, 100.0)[0] >= 253);
    }

    #[test]
    fn a_diagonal_linear_gradient_reaches_its_corners_exactly() {
        // 45deg in a square: the line is as long as the diagonal projected, so
        // the first and last colors are the corners.
        let shader = linear(LinearDirection::Angle(135.0), 100.0, 100.0);
        assert!(shader.color_at(0.5, 0.5)[0] <= 2);
        assert!(shader.color_at(99.5, 99.5)[0] >= 253);
        // The two other corners sit at the middle color.
        for corner in [(99.5, 0.5), (0.5, 99.5)] {
            let [value, ..] = shader.color_at(corner.0, corner.1);
            assert!((i32::from(value) - 128).abs() <= 3, "{corner:?}: {value}");
        }
    }

    #[test]
    fn a_corner_direction_puts_the_middle_color_on_the_other_diagonal() {
        let shader = linear(
            LinearDirection::Corner {
                right: true,
                bottom: true,
            },
            300.0,
            100.0,
        );
        for corner in [(299.5, 0.5), (0.5, 99.5)] {
            let [value, ..] = shader.color_at(corner.0, corner.1);
            assert!((i32::from(value) - 128).abs() <= 3, "{corner:?}: {value}");
        }
        assert!(shader.color_at(0.5, 0.5)[0] <= 2);
        assert!(shader.color_at(299.5, 99.5)[0] >= 253);
    }

    #[test]
    fn a_radial_gradient_follows_its_extent() {
        let gradient = |size| RadialGradient {
            size,
            center: (
                LengthPercentage {
                    length: 0.0,
                    percentage: 0.5,
                },
                LengthPercentage {
                    length: 0.0,
                    percentage: 0.5,
                },
            ),
            items: vec![
                GradientItem::Stop {
                    color: color(0, 0, 0),
                    position: None,
                },
                GradientItem::Stop {
                    color: color(255, 255, 255),
                    position: None,
                },
            ],
            repeating: false,
        };
        // A circle to the closest side of a 200x100 box reaches 50px.
        let closest = Shader::radial(
            &gradient(RadialSize::Extent {
                circle: true,
                extent: RadialExtent::ClosestSide,
            }),
            200.0,
            100.0,
            1.0,
        );
        assert!(closest.color_at(100.0, 50.0)[0] <= 2);
        assert!(closest.color_at(100.0, 99.0)[0] >= 250);
        assert!(
            closest.color_at(190.0, 50.0)[0] == 255,
            "past the radius it stays at the last color"
        );
        // The default ellipse to the farthest corner passes through the corners.
        let corner = Shader::radial(
            &gradient(RadialSize::Extent {
                circle: false,
                extent: RadialExtent::FarthestCorner,
            }),
            200.0,
            100.0,
            1.0,
        );
        assert!(corner.color_at(0.5, 0.5)[0] >= 253);
        assert!(corner.color_at(199.5, 99.5)[0] >= 253);
        // A fixed circle of 20px.
        let fixed = Shader::radial(&gradient(RadialSize::Circle(20.0)), 200.0, 100.0, 2.0);
        assert!(
            fixed.color_at(100.0 + 39.0, 50.0)[0] >= 245,
            "scaled by 2 reaches 40px"
        );
        assert!(fixed.color_at(100.0 + 20.0, 50.0)[0] <= 140);
    }

    #[test]
    fn a_conic_gradient_sweeps_clockwise_from_the_top() {
        let gradient = ConicGradient {
            from: 0.0,
            center: (
                LengthPercentage {
                    length: 0.0,
                    percentage: 0.5,
                },
                LengthPercentage {
                    length: 0.0,
                    percentage: 0.5,
                },
            ),
            items: vec![
                ConicItem::Stop {
                    color: color(0, 0, 0),
                    position: Some(0.0),
                },
                ConicItem::Stop {
                    color: color(255, 255, 255),
                    position: Some(1.0),
                },
            ],
            repeating: false,
        };
        let shader = Shader::conic(&gradient, 100.0, 100.0, 1.0);
        // Just after the top: black; a quarter turn is to the right.
        assert!(shader.color_at(50.5, 0.5)[0] <= 6);
        assert!((i32::from(shader.color_at(99.5, 50.0)[0]) - 64).abs() <= 3);
        assert!((i32::from(shader.color_at(50.0, 99.5)[0]) - 128).abs() <= 3);
        assert!((i32::from(shader.color_at(0.5, 50.0)[0]) - 191).abs() <= 3);
        // Starting at 90deg moves the start to the right.
        let from_right = Shader::conic(
            &ConicGradient {
                from: 90.0,
                ..gradient
            },
            100.0,
            100.0,
            1.0,
        );
        assert!(from_right.color_at(99.5, 50.6)[0] <= 6);
    }

    #[test]
    fn fill_samples_pixel_centers_relative_to_the_tile_origin() {
        let shader = Shader::Linear {
            table: Table::new(
                &resolve(
                    &[
                        stop(color(0, 0, 0), Some(0.0)),
                        stop(color(255, 255, 255), Some(10.0)),
                    ],
                    10.0,
                ),
                false,
            )
            .expect("a table"),
            center: (5.0, 0.5),
            direction: (1.0, 0.0),
            length: 10.0,
        };
        let mut target = vec![0u8; 12 * 4];
        // A tile starting at x = 1.5 shifts the ramp by that much.
        fill(
            &mut target,
            12,
            (0, 0, 12, 1),
            (1.5, 0.0, 12.0, 1.0),
            &shader,
        );
        let at = |x: usize| target[x * 4];
        // Pixel 0 is before the tile and untouched; pixel 1 is half inside it, so
        // it is half opaque; from pixel 2 on the pixels are whole.
        assert_eq!(target[3], 0, "before the tile nothing is drawn");
        let edge = target[4 + 3];
        assert!(
            (126..=130).contains(&edge),
            "a half-covered pixel is half opaque: {edge}"
        );
        assert_eq!(target[8 + 3], 255, "opaque stops give opaque pixels");
        assert!(at(3) > at(2) && at(11) == 255);
    }
}
