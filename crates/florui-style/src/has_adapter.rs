//! `:has()`, which the servo build of the style engine does not parse (its selector
//! parser answers no to `parse_has`), so a rule that holds one is dropped.
//!
//! Each `:has(...)` in a stylesheet is replaced, before parsing, by a class no
//! element has written, `florui-has-N`, where `N` stands for the list inside the
//! parentheses. Before a cascade the arguments are matched with the engine's own
//! selector matcher against the tree (see `stylo::evaluate_has`), and the class
//! is given to each element it holds for. A rule is then an ordinary class rule
//! the cascade, the sharing of styles between alike elements and the partial
//! restyle all handle as they handle any other.
//!
//! The specificity of a `:has()` is that of its most specific argument. The class
//! stands in `:where()`, which has none, and `:not()` of a name nothing has
//! written is added once for each id, class and type the argument counts.

use std::borrow::Cow;
use std::sync::{Arc, Mutex};

/// How an argument of `:has()` reaches the elements it looks at from the
/// element it is asked of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Relative {
    /// The argument written as a selector with `:scope` where the element is:
    /// `:scope > .a`, `:scope .a .b`, `:scope + .c`.
    pub selector: String,
    /// Whether the argument looks at the siblings after the element, and below
    /// them, instead of below the element.
    pub siblings: bool,
}

/// One `:has()` of the stylesheets: the arguments, any of which is enough.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HasSpec {
    pub relative: Vec<Relative>,
    /// Whether an argument names a state of an element (`:hover`, `:focus`), which
    /// changes what the element it is asked of has without a change in the tree.
    pub mentions_state: bool,
}

static REGISTRY: Mutex<Vec<Arc<HasSpec>>> = Mutex::new(Vec::new());

/// The number of `spec`, the same for every list written alike.
fn intern(spec: HasSpec) -> usize {
    let mut registry = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(at) = registry.iter().position(|held| **held == spec) {
        return at;
    }
    registry.push(Arc::new(spec));
    registry.len() - 1
}

/// The `:has()` that `florui-has-N` stands for.
pub(crate) fn spec(number: usize) -> Option<Arc<HasSpec>> {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(number)
        .cloned()
}

/// The number in the name of a class `florui-has-N`.
pub(crate) fn number_of_class(class: &str) -> Option<usize> {
    class.strip_prefix("florui-has-")?.parse().ok()
}

/// What an argument says of the state: any of these pseudo-classes.
fn mentions_state(argument: &str) -> bool {
    const STATES: [&str; 8] = [
        ":hover",
        ":focus",
        ":active",
        ":link",
        ":visited",
        ":target",
        ":focus-within",
        ":focus-visible",
    ];
    let lower = argument.to_ascii_lowercase();
    STATES.iter().any(|state| lower.contains(state))
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

/// `:has(` arguments as relative selectors, or `None` when one is empty or
/// does not parse (an invalid `:has()` makes the rule invalid, which the style
/// engine then drops).
fn parse_arguments(arguments: &str) -> Option<(HasSpec, (u32, u32, u32))> {
    let mut relative = Vec::new();
    let mut specificity = (0, 0, 0);
    for argument in split_top_level(arguments) {
        let argument = argument.trim();
        let (combinator, rest) = match argument.chars().next()? {
            c @ ('>' | '+' | '~') => (c, argument[1..].trim()),
            _ => (' ', argument),
        };
        if rest.is_empty() {
            return None;
        }
        let selector = match combinator {
            ' ' => format!(":scope {rest}"),
            c => format!(":scope {c} {rest}"),
        };
        let list = crate::stylo::parse_selector_list(&selector)?;
        let alone = crate::stylo::parse_selector_list(rest)?;
        let packed = alone.iter().copied().max().unwrap_or(0);
        specificity = specificity.max((packed >> 20, (packed >> 10) & 0x3ff, packed & 0x3ff));
        if list.is_empty() {
            return None;
        }
        relative.push(Relative {
            selector,
            siblings: matches!(combinator, '+' | '~'),
        });
    }
    Some((
        HasSpec {
            relative,
            mentions_state: mentions_state(arguments),
        },
        specificity,
    ))
}

/// What `:has(arguments)` is replaced with: the class of its spec, with the
/// specificity of its arguments.
fn replacement(number: usize, specificity: (u32, u32, u32)) -> String {
    let mut text = format!(":where(.florui-has-{number})");
    for _ in 0..specificity.0 {
        text.push_str(":not(#florui-has-id)");
    }
    for _ in 0..specificity.1 {
        text.push_str(":not(.florui-has-class)");
    }
    for _ in 0..specificity.2 {
        text.push_str(":not(florui-has-type)");
    }
    text
}

/// `css` with each `:has()` replaced, and the numbers of the ones it held.
pub(crate) fn rewrite(css: &str) -> (Cow<'_, str>, Vec<usize>) {
    let lower = css.to_ascii_lowercase();
    if !lower.contains(":has(") {
        return (Cow::Borrowed(css), Vec::new());
    }
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut numbers = Vec::new();
    let mut at = 0;
    let mut copied = 0;
    while at < bytes.len() {
        match bytes[at] {
            // A comment, or a string: nothing in it is a selector.
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
            b':' if lower.as_bytes()[at..].starts_with(b":has(") => {
                let open = at + ":has".len();
                let mut depth = 0usize;
                let mut close = None;
                for (offset, &b) in bytes[open..].iter().enumerate() {
                    match b {
                        b'(' => depth += 1,
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(open + offset);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let Some(close) = close else {
                    break;
                };
                match parse_arguments(&css[open + 1..close]) {
                    Some((spec, specificity)) => {
                        let number = intern(spec);
                        if !numbers.contains(&number) {
                            numbers.push(number);
                        }
                        out.push_str(&css[copied..at]);
                        out.push_str(&replacement(number, specificity));
                        copied = close + 1;
                        at = close + 1;
                    }
                    None => at = close + 1,
                }
            }
            _ => at += 1,
        }
    }
    out.push_str(&css[copied.min(css.len())..]);
    (Cow::Owned(out), numbers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_has_becomes_a_class_with_the_specificity_of_its_arguments() {
        let (css, numbers) = rewrite(".row:has(.cell) { color: red }");
        assert_eq!(numbers.len(), 1);
        assert_eq!(
            css,
            format!(
                ".row:where(.florui-has-{}):not(.florui-has-class) {{ color: red }}",
                numbers[0]
            )
        );
        let spec = spec(numbers[0]).unwrap();
        assert_eq!(spec.relative[0].selector, ":scope .cell");
        assert!(!spec.relative[0].siblings);
        assert!(!spec.mentions_state);
    }

    #[test]
    fn combinators_lists_and_states_are_read_from_the_arguments() {
        let (_, numbers) = rewrite("a:has(> img, + .b:hover, ~ p span) { x: y }");
        let spec = spec(numbers[0]).unwrap();
        let selectors: Vec<(&str, bool)> = spec
            .relative
            .iter()
            .map(|r| (r.selector.as_str(), r.siblings))
            .collect();
        assert_eq!(
            selectors,
            [
                (":scope > img", false),
                (":scope + .b:hover", true),
                (":scope ~ p span", true)
            ]
        );
        assert!(spec.mentions_state);
    }

    #[test]
    fn the_same_list_has_the_same_number_and_strings_and_comments_are_left_alone() {
        let (first, a) = rewrite(".a:has(.x) {} .b:has(.x) {}");
        assert_eq!(a.len(), 1, "one number for the same arguments");
        assert_eq!(
            first.matches("florui-has-").count() - first.matches("florui-has-class").count(),
            2
        );
        let (css, none) = rewrite("/* a:has(.x) */ .c::before { content: \":has(.y)\" }");
        assert!(none.is_empty());
        assert_eq!(css, "/* a:has(.x) */ .c::before { content: \":has(.y)\" }");
    }

    #[test]
    fn an_invalid_has_is_left_for_the_style_engine_to_drop() {
        let (css, numbers) = rewrite("a:has() { x: y } b:has(,) { x: y }");
        assert!(numbers.is_empty());
        assert_eq!(css, "a:has() { x: y } b:has(,) { x: y }");
    }

    #[test]
    fn the_specificity_of_the_most_specific_argument_is_kept() {
        let (css, _) = rewrite("x:has(#a, .b .c, d) { }");
        assert_eq!(css.matches(":not(#florui-has-id)").count(), 1);
        assert_eq!(css.matches(":not(.florui-has-class)").count(), 0);
    }
}

#[cfg(test)]
mod cascade_tests {
    use florui::prelude::*;

    use crate::cascade::{Viewport, compute};
    use crate::color::Rgba;
    use crate::interaction::InteractionState;
    use crate::stylesheet_parse::parse_stylesheet;
    use crate::tree::Arena;

    fn colors(tree: &Element, css: &str, state: &InteractionState) -> (Arena, Vec<Rgba>) {
        let arena = Arena::build(tree);
        let rules = parse_stylesheet(css).unwrap();
        let styles = compute(
            &arena,
            &rules,
            state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        let all: Vec<usize> = arena.find_all(|_, _| true);
        let colors = all.iter().map(|id| styles[id].background_color).collect();
        (arena, colors)
    }

    const GREEN: Rgba = Rgba::opaque(0, 255, 0);

    #[test]
    fn a_row_with_a_cell_below_it_takes_the_rule() {
        let tree: Element = view! {
            <div class="row"><span class="cell" /></div>
        };
        let (_, colors) = colors(
            &tree,
            ".row:has(.cell) { background-color: #00ff00 }",
            &InteractionState::new(),
        );
        assert_eq!(colors[0], GREEN, "the row");
        assert_ne!(colors[1], GREEN, "the cell has no cell below it");
    }

    #[test]
    fn a_row_without_one_does_not() {
        let tree: Element = view! {
            <div><div class="row"><span class="other" /></div><div class="row"><span class="cell" /></div></div>
        };
        let (arena, colors) = colors(
            &tree,
            ".row:has(.cell) { background-color: #00ff00 }",
            &InteractionState::new(),
        );
        let rows = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"));
        assert_ne!(colors[rows[0]], GREEN);
        assert_eq!(colors[rows[1]], GREEN);
    }

    #[test]
    fn cousins_under_cards_that_differ_in_what_they_have_do_not_share_a_style() {
        let tree: Element = view! {
            <div>
                <div class="card"><span class="child" /><span class="x" /></div>
                <div class="card"><span class="child" /></div>
            </div>
        };
        let (arena, colors) = colors(
            &tree,
            ".card:has(.x) .child { background-color: #00ff00 }",
            &InteractionState::new(),
        );
        let children = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "child"));
        assert_eq!(colors[children[0]], GREEN);
        assert_ne!(colors[children[1]], GREEN, "its card has no .x");
    }
    #[test]
    fn an_element_with_text_is_not_empty_and_one_without_any_is() {
        let tree: Element = view! {
            <div><span class="a" /><span class="b">"t"</span><span class="c"><i /></span></div>
        };
        let (arena, colors) = colors(
            &tree,
            "span:empty { background-color: #00ff00 }",
            &InteractionState::new(),
        );
        let of = |class: &str| {
            arena
                .find(|a, id| a.classes(id).iter().any(|c| c == class))
                .expect("the node exists")
        };
        assert_eq!(colors[of("a")], GREEN);
        assert_ne!(colors[of("b")], GREEN, "it holds text");
        assert_ne!(colors[of("c")], GREEN, "it holds an element");
    }
}
