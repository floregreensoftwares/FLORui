//! `text-overflow`: cutting a line that is wider than its box and putting an
//! ellipsis (or the author's string) where the cut is.

use crate::{Font, ShapedRun, TextStyle};

/// The tolerance when comparing a width with the limit, in pixels.
const EPSILON: f32 = 0.01;

impl Font {
    /// Cuts every line of `runs` (glyphs positioned from the left of the box)
    /// that extends past `limit` pixels so that `mark`, shaped in `style`,
    /// ends at the limit, and puts the mark there. A line that fits is left
    /// as it is. Returns the lines that were cut, each with the x its mark ends at.
    ///
    /// The mark goes where the last glyph that still leaves room for it ends,
    /// on the baseline of the line, and belongs to element `element`.
    pub fn ellipsize_runs(
        &mut self,
        runs: &mut Vec<ShapedRun>,
        style: TextStyle,
        mark: &str,
        limit: f32,
        element: usize,
    ) -> Vec<(u32, f32)> {
        // Runs come in line order; a line is a stretch of runs with one line number.
        let line_of = |run: &ShapedRun| (!run.glyphs.is_empty()).then_some(run.line);
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        while start < runs.len() {
            let Some(key) = line_of(&runs[start]) else {
                start += 1;
                continue;
            };
            let mut end = start + 1;
            while end < runs.len() && line_of(&runs[end]).is_none_or(|other| other == key) {
                end += 1;
            }
            lines.push((start, end));
            start = end;
        }

        // The mark is a text of its own: no indent, no alignment.
        let mark_style = style
            .with_indent(crate::TextIndent::default())
            .with_align(crate::TextAlign::Start);
        let shaped_mark = self.shape(mark_style, mark);
        let mark_width = shaped_mark.width;
        let mut marks: Vec<(usize, ShapedRun)> = Vec::new();
        let mut cuts: Vec<(u32, f32)> = Vec::new();
        for &(first, last) in &lines {
            let extent = runs[first..last]
                .iter()
                .map(|run| run.x + run.width)
                .fold(f32::MIN, f32::max);
            if extent <= limit + EPSILON {
                continue;
            }
            let line_start = runs[first].x;
            let baseline = runs[first].glyphs[0].y;
            let room = limit - mark_width;
            // Where the kept text ends, and the run it ends in.
            let mut kept_end = line_start;
            let mut finished = false;
            for run in &mut runs[first..last] {
                if finished {
                    run.glyphs.clear();
                    continue;
                }
                let run_end = run.x + run.width;
                let mut keep = 0;
                for number in 0..run.glyphs.len() {
                    let end = run.glyphs.get(number + 1).map_or(run_end, |next| next.x);
                    if end > room + EPSILON {
                        finished = true;
                        break;
                    }
                    keep = number + 1;
                    kept_end = end;
                }
                run.glyphs.truncate(keep);
                run.width = (kept_end - run.x).max(0.0);
            }
            cuts.push((runs[first].line, kept_end + mark_width));
            // The mark, shifted to where the kept text ends.
            for mark_run in &shaped_mark.runs {
                let Some(first_glyph) = mark_run.glyphs.first() else {
                    continue;
                };
                let shift_y = baseline - first_glyph.y;
                let mut placed = mark_run.clone();
                for glyph in &mut placed.glyphs {
                    glyph.x += kept_end;
                    glyph.y += shift_y;
                }
                placed.x += kept_end;
                placed.element = element;
                marks.push((last, placed));
            }
        }
        if cuts.is_empty() {
            return cuts;
        }
        // Back to front so the indices stay right.
        let mut inserts = marks;
        inserts.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        for (at, run) in inserts {
            runs.insert(at, run);
        }
        // Runs emptied by the cut are gone.
        runs.retain(|run| !run.glyphs.is_empty());
        cuts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontFamily, TextStyle};

    fn style() -> TextStyle {
        TextStyle::new(FontFamily::SansSerif, 16.0, 400.0)
    }

    #[test]
    fn a_line_that_fits_is_left_alone() {
        let mut font = Font::load_embedded();
        let mut runs = font.shape(style(), "Short").runs;
        let before: Vec<usize> = runs.iter().map(|run| run.glyphs.len()).collect();
        assert!(
            font.ellipsize_runs(&mut runs, style(), "\u{2026}", 90.0, 0)
                .is_empty()
        );
        let after: Vec<usize> = runs.iter().map(|run| run.glyphs.len()).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn a_long_line_ends_in_the_mark_at_the_limit() {
        let mut font = Font::load_embedded();
        let mut runs = font.shape(style(), "The quick brown fox jumps").runs;
        assert!(
            !font
                .ellipsize_runs(&mut runs, style(), "\u{2026}", 90.0, 0)
                .is_empty()
        );
        // Text plus the mark fits the limit, and the mark is the last glyph.
        let end = runs
            .iter()
            .map(|run| run.x + run.width)
            .fold(f32::MIN, f32::max);
        assert!(end <= 90.0 + 0.5, "{end}");
        let mark = font.shape(style(), "\u{2026}");
        let last = runs.last().and_then(|run| run.glyphs.last()).unwrap();

        assert_eq!(last.id, mark.runs[0].glyphs[0].id);
        // The mark stands on the baseline of the line.
        let baseline = runs[0].glyphs[0].y;
        assert!((last.y - baseline).abs() < 0.01);
        // And it is further along than half the limit: most of the text stayed.
        assert!(last.x > 45.0, "{}", last.x);
    }

    #[test]
    fn every_overflowing_line_gets_a_mark() {
        use crate::{OverflowWrap, WhiteSpaceCollapse, WordBreak};
        let pre = style().with_wrapping(
            WhiteSpaceCollapse::Preserve,
            false,
            WordBreak::Normal,
            OverflowWrap::Normal,
        );
        let mut font = Font::load_embedded();
        let mut runs = font
            .shape(
                pre,
                "The quick brown fox jumps
over the lazy dog again",
            )
            .runs;
        assert_eq!(font.ellipsize_runs(&mut runs, pre, "~", 90.0, 0).len(), 2);
        let mark = font.shape(pre, "~").runs[0].glyphs[0].id;
        let marks = runs
            .iter()
            .flat_map(|run| run.glyphs.iter())
            .filter(|glyph| glyph.id == mark)
            .count();
        assert_eq!(marks, 2);
    }

    #[test]
    fn an_indented_line_is_cut_from_where_it_starts() {
        use crate::{OverflowWrap, TextIndent, WhiteSpaceCollapse, WordBreak};
        let indented = style()
            .with_indent(TextIndent {
                length: 70.0,
                ..TextIndent::default()
            })
            .with_wrapping(
                WhiteSpaceCollapse::Collapse,
                false,
                WordBreak::Normal,
                OverflowWrap::Normal,
            );
        let mut font = Font::load_embedded();
        let mut runs = font.shape(indented, "Short").runs;
        font.ellipsize_runs(&mut runs, indented, "…", 90.0, 0);
        // The first letter does not leave room for the mark, so the mark
        // stands where the text starts, after the indent and not twice of it.
        let mark = runs.last().and_then(|run| run.glyphs.last()).unwrap();
        assert!((mark.x - 70.0).abs() < 0.01, "{}", mark.x);
    }
}
