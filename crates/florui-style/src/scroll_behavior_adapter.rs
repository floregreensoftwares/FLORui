//! Stylo's servo build has no `scroll-behavior` (it is declared for the
//! gecko engine only), so an author's `scroll-behavior: smooth` is parsed
//! and silently dropped. This rewrites each valid declaration's property
//! name to the internal `--florui-scroll-behavior` custom property before
//! Stylo parses the text, so authors keep writing the real property.
//!
//! Only a declaration whose whole value is one of the property's keywords
//! is rewritten. Anything else is left as written, so Stylo drops it exactly
//! as a browser drops an invalid declaration and the earlier cascade value
//! survives. `unset` becomes `initial`: the real property does not inherit,
//! but a custom property does, so `unset` would otherwise inherit. The
//! user-agent sheet resets the custom property on every element for the
//! same reason. See [`crate::default_stylesheet`].
//!
//! Uses `cssparser` so comments, strings and selectors that merely contain
//! the text are never touched.

use std::borrow::Cow;

use cssparser::{Parser, ParserInput, Token};

pub(crate) const INTERNAL_PROPERTY: &str = "--florui-scroll-behavior";

const KEYWORDS: [&str; 7] = [
    "auto",
    "smooth",
    "inherit",
    "initial",
    "unset",
    "revert",
    "revert-layer",
];

struct Replacement {
    name: (usize, usize),
    value: Option<(usize, usize)>,
}

/// Rewrites a stylesheet's `scroll-behavior` declarations (those inside
/// `{...}` blocks). Returns `css` unchanged, borrowed, when it never
/// mentions the property.
pub(crate) fn rewrite_stylesheet(css: &str) -> Cow<'_, str> {
    rewrite(css, false)
}

/// The same for a bare declaration list, such as a `style` attribute.
pub(crate) fn rewrite_declarations(css: &str) -> Cow<'_, str> {
    rewrite(css, true)
}

fn rewrite(css: &str, is_declaration_list: bool) -> Cow<'_, str> {
    if !css.to_ascii_lowercase().contains("scroll-behavior") {
        return Cow::Borrowed(css);
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut replacements = Vec::new();
    scan(&mut parser, is_declaration_list, &mut replacements);
    if replacements.is_empty() {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len() + 32);
    let mut cursor = 0;
    for replacement in replacements {
        out.push_str(&css[cursor..replacement.name.0]);
        out.push_str(INTERNAL_PROPERTY);
        cursor = replacement.name.1;
        if let Some((start, end)) = replacement.value {
            out.push_str(&css[cursor..start]);
            out.push_str("initial");
            cursor = end;
        }
    }
    out.push_str(&css[cursor..]);
    Cow::Owned(out)
}

/// Walks one block (or the top level). `declarations_here` says whether a
/// declaration may start at this level.
fn scan<'i>(
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
                if at_declaration_start && name.eq_ignore_ascii_case("scroll-behavior") =>
            {
                let name_end = parser.position().byte_index();
                if let Some(value) = read_value(parser) {
                    replacements.push(Replacement {
                        name: (start, name_end),
                        value,
                    });
                }
                at_declaration_start = false;
            }
            Token::CurlyBracketBlock => {
                let _: Result<(), cssparser::ParseError<'_, ()>> =
                    parser.parse_nested_block(|inner| {
                        scan(inner, true, replacements);
                        Ok(())
                    });
                at_declaration_start = declarations_here;
            }
            _ => at_declaration_start = false,
        }
    }
}

/// After the property name: `: <keyword> [!important]` then `;`, the end of
/// the block, or nothing. On success returns the span of an `unset` keyword
/// to replace (`Some(None)` for a keyword kept as is); on failure restores
/// the parser and returns `None`.
fn read_value(parser: &mut Parser<'_, '_>) -> Option<Option<(usize, usize)>> {
    let state = parser.state();
    let attempt: Result<Option<(usize, usize)>, cssparser::ParseError<'_, ()>> =
        parser.try_parse(|p| {
            p.expect_colon()?;
            p.skip_whitespace();
            let value_start = p.position().byte_index();
            let keyword = p.expect_ident()?.clone();
            let value_end = p.position().byte_index();
            if !KEYWORDS.iter().any(|k| keyword.eq_ignore_ascii_case(k)) {
                return Err(p.new_custom_error(()));
            }
            let _ = p.try_parse(|p| {
                p.expect_delim('!')?;
                p.expect_ident_matching("important")
            });
            // The declaration must end here; the terminator is left for
            // `scan` to see.
            let before_terminator = p.state();
            match p.next() {
                Ok(Token::Semicolon) | Err(_) => p.reset(&before_terminator),
                Ok(_) => return Err(p.new_custom_error(())),
            }
            Ok(keyword
                .eq_ignore_ascii_case("unset")
                .then_some((value_start, value_end)))
        });
    match attempt {
        Ok(span) => Some(span),
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
        let css = ".a { color: red; }";
        assert!(matches!(rewrite_stylesheet(css), Cow::Borrowed(_)));
    }

    #[test]
    fn a_valid_declaration_is_renamed_to_the_internal_property() {
        assert_eq!(
            sheet(".a { scroll-behavior: smooth; color: red; }"),
            ".a { --florui-scroll-behavior: smooth; color: red; }"
        );
        assert_eq!(
            sheet(".a{SCROLL-BEHAVIOR:Auto}"),
            ".a{--florui-scroll-behavior:Auto}"
        );
    }

    #[test]
    fn important_and_the_css_wide_keywords_are_kept() {
        assert_eq!(
            sheet(".a { scroll-behavior: smooth !important; }"),
            ".a { --florui-scroll-behavior: smooth !important; }"
        );
        for keyword in ["inherit", "initial", "revert", "revert-layer"] {
            assert_eq!(
                sheet(&format!(".a {{ scroll-behavior: {keyword}; }}")),
                format!(".a {{ --florui-scroll-behavior: {keyword}; }}")
            );
        }
    }

    #[test]
    fn unset_becomes_initial_because_the_custom_property_would_inherit() {
        assert_eq!(
            sheet(".a { scroll-behavior: unset; }"),
            ".a { --florui-scroll-behavior: initial; }"
        );
    }

    #[test]
    fn an_invalid_value_is_left_for_the_engine_to_drop() {
        for css in [
            ".a { scroll-behavior: instant; }",
            ".a { scroll-behavior: smooth fast; }",
            ".a { scroll-behavior: 10px; }",
            ".a { scroll-behavior: ; }",
            ".a { scroll-behavior; }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn text_in_comments_strings_and_selectors_is_never_touched() {
        for css in [
            "/* scroll-behavior: smooth; */ .a { color: red; }",
            ".a { content: \"scroll-behavior: smooth;\"; }",
            ".scroll-behavior { color: red; }",
            ".a:not(.scroll-behavior) { color: red; }",
            "scroll-behavior:hover { color: red; }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn a_declaration_inside_a_nested_at_rule_is_rewritten() {
        assert_eq!(
            sheet("@media (min-width: 10px) { .a { scroll-behavior: smooth; } }"),
            "@media (min-width: 10px) { .a { --florui-scroll-behavior: smooth; } }"
        );
    }

    #[test]
    fn only_the_declaration_position_counts_not_a_value_that_names_the_property() {
        let css = ".a { transition-property: scroll-behavior; }";
        assert_eq!(sheet(css), css);
    }

    #[test]
    fn several_declarations_in_one_sheet_are_all_rewritten() {
        assert_eq!(
            sheet(".a { scroll-behavior: smooth } .b { scroll-behavior: auto; color: red }"),
            ".a { --florui-scroll-behavior: smooth } .b { --florui-scroll-behavior: auto; color: red }"
        );
    }

    #[test]
    fn a_style_attribute_declaration_list_is_rewritten_at_the_top_level() {
        assert_eq!(
            rewrite_declarations("color: red; scroll-behavior: smooth").into_owned(),
            "color: red; --florui-scroll-behavior: smooth"
        );
        assert_eq!(
            sheet("scroll-behavior: smooth"),
            "scroll-behavior: smooth",
            "a stylesheet has no declarations outside a block"
        );
    }
}
