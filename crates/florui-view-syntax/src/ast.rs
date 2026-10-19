//! The parsed shape of a `view!` body — shared by `florui-macros`'s real
//! codegen and `florui-fmt`'s own formatter (see this crate's own module
//! doc for why that split exists).

use syn::{Expr, Ident, LitStr};

#[cfg_attr(test, derive(Debug))]
pub enum Node {
    /// A `{expr}` child: an arbitrary Rust expression contributing zero or
    /// more sibling elements through `IntoNodes`.
    Expr(Expr),
    /// A run of bare text between tags, already reconstructed into a single
    /// string by [`crate::text`].
    Text(String),
    /// A tag: lowercase is a primitive HTML-like element, capitalized is a
    /// component call. `self_closing` tags never carry a `children` value.
    Element {
        tag: Ident,
        attrs: Vec<(AttrName, AttrValue)>,
        children: Vec<Node>,
        self_closing: bool,
    },
}

// `Expr` genuinely is much larger than `LitStr`, but this tree is parsed
// once per `view!` invocation (or once per real formatter run), never a
// hot allocation path boxing would meaningfully help.
#[allow(clippy::large_enum_variant)]
#[cfg_attr(test, derive(Debug))]
pub enum AttrValue {
    Lit(LitStr),
    Expr(Expr),
}

/// The name of an attribute: one identifier (`class`, `viewBox`), or several
/// joined by `-` or `:` (`stop-color`, `xlink:href`), as SVG has them. Only a
/// primitive element takes the second kind; a component's props are fields.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug))]
pub struct AttrName {
    first: Ident,
    text: String,
}

impl AttrName {
    pub fn new(first: Ident, text: String) -> Self {
        Self { first, text }
    }

    /// The name as written, with its `-` and `:`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The identifier, when the name is only one.
    pub fn ident(&self) -> Option<&Ident> {
        (self.first == self.text.as_str()).then_some(&self.first)
    }

    pub fn span(&self) -> proc_macro2::Span {
        self.first.span()
    }
}

impl std::fmt::Display for AttrName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl PartialEq<str> for AttrName {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}

impl PartialEq<&str> for AttrName {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

impl Node {
    pub fn is_component(tag: &Ident) -> bool {
        tag.to_string()
            .chars()
            .next()
            .is_some_and(|c| c.is_uppercase())
    }
}
