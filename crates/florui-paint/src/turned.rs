//! The lines of a vertical writing mode.
//!
//! Layout and shaping lay the lines out horizontally, in a frame turned a quarter (see
//! `florui-layout`'s `vertical` module): a line runs along `u`, and the lines stack along
//! `v`. What a box with such lines draws is drawn in that frame on a surface of its own,
//! and the surface is turned onto the page: `u` runs down the page, and `v` runs from the
//! right edge of the box towards the left, so the tops of the letters face right. In
//! `sideways-lr` it is turned the other way: `u` runs up the page and `v` from the left edge
//! of the box towards the right, so the tops of the letters face left.
//!
//! The surface is aligned to the page's pixels, so turning it copies pixels and nothing is
//! resampled: the text is as sharp as if it had been drawn upright there.

use florui_style::WritingMode;
use tiny_skia::{Mask, Pixmap, PixmapPaint};

use super::{Surface, draw_shifted};

/// How far past the lines what is drawn may reach: glyphs that overhang their line, and
/// what a shadow or a decoration adds.
const OVERHANG: f32 = 6.0;

/// Draws with `draw` what a box of a vertical writing mode holds, in the frame of its lines,
/// and puts it on `buffer` turned.
///
/// `content` is the content box on the page, `(x, y, width, height)` in painted pixels, and
/// `ink` how far the lines run, how far they stack, and how far they stack the other way
/// from the corner (the lines that overflow a `vertical-lr` box), which is what the surface
/// has to cover. `draw` is given the surface and where the content box's corner is on it,
/// where the first line starts.
pub(super) fn paint_turned(
    buffer: &mut Surface,
    mode: WritingMode,
    content: (f32, f32, f32, f32),
    ink: (f32, f32, f32),
    scale_factor: f32,
    clip: Option<&Mask>,
    draw: impl FnOnce(&mut Surface, (f32, f32)),
) {
    let (x, y, width, height) = content;
    let reach = OVERHANG * scale_factor;
    if mode == WritingMode::SidewaysLr {
        // The corner where `u` and `v` are `0` is the bottom left one of the box.
        let (corner_x, corner_y) = (x, y + height);
        let left = (corner_x - reach).floor();
        let right = (corner_x + ink.1 + reach).ceil();
        let top = (corner_y - ink.0 - reach).floor();
        let bottom = (corner_y + reach).ceil();
        let (along, across) = ((bottom - top) as u32, (right - left) as u32);
        if along == 0 || across == 0 {
            return;
        }
        let Some(mut frame) = Surface::new(along, across, (0, 0)) else {
            return;
        };
        draw(&mut frame, (bottom - corner_y, corner_x - left));
        let Some(turned) = turn_counter_clockwise(&frame.pixmap) else {
            return;
        };
        draw_shifted(
            &mut buffer.pixmap,
            &turned,
            (left as i32 - buffer.origin.0, top as i32 - buffer.origin.1),
            &PixmapPaint::default(),
            clip,
        );
        return;
    }
    // The corner of the content box, where `u` and `v` are `0`, is its top right one.
    let (corner_x, corner_y) = (x + width, y);
    let left = (corner_x - ink.1 - reach).floor();
    let right = (corner_x + ink.2 + reach).ceil();
    let top = (corner_y - reach).floor();
    let bottom = (corner_y + ink.0 + reach).ceil();
    let (along, across) = ((bottom - top) as u32, (right - left) as u32);
    if along == 0 || across == 0 {
        return;
    }
    let Some(mut frame) = Surface::new(along, across, (0, 0)) else {
        return;
    };
    draw(&mut frame, (corner_y - top, right - corner_x));
    let Some(turned) = turn_clockwise(&frame.pixmap) else {
        return;
    };
    draw_shifted(
        &mut buffer.pixmap,
        &turned,
        (left as i32 - buffer.origin.0, top as i32 - buffer.origin.1),
        &PixmapPaint::default(),
        clip,
    );
}

/// `source` turned a quarter anticlockwise: its top row becomes the left column.
fn turn_counter_clockwise(source: &Pixmap) -> Option<Pixmap> {
    let (width, height) = (source.width() as usize, source.height() as usize);
    let mut turned = Pixmap::new(height as u32, width as u32)?;
    let from = source.data();
    let to = turned.data_mut();
    for row in 0..height {
        for column in 0..width {
            let at = (row * width + column) * 4;
            let into = ((width - 1 - column) * height + row) * 4;
            to[into..into + 4].copy_from_slice(&from[at..at + 4]);
        }
    }
    Some(turned)
}

/// `source` turned a quarter clockwise: its top row becomes the right column.
fn turn_clockwise(source: &Pixmap) -> Option<Pixmap> {
    let (width, height) = (source.width() as usize, source.height() as usize);
    let mut turned = Pixmap::new(height as u32, width as u32)?;
    let from = source.data();
    let to = turned.data_mut();
    for row in 0..height {
        for column in 0..width {
            let at = (row * width + column) * 4;
            let into = (column * height + (height - 1 - row)) * 4;
            to[into..into + 4].copy_from_slice(&from[at..at + 4]);
        }
    }
    Some(turned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quarter_turn_the_other_way_puts_the_top_row_on_the_left_column() {
        let mut source = Pixmap::new(3, 2).unwrap();
        // Top left pixel red, bottom right pixel green.
        source.data_mut()[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let last = (3 + 2) * 4;
        source.data_mut()[last..last + 4].copy_from_slice(&[0, 255, 0, 255]);
        let turned = turn_counter_clockwise(&source).unwrap();
        assert_eq!((turned.width(), turned.height()), (2, 3));
        let at = |x: usize, y: usize| {
            let start = (y * 2 + x) * 4;
            turned.data()[start..start + 4].to_vec()
        };
        // The top left of the source is the bottom left of the result; the bottom right of
        // the source is the top right.
        assert_eq!(at(0, 2), vec![255, 0, 0, 255]);
        assert_eq!(at(1, 0), vec![0, 255, 0, 255]);
    }

    #[test]
    fn a_quarter_turn_puts_the_top_row_on_the_right_column() {
        let mut source = Pixmap::new(3, 2).unwrap();
        // Top left pixel red, bottom right pixel green.
        source.data_mut()[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let last = (3 + 2) * 4;
        source.data_mut()[last..last + 4].copy_from_slice(&[0, 255, 0, 255]);
        let turned = turn_clockwise(&source).unwrap();
        assert_eq!((turned.width(), turned.height()), (2, 3));
        // The top left of the source is the top right of the result; the bottom right of the
        // source is the bottom left.
        let at = |x: usize, y: usize| {
            let start = (y * 2 + x) * 4;
            turned.data()[start..start + 4].to_vec()
        };
        assert_eq!(at(1, 0), vec![255, 0, 0, 255]);
        assert_eq!(at(0, 2), vec![0, 255, 0, 255]);
    }
}
