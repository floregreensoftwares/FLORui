//! The element tree produced by `view!` and by components.
//!
//! This is a plain data tree with no identity, styling, layout, or paint
//! attached yet — those are separate, not-yet-built subsystems. An `Element`
//! only records what was written: tags, string attributes, event handlers,
//! text, and children.

use florui_reactive::Binding;

use crate::{Handler, SelectionHandler, SubmitHandler, ValueHandler};

/// Whether `view!` records where each element was written. On in a debug
/// build and with the `source-locations` feature; off otherwise, so a release
/// build carries no file paths and pays nothing for it.
pub const SOURCE_LOCATIONS: bool = cfg!(debug_assertions) || cfg!(feature = "source-locations");

/// The part of where an element was written that is known when the code is
/// compiled: the file, line and column of its own `<tag` in a `view!` block,
/// and its place in the block (`div > ul > li[3]` is the third `li` of the
/// `ul`). `view!` emits one constant of this per element, so an element holds
/// a reference to it rather than a copy.
#[derive(Debug, PartialEq, Eq)]
pub struct SourceSite {
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
    pub path: &'static str,
}

/// Where an element was written and by whom: its [`SourceSite`] plus the
/// component whose body built it, which is only known while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLocation {
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
    /// The component active while the element was built, if any.
    pub component: Option<&'static str>,
    pub path: &'static str,
}

impl std::fmt::Display for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(component) = self.component {
            write!(f, "{component} ")?;
        }
        write!(
            f,
            "{}:{}:{} {}",
            self.file, self.line, self.column, self.path
        )
    }
}

/// An element's [`SourceLocation`], if it has one. Where an element was
/// written is not part of what it is, so two elements with the same content are
/// equal whatever their locations: a `view!` element equals a hand-built one.
#[derive(Clone, Copy, Default)]
pub struct Provenance {
    site: Option<&'static SourceSite>,
    component: Option<&'static str>,
}

impl Provenance {
    pub const fn none() -> Self {
        Self {
            site: None,
            component: None,
        }
    }

    /// The provenance of an element built now at `site`: the component is
    /// whichever one is active on this thread.
    pub fn at(site: &'static SourceSite) -> Self {
        Self {
            site: Some(site),
            component: florui_reactive::trace::current_component(),
        }
    }

    pub fn location(&self) -> Option<SourceLocation> {
        self.site.map(|site| SourceLocation {
            file: site.file,
            line: site.line,
            column: site.column,
            component: self.component,
            path: site.path,
        })
    }
}

impl PartialEq for Provenance {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Provenance {}

impl std::fmt::Debug for Provenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.location() {
            Some(location) => write!(f, "Provenance({location})"),
            None => f.write_str("Provenance(none)"),
        }
    }
}

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
    /// attribute (optional convenience versus
    /// explicit control).
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
    /// `onsubmit` on `<form>` -- see [`SubmitHandler`].
    pub submit_handlers: Vec<(String, SubmitHandler)>,
    /// Where this element was written; see [`Provenance`].
    pub source: Provenance,
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
            submit_handlers: Vec::new(),
            source: Provenance::none(),
            children,
        })
    }

    /// Attaches `onsubmit` handlers to an element built by one of the
    /// constructors above. A no-op for anything but a node.
    pub fn with_submit_handlers(mut self, handlers: Vec<(String, SubmitHandler)>) -> Self {
        if let Element::Node(node) = &mut self {
            node.submit_handlers = handlers;
        }
        self
    }

    /// Records where an element built by one of the constructors above was
    /// written. A no-op for anything but a node.
    pub fn with_source(mut self, source: Provenance) -> Self {
        if let Element::Node(node) = &mut self {
            node.source = source;
        }
        self
    }

    /// Where this element was written, if it is a node and that was recorded.
    pub fn source(&self) -> Option<SourceLocation> {
        match self {
            Element::Node(node) => node.source.location(),
            _ => None,
        }
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
