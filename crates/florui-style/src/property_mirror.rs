//! Properties the style engine's servo build does not know, which would make
//! a declaration invalid and be dropped, are carried instead as internal
//! custom properties of the same value, `--florui-<name>`, written in their
//! place before the engine parses the text. Authors keep writing the real
//! property; [`crate::stylo`] reads the custom property back.
//!
//! Two shorthands the engine only half knows are expanded here as well:
//! `font-variant` into `font-variant-caps` (which it has) and the mirrored
//! ligatures, numeric, east-asian and position properties, and
//! `text-decoration` into line, style and color (which it has) and the
//! mirrored thickness.
//!
//! A custom property inherits, and a few of the properties it stands for do
//! not (`text-decoration-thickness`): a `text-decoration` shorthand therefore
//! always sets the thickness, to `auto` when it names none, as a browser's own
//! shorthand resets it.
//!
//! Uses `cssparser`, so comments, strings and selectors that merely contain a
//! name are never touched.

use std::borrow::Cow;

use cssparser::{Parser, ParserInput, Token};

/// The properties carried as `--florui-<name>` as written.
pub(crate) const MIRRORED: &[&str] = &[
    "font-feature-settings",
    "font-variant-caps",
    "font-variation-settings",
    "font-kerning",
    "font-variant-ligatures",
    "font-variant-numeric",
    "font-variant-east-asian",
    "font-variant-position",
    "font-optical-sizing",
    "font-synthesis",
    "font-synthesis-style",
    "font-synthesis-small-caps",
    "font-synthesis-position",
    "text-decoration-thickness",
    "text-underline-offset",
    "text-underline-position",
    "text-decoration-skip-ink",
];

const GLOBAL_KEYWORDS: [&str; 5] = ["inherit", "initial", "unset", "revert", "revert-layer"];

/// The custom property standing for `name`.
pub(crate) fn mirror_name(name: &str) -> String {
    format!("--florui-{name}")
}

struct Replacement {
    span: (usize, usize),
    text: String,
}

/// Rewrites a stylesheet's declarations (those inside `{...}` blocks).
/// Returns `css` borrowed and unchanged when it names none of the properties.
pub(crate) fn rewrite_stylesheet(css: &str) -> Cow<'_, str> {
    rewrite(css, false)
}

/// The same for a bare declaration list, such as a `style` attribute.
pub(crate) fn rewrite_declarations(css: &str) -> Cow<'_, str> {
    rewrite(css, true)
}

fn mentions(css: &str) -> bool {
    let lower = css.to_ascii_lowercase();
    lower.contains("font-variant")
        || lower.contains("text-decoration")
        || MIRRORED.iter().any(|name| lower.contains(name))
}

fn rewrite(css: &str, is_declaration_list: bool) -> Cow<'_, str> {
    if !mentions(css) {
        return Cow::Borrowed(css);
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut replacements = Vec::new();
    scan(css, &mut parser, is_declaration_list, &mut replacements);
    if replacements.is_empty() {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len() + 64 * replacements.len());
    let mut cursor = 0;
    for replacement in replacements {
        out.push_str(&css[cursor..replacement.span.0]);
        out.push_str(&replacement.text);
        cursor = replacement.span.1;
    }
    out.push_str(&css[cursor..]);
    Cow::Owned(out)
}

fn scan<'i>(
    css: &str,
    parser: &mut Parser<'i, '_>,
    declarations_here: bool,
    replacements: &mut Vec<Replacement>,
) {
    let mut at_declaration_start = declarations_here;
    loop {
        let start = parser.position().byte_index();
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => break,
        };
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => continue,
            Token::Semicolon => at_declaration_start = declarations_here,
            Token::Ident(ref name) if at_declaration_start && is_handled(name) => {
                let lower = name.to_ascii_lowercase();
                if let Some(replacement) = read_declaration(css, parser, start, &lower) {
                    replacements.push(replacement);
                }
                at_declaration_start = false;
            }
            Token::CurlyBracketBlock => {
                let _: Result<(), cssparser::ParseError<'_, ()>> =
                    parser.parse_nested_block(|inner| {
                        scan(css, inner, true, replacements);
                        Ok(())
                    });
                at_declaration_start = declarations_here;
            }
            _ => at_declaration_start = false,
        }
    }
}

fn is_handled(name: &str) -> bool {
    name.eq_ignore_ascii_case("font-variant")
        || name.eq_ignore_ascii_case("text-decoration")
        || MIRRORED
            .iter()
            .any(|mirrored| name.eq_ignore_ascii_case(mirrored))
}

/// After the property name: `: <value> [!important]`, ending at `;`, the end
/// of the block or nothing. On failure restores the parser and returns `None`,
/// leaving the declaration for the engine to drop or keep.
fn read_declaration(
    css: &str,
    parser: &mut Parser<'_, '_>,
    name_start: usize,
    name: &str,
) -> Option<Replacement> {
    let state = parser.state();
    let attempt: Result<Replacement, cssparser::ParseError<'_, ()>> = parser.try_parse(|p| {
        p.expect_colon()?;
        p.skip_whitespace();
        let value_start = p.position().byte_index();
        let mut value_end = value_start;
        // Where the value ends, and where the declaration does (after `!important`).
        let mut span_end = value_start;
        let mut important = false;
        loop {
            let before = p.state();
            let token = match p.next_including_whitespace_and_comments() {
                Ok(token) => token.clone(),
                Err(_) => break,
            };
            match token {
                Token::Semicolon => {
                    p.reset(&before);
                    break;
                }
                Token::Delim('!') => {
                    p.expect_ident_matching("important")?;
                    important = true;
                    span_end = p.position().byte_index();
                    let after = p.state();
                    match p.next() {
                        Ok(Token::Semicolon) | Err(_) => p.reset(&after),
                        Ok(_) => return Err(p.new_custom_error(())),
                    }
                    break;
                }
                Token::WhiteSpace(_) | Token::Comment(_) => {}
                _ => {
                    value_end = p.position().byte_index();
                    span_end = value_end;
                }
            }
        }
        let value = css[value_start..value_end].trim();
        if value.is_empty() {
            return Err(p.new_custom_error(()));
        }
        let suffix = if important { " !important" } else { "" };
        let text = expand(name, value, suffix).ok_or_else(|| p.new_custom_error(()))?;
        Ok(Replacement {
            span: (name_start, span_end),
            text,
        })
    });
    match attempt {
        Ok(replacement) => Some(replacement),
        Err(_) => {
            parser.reset(&state);
            None
        }
    }
}

fn is_global(word: &str) -> bool {
    GLOBAL_KEYWORDS.iter().any(|k| word.eq_ignore_ascii_case(k))
}

/// What replaces a `name: value` declaration; `None` leaves it as written.
fn expand(name: &str, value: &str, important: &str) -> Option<String> {
    match name {
        "font-variant" => expand_font_variant(value, important),
        "text-decoration" => expand_text_decoration(value, important),
        _ => Some(format!("{}: {value}{important}", mirror_name(name))),
    }
}

const CAPS: [&str; 6] = [
    "small-caps",
    "all-small-caps",
    "petite-caps",
    "all-petite-caps",
    "unicase",
    "titling-caps",
];
const LIGATURES: [&str; 9] = [
    "common-ligatures",
    "no-common-ligatures",
    "discretionary-ligatures",
    "no-discretionary-ligatures",
    "historical-ligatures",
    "no-historical-ligatures",
    "contextual",
    "no-contextual",
    "none",
];
const NUMERIC: [&str; 8] = [
    "lining-nums",
    "oldstyle-nums",
    "proportional-nums",
    "tabular-nums",
    "diagonal-fractions",
    "stacked-fractions",
    "ordinal",
    "slashed-zero",
];
const EAST_ASIAN: [&str; 9] = [
    "jis78",
    "jis83",
    "jis90",
    "jis04",
    "simplified",
    "traditional",
    "full-width",
    "proportional-width",
    "ruby",
];
const POSITION: [&str; 2] = ["sub", "super"];

/// Splits a value into words at top-level whitespace (a function stays whole).
fn words(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    for (index, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => {
                if let Some(begin) = start.take() {
                    out.push(&value[begin..index]);
                }
                continue;
            }
            _ => {}
        }
        start.get_or_insert(index);
    }
    if let Some(begin) = start {
        out.push(&value[begin..]);
    }
    out
}

fn expand_font_variant(value: &str, important: &str) -> Option<String> {
    let words = words(value);
    let mirrored =
        |property: &str, setting: &str| format!("{}: {setting}{important}", mirror_name(property));
    if let [word] = words.as_slice()
        && is_global(word)
    {
        return Some(format!(
            "{}; {}; {}; {}; {}",
            mirrored("font-variant-caps", word),
            mirrored("font-variant-ligatures", word),
            mirrored("font-variant-numeric", word),
            mirrored("font-variant-east-asian", word),
            mirrored("font-variant-position", word),
        ));
    }
    if let [word] = words.as_slice()
        && word.eq_ignore_ascii_case("normal")
    {
        return Some(format!(
            "{}; {}; {}; {}; {}",
            mirrored("font-variant-caps", "normal"),
            mirrored("font-variant-ligatures", "normal"),
            mirrored("font-variant-numeric", "normal"),
            mirrored("font-variant-east-asian", "normal"),
            mirrored("font-variant-position", "normal"),
        ));
    }
    let (mut caps, mut ligatures, mut numeric, mut east_asian, mut position) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for word in &words {
        let lower = word.to_ascii_lowercase();
        let lower = lower.as_str();
        if CAPS.contains(&lower) {
            caps.push(lower.to_owned());
        } else if LIGATURES.contains(&lower) {
            ligatures.push(lower.to_owned());
        } else if NUMERIC.contains(&lower) {
            numeric.push(lower.to_owned());
        } else if EAST_ASIAN.contains(&lower) {
            east_asian.push(lower.to_owned());
        } else if POSITION.contains(&lower) {
            position.push(lower.to_owned());
        } else {
            // `normal` among others, or a keyword this does not know: invalid.
            return None;
        }
    }
    let or_normal = |list: Vec<String>| {
        if list.is_empty() {
            "normal".to_owned()
        } else {
            list.join(" ")
        }
    };
    if caps.len() > 1 {
        return None;
    }
    Some(format!(
        "{}; {}; {}; {}; {}",
        mirrored("font-variant-caps", &or_normal(caps)),
        mirrored("font-variant-ligatures", &or_normal(ligatures)),
        mirrored("font-variant-numeric", &or_normal(numeric)),
        mirrored("font-variant-east-asian", &or_normal(east_asian)),
        mirrored("font-variant-position", &or_normal(position)),
    ))
}

const DECORATION_LINES: [&str; 4] = ["underline", "overline", "line-through", "blink"];
const DECORATION_STYLES: [&str; 5] = ["solid", "double", "dotted", "dashed", "wavy"];

fn expand_text_decoration(value: &str, important: &str) -> Option<String> {
    let words = words(value);
    if let [word] = words.as_slice()
        && is_global(word)
    {
        return Some(format!(
            "text-decoration-line: {word}{important}; text-decoration-style: {word}{important}; \
             text-decoration-color: {word}{important}; {}: {word}{important}",
            mirror_name("text-decoration-thickness"),
        ));
    }
    let (mut lines, mut style, mut color, mut thickness): (
        Vec<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = (Vec::new(), None, None, None);
    for word in &words {
        let lower = word.to_ascii_lowercase();
        if lower == "none" || DECORATION_LINES.contains(&lower.as_str()) {
            lines.push(lower);
        } else if DECORATION_STYLES.contains(&lower.as_str()) {
            if style.replace(lower).is_some() {
                return None;
            }
        } else if lower == "auto" || lower == "from-font" || is_length_percentage(word) {
            if thickness.replace((*word).to_owned()).is_some() {
                return None;
            }
        } else if is_color(word) {
            if color.replace((*word).to_owned()).is_some() {
                return None;
            }
        } else {
            // A word that is no part of the value makes the whole shorthand
            // invalid, as in a browser.
            return None;
        }
    }
    if lines.is_empty() && style.is_none() && color.is_none() && thickness.is_none() {
        return None;
    }
    Some(format!(
        "text-decoration-line: {}{important}; text-decoration-style: {}{important}; \
         text-decoration-color: {}{important}; {}: {}{important}",
        if lines.is_empty() {
            "none".to_owned()
        } else {
            lines.join(" ")
        },
        style.as_deref().unwrap_or("solid"),
        color.as_deref().unwrap_or("currentcolor"),
        mirror_name("text-decoration-thickness"),
        thickness.as_deref().unwrap_or("auto"),
    ))
}

/// A color: a keyword, `#rrggbb` or a color function.
fn is_color(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix('#') {
        return digits.chars().all(|c| c.is_ascii_hexdigit())
            && matches!(digits.len(), 3 | 4 | 6 | 8);
    }
    if let Some((name, _)) = lower.split_once('(') {
        return [
            "rgb",
            "rgba",
            "hsl",
            "hsla",
            "hwb",
            "lab",
            "lch",
            "oklab",
            "oklch",
            "color",
            "color-mix",
            "light-dark",
        ]
        .contains(&name);
    }
    lower == "currentcolor"
        || lower == "transparent"
        || cssparser::color::parse_named_color(&lower).is_ok()
}

/// A number with a unit or `%`, or a math function: a thickness.
fn is_length_percentage(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if ["calc(", "min(", "max(", "clamp("]
        .iter()
        .any(|function| lower.starts_with(function))
    {
        return true;
    }
    let numeric = lower.trim_start_matches(['+', '-']);
    if numeric == "0" {
        return true;
    }
    numeric
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '.')
        && lower.chars().any(|c| c.is_ascii_alphabetic() || c == '%')
        && !lower.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(css: &str) -> String {
        rewrite_stylesheet(css).into_owned()
    }

    #[test]
    fn a_sheet_that_names_none_of_them_is_returned_unchanged_and_unallocated() {
        assert!(matches!(
            rewrite_stylesheet(".a { color: red; }"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_mirrored_property_moves_to_its_custom_property_as_written() {
        assert_eq!(
            sheet(".a { font-feature-settings: \"liga\" 0, \"ss01\"; color: red }"),
            ".a { --florui-font-feature-settings: \"liga\" 0, \"ss01\"; color: red }"
        );
        assert_eq!(
            sheet(".a { text-underline-offset: 3px !important }"),
            ".a { --florui-text-underline-offset: 3px !important }"
        );
        assert_eq!(
            sheet(".a{FONT-KERNING:none}"),
            ".a{--florui-font-kerning: none}"
        );
    }

    #[test]
    fn font_variant_is_split_into_the_one_property_the_engine_has_and_the_rest() {
        assert_eq!(
            sheet(".a { font-variant: small-caps tabular-nums }"),
            ".a { --florui-font-variant-caps: small-caps; --florui-font-variant-ligatures: normal; \
             --florui-font-variant-numeric: tabular-nums; --florui-font-variant-east-asian: normal; \
             --florui-font-variant-position: normal }"
        );
        assert!(
            sheet(".a { font-variant: none }").contains("--florui-font-variant-ligatures: none")
        );
        assert!(
            sheet(".a { font-variant: inherit }").contains("--florui-font-variant-caps: inherit")
        );
    }

    #[test]
    fn text_decoration_always_sets_the_thickness_too() {
        assert_eq!(
            sheet(".a { text-decoration: underline wavy #c0392b 2px }"),
            ".a { text-decoration-line: underline; text-decoration-style: wavy; \
             text-decoration-color: #c0392b; --florui-text-decoration-thickness: 2px }"
        );
        assert_eq!(
            sheet(".a { text-decoration: underline }"),
            ".a { text-decoration-line: underline; text-decoration-style: solid; \
             text-decoration-color: currentcolor; --florui-text-decoration-thickness: auto }"
        );
        assert!(
            sheet(".a { text-decoration: overline line-through rgb(1 2 3) }")
                .contains("text-decoration-line: overline line-through")
        );
    }

    #[test]
    fn an_invalid_value_is_left_for_the_engine() {
        for css in [
            ".a { font-variant: small-caps all-small-caps }",
            ".a { font-variant: bogus }",
            ".a { text-decoration: }",
            ".a { text-decoration: underline wavy dotted }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn a_word_that_is_no_part_of_the_value_leaves_the_whole_shorthand_to_the_engine() {
        for css in [
            ".a { text-decoration: underlin }",
            ".a { text-decoration: underline foo }",
            ".a { text-decoration: underline #12 }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
        assert!(sheet(".a { text-decoration: underline AliceBlue }").contains("AliceBlue"));
        assert!(sheet(".a { text-decoration: underline rgb(1 2 3) 0 }").contains("-thickness: 0"));
        assert!(sheet(".a { text-decoration: auto }").contains("-thickness: auto"));
    }

    #[test]
    fn text_in_comments_strings_and_selectors_is_never_touched() {
        for css in [
            "/* font-kerning: none; */ .a { color: red; }",
            ".a { content: \"text-decoration: underline;\"; }",
            ".text-decoration { color: red; }",
            ".a { transition-property: text-decoration-thickness; }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn a_style_attribute_is_rewritten_at_the_top_level() {
        assert_eq!(
            rewrite_declarations("color: red; font-kerning: none").into_owned(),
            "color: red; --florui-font-kerning: none"
        );
    }
}
