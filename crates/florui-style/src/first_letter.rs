//! Which characters of a text are its first letter.
//!
//! As Chromium reads it: the white space the text starts with stays out; then
//! any punctuation (the Unicode categories Ps, Pe, Pi, Pf and Po, so a dash or a
//! currency sign is not one), then one letter, which is whatever comes next that
//! is not white space (a symbol, a digit, an emoji) together with the marks
//! that go with it, then the punctuation right after it. A text with no
//! letter after the punctuation, or none at all, has no first letter.

use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_segmentation::UnicodeSegmentation;

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

fn is_punctuation(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation
    )
}

/// How many bytes of `text` are its first letter, white space it starts with
/// included; zero when it has none.
pub(crate) fn first_letter_len(text: &str) -> usize {
    let mut at = 0;
    let rest = |at: usize| &text[at..];
    while let Some(c) = rest(at).chars().next().filter(|&c| is_space(c)) {
        at += c.len_utf8();
    }
    while let Some(c) = rest(at).chars().next().filter(|&c| is_punctuation(c)) {
        at += c.len_utf8();
    }
    let Some(letter) = rest(at).graphemes(true).next() else {
        return 0;
    };
    if letter.chars().next().is_none_or(is_space) {
        return 0;
    }
    at += letter.len();
    while let Some(c) = rest(at).chars().next().filter(|&c| is_punctuation(c)) {
        at += c.len_utf8();
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letters(text: &str) -> &str {
        &text[..first_letter_len(text)]
    }

    #[test]
    fn a_letter_a_digit_or_a_symbol_is_a_first_letter() {
        assert_eq!(letters("Hello"), "H");
        assert_eq!(letters("1st place"), "1");
        assert_eq!(letters("$5 here"), "$");
        assert_eq!(letters("--Hi"), "-", "a dash is not punctuation here");
        assert_eq!(letters("😀 Hello"), "😀");
    }

    #[test]
    fn the_white_space_before_stays_out_in_the_count_and_punctuation_around_the_letter_goes_in() {
        assert_eq!(letters("  Hello"), "  H");
        assert_eq!(letters("\"Hello\" world"), "\"H");
        assert_eq!(letters("H\"ello"), "H\"");
        assert_eq!(letters("(1) item"), "(1)");
        assert_eq!(letters("«Hi» x"), "«H");
        assert_eq!(letters("H. Smith"), "H.");
        assert_eq!(letters("\"\"\"x"), "\"\"\"x");
        assert_eq!(letters("¿Qué?"), "¿Q");
        assert_eq!(letters("3.14"), "3.");
    }

    #[test]
    fn a_letter_takes_its_marks_with_it() {
        assert_eq!(letters("H\u{301}x"), "H\u{301}");
        assert_eq!(letters("ÉCOLE"), "É");
    }

    #[test]
    fn a_text_with_no_letter_has_no_first_letter() {
        for text in ["", "   ", "!!!", "* Hello", "\"", "(  )"] {
            assert_eq!(first_letter_len(text), 0, "{text:?}");
        }
    }
}
