//! An indexed, parent-linked view of a `florui::Element` tree.
//!
//! `Element` itself has no parent pointers (children are owned by value),
//! but descendant-combinator matching needs to walk upward — this builds
//! that view once, up front, rather than threading parent references
//! through `Element` itself.

use florui::{Element, Handler, SelectionHandler, SubmitHandler, ValueHandler};
use florui_reactive::Binding;

pub type NodeId = usize;

/// The `role` attribute values this crate understands: `switch`, only
/// meaningful on `<input type="checkbox">`, and the overlay roles, valid on
/// any element. Any other value is reported by [`Arena::unsupported_role`]
/// rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibleRole {
    Switch,
    Dialog,
    AlertDialog,
    Menu,
    MenuBar,
    MenuItem,
    Tooltip,
    List,
    ListItem,
}

impl AccessibleRole {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "switch" => Some(Self::Switch),
            "dialog" => Some(Self::Dialog),
            "alertdialog" => Some(Self::AlertDialog),
            "menu" => Some(Self::Menu),
            "menubar" => Some(Self::MenuBar),
            "menuitem" => Some(Self::MenuItem),
            "tooltip" => Some(Self::Tooltip),
            "list" => Some(Self::List),
            "listitem" => Some(Self::ListItem),
            _ => None,
        }
    }
}

/// One direct child of a node, in original source order, for inline
/// layout: consecutive [`Element::Text`] siblings merge into one
/// [`InlineItem::Text`], the same way a real inline formatting context
/// treats adjacent text as one run rather than artificially splitting it
/// at whatever points the author happened to write separate string
/// literals. See [`Arena::inline_items`].
#[derive(Debug, Clone, PartialEq)]
pub enum InlineItem {
    Text(String),
    Element(NodeId),
}

struct ArenaNode {
    tag: &'static str,
    /// Where the element was written, if `view!` recorded it. See
    /// [`Arena::source`].
    source: florui::Provenance,
    /// Every attribute exactly as authored, kept alongside the specific
    /// typed fields below (`disabled`, `checked`, ...) — those exist so a
    /// consumer doesn't have to re-parse a known attribute by name every
    /// time, but real CSS attribute selectors (`[attr]`, `[attr="v"]`)
    /// need the raw list itself. See [`Arena::attrs`].
    attrs: Vec<(String, String)>,
    classes: Vec<String>,
    id: Option<String>,
    /// Whether the `disabled` attribute is present and `"true"` — markup
    /// state, computed once here exactly like `id`/`classes`, not through
    /// [`crate::InteractionState`] (see `stylo.rs`'s own `StyloTree::new`
    /// for why).
    disabled: bool,
    /// Whether an enclosing `<fieldset disabled>` disables this node —
    /// its first `<legend>`'s descendants are exempt from that fieldset
    /// only. Folded into [`Arena::is_disabled`].
    in_disabled_fieldset: bool,
    /// Whether the `checked` attribute is present and `"true"` — markup
    /// state for `<input type="checkbox">`/`type="radio"`, read the same
    /// way `disabled` is. See [`Arena::is_checked`].
    checked: bool,
    /// Whether the `indeterminate` attribute is present and `"true"` —
    /// markup state for `<input type="checkbox">`, real HTML's own
    /// IDL-only "mixed" display state (no matching content attribute
    /// there; this crate has no scripting layer to set an IDL property
    /// from, so it is a plain attribute here, read the same way `checked`
    /// is). See [`Arena::is_indeterminate`].
    indeterminate: bool,
    /// `min`/`max`/`step` on `<input type="range">`, and `value` parsed as
    /// a number instead of the raw string [`Self::value`] already keeps —
    /// real HTML's own initial values (`0`/`100`/`1`) when absent or not a
    /// valid number. See [`Arena::range_min`]/[`Arena::range_max`]/
    /// [`Arena::range_step`]/[`Arena::range_value`].
    range_min: f32,
    range_max: f32,
    range_step: f32,
    range_value: f32,
    /// Whether the `selected` attribute is present and `"true"` — markup
    /// state for `<option>`, read the same way `checked` is (real HTML
    /// uses a different attribute name for a `<select>`'s own child than
    /// for a checkbox/radio, so this is its own field rather than reusing
    /// `checked`). See [`Arena::is_selected`].
    selected: bool,
    /// Whether the `active` attribute is present and `"true"` — an
    /// `<option>`'s own keyboard-highlight state while its select is
    /// open, distinct from `selected` (the committed value): real HTML
    /// lets arrow keys preview an option without committing it until
    /// Enter/click. See [`Arena::is_active`].
    active: bool,
    /// Whether the `open` attribute is present and `"true"` — markup
    /// state for `<select>`: whether its option list is currently showing
    /// as an overlay. See [`Arena::is_open`].
    open: bool,
    /// The `label` attribute on `<optgroup>` — real HTML's own group
    /// caption, an attribute rather than child text content (an
    /// `<optgroup>`'s only real children are `<option>`s). See
    /// [`Arena::group_label`].
    group_label: Option<String>,
    /// Whether the `multiple` attribute is present and `"true"` — a
    /// `<select multiple>` renders as an always-visible listbox instead
    /// of a collapsible dropdown; `open` has no meaning for it. See
    /// [`Arena::is_multiple`].
    multiple: bool,
    /// `onselectionchange` on `<select multiple>` — see
    /// [`Arena::selection_handler`].
    selection_handlers: Vec<(String, SelectionHandler)>,
    /// `onsubmit` on `<form>` -- see [`Arena::submit_handler`].
    submit_handlers: Vec<(String, SubmitHandler)>,
    /// The raw, unparsed `style="..."` attribute text, if declared — see
    /// [`Arena::style_attr`].
    style: Option<String>,
    /// The current `value` attribute's plain string form — kept
    /// unconditionally, the same as every other primitive attribute,
    /// regardless of whether it came from a literal or a `Binding` (see
    /// [`Arena::value_binding`] for the write-back half). See
    /// [`Arena::value_attr`].
    value: Option<String>,
    /// The `type` attribute on `<input>` (`"text"`/`"password"`/
    /// `"checkbox"`/`"radio"` — the only values `view!` accepts at all).
    /// See [`Arena::input_type`].
    input_type: Option<String>,
    /// The `name` attribute on `<input type="radio">` — the group key.
    /// See [`Arena::name`].
    name: Option<String>,
    /// The `for` attribute on `<label>` — the id of the control it
    /// captions. See [`Arena::label_for`].
    label_for: Option<String>,
    /// The `accessible_label` attribute — an explicit accessible name for
    /// an element whose visible content isn't a useful spoken name (an
    /// icon-only button's glyph text, say). Underscored rather than
    /// hyphenated like HTML's `aria-label`: `view!` attribute names are
    /// plain `syn::Ident`s, which can't contain `-`. See
    /// [`Arena::accessible_label`].
    accessible_label: Option<String>,
    /// The raw `role` attribute. See [`Arena::role`].
    role: Option<String>,
    /// The `set_size` and `position_in_set` attributes: how many items the
    /// collection this item belongs to holds and its 1-based place in it,
    /// for a virtualized list that mounts only some of them. See
    /// [`Arena::collection_position`].
    set_size: Option<usize>,
    position_in_set: Option<usize>,
    /// The `href` attribute on `<a>`. See [`Arena::href`].
    href: Option<String>,
    /// `<img>`'s own `src` — an asset source, not resolved or read here;
    /// `crate::Arena` has no filesystem/asset dependency at all. See
    /// [`Arena::src_attr`].
    src: Option<String>,
    /// `<img>`'s own `alt` — its accessible-name override, the same role
    /// [`Self::accessible_label`] plays for other tags. Kept as its own
    /// field (matching `<img>`'s own real HTML attribute name) rather
    /// than folded into `accessible_label`, so an author never has to
    /// declare a replaced element's alt text twice under two different
    /// names. See [`Arena::alt_attr`].
    alt: Option<String>,
    /// The node's own direct text, for text measurement — not inherited
    /// from or propagated to any other node.
    text: String,
    /// Same direct children as `text`/`children` below, but preserving the
    /// interleaving between them that both those flattened views lose —
    /// see [`Arena::inline_items`].
    inline_items: Vec<InlineItem>,
    handlers: Vec<(String, Handler)>,
    /// A typed write-back channel for a primitive attribute (today, only
    /// `value` on `<input>`) — see [`Arena::value_binding`]. Mutually
    /// exclusive with `value_handlers` for the same attribute.
    bindings: Vec<(String, Binding<String>)>,
    /// The explicit (non-`Binding`) controlled-value channel — see
    /// [`Arena::value_handler`].
    value_handlers: Vec<(String, ValueHandler)>,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// A tree may have more than one root: `view!` can produce an
/// [`Element::Fragment`] at the top level, and a `Text`-only tree has
/// none at all (there is nothing selectable to style).
pub struct Arena {
    nodes: Vec<ArenaNode>,
    roots: Vec<NodeId>,
    /// How many of `roots`' leading entries are document roots — the
    /// rest (extracted from any [`Element::Portal`] found while building,
    /// if any) are portal overlay roots. `roots()` itself stays one flat,
    /// ordered list on purpose: paint (later root paints on top,
    /// unclipped) and hit-test (later root wins) both already treat a
    /// later root as "above" an earlier one with no changes needed —
    /// only layout needs to tell the two groups apart, via
    /// [`Self::document_roots`]/[`Self::overlay_roots`].
    document_root_count: usize,
}

/// One still-unprocessed slice of sibling [`Element`]s, and where their
/// [`NodeId`]s attach — a real parent, or the root list.
struct Frame<'a> {
    elements: &'a [Element],
    index: usize,
    parent: Option<NodeId>,
}

impl Arena {
    /// Walks `root`, discovering any [`Element::Portal`] structurally
    /// (no side-channel registry) and extracting each one into its own
    /// overlay root — level by level, so a portal nested inside another
    /// portal's own content always lands in a *later* pass than its
    /// ancestor, and so always later in [`Self::roots`] ("later root
    /// wins/paints on top" — see [`Self::document_root_count`]'s own
    /// doc). `document_root_count` is captured after the very first
    /// pass, before any portal becomes a real root, so every document
    /// root precedes every portal root regardless of nesting depth.
    pub fn build(root: &Element) -> Self {
        let mut arena = Arena {
            nodes: Vec::new(),
            roots: Vec::new(),
            document_root_count: 0,
        };
        let mut next_level = Vec::new();
        arena.push_all(std::slice::from_ref(root), &mut next_level);
        arena.document_root_count = arena.roots.len();
        arena.drain_levels(next_level);
        arena
    }

    /// Like [`Self::build`], but `overlays` becomes a second group of
    /// roots — [`Self::overlay_roots`] — appended after `document`'s
    /// own, for a caller that already has two independently-built trees
    /// rather than a single one with `Portal`s inside it. `overlays`
    /// itself may still contain `Portal`s of its own.
    pub fn build_with_overlays(document: &Element, overlays: &Element) -> Self {
        let mut arena = Arena {
            nodes: Vec::new(),
            roots: Vec::new(),
            document_root_count: 0,
        };
        let mut next_level = Vec::new();
        arena.push_all(std::slice::from_ref(document), &mut next_level);
        arena.document_root_count = arena.roots.len();
        next_level.push(std::slice::from_ref(overlays));
        arena.drain_levels(next_level);
        arena
    }

    /// Runs one more [`Self::push_all`] pass per entry already queued,
    /// then repeats for whatever `Portal`s that pass itself discovers,
    /// until a pass finds none — see [`Self::build`]'s own doc for why
    /// this level-by-level order is what makes nesting paint correctly.
    fn drain_levels(&mut self, mut next_level: Vec<&[Element]>) {
        while !next_level.is_empty() {
            let this_level = std::mem::take(&mut next_level);
            for children in this_level {
                self.push_all(children, &mut next_level);
            }
        }
    }

    /// Whether a child with tag `child_tag` under `parent` is disabled by
    /// `parent`'s own `<fieldset disabled>`, or by one already disabling
    /// `parent` itself.
    fn disabled_by_fieldset(&self, parent: NodeId, child_tag: &str) -> bool {
        let p = &self.nodes[parent];
        if p.tag != "fieldset" || !p.disabled {
            return p.in_disabled_fieldset;
        }
        let first_legend = child_tag == "legend"
            && p.children
                .iter()
                .all(|&sibling| self.nodes[sibling].tag != "legend");
        p.in_disabled_fieldset || !first_legend
    }

    /// Iterative pre-order walk: an explicit stack instead of one call
    /// frame per tree level, so a deep tree can't overflow the stack.
    /// A [`Element::Portal`] contributes nothing at its own position
    /// (like an empty [`Element::Fragment`]) and instead queues its own
    /// content into `next_level` for [`Self::drain_levels`] to expand
    /// into a real root afterward.
    fn push_all<'a>(&mut self, root_elements: &'a [Element], next_level: &mut Vec<&'a [Element]>) {
        let mut stack = vec![Frame {
            elements: root_elements,
            index: 0,
            parent: None,
        }];

        while let Some(frame) = stack.last_mut() {
            let Some(element) = frame.elements.get(frame.index) else {
                stack.pop();
                continue;
            };
            frame.index += 1;
            let parent = frame.parent;

            match element {
                Element::Node(node) => {
                    let id = self.nodes.len();
                    // Real HTML's own default `value` when absent is the
                    // midpoint of `min`/`max`, so it needs those already
                    // parsed rather than a fixed literal default.
                    let range_min = attr_f32(&node.attrs, "min", 0.0);
                    let range_max = attr_f32(&node.attrs, "max", 100.0);
                    self.nodes.push(ArenaNode {
                        tag: node.tag,
                        source: node.source,
                        attrs: node.attrs.clone(),
                        classes: class_list(&node.attrs),
                        id: attr_value(&node.attrs, "id"),
                        disabled: attr_bool(&node.attrs, "disabled"),
                        in_disabled_fieldset: parent
                            .is_some_and(|p| self.disabled_by_fieldset(p, node.tag)),
                        checked: attr_bool(&node.attrs, "checked"),
                        indeterminate: attr_bool(&node.attrs, "indeterminate"),
                        range_min,
                        range_max,
                        range_step: attr_f32(&node.attrs, "step", 1.0),
                        range_value: attr_f32(&node.attrs, "value", (range_min + range_max) / 2.0),
                        selected: attr_bool(&node.attrs, "selected"),
                        active: attr_bool(&node.attrs, "active"),
                        open: attr_bool(&node.attrs, "open"),
                        group_label: attr_value(&node.attrs, "label"),
                        multiple: attr_bool(&node.attrs, "multiple"),
                        selection_handlers: node.selection_handlers.clone(),
                        submit_handlers: node.submit_handlers.clone(),
                        style: attr_value(&node.attrs, "style"),
                        value: attr_value(&node.attrs, "value"),
                        input_type: attr_value(&node.attrs, "type"),
                        name: attr_value(&node.attrs, "name"),
                        label_for: attr_value(&node.attrs, "for"),
                        accessible_label: attr_value(&node.attrs, "accessible_label"),
                        role: attr_value(&node.attrs, "role"),
                        set_size: attr_value(&node.attrs, "set_size").and_then(|v| v.parse().ok()),
                        position_in_set: attr_value(&node.attrs, "position_in_set")
                            .and_then(|v| v.parse().ok()),
                        href: attr_value(&node.attrs, "href"),
                        src: attr_value(&node.attrs, "src"),
                        alt: attr_value(&node.attrs, "alt"),
                        text: collect_text(&node.children),
                        inline_items: Vec::new(),
                        handlers: node.handlers.clone(),
                        bindings: node.bindings.clone(),
                        value_handlers: node.value_handlers.clone(),
                        parent,
                        children: Vec::new(),
                    });
                    match parent {
                        Some(p) => {
                            self.nodes[p].children.push(id);
                            self.nodes[p].inline_items.push(InlineItem::Element(id));
                        }
                        None => self.roots.push(id),
                    }
                    stack.push(Frame {
                        elements: &node.children,
                        index: 0,
                        parent: Some(id),
                    });
                }
                Element::Fragment(nested) => {
                    stack.push(Frame {
                        elements: nested,
                        index: 0,
                        parent,
                    });
                }
                Element::Portal(children) => {
                    next_level.push(children.as_slice());
                }
                Element::Text(value) => {
                    if let Some(p) = parent {
                        match self.nodes[p].inline_items.last_mut() {
                            Some(InlineItem::Text(existing)) => existing.push_str(value),
                            _ => self.nodes[p]
                                .inline_items
                                .push(InlineItem::Text(value.clone())),
                        }
                    }
                }
            }
        }
    }

    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    /// The ordinary document roots — everything [`Self::build`] always
    /// produces, and the leading part of [`Self::build_with_overlays`]'s
    /// own result.
    pub fn document_roots(&self) -> &[NodeId] {
        &self.roots[..self.document_root_count]
    }

    /// Portal overlay roots, if any — empty for anything built via
    /// [`Self::build`]. See [`Self::build_with_overlays`].
    pub fn overlay_roots(&self) -> &[NodeId] {
        &self.roots[self.document_root_count..]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id].parent
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id].children
    }

    pub fn tag(&self, id: NodeId) -> &'static str {
        self.nodes[id].tag
    }

    /// Where `id`'s element was written, when `view!` recorded it (a debug
    /// build, or the `source-locations` feature of `florui`).
    pub fn source(&self, id: NodeId) -> Option<florui::SourceLocation> {
        self.nodes[id].source.location()
    }

    pub fn classes(&self, id: NodeId) -> &[String] {
        &self.nodes[id].classes
    }

    /// Every attribute this node declared, exactly as authored — for a
    /// real CSS attribute selector (`[attr]`, `[attr="v"]`); see
    /// [`ArenaNode::attrs`].
    pub fn attrs(&self, id: NodeId) -> &[(String, String)] {
        &self.nodes[id].attrs
    }

    pub fn id_attr(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].id.as_deref()
    }

    /// Whether this node's `disabled` attribute is present and `"true"` —
    /// e.g. `<button disabled={is_disabled}>` where `is_disabled: bool`
    /// lowers via `ToString` (see `view!`'s own attribute lowering).
    /// `false` for any other value or an absent attribute — bare
    /// `<button disabled>` isn't parseable `view!` syntax, so only an
    /// explicit `"true"`/`"false"` is ever seen in practice.
    pub fn is_disabled(&self, id: NodeId) -> bool {
        self.nodes[id].disabled || self.nodes[id].in_disabled_fieldset
    }

    /// The raw value of attribute `name` exactly as authored, for
    /// attributes with no typed accessor of their own.
    pub fn attr(&self, id: NodeId, name: &str) -> Option<&str> {
        self.nodes[id]
            .attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// Whether attribute `name` is present and `"true"` — the same
    /// contract as [`Self::is_disabled`]'s own attribute.
    pub fn attr_flag(&self, id: NodeId, name: &str) -> bool {
        self.attr(id, name) == Some("true")
    }

    /// Whether this node's `checked` attribute is present and `"true"` —
    /// same lowering/parsing contract as [`Self::is_disabled`], for
    /// `<input type="checkbox">`/`type="radio"`.
    pub fn is_checked(&self, id: NodeId) -> bool {
        self.nodes[id].checked
    }

    /// This node's `indeterminate` attribute — see [`ArenaNode::indeterminate`].
    pub fn is_indeterminate(&self, id: NodeId) -> bool {
        self.nodes[id].indeterminate
    }

    /// `<input type="range">`'s own `min` — real HTML's own initial `0`
    /// when absent or not a number.
    pub fn range_min(&self, id: NodeId) -> f32 {
        self.nodes[id].range_min
    }

    /// `<input type="range">`'s own `max` — real HTML's own initial `100`
    /// when absent or not a number. Not clamped against `min` here; a
    /// caller that needs a well-ordered pair should treat `max < min` the
    /// way it treats any other malformed input.
    pub fn range_max(&self, id: NodeId) -> f32 {
        self.nodes[id].range_max
    }

    /// `<input type="range">`'s own `step` — real HTML's own initial `1`
    /// when absent or not a number.
    pub fn range_step(&self, id: NodeId) -> f32 {
        self.nodes[id].range_step
    }

    /// `<input type="range">`'s own `value`, parsed as a number and
    /// clamped into `[min, max]` (real HTML's own behavior for an
    /// out-of-range value) — `[`Self::value_attr`] keeps the same
    /// attribute's raw string form. Defaults to the midpoint of `min`/
    /// `max` when absent or not a number, real HTML's own initial value.
    pub fn range_value(&self, id: NodeId) -> f32 {
        let node = &self.nodes[id];
        let (min, max) = (node.range_min, node.range_max);
        if min <= max {
            node.range_value.clamp(min, max)
        } else {
            node.range_value
        }
    }

    /// Whether this node's `selected` attribute is present and `"true"` —
    /// for `<option>`. See [`ArenaNode::selected`].
    pub fn is_selected(&self, id: NodeId) -> bool {
        self.nodes[id].selected
    }

    /// Whether this node's `active` attribute is present and `"true"` —
    /// for `<option>`. See [`ArenaNode::active`].
    pub fn is_active(&self, id: NodeId) -> bool {
        self.nodes[id].active
    }

    /// Whether this node's `open` attribute is present and `"true"` — for
    /// `<select>`. See [`ArenaNode::open`].
    pub fn is_open(&self, id: NodeId) -> bool {
        self.nodes[id].open
    }

    /// This node's `label` attribute — for `<optgroup>`. See
    /// [`ArenaNode::group_label`].
    pub fn group_label(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].group_label.as_deref()
    }

    /// Whether this node's `multiple` attribute is present and `"true"`
    /// — for `<select>`. See [`ArenaNode::multiple`].
    pub fn is_multiple(&self, id: NodeId) -> bool {
        self.nodes[id].multiple
    }

    /// The [`SubmitHandler`] `<form onsubmit>` declared, if any.
    pub fn submit_handler(&self, id: NodeId) -> Option<&SubmitHandler> {
        self.nodes[id].submit_handlers.first().map(|(_, h)| h)
    }

    /// The [`SelectionHandler`] this node declared for `attr` (today,
    /// only `"selection"` for `onselectionchange`), if any.
    pub fn selection_handler(&self, id: NodeId, attr: &str) -> Option<&SelectionHandler> {
        self.nodes[id]
            .selection_handlers
            .iter()
            .find(|(name, _)| name == attr)
            .map(|(_, handler)| handler)
    }

    /// This node's raw, unparsed `style="..."` attribute text, if it
    /// declared one — a real, cascade-honoring inline declaration (highest
    /// specificity, same as real CSS), not an inert string; see
    /// [`crate::stylo`]'s `style_attribute` for where it's actually
    /// parsed and merged in.
    pub fn style_attr(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].style.as_deref()
    }

    /// This node's `value` attribute, in its current plain string form —
    /// the displayed text for a text-editing control, regardless of
    /// whether it was written as a literal or a `Binding` (the string
    /// snapshot always exists either way; see [`Self::value_binding`]).
    pub fn value_attr(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].value.as_deref()
    }

    /// This node's `type` attribute — meaningful only on `<input>`,
    /// `None` for any tag that never declared one.
    pub fn input_type(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].input_type.as_deref()
    }

    /// This node's `name` attribute — meaningful only on `<input
    /// type="radio">`, where radios sharing one non-empty name form a
    /// group. `None` for any node that never declared one.
    pub fn name(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].name.as_deref()
    }

    /// This node's `for` attribute — meaningful only on `<label>`: the
    /// `id` of the control it captions, resolved by whoever consumes this
    /// (e.g. the accessibility bridge's `labelled_by` association), not
    /// by `Arena` itself.
    pub fn label_for(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].label_for.as_deref()
    }

    /// This node's `href` attribute — meaningful only on `<a>`, where its
    /// presence (not its content) also decides focusability, see
    /// `florui-platform`'s own `focus::is_focusable`.
    pub fn href(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].href.as_deref()
    }

    /// This node's `accessible_label` attribute, if declared — an
    /// accessibility consumer (e.g. `accessibility::tree`'s own button
    /// arm) should prefer this over the node's visible text content when
    /// present.
    pub fn accessible_label(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].accessible_label.as_deref()
    }

    /// `<img>`'s own `src` — meaningless on any other tag. Not resolved
    /// or read from disk here; see `florui-platform`'s image loading.
    pub fn src_attr(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].src.as_deref()
    }

    /// `<img>`'s own `alt` — its accessible-name override. See
    /// [`Self::accessible_label`] for the equivalent on every other tag.
    pub fn alt_attr(&self, id: NodeId) -> Option<&str> {
        self.nodes[id].alt.as_deref()
    }

    /// This node's `role` attribute, if it names a role this crate
    /// supports (see [`AccessibleRole`]).
    pub fn role(&self, id: NodeId) -> Option<AccessibleRole> {
        self.nodes[id]
            .role
            .as_deref()
            .and_then(AccessibleRole::parse)
    }

    /// `(position, size)` of this item within its collection, both from its
    /// `position_in_set` (1-based) and `set_size` attributes; `None` unless
    /// both are present and the position fits the size.
    pub fn collection_position(&self, id: NodeId) -> Option<(usize, usize)> {
        let node = &self.nodes[id];
        let (position, size) = (node.position_in_set?, node.set_size?);
        (position >= 1 && position <= size).then_some((position, size))
    }

    /// This node's `role` attribute when it names a role this crate does
    /// not support -- the diagnostic for a role that would otherwise be
    /// silently ignored. Nothing is printed: this crate has no logging.
    pub fn unsupported_role(&self, id: NodeId) -> Option<&str> {
        self.nodes[id]
            .role
            .as_deref()
            .filter(|value| AccessibleRole::parse(value).is_none())
    }

    /// This node's own direct text, for measurement: its direct
    /// [`Element::Text`] children concatenated in order, flattening
    /// through any [`Element::Fragment`] child but not descending into a
    /// child [`Element::Node`] — that child's text belongs to its own
    /// `NodeId`, not its parent's.
    pub fn text_content(&self, id: NodeId) -> &str {
        &self.nodes[id].text
    }

    /// This node's direct children — text runs and element children — in
    /// their original interleaved source order, with an
    /// [`Element::Fragment`] child's own children spliced in at its
    /// position (the same flattening [`Self::text_content`]/
    /// [`Self::children`] already apply, just without losing which parts
    /// are text and which are elements, or their relative order). Unlike
    /// [`Self::text_content`], this is what real mixed inline content
    /// (`"Hello "<b>world</b>"!"`) needs to lay out correctly — see
    /// `florui_layout`'s own inline formatting context, which is the only
    /// consumer so far; nothing else needs to change what it reads.
    pub fn inline_items(&self, id: NodeId) -> &[InlineItem] {
        &self.nodes[id].inline_items
    }

    /// The handler this node declared for `event` (e.g. `"click"` for an
    /// `onclick={...}` attribute), if any.
    pub fn handler(&self, id: NodeId, event: &str) -> Option<&Handler> {
        self.nodes[id]
            .handlers
            .iter()
            .find(|(name, _)| name == event)
            .map(|(_, handler)| handler)
    }

    /// The typed write-back channel this node declared for `attr` (e.g.
    /// `"value"` for a `value={binding}` attribute on `<input>`), if any
    /// — `None` both when the node declared no such attribute at all and
    /// when it declared a plain string literal instead (nothing to write
    /// back to either way).
    pub fn value_binding(&self, id: NodeId, attr: &str) -> Option<&Binding<String>> {
        self.nodes[id]
            .bindings
            .iter()
            .find(|(name, _)| name == attr)
            .map(|(_, binding)| binding)
    }

    /// The explicit (non-`Binding`) write-back channel this node declared
    /// for `attr`, if any — mutually exclusive with [`Self::value_binding`]
    /// for the same `attr`; `view!`'s own codegen never emits both.
    pub fn value_handler(&self, id: NodeId, attr: &str) -> Option<&ValueHandler> {
        self.nodes[id]
            .value_handlers
            .iter()
            .find(|(name, _)| name == attr)
            .map(|(_, handler)| handler)
    }

    /// Depth-first pre-order search across every root, for tests and
    /// callers that need to locate a node before marking it in an
    /// [`crate::InteractionState`].
    pub fn find(&self, mut predicate: impl FnMut(&Self, NodeId) -> bool) -> Option<NodeId> {
        let mut stack: Vec<NodeId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            if predicate(self, id) {
                return Some(id);
            }
            stack.extend(self.children(id).iter().rev());
        }
        None
    }

    /// Like [`Self::find`], but collects every match in document order
    /// instead of stopping at the first — the shared primitive both tab
    /// order and [`crate::focus::FocusPath::resolve`] need.
    pub fn find_all(&self, mut predicate: impl FnMut(&Self, NodeId) -> bool) -> Vec<NodeId> {
        let mut matches = Vec::new();
        let mut stack: Vec<NodeId> = self.roots.iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            if predicate(self, id) {
                matches.push(id);
            }
            stack.extend(self.children(id).iter().rev());
        }
        matches
    }
}

fn class_list(attrs: &[(String, String)]) -> Vec<String> {
    attr_value(attrs, "class")
        .map(|value| value.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default()
}

fn attr_value(attrs: &[(String, String)], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn attr_bool(attrs: &[(String, String)], name: &str) -> bool {
    attr_value(attrs, name).as_deref() == Some("true")
}

/// `name`'s attribute value parsed as `f32`, or `default` if absent or not
/// a valid float — same lenient-fallback shape `attr_bool` gives a missing
/// or non-`"true"` boolean attribute.
fn attr_f32(attrs: &[(String, String)], name: &str, default: f32) -> f32 {
    attr_value(attrs, name)
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn collect_text(children: &[Element]) -> String {
    let mut text = String::new();
    for child in children {
        append_text(child, &mut text);
    }
    text
}

fn append_text(element: &Element, out: &mut String) {
    match element {
        Element::Text(value) => out.push_str(value),
        Element::Fragment(children) => {
            for child in children {
                append_text(child, out);
            }
        }
        Element::Node(_) | Element::Portal(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    #[test]
    fn builds_parent_links_across_the_tree() {
        let tree: Element = view! {
            <div class="card">
                <h2>{"Title"}</h2>
                <button class="primary">{"Go"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        assert_eq!(arena.roots().len(), 1);
        let card = arena.roots()[0];
        assert_eq!(arena.tag(card), "div");
        assert_eq!(arena.classes(card), &["card".to_string()]);
        assert_eq!(arena.children(card).len(), 2);

        let button = arena.children(card)[1];
        assert_eq!(arena.tag(button), "button");
        assert_eq!(arena.parent(button), Some(card));
    }

    #[test]
    fn a_fragment_root_yields_multiple_roots_with_no_shared_parent() {
        let tree: Element = view! {
            <div style="background-color: #111;" />
            <div style="background-color: #222;" />
        };
        let arena = Arena::build(&tree);
        assert_eq!(arena.roots().len(), 2);
        assert_eq!(arena.parent(arena.roots()[0]), None);
    }

    #[test]
    fn build_alone_reports_every_root_as_a_document_root() {
        let tree: Element = view! {
            <div />
            <div />
        };
        let arena = Arena::build(&tree);
        assert_eq!(arena.document_roots().len(), 2);
        assert!(arena.overlay_roots().is_empty());
    }

    #[test]
    fn build_with_overlays_partitions_document_and_overlay_roots_in_order() {
        let document: Element = view! {
            <div class="doc-a" />
            <div class="doc-b" />
        };
        let overlays: Element = view! { <div class="overlay-a" /> };
        let arena = Arena::build_with_overlays(&document, &overlays);

        assert_eq!(
            arena.roots().len(),
            3,
            "roots() stays one flat, ordered list"
        );
        assert_eq!(arena.document_roots().len(), 2);
        assert_eq!(arena.overlay_roots().len(), 1);
        assert_eq!(
            arena.classes(arena.overlay_roots()[0]),
            &["overlay-a".to_string()]
        );
        assert_eq!(
            arena.roots()[2],
            arena.overlay_roots()[0],
            "overlay roots come after every document root in roots()"
        );
    }

    #[test]
    fn a_portal_nested_three_levels_deep_lands_in_ancestor_before_descendant_order() {
        let c = Element::node("div", vec![("id".to_string(), "c".to_string())], vec![]);
        let b = Element::node(
            "div",
            vec![("id".to_string(), "b".to_string())],
            vec![Element::Portal(vec![c])],
        );
        let a = Element::node(
            "div",
            vec![("id".to_string(), "a".to_string())],
            vec![Element::Portal(vec![b])],
        );
        let document = Element::node("div", vec![], vec![Element::Portal(vec![a])]);

        let arena = Arena::build(&document);

        assert_eq!(arena.document_roots().len(), 1);
        let overlays = arena.overlay_roots();
        assert_eq!(overlays.len(), 3);
        assert_eq!(arena.id_attr(overlays[0]), Some("a"));
        assert_eq!(arena.id_attr(overlays[1]), Some("b"));
        assert_eq!(
            arena.id_attr(overlays[2]),
            Some("c"),
            "a portal nested inside another must land after its ancestor, however deep"
        );
    }

    #[test]
    fn two_unrelated_sibling_portals_keep_their_own_document_order() {
        let a = Element::node("div", vec![("id".to_string(), "a".to_string())], vec![]);
        let b = Element::node("div", vec![("id".to_string(), "b".to_string())], vec![]);
        let document = Element::node(
            "div",
            vec![],
            vec![Element::Portal(vec![a]), Element::Portal(vec![b])],
        );

        let arena = Arena::build(&document);

        let overlays = arena.overlay_roots();
        assert_eq!(overlays.len(), 2);
        assert_eq!(arena.id_attr(overlays[0]), Some("a"));
        assert_eq!(arena.id_attr(overlays[1]), Some("b"));
    }

    #[test]
    fn text_nodes_are_not_addressable() {
        let tree: Element = view! { <p>{"hello"}</p> };
        let arena = Arena::build(&tree);
        let p = arena.roots()[0];
        assert!(arena.children(p).is_empty(), "text is not a styleable node");
    }

    #[test]
    fn id_attribute_is_read() {
        let tree: Element = view! { <div id="main" /> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.id_attr(arena.roots()[0]), Some("main"));
    }

    #[test]
    fn disabled_attribute_true_is_read() {
        let tree: Element = view! { <button disabled="true" /> };
        let arena = Arena::build(&tree);
        assert!(arena.is_disabled(arena.roots()[0]));
    }

    #[test]
    fn disabled_attribute_false_is_read_as_not_disabled() {
        let tree: Element = view! { <button disabled="false" /> };
        let arena = Arena::build(&tree);
        assert!(!arena.is_disabled(arena.roots()[0]));
    }

    #[test]
    fn disabled_attribute_absent_defaults_to_not_disabled() {
        let tree: Element = view! { <button /> };
        let arena = Arena::build(&tree);
        assert!(!arena.is_disabled(arena.roots()[0]));
    }

    #[test]
    fn checked_attribute_true_is_read() {
        let tree: Element = view! { <input type="checkbox" checked="true" /> };
        let arena = Arena::build(&tree);
        assert!(arena.is_checked(arena.roots()[0]));
    }

    #[test]
    fn range_min_max_step_default_to_reals_htmls_own_initial_values() {
        let tree: Element = view! { <input type="range" /> };
        let arena = Arena::build(&tree);
        let input = arena.roots()[0];
        assert_eq!(arena.range_min(input), 0.0);
        assert_eq!(arena.range_max(input), 100.0);
        assert_eq!(arena.range_step(input), 1.0);
        assert_eq!(arena.range_value(input), 50.0, "midpoint of min/max");
    }

    #[test]
    fn range_min_max_step_value_are_read_from_their_attributes() {
        let tree: Element = view! {
            <input type="range" min="10" max="20" step="5" value="17" />
        };
        let arena = Arena::build(&tree);
        let input = arena.roots()[0];
        assert_eq!(arena.range_min(input), 10.0);
        assert_eq!(arena.range_max(input), 20.0);
        assert_eq!(arena.range_step(input), 5.0);
        assert_eq!(
            arena.range_value(input),
            17.0,
            "range_value does not snap to step"
        );
    }

    #[test]
    fn range_value_clamps_into_min_max_but_value_attr_keeps_the_raw_string() {
        let tree: Element = view! { <input type="range" min="0" max="10" value="99" /> };
        let arena = Arena::build(&tree);
        let input = arena.roots()[0];
        assert_eq!(arena.range_value(input), 10.0);
        assert_eq!(
            arena.value_attr(input),
            Some("99"),
            "raw string form is untouched"
        );
    }

    #[test]
    fn a_non_numeric_value_falls_back_to_the_min_max_midpoint() {
        let tree: Element = view! { <input type="range" min="0" max="10" value="not-a-number" /> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.range_value(arena.roots()[0]), 5.0);
    }

    #[test]
    fn indeterminate_attribute_true_is_read_and_absent_defaults_to_false() {
        let tree: Element = view! { <input type="checkbox" indeterminate="true" /> };
        let arena = Arena::build(&tree);
        assert!(arena.is_indeterminate(arena.roots()[0]));

        let tree: Element = view! { <input type="checkbox" /> };
        let arena = Arena::build(&tree);
        assert!(!arena.is_indeterminate(arena.roots()[0]));
    }

    #[test]
    fn checked_attribute_absent_defaults_to_not_checked() {
        let tree: Element = view! { <input type="checkbox" /> };
        let arena = Arena::build(&tree);
        assert!(!arena.is_checked(arena.roots()[0]));
    }

    #[test]
    fn selected_attribute_true_is_read_and_absent_defaults_to_false() {
        let tree: Element = view! {
            <select>
                <option selected="true">{"A"}</option>
                <option>{"B"}</option>
            </select>
        };
        let arena = Arena::build(&tree);
        let options = arena.find_all(|a, id| a.tag(id) == "option");
        assert!(arena.is_selected(options[0]));
        assert!(!arena.is_selected(options[1]));
    }

    #[test]
    fn active_attribute_true_is_read_and_absent_defaults_to_false() {
        let tree: Element = view! {
            <select>
                <option active="true">{"A"}</option>
                <option>{"B"}</option>
            </select>
        };
        let arena = Arena::build(&tree);
        let options = arena.find_all(|a, id| a.tag(id) == "option");
        assert!(arena.is_active(options[0]));
        assert!(!arena.is_active(options[1]));
    }

    #[test]
    fn group_label_reads_the_plain_string_form_and_is_none_without_it() {
        let tree: Element = view! { <optgroup label="Fruits" /> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.group_label(arena.roots()[0]), Some("Fruits"));

        let tree: Element = view! { <optgroup /> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.group_label(arena.roots()[0]), None);
    }

    #[test]
    fn open_attribute_true_is_read_and_absent_defaults_to_false() {
        let tree: Element = view! { <select open="true" /> };
        let arena = Arena::build(&tree);
        assert!(arena.is_open(arena.roots()[0]));

        let tree: Element = view! { <select /> };
        let arena = Arena::build(&tree);
        assert!(!arena.is_open(arena.roots()[0]));
    }

    #[test]
    fn name_attribute_is_read() {
        let tree: Element = view! { <input type="radio" name="size" /> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.name(arena.roots()[0]), Some("size"));
        let bare: Element = view! { <input type="radio" /> };
        let arena = Arena::build(&bare);
        assert_eq!(arena.name(arena.roots()[0]), None);
    }

    #[test]
    fn find_locates_a_descendant_by_predicate() {
        let tree: Element = view! {
            <div>
                <button class="primary">{"Go"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let found = arena.find(|arena, id| arena.tag(id) == "button").unwrap();
        assert_eq!(arena.tag(found), "button");
    }

    #[test]
    fn find_all_collects_every_match_in_document_order() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <span>{"Not a match"}</span>
                <button>{"Second"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let buttons = arena.find_all(|arena, id| arena.tag(id) == "button");
        assert_eq!(buttons.len(), 2);
        assert_eq!(arena.text_content(buttons[0]), "First");
        assert_eq!(arena.text_content(buttons[1]), "Second");
    }

    #[test]
    fn text_content_concatenates_direct_text_children() {
        let tree: Element = view! { <h2>{"Hello"}</h2> };
        let arena = Arena::build(&tree);
        let h2 = arena.roots()[0];
        assert_eq!(arena.text_content(h2), "Hello");
    }

    #[test]
    fn text_content_is_empty_for_a_node_with_no_text_children() {
        let tree: Element = view! {
            <div>
                <span>{"x"}</span>
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        assert_eq!(
            arena.text_content(div),
            "",
            "the text belongs to the span, not its ancestor"
        );
    }

    #[test]
    fn handler_finds_the_declared_event_by_name() {
        let tree: Element = view! { <button onclick={|| ()} /> };
        let arena = Arena::build(&tree);
        let button = arena.roots()[0];
        assert!(arena.handler(button, "click").is_some());
        assert!(arena.handler(button, "mouseenter").is_none());
    }

    #[test]
    fn value_binding_finds_the_declared_attributes_typed_binding() {
        let binding = Binding::new("Ada".to_string(), |_| {});
        let tree: Element = view! { <input type="text" value={binding} /> };
        let arena = Arena::build(&tree);
        let input = arena.roots()[0];
        assert!(arena.value_binding(input, "value").is_some());
        assert!(arena.value_binding(input, "type").is_none());
    }

    #[test]
    fn value_handler_finds_the_explicit_contracts_callback() {
        let tree: Element = view! {
            <input type="text" value={"Ada".to_string()} oninput={|_: String| ()} />
        };
        let arena = Arena::build(&tree);
        let input = arena.roots()[0];
        assert!(arena.value_handler(input, "value").is_some());
        assert!(
            arena.value_binding(input, "value").is_none(),
            "the explicit contract must not also carry a Binding"
        );
    }

    #[test]
    fn value_binding_is_none_for_a_string_literal_value() {
        let tree: Element = view! { <input type="text" value="static" /> };
        let arena = Arena::build(&tree);
        let input = arena.roots()[0];
        assert!(arena.value_binding(input, "value").is_none());
    }

    #[test]
    fn value_attr_and_input_type_read_the_plain_string_form_either_way() {
        let binding = Binding::new("Ada".to_string(), |_| {});
        let bound: Element = view! { <input type="password" value={binding} /> };
        let arena = Arena::build(&bound);
        let input = arena.roots()[0];
        assert_eq!(arena.value_attr(input), Some("Ada"));
        assert_eq!(arena.input_type(input), Some("password"));

        let literal: Element = view! { <input type="checkbox" value="on" /> };
        let arena = Arena::build(&literal);
        let input = arena.roots()[0];
        assert_eq!(arena.value_attr(input), Some("on"));
        assert_eq!(arena.input_type(input), Some("checkbox"));
    }

    #[test]
    fn label_for_reads_the_plain_string_form() {
        let tree: Element = view! { <label for="name-input">{"Name"}</label> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.label_for(arena.roots()[0]), Some("name-input"));
    }

    #[test]
    fn label_for_is_none_without_the_attribute() {
        let tree: Element = view! { <label>{"Name"}</label> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.label_for(arena.roots()[0]), None);
    }

    #[test]
    fn accessible_label_reads_the_plain_string_form() {
        let tree: Element = view! { <button accessible_label="Close">{"x"}</button> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.accessible_label(arena.roots()[0]), Some("Close"));
    }

    #[test]
    fn role_switch_is_parsed_and_an_unknown_role_is_reported() {
        let tree: Element = view! {
            <div>
                <input type="checkbox" role="switch" />
                <input type="checkbox" role="slider" />
                <input type="checkbox" />
            </div>
        };
        let arena = Arena::build(&tree);
        let inputs = arena.find_all(|a, id| a.tag(id) == "input");
        assert_eq!(arena.role(inputs[0]), Some(AccessibleRole::Switch));
        assert_eq!(arena.unsupported_role(inputs[0]), None);
        assert_eq!(arena.role(inputs[1]), None);
        assert_eq!(arena.unsupported_role(inputs[1]), Some("slider"));
        assert_eq!(arena.role(inputs[2]), None);
        assert_eq!(arena.unsupported_role(inputs[2]), None);
    }

    #[test]
    fn the_overlay_roles_are_parsed_on_any_element() {
        let tree: Element = view! {
            <div>
                <div role="dialog" />
                <div role="alertdialog" />
                <div role="menu" />
                <div role="menubar" />
                <button role="menuitem">{"x"}</button>
                <div role="tooltip" />
                <div role="grid" />
            </div>
        };
        let arena = Arena::build(&tree);
        let roles: Vec<_> = arena
            .find_all(|a, id| a.tag(id) != "div" || a.children(id).is_empty())
            .into_iter()
            .map(|id| arena.role(id))
            .collect();
        use AccessibleRole::*;
        assert_eq!(
            roles,
            [
                Some(Dialog),
                Some(AlertDialog),
                Some(Menu),
                Some(MenuBar),
                Some(MenuItem),
                Some(Tooltip),
                None
            ]
        );
        let grid = arena
            .find(|a, id| a.unsupported_role(id).is_some())
            .unwrap();
        assert_eq!(arena.unsupported_role(grid), Some("grid"));
    }

    #[test]
    fn list_roles_and_a_collection_position_are_read() {
        let tree: Element = view! {
            <div role="list">
                <div role="listitem" set_size="500" position_in_set="42" />
                <div set_size="3" position_in_set="4" />
                <div position_in_set="2" />
                <div set_size="3" position_in_set="0" />
            </div>
        };
        let arena = Arena::build(&tree);
        let list = arena.roots()[0];
        assert_eq!(arena.role(list), Some(AccessibleRole::List));
        let items = arena.children(list);
        assert_eq!(arena.role(items[0]), Some(AccessibleRole::ListItem));
        assert_eq!(arena.collection_position(items[0]), Some((42, 500)));
        assert_eq!(arena.collection_position(items[1]), None, "past the end");
        assert_eq!(arena.collection_position(items[2]), None, "no size");
        assert_eq!(
            arena.collection_position(items[3]),
            None,
            "positions start at 1"
        );
    }
    #[test]
    fn accessible_label_is_none_without_the_attribute() {
        let tree: Element = view! { <button>{"x"}</button> };
        let arena = Arena::build(&tree);
        assert_eq!(arena.accessible_label(arena.roots()[0]), None);
    }

    #[test]
    fn src_and_alt_read_the_plain_string_form() {
        let tree: Element = view! { <img src="photo.png" alt="A photo" /> };
        let arena = Arena::build(&tree);
        let img = arena.roots()[0];
        assert_eq!(arena.src_attr(img), Some("photo.png"));
        assert_eq!(arena.alt_attr(img), Some("A photo"));
    }

    #[test]
    fn src_and_alt_are_none_without_the_attributes() {
        let tree: Element = view! { <img /> };
        let arena = Arena::build(&tree);
        let img = arena.roots()[0];
        assert_eq!(arena.src_attr(img), None);
        assert_eq!(arena.alt_attr(img), None);
    }

    #[test]
    fn text_content_flattens_through_a_fragment_but_skips_nested_nodes() {
        let tree = Element::node(
            "p",
            vec![],
            vec![Element::Fragment(vec![
                Element::text("Hello, "),
                Element::node("b", vec![], vec![Element::text("world")]),
                Element::text("!"),
            ])],
        );
        let arena = Arena::build(&tree);
        let p = arena.roots()[0];
        assert_eq!(
            arena.text_content(p),
            "Hello, !",
            "the <b>'s own text belongs to its own node, not its parent's"
        );
        let b = arena.children(p)[0];
        assert_eq!(arena.text_content(b), "world");
    }

    #[test]
    fn inline_items_preserves_the_interleaved_order_text_content_loses() {
        let tree = Element::node(
            "p",
            vec![],
            vec![Element::Fragment(vec![
                Element::text("Hello, "),
                Element::node("b", vec![], vec![Element::text("world")]),
                Element::text("!"),
            ])],
        );
        let arena = Arena::build(&tree);
        let p = arena.roots()[0];
        let b = arena.children(p)[0];

        assert_eq!(
            arena.inline_items(p),
            &[
                InlineItem::Text("Hello, ".to_string()),
                InlineItem::Element(b),
                InlineItem::Text("!".to_string()),
            ],
            "text_content alone would report \"Hello, !\", losing where <b> sat"
        );
    }

    #[test]
    fn inline_items_merges_consecutive_text_runs_into_one_item() {
        // Two separate string literals that happen to be adjacent siblings
        // (e.g. from two interpolated `{...}` expressions in view!) are one
        // inline run, the same as a real inline formatting context treats
        // adjacent text nodes — not two zero-width-joined items.
        let tree = Element::node(
            "p",
            vec![],
            vec![Element::text("foo"), Element::text("bar")],
        );
        let arena = Arena::build(&tree);
        let p = arena.roots()[0];
        assert_eq!(
            arena.inline_items(p),
            &[InlineItem::Text("foobar".to_string())]
        );
    }

    #[test]
    fn inline_items_is_all_text_for_a_pure_text_node() {
        let tree: Element = view! { <h2>{"Hello"}</h2> };
        let arena = Arena::build(&tree);
        let h2 = arena.roots()[0];
        assert_eq!(
            arena.inline_items(h2),
            &[InlineItem::Text("Hello".to_string())]
        );
    }

    #[test]
    fn inline_items_is_all_elements_for_pure_element_children() {
        let tree: Element = view! {
            <div>
                <span>{"a"}</span>
                <span>{"b"}</span>
            </div>
        };
        let arena = Arena::build(&tree);
        let div = arena.roots()[0];
        let children = arena.children(div);
        assert_eq!(
            arena.inline_items(div),
            &[
                InlineItem::Element(children[0]),
                InlineItem::Element(children[1]),
            ]
        );
    }

    #[test]
    fn inline_items_is_empty_for_a_childless_node() {
        let tree: Element = view! { <div /> };
        let arena = Arena::build(&tree);
        assert!(arena.inline_items(arena.roots()[0]).is_empty());
    }

    /// `build`/`find` used to recurse once per tree level and overflow
    /// the stack well before this depth — an explicit stack fixed that.
    #[test]
    fn build_and_find_survive_a_tree_far_deeper_than_the_old_recursion_limit() {
        let depth = 20_000;
        let mut tree = Element::node("div", vec![("class".into(), "leaf".into())], vec![]);
        for _ in 0..depth {
            tree = Element::node("div", vec![], vec![tree]);
        }

        let arena = Arena::build(&tree);
        let leaf = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "leaf"))
            .expect("the leaf must still be reachable at full depth");
        assert!(arena.children(leaf).is_empty());

        let mut depth_from_leaf = 0;
        let mut current = leaf;
        while let Some(parent) = arena.parent(current) {
            current = parent;
            depth_from_leaf += 1;
        }
        assert_eq!(depth_from_leaf, depth);
    }

    #[test]
    fn a_node_reports_where_its_element_was_written() {
        let marker = line!();
        let tree = view! {
            <div class="outer">
                <p class="inner">{"x"}</p>
            </div>
        };
        let arena = Arena::build(&tree);
        let find = |class: &str| {
            arena
                .find(|a, id| a.classes(id).iter().any(|c| c == class))
                .unwrap()
        };
        let outer = arena.source(find("outer")).expect("view! records it");
        let inner = arena.source(find("inner")).expect("view! records it");
        assert_eq!((outer.line, outer.path), (marker + 2, "div"));
        assert_eq!((inner.line, inner.path), (marker + 3, "div > p"));
        assert!(outer.file.ends_with("tree.rs"));
    }

    #[test]
    fn an_element_built_by_hand_has_no_location() {
        let tree = Element::node("div", vec![], vec![]);
        let arena = Arena::build(&tree);
        assert_eq!(arena.source(0), None);
    }

    #[test]
    fn overlay_content_keeps_its_location() {
        let marker = line!();
        let content = view! {
            <span class="overlay">{"o"}</span>
        };
        let tree = Element::node("div", vec![], vec![Element::Portal(vec![content])]);
        let arena = Arena::build(&tree);
        let overlay = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "overlay"))
            .expect("the portal's content becomes a root of its own");
        let at = arena.source(overlay).expect("view! records it");
        assert_eq!(at.line, marker + 2);
    }
}
