//! Bridges Stylo's `TDocument`/`TNode`/`TElement`/`selectors::Element`
//! traits onto [`crate::tree::Arena`], and converts its resolved
//! `ComputedValues` back into [`crate::cascade::ComputedStyle`]. This is
//! the crate's real CSS engine: selector matching, cascade, and
//! inheritance are Stylo's own, not reimplemented here.
//!
//! Each [`StyloNode`] handle is a single reference (`&NodeSlot`), not a
//! `(tree, id)` pair: Stylo's internal style-sharing cache type-erases a
//! fixed-size element buffer via `mem::transmute` and asserts the erased
//! and real sizes match at runtime, so `TElement`'s concrete type has to
//! stay pointer-sized. `NodeSlot` copies what it needs out of the Arena
//! once, up front, with parent/child pointers resolved in a second pass
//! once every slot has a stable address, so the handle never needs a
//! second reference back to any shared context.

use crate::shape_function::Units;
use crate::style_map::StyleMap;
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;

use atomic_refcell::AtomicRefCell;
use selectors::attr::{AttrSelectorOperation, NamespaceConstraint};
use selectors::matching::{ElementSelectorFlags, MatchingContext, VisitedHandlingMode};
use selectors::{Element as SelectorsElement, OpaqueElement};
use servo_arc::{Arc, ArcBorrow};
use style::animation::{AnimationSetKey, AnimationState};
use style::context::{
    CascadeInputs, QuirksMode, RegisteredSpeculativePainter, RegisteredSpeculativePainters,
    SharedStyleContext, StyleContext, ThreadLocalStyleContext,
};
use style::data::{ElementData, ElementStyles};
use style::dom::{LayoutIterator, NodeInfo, OpaqueNode, TDocument, TElement, TNode, TShadowRoot};
use style::font_metrics::FontMetrics;
use style::global_style_data::GLOBAL_STYLE_DATA;
use style::media_queries::{Device, MediaType};
use style::properties::style_structs::Font as FontStruct;
use style::properties::{ComputedValues, PropertyDeclarationBlock};
use style::queries::values::PrefersColorScheme;
use style::rule_tree::CascadeLevel;
use style::selector_parser::{AttrValue, Lang, PseudoElement, SelectorImpl};
use style::servo::media_queries::FontMetricsProvider;
use style::shared_lock::{Locked, SharedRwLock, StylesheetGuards};
use style::sharing::StyleSharingTarget;
use style::style_resolver::{PseudoElementResolution, StyleResolverForElement};
use style::stylesheets::DocumentStyleSheet;
use style::stylesheets::layer_rule::LayerOrder;
use style::stylist::{CascadeData, RuleInclusion, Stylist};
use style::traversal_flags::TraversalFlags;
use style::values::AtomIdent;
use style::values::computed::font::GenericFontFamily;
use style::values::computed::{CSSPixelLength, Display, Length};
use style::{Atom, LocalName};
use style_traits::CssWriter;
use stylo_atoms::Atom as WeakAtom;
use stylo_dom::ElementState;

use crate::animation::AnimationTimeline;
use crate::cascade::{
    Appearance as FlorAppearance, AspectRatio as FlorAspectRatio, BorderLineStyle,
    BorderSide as FlorBorderSide, BoxShadow as FlorBoxShadow, ComputedStyle,
    ContainerType as FlorContainerType, ContentAlignment, Corners, Display as FlorDisplay, Edges,
    FilterFunction as FlorFilterFunction, FlexDirection, FlexWrap, FontFamily as FlorFontFamily,
    ItemAlignment, LengthPercentage as FlorLengthPercentage, ObjectFit as FlorObjectFit,
    Resize as FlorResize, TransformFunction as FlorTransformFunction, Viewport as FlorViewport,
};
use crate::clip::{ClipPath, ClipShape, PathSegment, ReferenceBox, ShapeRadius};
use crate::color::Rgba;
use crate::grid::{
    GridArea, GridAutoFlow, GridBreadth, GridLimit, GridPlacement, GridRepeat, GridRepeatCount,
    GridTemplate, GridTemplateAreas, GridTemplateComponent, GridTrackSize,
};
use crate::interaction::InteractionState;
use crate::stylesheet_parse::Rule;
use crate::tree::{Arena, NodeId};

type BorrowedLocalName = <SelectorImpl as selectors::parser::SelectorImpl>::BorrowedLocalName;
type BorrowedNamespaceUrl = <SelectorImpl as selectors::parser::SelectorImpl>::BorrowedNamespaceUrl;

/// Everything one node needs to act as a Stylo element, copied out of
/// [`Arena`] once at construction — see the module doc for why this can't
/// instead hold a `NodeId` plus a back-reference to a shared tree.
struct NodeSlot {
    parent: Option<*const NodeSlot>,
    depth: usize,
    sibling_index: usize,
    children: Vec<*const NodeSlot>,
    tag: &'static str,
    /// Every attribute exactly as authored — see [`Arena::attrs`] and
    /// this struct's own use in `attr_matches`.
    attrs: Vec<(String, String)>,
    classes: Vec<String>,
    /// This element's identity for animation purposes, stable across
    /// separate [`compute`] calls unlike this ephemeral slot's own address
    /// — see [`crate::animation`]'s module doc.
    stable_id: usize,
    id_attr: Option<String>,
    /// This element's real inline `style="..."` declaration, parsed once
    /// at construction — see [`style_attribute`](TElement::style_attribute)'s
    /// own doc for why this is the highest-specificity input the cascade
    /// sees, not an inert string.
    style_pdb: Option<Arc<Locked<PropertyDeclarationBlock>>>,
    /// Same value as `id_attr`, pre-interned — `TElement::id` needs this
    /// exact type back, and it's also what Stylo's selector map uses to
    /// bucket `#id` rules by hash before ever calling `has_id`, so a
    /// node whose `id()` doesn't match its own `has_id` would have its
    /// `#id` rules silently never even considered a candidate.
    id_atom: Option<WeakAtom>,
    state: Cell<ElementState>,
    /// The state of the nodes above this one, as a number: two nodes with the same
    /// number have the same state in every ancestor. See
    /// [`StyloTree::number_contexts`].
    context: Cell<u32>,
    /// The `:has()` this node has, by number, as the classes `florui-has-N` the
    /// rules were rewritten to (see [`crate::has_adapter`]).
    has: std::cell::RefCell<Vec<usize>>,
    /// Whether the node holds text, which `:empty` counts as content.
    has_text: bool,
    /// [`NodeSlot::identity`], worked out once: the elements do not change in a kept tree.
    identity_hash: std::cell::OnceCell<u64>,
    /// For the node of a first letter, the slot of the element it is the first letter
    /// of.
    fl_owner: Option<usize>,
    /// The first-letter selectors the owner matches, by number, as the classes
    /// `florui-fl-N` the rules were rewritten to (see [`crate::first_letter_adapter`]).
    fl: std::cell::RefCell<Vec<usize>>,
    /// Whether the node is a first letter in a box of `::before` or `::after`, which
    /// inherits from that box and not from the element.
    fl_in_box: bool,
    /// The values of the `::before` and `::after` of the node: a first letter in the box of
    /// the `::before` inherits from the first, and a first line works out the style of both.
    pseudo_values: std::cell::RefCell<[Option<Arc<ComputedValues>>; 2]>,
    data: AtomicRefCell<ElementData>,
    dirty_descendants: Cell<bool>,
    local_name: BorrowedLocalName,
    namespace: BorrowedNamespaceUrl,
}

/// The tag of the node of a first line.
const FIRST_LINE_TAG: &str = "florui-first-line";

/// Owns every node's [`NodeSlot`] in one `Vec` sized exactly once up
/// front, so pushing never reallocates and the raw pointers `NodeSlot`s
/// hold to each other (set in a second pass, once every slot has its
/// final address) stay valid for the tree's whole lifetime.
pub(crate) struct StyloTree {
    slots: Vec<NodeSlot>,
    /// Where each node sits in `slots`, indexed by its `NodeId` (a pre-order
    /// index, so small and dense); `usize::MAX` for an id not in the tree.
    index_of: Vec<usize>,
    /// Pre-order (parent before children), the same order [`Self::slots`]
    /// was built in — [`compute_in_layout_state`] resolves in this order
    /// rather than `index_of's arbitrary `HashMap` iteration order, so an
    /// ancestor is always styled before any of its descendants.
    order: Vec<NodeId>,
    /// Whether any `style` attribute declares `scroll-behavior`, which needs
    /// the same user-agent reset an author sheet declaring it does.
    uses_inline_scroll_behavior: bool,
    /// Whether the last cascade gave any node a `:has()`, so the next one knows
    /// whether there is anything to take away.
    has_given: Cell<bool>,
    /// The slot of the parent of each slot.
    parents: Vec<Option<usize>>,
    /// The `::before` and `::after` boxes of each node, by slot, where the arena
    /// has them; empty when it has none.
    generated: Vec<[Option<NodeId>; 2]>,
    /// The slots of the first-letter nodes.
    first_letters: Vec<usize>,
    /// The slots of the first-line nodes.
    first_lines: Vec<usize>,
}

/// Where the number of a node's context sits in the state style sharing sees:
/// in the eight bits above the ones Stylo gives meaning to.
const CONTEXT_SHIFT: u32 = 56;
const _: () = assert!(ElementState::all().bits() < 1 << CONTEXT_SHIFT);

/// The context of every node past the 254 distinct ones the bits can name.
/// Such a node does not take part in style sharing at all.
const OVERFLOW_CONTEXT: u32 = (1 << (64 - CONTEXT_SHIFT)) - 1;

/// The state flags of node `id`: its markup state (`disabled`, `checked`, ...) read
/// from `arena`, and its interaction state (hover, focus, form validity, visited)
/// from `state`.
fn node_state_of(arena: &Arena, state: &InteractionState, id: NodeId) -> ElementState {
    let mut node_state = ElementState::empty();
    if state.is_hovered(id) {
        node_state |= ElementState::HOVER;
    }
    if state.is_focused(id) {
        node_state |= ElementState::FOCUS;
    }
    if state.is_active(id) {
        node_state |= ElementState::ACTIVE;
    }
    if state.is_focus_visible(id) {
        node_state |= ElementState::FOCUSRING;
    }
    // Markup state, not runtime interaction state like hover/
    // focus above -- read straight from `arena` rather than
    // threaded through `InteractionState`. Tag-gated to match
    // `florui_platform::focus::is_focusable`'s own v1 scope:
    // `disabled` has no wired behavior outside these tags yet, and
    // leaving `state` untouched for every other tag means neither
    // :disabled nor :enabled ever matches there either -- the same
    // as real HTML, where both pseudo-classes only apply to form
    // controls.
    if matches!(
        arena.tag(id),
        "button" | "select" | "textarea" | "fieldset" | "input"
    ) {
        if arena.is_disabled(id) {
            node_state |= ElementState::DISABLED;
        } else {
            node_state |= ElementState::ENABLED;
        }
    }
    // Same markup-state contract as the `<button>` block above,
    // for `<input type="checkbox"|"radio">` -- plus `:checked`,
    // which `<button>` has no equivalent of.
    if arena.tag(id) == "input" && matches!(arena.input_type(id), Some("checkbox") | Some("radio"))
    {
        if arena.is_disabled(id) {
            node_state |= ElementState::DISABLED;
        } else {
            node_state |= ElementState::ENABLED;
        }
        if arena.is_checked(id) {
            node_state |= ElementState::CHECKED;
        }
        // Real HTML: `indeterminate` only exists on a checkbox,
        // never a radio.
        if arena.input_type(id) == Some("checkbox") && arena.is_indeterminate(id) {
            node_state |= ElementState::INDETERMINATE;
        }
    }
    // Same markup-state contract again, for `<input type="range">`
    // -- no `:checked`/`:indeterminate` equivalent for a slider.
    if arena.tag(id) == "input" && arena.input_type(id) == Some("range") {
        if arena.is_disabled(id) {
            node_state |= ElementState::DISABLED;
        } else {
            node_state |= ElementState::ENABLED;
        }
    }
    if let Some(form) = state.form_state(id) {
        for (on, flag) in [
            (form.required, ElementState::REQUIRED),
            (form.optional, ElementState::OPTIONAL_),
            (form.valid, ElementState::VALID),
            (form.invalid, ElementState::INVALID),
            (form.user_valid, ElementState::USER_VALID),
            (form.user_invalid, ElementState::USER_INVALID),
            (form.read_only, ElementState::READONLY),
            (form.read_write, ElementState::READWRITE),
            (form.default, ElementState::DEFAULT),
            (form.in_range, ElementState::INRANGE),
            (form.out_of_range, ElementState::OUTOFRANGE),
            (form.placeholder_shown, ElementState::PLACEHOLDER_SHOWN),
        ] {
            if on {
                node_state |= flag;
            }
        }
    }
    // `:link`/`:visited` for `<a href>` -- exactly one of the two
    // bits, never both, never neither, matching real Gecko's own
    // `is_link()` invariant. An `<a>` with no `href` is not a
    // hyperlink at all (real CSS: neither `:link` nor `:visited`
    // matches it), so `node_state` is left untouched there, same
    // as `is_focusable`'s own href gate. Deliberately simpler than
    // a real browser otherwise: this crate doesn't implement
    // `is_link()`/`visited_handling()`, or Stylo's dual primary/
    // visited cascade those exist to keep `:visited` from leaking
    // into -- `match_non_ts_pseudo_class`'s existing plain
    // state-bit check (same one `:hover`/`:focus`/`:active`
    // already use) is already correct here without a special
    // case, precisely because there's only one cascade pass in
    // this crate's own usage, so there's no primary/visited split
    // for a raw bit check to leak between. Real browsers need
    // that extra machinery to stop `:visited`-authored styles
    // (layout-affecting properties especially) from being
    // detectable by a script probing computed styles; florui has
    // no such script layer for a leak to reach.
    if arena.tag(id) == "a"
        && let Some(href) = arena.href(id)
    {
        node_state |= if state.is_visited(href) {
            ElementState::VISITED
        } else {
            ElementState::UNVISITED
        };
    }
    node_state
}

impl StyloTree {
    /// Builds a self-contained copy of every node reachable from `arena`'s
    /// roots, synthesizing a single container root when there is more
    /// than one (a `view!` `Fragment` can produce several) so Stylo
    /// always has exactly one document element to resolve from.
    pub(crate) fn new(
        arena: &Arena,
        state: &InteractionState,
        timeline: &mut AnimationTimeline,
    ) -> (Self, NodeId) {
        let mut order = Vec::new();
        for &root in arena.roots() {
            Self::collect_order(arena, root, &mut order);
        }

        let mut index_of = vec![usize::MAX; arena.node_count()];
        for (index, &id) in order.iter().enumerate() {
            index_of[id] = index;
        }
        // Each node's position among its siblings of the same tag, ignoring
        // class, so a class toggle alone does not reassign identity.
        let mut ordinal = vec![0usize; index_of.len()];
        let mut assign_ordinals = |siblings: &[NodeId]| {
            let mut seen: Vec<(&str, usize)> = Vec::new();
            for &sibling in siblings {
                let tag = arena.tag(sibling);
                match seen.iter_mut().find(|(seen_tag, _)| *seen_tag == tag) {
                    Some((_, count)) => {
                        ordinal[sibling] = *count;
                        *count += 1;
                    }
                    None => {
                        ordinal[sibling] = 0;
                        seen.push((tag, 1));
                    }
                }
            }
        };
        assign_ordinals(arena.roots());
        for &id in &order {
            assign_ordinals(&arena.element_child_ids(id));
        }
        let mut slots: Vec<NodeSlot> = Vec::with_capacity(order.len());
        let mut stable_ids: Vec<usize> = Vec::with_capacity(order.len());
        for &id in &order {
            let node_state = node_state_of(arena, state, id);
            let parent_stable = arena
                .style_parent(id)
                .map(|parent_id| stable_ids[index_of[parent_id]]);
            let stable_id = timeline.stable_id(parent_stable, arena.tag(id), ordinal[id]);
            stable_ids.push(stable_id);
            slots.push(NodeSlot {
                parent: None,
                depth: arena
                    .style_parent(id)
                    .map_or(0, |p| slots[index_of[p]].depth + 1),
                sibling_index: 0,
                children: Vec::new(),
                tag: arena.tag(id),
                attrs: arena.attrs(id).to_vec(),
                classes: arena.classes(id).to_vec(),
                stable_id,
                id_attr: arena.id_attr(id).map(str::to_owned),
                id_atom: arena.id_attr(id).map(WeakAtom::from),
                style_pdb: arena.style_attr(id).map(parse_inline_style),
                state: Cell::new(node_state),
                context: Cell::new(0),
                has: std::cell::RefCell::new(Vec::new()),
                has_text: !arena.text_content(id).is_empty(),
                identity_hash: std::cell::OnceCell::new(),
                fl_owner: arena.first_letter_owner(id).map(|owner| index_of[owner]),
                fl: std::cell::RefCell::new(Vec::new()),
                fl_in_box: arena.first_letter_owner(id).is_some()
                    && arena
                        .parent(id)
                        .is_some_and(|p| arena.generated(p).is_some()),
                pseudo_values: std::cell::RefCell::new([None, None]),
                data: AtomicRefCell::new(ElementData::default()),
                dirty_descendants: Cell::new(false),
                local_name: arena.tag(id).into(),
                namespace: "".into(),
            });
        }

        // Second pass: every slot has its final address now that the
        // Vec has stopped growing, so parent/child pointers can be taken.
        for (index, &id) in order.iter().enumerate() {
            let parent_ptr = arena
                .style_parent(id)
                .map(|parent_id| &slots[index_of[parent_id]] as *const NodeSlot);
            let element_children = arena.element_child_ids(id);
            let children_ptrs: Vec<*const NodeSlot> = element_children
                .iter()
                .map(|&child_id| &slots[index_of[child_id]] as *const NodeSlot)
                .collect();
            slots[index].parent = parent_ptr;
            slots[index].children = children_ptrs;
            for (position, child_id) in element_children.iter().enumerate() {
                slots[index_of[*child_id]].sibling_index = position;
            }
        }

        // Multiple roots (a Fragment) have no single document element for
        // Stylo to resolve from; a real florui tree of one component's
        // output is a single element in practice, so this only matters
        // for a bare multi-root Fragment, which resolves each of its own
        // roots as if it were independently the document.
        let primary_root = order[0];
        let uses_inline_scroll_behavior = order.iter().any(|&id| {
            arena
                .style_attr(id)
                .is_some_and(|css| css.to_ascii_lowercase().contains("scroll-behavior"))
        });
        let parents: Vec<Option<usize>> = order
            .iter()
            .map(|&id| arena.style_parent(id).map(|parent| index_of[parent]))
            .collect();
        let generated = if arena.has_generated() {
            order
                .iter()
                .map(|&id| {
                    [
                        arena.generated_child(id, crate::Pseudo::Before),
                        arena.generated_child(id, crate::Pseudo::After),
                    ]
                })
                .collect()
        } else {
            Vec::new()
        };
        let (first_lines, first_letters): (Vec<usize>, Vec<usize>) = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.fl_owner.is_some())
            .map(|(index, _)| index)
            .partition(|&index| slots[index].tag == FIRST_LINE_TAG);
        let tree = Self {
            slots,
            index_of,
            order,
            uses_inline_scroll_behavior,
            has_given: Cell::new(false),
            parents,
            generated,
            first_letters,
            first_lines,
        };
        tree.number_contexts();
        (tree, primary_root)
    }

    /// Numbers each node by the state of the nodes above it. and by what
    /// the selectors that name an ancestor read of it: its tag, its id and its classes. A selector such as
    /// `.row:hover .cell` matches two cells that are alike in everything else, under
    /// rows that differ only in their state, differently; Stylo's style sharing
    /// looks at the nodes and at the style of their parents, which is the same for
    /// such rows when no rule of their own tells them apart, and would take the
    /// cells for the same. Nodes with the same number have the same state in every
    /// ancestor, and share. (What a sibling before a node has is covered by
    /// Stylo's own check of the selectors with a sibling combinator.)
    fn number_contexts(&self) {
        let mut numbers: HashMap<(u32, u64, u64), u32> = HashMap::new();
        let mut of_slot: Vec<u32> = vec![0; self.slots.len()];
        for index in 0..self.order.len() {
            // What is above: the number of the parent, and the parent's own state.
            let above = match self.parents[index] {
                Some(parent) => (
                    of_slot[parent],
                    self.slots[parent].state.get().bits(),
                    self.slots[parent].identity(),
                ),
                None => (0, 0, 0),
            };
            let next = numbers.len() as u32 + 1;
            let number = match numbers.get(&above) {
                Some(&number) => number,
                None if next < OVERFLOW_CONTEXT => {
                    numbers.insert(above, next);
                    next
                }
                None => OVERFLOW_CONTEXT,
            };
            of_slot[index] = number;
            self.slots[index].context.set(number);
        }
    }

    /// Sets a tree built before to be styled under `state`: each node's state
    /// flags are read again and what a cascade left in it is cleared. All else a
    /// node holds comes from the elements, which are the same for as long as the
    /// tree is kept.
    pub(crate) fn refresh(&self, arena: &Arena, state: &InteractionState) {
        for (slot, &id) in self.slots.iter().zip(&self.order) {
            slot.state.set(node_state_of(arena, state, id));
            *slot.data.borrow_mut() = ElementData::default();
            slot.dirty_descendants.set(false);
        }
        self.number_contexts();
    }

    /// How many nodes the tree has.
    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    /// Where the slot a pointer to it names sits.
    fn slot_index(&self, slot: *const NodeSlot) -> usize {
        (slot as usize - self.slots.as_ptr() as usize) / std::mem::size_of::<NodeSlot>()
    }

    /// The slots of the first lines of the blocks that have one of `nodes` in them, or are
    /// one of them.
    pub(crate) fn first_lines_reaching(&self, nodes: &[NodeId]) -> Vec<usize> {
        let mut lines: Vec<usize> = Vec::new();
        for &node in nodes {
            let mut at = self
                .index_of
                .get(node)
                .copied()
                .filter(|&index| index != usize::MAX);
            while let Some(index) = at {
                if let Some(&line) = self
                    .first_lines
                    .iter()
                    .find(|&&line| self.slots[line].fl_owner == Some(index))
                    && !lines.contains(&line)
                {
                    lines.push(line);
                }
                at = self.slots[index]
                    .parent
                    .map(|parent| self.slot_index(parent));
            }
        }
        lines
    }

    /// Where `node` sits in the tree, and the node at a place.
    pub(crate) fn slot_of(&self, node: NodeId) -> usize {
        self.index_of[node]
    }

    pub(crate) fn node_of(&self, slot: usize) -> NodeId {
        self.order[slot]
    }

    /// Reads the state flags of every node again under `state` and returns the
    /// slots of those that changed. Nothing else in the tree is touched: what
    /// the last cascade left in the nodes stays.
    pub(crate) fn update_states(&self, arena: &Arena, state: &InteractionState) -> Vec<usize> {
        let mut changed = Vec::new();
        for (index, (slot, &id)) in self.slots.iter().zip(&self.order).enumerate() {
            let now = node_state_of(arena, state, id);
            if now != slot.state.get() {
                slot.state.set(now);
                changed.push(index);
            }
        }
        if !changed.is_empty() {
            self.number_contexts();
        }
        changed
    }

    /// The identity each node has on the animation timeline.
    pub(crate) fn stable_ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots.iter().map(|slot| slot.stable_id)
    }

    fn collect_order(arena: &Arena, id: NodeId, order: &mut Vec<NodeId>) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            order.push(id);
            if arena.has_first_letters() {
                order.extend(arena.first_letter_children(id));
                if let Some(before) = arena.generated_child(id, crate::Pseudo::Before) {
                    order.extend(arena.first_letter_children(before));
                }
            }
            stack.extend(arena.element_child_ids(id).iter().rev());
        }
    }

    /// The nodes level by level (every node of one depth, then the next),
    /// siblings in document order, one root's whole tree at a time. A parent
    /// is still styled before its children, and Stylo's style sharing cache,
    /// which only compares nodes at one depth, can then find a sibling or
    /// cousin that is already styled. A root at a time, because Stylo's
    /// ancestor filter needs the nodes it moves between to share an ancestor,
    /// which the separate roots of a fragment do not.
    fn level_order(&self) -> Vec<NodeId> {
        let depth = |id: NodeId| self.slots[self.index_of[id]].depth;
        let mut levels = Vec::with_capacity(self.order.len());
        let mut start = 0;
        for end in 1..=self.order.len() {
            if end == self.order.len() || depth(self.order[end]) == 0 {
                // Stable by depth, by counting: how many nodes sit at each
                // depth gives where each depth's run starts.
                let tree = &self.order[start..end];
                let deepest = tree.iter().map(|&id| depth(id)).max().unwrap_or(0);
                let mut next = vec![0usize; deepest + 2];
                for &id in tree {
                    next[depth(id) + 1] += 1;
                }
                for level in 1..next.len() {
                    next[level] += next[level - 1];
                }
                let base = levels.len();
                levels.resize(base + tree.len(), 0);
                for &id in tree {
                    let at = &mut next[depth(id)];
                    levels[base + *at] = id;
                    *at += 1;
                }
                start = end;
            }
        }
        levels
    }

    fn node(&self, id: NodeId) -> StyloNode<'_> {
        StyloNode(&self.slots[self.index_of[id]])
    }
}

#[derive(Copy, Clone)]
#[repr(transparent)]
struct StyloNode<'a>(&'a NodeSlot);

impl fmt::Debug for StyloNode<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StyloNode({:p})", self.0)
    }
}

impl PartialEq for StyloNode<'_> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}
impl Eq for StyloNode<'_> {}
impl std::hash::Hash for StyloNode<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.0 as *const NodeSlot).hash(state);
    }
}

impl<'a> StyloNode<'a> {
    fn index_in_parent_siblings(&self) -> Option<usize> {
        self.parent_node()?;
        Some(self.0.sibling_index)
    }
}

impl NodeInfo for StyloNode<'_> {
    fn is_element(&self) -> bool {
        true
    }

    fn is_text_node(&self) -> bool {
        false
    }
}

impl<'a> TDocument for StyloNode<'a> {
    type ConcreteNode = StyloNode<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn is_html_document(&self) -> bool {
        true
    }

    fn quirks_mode(&self) -> QuirksMode {
        QuirksMode::NoQuirks
    }

    fn shared_lock(&self) -> &SharedRwLock {
        // Every stylesheet this crate parses is wrapped under the one
        // process-wide lock `shared_lock()` returns (see its own doc) —
        // Stylo's own real animation code (`servo/animation.rs`'s
        // `IntermediateComputedKeyframe::resolve_style`) wraps a
        // synthesized per-keyframe declaration block under whatever this
        // returns and then reads it back through `context.guards`, which
        // is built from that same singleton, so this has to agree with it
        // rather than minting its own lock per node.
        shared_lock()
    }
}

impl<'a> TShadowRoot for StyloNode<'a> {
    type ConcreteNode = StyloNode<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn host(&self) -> <Self::ConcreteNode as TNode>::ConcreteElement {
        unreachable!("florui has no shadow DOM; this bridge never constructs a shadow root")
    }

    fn style_data<'b>(&self) -> Option<&'b CascadeData>
    where
        Self: 'b,
    {
        None
    }
}

impl<'a> TNode for StyloNode<'a> {
    type ConcreteElement = StyloNode<'a>;
    type ConcreteDocument = StyloNode<'a>;
    type ConcreteShadowRoot = StyloNode<'a>;

    fn parent_node(&self) -> Option<Self> {
        self.0.parent.map(|ptr| StyloNode(unsafe { &*ptr }))
    }

    fn first_child(&self) -> Option<Self> {
        self.0
            .children
            .first()
            .map(|&ptr| StyloNode(unsafe { &*ptr }))
    }

    fn last_child(&self) -> Option<Self> {
        self.0
            .children
            .last()
            .map(|&ptr| StyloNode(unsafe { &*ptr }))
    }

    fn prev_sibling(&self) -> Option<Self> {
        let index = self.index_in_parent_siblings()?;
        let parent = self.parent_node()?;
        index
            .checked_sub(1)
            .map(|i| StyloNode(unsafe { &*parent.0.children[i] }))
    }

    fn next_sibling(&self) -> Option<Self> {
        let index = self.index_in_parent_siblings()?;
        let parent = self.parent_node()?;
        parent
            .0
            .children
            .get(index + 1)
            .map(|&ptr| StyloNode(unsafe { &*ptr }))
    }

    fn owner_doc(&self) -> Self::ConcreteDocument {
        let mut node = *self;
        while let Some(parent) = node.parent_node() {
            node = parent;
        }
        node
    }

    fn is_in_document(&self) -> bool {
        true
    }

    fn traversal_parent(&self) -> Option<Self::ConcreteElement> {
        self.parent_node()
    }

    fn opaque(&self) -> OpaqueNode {
        OpaqueNode(self.0.stable_id)
    }

    fn debug_id(self) -> usize {
        self.0 as *const NodeSlot as usize
    }

    fn as_element(&self) -> Option<Self::ConcreteElement> {
        Some(*self)
    }

    fn as_document(&self) -> Option<Self::ConcreteDocument> {
        if self.parent_node().is_none() {
            Some(*self)
        } else {
            None
        }
    }

    fn as_shadow_root(&self) -> Option<Self::ConcreteShadowRoot> {
        None
    }
}

impl<'a> SelectorsElement for StyloNode<'a> {
    type Impl = SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.0)
    }

    fn parent_element(&self) -> Option<Self> {
        self.parent_node()
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.prev_sibling()
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.next_sibling()
    }

    fn first_element_child(&self) -> Option<Self> {
        self.first_child()
    }

    fn is_html_element_in_html_document(&self) -> bool {
        true
    }

    fn has_local_name(&self, local_name: &BorrowedLocalName) -> bool {
        self.0.tag == (local_name.as_ref() as &str)
    }

    fn has_namespace(&self, ns: &BorrowedNamespaceUrl) -> bool {
        ns.is_empty()
    }

    fn is_same_type(&self, other: &Self) -> bool {
        self.0.tag == other.0.tag
    }

    fn attr_matches(
        &self,
        _ns: &NamespaceConstraint<&<SelectorImpl as selectors::parser::SelectorImpl>::NamespaceUrl>,
        local_name: &<SelectorImpl as selectors::parser::SelectorImpl>::LocalName,
        operation: &AttrSelectorOperation<
            &<SelectorImpl as selectors::parser::SelectorImpl>::AttrValue,
        >,
    ) -> bool {
        let name = local_name.as_ref() as &str;
        self.0
            .attrs
            .iter()
            .find(|(key, _)| key == name)
            .is_some_and(|(_, value)| operation.eval_str(value))
    }

    fn match_non_ts_pseudo_class(
        &self,
        pseudo_class: &<SelectorImpl as selectors::parser::SelectorImpl>::NonTSPseudoClass,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        // State-based pseudo-classes (:hover, :focus, :active) are not
        // auto-matched by selectors from `state()` — the element is
        // expected to consult its own state against the pseudo-class's
        // flag itself, same as Servo's own TElement impls do.
        self.0.state.get().intersects(pseudo_class.state_flag())
    }

    fn match_pseudo_element(
        &self,
        _pseudo_element: &<SelectorImpl as selectors::parser::SelectorImpl>::PseudoElement,
        _context: &mut MatchingContext<'_, Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        false
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &AtomIdent, case_sensitivity: selectors::attr::CaseSensitivity) -> bool {
        self.0
            .id_attr
            .as_deref()
            .is_some_and(|attr| case_sensitivity.eq(attr.as_bytes(), id.as_ref().as_bytes()))
    }

    fn has_class(
        &self,
        name: &AtomIdent,
        case_sensitivity: selectors::attr::CaseSensitivity,
    ) -> bool {
        if let Some(number) = crate::has_adapter::number_of_class(name.as_ref()) {
            return self.0.has.borrow().contains(&number);
        }
        let line = self.0.tag == FIRST_LINE_TAG;
        if let Some(number) = crate::first_letter_adapter::number_of_class(name.as_ref(), line) {
            return self.0.fl.borrow().contains(&number);
        }
        self.0
            .classes
            .iter()
            .any(|class| case_sensitivity.eq(class.as_bytes(), name.as_ref().as_bytes()))
    }

    fn has_custom_state(&self, _name: &AtomIdent) -> bool {
        false
    }

    fn imported_part(&self, _name: &AtomIdent) -> Option<AtomIdent> {
        None
    }

    fn is_part(&self, _name: &AtomIdent) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.0.children.is_empty() && !self.0.has_text
    }

    fn is_root(&self) -> bool {
        self.0.parent.is_none()
    }

    fn add_element_unique_hashes(&self, _filter: &mut selectors::bloom::BloomFilter) -> bool {
        false
    }
}

impl<'a> TElement for StyloNode<'a> {
    type ConcreteNode = StyloNode<'a>;
    type TraversalChildrenIterator = std::vec::IntoIter<StyloNode<'a>>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn traversal_children(&self) -> LayoutIterator<Self::TraversalChildrenIterator> {
        let children: Vec<_> = self
            .0
            .children
            .iter()
            .map(|&ptr| StyloNode(unsafe { &*ptr }))
            .collect();
        LayoutIterator(children.into_iter())
    }

    fn is_html_element(&self) -> bool {
        true
    }

    fn is_mathml_element(&self) -> bool {
        false
    }

    fn is_svg_element(&self) -> bool {
        false
    }

    fn style_attribute(&self) -> Option<ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        self.0.style_pdb.as_ref().map(Arc::borrow_arc)
    }

    fn animation_rule(
        &self,
        _: &SharedStyleContext,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn transition_rule(
        &self,
        _: &SharedStyleContext,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    /// The state Stylo's style sharing tells nodes apart by: the node's own, and
    /// in the bits above it the number of its context (see [`NodeSlot::context`]),
    /// so that two nodes only share a style when the state of their ancestors is
    /// the same, which `.row:hover .cell` reads. Matching reads the state itself
    /// and does not see the number.
    fn state(&self) -> ElementState {
        ElementState::from_bits_retain(
            self.0.state.get().bits() | (u64::from(self.0.context.get()) << CONTEXT_SHIFT),
        )
    }

    fn has_part_attr(&self) -> bool {
        false
    }

    fn exports_any_part(&self) -> bool {
        false
    }

    fn id(&self) -> Option<&WeakAtom> {
        self.0.id_atom.as_ref()
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&AtomIdent),
    {
        for class in &self.0.classes {
            let atom = Atom::from(class.as_str());
            callback(AtomIdent::cast(&atom));
        }
        for number in self.0.has.borrow().iter() {
            let atom = Atom::from(format!("florui-has-{number}").as_str());
            callback(AtomIdent::cast(&atom));
        }
        let prefix = if self.0.tag == FIRST_LINE_TAG {
            "ll"
        } else {
            "fl"
        };
        for number in self.0.fl.borrow().iter() {
            let atom = Atom::from(format!("florui-{prefix}-{number}").as_str());
            callback(AtomIdent::cast(&atom));
        }
    }

    fn each_custom_state<F>(&self, _callback: F)
    where
        F: FnMut(&AtomIdent),
    {
    }

    fn each_attr_name<F>(&self, mut callback: F)
    where
        F: FnMut(&LocalName),
    {
        // Stylo's own fast-reject bloom filter is populated from this
        // before an attribute selector is even tried against
        // `attr_matches` -- leaving it empty (the previous stub) silently
        // made every `[attr]`/`[attr="v"]` selector never match at all,
        // not just less efficient.
        for (name, _) in &self.0.attrs {
            callback(&LocalName::from(name.as_str()));
        }
    }

    fn has_dirty_descendants(&self) -> bool {
        self.0.dirty_descendants.get()
    }

    fn has_snapshot(&self) -> bool {
        false
    }

    fn handled_snapshot(&self) -> bool {
        true
    }

    unsafe fn set_handled_snapshot(&self) {}

    unsafe fn set_dirty_descendants(&self) {
        self.0.dirty_descendants.set(true);
    }

    unsafe fn unset_dirty_descendants(&self) {
        self.0.dirty_descendants.set(false);
    }

    fn store_children_to_process(&self, _n: isize) {
        unimplemented!(
            "this bridge drives StyleResolverForElement directly, never the parallel/postorder \
             traversal driver that needs this"
        )
    }

    fn did_process_child(&self) -> isize {
        unimplemented!("see store_children_to_process")
    }

    unsafe fn ensure_data(&self) -> atomic_refcell::AtomicRefMut<'_, ElementData> {
        self.0.data.borrow_mut()
    }

    unsafe fn clear_data(&self) {
        *self.0.data.borrow_mut() = ElementData::default();
    }

    fn has_data(&self) -> bool {
        true
    }

    fn borrow_data(&self) -> Option<atomic_refcell::AtomicRef<'_, ElementData>> {
        Some(self.0.data.borrow())
    }

    fn mutate_data(&self) -> Option<atomic_refcell::AtomicRefMut<'_, ElementData>> {
        Some(self.0.data.borrow_mut())
    }

    fn skip_item_display_fixup(&self) -> bool {
        false
    }

    fn may_have_animations(&self) -> bool {
        // No per-element context available here to check for real; see
        // has_animations/has_css_animations/has_css_transitions for the
        // real check. Only consulted as a cheap early-out elsewhere in
        // Stylo (style sharing, an animation-declarations short circuit)
        // that this bridge doesn't use, so a conservative `true` costs
        // nothing but a skipped optimization.
        true
    }

    fn has_animations(&self, context: &SharedStyleContext) -> bool {
        let key = AnimationSetKey::new_for_non_pseudo(TNode::opaque(self));
        context
            .animations
            .sets
            .read()
            .get(&key)
            .is_some_and(|set| !set.animations.is_empty())
    }

    fn has_css_animations(
        &self,
        context: &SharedStyleContext,
        _pseudo_element: Option<PseudoElement>,
    ) -> bool {
        self.has_animations(context)
    }

    fn has_css_transitions(
        &self,
        context: &SharedStyleContext,
        _pseudo_element: Option<PseudoElement>,
    ) -> bool {
        let key = AnimationSetKey::new_for_non_pseudo(TNode::opaque(self));
        context
            .animations
            .sets
            .read()
            .get(&key)
            .is_some_and(|set| !set.transitions.is_empty())
    }

    fn shadow_root(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn containing_shadow(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn lang_attr(&self) -> Option<AttrValue> {
        None
    }

    fn match_element_lang(&self, _override_lang: Option<Option<AttrValue>>, _value: &Lang) -> bool {
        false
    }

    fn is_html_document_body_element(&self) -> bool {
        false
    }

    fn synthesize_presentational_hints_for_legacy_attributes<V>(
        &self,
        _visited_handling: VisitedHandlingMode,
        _hints: &mut V,
    ) where
        V: selectors::sink::Push<style::applicable_declarations::ApplicableDeclarationBlock>,
    {
    }

    fn local_name(&self) -> &BorrowedLocalName {
        &self.0.local_name
    }

    fn namespace(&self) -> &BorrowedNamespaceUrl {
        &self.0.namespace
    }

    fn query_container_size(
        &self,
        _display: &Display,
    ) -> euclid::Size2D<Option<app_units::Au>, euclid::UnknownUnit> {
        euclid::Size2D::new(None, None)
    }

    fn has_selector_flags(&self, _flags: ElementSelectorFlags) -> bool {
        false
    }

    fn relative_selector_search_direction(&self) -> ElementSelectorFlags {
        ElementSelectorFlags::empty()
    }
}

struct NoFontMetrics;

impl fmt::Debug for NoFontMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NoFontMetrics")
    }
}

impl FontMetricsProvider for NoFontMetrics {
    fn query_font_metrics(
        &self,
        _vertical: bool,
        _font: &FontStruct,
        _base_size: CSSPixelLength,
        _flags: style::values::specified::font::QueryFontMetricsFlags,
    ) -> FontMetrics {
        FontMetrics::default()
    }

    // Chromium's default sizes: 16px, and for a list that is exactly
    // `monospace` 13px. Keyword sizes (and so every `em` that follows from one)
    // are worked out from this base.
    fn base_size_for_generic(&self, generic: GenericFontFamily) -> Length {
        Length::new(if generic == GenericFontFamily::Monospace {
            13.0
        } else {
            16.0
        })
    }
}

struct NoPainters;
impl RegisteredSpeculativePainters for NoPainters {
    fn get(&self, _name: &Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

/// Every stylesheet this crate parses, and every cascade it drives, share
/// this one process-wide lock — Stylo ties a stylesheet's rules to the
/// exact `SharedRwLock` instance it was parsed under (a per-document
/// isolation mechanism this crate has no use for), so parsing once in
/// [`crate::stylesheet_parse::parse_stylesheet`] and cascading later in
/// [`compute`] need to agree on the same lock rather than each minting
/// their own.
pub(crate) fn shared_lock() -> &'static SharedRwLock {
    static LOCK: std::sync::LazyLock<SharedRwLock> = std::sync::LazyLock::new(SharedRwLock::new);
    &LOCK
}

/// Parses `css` (a `style="..."` attribute's own text — a plain
/// declaration list, never a selector) into a real `PropertyDeclarationBlock`
/// under this crate's one shared lock — the same fixed `about:florui` URL
/// and `NoQuirks` mode every other parse in this crate already uses (see
/// [`crate::stylesheet_parse::parse_str`]), and no error reporter: a
/// malformed inline declaration is dropped silently, the same as any other
/// unsupported declaration this crate's cascade already tolerates.
fn parse_inline_style(css: &str) -> Arc<Locked<PropertyDeclarationBlock>> {
    let css = crate::scroll_behavior_adapter::rewrite_declarations(css);
    let css = match crate::text_indent_adapter::rewrite_declarations(&css) {
        std::borrow::Cow::Owned(rewritten) => std::borrow::Cow::Owned(rewritten),
        std::borrow::Cow::Borrowed(_) => css,
    };
    let css = match crate::property_mirror::rewrite_declarations(&css) {
        std::borrow::Cow::Owned(rewritten) => std::borrow::Cow::Owned(rewritten),
        std::borrow::Cow::Borrowed(_) => css,
    };
    let url_data: style::stylesheets::UrlExtraData = url::Url::parse("about:florui")
        .expect("a fixed, valid URL literal")
        .into();
    let block = style::properties::parse_style_attribute(
        &css,
        &url_data,
        None,
        QuirksMode::NoQuirks,
        style::stylesheets::CssRuleType::Style,
    );
    Arc::new(shared_lock().wrap(block))
}

fn device(viewport: FlorViewport, prefers_color_scheme: PrefersColorScheme) -> Device {
    Device::new(
        MediaType::screen(),
        QuirksMode::NoQuirks,
        euclid::Size2D::new(viewport.width, viewport.height),
        euclid::Scale::new(1.0),
        Box::new(NoFontMetrics),
        ComputedValues::initial_values_with_font_override(FontStruct::initial_values()),
        prefers_color_scheme,
    )
}

/// Computes real Stylo styles for every node in `arena`, driving Stylo's
/// own selector matching, cascade, and inheritance via [`StyleResolverForElement`]
/// — this crate reimplements none of them. `viewport` is what `@media`'s
/// own size features resolve against. `container_query_signature` is the
/// flattened, in-order truth value of every `@container` block across
/// `rules` for *this one cascade* — see [`crate::container_query_adapter`]'s
/// own module doc; the caller (`florui_layout::compute_with_style`) is
/// responsible for resolving it per node and calling this once per distinct
/// signature. `timeline` carries `transition`/`@keyframes` state across
/// calls — see [`crate::animation`]'s module doc.
pub(crate) fn compute(
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: FlorViewport,
    timeline: &mut AnimationTimeline,
    container_query_signature: &[bool],
) -> StyleMap {
    style::thread_state::enter(style::thread_state::ThreadState::LAYOUT);
    let result = compute_in_layout_state(
        arena,
        rules,
        state,
        viewport,
        timeline,
        container_query_signature,
    );
    style::thread_state::exit(style::thread_state::ThreadState::LAYOUT);
    result
}

fn compute_in_layout_state(
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: FlorViewport,
    timeline: &mut AnimationTimeline,
    container_query_signature: &[bool],
) -> StyleMap {
    if arena.roots().is_empty() {
        return StyleMap::new();
    }
    let (tree, _primary_root) = StyloTree::new(arena, state, timeline);
    compute_with_tree(
        &tree,
        arena,
        rules,
        viewport,
        timeline,
        container_query_signature,
    )
}

/// The specificities of the selectors of `text`, or `None` when it does not
/// parse as a list of them.
pub(crate) fn parse_selector_list(text: &str) -> Option<Vec<u32>> {
    let url_data: style::stylesheets::UrlExtraData = url::Url::parse("about:florui")
        .expect("a fixed, valid URL literal")
        .into();
    let list =
        style::selector_parser::SelectorParser::parse_author_origin_no_namespace(text, &url_data)
            .ok()?;
    Some(list.slice().iter().map(|s| s.specificity()).collect())
}

/// Gives each first-letter and first-line node the selectors its owner matches, with
/// the engine's own selector matching, so the rules rewritten for them apply.
fn evaluate_first_letters(tree: &StyloTree, rules: &[Rule]) {
    evaluate_pseudo_nodes(
        tree,
        &tree.first_letters,
        rules
            .iter()
            .flat_map(|rule| rule.first_letter_numbers().iter().copied())
            .collect(),
    );
    evaluate_pseudo_nodes(
        tree,
        &tree.first_lines,
        rules
            .iter()
            .flat_map(|rule| rule.first_line_numbers().iter().copied())
            .collect(),
    );
}

/// [`evaluate_first_letters`] for the nodes `nodes` of one pseudo-element, whose rules
/// hold the selectors `numbers`.
fn evaluate_pseudo_nodes(tree: &StyloTree, nodes: &[usize], mut numbers: Vec<usize>) {
    if nodes.is_empty() {
        return;
    }
    numbers.sort_unstable();
    numbers.dedup();
    for &index in nodes {
        tree.slots[index].fl.borrow_mut().clear();
    }
    if numbers.is_empty() {
        return;
    }
    let url_data: style::stylesheets::UrlExtraData = url::Url::parse("about:florui")
        .expect("a fixed, valid URL literal")
        .into();
    let parsed: Vec<_> = numbers
        .iter()
        .filter_map(|&number| {
            let text = crate::first_letter_adapter::subject(number)?;
            let list = style::selector_parser::SelectorParser::parse_author_origin_no_namespace(
                &text, &url_data,
            )
            .ok()?;
            Some((number, list))
        })
        .collect();
    let mut caches = selectors::matching::SelectorCaches::default();
    for &index in nodes {
        let slot = &tree.slots[index];
        let Some(owner) = slot.fl_owner else {
            continue;
        };
        let owner = StyloNode(&tree.slots[owner]);
        let mut context = selectors::matching::MatchingContext::new(
            selectors::matching::MatchingMode::Normal,
            None,
            &mut caches,
            QuirksMode::NoQuirks,
            selectors::matching::NeedsSelectorFlags::No,
            selectors::matching::MatchingForInvalidation::No,
        );
        for (number, list) in &parsed {
            let matched = list.slice().iter().any(|selector| {
                selectors::matching::matches_selector(selector, 0, None, &owner, &mut context)
            });
            if matched {
                slot.fl.borrow_mut().push(*number);
            }
        }
    }
}

/// The values `values` cascade to when their parent has the values `parent` instead of
/// the ones it was styled under: the rules that matched are kept.
fn recascade(
    shared: &SharedStyleContext<'_>,
    element: StyloNode<'_>,
    pseudo: Option<&PseudoElement>,
    values: &ComputedValues,
    parent: &ComputedValues,
) -> Arc<ComputedValues> {
    let mut conditions = style::rule_cache::RuleCacheConditions::default();
    shared.stylist.cascade_style_and_visited(
        Some(element),
        pseudo,
        CascadeInputs::new_from_style(values),
        &shared.guards,
        Some(parent),
        Some(parent),
        style::properties::cascade::FirstLineReparenting::No,
        None,
        &mut conditions,
    )
}

/// What takes the style a node has on the first line.
type PutStyle<'a> = dyn FnMut(NodeId, &std::sync::Arc<ComputedStyle>, Option<&ComputedValues>) + 'a;

/// Gives the nodes on the first line of each block that a `::first-line` rule matches
/// (`lines` are the slots of the nodes of those first lines) the style they have there,
/// as `first_line` of the style they have elsewhere: the block gets the style of its line
/// and what flows in the line inherits from that. So does a block that is the first thing
/// in a block, as the first line of the outer one is its first line. `style_of` is the
/// style a node has elsewhere, `out` takes the nodes whose style is to be replaced.
fn first_line_styles(
    tree: &StyloTree,
    arena: &Arena,
    shared: &SharedStyleContext<'_>,
    lines: &[usize],
    style_of: &dyn Fn(NodeId) -> Option<std::sync::Arc<ComputedStyle>>,
    mut out: impl FnMut(NodeId, std::sync::Arc<ComputedStyle>),
) {
    use FlorDisplay as D;

    let viewport = shared.stylist.device().au_viewport_size();
    let viewport_px = (viewport.width.to_f32_px(), viewport.height.to_f32_px());
    let mut put =
        |id: NodeId, normal: &std::sync::Arc<ComputedStyle>, alt: Option<&ComputedValues>| {
            if alt.is_none() && normal.first_line.is_none() {
                return;
            }
            let mut style = (**normal).clone();
            style.first_line =
                alt.map(|values| std::sync::Arc::new(to_computed_style(values, viewport_px)));
            out(id, std::sync::Arc::new(style));
        };
    let is_block_container = |display: D| {
        matches!(
            display,
            D::Block | D::FlowRoot | D::InlineBlock | D::ListItem | D::TableCell
        )
    };
    // The first lines taken in by a block above them.
    let mut done: Vec<usize> = Vec::new();
    for &line in lines {
        if done.contains(&line) {
            continue;
        }
        let line_slot = &tree.slots[line];
        let Some(owner) = line_slot.fl_owner else {
            continue;
        };
        let owner_id = tree.order[owner];
        let owner_style = style_of(owner_id);
        let applies = !line_slot.fl.borrow().is_empty()
            && owner_style
                .as_ref()
                .is_some_and(|style| is_block_container(style.display));
        let line_values = line_slot.data.borrow().styles.primary.clone();
        // The block, then everything below it: what the line reaches gets a style, and
        // what it does not loses the one an earlier cascade gave it.
        let mut stack: Vec<(usize, Option<Arc<ComputedValues>>, bool)> = Vec::new();
        if let Some(normal) = &owner_style {
            let alt = if applies { line_values } else { None };
            put(owner_id, normal, alt.as_deref());
            stack.push((owner, alt, true));
        }
        while let Some((at, alt, block)) = stack.pop() {
            let node = tree.order[at];
            let boxes = tree.generated.get(at).copied().unwrap_or([None, None]);
            // Whether the lines of the block have started: text, or an inline, comes before.
            let mut started = false;
            let inline_box = |position: usize, started: &mut bool, put: &mut PutStyle| {
                let Some(boxed) = boxes[position] else {
                    return;
                };
                let Some(normal) = style_of(boxed) else {
                    return;
                };
                let values = tree.slots[at].pseudo_values.borrow()[position].clone();
                let reached = match (&alt, values) {
                    (Some(alt), Some(values)) if normal.display == D::Inline => {
                        let pseudo = if position == 0 {
                            PseudoElement::Before
                        } else {
                            PseudoElement::After
                        };
                        Some(recascade(
                            shared,
                            StyloNode(&tree.slots[at]),
                            Some(&pseudo),
                            &values,
                            alt,
                        ))
                    }
                    _ => None,
                };
                put(boxed, &normal, reached.as_deref());
                if normal.display == D::Inline && normal.content.generates_box() {
                    *started = true;
                }
                // The first letter in the box inherits from it.
                for item in arena.inline_items(boxed) {
                    let crate::tree::InlineItem::Element(letter) = item else {
                        continue;
                    };
                    if arena.generated(*letter) != Some(crate::Pseudo::FirstLetter) {
                        continue;
                    }
                    let slot = tree.index_of[*letter];
                    let Some(letter_style) = style_of(*letter) else {
                        continue;
                    };
                    let values = tree.slots[slot].data.borrow().styles.primary.clone();
                    let reached = match (&reached, values) {
                        (Some(alt), Some(values)) if letter_style.display == D::Inline => Some(
                            recascade(shared, StyloNode(&tree.slots[slot]), None, &values, alt),
                        ),
                        _ => None,
                    };
                    put(*letter, &letter_style, reached.as_deref());
                }
            };
            inline_box(0, &mut started, &mut put);
            for item in arena.inline_items(node) {
                let child = match item {
                    crate::tree::InlineItem::Text(text) => {
                        started |= !text.trim().is_empty();
                        continue;
                    }
                    crate::tree::InlineItem::Element(child) => *child,
                };
                // The boxes of `::before` and `::after` are dealt with apart.
                if matches!(
                    arena.generated(child),
                    Some(crate::Pseudo::Before | crate::Pseudo::After)
                ) {
                    continue;
                }
                let slot = tree.index_of[child];
                if slot == usize::MAX {
                    continue;
                }
                let Some(normal) = style_of(child) else {
                    continue;
                };
                let out_of_flow = normal.float != crate::cascade::Float::None
                    || matches!(
                        normal.position,
                        crate::cascade::Position::Absolute | crate::cascade::Position::Fixed
                    );
                let values = tree.slots[slot].data.borrow().styles.primary.clone();
                let mut reached: Option<Arc<ComputedValues>> = None;
                let mut child_block = false;
                match normal.display {
                    D::None => {}
                    _ if out_of_flow => {}
                    D::Inline | D::Contents => {
                        started = true;
                        if let (Some(alt), Some(values)) = (&alt, &values) {
                            reached = Some(recascade(
                                shared,
                                StyloNode(&tree.slots[slot]),
                                None,
                                values,
                                alt,
                            ));
                        }
                    }
                    D::Block | D::ListItem if block && !started => {
                        // The first line of the block above is the first line of this one.
                        started = true;
                        child_block = true;
                        if let (Some(alt), Some(values)) = (&alt, &values) {
                            let base =
                                recascade(shared, StyloNode(&tree.slots[slot]), None, values, alt);
                            // With a first line of its own, which has what is in the first
                            // line above for a parent.
                            let own = tree
                                .first_lines
                                .iter()
                                .copied()
                                .find(|&own| tree.slots[own].fl_owner == Some(slot));
                            reached = Some(match own {
                                Some(own) if !tree.slots[own].fl.borrow().is_empty() => {
                                    done.push(own);
                                    let own_values =
                                        tree.slots[own].data.borrow().styles.primary.clone();
                                    match own_values {
                                        Some(own_values) => recascade(
                                            shared,
                                            StyloNode(&tree.slots[own]),
                                            None,
                                            &own_values,
                                            &base,
                                        ),
                                        None => base,
                                    }
                                }
                                _ => base,
                            });
                        }
                    }
                    _ => started = true,
                }
                put(child, &normal, reached.as_deref());
                stack.push((slot, reached, child_block));
            }
            inline_box(1, &mut started, &mut put);
        }
    }
}

/// Gives each node the `:has()` it has: for every one the rules hold, the
/// arguments are matched against the nodes below the node (or after it, and
/// below them) with the engine's own selector matching, `:scope` being the node.
/// The result is read back as the classes the rules were rewritten to.
fn evaluate_has(tree: &StyloTree, rules: &[Rule]) -> bool {
    let mut numbers: Vec<usize> = rules
        .iter()
        .flat_map(|rule| rule.has_specs().iter().copied())
        .collect();
    numbers.sort_unstable();
    numbers.dedup();
    if numbers.is_empty() && !tree.has_given.get() {
        return false;
    }
    for slot in &tree.slots {
        slot.has.borrow_mut().clear();
    }
    tree.has_given.set(false);
    if numbers.is_empty() {
        return true;
    }
    let url_data: style::stylesheets::UrlExtraData = url::Url::parse("about:florui")
        .expect("a fixed, valid URL literal")
        .into();
    let mut caches = selectors::matching::SelectorCaches::default();
    for number in numbers {
        let Some(spec) = crate::has_adapter::spec(number) else {
            continue;
        };
        let parsed: Vec<_> = spec
            .relative
            .iter()
            .filter_map(|relative| {
                let list =
                    style::selector_parser::SelectorParser::parse_author_origin_no_namespace(
                        &relative.selector,
                        &url_data,
                    )
                    .ok()?;
                Some((list, relative.siblings))
            })
            .collect();
        for slot in &tree.slots {
            let mut context = selectors::matching::MatchingContext::new(
                selectors::matching::MatchingMode::Normal,
                None,
                &mut caches,
                QuirksMode::NoQuirks,
                selectors::matching::NeedsSelectorFlags::No,
                selectors::matching::MatchingForInvalidation::No,
            );
            context.scope_element = Some(OpaqueElement::new(slot));
            let found = parsed.iter().any(|(list, siblings)| {
                let mut matches = |candidate: &NodeSlot| {
                    let node = StyloNode(candidate);
                    list.slice().iter().any(|selector| {
                        selectors::matching::matches_selector(
                            selector,
                            0,
                            None,
                            &node,
                            &mut context,
                        )
                    })
                };
                if *siblings {
                    // SAFETY: the pointers of a tree point into its own slots.
                    let Some(parent) = slot.parent.map(|p| unsafe { &*p }) else {
                        return false;
                    };
                    parent.children[slot.sibling_index + 1..]
                        .iter()
                        .any(|&sibling| any_in_subtree(unsafe { &*sibling }, &mut matches))
                } else {
                    slot.children
                        .iter()
                        .any(|&child| any_in_subtree(unsafe { &*child }, &mut matches))
                }
            });
            if found {
                slot.has.borrow_mut().push(number);
                tree.has_given.set(true);
            }
        }
    }
    true
}

/// Whether `test` holds for `root` or a node below it.
fn any_in_subtree(root: &NodeSlot, test: &mut impl FnMut(&NodeSlot) -> bool) -> bool {
    let mut stack = vec![root];
    while let Some(slot) = stack.pop() {
        if test(slot) {
            return true;
        }
        // SAFETY: the pointers of a tree point into its own slots.
        stack.extend(slot.children.iter().map(|&child| unsafe { &*child }));
    }
    false
}

/// The cascade over a tree already built and set to the state to style it in.
pub(crate) fn compute_with_tree(
    tree: &StyloTree,
    arena: &Arena,
    rules: &[Rule],
    viewport: FlorViewport,
    timeline: &mut AnimationTimeline,
    container_query_signature: &[bool],
) -> StyleMap {
    // One entry per node: growing a table of styles this big copies every
    // entry it already holds each time it doubles.
    if evaluate_has(tree, rules) {
        // The classes a node has can tell two nodes apart that the numbering took
        // for the same.
        tree.number_contexts();
    }
    // After the `:has()` classes, which the selectors of a first letter or line may hold.
    evaluate_first_letters(tree, rules);
    let mut result = StyleMap::with_capacity(tree.order.len());
    with_style_context(
        tree,
        rules,
        viewport,
        timeline,
        container_query_signature,
        |shared, thread_local, timeline| {
            // Converting a node's computed values is the same work for every
            // node that shares them, so it is done once per distinct set of
            // values. Every key is an `Arc` still held by its node's
            // `ElementData`, so no address is reused while the map is alive.
            let mut hidden = HiddenStyles::new();
            let mut converted = Converted::new();
            let mut fields = FieldStyles::default();
            // Level by level, so an ancestor's style is already committed to
            // its `ElementData` when a descendant resolves and Stylo's style
            // sharing cache sees every node of one depth together. One bloom
            // filter is kept across the whole walk instead of
            // `resolve_style`'s per-call `rebuild`, which re-hashes every
            // ancestor and made deep chains quadratic.
            for id in tree.level_order() {
                let styled = style_node(
                    shared,
                    thread_local,
                    tree.node(id),
                    timeline,
                    &mut converted,
                );
                generated_styles(
                    tree,
                    id,
                    &styled,
                    shared,
                    &mut converted,
                    &mut hidden,
                    |node, style| {
                        result.insert_shared(node, style);
                    },
                );
                let style = with_field_pseudos(&styled, shared, &mut converted, &mut fields);
                result.insert_shared(id, style);
            }
            let mut replaced = Vec::new();
            first_line_styles(
                tree,
                arena,
                shared,
                &tree.first_lines,
                &|id| result.get_shared(&id).cloned(),
                |id, style| replaced.push((id, style)),
            );
            for (id, style) in replaced {
                result.insert_shared(id, style);
            }
        },
    );
    timeline.sweep();
    result
}

/// Styles again `start` (slots of `tree`) and, below each node whose new values
/// can change what its children inherit, the children, in the order a cascade
/// would (a parent before its children). The other nodes keep what the last
/// cascade left in them, which is what a node reads of its parent. Returns the
/// new style of each node styled. The caller marks the nodes it did not style
/// as met on the timeline.
pub(crate) fn restyle_nodes(
    tree: &StyloTree,
    arena: &Arena,
    start: &[usize],
    rules: &[Rule],
    viewport: FlorViewport,
    timeline: &mut AnimationTimeline,
) -> Vec<(NodeId, std::sync::Arc<ComputedStyle>)> {
    with_style_context(
        tree,
        rules,
        viewport,
        timeline,
        &[],
        |shared, thread_local, timeline| {
            let mut hidden = HiddenStyles::new();
            let mut converted = Converted::new();
            let mut fields = FieldStyles::default();
            let mut work: std::collections::BTreeSet<(usize, usize)> = start
                .iter()
                .map(|&index| (tree.slots[index].depth, index))
                .collect();
            let mut styled = Vec::with_capacity(work.len());
            while let Some((_, index)) = work.pop_first() {
                let slot = &tree.slots[index];
                *slot.data.borrow_mut() = ElementData::default();
                let id = tree.order[index];
                let node = style_node(
                    shared,
                    thread_local,
                    StyloNode(slot),
                    timeline,
                    &mut converted,
                );
                let reaches_children = node
                    .previous
                    .as_ref()
                    .is_none_or(|old| may_change_children(old, &node.primary));
                if reaches_children {
                    for &child in arena.element_child_ids(id).iter() {
                        let child_index = tree.index_of[child];
                        work.insert((tree.slots[child_index].depth, child_index));
                    }
                }
                // The first letters in the node inherit from it.
                if arena.has_first_letters() {
                    let in_box = arena
                        .generated_child(id, crate::Pseudo::Before)
                        .into_iter()
                        .flat_map(|before| arena.first_letter_children(before));
                    for child in arena.first_letter_children(id).chain(in_box) {
                        let child_index = tree.index_of[child];
                        work.insert((tree.slots[child_index].depth, child_index));
                    }
                }
                generated_styles(
                    tree,
                    id,
                    &node,
                    shared,
                    &mut converted,
                    &mut hidden,
                    |generated, style| {
                        styled.push((generated, style));
                    },
                );
                let style = with_field_pseudos(&node, shared, &mut converted, &mut fields);
                styled.push((id, style));
            }
            styled
        },
    )
}

/// Gives again the first-line styles of the blocks `lines` (slots of first-line nodes)
/// hold, in `styles`, after the nodes below them were styled again. Returns the nodes
/// whose style changed.
pub(crate) fn restyle_first_lines(
    tree: &StyloTree,
    arena: &Arena,
    styles: &mut StyleMap,
    lines: &[usize],
    rules: &[Rule],
    viewport: FlorViewport,
    timeline: &mut AnimationTimeline,
) -> Vec<NodeId> {
    let mut replaced = Vec::new();
    with_style_context(tree, rules, viewport, timeline, &[], |shared, _, _| {
        first_line_styles(
            tree,
            arena,
            shared,
            lines,
            &|id| styles.get_shared(&id).cloned(),
            |id, style| replaced.push((id, style)),
        );
    });
    let mut nodes = Vec::with_capacity(replaced.len());
    for (id, style) in replaced {
        nodes.push(id);
        styles.insert_shared(id, style);
    }
    nodes
}

/// Whether a node whose style was `old` and is now `new` may have changed what
/// its children inherit: anything but equal inherited structs (Stylo shares each
/// between a parent and a child that inherits it unchanged, so an equal pointer
/// is the common case), the same custom properties, writing mode and zoom.
/// When it says yes the children are styled again, which costs and never gives a
/// wrong style.
fn may_change_children(old: &ComputedValues, new: &ComputedValues) -> bool {
    fn same<T: PartialEq>(a: &T, b: &T) -> bool {
        std::ptr::eq(a, b) || a == b
    }
    !(same(old.get_font(), new.get_font())
        && same(old.get_inherited_box(), new.get_inherited_box())
        && same(old.get_inherited_table(), new.get_inherited_table())
        && same(old.get_inherited_text(), new.get_inherited_text())
        && same(old.get_inherited_ui(), new.get_inherited_ui())
        && same(old.get_list(), new.get_list())
        && same(old.get_svg(), new.get_svg())
        && old.custom_properties_equal(new)
        && old.writing_mode == new.writing_mode
        && old.effective_zoom == new.effective_zoom)
}

/// Converted styles by the cascaded values they came from.
type Converted = HashMap<*const ComputedValues, std::sync::Arc<ComputedStyle>>;

/// The style of a `::before` or `::after` that has no box, by the style of the node it
/// belongs to.
#[derive(Default)]
struct HiddenStyles {
    hidden: HashMap<*const ComputedStyle, std::sync::Arc<ComputedStyle>>,
    /// The values the styles of the pseudo-elements were converted from, kept so no
    /// address `converted` is keyed by is reused while it is alive.
    kept: Vec<Arc<ComputedValues>>,
    /// The styles `hidden` is keyed by.
    parents: Vec<std::sync::Arc<ComputedStyle>>,
}

impl HiddenStyles {
    fn new() -> Self {
        Self::default()
    }
}

/// The styles of the `::before` and `::after` boxes of `id`, which come from the
/// values the cascade gave its pseudo-elements. One with no `content` has no box.
fn generated_styles(
    tree: &StyloTree,
    id: NodeId,
    styled: &Styled,
    shared: &SharedStyleContext<'_>,
    converted: &mut Converted,
    hidden: &mut HiddenStyles,
    mut out: impl FnMut(NodeId, std::sync::Arc<ComputedStyle>),
) {
    let Some(boxes) = tree.generated.get(tree.index_of[id]) else {
        return;
    };
    let viewport = shared.stylist.device().au_viewport_size();
    let viewport_px = (viewport.width.to_f32_px(), viewport.height.to_f32_px());
    for (position, node) in boxes.iter().enumerate() {
        let Some(node) = *node else {
            continue;
        };
        let own = styled.pseudos[position].as_ref().map(|values| {
            hidden.kept.push(Arc::clone(values));
            std::sync::Arc::clone(
                converted
                    .entry(&**values as *const ComputedValues)
                    .or_insert_with(|| std::sync::Arc::new(to_computed_style(values, viewport_px))),
            )
        });
        let style = match own {
            Some(style) if style.content.generates_box() => style,
            _ => {
                let key = std::sync::Arc::as_ptr(&styled.style);
                if !hidden.hidden.contains_key(&key) {
                    hidden.parents.push(std::sync::Arc::clone(&styled.style));
                    hidden.hidden.insert(
                        key,
                        std::sync::Arc::new(ComputedStyle {
                            display: FlorDisplay::None,
                            ..(*styled.style).clone()
                        }),
                    );
                }
                std::sync::Arc::clone(&hidden.hidden[&key])
            }
        };
        out(node, style);
    }
}

/// What styling one node gave.
struct Styled {
    /// Its style, the same one for every node whose values are the same.
    style: std::sync::Arc<ComputedStyle>,
    /// The cascaded values the timeline had for the node before this, if any.
    previous: Option<Arc<ComputedValues>>,
    /// The cascaded values now.
    primary: Arc<ComputedValues>,
    /// The values of the node's `::before` and `::after`, where a rule gave it one.
    pseudos: [Option<Arc<ComputedValues>>; 2],
    /// For a text field, the values of its `::placeholder` (which the rules are
    /// rewritten to put on the `::before` of a field, see
    /// [`crate::placeholder_adapter`]) and of its `::selection`.
    field_pseudos: [Option<Arc<ComputedValues>>; 2],
}

/// The styles of text fields that carry the style of their `::placeholder` and
/// `::selection`, by the cascaded values of the three, and what keeps those
/// addresses from being reused while the map is alive.
#[derive(Default)]
struct FieldStyles {
    styles: HashMap<(usize, usize, usize), std::sync::Arc<ComputedStyle>>,
    kept: Vec<Arc<ComputedValues>>,
}

/// `styled.style`, with the style of the `::placeholder` and the `::selection` of
/// a text field in it.
fn with_field_pseudos(
    styled: &Styled,
    shared: &SharedStyleContext<'_>,
    converted: &mut Converted,
    fields: &mut FieldStyles,
) -> std::sync::Arc<ComputedStyle> {
    if styled.field_pseudos.iter().all(Option::is_none) {
        return std::sync::Arc::clone(&styled.style);
    }
    let address = |values: &Option<Arc<ComputedValues>>| {
        values
            .as_ref()
            .map_or(0, |values| &**values as *const ComputedValues as usize)
    };
    let key = (
        &*styled.primary as *const ComputedValues as usize,
        address(&styled.field_pseudos[0]),
        address(&styled.field_pseudos[1]),
    );
    if let Some(style) = fields.styles.get(&key) {
        return std::sync::Arc::clone(style);
    }
    let viewport = shared.stylist.device().au_viewport_size();
    let viewport_px = (viewport.width.to_f32_px(), viewport.height.to_f32_px());
    let mut convert = |values: &Option<Arc<ComputedValues>>| {
        values.as_ref().map(|values| {
            fields.kept.push(Arc::clone(values));
            std::sync::Arc::clone(
                converted
                    .entry(&**values as *const ComputedValues)
                    .or_insert_with(|| std::sync::Arc::new(to_computed_style(values, viewport_px))),
            )
        })
    };
    let placeholder = convert(&styled.field_pseudos[0]);
    let selection = convert(&styled.field_pseudos[1]);
    fields.kept.push(Arc::clone(&styled.primary));
    let mut style = (*styled.style).clone();
    style.placeholder = placeholder;
    style.selection = selection;
    let style = std::sync::Arc::new(style);
    fields.styles.insert(key, std::sync::Arc::clone(&style));
    style
}

/// Runs `walk` with what the cascade of `tree` needs: a stylist holding the
/// sheets, the shared style context over it and the context of one thread.
fn with_style_context<'t, R>(
    tree: &'t StyloTree,
    rules: &[Rule],
    viewport: FlorViewport,
    timeline: &mut AnimationTimeline,
    container_query_signature: &[bool],
    walk: impl FnOnce(
        &SharedStyleContext<'_>,
        &mut ThreadLocalStyleContext<StyloNode<'t>>,
        &mut AnimationTimeline,
    ) -> R,
) -> R {
    let _counter_styles = crate::counter_style::install(crate::counter_style::collect(rules));
    let prefers_color_scheme = if timeline.prefers_dark_color_scheme() {
        PrefersColorScheme::Dark
    } else {
        PrefersColorScheme::Light
    };
    let mut stylist = Stylist::new(device(viewport, prefers_color_scheme), QuirksMode::NoQuirks);
    let lock = shared_lock();
    // The framework's own default element stylesheet first, under
    // Origin::UserAgent — Stylo's real cascade-origin precedence means an
    // application rule below overrides it regardless of specificity or
    // this registration order, the same as a real browser's UA stylesheet.
    // The default stylesheet never contains a `@container` block of its
    // own, so it always gets an empty slice regardless of this call's own
    // signature.
    let default_rule = crate::default_stylesheet::rule();
    stylist.append_stylesheet(
        DocumentStyleSheet(default_rule.stylesheet(
            viewport.height,
            &[],
            timeline.prefers_reduced_motion(),
        )),
        &lock.read(),
    );
    if tree.uses_inline_scroll_behavior || rules.iter().any(Rule::uses_scroll_behavior) {
        stylist.append_stylesheet(
            DocumentStyleSheet(
                crate::default_stylesheet::scroll_behavior_reset_rule().stylesheet(
                    viewport.height,
                    &[],
                    timeline.prefers_reduced_motion(),
                ),
            ),
            &lock.read(),
        );
    }
    // `container_query_signature` is one flat, in-order slice spanning
    // every rule's own `@container` blocks — each rule here only reads the
    // sub-slice its own `container_query_blocks()` contributed. Shorter
    // than that (an empty slice from `compute`'s own "treat everything as
    // non-matching" convenience wrapper included) is not an error: any
    // block past the end of what the caller provided is simply treated as
    // non-matching, same as [`crate::container_query_adapter::find_container`]
    // returning `None`.
    let mut signature_offset = 0;
    for rule in rules {
        let block_count = rule.container_query_blocks().len();
        let rule_signature: Vec<bool> = (signature_offset..signature_offset + block_count)
            .map(|i| container_query_signature.get(i).copied().unwrap_or(false))
            .collect();
        signature_offset += block_count;
        stylist.append_stylesheet(
            DocumentStyleSheet(rule.stylesheet(
                viewport.height,
                &rule_signature,
                timeline.prefers_reduced_motion(),
            )),
            &lock.read(),
        );
    }

    let guard = lock.read();
    let guards = StylesheetGuards {
        author: &guard,
        ua_or_user: &guard,
    };
    stylist.flush::<StyloNode<'_>>(&guards, None, None);

    let snapshot_map = style::servo::selector_parser::SnapshotMap::new();
    // Cloning a `DocumentAnimationSet` clones the `Arc<RwLock<_>>` handle,
    // not the map it wraps — `timeline` and `shared.animations` back onto
    // the exact same state for this call, and whatever this call leaves in
    // it (a started/updated/finished transition) is what `timeline` still
    // holds once this function returns.
    let animations = timeline.sets.clone();
    let shared = SharedStyleContext {
        traversal_flags: TraversalFlags::empty(),
        stylist: &stylist,
        options: GLOBAL_STYLE_DATA.options.clone(),
        guards,
        visited_styles_enabled: false,
        animations,
        current_time_for_animations: timeline.now,
        snapshot_map: &snapshot_map,
        registered_speculative_painters: &NoPainters,
    };
    let mut thread_local = ThreadLocalStyleContext::<StyloNode<'t>>::new();
    walk(&shared, &mut thread_local, timeline)
}

/// Styles one node of a tree whose ancestors already have a committed primary
/// style.
fn style_node<'t>(
    shared: &SharedStyleContext<'_>,
    thread_local: &mut ThreadLocalStyleContext<StyloNode<'t>>,
    target: StyloNode<'t>,
    timeline: &mut AnimationTimeline,
    converted: &mut Converted,
) -> Styled {
    let mut context = StyleContext {
        shared,
        thread_local,
    };
    let styles = resolve_sharing_styles(&mut context, target);
    let field_pseudos = if matches!(target.0.tag, "input" | "textarea") {
        [
            styles.pseudos.get(&PseudoElement::Before).cloned(),
            styles.pseudos.get(&PseudoElement::Selection).cloned(),
        ]
    } else {
        [None, None]
    };
    let pseudos = [
        styles.pseudos.get(&PseudoElement::Before).cloned(),
        styles.pseudos.get(&PseudoElement::After).cloned(),
    ];
    // What a first letter in the box of the `::before` inherits from.
    *target.0.pseudo_values.borrow_mut() = pseudos.clone();
    let viewport = shared.stylist.device().au_viewport_size();
    let viewport_px = (viewport.width.to_f32_px(), viewport.height.to_f32_px());

    // Nothing here starts or samples a transition/animation; real Servo
    // does that in a step its traversal driver runs after cascading
    // (private to Stylo), so it is replicated by hand for this node.
    let stable_id = target.0.stable_id;
    let primary = styles.primary().clone();
    // Keyframe resolution reads the parent style from `ElementData` and
    // panics if it is unpopulated, so commit it before animating.
    if let Some(mut data) = target.mutate_data() {
        data.styles.primary = Some(primary.clone());
    }
    let old_values = timeline.previous_style(stable_id);
    let has_active_animation =
        process_animations_for_style(target, &mut context, &old_values, &primary);
    // `process_animations_for_style` still runs unconditionally above —
    // its own `animation_set` bookkeeping (registering, updating,
    // pruning a transition/animation) stays fully consistent regardless
    // of suppression, so nothing needs special handling for when
    // suppression later turns back off. Only which value actually
    // renders changes here: suppressed means the plain cascaded value,
    // un-interpolated, every frame — not "duration forced near zero."
    let final_values = if has_active_animation && !timeline.should_suppress_animations() {
        splice_animation_declarations(target, &mut context, &primary, timeline.now)
    } else {
        primary.clone()
    };

    let style = if has_active_animation && !timeline.should_suppress_animations() {
        std::sync::Arc::new(to_computed_style(&final_values, viewport_px))
    } else {
        std::sync::Arc::clone(
            converted
                .entry(&*primary as *const ComputedValues)
                .or_insert_with(|| {
                    std::sync::Arc::new(to_computed_style(&final_values, viewport_px))
                }),
        )
    };
    // Elements that draw something of their own have no content to leave behind
    // when they generate no box: `display: contents` on one acts as `none`.
    let style = if style.display == FlorDisplay::Contents
        && matches!(
            target.0.tag,
            "img" | "icon" | "svg" | "textarea" | "select" | "input" | "br" | "hr"
        ) {
        std::sync::Arc::new(ComputedStyle {
            display: FlorDisplay::None,
            ..(*style).clone()
        })
    } else {
        style
    };
    // `primary` (the raw cascade result), not `final_values` (already
    // spliced with any in-progress transition/animation) -- Stylo's own
    // `update_transitions_for_new_style` compares next frame's `primary`
    // against *this* value to decide whether a transitionable property
    // actually changed. Feeding it back the already-animated midpoint
    // instead makes every still-converging frame look like a fresh
    // change, since the animated value never quite equals the settled
    // target -- restarting the transition from scratch every frame
    // forever instead of continuing the one already running, and
    // leaving `animation_set.transitions` growing without bound (each
    // fresh start is `AnimationState::Running`, never `Finished`, so
    // `process_animations_for_style`'s own `retain` never prunes it).
    timeline.set_current_style(stable_id, primary.clone());
    Styled {
        style,
        previous: old_values,
        primary,
        field_pseudos,
        pseudos,
    }
}

/// Stylo's `resolve_style` body for a node whose ancestors already have a committed
/// primary style, keeping `context`'s bloom filter across calls.
fn resolve_sharing_styles<'a>(
    context: &mut StyleContext<'_, StyloNode<'a>>,
    element: StyloNode<'a>,
) -> ElementStyles {
    context
        .thread_local
        .bloom_filter
        .insert_parents_recovering(element, element.0.depth);

    let parent: Option<StyloNode<'a>> = TElement::traversal_parent(&element);
    let mut parent_style = parent.and_then(|p| p.borrow_data().map(|d| d.styles.primary().clone()));
    if element.0.fl_in_box
        && let Some(before) = parent.and_then(|p| p.0.pseudo_values.borrow()[0].clone())
    {
        parent_style = Some(before);
    }
    let mut layout_parent = parent;
    let mut layout_parent_style = parent_style.clone();
    while let Some(style) = &layout_parent_style {
        if !style.is_display_contents() {
            break;
        }
        layout_parent = layout_parent.and_then(|p| TElement::traversal_parent(&p));
        layout_parent_style =
            layout_parent.and_then(|p| p.borrow_data().map(|d| d.styles.primary().clone()));
    }

    // A sibling or cousin already styled with the same matching rules and the
    // same parent style is Stylo's own shortcut for a long list of alike rows.
    // It is skipped under a `display: contents` parent, where the style the
    // children inherit is not the parent's own.
    let layout_parent_is_parent = match (&parent_style, &layout_parent_style) {
        (Some(parent), Some(layout)) => Arc::ptr_eq(parent, layout),
        (None, None) => true,
        _ => false,
    };
    let may_share = layout_parent_is_parent
        && !element.0.fl_in_box
        && element.0.context.get() != OVERFLOW_CONTEXT;
    let mut target = StyleSharingTarget::new(element);
    if may_share && let Some(shared) = target.share_style_if_possible(context) {
        return shared.into();
    }
    let resolved = StyleResolverForElement::new(
        element,
        context,
        RuleInclusion::All,
        PseudoElementResolution::Force,
    )
    .resolve_style(parent_style.as_deref(), layout_parent_style.as_deref());
    if may_share {
        context.thread_local.sharing_cache.insert_if_possible(
            &element,
            &resolved.primary,
            Some(&mut target),
            element.0.depth,
            context.shared,
        );
    }
    resolved.into()
}

/// Starts, updates, and samples `target`'s transitions/`@keyframes`
/// animations against its previous and new cascaded style — a by-hand
/// reimplementation of `servo/matching.rs`'s own
/// `process_animations_for_style` (private to Stylo, part of a
/// crate-private trait `resolve_style`'s point-query API doesn't drive).
/// Returns whether `target` has anything active to sample at all —
/// unrelated to whether this particular call started, changed, or ended
/// one: an already-running transition/animation needs its value re-spliced
/// (see [`splice_animation_declarations`]) on every call it's still
/// active for, not only the call it started or last changed on.
fn process_animations_for_style<'n>(
    target: StyloNode<'n>,
    context: &mut StyleContext<'_, StyloNode<'n>>,
    old_values: &Option<Arc<ComputedValues>>,
    new_values: &Arc<ComputedValues>,
) -> bool {
    let needs_animations_update =
        needs_animations_update(context, target, old_values.as_deref(), new_values);
    let might_need_transitions_update =
        might_need_transitions_update(context, target, old_values.as_deref(), new_values);

    let after_change_style = if might_need_transitions_update {
        StyleResolverForElement::new(
            target,
            context,
            RuleInclusion::All,
            PseudoElementResolution::IfApplicable,
        )
        .after_change_style(new_values)
    } else {
        None
    };

    let key = AnimationSetKey::new_for_non_pseudo(TNode::opaque(&target));
    let shared = context.shared;
    let mut animation_set = shared
        .animations
        .sets
        .write()
        .remove(&key)
        .unwrap_or_default();

    if needs_animations_update {
        let mut resolver = StyleResolverForElement::new(
            target,
            context,
            RuleInclusion::All,
            PseudoElementResolution::IfApplicable,
        );
        animation_set.update_animations_for_new_style::<StyloNode<'_>>(
            target,
            shared,
            new_values,
            &mut resolver,
        );
    }

    animation_set.update_transitions_for_new_style(
        might_need_transitions_update,
        shared,
        old_values.as_ref(),
        after_change_style.as_ref().unwrap_or(new_values),
    );

    animation_set
        .transitions
        .retain(|transition| transition.state != AnimationState::Finished);
    animation_set
        .animations
        .retain(|animation| animation.state != AnimationState::Finished);
    // `update_transitions_for_new_style`/`update_animations_for_new_style`
    // above cancel plenty of entries (a reversed transition, a property
    // dropped from `transition-property`, a `@keyframes` no longer
    // referenced) by setting `AnimationState::Canceled`, not by removing
    // them -- real Servo's own traversal driver sweeps those in a
    // separate `update_animations` task this bridge has no equivalent of,
    // so without this the two `retain`s above (which only ever look for
    // `Finished`) never see a `Canceled` entry leave, and it stays in
    // `animation_set` forever, one more former transition every frame it
    // keeps getting re-canceled.
    animation_set.clear_canceled_animations();

    // `dirty` only means "the active set itself changed shape this call"
    // (one started, finished, or got canceled) — real per-frame sampling
    // of an already-running transition/animation needs to happen on
    // every call it's still active for, not just the call it started or
    // changed on, so the splice is driven by non-emptiness here, not
    // `dirty`.
    let needs_splice = !animation_set.is_empty();
    if needs_splice {
        animation_set.dirty = false;
        shared.animations.sets.write().insert(key, animation_set);
    }
    needs_splice
}

/// Reimplementation of `servo/matching.rs`'s own (crate-private)
/// `needs_animations_update` — whether `@keyframes` animations need
/// starting, canceling, or restarting for this style change. Drops its
/// real counterpart's `TraversalFlags::ForCSSRuleChanges`/pseudo-element/
/// `writing-mode` branches: this bridge never sets that flag, never
/// resolves a pseudo-element, and this crate has no `writing-mode`
/// support to begin with, so each always takes its simplest real case.
fn needs_animations_update<'n>(
    context: &StyleContext<'_, StyloNode<'n>>,
    target: StyloNode<'n>,
    old_style: Option<&ComputedValues>,
    new_style: &ComputedValues,
) -> bool {
    let new_specifies_animations = new_style.get_ui().specifies_animations();
    let has_animations = target.has_animations(context.shared);
    if !new_specifies_animations && !has_animations {
        return false;
    }
    let Some(old_style) = old_style else {
        return new_specifies_animations;
    };
    if !old_style.get_ui().animations_equals(new_style.get_ui()) {
        return true;
    }
    let old_display = old_style.get_box().display;
    let new_display = new_style.get_box().display;
    if old_display == Display::None && new_display != Display::None {
        return new_specifies_animations;
    }
    if old_display != Display::None && new_display == Display::None {
        return has_animations;
    }
    false
}

/// Reimplementation of `servo/matching.rs`'s own (crate-private)
/// `might_need_transitions_update` — see [`needs_animations_update`]'s own
/// doc for why this bridge's version can drop its real counterpart's
/// pseudo-element handling.
fn might_need_transitions_update<'n>(
    context: &StyleContext<'_, StyloNode<'n>>,
    target: StyloNode<'n>,
    old_style: Option<&ComputedValues>,
    new_style: &ComputedValues,
) -> bool {
    let Some(old_style) = old_style else {
        return false;
    };
    if !target.has_css_transitions(context.shared, None)
        && !new_style.get_ui().specifies_transitions()
    {
        return false;
    }
    old_style.get_box().display != Display::None
}

/// Bakes whatever `transition`/`@keyframes` declarations are active right
/// now for `target` into its computed style, by re-cascading with them
/// spliced into the real CSS cascade at their own real cascade level
/// (above author styles, below `!important` — the same
/// `CascadeLevel::Transitions`/`::Animations` real CSS itself uses). This
/// is what Servo's own traversal driver does after
/// `process_animations_for_style` reports a change (`servo/matching.rs`'s
/// own `process_animations`); that method isn't reachable from here either
/// (it wants a full traversal's intermediate `ResolvedElementStyles`, not
/// `resolve_style`'s already-finished `ElementStyles`), so this
/// reimplements just the splice-and-recascade half by hand too.
fn splice_animation_declarations<'n>(
    target: StyloNode<'n>,
    context: &mut StyleContext<'_, StyloNode<'n>>,
    primary: &Arc<ComputedValues>,
    now: f64,
) -> Arc<ComputedValues> {
    let key = AnimationSetKey::new_for_non_pseudo(TNode::opaque(&target));
    let declarations = context
        .shared
        .animations
        .get_all_declarations(&key, now, shared_lock());

    let mut rule_node = primary.rules().clone();
    let mut important_rules_changed = false;
    if let Some(new_node) = context.shared.stylist.rule_tree().update_rule_at_level(
        CascadeLevel::Transitions,
        LayerOrder::root(),
        declarations.transitions.as_ref().map(|d| d.borrow_arc()),
        &rule_node,
        &context.shared.guards,
        &mut important_rules_changed,
    ) {
        rule_node = new_node;
    }
    if let Some(new_node) = context.shared.stylist.rule_tree().update_rule_at_level(
        CascadeLevel::Animations,
        LayerOrder::root(),
        declarations.animations.as_ref().map(|d| d.borrow_arc()),
        &rule_node,
        &context.shared.guards,
        &mut important_rules_changed,
    ) {
        rule_node = new_node;
    }

    if rule_node == *primary.rules() {
        return primary.clone();
    }

    let inputs = CascadeInputs {
        rules: Some(rule_node),
        ..CascadeInputs::new_from_style(primary)
    };
    StyleResolverForElement::new(
        target,
        context,
        RuleInclusion::All,
        PseudoElementResolution::IfApplicable,
    )
    .cascade_style_and_visited_with_default_parents(inputs)
    .0
}

fn to_computed_style(values: &ComputedValues, viewport_px: (f32, f32)) -> ComputedStyle {
    let background = values.get_background();
    let text = values.get_inherited_text();
    let position = values.get_position();
    let effects = values.get_effects();
    let box_style = values.get_box();
    let inherited_table = values.get_inherited_table();
    let font = values.get_font();
    let margin = values.get_margin();
    let padding = values.get_padding();
    let border = values.get_border();

    // `color`'s own computed value is always already-resolved (real CSS
    // never leaves it as `currentcolor`); resolving it first lets
    // `background-color`'s (and `border-*-color`'s) possible
    // `currentcolor` reference resolve against it, rather than an
    // arbitrary fallback.
    let color = to_absolute_rgba(&text.color);
    let font_size_px = font.font_size.computed_size.0.px();
    let font_size = style::values::computed::Length::new(font_size_px);

    ComputedStyle {
        background_color: background
            .background_color
            .as_absolute()
            .map(to_absolute_rgba)
            .unwrap_or(color),
        background_color_clip: to_background_color_clip(background, background_text_clips(values)),
        background_layers: to_background_layers(
            background,
            &text.color,
            background_blends(values),
            background_text_clips(values),
        ),
        color,
        width: to_optional_length(&position.width),
        height: to_optional_length(&position.height),
        width_percent: to_size_percentage(&position.width),
        height_percent: to_size_percentage(&position.height),
        max_width: to_max_length(&position.max_width),
        max_height: to_max_length(&position.max_height),
        min_width: to_min_size(&position.min_width),
        min_height: to_min_size(&position.min_height),
        box_sizing: to_box_sizing(position.box_sizing),
        margin: Edges {
            top: to_optional_margin(&margin.margin_top),
            right: to_optional_margin(&margin.margin_right),
            bottom: to_optional_margin(&margin.margin_bottom),
            left: to_optional_margin(&margin.margin_left),
        },
        padding: Edges {
            top: to_length(&padding.padding_top),
            right: to_length(&padding.padding_right),
            bottom: to_length(&padding.padding_bottom),
            left: to_length(&padding.padding_left),
        },
        font_size: font_size_px,
        font_weight: font.font_weight.value(),
        line_height: to_line_height(&font.line_height),
        letter_spacing: text.letter_spacing.0.resolve(font_size).px(),
        word_spacing: text.word_spacing.resolve(font_size).px(),
        text_indent: to_text_indent(&text.text_indent, values),
        white_space_collapse: to_white_space_collapse(text.white_space_collapse),
        text_wrap: text.text_wrap_mode == style::computed_values::text_wrap_mode::T::Wrap,
        word_break: to_word_break(text.word_break),
        overflow_wrap: to_overflow_wrap(text.overflow_wrap),
        text_overflow: to_text_overflow(&values.get_text().text_overflow),
        vertical_align: to_vertical_align(&box_style.vertical_align),
        display: to_display(values.get_box().display),
        author_hidden: authored_display_none(values),
        flex_direction: to_flex_direction(position.flex_direction),
        flex_wrap: to_flex_wrap(position.flex_wrap),
        justify_content: to_content_alignment(position.justify_content.0),
        align_content: to_content_alignment(position.align_content.0),
        align_items: to_item_alignment(position.align_items.0),
        align_self: to_item_alignment(position.align_self.0.0),
        flex_grow: position.flex_grow.0,
        flex_shrink: position.flex_shrink.0,
        flex_basis: to_flex_basis(&position.flex_basis),
        column_gap: to_gap(&position.column_gap),
        row_gap: to_gap(&position.row_gap),
        gap_normal: (
            matches!(
                position.column_gap,
                style::values::generics::length::GenericLengthPercentageOrNormal::Normal
            ),
            matches!(
                position.row_gap,
                style::values::generics::length::GenericLengthPercentageOrNormal::Normal
            ),
        ),
        position: to_position(box_style.position),
        inset: Edges {
            top: to_optional_inset(&position.top),
            right: to_optional_inset(&position.right),
            bottom: to_optional_inset(&position.bottom),
            left: to_optional_inset(&position.left),
        },
        z_index: to_z_index(position.z_index),
        // Stylo already clamps a declared `opacity` to this range at
        // computed-value time per spec; clamping again here costs nothing
        // and keeps this conversion honest on its own, independent of
        // that upstream guarantee holding across a future Stylo upgrade.
        opacity: effects.opacity.clamp(0.0, 1.0),
        pointer_events_none: matches!(
            values.get_inherited_ui().pointer_events,
            style::values::specified::PointerEvents::None
        ),
        visibility: to_visibility(values.get_inherited_box().visibility),
        deferred: to_deferred(values),
        font_style: to_font_style(&font.font_style),
        font_variant_caps: to_font_variant_caps(values),
        font_synthesis: to_font_synthesis(values),
        font_features: to_font_features(values),
        font_variations: to_font_variations(values),
        text_transform: to_text_transform(text.text_transform),
        text_align: to_text_align(text.text_align),
        block_align: to_block_align(text.text_align),
        text_shadow: to_text_shadows(&text.text_shadow.0, &text.color),
        glass: to_glass(values),
        backdrop_blur: to_backdrop_blur(values),
        text_decoration: to_text_decoration(values, font_size_px),
        outline: to_outline(values, color),
        border_image: to_border_image(border, &text.color),
        mask_box_image: to_mask_box_image(values, font_size_px, color, viewport_px),
        cursor_pointer: matches!(
            values.get_inherited_ui().cursor.keyword,
            style::values::computed::ui::CursorKind::Pointer
        ),
        overflow_clips: to_overflow_clips(box_style.overflow_x, box_style.overflow_y),
        overflow_makes_context: to_overflow_makes_context(
            box_style.overflow_x,
            box_style.overflow_y,
        ),
        overflow_scrolls_x: to_overflow_scrolls_x(box_style.overflow_x),
        overflow_scrolls_y: to_overflow_scrolls_y(box_style.overflow_y),
        font_family: to_font_family(&font.font_family),
        border: Edges {
            top: to_border_side(
                border.border_top_style,
                border.border_top_width,
                &border.border_top_color,
                color,
            ),
            right: to_border_side(
                border.border_right_style,
                border.border_right_width,
                &border.border_right_color,
                color,
            ),
            bottom: to_border_side(
                border.border_bottom_style,
                border.border_bottom_width,
                &border.border_bottom_color,
                color,
            ),
            left: to_border_side(
                border.border_left_style,
                border.border_left_width,
                &border.border_left_color,
                color,
            ),
        },
        border_hidden: Edges {
            top: is_hidden(border.border_top_style),
            right: is_hidden(border.border_right_style),
            bottom: is_hidden(border.border_bottom_style),
            left: is_hidden(border.border_left_style),
        },
        list_style_type: mirrored(values, "list-style-type")
            .map(|value| {
                crate::list::parse_list_style_type_with(&value, &crate::counter_style::lookup)
            })
            .unwrap_or_default(),
        list_style_position: to_list_style_position(values),
        list_style_image: to_background_image(&values.get_list().list_style_image, &text.color),
        counter_reset: to_counters(values, "counter-reset", 0),
        counter_increment: to_counters(values, "counter-increment", 1),
        counter_set: to_counters(values, "counter-set", 0),
        border_radius: Corners {
            top_left: to_corner_radius(&border.border_top_left_radius),
            top_right: to_corner_radius(&border.border_top_right_radius),
            bottom_right: to_corner_radius(&border.border_bottom_right_radius),
            bottom_left: to_corner_radius(&border.border_bottom_left_radius),
        },
        grid_template_columns: to_grid_template_of(
            values,
            "grid-template-columns",
            &position.grid_template_columns,
        ),
        grid_template_rows: to_grid_template_of(
            values,
            "grid-template-rows",
            &position.grid_template_rows,
        ),
        grid_auto_columns: to_implicit_tracks(&position.grid_auto_columns),
        grid_auto_rows: to_implicit_tracks(&position.grid_auto_rows),
        grid_auto_flow: to_grid_auto_flow(position.grid_auto_flow),
        grid_template_areas: to_grid_template_areas(&position.grid_template_areas),
        justify_items: to_item_alignment(position.justify_items.computed.0),
        justify_self: to_item_alignment(position.justify_self.0.0),
        order: position.order,
        float: to_float(box_style.float),
        clear: to_clear(box_style.clear),
        border_collapse: if inherited_table.border_collapse
            == style::computed_values::border_collapse::T::Collapse
        {
            crate::cascade::BorderCollapse::Collapse
        } else {
            crate::cascade::BorderCollapse::Separate
        },
        border_spacing: (
            inherited_table.border_spacing.0.width.px(),
            inherited_table.border_spacing.0.height.px(),
        ),
        table_layout: if values.get_table().table_layout
            == style::computed_values::table_layout::T::Fixed
        {
            crate::cascade::TableLayout::Fixed
        } else {
            crate::cascade::TableLayout::Auto
        },
        caption_side: if inherited_table.caption_side
            == style::values::computed::table::CaptionSide::Bottom
        {
            crate::cascade::CaptionSide::Bottom
        } else {
            crate::cascade::CaptionSide::Top
        },
        empty_cells: if inherited_table.empty_cells == style::computed_values::empty_cells::T::Hide
        {
            crate::cascade::EmptyCells::Hide
        } else {
            crate::cascade::EmptyCells::Show
        },
        grid_column: (
            to_grid_placement(&position.grid_column_start),
            to_grid_placement(&position.grid_column_end),
        ),
        grid_row: (
            to_grid_placement(&position.grid_row_start),
            to_grid_placement(&position.grid_row_end),
        ),
        box_shadow: to_box_shadows(&effects.box_shadow.0, color),
        transform: to_transform(&box_style.transform),
        translate: to_translate(&box_style.translate),
        rotate: to_rotate(&box_style.rotate),
        scale: to_scale(&box_style.scale),
        transform_origin: to_transform_origin(&box_style.transform_origin),
        transform_origin_z: box_style.transform_origin.depth.px(),
        perspective: to_perspective(&box_style.perspective),
        perspective_origin: to_object_position(&box_style.perspective_origin),
        transform_box: to_transform_box(values),
        svg_properties: to_svg_properties(values),
        preserve_3d: matches!(
            box_style.transform_style,
            style::values::computed::TransformStyle::Preserve3d
        ),
        backface_hidden: matches!(
            box_style.backface_visibility,
            style::computed_values::backface_visibility::T::Hidden
        ),
        clip_path: to_clip_path_with_shape(values, Units::new(font_size_px, viewport_px)),
        mask: to_mask_layers(values, &text.color, Units::new(font_size_px, viewport_px)),
        mix_blend_mode: to_blend_mode(effects.mix_blend_mode),
        isolate: matches!(
            box_style.isolation,
            style::computed_values::isolation::T::Isolate
        ),
        filter: with_filter_urls(values, to_filter(&effects.filter.0, color), "filter-urls"),
        backdrop_filter: with_filter_urls(
            values,
            to_filter(&effects.backdrop_filter.0, color),
            "backdrop-filter-urls",
        ),
        container_type: to_container_type(box_style.clone_container_type()),
        container_name: to_container_name(&box_style.clone_container_name()),
        object_fit: to_object_fit(position.object_fit),
        object_position: to_object_position(&position.object_position),
        aspect_ratio: to_aspect_ratio(&position.aspect_ratio),
        appearance: to_appearance(values),
        resize: to_resize(values),
        placeholder_color: to_placeholder_color(values),
        scroll_behavior_smooth: to_scroll_behavior_smooth(values),
        placeholder: None,
        selection: None,
        caret_color: mirrored(values, "caret-color").and_then(|css| parse_color(&css)),
        content: to_content(values),
        quotes: to_quotes(values),
        first_line: None,
    }
}

/// The `--florui-*` custom properties read for every node, interned once per
/// thread: building an `Atom` from a string hashes it and takes the global
/// table's lock, which dominated this conversion when done four times a node.
struct FlorNames {
    scroll_behavior: Atom,
    appearance: Atom,
    resize: Atom,
    placeholder_color: Atom,
    glass: Atom,
    glass_refraction: Atom,
    glass_edge: Atom,
    glass_light_angle: Atom,
    glass_light_strength: Atom,
    glass_quality: Atom,
    backdrop_blur: Atom,
    text_indent_keywords: Atom,
    /// The mirrored properties of [`crate::property_mirror::MIRRORED`], in its order.
    mirrors: Vec<Atom>,
}

thread_local! {
    static FLOR_NAMES: FlorNames = FlorNames {
        scroll_behavior: Atom::from("florui-scroll-behavior"),
        appearance: Atom::from("florui-appearance"),
        resize: Atom::from("florui-resize"),
        placeholder_color: Atom::from("florui-placeholder-color"),
        glass: Atom::from("florui-glass"),
        glass_refraction: Atom::from("florui-glass-refraction"),
        glass_edge: Atom::from("florui-glass-edge"),
        glass_light_angle: Atom::from("florui-glass-light-angle"),
        glass_light_strength: Atom::from("florui-glass-light-strength"),
        glass_quality: Atom::from("florui-glass-quality"),
        backdrop_blur: Atom::from("florui-backdrop-blur"),
        text_indent_keywords: Atom::from("florui-text-indent-keywords"),
        mirrors: crate::property_mirror::MIRRORED
            .iter()
            .map(|name| Atom::from(format!("florui-{name}").as_str()))
            .collect(),
    };
}

/// The CSS text of one of this crate's own unregistered custom properties,
/// or `None` when the node does not carry it. Every custom property authors
/// can declare is unregistered (no `@property` support), so
/// [`PropertyRegistrationData::unregistered`] (universal syntax, inherits)
/// is the registration to read it against, the same default the cascade
/// applies when substituting `var()`. Read directly, not substituted into
/// another property, since there is no real property to substitute it into
/// (see [`FlorAppearance`]'s own doc for why).
fn florui_property(values: &ComputedValues, name: impl Fn(&FlorNames) -> &Atom) -> Option<String> {
    use style::properties_and_values::registry::PropertyRegistrationData;
    use style_traits::ToCss;

    let properties = values.custom_properties();
    if properties.is_empty() {
        return None;
    }
    FLOR_NAMES.with(|names| {
        let value = properties.get(PropertyRegistrationData::unregistered(), name(names))?;
        let mut css = String::new();
        value.to_css(&mut CssWriter::new(&mut css)).ok()?;
        Some(css)
    })
}

/// The value of a property of [`crate::property_mirror::MIRRORED`], as written
/// (and inherited like the custom property carrying it).
fn mirrored(values: &ComputedValues, name: &str) -> Option<String> {
    // Almost no element has a custom property: nothing to look for then.
    if values.custom_properties().is_empty() {
        return None;
    }
    let index = crate::property_mirror::MIRRORED
        .iter()
        .position(|mirrored| *mirrored == name)?;
    florui_property(values, |names| &names.mirrors[index])
        .map(|css| css.trim().to_owned())
        .filter(|css| !css.is_empty())
}

/// `content`, with the items that come after the `/` of alternative text left out.
fn to_content(values: &ComputedValues) -> crate::content::Content {
    use crate::content::{Content, ContentItem};
    use style::values::generics::counters::{Content as Stylo, ContentItem as Item};
    use style_traits::ToCss;

    let text = |value: &str| -> std::sync::Arc<str> { value.into() };
    match &values.get_counters().content {
        Stylo::Normal => Content::Normal,
        Stylo::None => Content::None,
        Stylo::Items(items) => Content::Items(
            items
                .items
                .iter()
                .take(items.alt_start)
                .map(|item| match item {
                    Item::String(value) => ContentItem::Text(text(value)),
                    Item::Attr(attr) => ContentItem::Attr(text(&attr.attribute)),
                    Item::OpenQuote => ContentItem::OpenQuote,
                    Item::CloseQuote => ContentItem::CloseQuote,
                    Item::NoOpenQuote => ContentItem::NoOpenQuote,
                    Item::NoCloseQuote => ContentItem::NoCloseQuote,
                    Item::Counter(name, style) => ContentItem::Counter {
                        name: text(name.0.as_ref()),
                        style: text(&style.to_css_string()),
                    },
                    Item::Counters(name, separator, style) => ContentItem::Counters {
                        name: text(name.0.as_ref()),
                        separator: text(separator),
                        style: text(&style.to_css_string()),
                    },
                    Item::Image(image) => ContentItem::Image(text(&image.to_css_string())),
                })
                .collect(),
        ),
    }
}

/// `quotes`.
fn to_quotes(values: &ComputedValues) -> crate::content::Quotes {
    use crate::content::Quotes;
    use style::values::computed::Quotes as Stylo;

    match &values.get_list().quotes {
        Stylo::Auto => Quotes::Auto,
        Stylo::QuoteList(list) if list.0.is_empty() => Quotes::None,
        Stylo::QuoteList(list) => Quotes::Pairs(
            list.0
                .iter()
                .map(|pair| {
                    (
                        std::sync::Arc::<str>::from(&*pair.opening),
                        std::sync::Arc::<str>::from(&*pair.closing),
                    )
                })
                .collect(),
        ),
    }
}

/// Reads `--florui-scroll-behavior`: smooth only for the exact keyword.
fn to_scroll_behavior_smooth(values: &ComputedValues) -> bool {
    florui_property(values, |names| &names.scroll_behavior)
        .is_some_and(|css| css.trim().eq_ignore_ascii_case("smooth"))
}

/// Reads `--florui-backdrop-blur`: `fast` is the only value that changes
/// anything, so a misspelling keeps the exact blur.
fn to_backdrop_blur(values: &ComputedValues) -> crate::BackdropBlur {
    match florui_property(values, |names| &names.backdrop_blur)
        .as_deref()
        .map(str::trim)
    {
        Some(value) if value.eq_ignore_ascii_case("fast") => crate::BackdropBlur::Fast,
        _ => crate::BackdropBlur::Exact,
    }
}

/// Reads `--florui-glass` and its parameters. Absent or not `refract` is no
/// material. A parameter that is present but not a valid value of its kind
/// makes the whole request invalid rather than silently using a default.
fn to_glass(values: &ComputedValues) -> crate::GlassSpec {
    use crate::{GlassMaterial, GlassQuality, GlassSpec};

    match florui_property(values, |names| &names.glass)
        .as_deref()
        .map(str::trim)
    {
        Some("refract") => {}
        _ => return GlassSpec::None,
    }
    let length =
        |name: fn(&FlorNames) -> &Atom, default: f32, reason: &'static str| match florui_property(
            values, name,
        ) {
            None => Ok(default),
            Some(css) => parse_px(css.trim()).filter(|v| *v >= 0.0).ok_or(reason),
        };
    let refraction = length(
        |n| &n.glass_refraction,
        12.0,
        "--florui-glass-refraction is not a length of 0 or more",
    );
    let edge = length(
        |n| &n.glass_edge,
        20.0,
        "--florui-glass-edge is not a length of 0 or more",
    );
    let light_angle = match florui_property(values, |n| &n.glass_light_angle) {
        None => Ok(315.0),
        Some(css) => {
            parse_deg(css.trim()).ok_or("--florui-glass-light-angle is not an angle in degrees")
        }
    };
    let light_strength = match florui_property(values, |n| &n.glass_light_strength) {
        None => Ok(0.0),
        Some(css) => css
            .trim()
            .parse::<f32>()
            .ok()
            .filter(|v| (0.0..=1.0).contains(v))
            .ok_or("--florui-glass-light-strength is not a number from 0 to 1"),
    };
    let quality = match florui_property(values, |n| &n.glass_quality)
        .as_deref()
        .map(str::trim)
    {
        None | Some("full") => Ok(GlassQuality::Full),
        Some("reduced") => Ok(GlassQuality::Reduced),
        Some("off") => Ok(GlassQuality::Off),
        Some(_) => Err("--florui-glass-quality is not full, reduced or off"),
    };
    match (refraction, edge, light_angle, light_strength, quality) {
        (Ok(refraction), Ok(edge), Ok(light_angle), Ok(light_strength), Ok(quality)) => {
            GlassSpec::Material(GlassMaterial {
                refraction,
                edge,
                light_angle,
                light_strength,
                quality,
            })
        }
        (Err(reason), ..)
        | (_, Err(reason), ..)
        | (_, _, Err(reason), ..)
        | (_, _, _, Err(reason), _)
        | (.., Err(reason)) => GlassSpec::Invalid(reason),
    }
}

/// `12px`, or a bare `0`.
fn parse_px(css: &str) -> Option<f32> {
    match css.strip_suffix("px") {
        Some(number) => number.trim().parse().ok(),
        None => (css == "0").then_some(0.0),
    }
}

/// `315deg`, or a bare `0`.
fn parse_deg(css: &str) -> Option<f32> {
    match css.strip_suffix("deg") {
        Some(number) => number.trim().parse().ok(),
        None => (css == "0").then_some(0.0),
    }
}

/// Reads `appearance`, which the engine's servo build lacks and which is carried as
/// `--florui-appearance` (see [`crate::property_mirror`]). Neither spelling inherits, as the
/// property does not: only this element's own rules count. `none` removes the built-in
/// decoration; any other value leaves it.
fn to_appearance(values: &ComputedValues) -> FlorAppearance {
    if !declares_mirrored(values, "appearance") {
        return FlorAppearance::Auto;
    }
    match florui_property(values, |names| &names.appearance) {
        Some(css) if css.trim().eq_ignore_ascii_case("none") => FlorAppearance::None,
        _ => FlorAppearance::Auto,
    }
}

/// Reads `resize`, carried as `--florui-resize` and, like `appearance`, from this element's
/// own rules only. `block` and `inline` are the vertical and horizontal directions in the
/// horizontal writing mode every box here has.
fn to_resize(values: &ComputedValues) -> FlorResize {
    if !declares_mirrored(values, "resize") {
        return FlorResize::Both;
    }
    match florui_property(values, |names| &names.resize)
        .as_deref()
        .map(|css| css.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("none") => FlorResize::None,
        Some("vertical" | "block") => FlorResize::Vertical,
        Some("horizontal" | "inline") => FlorResize::Horizontal,
        _ => FlorResize::Both,
    }
}

/// Reads `--florui-placeholder-color`, then parses the color: hex,
/// `rgb()`/`rgba()` and named colors.
fn to_placeholder_color(values: &ComputedValues) -> Option<Rgba> {
    parse_color(florui_property(values, |names| &names.placeholder_color)?.trim())
}

fn parse_color(css: &str) -> Option<Rgba> {
    use cssparser::{Parser, ParserInput, Token};

    if let Some(hex) = css.strip_prefix('#') {
        let (r, g, b, a) = cssparser::color::parse_hash_color(hex.as_bytes()).ok()?;
        return Some(Rgba {
            r,
            g,
            b,
            a: (a * 255.0).round() as u8,
        });
    }
    if css.eq_ignore_ascii_case("transparent") {
        return Some(Rgba::TRANSPARENT);
    }
    if let Ok((r, g, b)) = cssparser::color::parse_named_color(css) {
        return Some(Rgba::opaque(r, g, b));
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let function = parser.expect_function().ok()?.to_ascii_lowercase();
    if function != "rgb" && function != "rgba" {
        return None;
    }
    parser
        .parse_nested_block(|args| {
            let channel = |args: &mut Parser<'_, '_>| -> Option<u8> {
                match args.next().ok()? {
                    Token::Number { value, .. } => Some(value.clamp(0.0, 255.0).round() as u8),
                    Token::Percentage { unit_value, .. } => {
                        Some((unit_value.clamp(0.0, 1.0) * 255.0).round() as u8)
                    }
                    _ => None,
                }
            };
            let separator = |args: &mut Parser<'_, '_>| {
                let _ = args.try_parse(|p| p.expect_comma());
            };
            let r = channel(args);
            separator(args);
            let g = channel(args);
            separator(args);
            let b = channel(args);
            let alpha = if args.try_parse(|p| p.expect_comma()).is_ok()
                || args.try_parse(|p| p.expect_delim('/')).is_ok()
            {
                match args.next().ok() {
                    Some(Token::Number { value, .. }) => value.clamp(0.0, 1.0),
                    Some(Token::Percentage { unit_value, .. }) => unit_value.clamp(0.0, 1.0),
                    _ => 1.0,
                }
            } else {
                1.0
            };
            Ok::<_, cssparser::ParseError<'_, ()>>(match (r, g, b) {
                (Some(r), Some(g), Some(b)) => Some(Rgba {
                    r,
                    g,
                    b,
                    a: (alpha * 255.0).round() as u8,
                }),
                _ => None,
            })
        })
        .ok()
        .flatten()
}

/// `object-fit`'s variant set matches real CSS's own 1:1 (`Fill`/
/// `Contain`/`Cover`/`None`/`ScaleDown`) -- no translation beyond naming.
fn to_object_fit(value: style::computed_values::object_fit::T) -> FlorObjectFit {
    use style::computed_values::object_fit::T as StyloObjectFit;
    match value {
        StyloObjectFit::Fill => FlorObjectFit::Fill,
        StyloObjectFit::Contain => FlorObjectFit::Contain,
        StyloObjectFit::Cover => FlorObjectFit::Cover,
        StyloObjectFit::None => FlorObjectFit::None,
        StyloObjectFit::ScaleDown => FlorObjectFit::ScaleDown,
    }
}

/// `object-position`'s `horizontal`/`vertical` components -- same shape
/// and same resolve-against-the-final-box reasoning as
/// [`to_transform_origin`].
fn to_object_position(
    value: &style::values::computed::position::Position,
) -> (FlorLengthPercentage, FlorLengthPercentage) {
    (
        to_length_percentage(&value.horizontal),
        to_length_percentage(&value.vertical),
    )
}

/// `aspect-ratio`'s `auto || <ratio>` grammar -- see [`FlorAspectRatio`]'s
/// own doc for why both components are carried through independently
/// rather than resolved into one value here.
fn to_aspect_ratio(value: &style::values::computed::position::AspectRatio) -> FlorAspectRatio {
    FlorAspectRatio {
        prefers_intrinsic: value.auto,
        ratio: match &value.ratio {
            style::values::generics::position::PreferredRatio::None => None,
            style::values::generics::position::PreferredRatio::Ratio(ratio) => {
                Some((ratio.0.0, ratio.1.0))
            }
        },
    }
}

/// `container-type` — checked in the same `size` before `inline-size`
/// order Stylo's own `container_rule.rs::container_type_axes` uses
/// (`size` containment is itself a superset of `inline-size`'s own bit).
fn to_container_type(value: style::values::computed::ContainerType) -> FlorContainerType {
    use style::values::computed::ContainerType as StyloContainerType;
    if value.intersects(StyloContainerType::SIZE) {
        FlorContainerType::Size
    } else if value.intersects(StyloContainerType::INLINE_SIZE) {
        FlorContainerType::InlineSize
    } else {
        FlorContainerType::Normal
    }
}

fn to_container_name(value: &style::values::computed::ContainerName) -> Vec<String> {
    value.0.iter().map(|ident| ident.0.to_string()).collect()
}

/// A filter list with the `url()` items the engine's servo build cannot hold put
/// back where they were written. The rewrite of a `filter` (or `backdrop-filter`)
/// gives the engine the list without them and the custom property the shape of the
/// whole one, `n` for an item the engine has and `url(...)` for the others; it is
/// used only when this element's own rules declare it and it fits the engine's list.
fn with_filter_urls(
    values: &ComputedValues,
    native: Vec<FlorFilterFunction>,
    mirror: &str,
) -> Vec<FlorFilterFunction> {
    if !declares_mirrored(values, mirror) {
        return native;
    }
    let Some(shape) = mirrored(values, mirror) else {
        return native;
    };
    let items = crate::property_mirror::split_commas(&shape);
    if !items.iter().any(|item| item.trim().starts_with("url(")) {
        return native;
    }
    // A `drop-shadow()` or any other item the engine dropped as invalid would
    // leave the shape and the list apart: then the list is the engine's alone.
    let natives = items.iter().filter(|item| item.trim() == "n").count();
    if natives != native.len() {
        return native;
    }
    let mut native = native.into_iter();
    items
        .iter()
        .filter_map(|item| {
            let item = item.trim();
            if item == "n" {
                native.next()
            } else {
                let address = crate::property_mirror::url_address(item)?;
                Some(FlorFilterFunction::Url(crate::intern::intern(&address)))
            }
        })
        .collect()
}

/// Shared by `filter` and `backdrop-filter` — same grammar, and Stylo's
/// two `OwnedList` wrappers share the same inner `OwnedSlice` type, so
/// one function converts both. See [`crate::cascade::FilterFunction`] for
/// which functions survive.
#[allow(clippy::type_complexity)]
fn to_filter(
    value: &style::OwnedSlice<
        style::values::generics::effects::GenericFilter<
            style::values::computed::Angle,
            style::values::generics::NonNegative<f32>,
            style::values::generics::ZeroToOne<f32>,
            style::values::generics::NonNegative<style::values::computed::Length>,
            style::values::generics::effects::GenericSimpleShadow<
                style::values::generics::color::GenericColor<style::values::computed::Percentage>,
                style::values::computed::Length,
                style::values::generics::NonNegative<style::values::computed::Length>,
            >,
            style::values::Impossible,
        >,
    >,
    inherited_color: Rgba,
) -> Vec<FlorFilterFunction> {
    use style::values::generics::effects::GenericFilter;
    value
        .iter()
        .filter_map(|f| match f {
            GenericFilter::Blur(length) => Some(FlorFilterFunction::Blur(length.0.px())),
            GenericFilter::Brightness(factor) => Some(FlorFilterFunction::Brightness(factor.0)),
            GenericFilter::Contrast(factor) => Some(FlorFilterFunction::Contrast(factor.0)),
            GenericFilter::Saturate(factor) => Some(FlorFilterFunction::Saturate(factor.0)),
            GenericFilter::Grayscale(amount) => Some(FlorFilterFunction::Grayscale(amount.0)),
            GenericFilter::HueRotate(angle) => Some(FlorFilterFunction::HueRotate(angle.degrees())),
            GenericFilter::Invert(amount) => Some(FlorFilterFunction::Invert(amount.0)),
            GenericFilter::Opacity(amount) => Some(FlorFilterFunction::Opacity(amount.0)),
            GenericFilter::Sepia(amount) => Some(FlorFilterFunction::Sepia(amount.0)),
            GenericFilter::DropShadow(shadow) => Some(FlorFilterFunction::DropShadow {
                x: shadow.horizontal.px(),
                y: shadow.vertical.px(),
                blur: shadow.blur.0.px(),
                color: shadow
                    .color
                    .as_absolute()
                    .map(to_absolute_rgba)
                    .unwrap_or(inherited_color),
            }),
            // `url()` names an SVG filter, which cannot be defined here.
            GenericFilter::Url(_) => None,
        })
        .collect()
}

/// The layers of `mask-image` with what goes with each. The images are the
/// engine's; the other properties come from the custom properties that carry
/// them (see [`crate::mask_values`]) when this element's own rules declare them,
/// since a custom property inherits and these do not. Each list is read
/// cyclically against the images, as the background's are; a layer with `none`
/// or an image that is not a gradient or a `url()` masks nothing and has no entry.
fn to_mask_layers(
    values: &ComputedValues,
    current: &style::color::AbsoluteColor,
    units: Units,
) -> Vec<crate::MaskLayer> {
    use crate::mask_values::{
        parse_boxes, parse_composites, parse_modes, parse_positions, parse_repeats, parse_sizes,
    };
    use crate::{
        BackgroundBox, BackgroundLayer, BackgroundRepeat, BackgroundSize, MaskComposite, MaskLayer,
        MaskMode,
    };
    let images: Vec<Option<crate::BackgroundImage>> = values
        .get_svg()
        .mask_image
        .0
        .iter()
        .map(|image| to_background_image(image, current))
        .collect();
    if images.iter().all(Option::is_none) {
        return Vec::new();
    }
    let read = |name: &str| {
        declares_mirrored(values, name)
            .then(|| mirrored(values, name))
            .flatten()
    };
    let modes = read("mask-mode").and_then(|css| parse_modes(&css));
    let repeats = read("mask-repeat").and_then(|css| parse_repeats(&css));
    let positions = read("mask-position").and_then(|css| parse_positions(&css, units));
    let sizes = read("mask-size").and_then(|css| parse_sizes(&css, units));
    let origins = read("mask-origin").and_then(|css| parse_boxes(&css, false));
    let clips = read("mask-clip").and_then(|css| parse_boxes(&css, true));
    let composites = read("mask-composite").and_then(|css| parse_composites(&css));
    fn pick<T: Copy>(list: &Option<Vec<T>>, index: usize, initial: T) -> T {
        match list {
            Some(list) if !list.is_empty() => list[index % list.len()],
            _ => initial,
        }
    }
    let start = FlorLengthPercentage::default();
    images
        .into_iter()
        .enumerate()
        .filter_map(|(index, image)| {
            Some(MaskLayer {
                layer: BackgroundLayer {
                    image: image?,
                    size: pick(&sizes, index, BackgroundSize::Explicit(None, None)),
                    position: pick(&positions, index, (start, start)),
                    repeat: pick(
                        &repeats,
                        index,
                        (BackgroundRepeat::Repeat, BackgroundRepeat::Repeat),
                    ),
                    origin: pick(&origins, index, BackgroundBox::Border),
                    clip: pick(&clips, index, BackgroundBox::Border),
                    blend: crate::BlendMode::Normal,
                },
                composite: pick(&composites, index, MaskComposite::Add),
                mode: pick(&modes, index, MaskMode::MatchSource),
            })
        })
        .collect()
}

/// `clip-path`, with the `shape()` function the engine does not accept read back
/// from the custom property that carries it (only when this very element's rules
/// declare it, since a custom property inherits and `clip-path` does not).
fn to_clip_path_with_shape(values: &ComputedValues, units: Units) -> ClipPath {
    let clip_path = to_clip_path(&values.get_svg().clip_path);
    if clip_path == ClipPath::None
        && declares_mirrored(values, "clip-path")
        && let Some(css) = mirrored(values, "clip-path")
        && let Some(shape) = crate::shape_function::parse_clip_path(&css, units)
    {
        return shape;
    }
    clip_path
}

/// `clip-path`: a basic shape against a reference box, a box alone, or `none`.
/// A `url()` to an SVG `<clipPath>` is `none`: there is no way to define one.
fn to_clip_path(value: &style::values::computed::basic_shape::ClipPath) -> ClipPath {
    use style::values::generics::basic_shape::GenericClipPath;
    match value {
        GenericClipPath::None => ClipPath::None,
        GenericClipPath::Url(url) => ClipPath::Url(url_text(url)),
        GenericClipPath::Box(reference) => ClipPath::Box(to_reference_box(*reference)),
        GenericClipPath::Shape(shape, reference) => {
            ClipPath::Shape(Box::new(to_clip_shape(shape)), to_reference_box(*reference))
        }
    }
}

fn to_reference_box(value: style::values::generics::basic_shape::ShapeGeometryBox) -> ReferenceBox {
    use style::values::generics::basic_shape::{ShapeBox, ShapeGeometryBox};
    match value {
        ShapeGeometryBox::ShapeBox(ShapeBox::MarginBox) => ReferenceBox::Margin,
        ShapeGeometryBox::ShapeBox(ShapeBox::PaddingBox) => ReferenceBox::Padding,
        // `fill-box` is the content box of an element with a CSS layout box.
        ShapeGeometryBox::ShapeBox(ShapeBox::ContentBox) | ShapeGeometryBox::FillBox => {
            ReferenceBox::Content
        }
        // `stroke-box` and `view-box` are the border box, as is the default.
        ShapeGeometryBox::ShapeBox(ShapeBox::BorderBox)
        | ShapeGeometryBox::StrokeBox
        | ShapeGeometryBox::ViewBox
        | ShapeGeometryBox::ElementDependent => ReferenceBox::Border,
    }
}

fn to_clip_shape(shape: &style::values::computed::basic_shape::BasicShape) -> ClipShape {
    use style::values::generics::basic_shape::{BasicShape, FillRule, GenericPathOrShapeFunction};
    match shape {
        BasicShape::Rect(inset) => {
            let corner = |c: &style::values::computed::BorderCornerRadius| {
                (
                    to_length_percentage(&c.0.width.0),
                    to_length_percentage(&c.0.height.0),
                )
            };
            ClipShape::Inset {
                top: to_length_percentage(&inset.rect.0),
                right: to_length_percentage(&inset.rect.1),
                bottom: to_length_percentage(&inset.rect.2),
                left: to_length_percentage(&inset.rect.3),
                radii: [
                    corner(&inset.round.top_left),
                    corner(&inset.round.top_right),
                    corner(&inset.round.bottom_right),
                    corner(&inset.round.bottom_left),
                ],
            }
        }
        BasicShape::Circle(circle) => ClipShape::Circle {
            center: to_shape_center(&circle.position),
            radius: to_shape_radius(&circle.radius),
        },
        BasicShape::Ellipse(ellipse) => ClipShape::Ellipse {
            center: to_shape_center(&ellipse.position),
            radius_x: to_shape_radius(&ellipse.semiaxis_x),
            radius_y: to_shape_radius(&ellipse.semiaxis_y),
        },
        BasicShape::Polygon(polygon) => ClipShape::Polygon {
            even_odd: matches!(polygon.fill, FillRule::Evenodd),
            points: polygon
                .coordinates
                .iter()
                .map(|c| (to_length_percentage(&c.0), to_length_percentage(&c.1)))
                .collect(),
        },
        BasicShape::PathOrShape(GenericPathOrShapeFunction::Path(path)) => ClipShape::Path {
            even_odd: matches!(path.fill, FillRule::Evenodd),
            // A `path()`'s numbers are pixels, not percentages.
            segments: to_path_segments(
                path.commands(),
                |v: &f32| FlorLengthPercentage {
                    length: *v,
                    percentage: 0.0,
                },
                |degrees: &f32| *degrees,
            ),
        },
        BasicShape::PathOrShape(GenericPathOrShapeFunction::Shape(shape)) => ClipShape::Path {
            even_odd: matches!(shape.fill, FillRule::Evenodd),
            segments: to_path_segments(shape.commands(), to_length_percentage, |angle| {
                angle.degrees()
            }),
        },
    }
}

fn to_shape_center(
    position: &style::values::generics::position::GenericPositionOrAuto<
        style::values::computed::Position,
    >,
) -> (FlorLengthPercentage, FlorLengthPercentage) {
    use style::values::generics::position::GenericPositionOrAuto;
    match position {
        GenericPositionOrAuto::Position(p) => (
            to_length_percentage(&p.horizontal),
            to_length_percentage(&p.vertical),
        ),
        GenericPositionOrAuto::Auto => (
            FlorLengthPercentage {
                length: 0.0,
                percentage: 0.5,
            },
            FlorLengthPercentage {
                length: 0.0,
                percentage: 0.5,
            },
        ),
    }
}

fn to_shape_radius(
    radius: &style::values::generics::basic_shape::GenericShapeRadius<
        style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
    >,
) -> ShapeRadius {
    use style::values::generics::basic_shape::GenericShapeRadius;
    match radius {
        GenericShapeRadius::Length(length) => ShapeRadius::Length(to_length_percentage(&length.0)),
        GenericShapeRadius::ClosestSide => ShapeRadius::ClosestSide,
        GenericShapeRadius::FarthestSide => ShapeRadius::FarthestSide,
    }
}

/// The commands of a `path()` or a `shape()`, which share one type, as absolute
/// segments. `length` reads a coordinate and `degrees` an angle.
fn to_path_segments<L, A>(
    commands: &[style::values::generics::basic_shape::GenericShapeCommand<A, L>],
    length: impl Fn(&L) -> FlorLengthPercentage,
    degrees: impl Fn(&A) -> f32,
) -> Vec<PathSegment> {
    use crate::clip::{RawCommand, absolutize};
    use style::values::generics::basic_shape::{ArcSize, ArcSweep, GenericShapeCommand as Cmd};
    let pair =
        |p: &style::values::generics::basic_shape::CoordinatePair<L>| (length(&p.x), length(&p.y));
    let raw: Vec<RawCommand> = commands
        .iter()
        .map(|command| match command {
            Cmd::Move { by_to, point } => RawCommand::Move {
                abs: by_to.is_abs(),
                to: pair(point),
            },
            Cmd::Line { by_to, point } => RawCommand::Line {
                abs: by_to.is_abs(),
                to: pair(point),
            },
            Cmd::HLine { by_to, x } => RawCommand::HLine {
                abs: by_to.is_abs(),
                x: length(x),
            },
            Cmd::VLine { by_to, y } => RawCommand::VLine {
                abs: by_to.is_abs(),
                y: length(y),
            },
            Cmd::CubicCurve {
                by_to,
                point,
                control1,
                control2,
            } => RawCommand::Cubic {
                abs: by_to.is_abs(),
                to: pair(point),
                c1: pair(control1),
                c2: pair(control2),
            },
            Cmd::QuadCurve {
                by_to,
                point,
                control1,
            } => RawCommand::Quad {
                abs: by_to.is_abs(),
                to: pair(point),
                c1: pair(control1),
            },
            Cmd::SmoothCubic {
                by_to,
                point,
                control2,
            } => RawCommand::SmoothCubic {
                abs: by_to.is_abs(),
                to: pair(point),
                c2: pair(control2),
            },
            Cmd::SmoothQuad { by_to, point } => RawCommand::SmoothQuad {
                abs: by_to.is_abs(),
                to: pair(point),
            },
            Cmd::Arc {
                by_to,
                point,
                radii,
                arc_sweep,
                arc_size,
                rotate,
            } => RawCommand::Arc {
                abs: by_to.is_abs(),
                to: pair(point),
                radii: pair(radii),
                rotation: degrees(rotate),
                large: matches!(arc_size, ArcSize::Large),
                clockwise: matches!(arc_sweep, ArcSweep::Cw),
            },
            Cmd::Close => RawCommand::Close,
        })
        .collect();
    absolutize(&raw)
}

/// `transform`'s own function list — see
/// [`crate::cascade::TransformFunction`]'s own doc for exactly which
/// functions survive and why the rest are dropped.
fn to_transform(
    value: &style::values::generics::transform::Transform<
        style::values::generics::transform::TransformOperation<
            style::values::computed::Angle,
            f32,
            style::values::computed::Length,
            i32,
            style::values::computed::LengthPercentage,
        >,
    >,
) -> Vec<FlorTransformFunction> {
    use style::values::generics::transform::TransformOperation;
    value
        .0
        .iter()
        .filter_map(|op| match op {
            TransformOperation::Matrix(m) => Some(FlorTransformFunction::Matrix {
                a: m.a,
                b: m.b,
                c: m.c,
                d: m.d,
                e: m.e,
                f: m.f,
            }),
            TransformOperation::Translate(x, y) => Some(FlorTransformFunction::Translate(
                to_length_percentage(x),
                to_length_percentage(y),
            )),
            TransformOperation::TranslateX(x) => Some(FlorTransformFunction::Translate(
                to_length_percentage(x),
                FlorLengthPercentage::default(),
            )),
            TransformOperation::TranslateY(y) => Some(FlorTransformFunction::Translate(
                FlorLengthPercentage::default(),
                to_length_percentage(y),
            )),
            TransformOperation::Scale(sx, sy) => Some(FlorTransformFunction::Scale(*sx, *sy)),
            TransformOperation::ScaleX(sx) => Some(FlorTransformFunction::Scale(*sx, 1.0)),
            TransformOperation::ScaleY(sy) => Some(FlorTransformFunction::Scale(1.0, *sy)),
            TransformOperation::Rotate(angle) => {
                Some(FlorTransformFunction::Rotate(angle.degrees()))
            }
            TransformOperation::Skew(x, y) => {
                Some(FlorTransformFunction::Skew(x.degrees(), y.degrees()))
            }
            TransformOperation::SkewX(x) => Some(FlorTransformFunction::Skew(x.degrees(), 0.0)),
            TransformOperation::SkewY(y) => Some(FlorTransformFunction::Skew(0.0, y.degrees())),
            TransformOperation::TranslateZ(z) => Some(FlorTransformFunction::Translate3d(
                FlorLengthPercentage::default(),
                FlorLengthPercentage::default(),
                z.px(),
            )),
            TransformOperation::Translate3D(x, y, z) => Some(FlorTransformFunction::Translate3d(
                to_length_percentage(x),
                to_length_percentage(y),
                z.px(),
            )),
            TransformOperation::ScaleZ(sz) => Some(FlorTransformFunction::Scale3d(1.0, 1.0, *sz)),
            TransformOperation::Scale3D(sx, sy, sz) => {
                Some(FlorTransformFunction::Scale3d(*sx, *sy, *sz))
            }
            TransformOperation::RotateX(angle) => Some(FlorTransformFunction::Rotate3d {
                x: 1.0,
                y: 0.0,
                z: 0.0,
                degrees: angle.degrees(),
            }),
            TransformOperation::RotateY(angle) => Some(FlorTransformFunction::Rotate3d {
                x: 0.0,
                y: 1.0,
                z: 0.0,
                degrees: angle.degrees(),
            }),
            TransformOperation::RotateZ(angle) => {
                Some(FlorTransformFunction::Rotate(angle.degrees()))
            }
            TransformOperation::Rotate3D(x, y, z, angle) => Some(FlorTransformFunction::Rotate3d {
                x: *x,
                y: *y,
                z: *z,
                degrees: angle.degrees(),
            }),
            TransformOperation::Perspective(function) => {
                use style::values::generics::transform::PerspectiveFunction;
                Some(FlorTransformFunction::Perspective(match function {
                    PerspectiveFunction::None => f32::INFINITY,
                    PerspectiveFunction::Length(length) => length.px().max(1.0),
                }))
            }
            TransformOperation::Matrix3D(m) => Some(FlorTransformFunction::Matrix3d([
                m.m11, m.m12, m.m13, m.m14, m.m21, m.m22, m.m23, m.m24, m.m31, m.m32, m.m33, m.m34,
                m.m41, m.m42, m.m43, m.m44,
            ])),
            // The animation-only interpolate/accumulate intermediates.
            _ => None,
        })
        .collect()
}

/// The individual `translate` property.
fn to_translate(
    value: &style::values::generics::transform::Translate<
        style::values::computed::LengthPercentage,
        style::values::computed::Length,
    >,
) -> Option<(FlorLengthPercentage, FlorLengthPercentage, f32)> {
    use style::values::generics::transform::Translate;
    match value {
        Translate::None => None,
        Translate::Translate(x, y, z) => {
            Some((to_length_percentage(x), to_length_percentage(y), z.px()))
        }
    }
}

/// The individual `rotate` property: the axis and angle in degrees.
fn to_rotate(
    value: &style::values::generics::transform::Rotate<f32, style::values::computed::Angle>,
) -> Option<(f32, f32, f32, f32)> {
    use style::values::generics::transform::Rotate;
    match value {
        Rotate::None => None,
        Rotate::Rotate(angle) => Some((0.0, 0.0, 1.0, angle.degrees())),
        Rotate::Rotate3D(x, y, z, angle) => Some((*x, *y, *z, angle.degrees())),
    }
}

/// The individual `scale` property.
fn to_scale(value: &style::values::generics::transform::Scale<f32>) -> Option<(f32, f32, f32)> {
    use style::values::generics::transform::Scale;
    match value {
        Scale::None => None,
        Scale::Scale(x, y, z) => Some((*x, *y, *z)),
    }
}

/// `perspective`, with the 1px floor CSS puts on a distance.
fn to_perspective(value: &style::values::computed::Perspective) -> Option<f32> {
    use style::values::generics::box_::Perspective;
    match value {
        Perspective::None => None,
        Perspective::Length(length) => Some(length.0.px().max(1.0)),
    }
}

/// `transform-origin`'s `x`/`y` components; its `z` component is
/// [`ComputedStyle::transform_origin_z`].
fn to_transform_origin(
    value: &style::values::generics::transform::TransformOrigin<
        style::values::computed::LengthPercentage,
        style::values::computed::LengthPercentage,
        style::values::computed::Length,
    >,
) -> (FlorLengthPercentage, FlorLengthPercentage) {
    (
        to_length_percentage(&value.horizontal),
        to_length_percentage(&value.vertical),
    )
}

/// Decomposes a Stylo `<length-percentage>` into this crate's own
/// `{ length, percentage }` pair without reaching into its private
/// representation: [`style::values::computed::LengthPercentage::resolve`]
/// is affine in its `basis` argument for every value the real grammar can
/// produce (a plain length, a plain percentage, or any spec-legal
/// `calc()` mixing the two — CSS never multiplies two percentages
/// together here), so evaluating it at `0px` and `1px` recovers exactly
/// the length and percentage coefficients algebraically: `resolve(0px)`
/// is the length term alone (the percentage term vanishes), and
/// `resolve(1px) - resolve(0px)` is the percentage term's own coefficient
/// (since the length term cancels). The same "read Stylo's own real
/// behavior instead of guessing" spirit as this module's compile-error
/// type probes, applied to a value instead of a type.
fn to_length_percentage(value: &style::values::computed::LengthPercentage) -> FlorLengthPercentage {
    use style::values::computed::Length;
    // A basis of 100 keeps the percentage exact to the last digit of an `f32`
    // (a basis of 1 loses two).
    let at_zero = value.resolve(Length::new(0.0)).px();
    let at_hundred = value.resolve(Length::new(100.0)).px();
    let at_two_hundred = value.resolve(Length::new(200.0)).px();
    let percentage = (at_hundred - at_zero) / 100.0;
    // A value that cannot be negative, like a track size, is clamped at zero
    // when its length is negative (`calc(50% - 10px)` is `0` at a basis of
    // `0`), which bends the line at the start. The two larger bases are past
    // the bend and give the same line the value has everywhere above it.
    if ((at_two_hundred - at_hundred) / 100.0 - percentage).abs() > 1e-4 {
        let percentage = (at_two_hundred - at_hundred) / 100.0;
        return FlorLengthPercentage {
            length: at_hundred - percentage * 100.0,
            percentage,
        };
    }
    FlorLengthPercentage {
        length: at_zero,
        percentage,
    }
}

fn to_blend_mode(value: style::computed_values::mix_blend_mode::T) -> crate::BlendMode {
    use crate::BlendMode;
    use style::computed_values::mix_blend_mode::T;
    match value {
        T::Normal => BlendMode::Normal,
        T::Multiply => BlendMode::Multiply,
        T::Screen => BlendMode::Screen,
        T::Overlay => BlendMode::Overlay,
        T::Darken => BlendMode::Darken,
        T::Lighten => BlendMode::Lighten,
        T::ColorDodge => BlendMode::ColorDodge,
        T::ColorBurn => BlendMode::ColorBurn,
        T::HardLight => BlendMode::HardLight,
        T::SoftLight => BlendMode::SoftLight,
        T::Difference => BlendMode::Difference,
        T::Exclusion => BlendMode::Exclusion,
        T::Hue => BlendMode::Hue,
        T::Saturation => BlendMode::Saturation,
        T::Color => BlendMode::Color,
        T::Luminosity => BlendMode::Luminosity,
        T::PlusLighter => BlendMode::PlusLighter,
    }
}

fn clip_box_of(
    value: &style::properties::longhands::background_clip::computed_value::single_value::T,
) -> crate::BackgroundBox {
    use crate::BackgroundBox;
    use style::properties::longhands::background_clip::computed_value::single_value::T;
    match value {
        T::BorderBox => BackgroundBox::Border,
        T::PaddingBox => BackgroundBox::Padding,
        T::ContentBox => BackgroundBox::Content,
    }
}

/// The box `background-color` is cut to, which is the clip of the bottom layer
/// (the last of the images, or the only one when there are none).
fn to_background_color_clip(
    background: &style::properties::style_structs::Background,
    text_clips: Vec<bool>,
) -> crate::BackgroundBox {
    let clips = &background.background_clip.0;
    let bottom = background.background_image.0.len().max(1) - 1;
    let cycle = |len: usize| if len == 0 { 0 } else { bottom % len };
    if text_clips.get(cycle(text_clips.len())).copied() == Some(true) {
        return crate::BackgroundBox::Text;
    }
    clips
        .get(cycle(clips.len()))
        .map_or(crate::BackgroundBox::Border, clip_box_of)
}

/// The layers of `background-image`. The list is as long as the images are
/// many; `background-size`, `-position`, `-repeat`, `-origin` and `-clip` are
/// read cyclically against it, as CSS says. A `none` or `url()` image makes
/// no layer here, but still takes its place in that cycle.
fn to_background_layers(
    background: &style::properties::style_structs::Background,
    current: &style::color::AbsoluteColor,
    blends: Vec<crate::BlendMode>,
    text_clips: Vec<bool>,
) -> Vec<crate::BackgroundLayer> {
    use crate::{BackgroundBox, BackgroundLayer};
    use style::properties::longhands::background_origin;

    let images = &background.background_image.0;
    let sizes = &background.background_size.0;
    let xs = &background.background_position_x.0;
    let ys = &background.background_position_y.0;
    let repeats = &background.background_repeat.0;
    let origins = &background.background_origin.0;
    let clips = &background.background_clip.0;
    let cycle = |len: usize, index: usize| if len == 0 { 0 } else { index % len };

    let origin_box = |value: &background_origin::computed_value::single_value::T| match value {
        background_origin::computed_value::single_value::T::BorderBox => BackgroundBox::Border,
        background_origin::computed_value::single_value::T::PaddingBox => BackgroundBox::Padding,
        background_origin::computed_value::single_value::T::ContentBox => BackgroundBox::Content,
    };
    let clip_box = clip_box_of;

    images
        .iter()
        .enumerate()
        .filter_map(|(index, image)| {
            let image = to_background_image(image, current)?;
            let size = sizes.get(cycle(sizes.len(), index))?;
            let x = xs.get(cycle(xs.len(), index))?;
            let y = ys.get(cycle(ys.len(), index))?;
            let repeat = repeats.get(cycle(repeats.len(), index))?;
            let origin = origins.get(cycle(origins.len(), index))?;
            let clip = clips.get(cycle(clips.len(), index))?;
            Some(BackgroundLayer {
                image,
                size: to_background_size(size),
                position: (to_length_percentage(x), to_length_percentage(y)),
                repeat: (to_repeat(repeat.0), to_repeat(repeat.1)),
                origin: origin_box(origin),
                clip: if text_clips
                    .get(cycle(text_clips.len(), index))
                    .copied()
                    .unwrap_or(false)
                {
                    BackgroundBox::Text
                } else {
                    clip_box(clip)
                },
                blend: blends
                    .get(cycle(blends.len(), index))
                    .copied()
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// `transform-box`, which the engine's servo build lacks, read back from the
/// custom property that carries it when this element's own rules declare it.
fn to_transform_box(values: &ComputedValues) -> crate::TransformBox {
    use crate::TransformBox as T;
    if !declares_mirrored(values, "transform-box") {
        return T::View;
    }
    match mirrored(values, "transform-box")
        .map(|css| css.to_ascii_lowercase())
        .as_deref()
    {
        Some("border-box") => T::Border,
        Some("content-box") => T::Content,
        Some("fill-box") => T::Fill,
        Some("stroke-box") => T::Stroke,
        _ => T::View,
    }
}

/// Which layers `background-clip` cuts to the text, which the engine's servo
/// build cannot hold, read back from the custom property that carries it when
/// this element's own rules declare it.
fn background_text_clips(values: &ComputedValues) -> Vec<bool> {
    if !declares_mirrored(values, "background-clip") {
        return Vec::new();
    }
    mirrored(values, "background-clip")
        .map(|css| crate::mask_values::parse_text_clips(&css))
        .unwrap_or_default()
}

/// `background-blend-mode`, which the engine's servo build lacks, read back from
/// the custom property that carries it when this element's own rules declare it
/// (the property does not inherit and the custom property does).
fn background_blends(values: &ComputedValues) -> Vec<crate::BlendMode> {
    if !declares_mirrored(values, "background-blend-mode") {
        return Vec::new();
    }
    mirrored(values, "background-blend-mode")
        .and_then(|css| crate::mask_values::parse_blend_modes(&css))
        .unwrap_or_default()
}

fn to_repeat(
    keyword: style::values::specified::background::BackgroundRepeatKeyword,
) -> crate::BackgroundRepeat {
    use crate::BackgroundRepeat;
    use style::values::specified::background::BackgroundRepeatKeyword as K;
    match keyword {
        K::Repeat => BackgroundRepeat::Repeat,
        K::Space => BackgroundRepeat::Space,
        K::Round => BackgroundRepeat::Round,
        K::NoRepeat => BackgroundRepeat::NoRepeat,
    }
}

fn to_background_size(size: &style::values::computed::BackgroundSize) -> crate::BackgroundSize {
    use crate::BackgroundSize;
    use style::values::generics::background::BackgroundSize as S;
    use style::values::generics::length::LengthPercentageOrAuto as Axis;
    let axis = |value: &Axis<style::values::computed::NonNegativeLengthPercentage>| match value {
        Axis::LengthPercentage(lp) => Some(to_length_percentage(&lp.0)),
        Axis::Auto => None,
    };
    match size {
        S::ExplicitSize { width, height } => BackgroundSize::Explicit(axis(width), axis(height)),
        S::Cover => BackgroundSize::Cover,
        S::Contain => BackgroundSize::Contain,
    }
}

/// One gradient color: `currentcolor` and the like settle against this
/// element's own `color`.
fn to_gradient_color(
    color: &style::values::computed::Color,
    current: &style::color::AbsoluteColor,
) -> Rgba {
    to_absolute_rgba(&color.resolve_to_absolute(current))
}

/// The text of a `url()`: what the stylesheet wrote for a path that does not
/// resolve against the stylesheet's own address (every relative one, which is
/// then read against the working directory like `<img src>`), or the full
/// address of one that does (`data:`).
fn url_text(url: &style::servo::url::ComputedUrl) -> String {
    use style::servo::url::ComputedUrl;
    match url {
        ComputedUrl::Valid(resolved) => resolved.as_str().to_string(),
        ComputedUrl::Invalid(original) => original.to_string(),
    }
}

fn to_background_image(
    image: &style::values::computed::Image,
    current: &style::color::AbsoluteColor,
) -> Option<crate::BackgroundImage> {
    use crate::{BackgroundImage, ConicGradient, LinearGradient, RadialGradient};
    use style::values::computed::Image;
    use style::values::generics::image::{GenericGradient as G, GradientFlags};

    let gradient = match image {
        Image::Gradient(gradient) => gradient,
        Image::Url(url) => return Some(BackgroundImage::Url(url_text(url))),
        _ => return None,
    };
    let center = |position: &style::values::computed::Position| {
        (
            to_length_percentage(&position.horizontal),
            to_length_percentage(&position.vertical),
        )
    };
    match &**gradient {
        G::Linear {
            direction,
            items,
            flags,
            ..
        } => Some(BackgroundImage::Linear(LinearGradient {
            direction: to_linear_direction(direction),
            items: to_gradient_items(items, current),
            repeating: flags.contains(GradientFlags::REPEATING),
        })),
        G::Radial {
            shape,
            position,
            items,
            flags,
            ..
        } => Some(BackgroundImage::Radial(RadialGradient {
            size: to_radial_size(shape),
            center: center(position),
            items: to_gradient_items(items, current),
            repeating: flags.contains(GradientFlags::REPEATING),
        })),
        G::Conic {
            angle,
            position,
            items,
            flags,
            ..
        } => Some(BackgroundImage::Conic(ConicGradient {
            from: angle.degrees(),
            center: center(position),
            items: to_conic_items(items, current),
            repeating: flags.contains(GradientFlags::REPEATING),
        })),
    }
}

fn to_linear_direction(
    direction: &style::values::computed::image::LineDirection,
) -> crate::LinearDirection {
    use crate::LinearDirection;
    use style::values::computed::image::LineDirection as D;
    use style::values::specified::position::{
        HorizontalPositionKeyword as H, VerticalPositionKeyword as V,
    };
    match direction {
        D::Angle(angle) => LinearDirection::Angle(angle.degrees()),
        D::Horizontal(H::Left) => LinearDirection::Angle(270.0),
        D::Horizontal(H::Right) => LinearDirection::Angle(90.0),
        D::Vertical(V::Top) => LinearDirection::Angle(0.0),
        D::Vertical(V::Bottom) => LinearDirection::Angle(180.0),
        D::Corner(h, v) => LinearDirection::Corner {
            right: matches!(h, H::Right),
            bottom: matches!(v, V::Bottom),
        },
    }
}

fn to_radial_size(shape: &style::values::computed::image::EndingShape) -> crate::RadialSize {
    use crate::{RadialExtent, RadialSize};
    use style::values::generics::image::{Circle, Ellipse, EndingShape, ShapeExtent};
    let extent = |extent: &ShapeExtent| match extent {
        ShapeExtent::ClosestSide | ShapeExtent::Contain => RadialExtent::ClosestSide,
        ShapeExtent::FarthestSide => RadialExtent::FarthestSide,
        ShapeExtent::ClosestCorner => RadialExtent::ClosestCorner,
        ShapeExtent::FarthestCorner | ShapeExtent::Cover => RadialExtent::FarthestCorner,
    };
    match shape {
        EndingShape::Circle(Circle::Radius(radius)) => RadialSize::Circle(radius.0.px()),
        EndingShape::Circle(Circle::Extent(e)) => RadialSize::Extent {
            circle: true,
            extent: extent(e),
        },
        EndingShape::Ellipse(Ellipse::Radii(w, h)) => {
            RadialSize::Ellipse(to_length_percentage(&w.0), to_length_percentage(&h.0))
        }
        EndingShape::Ellipse(Ellipse::Extent(e)) => RadialSize::Extent {
            circle: false,
            extent: extent(e),
        },
    }
}

fn to_gradient_items(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::LengthPercentage,
    >],
    current: &style::color::AbsoluteColor,
) -> Vec<crate::GradientItem> {
    use style::values::generics::image::GenericGradientItem as I;
    items
        .iter()
        .map(|item| match item {
            I::SimpleColorStop(color) => crate::GradientItem::Stop {
                color: to_gradient_color(color, current),
                position: None,
            },
            I::ComplexColorStop { color, position } => crate::GradientItem::Stop {
                color: to_gradient_color(color, current),
                position: Some(to_length_percentage(position)),
            },
            I::InterpolationHint(position) => {
                crate::GradientItem::Hint(to_length_percentage(position))
            }
        })
        .collect()
}

fn to_conic_items(
    items: &[style::values::generics::image::GenericGradientItem<
        style::values::computed::Color,
        style::values::computed::AngleOrPercentage,
    >],
    current: &style::color::AbsoluteColor,
) -> Vec<crate::ConicItem> {
    use style::values::computed::AngleOrPercentage;
    use style::values::generics::image::GenericGradientItem as I;
    let turns = |value: &AngleOrPercentage| match value {
        AngleOrPercentage::Percentage(p) => p.0,
        AngleOrPercentage::Angle(a) => a.degrees() / 360.0,
    };
    items
        .iter()
        .map(|item| match item {
            I::SimpleColorStop(color) => crate::ConicItem::Stop {
                color: to_gradient_color(color, current),
                position: None,
            },
            I::ComplexColorStop { color, position } => crate::ConicItem::Stop {
                color: to_gradient_color(color, current),
                position: Some(turns(position)),
            },
            I::InterpolationHint(position) => crate::ConicItem::Hint(turns(position)),
        })
        .collect()
}

/// `text-shadow`'s layers, in source order; `currentcolor` is the element's
/// own `color`.
fn to_text_shadows(
    shadows: &[style::values::computed::effects::SimpleShadow],
    current: &style::color::AbsoluteColor,
) -> Vec<crate::TextShadow> {
    shadows
        .iter()
        .map(|shadow| crate::TextShadow {
            offset_x: shadow.horizontal.px(),
            offset_y: shadow.vertical.px(),
            blur_radius: shadow.blur.px(),
            color: to_gradient_color(&shadow.color, current),
        })
        .collect()
}

/// `box-shadow`'s own list of layers, in source order — see
/// [`FlorBoxShadow`]'s own doc for the per-layer conversion and which
/// field it carries through unrendered.
fn to_box_shadows(
    shadows: &[style::values::computed::BoxShadow],
    inherited_color: Rgba,
) -> Vec<FlorBoxShadow> {
    shadows
        .iter()
        .map(|shadow| to_box_shadow(shadow, inherited_color))
        .collect()
}

/// `currentcolor` resolves against `inherited_color` (this element's own
/// already-resolved `color`), the same fallback `background-color`/
/// `border-*-color` already use.
fn to_box_shadow(
    shadow: &style::values::computed::BoxShadow,
    inherited_color: Rgba,
) -> FlorBoxShadow {
    FlorBoxShadow {
        offset_x: shadow.base.horizontal.px(),
        offset_y: shadow.base.vertical.px(),
        blur_radius: shadow.base.blur.px(),
        spread_radius: shadow.spread.px(),
        color: shadow
            .base
            .color
            .as_absolute()
            .map(to_absolute_rgba)
            .unwrap_or(inherited_color),
        inset: shadow.inset,
    }
}

/// One corner's (horizontal, vertical) radius pair, percentages left
/// unresolved -- see [`crate::cascade::ComputedStyle::border_radius`].
fn to_corner_radius(
    radius: &style::values::computed::BorderCornerRadius,
) -> (FlorLengthPercentage, FlorLengthPercentage) {
    (
        to_length_percentage(&radius.0.width.0),
        to_length_percentage(&radius.0.height.0),
    )
}

/// `border-image`, when its source is a gradient or an image.
fn to_border_image(
    border: &style::properties::style_structs::Border,
    current: &style::color::AbsoluteColor,
) -> Option<Box<crate::BorderImage>> {
    use crate::{BorderImageOutset, BorderImageRepeat, BorderImageSlice, BorderImageWidth};
    use style::values::computed::border::BorderImageSideWidth as Side;
    use style::values::generics::length::GenericLengthOrNumber as Outset;
    use style::values::specified::border::BorderImageRepeatKeyword as Keyword;

    let source = to_background_image(&border.border_image_source, current)?;
    let slice = &border.border_image_slice;
    let slice_of = |value: &style::values::computed::NonNegativeNumberOrPercentage| match value.0 {
        style::values::computed::NumberOrPercentage::Number(n) => BorderImageSlice::Number(n),
        style::values::computed::NumberOrPercentage::Percentage(p) => {
            BorderImageSlice::Percent(p.0)
        }
    };
    let width_of = |side: &Side| match side {
        Side::Number(n) => BorderImageWidth::Number(n.0),
        Side::LengthPercentage(lp) => BorderImageWidth::Length(to_length_percentage(&lp.0)),
        Side::Auto => BorderImageWidth::Auto,
    };
    let outset_of = |side: &style::values::computed::NonNegativeLengthOrNumber| match side {
        Outset::Number(n) => BorderImageOutset::Number(n.0),
        Outset::Length(length) => BorderImageOutset::Length(length.0.px()),
    };
    let repeat_of = |keyword: &Keyword| match keyword {
        Keyword::Stretch => BorderImageRepeat::Stretch,
        Keyword::Repeat => BorderImageRepeat::Repeat,
        Keyword::Round => BorderImageRepeat::Round,
        Keyword::Space => BorderImageRepeat::Space,
    };
    let (s, w, o) = (
        &slice.offsets,
        &border.border_image_width,
        &border.border_image_outset,
    );
    Some(Box::new(crate::BorderImage {
        source,
        slice: [
            slice_of(&s.0),
            slice_of(&s.1),
            slice_of(&s.2),
            slice_of(&s.3),
        ],
        fill: slice.fill,
        width: [
            width_of(&w.0),
            width_of(&w.1),
            width_of(&w.2),
            width_of(&w.3),
        ],
        outset: [
            outset_of(&o.0),
            outset_of(&o.1),
            outset_of(&o.2),
            outset_of(&o.3),
        ],
        repeat: (
            repeat_of(&border.border_image_repeat.0),
            repeat_of(&border.border_image_repeat.1),
        ),
    }))
}

/// `-webkit-mask-box-image` and its parts, read back from the custom properties
/// that carry them (the engine's servo build lacks them) when this element's
/// own rules declare them, and computed as the `border-image` parts they are the
/// twins of.
fn to_mask_box_image(
    values: &ComputedValues,
    font_size_px: f32,
    color: Rgba,
    viewport_px: (f32, f32),
) -> Option<Box<crate::mask_box_image::MaskBoxImageSpec>> {
    const PARTS: [&str; 5] = [
        "mask-box-image-source",
        "mask-box-image-slice",
        "mask-box-image-width",
        "mask-box-image-outset",
        "mask-box-image-repeat",
    ];
    if !declares_mirrored(values, PARTS[0]) {
        return None;
    }
    let parts = PARTS.map(|part| {
        if declares_mirrored(values, part) {
            mirrored(values, part)
        } else {
            None
        }
    });
    Some(Box::new(crate::mask_box_image::MaskBoxImageSpec {
        parts,
        font_size: font_size_px,
        color,
        viewport: viewport_px,
    }))
}

/// The `outline`: no width for `none`, `currentcolor` resolved against `color`.
fn to_outline(values: &ComputedValues, color: Rgba) -> crate::Outline {
    use style::values::computed::OutlineStyle as S;
    let outline = values.get_outline();
    let style = match outline.outline_style {
        S::Auto => crate::OutlineStyle::Auto,
        S::BorderStyle(border) if border.none_or_hidden() => return crate::Outline::default(),
        S::BorderStyle(border) => crate::OutlineStyle::Line(to_border_line_style(border)),
    };
    crate::Outline {
        width: outline.outline_width.to_f32_px(),
        style,
        color: outline
            .outline_color
            .as_absolute()
            .map(to_absolute_rgba)
            .unwrap_or(color),
        offset: outline.outline_offset.px(),
    }
}

fn to_border_line_style(style: style::values::computed::BorderStyle) -> BorderLineStyle {
    use style::values::computed::BorderStyle as S;
    match style {
        S::Dashed => BorderLineStyle::Dashed,
        S::Dotted => BorderLineStyle::Dotted,
        S::Double => BorderLineStyle::Double,
        S::Groove => BorderLineStyle::Groove,
        S::Ridge => BorderLineStyle::Ridge,
        S::Inset => BorderLineStyle::Inset,
        S::Outset => BorderLineStyle::Outset,
        _ => BorderLineStyle::Solid,
    }
}

/// One border side: `0.0` width for `none`/`hidden` (real CSS's initial
/// style, which makes a border invisible regardless of its width/color —
/// see [`FlorBorderSide`]'s own doc), a solid-only rendering treatment for
/// every other style. `currentcolor` resolves against `inherited_color`
/// (this element's own already-resolved `color`), the same fallback
/// `background-color` already uses.
fn to_list_style_position(values: &ComputedValues) -> crate::list::ListStylePosition {
    use style::computed_values::list_style_position::T;
    match values.get_list().list_style_position {
        T::Inside => crate::list::ListStylePosition::Inside,
        T::Outside => crate::list::ListStylePosition::Outside,
    }
}

/// The counters a mirrored `counter-reset`, `counter-increment` or `counter-set`
/// names on this element (they do not inherit, as the custom property does).
fn to_counters(values: &ComputedValues, name: &str, default: i32) -> Vec<(String, i32)> {
    if !declares_mirrored(values, name) {
        return Vec::new();
    }
    mirrored(values, name)
        .map(|value| crate::list::parse_counters(&value, default))
        .unwrap_or_default()
}

fn is_hidden(style: style::values::computed::BorderStyle) -> bool {
    style == style::values::computed::BorderStyle::Hidden
}

fn to_border_side(
    style: style::values::computed::BorderStyle,
    width: app_units::Au,
    color: &style::values::computed::Color,
    inherited_color: Rgba,
) -> FlorBorderSide {
    if style.none_or_hidden() {
        return FlorBorderSide {
            width: 0.0,
            color: inherited_color,
            style: Default::default(),
        };
    }
    let line = to_border_line_style(style);
    // Chromium paints a groove, ridge, inset or outset border whose colour is
    // `currentcolor` from a fixed light grey, whatever `color` is.
    let current = if matches!(
        line,
        BorderLineStyle::Groove
            | BorderLineStyle::Ridge
            | BorderLineStyle::Inset
            | BorderLineStyle::Outset
    ) {
        Rgba::opaque(0xee, 0xee, 0xee)
    } else {
        inherited_color
    };
    FlorBorderSide {
        style: line,
        width: width.to_f32_px(),
        color: color.as_absolute().map(to_absolute_rgba).unwrap_or(current),
    }
}

/// A `grid-template-columns`/`-rows`, read from the mirrored declaration when
/// it was a `subgrid`, which the engine's build does not take.
fn to_grid_template_of(
    values: &ComputedValues,
    name: &str,
    component: &style::values::computed::GridTemplateComponent,
) -> GridTemplate {
    // Only a grid container can be a subgrid.
    let is_grid = matches!(to_display(values.get_box().display), FlorDisplay::Grid);
    if is_grid
        && declares_mirrored(values, name)
        && let Some(css) = mirrored(values, name)
        && let Some(names) = crate::grid::parse_subgrid(&css)
    {
        return GridTemplate {
            subgrid: Some(Box::new(names)),
            ..GridTemplate::default()
        };
    }
    to_grid_template(component)
}

/// A `grid-template-columns`/`-rows` track list: tracks, `repeat()`s and the
/// names of the lines. `none`, `subgrid` and `masonry` read as no template,
/// the same as an unsupported keyword.
fn to_grid_template(component: &style::values::computed::GridTemplateComponent) -> GridTemplate {
    use style::values::generics::grid::{GridTemplateComponent as Component, TrackListValue};

    let list = match component {
        Component::TrackList(list) => list,
        _ => return GridTemplate::default(),
    };
    let components = list
        .values
        .iter()
        .map(|value| match value {
            TrackListValue::TrackSize(size) => {
                GridTemplateComponent::Track(to_grid_track_size(size))
            }
            TrackListValue::TrackRepeat(repeat) => GridTemplateComponent::Repeat(GridRepeat {
                count: match repeat.count {
                    style::values::generics::grid::RepeatCount::Number(count) => {
                        GridRepeatCount::Count(count.max(1) as u16)
                    }
                    style::values::generics::grid::RepeatCount::AutoFill => {
                        GridRepeatCount::AutoFill
                    }
                    style::values::generics::grid::RepeatCount::AutoFit => GridRepeatCount::AutoFit,
                },
                tracks: repeat.track_sizes.iter().map(to_grid_track_size).collect(),
                line_names: to_line_names(&repeat.line_names),
            }),
        })
        .collect();
    GridTemplate {
        components,
        line_names: to_line_names(&list.line_names),
        subgrid: None,
    }
}

/// The names of each line, empty when none is named.
fn to_line_names(names: &[style::OwnedSlice<style::values::CustomIdent>]) -> Vec<Vec<String>> {
    if names.iter().all(|line| line.is_empty()) {
        return Vec::new();
    }
    names
        .iter()
        .map(|line| line.iter().map(|name| name.0.to_string()).collect())
        .collect()
}

/// The implicit tracks of `grid-auto-columns`/`-rows`.
fn to_implicit_tracks(tracks: &style::values::computed::ImplicitGridTracks) -> Vec<GridTrackSize> {
    tracks.0.iter().map(to_grid_track_size).collect()
}

fn to_grid_track_size(size: &style::values::computed::TrackSize) -> GridTrackSize {
    use style::values::generics::grid::TrackSize;
    match size {
        TrackSize::Breadth(breadth) => GridTrackSize::Breadth(to_grid_breadth(breadth)),
        TrackSize::Minmax(min, max) => {
            GridTrackSize::MinMax(to_grid_breadth(min), to_grid_breadth(max))
        }
        TrackSize::FitContent(limit) => GridTrackSize::FitContent(match limit {
            style::values::generics::grid::TrackBreadth::Breadth(value) => {
                match to_grid_breadth_length(value) {
                    GridBreadth::Length(px) => GridLimit::Length(px),
                    GridBreadth::Percent(fraction) => GridLimit::Percent(fraction),
                    GridBreadth::Calc(calc) => GridLimit::Calc(calc),
                    _ => GridLimit::Length(0.0),
                }
            }
            _ => GridLimit::Length(0.0),
        }),
    }
}

fn to_grid_breadth(breadth: &style::values::computed::TrackBreadth) -> GridBreadth {
    use style::values::generics::grid::TrackBreadth;
    match breadth {
        TrackBreadth::Breadth(value) => to_grid_breadth_length(value),
        TrackBreadth::Fr(share) => GridBreadth::Fr(*share),
        TrackBreadth::Auto => GridBreadth::Auto,
        TrackBreadth::MinContent => GridBreadth::MinContent,
        TrackBreadth::MaxContent => GridBreadth::MaxContent,
    }
}

/// A `<length-percentage>` track size: a length, a percentage or a mix.
fn to_grid_breadth_length(value: &style::values::computed::LengthPercentage) -> GridBreadth {
    let parts = to_length_percentage(value);
    if parts.percentage == 0.0 {
        GridBreadth::Length(parts.length)
    } else if parts.length == 0.0 {
        GridBreadth::Percent(parts.percentage)
    } else {
        GridBreadth::Calc(parts)
    }
}

/// `grid-template-areas` with the size of its template.
fn to_grid_template_areas(
    areas: &style::values::computed::GridTemplateAreas,
) -> Option<GridTemplateAreas> {
    let style::values::computed::GridTemplateAreas::Areas(areas) = areas else {
        return None;
    };
    let template = &areas.0;
    Some(GridTemplateAreas {
        areas: template
            .areas
            .iter()
            .map(|area| GridArea {
                name: area.name.to_string(),
                row_start: area.rows.start as u16,
                row_end: area.rows.end as u16,
                column_start: area.columns.start as u16,
                column_end: area.columns.end as u16,
            })
            .collect(),
        rows: template.strings.len() as u16,
        columns: template.width as u16,
    })
}

fn to_grid_auto_flow(flow: style::values::computed::GridAutoFlow) -> GridAutoFlow {
    use style::values::computed::GridAutoFlow as Flow;
    match (flow.contains(Flow::COLUMN), flow.contains(Flow::DENSE)) {
        (false, false) => GridAutoFlow::Row,
        (false, true) => GridAutoFlow::RowDense,
        (true, false) => GridAutoFlow::Column,
        (true, true) => GridAutoFlow::ColumnDense,
    }
}

/// A `grid-{row,column}-{start,end}` line.
fn to_grid_placement(line: &style::values::computed::GridLine) -> GridPlacement {
    let name = line.ident.0.to_string();
    if line.is_auto() {
        return GridPlacement::Auto;
    }
    match (name.is_empty(), line.is_span) {
        (true, true) => GridPlacement::Span(line.line_num.max(1) as u16),
        (true, false) => GridPlacement::Line(line.line_num as i16),
        (false, true) => GridPlacement::NamedSpan(name, line.line_num.max(1) as u16),
        (false, false) => GridPlacement::NamedLine(name, line.line_num as i16),
    }
}

/// `Display`'s own `inside()`/`outside()` split matches real CSS's
/// two-value `display` syntax — see [`FlorDisplay`]'s own doc for why this
/// crate conflates both into one field. `inline-block` is
/// `DisplayOutside::Inline` + `DisplayInside::FlowRoot` (real CSS's own
/// encoding, not a guess); every other inline-outside value maps to
/// `Inline`, and everything else falls through to `inside()` alone.
///
/// Stylo blockifies `outside()` on its own for contexts real CSS also
/// blockifies in (the root element, a flex/grid item, floats, absolute
/// positioning — <https://drafts.csswg.org/css-display/#blockify>) — a
/// `<span>` with no parent (a bare tree root) or a direct flex-item child
/// genuinely computes `outside: Block` even with `display: inline`
/// authored, the same as a real browser. Caught directly while writing this
/// slice's own tests: a `<span>` at tree root read back as `Block`, which
/// briefly looked like a bug in this function before nesting it under a
/// `<div>` (the realistic case) showed `Inline` as expected — Stylo was
/// already correct.
/// Whether this element is hidden by `display: none` that an author wrote (in a
/// stylesheet or a `style` attribute), and not by the default stylesheet, which
/// hides what is in an inline `<svg>` from layout.
fn authored_display_none(values: &ComputedValues) -> bool {
    use style::properties::PropertyDeclaration;
    use style::stylesheets::Origin;
    let hidden = values.get_box().display == style::values::computed::Display::None;
    if !hidden {
        return false;
    }
    let guard = shared_lock().read();
    values.rules().self_and_ancestors().any(|node| {
        node.cascade_level().origin() != Origin::UserAgent
            && node.style_source().is_some_and(|source| {
                source
                    .read(&guard)
                    .declarations()
                    .iter()
                    .any(|declaration| {
                        matches!(declaration, PropertyDeclaration::Display(display)
                        if *display == style::values::specified::Display::None)
                    })
            })
    })
}

fn to_display(display: style::values::computed::Display) -> FlorDisplay {
    use style::values::specified::box_::{DisplayInside as I, DisplayOutside as O};
    match (display.outside(), display.inside()) {
        (_, I::Contents) => FlorDisplay::Contents,
        (O::None, _) => FlorDisplay::None,
        (O::Inline, I::FlowRoot) => FlorDisplay::InlineBlock,
        (O::Inline, I::Flex) => FlorDisplay::InlineFlex,
        (O::Inline, I::Grid) => FlorDisplay::InlineGrid,
        (O::Inline, I::Table) => FlorDisplay::InlineTable,
        (O::Inline, _) => FlorDisplay::Inline,
        (O::TableCaption, _) => FlorDisplay::TableCaption,
        (_, I::Flex) => FlorDisplay::Flex,
        (_, I::Grid) => FlorDisplay::Grid,
        (_, I::FlowRoot) => FlorDisplay::FlowRoot,
        (_, I::Table) => FlorDisplay::Table,
        (_, I::TableRowGroup) => FlorDisplay::TableRowGroup,
        (_, I::TableHeaderGroup) => FlorDisplay::TableHeaderGroup,
        (_, I::TableFooterGroup) => FlorDisplay::TableFooterGroup,
        (_, I::TableRow) => FlorDisplay::TableRow,
        (_, I::TableColumn) => FlorDisplay::TableColumn,
        (_, I::TableColumnGroup) => FlorDisplay::TableColumnGroup,
        (_, I::TableCell) => FlorDisplay::TableCell,
        _ if display.is_list_item() => FlorDisplay::ListItem,
        _ => FlorDisplay::Block,
    }
}

fn to_float(value: style::values::computed::Float) -> crate::cascade::Float {
    use crate::cascade::Float;
    use style::values::computed::Float as F;
    match value {
        F::Left | F::InlineStart => Float::Left,
        F::Right | F::InlineEnd => Float::Right,
        F::None => Float::None,
    }
}

fn to_clear(value: style::values::computed::Clear) -> crate::cascade::Clear {
    use crate::cascade::Clear;
    use style::values::computed::Clear as C;
    match value {
        C::None => Clear::None,
        C::Left | C::InlineStart => Clear::Left,
        C::Right | C::InlineEnd => Clear::Right,
        C::Both => Clear::Both,
    }
}

/// `None` for `auto` (the initial value, and the only value that leaves a
/// flex/grid item painted in plain document order relative to its
/// siblings — see [`crate::cascade::ComputedStyle::z_index`]'s own doc for
/// what a `Some` value actually changes).
fn to_z_index(value: style::values::computed::position::ZIndex) -> Option<i32> {
    use style::values::generics::position::GenericZIndex;
    match value {
        GenericZIndex::Integer(index) => Some(index),
        GenericZIndex::Auto => None,
    }
}

/// Whether either axis's `overflow` clips its own content to the padding
/// box — see [`crate::cascade::ComputedStyle::overflow_clips`]'s own doc
/// for why a single bool is the right shape for this, not a loss of
/// precision.
fn to_overflow_clips(
    overflow_x: style::computed_values::overflow_x::T,
    overflow_y: style::computed_values::overflow_y::T,
) -> bool {
    use style::computed_values::overflow_x::T as OverflowX;
    use style::computed_values::overflow_y::T as OverflowY;
    !matches!(overflow_x, OverflowX::Visible) || !matches!(overflow_y, OverflowY::Visible)
}

/// Whether either axis's `overflow` is `hidden`, `scroll` or `auto`, which makes
/// the box a scroll container and so the root of its own block formatting
/// context. `clip` is not one.
fn to_overflow_makes_context(
    overflow_x: style::computed_values::overflow_x::T,
    overflow_y: style::computed_values::overflow_y::T,
) -> bool {
    use style::computed_values::overflow_x::T as OverflowX;
    use style::computed_values::overflow_y::T as OverflowY;
    !matches!(overflow_x, OverflowX::Visible | OverflowX::Clip)
        || !matches!(overflow_y, OverflowY::Visible | OverflowY::Clip)
}

/// Whether this axis's `overflow` is real CSS's `scroll`/`auto` — the two
/// keywords that make an axis's clipped-away content reachable again via
/// scrolling, unlike `hidden`/`clip` which clip it away for good. Each axis
/// is independent here, unlike [`to_overflow_clips`]'s deliberate OR across
/// both: an element can scroll on one axis while clipping-only on the
/// other.
fn to_overflow_scrolls_x(overflow_x: style::computed_values::overflow_x::T) -> bool {
    use style::computed_values::overflow_x::T as OverflowX;
    matches!(overflow_x, OverflowX::Scroll | OverflowX::Auto)
}

/// Same as [`to_overflow_scrolls_x`], for the `overflow-y` axis.
fn to_overflow_scrolls_y(overflow_y: style::computed_values::overflow_y::T) -> bool {
    use style::computed_values::overflow_y::T as OverflowY;
    matches!(overflow_y, OverflowY::Scroll | OverflowY::Auto)
}

fn to_flex_direction(value: style::computed_values::flex_direction::T) -> FlexDirection {
    use style::computed_values::flex_direction::T;
    match value {
        T::Row => FlexDirection::Row,
        T::RowReverse => FlexDirection::RowReverse,
        T::Column => FlexDirection::Column,
        T::ColumnReverse => FlexDirection::ColumnReverse,
    }
}

fn to_flex_wrap(value: style::computed_values::flex_wrap::T) -> FlexWrap {
    use style::computed_values::flex_wrap::T;
    match value {
        T::Nowrap => FlexWrap::NoWrap,
        T::Wrap => FlexWrap::Wrap,
        T::WrapReverse => FlexWrap::WrapReverse,
    }
}

/// Shared by `justify-content`/`align-content`, both a `ContentDistribution`
/// in Stylo — `normal` (no fallback alignment declared) maps to `None`,
/// distinct from every explicit keyword.
fn to_content_alignment(
    value: style::values::specified::align::ContentDistribution,
) -> Option<ContentAlignment> {
    use style::values::specified::align::AlignFlags;
    match value.primary().value() {
        AlignFlags::START => Some(ContentAlignment::Start),
        AlignFlags::END => Some(ContentAlignment::End),
        AlignFlags::LEFT => Some(ContentAlignment::Start),
        AlignFlags::RIGHT => Some(ContentAlignment::End),
        AlignFlags::FLEX_START => Some(ContentAlignment::FlexStart),
        AlignFlags::FLEX_END => Some(ContentAlignment::FlexEnd),
        AlignFlags::CENTER => Some(ContentAlignment::Center),
        AlignFlags::STRETCH => Some(ContentAlignment::Stretch),
        AlignFlags::SPACE_BETWEEN => Some(ContentAlignment::SpaceBetween),
        AlignFlags::SPACE_AROUND => Some(ContentAlignment::SpaceAround),
        AlignFlags::SPACE_EVENLY => Some(ContentAlignment::SpaceEvenly),
        _ => None,
    }
}

/// Shared by `align-items`/`align-self`, both an `AlignFlags` in Stylo —
/// `auto`/`normal` map to `None`, meaning "defer to the container's own
/// `align-items`" for `align-self`, or "stretch" for `align-items` (Taffy's
/// own default already matches CSS's real initial value there, so
/// `florui-layout` can supply that default itself rather than this
/// function inventing one).
fn to_item_alignment(value: style::values::specified::align::AlignFlags) -> Option<ItemAlignment> {
    use style::values::specified::align::AlignFlags;
    match value.value() {
        AlignFlags::STRETCH => Some(ItemAlignment::Stretch),
        AlignFlags::FLEX_START => Some(ItemAlignment::FlexStart),
        AlignFlags::FLEX_END => Some(ItemAlignment::FlexEnd),
        AlignFlags::SELF_START => Some(ItemAlignment::Start),
        AlignFlags::SELF_END => Some(ItemAlignment::End),
        AlignFlags::START => Some(ItemAlignment::Start),
        AlignFlags::END => Some(ItemAlignment::End),
        AlignFlags::LEFT => Some(ItemAlignment::Start),
        AlignFlags::RIGHT => Some(ItemAlignment::End),
        AlignFlags::CENTER => Some(ItemAlignment::Center),
        AlignFlags::BASELINE => Some(ItemAlignment::Baseline),
        _ => None,
    }
}

/// `flex-basis` shares `width`/`height`'s own generated size type
/// (`content` aside) — see [`to_optional_length`] for the same fallback
/// reasoning on anything this crate can't yet resolve to one pixel value.
fn to_flex_basis(
    value: &style::values::generics::flex::GenericFlexBasis<
        style::values::generics::length::GenericSize<
            style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
        >,
    >,
) -> Option<f32> {
    use style::values::generics::flex::GenericFlexBasis;
    match value {
        GenericFlexBasis::Content => None,
        GenericFlexBasis::Size(size) => to_optional_length(size),
    }
}

/// `column-gap`/`row-gap` share this generated type — `normal` (the CSS
/// initial value) is `0px`, same as this crate's own [`ComputedStyle::padding`]
/// treats anything it can't resolve to one pixel value.
fn to_gap(
    value: &style::values::generics::length::GenericLengthPercentageOrNormal<
        style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
    >,
) -> f32 {
    use style::values::generics::length::GenericLengthPercentageOrNormal;
    match value {
        GenericLengthPercentageOrNormal::LengthPercentage(lp) => {
            lp.0.to_length().map(|length| length.px()).unwrap_or(0.0)
        }
        GenericLengthPercentageOrNormal::Normal => 0.0,
    }
}

/// Resolves real CSS's whole comma-separated `font-family` preference list
/// down to the one distinction this crate's two embedded fonts actually
/// support: this crate has no way to honor a specific requested name
/// (`"Helvetica"`) or most other generics (`serif`, `cursive`, `fantasy`),
/// so only a first-preference `monospace` resolves to
/// [`FlorFontFamily::Monospace`] — everything else, including an empty
/// list (real CSS's own initial value), falls back to
/// [`FlorFontFamily::SansSerif`], this crate's stand-in default.
fn to_font_family(value: &style::values::computed::font::FontFamily) -> FlorFontFamily {
    use style::values::computed::font::{GenericFontFamily, SingleFontFamily};
    // The two embedded families, alone, are all but every element's: found
    // without building a list.
    match value.families.list.as_ref() {
        [] | [SingleFontFamily::Generic(GenericFontFamily::SansSerif)] => {
            return FlorFontFamily::SansSerif;
        }
        [SingleFontFamily::Generic(GenericFontFamily::Monospace)] => {
            return FlorFontFamily::Monospace;
        }
        _ => {}
    }
    let mut names: Vec<String> = Vec::new();
    for family in value.families.list.iter() {
        match family {
            // The embedded fonts that stand for these two are always found,
            // so nothing after one of them is ever reached.
            SingleFontFamily::Generic(GenericFontFamily::Monospace) => {
                names.push("\"Fira Mono\"".to_owned());
                break;
            }
            SingleFontFamily::Generic(GenericFontFamily::SansSerif) => {
                names.push("\"Open Sans\"".to_owned());
                break;
            }
            SingleFontFamily::Generic(generic) => {
                names.push(generic_family_keyword(generic).to_owned())
            }
            SingleFontFamily::FamilyName(name) => {
                let name = name
                    .name
                    .to_string()
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"");
                names.push(format!("\"{name}\""));
            }
        }
    }
    match names.as_slice() {
        [] => FlorFontFamily::SansSerif,
        [only] if only == "\"Open Sans\"" => FlorFontFamily::SansSerif,
        [only] if only == "\"Fira Mono\"" => FlorFontFamily::Monospace,
        _ => FlorFontFamily::Stack(std::sync::Arc::from(names.join(", ").as_str())),
    }
}

/// The CSS keyword of a generic family the system, not the embedded fonts,
/// answers for.
fn generic_family_keyword(
    generic: &style::values::computed::font::GenericFontFamily,
) -> &'static str {
    use style::values::computed::font::GenericFontFamily as G;
    match generic {
        G::Serif => "serif",
        G::Cursive => "cursive",
        G::Fantasy => "fantasy",
        G::SystemUi => "system-ui",
        G::Monospace => "monospace",
        _ => "sans-serif",
    }
}

fn to_absolute_rgba(color: &style::color::AbsoluteColor) -> Rgba {
    let srgb = color.to_color_space(style::color::ColorSpace::Srgb);
    Rgba {
        r: to_channel(srgb.components.0),
        g: to_channel(srgb.components.1),
        b: to_channel(srgb.components.2),
        a: to_channel(srgb.alpha),
    }
}

fn to_channel(component: f32) -> u8 {
    (component.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The pixel length of a `max-width`/`max-height`; `none` and anything
/// unresolvable (a percentage, a `calc()`) impose no limit.
fn to_max_length(
    value: &style::values::generics::length::GenericMaxSize<
        style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
    >,
) -> Option<f32> {
    use style::values::generics::length::GenericMaxSize;
    match value {
        GenericMaxSize::LengthPercentage(lp) => lp.0.to_length().map(|length| length.px()),
        _ => None,
    }
}

/// `box-sizing`: Stylo's keyword enum into this crate's own.
fn to_box_sizing(
    value: style::properties::longhands::box_sizing::computed_value::T,
) -> crate::cascade::BoxSizing {
    use style::properties::longhands::box_sizing::computed_value::T;
    match value {
        T::BorderBox => crate::cascade::BoxSizing::BorderBox,
        T::ContentBox => crate::cascade::BoxSizing::ContentBox,
    }
}

/// `min-width`/`min-height`: `auto` is `None`, anything else keeps its
/// percentage component for layout to resolve.
fn to_min_size(
    value: &style::values::generics::length::GenericSize<
        style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
    >,
) -> Option<FlorLengthPercentage> {
    use style::values::generics::length::GenericSize;
    match value {
        GenericSize::LengthPercentage(lp) => Some(to_length_percentage(&lp.0)),
        _ => None,
    }
}

fn to_line_height(value: &style::values::computed::font::LineHeight) -> crate::LineHeight {
    use style::values::computed::font::LineHeight as Stylo;
    match value {
        Stylo::Number(number) => crate::LineHeight::Number(number.0),
        Stylo::Length(length) => crate::LineHeight::Length(length.0.px()),
        _ => crate::LineHeight::Normal,
    }
}

/// The length is Stylo's; the `hanging` and `each-line` keywords its servo
/// build does not parse come through `--florui-text-indent-keywords` (see
/// `text_indent_adapter`).
fn to_text_indent(
    value: &style::values::computed::text::TextIndent,
    values: &ComputedValues,
) -> crate::TextIndent {
    let keywords = florui_property(values, |names| &names.text_indent_keywords);
    let has = |word: &str| {
        keywords
            .as_deref()
            .is_some_and(|css| css.split_whitespace().any(|k| k.eq_ignore_ascii_case(word)))
    };
    crate::TextIndent {
        length: to_length_percentage(&value.length),
        hanging: has("hanging"),
        each_line: has("each-line"),
    }
}

fn to_white_space_collapse(
    value: style::computed_values::white_space_collapse::T,
) -> crate::WhiteSpaceCollapse {
    use style::computed_values::white_space_collapse::T;
    match value {
        T::Collapse => crate::WhiteSpaceCollapse::Collapse,
        T::Preserve => crate::WhiteSpaceCollapse::Preserve,
        T::PreserveBreaks => crate::WhiteSpaceCollapse::PreserveBreaks,
        T::BreakSpaces => crate::WhiteSpaceCollapse::BreakSpaces,
    }
}

fn to_word_break(value: style::computed_values::word_break::T) -> crate::WordBreak {
    use style::computed_values::word_break::T;
    match value {
        T::Normal => crate::WordBreak::Normal,
        T::BreakAll => crate::WordBreak::BreakAll,
        T::KeepAll => crate::WordBreak::KeepAll,
    }
}

fn to_overflow_wrap(value: style::computed_values::overflow_wrap::T) -> crate::OverflowWrap {
    use style::computed_values::overflow_wrap::T;
    match value {
        T::Normal => crate::OverflowWrap::Normal,
        T::BreakWord => crate::OverflowWrap::BreakWord,
        T::Anywhere => crate::OverflowWrap::Anywhere,
    }
}

/// The end side of `text-overflow`: with two values the second is the end of a
/// left-to-right line.
fn to_text_overflow(value: &style::values::computed::TextOverflow) -> crate::TextOverflow {
    use style::values::specified::text::TextOverflowSide;
    match &value.second {
        TextOverflowSide::Clip => crate::TextOverflow::Clip,
        TextOverflowSide::Ellipsis => crate::TextOverflow::Ellipsis,
        TextOverflowSide::String(text) => crate::TextOverflow::Text(text.to_string()),
    }
}

fn to_vertical_align(value: &style::values::computed::VerticalAlign) -> crate::VerticalAlign {
    use style::values::generics::box_::{VerticalAlign as Value, VerticalAlignKeyword as Keyword};
    match value {
        Value::Keyword(Keyword::Sub) => crate::VerticalAlign::Sub,
        Value::Keyword(Keyword::Super) => crate::VerticalAlign::Super,
        Value::Keyword(Keyword::TextTop) => crate::VerticalAlign::TextTop,
        Value::Keyword(Keyword::TextBottom) => crate::VerticalAlign::TextBottom,
        Value::Keyword(Keyword::Middle) => crate::VerticalAlign::Middle,
        Value::Keyword(Keyword::Top) => crate::VerticalAlign::Top,
        Value::Keyword(Keyword::Bottom) => crate::VerticalAlign::Bottom,
        Value::Keyword(_) => crate::VerticalAlign::Baseline,
        Value::Length(length) => crate::VerticalAlign::Raise(to_length_percentage(length)),
    }
}

fn to_block_align(value: style::values::computed::text::TextAlign) -> crate::BlockAlign {
    use style::values::specified::text::TextAlignKeyword as Keyword;
    match value {
        Keyword::MozCenter => crate::BlockAlign::Center,
        Keyword::MozLeft => crate::BlockAlign::Left,
        Keyword::MozRight => crate::BlockAlign::Right,
        _ => crate::BlockAlign::None,
    }
}

fn to_text_align(value: style::values::computed::text::TextAlign) -> crate::TextAlign {
    use style::values::specified::text::TextAlignKeyword as Keyword;
    match value {
        Keyword::Center | Keyword::MozCenter => crate::TextAlign::Center,
        Keyword::Right | Keyword::MozRight | Keyword::End => crate::TextAlign::End,
        Keyword::Justify => crate::TextAlign::Justify,
        _ => crate::TextAlign::Start,
    }
}

/// The fraction of a `width`/`height` that is a plain percentage, for layout
/// to resolve against the containing block.
fn to_size_percentage(
    value: &style::values::generics::length::GenericSize<
        style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
    >,
) -> Option<f32> {
    use style::values::generics::length::GenericSize;
    match value {
        GenericSize::LengthPercentage(lp) => lp.0.to_percentage().map(|percentage| percentage.0),
        _ => None,
    }
}

/// `None` for anything without a single resolved pixel length — `auto`,
/// a percentage, or a `calc()` mixing the two — since nothing downstream
/// of this crate can resolve a percentage without a containing size yet;
/// treating it as `auto` is the closest honest fallback available today.
fn to_optional_length(
    value: &style::values::generics::length::GenericSize<
        style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
    >,
) -> Option<f32> {
    use style::values::generics::length::GenericSize;
    match value {
        GenericSize::LengthPercentage(lp) => lp.0.to_length().map(|length| length.px()),
        _ => None,
    }
}

/// Same fallback reasoning as [`to_optional_length`] (`auto`, a
/// percentage, or a `calc()` all become `None`), for margin's own
/// generated value type — real CSS margins allow negative lengths, so
/// unlike `width`/`height` there is no `NonNegative` wrapper here.
fn to_optional_margin(
    value: &style::values::generics::length::GenericMargin<
        style::values::computed::LengthPercentage,
    >,
) -> Option<f32> {
    use style::values::generics::length::GenericMargin;
    match value {
        GenericMargin::LengthPercentage(lp) => lp.to_length().map(|length| length.px()),
        _ => None,
    }
}

/// What `values` holds that only layout can turn into a number: a percentage
/// where this crate keeps no percentage (the properties that have one
/// natively are the exception for a plain one), a math function over one, and
/// the keywords naming a size that depends on content or room.
fn to_deferred(values: &ComputedValues) -> Vec<crate::cascade::Deferred> {
    use crate::cascade::DeferredSlot as Slot;
    use style::values::computed::LengthPercentage;
    use style::values::generics::NonNegative;
    use style::values::generics::length::{GenericMargin, GenericMaxSize, GenericSize};
    use style::values::generics::position::GenericInset;

    type Out = Vec<crate::cascade::Deferred>;

    fn math(out: &mut Out, slot: Slot, lp: &LengthPercentage) {
        let natively = matches!(
            slot,
            Slot::Width
                | Slot::Height
                | Slot::MinWidth
                | Slot::MinHeight
                | Slot::InsetTop
                | Slot::InsetRight
                | Slot::InsetBottom
                | Slot::InsetLeft
        );
        if lp.has_percentage() && !(natively && lp.to_percentage().is_some()) {
            out.push(crate::cascade::Deferred {
                slot,
                value: crate::cascade::DeferredValue::Math(crate::cascade::Calc::new(lp)),
            });
        }
    }

    fn keyword(out: &mut Out, slot: Slot, keyword: crate::cascade::SizeKeyword) {
        out.push(crate::cascade::Deferred {
            slot,
            value: crate::cascade::DeferredValue::Keyword(keyword),
        });
    }

    fn fit_content_of(out: &mut Out, slot: Slot, lp: &LengthPercentage) {
        out.push(crate::cascade::Deferred {
            slot,
            value: crate::cascade::DeferredValue::FitContentOf(crate::cascade::Calc::new(lp)),
        });
    }

    fn size(out: &mut Out, slot: Slot, value: &GenericSize<NonNegative<LengthPercentage>>) {
        use crate::cascade::SizeKeyword as K;
        match value {
            GenericSize::LengthPercentage(lp) => math(out, slot, &lp.0),
            GenericSize::MaxContent => keyword(out, slot, K::MaxContent),
            GenericSize::MinContent => keyword(out, slot, K::MinContent),
            GenericSize::FitContent => keyword(out, slot, K::FitContent),
            GenericSize::Stretch | GenericSize::WebkitFillAvailable => {
                keyword(out, slot, K::Stretch)
            }
            GenericSize::FitContentFunction(lp) => fit_content_of(out, slot, &lp.0),
            _ => {}
        }
    }

    fn max_size(out: &mut Out, slot: Slot, value: &GenericMaxSize<NonNegative<LengthPercentage>>) {
        use crate::cascade::SizeKeyword as K;
        match value {
            GenericMaxSize::LengthPercentage(lp) => math(out, slot, &lp.0),
            GenericMaxSize::MaxContent => keyword(out, slot, K::MaxContent),
            GenericMaxSize::MinContent => keyword(out, slot, K::MinContent),
            GenericMaxSize::FitContent => keyword(out, slot, K::FitContent),
            GenericMaxSize::Stretch | GenericMaxSize::WebkitFillAvailable => {
                keyword(out, slot, K::Stretch)
            }
            GenericMaxSize::FitContentFunction(lp) => fit_content_of(out, slot, &lp.0),
            _ => {}
        }
    }

    let position = values.get_position();
    let margin = values.get_margin();
    let padding = values.get_padding();
    let mut out = Out::new();
    size(&mut out, Slot::Width, &position.width);
    size(&mut out, Slot::Height, &position.height);
    size(&mut out, Slot::MinWidth, &position.min_width);
    size(&mut out, Slot::MinHeight, &position.min_height);
    max_size(&mut out, Slot::MaxWidth, &position.max_width);
    max_size(&mut out, Slot::MaxHeight, &position.max_height);
    if let style::values::generics::flex::GenericFlexBasis::Size(basis) = &position.flex_basis {
        size(&mut out, Slot::FlexBasis, basis);
    }
    for (slot, value) in [
        (Slot::MarginTop, &margin.margin_top),
        (Slot::MarginRight, &margin.margin_right),
        (Slot::MarginBottom, &margin.margin_bottom),
        (Slot::MarginLeft, &margin.margin_left),
    ] {
        if let GenericMargin::LengthPercentage(lp) = value {
            math(&mut out, slot, lp);
        }
    }
    for (slot, value) in [
        (Slot::PaddingTop, &padding.padding_top),
        (Slot::PaddingRight, &padding.padding_right),
        (Slot::PaddingBottom, &padding.padding_bottom),
        (Slot::PaddingLeft, &padding.padding_left),
    ] {
        math(&mut out, slot, &value.0);
    }
    for (slot, value) in [
        (Slot::InsetTop, &position.top),
        (Slot::InsetRight, &position.right),
        (Slot::InsetBottom, &position.bottom),
        (Slot::InsetLeft, &position.left),
    ] {
        if let GenericInset::LengthPercentage(lp) = value {
            math(&mut out, slot, lp);
        }
    }
    for (slot, value) in [
        (Slot::ColumnGap, &position.column_gap),
        (Slot::RowGap, &position.row_gap),
    ] {
        if let style::values::generics::length::GenericLengthPercentageOrNormal::LengthPercentage(
            lp,
        ) = value
        {
            math(&mut out, slot, &lp.0);
        }
    }
    out
}

/// A length or a percentage as the mirrored properties carry them, text:
/// pixels (a font-relative unit resolved against `font_size`) or a fraction.
enum CssSize {
    Px(f32),
    Percent(f32),
}

fn parse_css_size(css: &str, font_size: f32) -> Option<CssSize> {
    let css = css.trim().to_ascii_lowercase();
    let split = css.find(|c: char| c.is_ascii_alphabetic() || c == '%')?;
    let (number, unit) = css.split_at(split);
    let number: f32 = number.parse().ok()?;
    let px = match unit {
        "%" => return Some(CssSize::Percent(number / 100.0)),
        "px" => number,
        "em" => number * font_size,
        "rem" => number * 16.0,
        "ex" => number * font_size * 0.5,
        "pt" => number * 4.0 / 3.0,
        "pc" => number * 16.0,
        "in" => number * 96.0,
        "cm" => number * 96.0 / 2.54,
        "mm" => number * 96.0 / 25.4,
        "q" => number * 96.0 / 101.6,
        _ => return None,
    };
    Some(CssSize::Px(px))
}

fn to_text_decoration(values: &ComputedValues, font_size: f32) -> crate::TextDecoration {
    use crate::{
        DecorationLines, DecorationStyle, DecorationThickness, SkipInk, UnderlineOffset,
        UnderlinePosition,
    };
    use style::computed_values::text_decoration_style::T as Style;
    use style::values::computed::TextDecorationLine as Line;

    let own = values.get_text();
    let line = own.text_decoration_line;
    let word = |name: &str| {
        mirrored(values, name)
            .map(|css| css.to_ascii_lowercase())
            .unwrap_or_default()
    };
    crate::TextDecoration {
        lines: DecorationLines {
            underline: line.contains(Line::UNDERLINE),
            overline: line.contains(Line::OVERLINE),
            line_through: line.contains(Line::LINE_THROUGH),
        },
        style: match own.text_decoration_style {
            Style::Double => DecorationStyle::Double,
            Style::Dotted => DecorationStyle::Dotted,
            Style::Dashed => DecorationStyle::Dashed,
            Style::Wavy => DecorationStyle::Wavy,
            _ => DecorationStyle::Solid,
        },
        color: own
            .text_decoration_color
            .as_absolute()
            .map(to_absolute_rgba),
        thickness: match {
            // The rules are only searched for a thickness that is there to read.
            let read = word("text-decoration-thickness");
            if read.is_empty() || !declares_mirrored(values, "text-decoration-thickness") {
                String::new()
            } else {
                read
            }
        }
        .as_str()
        {
            "from-font" => DecorationThickness::FromFont,
            css => match parse_css_size(css, font_size) {
                Some(CssSize::Px(px)) => DecorationThickness::Length(px),
                Some(CssSize::Percent(fraction)) => DecorationThickness::Percent(fraction),
                None => DecorationThickness::Auto,
            },
        },
        underline_offset: match parse_css_size(&word("text-underline-offset"), font_size) {
            Some(CssSize::Px(px)) => UnderlineOffset::Length(px),
            Some(CssSize::Percent(fraction)) => UnderlineOffset::Percent(fraction),
            None => UnderlineOffset::Auto,
        },
        skip_ink: match word("text-decoration-skip-ink").as_str() {
            "none" => SkipInk::None,
            "all" => SkipInk::All,
            _ => SkipInk::Auto,
        },
        position: match word("text-underline-position").as_str() {
            "under" => UnderlinePosition::Under,
            "left" => UnderlinePosition::Left,
            "right" => UnderlinePosition::Right,
            _ => UnderlinePosition::Auto,
        },
    }
}

/// Whether the rules applying to this very element (not an ancestor's) declare the
/// mirrored `name`: it is a custom property and so inherits, while the property
/// it stands for does not.
fn declares_mirrored(values: &ComputedValues, name: &str) -> bool {
    use style::properties::PropertyDeclaration;
    let Some(index) = crate::property_mirror::MIRRORED
        .iter()
        .position(|mirrored| *mirrored == name)
    else {
        return false;
    };
    // An element none of whose rules declared a custom property, and that
    // inherits none, has nothing to find.
    if values.custom_properties().is_empty() {
        return false;
    }
    let guard = shared_lock().read();
    FLOR_NAMES.with(|names| {
        values.rules().self_and_ancestors().any(|node| {
            node.style_source().is_some_and(|source| {
                source
                    .read(&guard)
                    .declarations()
                    .iter()
                    .any(|declaration| {
                        matches!(declaration, PropertyDeclaration::Custom(custom)
                        if custom.name == names.mirrors[index])
                    })
            })
        })
    })
}

/// The SVG properties this element's own rules declare (see
/// [`crate::property_mirror::SVG_PROPERTIES`]), with their values as written; the
/// rule tree is read once for all of them.
fn to_svg_properties(values: &ComputedValues) -> Vec<(&'static str, String)> {
    use style::properties::PropertyDeclaration;
    // Almost no element has a custom property: nothing to look for then.
    if values.custom_properties().is_empty() {
        return Vec::new();
    }
    let mirrors = crate::property_mirror::MIRRORED;
    let wanted: Vec<(&'static str, usize)> = crate::property_mirror::SVG_PROPERTIES
        .iter()
        .filter_map(|name| {
            mirrors
                .iter()
                .position(|mirrored| mirrored == name)
                .map(|index| (*name, index))
        })
        .collect();
    let mut declared = Vec::new();
    {
        let guard = shared_lock().read();
        FLOR_NAMES.with(|names| {
            for node in values.rules().self_and_ancestors() {
                let Some(source) = node.style_source() else {
                    continue;
                };
                for declaration in source.read(&guard).declarations() {
                    let PropertyDeclaration::Custom(custom) = declaration else {
                        continue;
                    };
                    if let Some((name, _)) = wanted
                        .iter()
                        .find(|(_, index)| custom.name == names.mirrors[*index])
                        && !declared.contains(name)
                    {
                        declared.push(*name);
                    }
                }
            }
        });
    }
    declared
        .into_iter()
        .filter_map(|name| mirrored(values, name).map(|value| (name, value)))
        .collect()
}

/// `font-synthesis` and its longhands: the shorthand says which forms are
/// allowed (`none` none of them), a longhand then says `auto` or `none`.
fn to_font_synthesis(values: &ComputedValues) -> crate::FontSynthesis {
    let mut synthesis = crate::FontSynthesis::default();
    if let Some(css) = mirrored(values, "font-synthesis") {
        let css = css.to_ascii_lowercase();
        let allowed = |word: &str| css.split_whitespace().any(|w| w == word);
        let none = allowed("none");
        synthesis = crate::FontSynthesis {
            weight: !none && allowed("weight"),
            style: !none && allowed("style"),
            small_caps: !none && allowed("small-caps"),
            position: !none && allowed("position"),
        };
    }
    let longhand = |name: &str, current: bool| match mirrored(values, name)
        .map(|css| css.to_ascii_lowercase())
        .as_deref()
    {
        Some("none") => false,
        Some("auto") => true,
        _ => current,
    };
    synthesis.style = longhand("font-synthesis-style", synthesis.style);
    synthesis.small_caps = longhand("font-synthesis-small-caps", synthesis.small_caps);
    synthesis.position = longhand("font-synthesis-position", synthesis.position);
    if values.get_font().font_synthesis_weight
        == style::computed_values::font_synthesis_weight::T::None
    {
        synthesis.weight = false;
    }
    synthesis
}

fn to_font_style(value: &style::values::computed::font::FontStyle) -> crate::FontStyle {
    use style::values::computed::font::FontStyle;
    if *value == FontStyle::ITALIC {
        return crate::FontStyle::Italic;
    }
    match value.oblique_degrees() {
        0.0 => crate::FontStyle::Normal,
        degrees => crate::FontStyle::Oblique(degrees),
    }
}

fn to_text_transform(value: style::values::computed::TextTransform) -> crate::TextTransform {
    use style::values::specified::TextTransform as Flag;
    crate::TextTransform {
        case: if value.contains(Flag::UPPERCASE) {
            crate::TextCase::Uppercase
        } else if value.contains(Flag::LOWERCASE) {
            crate::TextCase::Lowercase
        } else if value.contains(Flag::CAPITALIZE) {
            crate::TextCase::Capitalize
        } else {
            crate::TextCase::None
        },
        full_width: value.contains(Flag::FULL_WIDTH),
        full_size_kana: value.contains(Flag::FULL_SIZE_KANA),
    }
}

/// The engine's own `font-variant-caps` knows only `normal` and `small-caps`
/// (the `font` shorthand sets it); every declaration of the property, and of
/// `font-variant`, is carried by the mirror, which wins when there is one.
fn to_font_variant_caps(values: &ComputedValues) -> crate::FontVariantCaps {
    use crate::FontVariantCaps as Caps;
    match mirrored(values, "font-variant-caps")
        .map(|css| css.to_ascii_lowercase())
        .as_deref()
    {
        Some("small-caps") => Caps::SmallCaps,
        Some("all-small-caps") => Caps::AllSmallCaps,
        Some("petite-caps") => Caps::PetiteCaps,
        Some("all-petite-caps") => Caps::AllPetiteCaps,
        Some("unicase") => Caps::Unicase,
        Some("titling-caps") => Caps::TitlingCaps,
        Some("normal") => Caps::Normal,
        _ => match values.get_font().font_variant_caps {
            style::computed_values::font_variant_caps::T::SmallCaps => Caps::SmallCaps,
            _ => Caps::Normal,
        },
    }
}

/// The OpenType features the `font-variant-*` properties, `font-kerning` and
/// `font-feature-settings` ask for, as CSS (`"liga" 0, "tnum"`), settings
/// last so they win.
fn to_font_features(values: &ComputedValues) -> Option<std::sync::Arc<str>> {
    let mut features: Vec<String> = Vec::new();
    let mut add = |tag: &str, value: u32| features.push(format!("\"{tag}\" {value}"));
    let words = |css: Option<String>| -> Vec<String> {
        css.map(|css| {
            css.split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default()
    };
    for word in words(mirrored(values, "font-variant-ligatures")) {
        match word.as_str() {
            "none" => {
                add("liga", 0);
                add("clig", 0);
                add("dlig", 0);
                add("hlig", 0);
                add("calt", 0);
            }
            "common-ligatures" => {
                add("liga", 1);
                add("clig", 1);
            }
            "no-common-ligatures" => {
                add("liga", 0);
                add("clig", 0);
            }
            "discretionary-ligatures" => add("dlig", 1),
            "no-discretionary-ligatures" => add("dlig", 0),
            "historical-ligatures" => add("hlig", 1),
            "no-historical-ligatures" => add("hlig", 0),
            "contextual" => add("calt", 1),
            "no-contextual" => add("calt", 0),
            _ => {}
        }
    }
    for word in words(mirrored(values, "font-variant-numeric")) {
        match word.as_str() {
            "lining-nums" => add("lnum", 1),
            "oldstyle-nums" => add("onum", 1),
            "proportional-nums" => add("pnum", 1),
            "tabular-nums" => add("tnum", 1),
            "diagonal-fractions" => add("frac", 1),
            "stacked-fractions" => add("afrc", 1),
            "ordinal" => add("ordn", 1),
            "slashed-zero" => add("zero", 1),
            _ => {}
        }
    }
    for word in words(mirrored(values, "font-variant-east-asian")) {
        match word.as_str() {
            "jis78" => add("jp78", 1),
            "jis83" => add("jp83", 1),
            "jis90" => add("jp90", 1),
            "jis04" => add("jp04", 1),
            "simplified" => add("smpl", 1),
            "traditional" => add("trad", 1),
            "full-width" => add("fwid", 1),
            "proportional-width" => add("pwid", 1),
            "ruby" => add("ruby", 1),
            _ => {}
        }
    }
    for word in words(mirrored(values, "font-variant-position")) {
        match word.as_str() {
            "sub" => add("subs", 1),
            "super" => add("sups", 1),
            _ => {}
        }
    }
    match mirrored(values, "font-kerning")
        .map(|css| css.to_ascii_lowercase())
        .as_deref()
    {
        Some("none") => add("kern", 0),
        Some("normal") => add("kern", 1),
        _ => {}
    }
    if let Some(settings) = mirrored(values, "font-feature-settings")
        && !settings.eq_ignore_ascii_case("normal")
    {
        features.push(settings);
    }
    (!features.is_empty()).then(|| std::sync::Arc::from(features.join(", ").as_str()))
}

/// `font-variation-settings`, carried by the mirror as written (`"wght" 650`).
fn to_font_variations(values: &ComputedValues) -> Option<std::sync::Arc<str>> {
    mirrored(values, "font-variation-settings")
        .filter(|css| !css.eq_ignore_ascii_case("normal"))
        .map(|css| std::sync::Arc::from(css.as_str()))
}

fn to_visibility(
    value: style::properties::longhands::visibility::computed_value::T,
) -> crate::cascade::Visibility {
    use style::properties::longhands::visibility::computed_value::T;
    match value {
        T::Visible => crate::cascade::Visibility::Visible,
        T::Hidden => crate::cascade::Visibility::Hidden,
        T::Collapse => crate::cascade::Visibility::Collapse,
    }
}

fn to_position(value: style::values::computed::PositionProperty) -> crate::cascade::Position {
    use style::values::computed::PositionProperty;
    match value {
        PositionProperty::Static => crate::cascade::Position::Static,
        PositionProperty::Relative => crate::cascade::Position::Relative,
        PositionProperty::Absolute => crate::cascade::Position::Absolute,
        PositionProperty::Fixed => crate::cascade::Position::Fixed,
        PositionProperty::Sticky => crate::cascade::Position::Sticky,
    }
}

/// Same fallback reasoning as [`to_optional_margin`] (`auto`, a
/// percentage, or a `calc()` all become `None`) — `top`/`right`/`bottom`/
/// `left` share `margin`'s own generated value shape (a plain
/// `LengthPercentage`, real CSS inset values allow negative lengths too).
/// `None` for `auto` (real CSS's own initial value for every inset edge).
/// A declared length-percentage is kept unresolved — see
/// [`crate::cascade::ComputedStyle::inset`]'s own doc for why, unlike
/// [`to_optional_length`]'s own collapse-to-`None`.
fn to_optional_inset(
    value: &style::values::computed::position::Inset,
) -> Option<FlorLengthPercentage> {
    use style::values::generics::position::GenericInset;
    match value {
        GenericInset::LengthPercentage(lp) => Some(to_length_percentage(lp)),
        _ => None,
    }
}

/// `0.0` for anything without a single resolved pixel length — a
/// percentage or a `calc()` mixing the two — since real CSS padding has
/// no `auto` to fall back to; see [`to_optional_length`] for the same
/// fallback reasoning on `width`/`height`/margin.
fn to_length(
    value: &style::values::generics::NonNegative<style::values::computed::LengthPercentage>,
) -> f32 {
    value.0.to_length().map(|length| length.px()).unwrap_or(0.0)
}

#[cfg(test)]
mod attr_selector_tests {
    use florui::prelude::*;

    use crate::animation::AnimationTimeline;
    use crate::cascade::{Viewport, compute};
    use crate::interaction::InteractionState;
    use crate::stylesheet_parse::parse_stylesheet;
    use crate::tree::Arena;

    #[test]
    fn an_attribute_selector_matches_a_real_attribute_value() {
        let tree: Element = view! {
            <div>
                <option selected="true">{"A"}</option>
                <option selected="false">{"B"}</option>
            </div>
        };
        let arena = Arena::build(&tree);
        let options = arena.find_all(|a, id| a.tag(id) == "option");
        let rules = parse_stylesheet("option[selected=\"true\"] { color: #ff0000; }").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut AnimationTimeline::default(),
        );
        assert_eq!(
            computed[&options[0]].color,
            crate::color::Rgba::opaque(0xff, 0x00, 0x00)
        );
        assert_ne!(
            computed[&options[1]].color,
            crate::color::Rgba::opaque(0xff, 0x00, 0x00)
        );
    }

    #[test]
    fn a_bare_attribute_selector_matches_presence_regardless_of_value() {
        let tree: Element = view! {
            <div>
                <option value="x">{"A"}</option>
                <option>{"B"}</option>
            </div>
        };
        let arena = Arena::build(&tree);
        let options = arena.find_all(|a, id| a.tag(id) == "option");
        let rules = parse_stylesheet("option[value] { color: #00ff00; }").unwrap();
        let computed = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut AnimationTimeline::default(),
        );
        assert_eq!(
            computed[&options[0]].color,
            crate::color::Rgba::opaque(0x00, 0xff, 0x00)
        );
        assert_ne!(
            computed[&options[1]].color,
            crate::color::Rgba::opaque(0x00, 0xff, 0x00)
        );
    }
}

#[cfg(test)]
mod placeholder_color_tests {
    use super::parse_color;
    use crate::color::Rgba;

    #[test]
    fn hex_rgb_and_named_colors_parse_and_junk_does_not() {
        assert_eq!(parse_color("#ff0000"), Some(Rgba::opaque(255, 0, 0)));
        assert_eq!(parse_color("#f00"), Some(Rgba::opaque(255, 0, 0)));
        assert_eq!(
            parse_color("rgb(10, 20, 30)"),
            Some(Rgba::opaque(10, 20, 30))
        );
        assert_eq!(parse_color("rgb(10 20 30)"), Some(Rgba::opaque(10, 20, 30)));
        assert_eq!(
            parse_color("rgba(0, 0, 255, 0.5)"),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 255,
                a: 128
            })
        );
        assert_eq!(parse_color("rgb(0 0 255 / 50%)").map(|c| c.a), Some(128));
        assert_eq!(
            parse_color("rebeccapurple"),
            Some(Rgba::opaque(102, 51, 153))
        );
        assert_eq!(parse_color("transparent"), Some(Rgba::TRANSPARENT));
        assert_eq!(parse_color("not-a-color"), None);
        assert_eq!(parse_color("rgb(1, 2)"), None);
    }
}

#[cfg(test)]
mod style_sharing_tests {
    use florui::prelude::*;

    use crate::{
        AnimationTimeline, Arena, InteractionState, StyleMap, Viewport, compute, parse_stylesheet,
    };

    const GREEN: u8 = 255;

    fn class(name: &str) -> Vec<(String, String)> {
        vec![("class".to_string(), name.to_string())]
    }

    /// `rows` rows of one cell each under one list.
    fn rows(count: usize) -> Element {
        Element::node(
            "div",
            class("list"),
            (0..count)
                .map(|_| {
                    Element::node(
                        "div",
                        class("row"),
                        vec![Element::node("span", class("cell"), vec![])],
                    )
                })
                .collect(),
        )
    }

    /// The styles of `arena` under `css` with `hovered` and its ancestors hovered.
    fn styles(arena: &Arena, css: &str, hovered: Option<usize>) -> StyleMap {
        let mut state = InteractionState::new();
        if let Some(node) = hovered {
            state = state.with_hovered(node);
            let mut up = arena.parent(node);
            while let Some(parent) = up {
                state = state.with_hovered(parent);
                up = arena.parent(parent);
            }
        }
        compute(
            arena,
            &parse_stylesheet(css).unwrap(),
            &state,
            Viewport::default(),
            &mut AnimationTimeline::default(),
        )
    }

    /// The nodes with `class` that the styles turn green.
    fn green(arena: &Arena, styles: &StyleMap, class: &str) -> Vec<usize> {
        arena
            .find_all(|a, id| a.classes(id).iter().any(|c| c == class))
            .into_iter()
            .enumerate()
            .filter(|&(_, id)| styles[&id].color.g == GREEN && styles[&id].color.r == 0)
            .map(|(position, _)| position)
            .collect()
    }

    #[test]
    fn the_cell_of_the_hovered_row_alone_takes_the_rule_of_a_hovered_ancestor() {
        let arena = Arena::build(&rows(6));
        let all = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"));
        // Rows alike in every style but their state, with and without a rule of
        // their own, which is what makes Stylo take their cells for the same.
        for css in [
            ".row:hover .cell { color: #00ff00; }",
            ".row:hover .cell { color: #00ff00; } .row { opacity: 0.9; }",
            ".row:hover > .cell { color: #00ff00; } .cell { width: 3px; }",
        ] {
            for (position, &row) in all.iter().enumerate() {
                let result = styles(&arena, css, Some(row));
                assert_eq!(
                    green(&arena, &result, "cell"),
                    vec![position],
                    "hovering row {position} under {css}"
                );
            }
            let none = styles(&arena, css, None);
            assert!(green(&arena, &none, "cell").is_empty(), "nothing hovered");
        }
    }

    #[test]
    fn the_rows_after_the_hovered_one_take_the_sibling_rules_and_no_other() {
        let arena = Arena::build(&rows(6));
        let all = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"));
        for (position, &hovered) in all.iter().enumerate() {
            let adjacent = styles(
                &arena,
                ".row:hover + .row { color: #00ff00; }",
                Some(hovered),
            );
            let expected: Vec<usize> = (position + 1 < 6)
                .then_some(position + 1)
                .into_iter()
                .collect();
            assert_eq!(
                green(&arena, &adjacent, "row"),
                expected,
                "next sibling of {position}"
            );

            // The same below the sibling: nodes under rows that differ only in what
            // is before them.
            let below = styles(
                &arena,
                ".row:hover + .row .cell { color: #00ff00; }",
                Some(hovered),
            );
            assert_eq!(
                green(&arena, &below, "cell"),
                expected,
                "cell of the next sibling of {position}"
            );

            let later = styles(
                &arena,
                ".row:hover ~ .row { color: #00ff00; }",
                Some(hovered),
            );
            assert_eq!(
                green(&arena, &later, "row"),
                (position + 1..6).collect::<Vec<_>>(),
                "later siblings of {position}"
            );
        }
    }

    #[test]
    fn the_label_after_a_checked_box_alone_takes_the_checked_sibling_rule() {
        let pair = |checked: bool| {
            let mut attrs = vec![("type".to_string(), "checkbox".to_string())];
            if checked {
                attrs.push(("checked".to_string(), "true".to_string()));
            }
            vec![
                Element::node("input", attrs, vec![]),
                Element::node("label", class("name"), vec![]),
            ]
        };
        let tree = Element::node(
            "div",
            vec![],
            [true, false, false, true, false]
                .into_iter()
                .flat_map(pair)
                .collect(),
        );
        let arena = Arena::build(&tree);
        let result = styles(&arena, "input:checked + .name { color: #00ff00; }", None);
        assert_eq!(green(&arena, &result, "name"), vec![0, 3]);
    }

    #[test]
    fn the_cells_of_the_hovered_card_alone_take_a_rule_two_levels_down() {
        let card = |rows: usize| {
            Element::node(
                "div",
                class("card"),
                (0..rows)
                    .map(|_| {
                        Element::node(
                            "div",
                            class("row"),
                            vec![Element::node("span", class("cell"), vec![])],
                        )
                    })
                    .collect(),
            )
        };
        let tree = Element::node("div", vec![], vec![card(3), card(3), card(3)]);
        let arena = Arena::build(&tree);
        let cards = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "card"));
        for (position, &hovered) in cards.iter().enumerate() {
            let result = styles(
                &arena,
                ".card:hover .row .cell { color: #00ff00; }",
                Some(hovered),
            );
            let expected: Vec<usize> = (position * 3..position * 3 + 3).collect();
            assert_eq!(
                green(&arena, &result, "cell"),
                expected,
                "hovering card {position}"
            );
        }
    }
}

impl NodeSlot {
    /// A hash of what a selector with an ancestor in it can tell this node by: its tag,
    /// its id, its classes and its attributes.
    fn identity(&self) -> u64 {
        let base = *self.identity_hash.get_or_init(|| self.hash_identity());
        let has = self.has.borrow();
        if has.is_empty() {
            return base;
        }
        // The `:has()` it has changes with the tree below it and the state, so it is
        // not part of what is kept.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        base.hash(&mut hasher);
        has.hash(&mut hasher);
        hasher.finish()
    }

    fn hash_identity(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.tag.hash(&mut hasher);
        self.id_attr.hash(&mut hasher);
        self.classes.hash(&mut hasher);
        self.attrs.hash(&mut hasher);
        hasher.finish()
    }
}
