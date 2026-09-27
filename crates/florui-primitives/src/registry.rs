//! The closed set of lowercase primitive tags `view!` recognizes, and the
//! metadata needed to validate their use consistently in one place instead
//! of scattering ad hoc checks through the macro.
//!
//! A capitalized tag (`<Card>`) is always a user-defined `#[component]`
//! call, never looked up here — this registry only ever describes
//! primitives.
//!
//! There is no definitive primitive list yet; this is a deliberately small
//! starting vocabulary for pages, forms, and semantic composition. Being
//! listed here means `view!` recognizes the tag, not that a
//! layout/paint/interaction engine implements its behavior — no such
//! engine exists yet; see [`Status`]. The tag only sets defaults and
//! semantics; CSS still decides layout participation (a `div` can be
//! `display: grid`, a `span` can be `display: block`), so this registry
//! must never harden into a fixed widget catalog.

/// How much of a primitive's real behavior actually exists.
///
/// Every primitive is [`Status::Recognized`] today: no style, layout,
/// paint, or interaction engine exists yet for any tag. This distinction
/// exists so that once real per-element behavior starts landing, tags that
/// still only parse can be told apart from ones the engine actually
/// implements, instead of both looking identical because both compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// `view!` accepts the tag and builds an element for it. No claim is
    /// made about its default styling, layout participation, or
    /// interactive behavior.
    Recognized,
}

/// Whether a primitive can have children, per HTML's void-element list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Content {
    /// May contain nested elements/text (`<div>...</div>`).
    Normal,
    /// Never has children; must be self-closing (`<br />`, `<img />`).
    Void,
}

#[derive(Debug, Clone, Copy)]
pub struct Primitive {
    pub tag: &'static str,
    pub content: Content,
    pub status: Status,
    /// A real implication of this tag worth knowing, for documentation
    /// only — `view!` does not enforce most of these yet.
    pub note: &'static str,
}

macro_rules! primitive {
    ($tag:literal, $content:expr, $note:expr) => {
        Primitive {
            tag: $tag,
            content: $content,
            status: Status::Recognized,
            note: $note,
        }
    };
}

pub const PRIMITIVES: &[Primitive] = &[
    // Structure and grouping.
    primitive!(
        "div",
        Content::Normal,
        "no default display; a block box until styled otherwise"
    ),
    primitive!(
        "span",
        Content::Normal,
        "no default display; an inline box until styled otherwise"
    ),
    // Semantic sections.
    primitive!(
        "main",
        Content::Normal,
        "one top-level landmark per document, once accessibility exists"
    ),
    primitive!("section", Content::Normal, "a generic sectioning landmark"),
    primitive!("article", Content::Normal, "a self-contained composition"),
    primitive!("aside", Content::Normal, "tangentially related content"),
    primitive!(
        "header",
        Content::Normal,
        "introductory content for its nearest sectioning ancestor"
    ),
    primitive!(
        "footer",
        Content::Normal,
        "footer content for its nearest sectioning ancestor"
    ),
    primitive!("nav", Content::Normal, "a navigation landmark"),
    // Text.
    primitive!(
        "h1",
        Content::Normal,
        "heading level 1; combines default style with heading semantics"
    ),
    primitive!(
        "h2",
        Content::Normal,
        "heading level 2; combines default style with heading semantics"
    ),
    primitive!(
        "h3",
        Content::Normal,
        "heading level 3; combines default style with heading semantics"
    ),
    primitive!(
        "h4",
        Content::Normal,
        "heading level 4; combines default style with heading semantics"
    ),
    primitive!(
        "h5",
        Content::Normal,
        "heading level 5; combines default style with heading semantics"
    ),
    primitive!(
        "h6",
        Content::Normal,
        "heading level 6; combines default style with heading semantics"
    ),
    primitive!("p", Content::Normal, "a paragraph"),
    primitive!(
        "strong",
        Content::Normal,
        "strong importance; combines default style with semantics"
    ),
    primitive!(
        "em",
        Content::Normal,
        "stress emphasis; combines default style with semantics"
    ),
    primitive!("small", Content::Normal, "side comments and small print"),
    primitive!("code", Content::Normal, "a fragment of computer code"),
    primitive!(
        "pre",
        Content::Normal,
        "preformatted text; needs exact whitespace handling once text layout exists"
    ),
    primitive!(
        "br",
        Content::Void,
        "a real line break, not a styled empty box"
    ),
    primitive!("hr", Content::Void, "a thematic break"),
    // Lists.
    primitive!("ul", Content::Normal, "an unordered list"),
    primitive!("ol", Content::Normal, "an ordered list"),
    primitive!("li", Content::Normal, "a list item"),
    // Interaction.
    primitive!(
        "button",
        Content::Normal,
        "real focus, keyboard activation, and disabled-state behavior"
    ),
    primitive!(
        "a",
        Content::Normal,
        "real focus, Enter activation, a default external-link-open action, and \
         :link/:visited styling backed by an in-memory, per-session visited-link record"
    ),
    // Forms.
    primitive!("form", Content::Normal, "a form"),
    primitive!("label", Content::Normal, "a caption for a form control"),
    primitive!(
        "input",
        Content::Void,
        "behavior depends on `type`; see `INITIAL_INPUT_TYPES`"
    ),
    primitive!("textarea", Content::Normal, "a multiline text control"),
    primitive!(
        "select",
        Content::Normal,
        "a dropdown; open/value are the app's own controlled state, like checked on a checkbox"
    ),
    primitive!(
        "option",
        Content::Normal,
        "an item within a select, chosen via its own selected attribute"
    ),
    primitive!(
        "optgroup",
        Content::Normal,
        "labels a group of options within a select, via its own label attribute"
    ),
    primitive!("fieldset", Content::Normal, "groups related form controls"),
    primitive!("legend", Content::Normal, "a caption for a fieldset"),
    // Images.
    primitive!("img", Content::Void, "an image"),
];

pub fn find(tag: &str) -> Option<&'static Primitive> {
    PRIMITIVES.iter().find(|p| p.tag == tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_documented_examples() {
        for tag in ["div", "span", "h2", "button"] {
            assert!(find(tag).is_some(), "{tag} should be registered");
        }
    }

    #[test]
    fn void_elements_are_marked_void() {
        for tag in ["br", "hr", "img", "input"] {
            assert_eq!(
                find(tag).unwrap().content,
                Content::Void,
                "{tag} should be void"
            );
        }
    }

    #[test]
    fn normal_elements_are_not_void() {
        assert_eq!(find("div").unwrap().content, Content::Normal);
    }

    #[test]
    fn rejects_unknown_tags() {
        assert!(find("marquee").is_none());
    }
}
