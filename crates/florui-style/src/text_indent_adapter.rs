//! Stylo's servo build parses `text-indent` as a length only: the `hanging`
//! and `each-line` keywords of the real grammar make the whole declaration
//! invalid and it is silently dropped. This rewrites each valid declaration
//! to the length alone plus an internal `--florui-text-indent-keywords`
//! custom property that carries the keywords, before Stylo parses the text,
//! so authors keep writing the real property.
//!
//! Every `text-indent` declaration sets the custom property, to `none` when
//! it names no keyword, because the real property resets the keywords along
//! with the length. Both inherit, so they stay together down the tree. A
//! css-wide keyword is copied to both. Anything that is not exactly one
//! length, percentage or math function, with optional keywords, is left as
//! written so Stylo drops it as a browser would.
//!
//! Uses `cssparser` so comments, strings and selectors that merely contain
//! the text are never touched.

use std::borrow::Cow;

use cssparser::{Parser, ParserInput, Token};

pub(crate) const INTERNAL_PROPERTY: &str = "--florui-text-indent-keywords";

const GLOBAL_KEYWORDS: [&str; 5] = ["inherit", "initial", "unset", "revert", "revert-layer"];

struct Replacement {
    /// From the property name to the end of the value, before `!important`.
    span: (usize, usize),
    /// What replaces the property name and value.
    text: String,
}

/// Rewrites a stylesheet's `text-indent` declarations (those inside `{...}`
/// blocks). Returns `css` unchanged, borrowed, when it never mentions the
/// property.
pub(crate) fn rewrite_stylesheet(css: &str) -> Cow<'_, str> {
    rewrite(css, false)
}

/// The same for a bare declaration list, such as a `style` attribute.
pub(crate) fn rewrite_declarations(css: &str) -> Cow<'_, str> {
    rewrite(css, true)
}

fn rewrite(css: &str, is_declaration_list: bool) -> Cow<'_, str> {
    if !css.to_ascii_lowercase().contains("text-indent") {
        return Cow::Borrowed(css);
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut replacements = Vec::new();
    scan(css, &mut parser, is_declaration_list, &mut replacements);
    if replacements.is_empty() {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len() + 48 * replacements.len());
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
            Token::Ident(ref name)
                if at_declaration_start && name.eq_ignore_ascii_case("text-indent") =>
            {
                if let Some(replacement) = read_declaration(css, parser, start) {
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

/// After the property name: `: <value> [!important]`, ending at `;`, the end
/// of the block or nothing. On failure restores the parser and returns
/// `None`, leaving the declaration for Stylo to drop.
fn read_declaration(
    css: &str,
    parser: &mut Parser<'_, '_>,
    name_start: usize,
) -> Option<Replacement> {
    let state = parser.state();
    let attempt: Result<Replacement, cssparser::ParseError<'_, ()>> = parser.try_parse(|p| {
        p.expect_colon()?;
        p.skip_whitespace();

        let mut length: Option<(usize, usize)> = None;
        let mut global: Option<String> = None;
        let mut hanging = false;
        let mut each_line = false;
        let mut value_end = p.position().byte_index();
        let mut important = false;

        loop {
            let before = p.state();
            let token_start = p.position().byte_index();
            let token = match p.next() {
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
                    value_end = p.position().byte_index();
                    // Nothing may follow `!important`.
                    let after = p.state();
                    match p.next() {
                        Ok(Token::Semicolon) | Err(_) => p.reset(&after),
                        Ok(_) => return Err(p.new_custom_error(())),
                    }
                    break;
                }
                Token::Ident(ref word) if word.eq_ignore_ascii_case("hanging") && !hanging => {
                    hanging = true;
                }
                Token::Ident(ref word) if word.eq_ignore_ascii_case("each-line") && !each_line => {
                    each_line = true;
                }
                Token::Ident(ref word)
                    if GLOBAL_KEYWORDS.iter().any(|k| word.eq_ignore_ascii_case(k)) =>
                {
                    if global.is_some() || length.is_some() {
                        return Err(p.new_custom_error(()));
                    }
                    global = Some(word.to_ascii_lowercase());
                }
                Token::Dimension { .. } | Token::Percentage { .. } | Token::Number { .. } => {
                    if length.is_some() || global.is_some() {
                        return Err(p.new_custom_error(()));
                    }
                    length = Some((token_start, p.position().byte_index()));
                }
                Token::Function(_) => {
                    if length.is_some() || global.is_some() {
                        return Err(p.new_custom_error(()));
                    }
                    p.parse_nested_block(|inner| {
                        while inner.next_including_whitespace().is_ok() {}
                        Ok::<(), cssparser::ParseError<'_, ()>>(())
                    })?;
                    length = Some((token_start, p.position().byte_index()));
                }
                Token::WhiteSpace(_) => continue,
                _ => return Err(p.new_custom_error(())),
            }
            value_end = p.position().byte_index();
        }

        let important_suffix = if important { " !important" } else { "" };
        let (value, keywords) = match (&global, length) {
            (Some(word), None) if !hanging && !each_line => (word.clone(), word.clone()),
            (None, Some((start, end))) => {
                let keywords = match (hanging, each_line) {
                    (false, false) => "none",
                    (true, false) => "hanging",
                    (false, true) => "each-line",
                    (true, true) => "hanging each-line",
                };
                (css[start..end].trim().to_string(), keywords.to_string())
            }
            _ => return Err(p.new_custom_error(())),
        };
        Ok(Replacement {
            span: (name_start, value_end),
            text: format!(
                "text-indent: {value}{important_suffix}; {INTERNAL_PROPERTY}: {keywords}{important_suffix}"
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(css: &str) -> String {
        rewrite_stylesheet(css).into_owned()
    }

    #[test]
    fn a_stylesheet_that_never_mentions_the_property_is_returned_unchanged_and_unallocated() {
        assert!(matches!(
            rewrite_stylesheet(".a { color: red; }"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_plain_length_also_resets_the_keywords() {
        assert_eq!(
            sheet(".a { text-indent: 24px; color: red; }"),
            ".a { text-indent: 24px; --florui-text-indent-keywords: none; color: red; }"
        );
        assert_eq!(
            sheet(".a{TEXT-INDENT:10%}"),
            ".a{text-indent: 10%; --florui-text-indent-keywords: none}"
        );
    }

    #[test]
    fn the_keywords_move_to_the_custom_property_in_any_order() {
        assert_eq!(
            sheet(".a { text-indent: 12px hanging; }"),
            ".a { text-indent: 12px; --florui-text-indent-keywords: hanging; }"
        );
        assert_eq!(
            sheet(".a { text-indent: each-line 1em hanging; }"),
            ".a { text-indent: 1em; --florui-text-indent-keywords: hanging each-line; }"
        );
        assert_eq!(
            sheet(".a { text-indent: calc(1px + 2%) each-line }"),
            ".a { text-indent: calc(1px + 2%); --florui-text-indent-keywords: each-line }"
        );
    }

    #[test]
    fn important_applies_to_both_declarations() {
        assert_eq!(
            sheet(".a { text-indent: 5px hanging !important; }"),
            ".a { text-indent: 5px !important; --florui-text-indent-keywords: hanging !important; }"
        );
    }

    #[test]
    fn a_css_wide_keyword_is_copied_to_both() {
        for keyword in ["inherit", "initial", "unset", "revert", "revert-layer"] {
            assert_eq!(
                sheet(&format!(".a {{ text-indent: {keyword}; }}")),
                format!(
                    ".a {{ text-indent: {keyword}; --florui-text-indent-keywords: {keyword}; }}"
                )
            );
        }
    }

    #[test]
    fn an_invalid_value_is_left_for_the_engine_to_drop() {
        for css in [
            ".a { text-indent: hanging; }",
            ".a { text-indent: 10px 20px; }",
            ".a { text-indent: 10px hanging hanging; }",
            ".a { text-indent: 10px inherit; }",
            ".a { text-indent: red; }",
            ".a { text-indent: ; }",
            ".a { text-indent; }",
            ".a { text-indent: 10px !important hanging; }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn text_in_comments_strings_and_selectors_is_never_touched() {
        for css in [
            "/* text-indent: 10px hanging; */ .a { color: red; }",
            ".a { content: \"text-indent: 10px;\"; }",
            ".text-indent { color: red; }",
            ".a:not(.text-indent) { color: red; }",
            ".a { transition-property: text-indent; }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn several_declarations_and_nested_rules_are_all_rewritten() {
        assert_eq!(
            sheet(
                "@media (min-width: 10px) { .a { text-indent: 4px hanging } } .b { text-indent: 2px }"
            ),
            "@media (min-width: 10px) { .a { text-indent: 4px; --florui-text-indent-keywords: hanging } } .b { text-indent: 2px; --florui-text-indent-keywords: none }"
        );
    }

    #[test]
    fn a_style_attribute_declaration_list_is_rewritten_at_the_top_level() {
        assert_eq!(
            rewrite_declarations("color: red; text-indent: 8px each-line").into_owned(),
            "color: red; text-indent: 8px; --florui-text-indent-keywords: each-line"
        );
        assert_eq!(
            sheet("text-indent: 8px"),
            "text-indent: 8px",
            "a stylesheet has no declarations outside a block"
        );
    }
}
