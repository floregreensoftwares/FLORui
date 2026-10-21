//! Which elements get a box for `::before` and `::after`.
//!
//! Generated content is boxes in the arena, laid out and painted like any other.
//! An element that a rule gives `content` through `::before` or `::after` is built
//! with two such nodes (what a node holds is read after the cascade, see
//! [`crate::Arena::apply_generated`]); the rest are built without, so a page
//! pays only for the elements its rules can reach. The test is on the text of the
//! rules: the compound selector in front of each `::before` and `::after`, in a
//! rule that declares `content`.

/// The two pseudo-elements that generate a box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pseudo {
    Before,
    After,
    /// The first letter of a block, a node inside the element its text is in.
    FirstLetter,
}

impl Pseudo {
    /// The tag of the node of this pseudo-element, which no author can write.
    pub fn tag(self) -> &'static str {
        match self {
            Pseudo::Before => "florui-before",
            Pseudo::After => "florui-after",
            Pseudo::FirstLetter => "florui-first-letter",
        }
    }
}

/// What a rule says of the elements that may have a generated box: a tag, some
/// classes and an id, any of which may be missing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Subject {
    tag: Option<String>,
    classes: Vec<String>,
    id: Option<String>,
}

impl Subject {
    /// Whether an element with this tag and attributes is one the subject can name.
    pub(crate) fn fits(&self, tag: &str, attrs: &[(String, String)]) -> bool {
        let attr = |name: &str| {
            attrs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        self.tag.as_deref().is_none_or(|t| t == tag)
            && (self.classes.is_empty() || {
                let class = attr("class").unwrap_or("");
                self.classes
                    .iter()
                    .all(|c| class.split_whitespace().any(|held| held == c))
            })
            && self.id.as_deref().is_none_or(|i| attr("id") == Some(i))
    }
}

/// What a stylesheet says of the nodes the arena builds beside its elements: which
/// elements get a `::before` and an `::after`, and which a `::first-letter`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Extras {
    pub pseudo: Vec<Subject>,
    pub first_letter: Vec<Subject>,
    /// The numbers of the selectors the sheet holds a `::first-letter` of.
    pub first_letter_numbers: Vec<usize>,
}

/// The rightmost compound of `selector`: the elements it can match.
pub(crate) fn subject_of_selector(selector: &str) -> Subject {
    compound_before(selector, selector.len())
}

/// The elements that get generated boxes: those a [`Subject`] of a rule fits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PseudoSlots {
    subjects: Vec<Subject>,
    first_letter: Vec<Subject>,
}

/// Tags that never have a generated box: replaced elements and the controls
/// that draw their own content.
fn has_no_generated_box(tag: &str) -> bool {
    matches!(
        tag,
        "img" | "input" | "icon" | "svg" | "br" | "textarea" | "select"
    )
}

impl PseudoSlots {
    /// The slots of a set of rules: those of the author's rules and of the
    /// default sheet.
    pub fn from_rules(rules: &[crate::Rule]) -> Self {
        let mut subjects = crate::default_stylesheet::rule().pseudo_subjects().to_vec();
        for rule in rules {
            for subject in rule.pseudo_subjects() {
                if !subjects.contains(subject) {
                    subjects.push(subject.clone());
                }
            }
        }
        let mut first_letter = Vec::new();
        for rule in rules {
            for subject in rule.first_letter_subjects() {
                if !first_letter.contains(subject) {
                    first_letter.push(subject.clone());
                }
            }
        }
        Self {
            subjects,
            first_letter,
        }
    }

    /// Whether no element is built with a node of a pseudo-element.
    pub fn is_empty(&self) -> bool {
        self.subjects.is_empty() && self.first_letter.is_empty()
    }

    /// Whether an element with this tag and attributes may have a first letter.
    pub(crate) fn wants_first_letter(&self, tag: &str, attrs: &[(String, String)]) -> bool {
        !has_no_generated_box(tag)
            && self
                .first_letter
                .iter()
                .any(|subject| subject.fits(tag, attrs))
    }

    /// Whether an element with this tag and attributes is built with generated nodes.
    /// Read for every element each time the arena is built or refreshed, so it
    /// allocates nothing and looks at the tag first.
    pub(crate) fn wants(&self, tag: &str, attrs: &[(String, String)]) -> bool {
        !has_no_generated_box(tag) && self.subjects.iter().any(|subject| subject.fits(tag, attrs))
    }
}

/// The subjects of the `::before` and `::after` rules of `css` that declare `content`.
pub(crate) fn subjects_in(css: &str) -> Vec<Subject> {
    let lower = css.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut subjects = Vec::new();
    let mut from = 0;
    while let Some(found) = lower[from..].find(':') {
        let at = from + found;
        from = at + 1;
        // `::before` and `::after`, or the one-colon forms of the same.
        let after_colons = if bytes.get(at + 1) == Some(&b':') {
            at + 2
        } else if at > 0 && bytes[at - 1] == b':' {
            continue;
        } else {
            at + 1
        };
        let name = &lower[after_colons..];
        let length = if name.starts_with("before") {
            6
        } else if name.starts_with("after") {
            5
        } else {
            continue;
        };
        let end = after_colons + length;
        if bytes
            .get(end)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        {
            continue;
        }
        if !body_declares_content(&lower, end) {
            continue;
        }
        let subject = compound_before(css, at);
        if !subjects.contains(&subject) {
            subjects.push(subject);
        }
    }
    subjects
}

/// Whether the rule whose selector ends at `from` declares `content`.
fn body_declares_content(lower: &str, from: usize) -> bool {
    let bytes = lower.as_bytes();
    let mut depth = 0usize;
    let mut open = None;
    for (offset, &b) in bytes[from..].iter().enumerate() {
        match b {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'{' if depth == 0 => {
                open = Some(from + offset + 1);
                break;
            }
            b';' | b'}' if depth == 0 => return false,
            _ => {}
        }
    }
    let Some(open) = open else {
        return false;
    };
    let mut nesting = 1usize;
    let mut close = bytes.len();
    for (offset, &b) in bytes[open..].iter().enumerate() {
        match b {
            b'{' => nesting += 1,
            b'}' => {
                nesting -= 1;
                if nesting == 0 {
                    close = open + offset;
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &lower[open..close];
    body.match_indices("content").any(|(at, _)| {
        let before_ok =
            at == 0 || !matches!(body.as_bytes()[at - 1], b'-' | b'_' | b'a'..=b'z' | b'0'..=b'9');
        let rest = body[at + "content".len()..].trim_start();
        before_ok && rest.starts_with(':')
    })
}

/// The compound selector that ends at `at` (the first colon of the pseudo-element).
fn compound_before(css: &str, at: usize) -> Subject {
    let bytes = css.as_bytes();
    let mut start = at;
    let mut depth = 0usize;
    while start > 0 {
        let b = bytes[start - 1];
        match b {
            b')' | b']' => depth += 1,
            b'(' | b'[' => depth = depth.saturating_sub(1),
            b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'+' | b'~' | b',' | b'{' | b'}' | b';'
                if depth == 0 =>
            {
                break;
            }
            _ => {}
        }
        start -= 1;
    }
    parse_compound(&css[start..at])
}

fn is_ident(b: char) -> bool {
    b.is_alphanumeric() || b == '-' || b == '_' || !b.is_ascii()
}

fn parse_compound(compound: &str) -> Subject {
    let chars: Vec<char> = compound.chars().collect();
    let mut subject = Subject::default();
    let mut i = 0;
    let read_ident = |i: &mut usize| {
        let begin = *i;
        while *i < chars.len() && is_ident(chars[*i]) {
            *i += 1;
        }
        chars[begin..*i].iter().collect::<String>()
    };
    if i < chars.len() && chars[i] == '*' {
        i += 1;
    } else if i < chars.len() && is_ident(chars[i]) {
        subject.tag = Some(read_ident(&mut i));
    }
    while i < chars.len() {
        match chars[i] {
            '.' => {
                i += 1;
                subject.classes.push(read_ident(&mut i));
            }
            '#' => {
                i += 1;
                subject.id = Some(read_ident(&mut i));
            }
            '[' | '(' => {
                let mut depth = 0;
                while i < chars.len() {
                    match chars[i] {
                        '[' | '(' => depth += 1,
                        ']' | ')' => {
                            depth -= 1;
                            if depth == 0 {
                                i += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    subject
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(tag: Option<&str>, classes: &[&str], id: Option<&str>) -> Subject {
        Subject {
            tag: tag.map(str::to_owned),
            classes: classes.iter().map(|c| (*c).to_owned()).collect(),
            id: id.map(str::to_owned),
        }
    }

    #[test]
    fn a_rule_that_declares_content_names_its_subject() {
        assert_eq!(
            subjects_in(".note::before { content: \"x\"; color: red }"),
            vec![subject(None, &["note"], None)]
        );
        assert_eq!(
            subjects_in("div.a.b:hover::after, p#i:before { content: ''; }"),
            vec![
                subject(Some("div"), &["a", "b"], None),
                subject(Some("p"), &[], Some("i"))
            ]
        );
    }

    #[test]
    fn a_rule_without_content_makes_no_slots() {
        assert!(subjects_in("*::before, *::after { box-sizing: border-box }").is_empty());
        assert!(
            subjects_in(".a::before { align-content: center; justify-content: end }").is_empty()
        );
    }

    #[test]
    fn combinators_and_nested_conditions_do_not_confuse_the_subject() {
        assert_eq!(
            subjects_in("ul > li:not(.skip)::before { content: '-' }"),
            vec![subject(Some("li"), &[], None)]
        );
        assert_eq!(
            subjects_in("@media (min-width: 1px) { .a ~ .b[x=\"y z\"]::after { content: 'x' } }"),
            vec![subject(None, &["b"], None)]
        );
        assert_eq!(
            subjects_in("*::before { content: '' }"),
            vec![subject(None, &[], None)]
        );
    }

    #[test]
    fn slots_fit_an_element_by_tag_classes_and_id_and_skip_replaced_ones() {
        let slots = PseudoSlots {
            subjects: vec![
                subject(Some("div"), &["a"], None),
                subject(None, &[], Some("only")),
            ],
            first_letter: Vec::new(),
        };
        let attrs = |class: &str, id: &str| {
            vec![
                ("class".to_owned(), class.to_owned()),
                ("id".to_owned(), id.to_owned()),
            ]
        };
        assert!(slots.wants("div", &attrs("a b", "")));
        assert!(!slots.wants("div", &attrs("b", "")));
        assert!(!slots.wants("span", &attrs("a", "")));
        assert!(slots.wants("span", &attrs("", "only")));
        assert!(!slots.wants("img", &attrs("", "only")));
        assert!(!PseudoSlots::default().wants("div", &[]));
    }
}

#[cfg(test)]
mod arena_tests {
    use florui::prelude::*;

    use crate::cascade::{Display, Viewport, compute};
    use crate::interaction::InteractionState;
    use crate::stylesheet_parse::parse_stylesheet;
    use crate::tree::Arena;
    use crate::{Pseudo, PseudoSlots};

    fn styled(tree: &Element, css: &str) -> (Arena, crate::StyleMap) {
        let rules = parse_stylesheet(css).unwrap();
        let slots = PseudoSlots::from_rules(&rules);
        let mut arena = Arena::build_with(tree, &slots);
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

    #[test]
    fn an_element_with_a_content_rule_gets_a_box_before_and_after_its_children() {
        let tree: Element = view! { <div><p class="p">"hello"</p><p>"plain"</p></div> };
        let (arena, styles) = styled(
            &tree,
            ".p::before { content: \"NOTE: \"; color: red } .p::after { content: attr(class) }",
        );
        let div = arena.roots()[0];
        let [p, plain] = arena.children(div)[..] else {
            panic!("two paragraphs");
        };
        assert_eq!(arena.element_child_ids(p).len(), 0);
        let kids = arena.children(p);
        assert_eq!(kids.len(), 2);
        assert_eq!(arena.generated(kids[0]), Some(Pseudo::Before));
        assert_eq!(arena.generated(kids[1]), Some(Pseudo::After));
        assert_eq!(arena.text_content(kids[0]), "NOTE: ");
        assert_eq!(arena.text_content(kids[1]), "p");
        assert_eq!(styles[&kids[0]].color, crate::Rgba::opaque(255, 0, 0));
        assert!(arena.children(plain).is_empty(), "only .p has slots");
    }

    #[test]
    fn a_box_without_content_is_hidden() {
        let tree: Element = view! { <p class="p">"hello"</p> };
        let (arena, styles) = styled(
            &tree,
            ".p::before { content: \"x\" } .p::after { color: blue } .p:hover::after { content: \"y\" }",
        );
        let p = arena.roots()[0];
        let [before, after] = arena.children(p)[..] else {
            panic!("two boxes");
        };
        assert_ne!(styles[&before].display, Display::None);
        assert_eq!(styles[&after].display, Display::None);
        assert_eq!(arena.text_content(after), "");
    }

    #[test]
    fn the_generated_boxes_do_not_count_as_siblings_for_selectors() {
        let tree: Element = view! { <div class="p"><span class="a" /><span class="b" /></div> };
        let (arena, styles) = styled(
            &tree,
            ".p::before { content: 'x' } .p::after { content: 'y' } \
             .a:first-child { color: #112233 } .b:last-child { color: #445566 } \
             .p:empty { color: red }",
        );
        let div = arena.roots()[0];
        let [_, a, b, _] = arena.children(div)[..] else {
            panic!("boxes around two spans");
        };
        assert_eq!(styles[&a].color, crate::Rgba::opaque(0x11, 0x22, 0x33));
        assert_eq!(styles[&b].color, crate::Rgba::opaque(0x44, 0x55, 0x66));
        assert_ne!(styles[&div].color, crate::Rgba::opaque(255, 0, 0));
    }

    #[test]
    fn quotation_marks_follow_the_nesting_of_the_boxes_in_document_order() {
        let tree: Element = view! {
            <div>
                <p class="q">"a" <span class="q">"b"</span></p>
            </div>
        };
        let (arena, _) = styled(
            &tree,
            ".q::before { content: open-quote } .q::after { content: close-quote }",
        );
        let marks: Vec<String> = arena
            .find_all(|tree, id| tree.generated(id).is_some())
            .into_iter()
            .map(|id| arena.text_content(id).to_owned())
            .collect();
        assert_eq!(
            marks,
            ["\u{201c}", "\u{2018}", "\u{2019}", "\u{201d}"],
            "outer open, inner open, inner close, outer close"
        );
    }

    #[test]
    fn a_q_element_is_wrapped_in_the_quotation_marks_of_its_depth_with_no_author_rule() {
        let tree: Element = view! { <p><q>"a " <q>"b"</q></q></p> };
        let (arena, _) = styled(&tree, "");
        let marks: Vec<String> = arena
            .find_all(|tree, id| tree.generated(id).is_some())
            .into_iter()
            .map(|id| arena.text_content(id).to_owned())
            .collect();
        assert_eq!(marks, ["\u{201c}", "\u{2018}", "\u{2019}", "\u{201d}"]);
    }
    #[test]
    fn rebuilding_with_the_same_slots_keeps_the_arena_and_other_slots_build_it_anew() {
        let tree: Element = view! { <p class="p">"hello"</p> };
        let rules = parse_stylesheet(".p::before { content: 'x' }").unwrap();
        let slots = PseudoSlots::from_rules(&rules);
        let mut arena = Arena::build_with(&tree, &slots);
        let shape = arena.style_shape();
        assert!(arena.rebuild_with(&tree, &slots));
        assert_eq!(arena.style_shape(), shape);
        assert!(!arena.rebuild_with(&tree, &PseudoSlots::default()));
        assert!(arena.children(arena.roots()[0]).is_empty());
    }
}

#[cfg(test)]
mod session_tests {
    use florui::prelude::*;

    use crate::cascade::{Viewport, compute};
    use crate::interaction::InteractionState;
    use crate::stylesheet_parse::parse_stylesheet;
    use crate::tree::Arena;
    use crate::{PseudoSlots, StyleSession};

    #[test]
    fn a_hover_restyles_the_boxes_of_the_node_and_part_of_the_tree_as_a_whole_cascade_does() {
        let tree: Element = view! {
            <div>
                <p class="p">"one"</p>
                <p class="p">"two"</p>
                <p class="p">"three"</p>
            </div>
        };
        let rules = parse_stylesheet(
            ".p::before { content: \"-\" } .p:hover::after { content: \"<\"; color: #c02020 } \
             .p:hover { background-color: #cce0ff }",
        )
        .unwrap();
        let arena = Arena::build_with(&tree, &PseudoSlots::from_rules(&rules));
        let viewport = Viewport::default();
        let mut session = StyleSession::new();
        let mut timeline = crate::AnimationTimeline::default();
        let mut fresh_timeline = crate::AnimationTimeline::default();
        let second = arena.children(arena.roots()[0])[1];
        let after = arena
            .generated_child(second, crate::Pseudo::After)
            .expect("the paragraph has an ::after box");
        let idle = InteractionState::new();
        session.compute(&arena, &rules, &idle, viewport, &mut timeline);
        let hovered = InteractionState::new().with_hovered(second);
        let (kept, changed) =
            session.compute_changed(&arena, &rules, &hovered, viewport, &mut timeline);
        let fresh = compute(&arena, &rules, &hovered, viewport, &mut fresh_timeline);
        assert!(changed.is_some(), "a hover restyles part of the tree");
        for (id, style) in fresh.iter() {
            assert_eq!(kept.get(id), Some(style), "node {id}");
        }
        assert_ne!(kept[&after].display, crate::Display::None);
        assert_eq!(kept[&after].color, crate::Rgba::opaque(0xc0, 0x20, 0x20));
    }
}
