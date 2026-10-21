//! `::first-letter`, which the servo build of the style engine does not know, so a
//! rule that holds one was dropped.
//!
//! The first letter of a block is built as a node of its own, a `florui-first-letter`
//! inline element placed before the text it takes the letters from (see
//! [`crate::Arena::apply_generated`]). A rule for `X::first-letter` is rewritten
//! into a rule for that node: `.florui-fl-N` stands for "the first letter of a
//! block that `X` matches" and the engine, which cannot tell it from any class, does
//! the cascade, with the node inheriting from the element the text is in. Before
//! each cascade the blocks `X` matches are found with the engine's own selector
//! matcher (see `stylo::evaluate_first_letters`) and their nodes are given the class.
//!
//! The rule gets `display: inline` ahead of its own declarations, because the
//! default sheet hides the node: a node no rule matched is not a first letter
//! and keeps its text. The specificity is that of `X` plus the one of a
//! pseudo-element, as written.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

static REGISTRY: Mutex<Vec<Arc<str>>> = Mutex::new(Vec::new());

/// The number of the selector `subject`, the same for every rule written alike.
fn intern(subject: &str) -> usize {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = registry.iter().position(|held| &**held == subject) {
        return at;
    }
    registry.push(Arc::from(subject));
    registry.len() - 1
}

/// The selector that `florui-fl-N` stands for the first letter of a block matching.
pub(crate) fn subject(number: usize) -> Option<Arc<str>> {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(number)
        .cloned()
}

/// Whether the selector names a state of an element.
pub(crate) fn mentions_state(selector: &str) -> bool {
    let lower = selector.to_ascii_lowercase();
    [
        ":hover", ":focus", ":active", ":link", ":visited", ":target",
    ]
    .iter()
    .any(|state| lower.contains(state))
}

/// The number in the name of a class `florui-fl-N`.
pub(crate) fn number_of_class(class: &str) -> Option<usize> {
    class.strip_prefix("florui-fl-")?.parse().ok()
}

/// What a rewrite of a stylesheet found.
pub(crate) struct Rewritten<'a> {
    pub css: Cow<'a, str>,
    /// The numbers of the selectors the stylesheet holds a `::first-letter` of.
    pub numbers: Vec<usize>,
    /// Which elements those selectors can match, for the arena to give them a node.
    pub subjects: Vec<crate::generated::Subject>,
}

/// Where a `::first-letter` ends a selector: the text of the pseudo-element.
fn trailing_pseudo(selector: &str) -> Option<&str> {
    let lower = selector.to_ascii_lowercase();
    ["::first-letter", ":first-letter"]
        .into_iter()
        .find(|name| lower.ends_with(name))
        .map(|name| &selector[selector.len() - name.len()..])
}

/// Splits `text` at the commas that are not inside parentheses, brackets or quotes.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    let mut quote: Option<char> = None;
    for (at, c) in text.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(' | '[') => depth += 1,
            (None, ')' | ']') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                parts.push(&text[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// `:where(.florui-fl-N)` with the specificity of the selector it stands for and the
/// one of the pseudo-element.
fn replacement(number: usize, specificity: (u32, u32, u32)) -> String {
    let mut text = format!(":where(.florui-fl-{number})");
    for _ in 0..specificity.0 {
        text.push_str(":not(#florui-fl-id)");
    }
    for _ in 0..specificity.1 {
        text.push_str(":not(.florui-fl-class)");
    }
    for _ in 0..=specificity.2 {
        text.push_str(":not(florui-fl-type)");
    }
    text
}

/// One rule, `prelude { body }`, with the `::first-letter` selectors of its list
/// turned into the selectors of the node. `None` when a selector does not parse,
/// which is left for the engine to drop.
fn rewrite_rule(
    prelude: &str,
    body: &str,
    numbers: &mut Vec<usize>,
    subjects: &mut Vec<crate::generated::Subject>,
) -> Option<String> {
    let mut letters = Vec::new();
    let mut others = Vec::new();
    let mut found = Vec::new();
    for selector in split_top_level(prelude) {
        let Some(pseudo) = trailing_pseudo(selector.trim_end()) else {
            others.push(selector.trim());
            continue;
        };
        let trimmed = selector.trim_end();
        let mut subject = trimmed[..trimmed.len() - pseudo.len()].to_owned();
        if subject.trim().is_empty()
            || subject.ends_with(|c: char| c.is_whitespace() || ">+~".contains(c))
        {
            subject.push('*');
        }
        let subject = subject.trim().to_owned();
        let packed = crate::stylo::parse_selector_list(&subject)?
            .into_iter()
            .max()
            .unwrap_or(0);
        let specificity = (packed >> 20, (packed >> 10) & 0x3ff, packed & 0x3ff);
        found.push((subject, specificity));
    }
    for (subject, specificity) in found {
        let number = intern(&subject);
        if !numbers.contains(&number) {
            numbers.push(number);
        }
        let own = crate::generated::subject_of_selector(&subject);
        if !subjects.contains(&own) {
            subjects.push(own);
        }
        letters.push(replacement(number, specificity));
    }
    let mut text = format!(
        "{} {{ display: inline; {} }}",
        letters.join(", "),
        body.trim()
    );
    if !others.is_empty() {
        text.push_str(&format!("\n{} {{ {} }}", others.join(", "), body.trim()));
    }
    Some(text)
}

/// `text` without its comments.
fn strip_comments(text: &str) -> Cow<'_, str> {
    if !text.contains("/*") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .find("*/")
            .map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Where the brace opened at `open` is closed.
fn matching_brace(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut at = open;
    while at < bytes.len() {
        match bytes[at] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return at;
                }
            }
            q @ (b'"' | b'\'') => {
                at += 1;
                while at < bytes.len() && bytes[at] != q {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
            }
            _ => {}
        }
        at += 1;
    }
    bytes.len().saturating_sub(1)
}

/// `css` with every rule for a `::first-letter` rewritten into one for its node.
pub(crate) fn rewrite_stylesheet(css: &str) -> Rewritten<'_> {
    let lower = css.to_ascii_lowercase();
    let mut numbers = Vec::new();
    let mut subjects = Vec::new();
    if !lower.contains(":first-letter") {
        return Rewritten {
            css: Cow::Borrowed(css),
            numbers,
            subjects,
        };
    }
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len() + 64);
    let mut copied = 0;
    let mut at = 0;
    let mut prelude_start = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = css[at + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| at + 2 + end + 2);
            }
            b'{' => {
                let close = matching_brace(bytes, at);
                let prelude = strip_comments(&css[prelude_start..at]);
                let prelude = prelude.as_ref();
                if prelude.trim_start().starts_with('@') {
                    // The rules inside an at-rule are rules of the sheet too.
                    at += 1;
                    prelude_start = at;
                    continue;
                }
                if prelude.to_ascii_lowercase().contains(":first-letter")
                    && let Some(rewritten) =
                        rewrite_rule(prelude, &css[at + 1..close], &mut numbers, &mut subjects)
                {
                    out.push_str(&css[copied..prelude_start]);
                    out.push_str(&rewritten);
                    copied = close + 1;
                }
                at = close + 1;
                prelude_start = at;
            }
            b'}' | b';' => {
                at += 1;
                prelude_start = at;
            }
            _ => at += 1,
        }
    }
    out.push_str(&css[copied.min(css.len())..]);
    Rewritten {
        css: Cow::Owned(out),
        numbers,
        subjects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_for_a_first_letter_becomes_one_for_its_node() {
        let rewritten = rewrite_stylesheet(".p::first-letter { font-size: 32px }");
        assert_eq!(rewritten.numbers.len(), 1);
        let n = rewritten.numbers[0];
        assert_eq!(
            rewritten.css,
            format!(
                ":where(.florui-fl-{n}):not(.florui-fl-class):not(florui-fl-type) \
                 {{ display: inline; font-size: 32px }}"
            )
        );
        assert_eq!(&*subject(n).unwrap(), ".p");
        assert_eq!(rewritten.subjects.len(), 1);
    }

    #[test]
    fn a_list_with_other_selectors_keeps_them_in_a_rule_of_their_own() {
        let rewritten = rewrite_stylesheet("h1, .a > .b:first-letter, h2 { color: red }");
        let css = rewritten.css.to_string();
        assert!(css.contains("display: inline; color: red"), "{css}");
        assert!(css.contains("\nh1, h2 { color: red }"), "{css}");
        assert_eq!(&*subject(rewritten.numbers[0]).unwrap(), ".a > .b");
    }

    #[test]
    fn a_pseudo_element_with_no_compound_in_front_is_asked_of_any_element() {
        let rewritten =
            rewrite_stylesheet("p ::first-letter { color: red } ::first-letter { color: blue }");
        let subjects: Vec<String> = rewritten
            .numbers
            .iter()
            .map(|&n| subject(n).unwrap().to_string())
            .collect();
        assert_eq!(subjects, ["p *", "*"]);
    }

    #[test]
    fn rules_in_at_rules_are_rewritten_and_text_and_other_rules_are_left_alone() {
        let rewritten = rewrite_stylesheet(
            "/* a::first-letter */ .x { content: \"::first-letter\" } \
             @media (min-width: 1px) { .y::first-letter { color: red } }",
        );
        let css = rewritten.css.to_string();
        assert!(css.starts_with("/* a::first-letter */ .x { content: \"::first-letter\" } @media"));
        assert!(css.contains("display: inline; color: red"), "{css}");
        assert_eq!(rewritten.numbers.len(), 1);
        let plain = rewrite_stylesheet(".a { color: red }");
        assert!(matches!(plain.css, Cow::Borrowed(_)));
    }

    #[test]
    fn the_specificity_of_the_subject_is_kept() {
        let rewritten = rewrite_stylesheet("#a .b p::first-letter { color: red }");
        let css = rewritten.css.to_string();
        assert_eq!(css.matches(":not(#florui-fl-id)").count(), 1);
        assert_eq!(css.matches(":not(.florui-fl-class)").count(), 1);
        assert_eq!(
            css.matches(":not(florui-fl-type)").count(),
            2,
            "p and the pseudo-element"
        );
    }

    mod arena {
        use florui::prelude::*;

        use crate::cascade::{Display, FontStyle, Viewport, compute};
        use crate::color::Rgba;
        use crate::interaction::InteractionState;
        use crate::stylesheet_parse::parse_stylesheet;
        use crate::tree::Arena;
        use crate::{Pseudo, PseudoSlots};

        fn styled(tree: &Element, css: &str) -> (Arena, crate::StyleMap) {
            let rules = parse_stylesheet(css).unwrap();
            let mut arena = Arena::build_with(tree, &PseudoSlots::from_rules(&rules));
            let styles = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut crate::AnimationTimeline::default(),
            );
            arena.apply_generated(&styles);
            (arena, styles)
        }

        fn letters_of(arena: &Arena) -> Vec<String> {
            arena
                .find_all(|tree, id| tree.generated(id) == Some(Pseudo::FirstLetter))
                .into_iter()
                .map(|id| arena.text_content(id).to_owned())
                .collect()
        }

        const RULE: &str = ".p::first-letter { font-size: 32px; color: #ff0000 }";

        #[test]
        fn a_block_a_rule_names_gets_a_node_that_holds_its_first_letter() {
            let tree: Element =
                view! { <div><p class="p">{"Hello world"}</p><p>{"Other"}</p></div> };
            let (arena, styles) = styled(&tree, RULE);
            let [p, other] = arena.children(arena.roots()[0])[..] else {
                panic!("two paragraphs");
            };
            let [letter] = arena.children(p)[..] else {
                panic!("the node of the first letter");
            };
            assert_eq!(arena.text_content(letter), "H");
            assert_eq!(arena.first_letter_owner(letter), Some(p));
            assert_eq!(styles[&letter].display, Display::Inline);
            assert_eq!(styles[&letter].font_size, 32.0);
            assert_eq!(styles[&letter].color, Rgba::opaque(255, 0, 0));
            let held: Vec<_> = arena.inline_items(p).to_vec();
            assert_eq!(held.len(), 2);
            assert_eq!(
                held[1],
                crate::tree::InlineItem::Text("ello world".to_owned())
            );
            assert!(arena.children(other).is_empty(), "no rule names it");
        }

        #[test]
        fn the_node_is_in_the_element_the_text_is_in_and_inherits_from_it() {
            let tree: Element = view! {
                <p class="p"><em>{"Hello"}</em>{" world"}</p>
            };
            let (arena, styles) = styled(&tree, RULE);
            let p = arena.roots()[0];
            let em = arena.children(p)[0];
            let [letter] = arena.children(em)[..] else {
                panic!("the node is in the em");
            };
            assert_eq!(arena.text_content(letter), "H");
            assert_eq!(styles[&letter].font_style, FontStyle::Italic);
        }

        #[test]
        fn punctuation_around_the_letter_goes_in_and_white_space_before_it_stays_out() {
            let tree: Element = view! { <p class="p">{"  \"Hello\" world"}</p> };
            let (arena, _) = styled(&tree, RULE);
            assert_eq!(letters_of(&arena), ["\"H"]);
            let p = arena.roots()[0];
            assert_eq!(
                arena.inline_items(p)[0],
                crate::tree::InlineItem::Text("  ".to_owned())
            );
        }

        #[test]
        fn a_text_with_no_letter_leaves_the_node_empty_and_the_text_whole() {
            let tree: Element = view! { <p class="p">{"!!!"}</p> };
            let (arena, _) = styled(&tree, RULE);
            assert_eq!(letters_of(&arena), [""]);
            let p = arena.roots()[0];
            assert_eq!(
                arena.inline_items(p)[1],
                crate::tree::InlineItem::Text("!!!".to_owned())
            );
        }

        #[test]
        fn a_flex_container_and_a_block_that_starts_with_an_image_have_no_first_letter() {
            let tree: Element = view! {
                <div>
                    <p class="p" style="display: flex">{"Hello"}</p>
                    <p class="p"><img />{"Hello"}</p>
                </div>
            };
            let (arena, _) = styled(&tree, RULE);
            assert_eq!(
                letters_of(&arena),
                [""],
                "only the flex one has a node, and it is empty"
            );
        }

        #[test]
        fn the_text_of_the_before_box_comes_first_and_the_letter_inherits_from_the_box() {
            let tree: Element = view! { <p class="p">{"world"}</p> };
            let (arena, styles) = styled(
                &tree,
                ".p::before { content: \"Note: \"; color: #00ff00 } \
                 .p::first-letter { font-size: 32px }",
            );
            let letters: Vec<(String, Rgba)> = arena
                .find_all(|tree, id| tree.generated(id) == Some(Pseudo::FirstLetter))
                .into_iter()
                .map(|id| (arena.text_content(id).to_owned(), styles[&id].color))
                .collect();
            assert_eq!(
                letters,
                [
                    ("N".to_owned(), Rgba::opaque(0, 255, 0)),
                    (String::new(), Rgba::opaque(0, 0, 0))
                ],
                "the first letter is the box's, in its color, and the text keeps its own"
            );
            let p = arena.roots()[0];
            let before = arena
                .generated_child(p, Pseudo::Before)
                .expect("a ::before box");
            assert_eq!(
                arena.inline_items(before)[1],
                crate::tree::InlineItem::Text("ote: ".to_owned())
            );
            assert!(
                arena
                    .inline_items(p)
                    .contains(&crate::tree::InlineItem::Text("world".to_owned()))
            );
        }

        #[test]
        fn a_before_box_with_no_content_leaves_the_letter_to_the_text() {
            let tree: Element = view! { <p class="p">{"world"}</p> };
            let (arena, _) = styled(
                &tree,
                ".p::before { color: #00ff00 } .p::first-letter { font-size: 32px }",
            );
            assert_eq!(letters_of(&arena), ["w"], "no box to take letters from");
        }
        #[test]
        fn a_selector_with_a_state_and_the_same_text_again_are_taken_from_the_arena_kept() {
            let tree: Element = view! { <p class="p">{"Hello world"}</p> };
            let rules = parse_stylesheet(RULE).unwrap();
            let slots = PseudoSlots::from_rules(&rules);
            let mut arena = Arena::build_with(&tree, &slots);
            let compute_again = |arena: &mut Arena| {
                let styles = compute(
                    arena,
                    &rules,
                    &InteractionState::new(),
                    Viewport::default(),
                    &mut crate::AnimationTimeline::default(),
                );
                arena.apply_generated(&styles);
                arena.apply_generated(&styles);
            };
            compute_again(&mut arena);
            assert_eq!(letters_of(&arena), ["H"]);
            let other: Element = view! { <p class="p">{"Another text"}</p> };
            assert!(
                arena.rebuild_with(&other, &slots),
                "the same shape keeps the arena"
            );
            compute_again(&mut arena);
            assert_eq!(letters_of(&arena), ["A"]);
            let p = arena.roots()[0];
            assert_eq!(
                arena.inline_items(p)[1],
                crate::tree::InlineItem::Text("nother text".to_owned())
            );
        }
    }
}
