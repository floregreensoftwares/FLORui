//! `text-transform` and the synthesized small capitals of `font-variant-caps`:
//! what changes in the text before it is shaped.

use std::borrow::Cow;
use std::ops::Range;

/// The case part of `text-transform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextCase {
    #[default]
    None,
    Uppercase,
    Lowercase,
    /// The first letter of every word in upper case.
    Capitalize,
}

/// `text-transform`: a case change, and independently the two that map
/// characters to their full-width and full-size forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextTransform {
    pub case: TextCase,
    pub full_width: bool,
    pub full_size_kana: bool,
}

impl TextTransform {
    pub fn is_none(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn bits(&self) -> u32 {
        self.case as u32 | (u32::from(self.full_width) << 2) | (u32::from(self.full_size_kana) << 3)
    }

    /// `text` as transformed. `in_word` says whether what came before ended
    /// inside a word (a word goes on across the elements of a line) and is
    /// updated for what comes after.
    pub fn apply<'a>(&self, text: &'a str, in_word: &mut bool) -> Cow<'a, str> {
        if self.is_none() {
            *in_word = ends_in_word(text, *in_word);
            return Cow::Borrowed(text);
        }
        let mut out = match self.case {
            TextCase::None => text.to_owned(),
            TextCase::Uppercase => text.to_uppercase(),
            TextCase::Lowercase => text.to_lowercase(),
            TextCase::Capitalize => capitalize(text, in_word),
        };
        if self.case != TextCase::Capitalize {
            *in_word = ends_in_word(text, *in_word);
        }
        if self.full_width {
            out = out.chars().map(to_full_width).collect();
        }
        if self.full_size_kana {
            out = out.chars().map(to_full_size_kana).collect();
        }
        Cow::Owned(out)
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Punctuation that does not end a word when a letter follows it: the
/// apostrophes and the middle dot, as in Chromium (a full stop or a colon does:
/// `e.g.` is capitalized as `E.G.`).
fn is_mid_word(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}' | '\u{b7}' | '\u{2027}')
}

fn ends_in_word(text: &str, mut in_word: bool) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        in_word = is_word_char(c)
            || (in_word && is_mid_word(c) && chars.peek().is_some_and(|n| is_word_char(*n)));
    }
    in_word
}

fn capitalize(text: &str, in_word: &mut bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if is_word_char(c) {
            if *in_word {
                out.push(c);
            } else if c == '\u{df}' {
                out.push_str("Ss");
            } else {
                out.extend(c.to_uppercase());
            }
            *in_word = true;
        } else {
            *in_word = *in_word && is_mid_word(c) && chars.peek().is_some_and(|n| is_word_char(*n));
            out.push(c);
        }
    }
    out
}

fn to_full_width(c: char) -> char {
    match c {
        ' ' => '\u{3000}',
        '\u{21}'..='\u{7e}' => char::from_u32(c as u32 + 0xfee0).unwrap_or(c),
        '\u{a2}' => '\u{ffe0}',
        '\u{a3}' => '\u{ffe1}',
        '\u{a5}' => '\u{ffe5}',
        '\u{a6}' => '\u{ffe4}',
        '\u{ac}' => '\u{ffe2}',
        '\u{af}' => '\u{ffe3}',
        '\u{20a9}' => '\u{ffe6}',
        '\u{ff61}'..='\u{ff9f}' => HALF_WIDTH_KANA
            .get(c as usize - 0xff61)
            .copied()
            .unwrap_or(c),
        '\u{ffa0}'..='\u{ffdc}' => c,
        '\u{ffe8}' => '\u{2502}',
        '\u{ffe9}' => '\u{2190}',
        '\u{ffea}' => '\u{2191}',
        '\u{ffeb}' => '\u{2192}',
        '\u{ffec}' => '\u{2193}',
        '\u{ffed}' => '\u{25a0}',
        '\u{ffee}' => '\u{25cb}',
        _ => c,
    }
}

/// U+FF61 to U+FF9F, as their full-width forms.
const HALF_WIDTH_KANA: [char; 63] = [
    '。', '「', '」', '、', '・', 'ヲ', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ャ', 'ュ', 'ョ', 'ッ', 'ー',
    'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ', 'サ', 'シ', 'ス', 'セ', 'ソ', 'タ',
    'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ',
    'ム', 'メ', 'モ', 'ヤ', 'ユ', 'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ン', '゛', '゜',
];

fn to_full_size_kana(c: char) -> char {
    match c {
        'ぁ' => 'あ',
        'ぃ' => 'い',
        'ぅ' => 'う',
        'ぇ' => 'え',
        'ぉ' => 'お',
        'っ' => 'つ',
        'ゃ' => 'や',
        'ゅ' => 'ゆ',
        'ょ' => 'よ',
        'ゎ' => 'わ',
        'ゕ' => 'か',
        'ゖ' => 'け',
        'ァ' => 'ア',
        'ィ' => 'イ',
        'ゥ' => 'ウ',
        'ェ' => 'エ',
        'ォ' => 'オ',
        'ッ' => 'ツ',
        'ャ' => 'ヤ',
        'ュ' => 'ユ',
        'ョ' => 'ヨ',
        'ヮ' => 'ワ',
        'ヵ' => 'カ',
        'ヶ' => 'ケ',
        'ㇰ' => 'ク',
        'ㇱ' => 'シ',
        'ㇲ' => 'ス',
        'ㇳ' => 'ト',
        'ㇴ' => 'ヌ',
        'ㇵ' => 'ハ',
        'ㇶ' => 'ヒ',
        'ㇷ' => 'フ',
        'ㇸ' => 'ヘ',
        'ㇹ' => 'ホ',
        'ㇺ' => 'ム',
        'ㇻ' => 'ラ',
        'ㇼ' => 'リ',
        'ㇽ' => 'ル',
        'ㇾ' => 'レ',
        'ㇿ' => 'ロ',
        _ => c,
    }
}

/// `font-variant-caps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontCaps {
    #[default]
    Normal,
    /// Lower case letters as small capitals.
    SmallCaps,
    /// Every letter as a small capital.
    AllSmallCaps,
    /// As small capitals, a little smaller still (the font has no petite ones).
    PetiteCaps,
    AllPetiteCaps,
    /// No synthesized form: left to the font's own `unic` and `titl` features.
    Unicase,
    TitlingCaps,
}

/// Where the synthesized small capitals are, as a fraction of the font size.
const SMALL_CAPS_SCALE: f32 = 0.7;

impl FontCaps {
    /// Whether the text is set with scaled capitals made here.
    pub fn is_synthesized(self) -> bool {
        matches!(
            self,
            Self::SmallCaps | Self::AllSmallCaps | Self::PetiteCaps | Self::AllPetiteCaps
        )
    }

    pub(crate) fn scale(self) -> f32 {
        SMALL_CAPS_SCALE
    }

    fn all(self) -> bool {
        matches!(self, Self::AllSmallCaps | Self::AllPetiteCaps)
    }
}

/// `text` with its small capitals made: each lower case letter (every
/// character for the `all-` forms) replaced by its capital, and the byte
/// ranges of the result those capitals are in, which are set smaller.
pub(crate) fn small_caps(text: &str, caps: FontCaps) -> (String, Vec<Range<usize>>) {
    if caps.all() {
        // As in Chromium, every character is set small, digits and spaces too.
        let upper = text.to_uppercase();
        let small = if upper.is_empty() {
            vec![]
        } else {
            std::iter::once(0..upper.len()).collect()
        };
        return (upper, small);
    }
    let mut out = String::with_capacity(text.len());
    let mut small: Vec<Range<usize>> = Vec::new();
    for c in text.chars() {
        let mut upper = c.to_uppercase();
        if c.is_lowercase() && upper.clone().ne(std::iter::once(c)) {
            let start = out.len();
            out.extend(&mut upper);
            match small.last_mut() {
                Some(last) if last.end == start => last.end = out.len(),
                _ => small.push(start..out.len()),
            }
        } else {
            out.push(c);
        }
    }
    (out, small)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(transform: TextTransform, text: &str) -> String {
        transform.apply(text, &mut false).into_owned()
    }

    fn case(case: TextCase) -> TextTransform {
        TextTransform {
            case,
            ..Default::default()
        }
    }

    #[test]
    fn upper_and_lower_case_follow_unicode() {
        assert_eq!(run(case(TextCase::Uppercase), "straße 3d"), "STRASSE 3D");
        assert_eq!(run(case(TextCase::Lowercase), "ΟΔΥΣΣΕΥΣ"), "οδυσσευς");
    }

    #[test]
    fn capitalize_raises_the_first_letter_of_each_word() {
        let capitalize = case(TextCase::Capitalize);
        assert_eq!(
            run(capitalize, "hello world-wide don't 3d e.g. x_y a:b"),
            "Hello World-Wide Don't 3d E.G. X_y A:B"
        );
    }

    #[test]
    fn a_word_goes_on_across_runs() {
        let capitalize = case(TextCase::Capitalize);
        let mut in_word = false;
        assert_eq!(capitalize.apply("hel", &mut in_word), "Hel");
        assert_eq!(capitalize.apply("lo wor", &mut in_word), "lo Wor");
        assert_eq!(capitalize.apply("ld", &mut in_word), "ld");
    }

    #[test]
    fn full_width_and_full_size_kana_map_characters() {
        let width = TextTransform {
            full_width: true,
            ..Default::default()
        };
        assert_eq!(run(width, "Ab 1!"), "Ａｂ　１！");
        let kana = TextTransform {
            full_size_kana: true,
            ..Default::default()
        };
        assert_eq!(run(kana, "ゃっ ォ"), "やつ オ");
    }

    #[test]
    fn small_caps_replace_lower_case_and_report_where() {
        let (text, ranges) = small_caps("Ab cD", FontCaps::SmallCaps);
        assert_eq!(text, "AB CD");
        assert_eq!(ranges, vec![1..2, 3..4]);
        let (text, ranges) = small_caps("Ab", FontCaps::AllSmallCaps);
        assert_eq!(text, "AB");
        assert_eq!(ranges, vec![0..2]);
    }
}
