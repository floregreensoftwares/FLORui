//! The element tree produced by `view!` and by components.
//!
//! This is a plain data tree with no identity, styling, layout, or paint
//! attached yet — those are separate, not-yet-built subsystems. An `Element`
//! only records what was written: tags, string attributes, event handlers,
//! text, and children.

use florui_reactive::Binding;

use crate::{Handler, SelectionHandler, ValueHandler};

/// A node produced by `view!`: a tagged element, a text run, or a fragment
/// (a sequence of siblings with no wrapping box of their own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Element {
    Node(ElementNode),
    Text(String),
    Fragment(Vec<Element>),
    /// A `<Portal>`'s own content — see `crates/florui-style/src/tree.rs`'s
    /// `Arena::build` for how this becomes an independent overlay root
    /// instead of being spliced in place like `Fragment`.
    Portal(Vec<Element>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementNode {
    pub tag: &'static str,
    pub attrs: Vec<(String, String)>,
    /// Callbacks from `on*` attributes (`onclick={...}`), keyed by event
    /// name with the leading `on` stripped (`"click"`).
    pub handlers: Vec<(String, Handler)>,
    /// A typed write-back channel for a primitive attribute (today, only
    /// `value={binding}` on `<input>`) alongside its plain-string form
    /// already in `attrs` -- every string-only consumer (measurement,
    /// paint) keeps reading `attrs` unchanged; only a write path (a real
    /// text-editing widget) needs this to request an update back to the
    /// owner. Mutually exclusive with `value_handlers` for the same
    /// attribute -- see slots-and-bindings.md's "Optional convenience and
    /// explicit control."
    pub bindings: Vec<(String, Binding<String>)>,
    /// The explicit (non-`Binding`) controlled-value channel -- `oninput`
    /// reports a new value directly rather than through a typed
    /// `Binding`'s owner-decides-acceptance contract. `view!`'s own
    /// codegen only ever emits one of `bindings`/`value_handlers` for a
    /// given attribute, never both.
    pub value_handlers: Vec<(String, ValueHandler)>,
    /// `onselectionchange` on `<select multiple>` -- reports the control's
    /// own computed new selection set. See [`SelectionHandler`]'s own doc.
    pub selection_handlers: Vec<(String, SelectionHandler)>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn node(tag: &'static str, attrs: Vec<(String, String)>, children: Vec<Element>) -> Self {
        Self::node_with_handlers(tag, attrs, Vec::new(), children)
    }

    pub fn node_with_handlers(
        tag: &'static str,
        attrs: Vec<(String, String)>,
        handlers: Vec<(String, Handler)>,
        children: Vec<Element>,
    ) -> Self {
        Self::node_with_bindings(tag, attrs, handlers, Vec::new(), children)
    }

    pub fn node_with_bindings(
        tag: &'static str,
        attrs: Vec<(String, String)>,
        handlers: Vec<(String, Handler)>,
        bindings: Vec<(String, Binding<String>)>,
        children: Vec<Element>,
    ) -> Self {
        Self::node_with_value_handlers(tag, attrs, handlers, bindings, Vec::new(), children)
    }

    pub fn node_with_value_handlers(
        tag: &'static str,
        attrs: Vec<(String, String)>,
        handlers: Vec<(String, Handler)>,
        bindings: Vec<(String, Binding<String>)>,
        value_handlers: Vec<(String, ValueHandler)>,
        children: Vec<Element>,
    ) -> Self {
        Self::node_with_selection_handlers(
            tag,
            attrs,
            handlers,
            bindings,
            value_handlers,
            Vec::new(),
            children,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn node_with_selection_handlers(
        tag: &'static str,
        attrs: Vec<(String, String)>,
        handlers: Vec<(String, Handler)>,
        bindings: Vec<(String, Binding<String>)>,
        value_handlers: Vec<(String, ValueHandler)>,
        selection_handlers: Vec<(String, SelectionHandler)>,
        children: Vec<Element>,
    ) -> Self {
        Element::Node(ElementNode {
            tag,
            attrs,
            handlers,
            bindings,
            value_handlers,
            selection_handlers,
            children,
        })
    }

    pub fn text(text: impl Into<String>) -> Self {
        Element::Text(text.into())
    }
}

/// Without this, dropping `Element` recurses once per tree level (the
/// compiler-generated default), which overflows the stack for a deep
/// enough tree. An explicit stack instead: each node's own children are
/// moved out before it drops, so its default per-field drop has nothing
/// left to recurse into.
impl Drop for Element {
    fn drop(&mut self) {
        let mut pending: Vec<Element> = match self {
            Element::Node(node) => std::mem::take(&mut node.children),
            Element::Fragment(children) | Element::Portal(children) => std::mem::take(children),
            Element::Text(_) => return,
        };
        while let Some(mut element) = pending.pop() {
            match &mut element {
                Element::Node(node) => pending.extend(std::mem::take(&mut node.children)),
                Element::Fragment(children) | Element::Portal(children) => {
                    pending.extend(std::mem::take(children))
                }
                Element::Text(_) => {}
            }
        }
    }
}
