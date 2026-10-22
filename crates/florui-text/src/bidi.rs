//! `direction` and `unicode-bidi` for text, which the shaper does not take from CSS:
//! it works out the direction of a paragraph from its first strong character. A
//! paragraph is given its direction by a mark (U+200E or U+200F) in front of its text,
//! which is strong, takes no room and draws nothing, and `unicode-bidi` is the
//! formatting characters of the Unicode algorithm (embeddings, isolates, overrides)
//! around the text of an element.

use unicode_bidi::BidiClass;

/// `direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextDirection {
    #[default]
    Ltr,
    Rtl,
}

/// `unicode-bidi`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BidiMode {
    #[default]
    Normal,
    Embed,
    Isolate,
    Override,
    IsolateOverride,
    Plaintext,
}

const LRM: &str = "\u{200E}";
const RLM: &str = "\u{200F}";
const LRE: &str = "\u{202A}";
const RLE: &str = "\u{202B}";
const PDF: &str = "\u{202C}";
const LRO: &str = "\u{202D}";
const RLO: &str = "\u{202E}";
const LRI: &str = "\u{2066}";
const RLI: &str = "\u{2067}";
const FSI: &str = "\u{2068}";
const PDI: &str = "\u{2069}";

/// Whether `c` is a character that makes the text around it run right to left when it
/// is the first strong one.
pub fn is_rtl_char(c: char) -> bool {
    matches!(
        unicode_bidi::bidi_class(c),
        BidiClass::R | BidiClass::AL | BidiClass::RLE | BidiClass::RLO | BidiClass::RLI
    )
}

/// The direction of the first strong character of `text`, if it has one.
pub fn first_strong(text: &str) -> Option<TextDirection> {
    text.chars()
        .find_map(|c| match unicode_bidi::bidi_class(c) {
            BidiClass::L => Some(TextDirection::Ltr),
            BidiClass::R | BidiClass::AL => Some(TextDirection::Rtl),
            _ => None,
        })
}

/// What goes before and after the text of a paragraph in `direction` under `mode`.
/// A paragraph that is left to right and has no right to left text needs nothing: the
/// shaper's own choice is the same.
pub fn paragraph_frame(
    direction: TextDirection,
    mode: BidiMode,
    has_rtl_text: bool,
) -> (&'static str, &'static str) {
    let rtl = direction == TextDirection::Rtl;
    let mark = if rtl {
        RLM
    } else if has_rtl_text {
        LRM
    } else {
        ""
    };
    match mode {
        // The first strong character says, and what is around is kept apart.
        BidiMode::Plaintext => ("", ""),
        BidiMode::Override | BidiMode::IsolateOverride => {
            // The mark is outside the override, so that it still sets the paragraph.
            match (mark, rtl) {
                ("", false) => (LRO, PDF),
                (_, false) => ("\u{200E}\u{202D}", PDF),
                (_, true) => ("\u{200F}\u{202E}", PDF),
            }
        }
        BidiMode::Normal | BidiMode::Embed | BidiMode::Isolate => (mark, ""),
    }
}

/// What goes before and after the text of an inline element in `direction` under
/// `mode`, which is what opens and closes its embedding, isolate or override.
pub fn element_frame(direction: TextDirection, mode: BidiMode) -> (&'static str, &'static str) {
    let rtl = direction == TextDirection::Rtl;
    match mode {
        BidiMode::Normal => ("", ""),
        BidiMode::Embed => (if rtl { RLE } else { LRE }, PDF),
        BidiMode::Isolate => (if rtl { RLI } else { LRI }, PDI),
        BidiMode::Override => (if rtl { RLO } else { LRO }, PDF),
        BidiMode::IsolateOverride => {
            // An isolate with an override inside it.
            if rtl {
                ("\u{2067}\u{202E}", "\u{202C}\u{2069}")
            } else {
                ("\u{2066}\u{202D}", "\u{202C}\u{2069}")
            }
        }
        BidiMode::Plaintext => (FSI, PDI),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_right_to_left_paragraph_is_marked_and_a_left_to_right_one_only_when_it_needs_to_be() {
        assert_eq!(
            paragraph_frame(TextDirection::Rtl, BidiMode::Normal, false),
            (RLM, "")
        );
        assert_eq!(
            paragraph_frame(TextDirection::Ltr, BidiMode::Normal, false),
            ("", "")
        );
        assert_eq!(
            paragraph_frame(TextDirection::Ltr, BidiMode::Normal, true),
            (LRM, "")
        );
    }

    #[test]
    fn an_override_keeps_the_mark_outside_and_closes_itself() {
        assert_eq!(
            paragraph_frame(TextDirection::Rtl, BidiMode::Override, false),
            ("\u{200F}\u{202E}", PDF)
        );
        assert_eq!(
            paragraph_frame(TextDirection::Ltr, BidiMode::Override, false),
            (LRO, PDF)
        );
        assert_eq!(
            paragraph_frame(TextDirection::Rtl, BidiMode::Plaintext, false),
            ("", "")
        );
    }

    #[test]
    fn an_element_opens_and_closes_what_unicode_bidi_says() {
        assert_eq!(
            element_frame(TextDirection::Rtl, BidiMode::Normal),
            ("", "")
        );
        assert_eq!(
            element_frame(TextDirection::Rtl, BidiMode::Embed),
            (RLE, PDF)
        );
        assert_eq!(
            element_frame(TextDirection::Ltr, BidiMode::Isolate),
            (LRI, PDI)
        );
        assert_eq!(
            element_frame(TextDirection::Ltr, BidiMode::Override),
            (LRO, PDF)
        );
        assert_eq!(
            element_frame(TextDirection::Ltr, BidiMode::Plaintext),
            (FSI, PDI)
        );
    }

    #[test]
    fn the_first_strong_character_gives_the_direction() {
        assert_eq!(first_strong("  123 abc"), Some(TextDirection::Ltr));
        assert_eq!(first_strong("123 \u{5d0}bc"), Some(TextDirection::Rtl));
        assert_eq!(first_strong("123 !"), None);
        assert!(is_rtl_char('\u{627}'));
        assert!(!is_rtl_char('a'));
    }
}
