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
    Appearance as FlorAppearance, AspectRatio as FlorAspectRatio, BorderSide as FlorBorderSide,
    BoxShadow as FlorBoxShadow, ComputedStyle, ContainerType as FlorContainerType,
    ContentAlignment, Corners, Display as FlorDisplay, Edges, FilterFunction as FlorFilterFunction,
    FlexDirection, FlexWrap, FontFamily as FlorFontFamily, ItemAlignment,
    LengthPercentage as FlorLengthPercentage, ObjectFit as FlorObjectFit, Resize as FlorResize,
    TransformFunction as FlorTransformFunction, Viewport as FlorViewport,
};
use crate::color::Rgba;
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
    state: ElementState,
    data: AtomicRefCell<ElementData>,
    dirty_descendants: Cell<bool>,
    local_name: BorrowedLocalName,
    namespace: BorrowedNamespaceUrl,
}

/// Owns every node's [`NodeSlot`] in one `Vec` sized exactly once up
/// front, so pushing never reallocates and the raw pointers `NodeSlot`s
/// hold to each other (set in a second pass, once every slot has its
/// final address) stay valid for the tree's whole lifetime.
struct StyloTree {
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
}

impl StyloTree {
    /// Builds a self-contained copy of every node reachable from `arena`'s
    /// roots, synthesizing a single container root when there is more
    /// than one (a `view!` `Fragment` can produce several) so Stylo
    /// always has exactly one document element to resolve from.
    fn new(
        arena: &Arena,
        state: &InteractionState,
        timeline: &mut AnimationTimeline,
    ) -> (Self, NodeId) {
        let mut order = Vec::new();
        for &root in arena.roots() {
            Self::collect_order(arena, root, &mut order);
        }

        let mut index_of = vec![usize::MAX; order.iter().copied().max().map_or(0, |max| max + 1)];
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
            assign_ordinals(arena.children(id));
        }
        let mut slots: Vec<NodeSlot> = Vec::with_capacity(order.len());
        let mut stable_ids: Vec<usize> = Vec::with_capacity(order.len());
        for &id in &order {
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
            if arena.tag(id) == "input"
                && matches!(arena.input_type(id), Some("checkbox") | Some("radio"))
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
            let parent_stable = arena
                .parent(id)
                .map(|parent_id| stable_ids[index_of[parent_id]]);
            let stable_id = timeline.stable_id(parent_stable, arena.tag(id), ordinal[id]);
            stable_ids.push(stable_id);
            slots.push(NodeSlot {
                parent: None,
                depth: arena.parent(id).map_or(0, |p| slots[index_of[p]].depth + 1),
                sibling_index: 0,
                children: Vec::new(),
                tag: arena.tag(id),
                attrs: arena.attrs(id).to_vec(),
                classes: arena.classes(id).to_vec(),
                stable_id,
                id_attr: arena.id_attr(id).map(str::to_owned),
                id_atom: arena.id_attr(id).map(WeakAtom::from),
                style_pdb: arena.style_attr(id).map(parse_inline_style),
                state: node_state,
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
                .parent(id)
                .map(|parent_id| &slots[index_of[parent_id]] as *const NodeSlot);
            let children_ptrs: Vec<*const NodeSlot> = arena
                .children(id)
                .iter()
                .map(|&child_id| &slots[index_of[child_id]] as *const NodeSlot)
                .collect();
            slots[index].parent = parent_ptr;
            slots[index].children = children_ptrs;
            for (position, child_id) in arena.children(id).iter().enumerate() {
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
        (
            Self {
                slots,
                index_of,
                order,
                uses_inline_scroll_behavior,
            },
            primary_root,
        )
    }

    fn collect_order(arena: &Arena, id: NodeId, order: &mut Vec<NodeId>) {
        let mut stack = vec![id];
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(arena.children(id).iter().rev());
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
                let mut tree = self.order[start..end].to_vec();
                tree.sort_by_key(|&id| depth(id));
                levels.extend(tree);
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
        self.0.state.intersects(pseudo_class.state_flag())
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
        self.0.children.is_empty()
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

    fn state(&self) -> ElementState {
        self.0.state
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

    fn base_size_for_generic(&self, _generic: GenericFontFamily) -> Length {
        Length::new(16.0)
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
) -> HashMap<NodeId, ComputedStyle> {
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
) -> HashMap<NodeId, ComputedStyle> {
    let mut result = HashMap::new();
    if arena.roots().is_empty() {
        return result;
    }

    let (tree, _primary_root) = StyloTree::new(arena, state, timeline);

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

    // Level by level, so an ancestor's style is already committed to its
    // `ElementData` when a descendant resolves and Stylo's style sharing
    // cache sees every node of one depth together. One bloom filter is kept
    // across the whole walk instead of `resolve_style`'s per-call `rebuild`,
    // which re-hashes every ancestor and made deep chains quadratic.
    let mut thread_local = ThreadLocalStyleContext::<StyloNode<'_>>::new();
    // Converting a node's computed values is the same work for every node that
    // shares them, so it is done once per distinct set of values. Every key
    // is an `Arc` still held by its node's `ElementData`, so no address is
    // reused while the map is alive.
    let mut converted: HashMap<*const ComputedValues, ComputedStyle> = HashMap::new();
    for id in tree.level_order() {
        let mut context = StyleContext {
            shared: &shared,
            thread_local: &mut thread_local,
        };
        let target = tree.node(id);
        let styles = resolve_sharing_styles(&mut context, target);

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

        let computed = if has_active_animation && !timeline.should_suppress_animations() {
            to_computed_style(&final_values)
        } else {
            converted
                .entry(&*primary as *const ComputedValues)
                .or_insert_with(|| to_computed_style(&final_values))
                .clone()
        };
        result.insert(id, computed);
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
        timeline.set_current_style(stable_id, primary);
    }
    timeline.sweep();

    result
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
    let parent_style = parent.and_then(|p| p.borrow_data().map(|d| d.styles.primary().clone()));
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
    let mut target = StyleSharingTarget::new(element);
    if layout_parent_is_parent && let Some(shared) = target.share_style_if_possible(context) {
        return shared.into();
    }
    let resolved = StyleResolverForElement::new(
        element,
        context,
        RuleInclusion::All,
        PseudoElementResolution::Force,
    )
    .resolve_style(parent_style.as_deref(), layout_parent_style.as_deref());
    if layout_parent_is_parent {
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

fn to_computed_style(values: &ComputedValues) -> ComputedStyle {
    let background = values.get_background();
    let text = values.get_inherited_text();
    let position = values.get_position();
    let effects = values.get_effects();
    let box_style = values.get_box();
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

    ComputedStyle {
        background_color: background
            .background_color
            .as_absolute()
            .map(to_absolute_rgba)
            .unwrap_or(color),
        color,
        width: to_optional_length(&position.width),
        height: to_optional_length(&position.height),
        max_width: to_max_length(&position.max_width),
        max_height: to_max_length(&position.max_height),
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
        font_size: font.font_size.computed_size.0.px(),
        font_weight: font.font_weight.value(),
        display: to_display(values.get_box().display),
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
        text_decoration_underline: values
            .get_text()
            .text_decoration_line
            .contains(style::values::computed::TextDecorationLine::UNDERLINE),
        cursor_pointer: matches!(
            values.get_inherited_ui().cursor.keyword,
            style::values::computed::ui::CursorKind::Pointer
        ),
        overflow_clips: to_overflow_clips(box_style.overflow_x, box_style.overflow_y),
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
        border_radius: Corners {
            top_left: to_corner_radius(&border.border_top_left_radius),
            top_right: to_corner_radius(&border.border_top_right_radius),
            bottom_right: to_corner_radius(&border.border_bottom_right_radius),
            bottom_left: to_corner_radius(&border.border_bottom_left_radius),
        },
        grid_template_columns: to_grid_template_tracks(&position.grid_template_columns),
        grid_template_rows: to_grid_template_tracks(&position.grid_template_rows),
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
        transform_origin: to_transform_origin(&box_style.transform_origin),
        filter: to_filter(&effects.filter.0),
        backdrop_filter: to_filter(&effects.backdrop_filter.0),
        container_type: to_container_type(box_style.clone_container_type()),
        container_name: to_container_name(&box_style.clone_container_name()),
        object_fit: to_object_fit(position.object_fit),
        object_position: to_object_position(&position.object_position),
        aspect_ratio: to_aspect_ratio(&position.aspect_ratio),
        appearance: to_appearance(values),
        resize: to_resize(values),
        placeholder_color: to_placeholder_color(values),
        scroll_behavior_smooth: to_scroll_behavior_smooth(values),
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
}

thread_local! {
    static FLOR_NAMES: FlorNames = FlorNames {
        scroll_behavior: Atom::from("florui-scroll-behavior"),
        appearance: Atom::from("florui-appearance"),
        resize: Atom::from("florui-resize"),
        placeholder_color: Atom::from("florui-placeholder-color"),
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

/// Reads `--florui-scroll-behavior`: smooth only for the exact keyword.
fn to_scroll_behavior_smooth(values: &ComputedValues) -> bool {
    florui_property(values, |names| &names.scroll_behavior)
        .is_some_and(|css| css.trim().eq_ignore_ascii_case("smooth"))
}

/// Reads `--florui-appearance`.
fn to_appearance(values: &ComputedValues) -> FlorAppearance {
    match florui_property(values, |names| &names.appearance) {
        Some(css) if css.trim() == "none" => FlorAppearance::None,
        _ => FlorAppearance::Auto,
    }
}

/// Reads `--florui-resize`.
fn to_resize(values: &ComputedValues) -> FlorResize {
    match florui_property(values, |names| &names.resize)
        .as_deref()
        .map(str::trim)
    {
        Some("none") => FlorResize::None,
        Some("vertical") => FlorResize::Vertical,
        Some("horizontal") => FlorResize::Horizontal,
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
) -> Vec<FlorFilterFunction> {
    use style::values::generics::effects::GenericFilter;
    value
        .iter()
        .filter_map(|f| match f {
            GenericFilter::Blur(length) => Some(FlorFilterFunction::Blur(length.0.px())),
            GenericFilter::Brightness(factor) => Some(FlorFilterFunction::Brightness(factor.0)),
            GenericFilter::Contrast(factor) => Some(FlorFilterFunction::Contrast(factor.0)),
            GenericFilter::Saturate(factor) => Some(FlorFilterFunction::Saturate(factor.0)),
            // Documented unsupported subset: grayscale, hue-rotate,
            // invert, the filter list's own opacity(), sepia,
            // drop-shadow, and url() — see `FlorFilterFunction`'s own
            // doc.
            _ => None,
        })
        .collect()
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
            // Documented unsupported subset: skew, every 3D function, and
            // the animation-only interpolate/accumulate matrix
            // intermediates — see `FlorTransformFunction`'s own doc.
            _ => None,
        })
        .collect()
}

/// `transform-origin`'s `x`/`y` components; its `z` component is dropped
/// (this crate's `transform` support is 2D-only).
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
    let at_zero = value.resolve(Length::new(0.0)).px();
    let at_one = value.resolve(Length::new(1.0)).px();
    FlorLengthPercentage {
        length: at_zero,
        percentage: at_one - at_zero,
    }
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

/// One border side: `0.0` width for `none`/`hidden` (real CSS's initial
/// style, which makes a border invisible regardless of its width/color —
/// see [`FlorBorderSide`]'s own doc), a solid-only rendering treatment for
/// every other style. `currentcolor` resolves against `inherited_color`
/// (this element's own already-resolved `color`), the same fallback
/// `background-color` already uses.
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
        };
    }
    FlorBorderSide {
        width: width.to_f32_px(),
        color: color
            .as_absolute()
            .map(to_absolute_rgba)
            .unwrap_or(inherited_color),
    }
}

/// A `grid-template-columns`/`-rows` track list down to what this crate
/// resolves — see [`crate::cascade::GridTrackSize`]'s own doc for the
/// bound (`repeat()`, `grid-template-areas`, `subgrid`, and `masonry`
/// aren't read back, only a plain track list).
fn to_grid_template_tracks(
    component: &style::values::computed::GridTemplateComponent,
) -> Vec<crate::cascade::GridTrackSize> {
    use style::values::generics::grid::{GridTemplateComponent, TrackListValue};

    let GridTemplateComponent::TrackList(list) = component else {
        return Vec::new();
    };
    list.values
        .iter()
        .filter_map(|value| match value {
            TrackListValue::TrackSize(size) => Some(to_grid_track_size(size)),
            // `repeat()` isn't expanded into concrete tracks in this slice.
            TrackListValue::TrackRepeat(_) => None,
        })
        .collect()
}

fn to_grid_track_size(size: &style::values::computed::TrackSize) -> crate::cascade::GridTrackSize {
    use style::values::generics::grid::TrackSize;
    match size {
        TrackSize::Breadth(breadth) => to_grid_track_breadth(breadth),
        // Only the max side is read back — real CSS's own "in all cases,
        // treat auto and fit-content() as max-content, except..." leaves
        // the min side mostly informational for this crate's purposes.
        TrackSize::Minmax(_, max) => to_grid_track_breadth(max),
        TrackSize::FitContent(_) => crate::cascade::GridTrackSize::Auto,
    }
}

fn to_grid_track_breadth(
    breadth: &style::values::computed::TrackBreadth,
) -> crate::cascade::GridTrackSize {
    use crate::cascade::GridTrackSize as FlorGridTrackSize;
    use style::values::generics::grid::TrackBreadth;
    match breadth {
        TrackBreadth::Breadth(lp) => lp
            .to_length()
            .map(|length| FlorGridTrackSize::Length(length.px()))
            .unwrap_or(FlorGridTrackSize::Auto),
        TrackBreadth::Fr(fraction) => FlorGridTrackSize::Fr(*fraction),
        TrackBreadth::Auto => FlorGridTrackSize::Auto,
        TrackBreadth::MinContent => FlorGridTrackSize::MinContent,
        TrackBreadth::MaxContent => FlorGridTrackSize::MaxContent,
    }
}

/// A `grid-{row,column}-{start,end}` line down to what this crate resolves
/// — see [`crate::cascade::GridPlacement`]'s own doc for the bound (named
/// lines fall back to `Auto`).
fn to_grid_placement(line: &style::values::computed::GridLine) -> crate::cascade::GridPlacement {
    use crate::cascade::GridPlacement as FlorGridPlacement;
    if line.is_auto() || !line.ident.0.is_empty() {
        return FlorGridPlacement::Auto;
    }
    if line.is_span {
        FlorGridPlacement::Span(line.line_num.max(1) as u16)
    } else {
        FlorGridPlacement::Line(line.line_num as i16)
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
fn to_display(display: style::values::computed::Display) -> FlorDisplay {
    use style::values::specified::box_::{DisplayInside, DisplayOutside};
    match (display.outside(), display.inside()) {
        (DisplayOutside::None, _) => FlorDisplay::None,
        (DisplayOutside::Inline, DisplayInside::FlowRoot) => FlorDisplay::InlineBlock,
        (DisplayOutside::Inline, _) => FlorDisplay::Inline,
        (_, DisplayInside::Flex) => FlorDisplay::Flex,
        (_, DisplayInside::Grid) => FlorDisplay::Grid,
        _ => FlorDisplay::Block,
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
    match value.families.list.first() {
        Some(SingleFontFamily::Generic(GenericFontFamily::Monospace)) => FlorFontFamily::Monospace,
        _ => FlorFontFamily::SansSerif,
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

/// `Fixed`/`Sticky` collapse to [`crate::cascade::Position::Static`]
/// rather than [`crate::cascade::Position::Absolute`] — see
/// [`crate::cascade::ComputedStyle::position`]'s own doc for why.
fn to_position(value: style::values::computed::PositionProperty) -> crate::cascade::Position {
    use style::values::computed::PositionProperty;
    match value {
        PositionProperty::Static => crate::cascade::Position::Static,
        PositionProperty::Relative => crate::cascade::Position::Relative,
        PositionProperty::Absolute => crate::cascade::Position::Absolute,
        PositionProperty::Fixed | PositionProperty::Sticky => crate::cascade::Position::Static,
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
