//! The counter styles that count a list: how a number is written (CSS Counter
//! Styles Level 3, the predefined styles Chrome draws), with what follows it.

/// A number written in a counter style, and what is put after it in a marker.
pub(crate) struct Counted {
    pub(crate) text: String,
    pub(crate) suffix: &'static str,
}

const DECIMAL: [char; 10] = ['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];

fn digits(name: &str) -> Option<[char; 10]> {
    let set = match name {
        "arabic-indic" => "٠١٢٣٤٥٦٧٨٩",
        "persian" => "۰۱۲۳۴۵۶۷۸۹",
        "devanagari" => "०१२३४५६७८९",
        "bengali" => "০১২৩৪৫৬৭৮৯",
        "gujarati" => "૦૧૨૩૪૫૬૭૮૯",
        "gurmukhi" => "੦੧੨੩੪੫੬੭੮੯",
        "kannada" => "೦೧೨೩೪೫೬೭೮೯",
        "khmer" | "cambodian" => "០១២៣៤៥៦៧៨៩",
        "lao" => "໐໑໒໓໔໕໖໗໘໙",
        "malayalam" => "൦൧൨൩൪൫൬൭൮൯",
        "mongolian" => "᠐᠑᠒᠓᠔᠕᠖᠗᠘᠙",
        "myanmar" => "၀၁၂၃၄၅၆၇၈၉",
        "oriya" => "୦୧୨୩୪୫୬୭୮୯",
        "telugu" => "౦౧౨౩౪౫౬౭౮౯",
        "thai" => "๐๑๒๓๔๕๖๗๘๙",
        "tibetan" => "༠༡༢༣༤༥༦༧༨༩",
        "tamil" => "௦௧௨௩௪௫௬௭௮௯",
        "cjk-decimal" => "〇一二三四五六七八九",
        _ => return None,
    };
    let mut out = [' '; 10];
    for (slot, c) in out.iter_mut().zip(set.chars()) {
        *slot = c;
    }
    Some(out)
}

/// A number in positional notation with `digits`, at least `pad` places.
fn numeric(value: i64, digits: &[char; 10], pad: usize) -> String {
    let mut n = value.unsigned_abs();
    let mut out: Vec<char> = Vec::new();
    loop {
        out.push(digits[(n % 10) as usize]);
        n /= 10;
        if n == 0 {
            break;
        }
    }
    while out.len() < pad {
        out.push(digits[0]);
    }
    if value < 0 {
        out.push('-');
    }
    out.iter().rev().collect()
}

/// Bijective base-n numbering (a, b, ..., z, aa, ab, ...) for `value` of at least 1.
fn alphabetic(value: i64, symbols: &[&str]) -> Option<String> {
    if value < 1 {
        return None;
    }
    let base = symbols.len() as i64;
    let mut n = value;
    let mut out: Vec<&str> = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(symbols[(n % base) as usize]);
        n /= base;
    }
    Some(out.iter().rev().copied().collect())
}

/// The greedy sum of the weights, largest first, within `range`.
fn additive(value: i64, weights: &[(i64, &str)], range: (i64, i64)) -> Option<String> {
    if value < range.0 || value > range.1 {
        return None;
    }
    let mut n = value;
    let mut out = String::new();
    for &(weight, symbol) in weights {
        if weight == 0 {
            continue;
        }
        while n >= weight {
            out.push_str(symbol);
            n -= weight;
        }
    }
    (n == 0).then_some(out)
}

fn letters(set: &str) -> Vec<&str> {
    set.split(' ').collect()
}

const ROMAN: [(i64, &str); 13] = [
    (1000, "m"),
    (900, "cm"),
    (500, "d"),
    (400, "cd"),
    (100, "c"),
    (90, "xc"),
    (50, "l"),
    (40, "xl"),
    (10, "x"),
    (9, "ix"),
    (5, "v"),
    (4, "iv"),
    (1, "i"),
];

const ARMENIAN: [(i64, &str); 36] = [
    (9000, "Ք"),
    (8000, "Փ"),
    (7000, "Ւ"),
    (6000, "Ց"),
    (5000, "Ր"),
    (4000, "Տ"),
    (3000, "Վ"),
    (2000, "Ս"),
    (1000, "Ռ"),
    (900, "Ջ"),
    (800, "Պ"),
    (700, "Չ"),
    (600, "Ո"),
    (500, "Շ"),
    (400, "Ն"),
    (300, "Յ"),
    (200, "Մ"),
    (100, "Ճ"),
    (90, "Ղ"),
    (80, "Ձ"),
    (70, "Հ"),
    (60, "Կ"),
    (50, "Ծ"),
    (40, "Խ"),
    (30, "Լ"),
    (20, "Ի"),
    (10, "Ժ"),
    (9, "Թ"),
    (8, "Ը"),
    (7, "Է"),
    (6, "Զ"),
    (5, "Ե"),
    (4, "Դ"),
    (3, "Գ"),
    (2, "Բ"),
    (1, "Ա"),
];

const GEORGIAN: [(i64, &str); 37] = [
    (10000, "ჵ"),
    (9000, "ჰ"),
    (8000, "ჯ"),
    (7000, "ჴ"),
    (6000, "ხ"),
    (5000, "ჭ"),
    (4000, "წ"),
    (3000, "ძ"),
    (2000, "ც"),
    (1000, "ჩ"),
    (900, "შ"),
    (800, "ყ"),
    (700, "ღ"),
    (600, "ქ"),
    (500, "ფ"),
    (400, "ჳ"),
    (300, "ტ"),
    (200, "ს"),
    (100, "რ"),
    (90, "ჟ"),
    (80, "პ"),
    (70, "ო"),
    (60, "ჲ"),
    (50, "ნ"),
    (40, "მ"),
    (30, "ლ"),
    (20, "კ"),
    (10, "ი"),
    (9, "თ"),
    (8, "ჱ"),
    (7, "ზ"),
    (6, "ვ"),
    (5, "ე"),
    (4, "დ"),
    (3, "გ"),
    (2, "ბ"),
    (1, "ა"),
];

const HEBREW: [(i64, &str); 31] = [
    (10000, "י׳"),
    (9000, "ט׳"),
    (8000, "ח׳"),
    (7000, "ז׳"),
    (6000, "ו׳"),
    (5000, "ה׳"),
    (4000, "ד׳"),
    (3000, "ג׳"),
    (2000, "ב׳"),
    (1000, "א׳"),
    (400, "ת"),
    (300, "ש"),
    (200, "ר"),
    (100, "ק"),
    (90, "צ"),
    (80, "פ"),
    (70, "ע"),
    (60, "ס"),
    (50, "נ"),
    (40, "מ"),
    (30, "ל"),
    (20, "כ"),
    (19, "יט"),
    (18, "יח"),
    (17, "יז"),
    (16, "טז"),
    (15, "טו"),
    (10, "י"),
    (9, "ט"),
    (8, "ח"),
    (7, "ז"),
];

const HEBREW_LOW: [(i64, &str); 6] = [(6, "ו"), (5, "ה"), (4, "ד"), (3, "ג"), (2, "ב"), (1, "א")];

fn lower_armenian(value: i64) -> Option<String> {
    additive(value, &ARMENIAN, (1, 9999)).map(|text| text.to_lowercase())
}

fn hebrew(value: i64) -> Option<String> {
    let all: Vec<(i64, &str)> = HEBREW.iter().chain(HEBREW_LOW.iter()).copied().collect();
    additive(value, &all, (1, 10999))
}

/// `value` in the counter style `name`, with its suffix; a number the style cannot
/// write is written in `decimal`.
pub(crate) fn format(name: &str, value: i64) -> Counted {
    let plain = |text: String| Counted { text, suffix: ". " };
    let cjk = |text: String| Counted {
        text, suffix: "、"
    };
    let decimal = || plain(numeric(value, &DECIMAL, 0));
    match name {
        "decimal" => decimal(),
        "decimal-leading-zero" => plain(numeric(value, &DECIMAL, 2)),
        "lower-roman" => additive(value, &ROMAN, (1, 3999)).map_or_else(decimal, plain),
        "upper-roman" => additive(value, &ROMAN, (1, 3999))
            .map(|text| text.to_uppercase())
            .map_or_else(decimal, plain),
        "lower-alpha" | "lower-latin" => alphabetic(
            value,
            &letters("a b c d e f g h i j k l m n o p q r s t u v w x y z"),
        )
        .map_or_else(decimal, plain),
        "upper-alpha" | "upper-latin" => alphabetic(
            value,
            &letters("A B C D E F G H I J K L M N O P Q R S T U V W X Y Z"),
        )
        .map_or_else(decimal, plain),
        "lower-greek" => alphabetic(
            value,
            &letters("α β γ δ ε ζ η θ ι κ λ μ ν ξ ο π ρ σ τ υ φ χ ψ ω"),
        )
        .map_or_else(decimal, plain),
        "armenian" | "upper-armenian" => {
            additive(value, &ARMENIAN, (1, 9999)).map_or_else(decimal, plain)
        }
        "lower-armenian" => lower_armenian(value).map_or_else(decimal, plain),
        "georgian" => additive(value, &GEORGIAN, (1, 19999)).map_or_else(decimal, plain),
        "hebrew" => hebrew(value).map_or_else(decimal, plain),
        "cjk-decimal" => cjk(numeric(value, &digits("cjk-decimal").unwrap_or(DECIMAL), 0)),
        "cjk-earthly-branch" => fixed(value, &letters("子 丑 寅 卯 辰 巳 午 未 申 酉 戌 亥"))
            .map_or_else(|| cjk(numeric(value, &digits("cjk-decimal").unwrap_or(DECIMAL), 0)), cjk),
        "cjk-heavenly-stem" => fixed(value, &letters("甲 乙 丙 丁 戊 己 庚 辛 壬 癸"))
            .map_or_else(|| cjk(numeric(value, &digits("cjk-decimal").unwrap_or(DECIMAL), 0)), cjk),
        "hiragana" => alphabetic(
            value,
            &letters(
                "あ い う え お か き く け こ さ し す せ そ た ち つ て と な に ぬ ね の は ひ ふ へ ほ ま み む め も や ゆ よ ら り る れ ろ わ ゐ ゑ を ん",
            ),
        )
        .map_or_else(decimal, cjk),
        "hiragana-iroha" => alphabetic(
            value,
            &letters(
                "い ろ は に ほ へ と ち り ぬ る を わ か よ た れ そ つ ね な ら む う ゐ の お く や ま け ふ こ え て あ さ き ゆ め み し ゑ ひ も せ す",
            ),
        )
        .map_or_else(decimal, cjk),
        "katakana" => alphabetic(
            value,
            &letters(
                "ア イ ウ エ オ カ キ ク ケ コ サ シ ス セ ソ タ チ ツ テ ト ナ ニ ヌ ネ ノ ハ ヒ フ ヘ ホ マ ミ ム メ モ ヤ ユ ヨ ラ リ ル レ ロ ワ ヰ ヱ ヲ ン",
            ),
        )
        .map_or_else(decimal, cjk),
        "katakana-iroha" => alphabetic(
            value,
            &letters(
                "イ ロ ハ ニ ホ ヘ ト チ リ ヌ ル ヲ ワ カ ヨ タ レ ソ ツ ネ ナ ラ ム ウ ヰ ノ オ ク ヤ マ ケ フ コ エ テ ア サ キ ユ メ ミ シ ヱ ヒ モ セ ス",
            ),
        )
        .map_or_else(decimal, cjk),
        _ => match digits(name) {
            Some(set) => plain(numeric(value, &set, 0)),
            None => decimal(),
        },
    }
}

/// The symbol of `value` among `symbols`, counted from 1, when there is one.
fn fixed(value: i64, symbols: &[&str]) -> Option<String> {
    usize::try_from(value - 1)
        .ok()
        .and_then(|index| symbols.get(index))
        .map(|symbol| (*symbol).to_owned())
}

#[cfg(test)]
mod tests {
    use super::format;

    fn text(name: &str, value: i64) -> String {
        format(name, value).text
    }

    #[test]
    fn numbers_are_written_in_the_style_of_the_list() {
        assert_eq!(text("decimal", 42), "42");
        assert_eq!(text("decimal", -7), "-7");
        assert_eq!(text("decimal-leading-zero", 5), "05");
        assert_eq!(text("decimal-leading-zero", 123), "123");
        assert_eq!(text("lower-roman", 1994), "mcmxciv");
        assert_eq!(text("upper-roman", 14), "XIV");
        assert_eq!(text("lower-alpha", 1), "a");
        assert_eq!(text("lower-alpha", 27), "aa");
        assert_eq!(text("upper-latin", 28), "AB");
        assert_eq!(text("lower-greek", 3), "γ");
        assert_eq!(text("armenian", 12), "ԺԲ");
        assert_eq!(text("georgian", 12), "იბ");
        assert_eq!(text("hebrew", 15), "טו");
        assert_eq!(text("arabic-indic", 25), "٢٥");
        assert_eq!(text("thai", 10), "๑๐");
        assert_eq!(text("cjk-decimal", 205), "二〇五");
        assert_eq!(text("hiragana", 2), "い");
        assert_eq!(text("cjk-earthly-branch", 3), "寅");
    }

    #[test]
    fn a_number_a_style_cannot_write_is_written_in_decimal() {
        assert_eq!(text("lower-roman", 0), "0");
        assert_eq!(text("lower-roman", 4000), "4000");
        assert_eq!(text("lower-alpha", 0), "0");
        assert_eq!(text("lower-alpha", -3), "-3");
        assert_eq!(text("no-such-style", 5), "5");
        assert_eq!(text("cjk-earthly-branch", 13), "一三");
    }

    #[test]
    fn what_follows_the_number_depends_on_the_style() {
        assert_eq!(format("decimal", 1).suffix, ". ");
        assert_eq!(format("hiragana", 1).suffix, "、");
        assert_eq!(format("cjk-decimal", 1).suffix, "、");
    }
}
