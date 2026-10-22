//! `::placeholder`, which the servo build of the style engine parses only in a
//! user-agent sheet, so an author rule such as `input::placeholder { color: red }`
//! was dropped.
//!
//! A text field has no generated content (`::before` and `::after` of an
//! `input` or `textarea` draw nothing in any browser), so its `::before` is
//! free: a selector with `::placeholder` is rewritten to put the same rule on the
//! `::before` of a field, `input::placeholder` becoming
//! `input:where(input, textarea)::before`. The `:where()` has no specificity, so
//! the rewritten selector counts as the original did. The engine resolves the
//! pseudo-element as it does any, with the inheritance of the field, and the
//! cascade carries the result in [`crate::ComputedStyle::placeholder`].

use std::borrow::Cow;

/// What stands for the pseudo-element in the rewritten selector.
const REPLACEMENT: &str = ":where(input, textarea)::before";

/// The spellings of the pseudo-element, each of which means the same.
const NAMES: [&str; 3] = [
    "::placeholder",
    "::-webkit-input-placeholder",
    "::-moz-placeholder",
];

/// `css` with every `::placeholder` replaced, outside strings and comments.
pub(crate) fn rewrite_stylesheet(css: &str) -> Cow<'_, str> {
    let lower = css.to_ascii_lowercase();
    if !NAMES.iter().any(|name| lower.contains(name)) {
        return Cow::Borrowed(css);
    }
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len() + 32);
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = css[at + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| at + 2 + end + 2);
            }
            quote @ (b'"' | b'\'') => {
                at += 1;
                while at < bytes.len() && bytes[at] != quote {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                at += 1;
            }
            b':' => {
                let rest = &lower.as_bytes()[at..];
                let found = NAMES.iter().find(|name| {
                    rest.starts_with(name.as_bytes())
                        && !rest
                            .get(name.len())
                            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
                });
                match found {
                    Some(name) => {
                        out.push_str(&css[copied..at]);
                        out.push_str(REPLACEMENT);
                        at += name.len();
                        copied = at;
                    }
                    None => at += 1,
                }
            }
            _ => at += 1,
        }
    }
    out.push_str(&css[copied.min(css.len())..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_becomes_the_before_of_a_field() {
        for name in NAMES {
            let css = format!("input{name} {{ color: red }}");
            assert_eq!(
                rewrite_stylesheet(&css),
                "input:where(input, textarea)::before { color: red }"
            );
        }
        assert_eq!(
            rewrite_stylesheet("::placeholder, .a::PLACEHOLDER { opacity: 0.5 }"),
            ":where(input, textarea)::before, .a:where(input, textarea)::before { opacity: 0.5 }"
        );
    }

    #[test]
    fn other_text_is_left_alone() {
        for css in [
            "input:placeholder-shown { color: red }",
            "/* input::placeholder */ .a { color: red }",
            ".a::before { content: \"::placeholder\" }",
            "input::placeholders { color: red }",
        ] {
            assert_eq!(rewrite_stylesheet(css), css);
        }
    }

    mod cascade {
        use florui::prelude::*;

        use crate::cascade::{Viewport, compute};
        use crate::color::Rgba;
        use crate::interaction::InteractionState;
        use crate::stylesheet_parse::parse_stylesheet;
        use crate::tree::Arena;

        fn field_styles(css: &str) -> (crate::ComputedStyle, crate::ComputedStyle) {
            let tree: Element = view! {
                <div>
                    <input class="field" placeholder="Hint" />
                    <textarea class="area" />
                </div>
            };
            let arena = Arena::build(&tree);
            let rules = parse_stylesheet(css).unwrap();
            let styles = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut crate::AnimationTimeline::default(),
            );
            let div = arena.roots()[0];
            let kids = arena.children(div);
            (styles[&kids[0]].clone(), styles[&kids[1]].clone())
        }

        #[test]
        fn the_default_placeholder_is_chromiums_gray() {
            let (input, _) = field_styles("");
            let placeholder = input.placeholder.expect("a field has a placeholder style");
            assert_eq!(placeholder.color, Rgba::opaque(0x75, 0x75, 0x75));
        }

        #[test]
        fn an_author_rule_styles_the_placeholder_of_the_fields_it_names() {
            let (input, area) = field_styles(
                ".field::placeholder { color: #ff0000; font-size: 20px; font-style: italic; \
                 opacity: 0.5; letter-spacing: 2px }",
            );
            let placeholder = input.placeholder.expect("styled");
            assert_eq!(placeholder.color, Rgba::opaque(255, 0, 0));
            assert_eq!(placeholder.font_size, 20.0);
            assert_eq!(placeholder.font_style, crate::cascade::FontStyle::Italic);
            assert_eq!(placeholder.opacity, 0.5);
            assert_eq!(placeholder.letter_spacing, 2.0);
            let other = area.placeholder.expect("the area has its own");
            assert_eq!(other.color, Rgba::opaque(0x75, 0x75, 0x75), "not named");
            assert_eq!(input.color, Rgba::opaque(0, 0, 0), "the field's own text");
        }

        #[test]
        fn a_global_rule_reaches_every_field_with_the_specificity_it_was_written_with() {
            let (input, area) = field_styles(
                "::placeholder { color: #00ff00 } .field::placeholder { color: #0000ff }",
            );
            assert_eq!(input.placeholder.unwrap().color, Rgba::opaque(0, 0, 255));
            assert_eq!(area.placeholder.unwrap().color, Rgba::opaque(0, 255, 0));
        }

        #[test]
        fn caret_color_is_read_through_the_mirror_and_auto_is_none() {
            let (input, area) =
                field_styles(".field { caret-color: #00ff00 } .area { caret-color: auto }");
            assert_eq!(input.caret_color, Some(Rgba::opaque(0, 255, 0)));
            assert_eq!(area.caret_color, None);
            let (plain, _) = field_styles("");
            assert_eq!(plain.caret_color, None, "the default is the text color");
        }

        #[test]
        fn a_selection_rule_styles_the_selection_of_a_field() {
            let (input, area) =
                field_styles(".field::selection { color: #ffffff; background-color: #ff0000 }");
            let selection = input.selection.expect("styled");
            assert_eq!(selection.color, Rgba::opaque(255, 255, 255));
            assert_eq!(selection.background_color, Rgba::opaque(255, 0, 0));
            assert!(area.selection.is_none(), "no rule names it");
        }
    }
}
