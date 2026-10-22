//! The room the upright characters of a vertical line take.
//!
//! A line is shaped horizontally, so each upright character is given, through the space
//! after it, the advance it has when it stands: the em for the wide ones, the height of the
//! font for the rest. The painter turns the glyph back to stand in it.

use std::ops::Range;

use crate::orientation::is_wide;
use crate::{Font, TextStyle, WhiteSpaceCollapse};

/// What marks the runs of upright characters of a layout: the last byte of the brush.
pub(crate) const UPRIGHT_BRUSH: [u8; 4] = [0, 0, 0, 1];

impl Font {
    /// The characters of `text` that stand upright in `style`, each with the room to add
    /// after it so that it fills its cell in the line.
    pub(crate) fn upright_cells(
        &mut self,
        style: TextStyle,
        text: &str,
    ) -> Vec<(Range<usize>, f32)> {
        let ranges = style.orientation.upright_chars(text);
        if ranges.is_empty() {
            return Vec::new();
        }
        let metrics = self.style_metrics(style);
        let height = metrics.ascent.round() + metrics.descent.round();
        ranges
            .into_iter()
            .map(|range| {
                let c = text[range.clone()].chars().next().unwrap_or(' ');
                let cell = if is_wide(c) { style.font_size } else { height };
                let natural = self.natural_advance(style, c);
                (range, cell - natural + style.letter_spacing)
            })
            .collect()
    }

    /// How far `c` advances, alone, in the font of `style`.
    fn natural_advance(&mut self, style: TextStyle, c: char) -> f32 {
        let key = (
            style.family.bits(),
            style.font_size.to_bits(),
            style.font_weight.to_bits(),
            c,
        );
        if let Some(&advance) = self.upright_advances.get(&key) {
            return advance;
        }
        let mut plain = TextStyle::new(style.family, style.font_size, style.font_weight)
            .with_font_style(style.font_style);
        plain.collapse = WhiteSpaceCollapse::Preserve;
        // As the upright characters are shaped: with the vertical forms of the font.
        plain.features = crate::intern_features("\"vert\" 1");
        let advance = self.measure(plain, c.encode_utf8(&mut [0; 4])).width;
        self.upright_advances.insert(key, advance);
        advance
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontFamily, Orientation};

    fn style(orientation: Orientation) -> TextStyle {
        TextStyle::new(FontFamily::SansSerif, 14.0, 400.0).with_orientation(orientation)
    }

    #[test]
    fn upright_letters_each_take_a_cell_as_tall_as_the_font() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(style(Orientation::Upright), "upr");
        let advances: Vec<f32> = shaped
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.advance))
            .collect();
        assert_eq!(advances.len(), 3);
        for advance in advances {
            assert!(
                (advance - 19.0).abs() < 0.01,
                "ascent and descent of the font, rounded: {advance}"
            );
        }
        assert!(shaped.runs.iter().all(|run| run.upright));
    }

    #[test]
    fn wide_characters_take_an_em_and_latin_stays_on_its_side_in_mixed_text() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(style(Orientation::Mixed), "ab日");
        let wide = shaped
            .runs
            .iter()
            .find(|run| run.upright)
            .expect("an upright run");
        assert_eq!(wide.glyphs.len(), 1);
        assert!((wide.glyphs[0].advance - 14.0).abs() < 0.01, "an em");
        let sideways = shaped
            .runs
            .iter()
            .find(|run| !run.upright)
            .expect("a sideways run");
        assert_eq!(sideways.glyphs.len(), 2);
        let natural = font
            .measure(TextStyle::new(FontFamily::SansSerif, 14.0, 400.0), "ab")
            .width;
        let advance: f32 = sideways.glyphs.iter().map(|glyph| glyph.advance).sum();
        assert!((advance - natural).abs() < 0.2, "{advance} {natural}");
    }

    #[test]
    fn the_lines_of_a_block_can_be_put_in_the_other_order_of_stacking() {
        let mut font = Font::load_embedded();
        let mut shaped = font.shape_wrapped(style(Orientation::Horizontal), "aaa bbb ccc", 30.0);
        assert!(shaped.lines.len() >= 3);
        let first = shaped.lines[0];
        let last = *shaped.lines.last().expect("lines");
        let extent = last.top + last.height;
        shaped.mirror_lines(extent);
        let mirrored = shaped.lines[0];
        assert!(
            (mirrored.top - (extent - first.height)).abs() < 0.01,
            "{mirrored:?}"
        );
    }
}
