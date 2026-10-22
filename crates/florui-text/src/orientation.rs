//! How the characters of a line of a vertical writing mode stand: `text-orientation`.
//!
//! Lines are shaped horizontally and turned a quarter by the painter. A character that
//! stands upright is turned back on its own, and takes the room of an upright box in the
//! line: an em for the wide characters of the East Asian scripts, and the height of the font
//! for the others.

use std::ops::Range;

/// `text-orientation`, for the text of a vertical writing mode; `Horizontal` is the text of
/// any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    #[default]
    Horizontal,
    /// Characters of the scripts written vertically stand upright, the rest lie on their side.
    Mixed,
    /// Every character stands upright.
    Upright,
    /// Every character lies on its side.
    Sideways,
}

impl Orientation {
    /// Whether any character of the text may stand upright.
    pub(crate) fn may_stand(self) -> bool {
        matches!(self, Orientation::Mixed | Orientation::Upright)
    }

    /// Whether `c` stands upright in text of this orientation.
    pub(crate) fn stands(self, c: char) -> bool {
        match self {
            Orientation::Upright => !c.is_control(),
            Orientation::Mixed => is_upright(c),
            Orientation::Horizontal | Orientation::Sideways => false,
        }
    }

    /// The byte ranges of `text`, one for each character that stands upright.
    pub(crate) fn upright_chars(self, text: &str) -> Vec<Range<usize>> {
        if !self.may_stand() {
            return Vec::new();
        }
        text.char_indices()
            .filter(|&(_, c)| self.stands(c))
            .map(|(at, c)| at..at + c.len_utf8())
            .collect()
    }
}

/// Whether the character has an upright form in the lines of a vertical writing mode of a
/// browser's default (`text-orientation: mixed`): the scripts written vertically, the wide
/// and full-width forms, and the pictographs.
pub(crate) fn is_upright(c: char) -> bool {
    is_wide(c)
        || matches!(
            c,
            '\u{a7}'
                | '\u{a9}'
                | '\u{ae}'
                | '\u{b1}'
                | '\u{bc}'..='\u{be}'
                | '\u{d7}'
                | '\u{f7}'
                | '\u{2016}'
                | '\u{2020}'
                | '\u{2021}'
                | '\u{2030}'
                | '\u{2031}'
                | '\u{203b}'
                | '\u{203c}'
                | '\u{2042}'
                | '\u{2047}'..='\u{2049}'
                | '\u{2100}'..='\u{214f}'
                | '\u{2190}'..='\u{23ff}'
                | '\u{2460}'..='\u{27bf}'
                | '\u{2b00}'..='\u{2bff}'
                | '\u{1f000}'..='\u{1faff}'
        )
}

/// Whether the character is wide: the East Asian ones that fill a whole em.
pub(crate) fn is_wide(c: char) -> bool {
    matches!(
        c,
        '\u{1100}'..='\u{115f}'
            | '\u{2e80}'..='\u{a4cf}'
            | '\u{ac00}'..='\u{d7a3}'
            | '\u{f900}'..='\u{faff}'
            | '\u{fe30}'..='\u{fe6f}'
            | '\u{ff00}'..='\u{ff60}'
            | '\u{ffe0}'..='\u{ffe6}'
            | '\u{1f300}'..='\u{1f64f}'
            | '\u{1f900}'..='\u{1f9ff}'
            | '\u{20000}'..='\u{3fffd}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripts_written_vertically_stand_upright_in_mixed_text_and_latin_lies_down() {
        assert!(is_upright('日'));
        assert!(is_upright('あ'));
        assert!(is_upright('한'));
        assert!(is_upright('、'));
        assert!(!is_upright('a'));
        assert!(!is_upright('Ж'));
        assert!(!is_upright(' '));
    }

    #[test]
    fn upright_text_stands_every_character_but_the_controls() {
        let ranges = Orientation::Upright.upright_chars("ab\n");
        assert_eq!(ranges, vec![0..1, 1..2]);
        assert_eq!(Orientation::Mixed.upright_chars("a日b"), vec![1..4]);
        assert!(Orientation::Sideways.upright_chars("日").is_empty());
    }
}
