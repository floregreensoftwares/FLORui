use super::*;
use florui_style::StyleMap;

/// A Taffy tree kept from one layout to the next, so what did not change is
/// not laid out again.
///
/// Taffy remembers, per node, what it measured at each size it was asked; a
/// tree built anew for every layout starts with nothing remembered. Here each
/// arena node keeps its Taffy node, and a layout compares the node's style,
/// its measured content and its children with what the Taffy node already
/// holds, writing (and so marking dirty, with its ancestors) only the ones
/// that differ. The result is exactly what a tree built anew would give.
///
/// Arena ids follow the order the tree was built in, so a node that moved
/// because an earlier one appeared or went away simply differs from what its
/// id held and is written again; correctness never depends on ids being
/// stable, only the saving does.
pub struct LayoutCache {
    tree: FlorTree,
    /// The Taffy node of each arena node laid out last time, by arena id.
    slots: Vec<Option<Slot>>,
    /// The synthetic block wrapping the roots of the document and of the
    /// overlays, which Taffy lays out as two separate trees, and the one
    /// holding the boxes fixed to the viewport.
    groups: [Option<Group>; 3],
    /// The fonts the cached measures were made with.
    font_generation: u64,
    /// Which layout last wrote a slot; a slot that is older is gone.
    epoch: u64,
    /// What the cascade last gave, and what it was given.
    styles: Option<StyleMemo>,
    /// The tree the cascade walks, kept for as long as the elements are the same.
    session: florui_style::StyleSession,
    /// The Taffy leaves of the anonymous blocks of the last layout, by their
    /// id. They have no arena node to keep a slot for, so each layout makes
    /// them anew and removes the last ones.
    anonymous: HashMap<NodeId, taffy::NodeId>,
}

/// One set of inputs to the cascade and, once they have come twice in a row,
/// the styles it gave. A tree that differs only in the text inside its
/// elements has the same styles, which is most updates. The styles are kept
/// only for inputs seen twice, so a first frame, or a tree that changes on
/// every update, does not pay for a copy it will not use.
struct StyleMemo {
    shape: u64,
    rules: (usize, usize),
    state: InteractionState,
    viewport: Viewport,
    settings: (bool, bool, bool),
    styles: Option<StyleMap>,
}

struct Slot {
    id: taffy::NodeId,
    children: Vec<taffy::NodeId>,
    /// The layout that last wrote this slot; one that is older is gone.
    epoch: u64,
    /// The layout that created it: a node made in this layout has no parent yet.
    created: u64,
}

struct Group {
    id: taffy::NodeId,
    children: Vec<taffy::NodeId>,
}

enum Step {
    Visit(NodeId),
    Build(NodeId),
    BuildMixed(NodeId, Vec<Part>),
}

impl Default for LayoutCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutCache {
    pub fn new() -> Self {
        let tree = FlorTree::new();
        // Text is measured at its exact width and paint reshapes it at the
        // laid out width: see `compute_layout_with_content_extents`.
        LayoutCache {
            tree,
            slots: Vec::new(),
            groups: [None, None, None],
            font_generation: 0,
            epoch: 0,
            styles: None,
            session: florui_style::StyleSession::new(),
            anonymous: HashMap::new(),
        }
    }

    /// [`florui_style::compute`] on the tree kept from the last call, for a
    /// caller that runs the cascade often on the same elements (a state change
    /// that does not go through [`Self::styles`]).
    pub fn cascade(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> StyleMap {
        self.session
            .compute(arena, rules, state, viewport, timeline)
    }

    /// [`Self::cascade`], and which nodes may have a style other than the one the
    /// call before gave: `None` when that is not known, otherwise the nodes that
    /// were styled again (a change of state restyles only what it can reach).
    pub fn cascade_changed(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> (StyleMap, Option<Vec<NodeId>>) {
        self.session
            .compute_changed(arena, rules, state, viewport, timeline)
    }

    /// Forgets the styles the cascade gave, for a caller that changed
    /// something the cascade reads and this cannot see.
    pub fn forget_styles(&mut self) {
        self.styles = None;
    }

    /// The styles of `arena`: the ones the last call gave if the tree
    /// (leaving out its text), the rules, the interaction state, the viewport
    /// and the animation settings are the same and nothing is animating, the
    /// cascade's otherwise. The second value is whether the cascade ran.
    pub(crate) fn styles(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> (StyleMap, bool) {
        let shape = arena.style_shape();
        let rules_key = (rules.as_ptr() as usize, rules.len());
        let settings = timeline.settings();
        let seen_before = self.styles.as_ref().is_some_and(|memo| {
            memo.shape == shape
                && memo.rules == rules_key
                && memo.viewport == viewport
                && memo.settings == settings
                && memo.state == *state
        });
        if seen_before
            && timeline.is_still()
            && let Some(styles) = self.styles.as_ref().and_then(|memo| memo.styles.as_ref())
        {
            return (styles.clone(), false);
        }
        let styles = self
            .session
            .compute(arena, rules, state, viewport, timeline);
        let keep = seen_before && timeline.is_still();
        self.styles = timeline.is_still().then(|| StyleMemo {
            shape,
            rules: rules_key,
            state: state.clone(),
            viewport,
            settings,
            styles: keep.then(|| styles.clone()),
        });
        (styles, true)
    }

    /// Same as [`compute_layout_with_content_extents`], remembering the tree
    /// for the next call.
    pub fn compute(
        &mut self,
        font: &mut florui_text::Font,
        arena: &Arena,
        styles: &StyleMap,
        available: Size<AvailableSpace>,
    ) -> Result<LayoutsAndContentExtents, LayoutError> {
        // On one grown stack for the whole layout: see
        // [`compute_layout_with_content_extents`].
        stacker::maybe_grow(RECURSION_RED_ZONE, RECURSION_STACK_SIZE, || {
            self.compute_in_place(font, arena, styles, available)
        })
    }

    fn compute_in_place(
        &mut self,
        font: &mut florui_text::Font,
        arena: &Arena,
        styles: &StyleMap,
        available: Size<AvailableSpace>,
    ) -> Result<LayoutsAndContentExtents, LayoutError> {
        if font.generation() != self.font_generation {
            *self = LayoutCache::new();
            self.font_generation = font.generation();
        }
        self.epoch += 1;
        for (_, gone) in self.anonymous.drain() {
            self.tree.remove(gone).map_err(LayoutError)?;
        }
        if self.slots.is_empty() {
            // A first layout: room for every node at once, as a tree built anew has.
            let tree = FlorTree::with_capacity(styles.len() + 2);
            self.tree = tree;
            self.slots.reserve(styles.len());
        }

        let mut inline_leaves: Vec<(NodeId, InlineContext)> = Vec::new();
        let flow_root = taffy::Style {
            display: Display::Block,
            ..Default::default()
        };
        self.sync_group(
            0,
            font,
            arena,
            styles,
            arena.document_roots(),
            &mut inline_leaves,
            &flow_root,
        )
        .map_err(LayoutError)?;
        self.sync_group(
            1,
            font,
            arena,
            styles,
            arena.overlay_roots(),
            &mut inline_leaves,
            &flow_root,
        )
        .map_err(LayoutError)?;
        let fixed = viewport_fixed_nodes(arena, styles);
        self.sync_group(
            2,
            font,
            arena,
            styles,
            &fixed,
            &mut inline_leaves,
            &viewport_root_style(available),
        )
        .map_err(LayoutError)?;
        self.drop_unused().map_err(LayoutError)?;

        let node_count = self.slots.len();
        for group in self.groups.iter().flatten() {
            run_layout(font, &mut self.tree, group.id, available, node_count)?;
        }
        let roots: Vec<taffy::NodeId> = self.groups.iter().flatten().map(|g| g.id).collect();
        let kept = &self.slots;
        let id_of = |node: NodeId| {
            kept.get(node)
                .and_then(|slot| slot.as_ref())
                .map(|slot| slot.id)
        };
        deferred::resolve(
            font,
            arena,
            styles,
            &mut self.tree,
            &id_of,
            &roots,
            available,
            node_count,
        )?;
        subgrid::resolve(
            font,
            arena,
            styles,
            &mut self.tree,
            &id_of,
            &roots,
            available,
            node_count,
        )?;

        let slots = &self.slots;
        let anonymous = &self.anonymous;
        collect_layouts(
            font,
            arena,
            styles,
            &self.tree,
            slots
                .iter()
                .enumerate()
                .filter_map(|(node, slot)| slot.as_ref().map(|slot| (node, slot.id)))
                .chain(anonymous.iter().map(|(&key, &id)| (key, id))),
            &inline_leaves,
            &fixed,
            |node| {
                if is_anonymous(node) {
                    anonymous[&node]
                } else {
                    slots[node]
                        .as_ref()
                        .map_or_else(|| unreachable!(), |s| s.id)
                }
            },
        )
    }

    /// Brings the Taffy nodes of `roots` and everything under them to what
    /// the arena says, and the group wrapping them to hold exactly them (but
    /// for the ones fixed to the viewport, which the group of those holds).
    #[allow(clippy::too_many_arguments)]
    fn sync_group(
        &mut self,
        group: usize,
        font: &mut florui_text::Font,
        arena: &Arena,
        styles: &StyleMap,
        roots: &[NodeId],
        inline_leaves: &mut Vec<(NodeId, InlineContext)>,
        style: &taffy::Style,
    ) -> Result<(), taffy::TaffyError> {
        if roots.is_empty() {
            if let Some(unused) = self.groups[group].take() {
                self.tree.remove(unused.id)?;
            }
            return Ok(());
        }
        for &root in roots {
            self.sync_subtree(font, arena, styles, root, inline_leaves)?;
        }
        let children: Vec<taffy::NodeId> = roots
            .iter()
            .filter(|&&root| group == 2 || !is_viewport_fixed(arena, styles, root))
            .map(|&root| self.id_of(root))
            .collect();
        match &mut self.groups[group] {
            Some(existing) => {
                if existing.children != children {
                    self.tree.set_children(existing.id, &children)?;
                    existing.children = children;
                }
                if self.tree.style(existing.id)? != style {
                    self.tree.set_style(existing.id, style.clone())?;
                }
            }
            slot @ None => {
                let id = self.tree.new_leaf(style.clone())?;
                self.tree.set_children(id, &children)?;
                *slot = Some(Group { id, children });
            }
        }
        Ok(())
    }

    fn id_of(&self, node: NodeId) -> taffy::NodeId {
        self.slots[node].as_ref().map_or_else(
            || unreachable!("a child is synced before its parent"),
            |s| s.id,
        )
    }

    /// The walk of `build_node`, writing into the kept tree instead of
    /// creating a new one.
    fn sync_subtree(
        &mut self,
        font: &mut florui_text::Font,
        arena: &Arena,
        styles: &StyleMap,
        root: NodeId,
        inline_leaves: &mut Vec<(NodeId, InlineContext)>,
    ) -> Result<(), taffy::TaffyError> {
        let mut stack = vec![Step::Visit(root)];
        while let Some(step) = stack.pop() {
            match step {
                Step::Visit(node) => {
                    let arena_children = flow_children(arena, styles, node);
                    // An inline `<svg>` is a leaf: what is in it is drawn with it.
                    let leaf = arena.children(node).is_empty() || arena.tag(node) == "svg";
                    let kind = if leaf {
                        LayoutKind::Blocks
                    } else {
                        layout_kind(arena, styles, node)
                    };
                    if leaf {
                        let context = leaf_context(font, arena, styles, node);
                        let style = if arena.tag(node) == "svg" {
                            crate::svg_taffy_style(arena, styles, node)
                        } else {
                            to_taffy_style(styles.get(&node))
                        };
                        self.place(node, style, context, &[], false)?;
                    } else if kind == LayoutKind::Inline {
                        let context = build_inline_content_items(font, arena, styles, node, None);
                        self.place(
                            node,
                            to_taffy_style(styles.get(&node)),
                            Some(LeafContext::Inline(context.clone())),
                            &[],
                            false,
                        )?;
                        inline_leaves.push((node, context));
                    } else if kind == LayoutKind::Mixed {
                        let parts = mixed_parts_of(arena, styles, node);
                        let blocks: Vec<NodeId> = parts
                            .iter()
                            .filter_map(|part| match part {
                                Part::Block(child) => Some(*child),
                                Part::Run(_) | Part::Spot(_) => None,
                            })
                            .collect();
                        stack.push(Step::BuildMixed(node, parts));
                        for &child in blocks.iter().rev() {
                            stack.push(Step::Visit(child));
                        }
                    } else {
                        stack.push(Step::Build(node));
                        for &child in arena_children.iter().rev() {
                            if !is_viewport_fixed(arena, styles, child) {
                                stack.push(Step::Visit(child));
                            }
                        }
                    }
                }
                Step::BuildMixed(node, parts) => {
                    let reorders = reorders_children(styles, node)
                        && parts.iter().any(|part| {
                            matches!(part, Part::Block(child) if order_of(styles, *child) != 0)
                        });
                    let mut children: Vec<taffy::NodeId> = Vec::with_capacity(parts.len());
                    let mut orders: Vec<i32> = Vec::new();
                    let mut run_number = 0;
                    for part in &parts {
                        match part {
                            Part::Block(child) => {
                                children.push(self.id_of(*child));
                                if reorders {
                                    orders.push(order_of(styles, *child));
                                }
                            }
                            Part::Spot(fixed) => {
                                let spot = self.tree.new_leaf(static_spot_style())?;
                                self.anonymous.insert(static_spot_id(*fixed), spot);
                                children.push(spot);
                                if reorders {
                                    orders.push(0);
                                }
                            }
                            Part::Run(range) => {
                                let context = build_run_content_items(
                                    font,
                                    arena,
                                    styles,
                                    node,
                                    range.clone(),
                                    None,
                                );
                                let leaf = self.tree.new_leaf_with_context(
                                    taffy::Style {
                                        display: Display::Block,
                                        ..Default::default()
                                    },
                                    LeafContext::Inline(context.clone()),
                                )?;
                                let key = anonymous_id(node, run_number);
                                run_number += 1;
                                self.anonymous.insert(key, leaf);
                                inline_leaves.push((key, context));
                                children.push(leaf);
                                if reorders {
                                    orders.push(0);
                                }
                            }
                        }
                    }
                    sort_by_order(&mut children, &orders);
                    self.place(
                        node,
                        to_taffy_style(styles.get(&node)),
                        None,
                        &children,
                        false,
                    )?;
                }
                Step::Build(node) => {
                    let reorders = reorders_children(styles, node)
                        && flow_children(arena, styles, node)
                            .iter()
                            .any(|&child| order_of(styles, child) != 0);
                    let mut children: Vec<taffy::NodeId> =
                        Vec::with_capacity(flow_children(arena, styles, node).len());
                    let mut orders: Vec<i32> = Vec::new();
                    let mut children_are_new = true;
                    for &child in flow_children(arena, styles, node).iter() {
                        if is_viewport_fixed(arena, styles, child) {
                            let spot = self.tree.new_leaf(static_spot_style())?;
                            self.anonymous.insert(static_spot_id(child), spot);
                            children.push(spot);
                            if reorders {
                                orders.push(0);
                            }
                            continue;
                        }
                        children.push(self.id_of(child));
                        if reorders {
                            orders.push(order_of(styles, child));
                        }
                        children_are_new &= self.slots[child]
                            .as_ref()
                            .is_some_and(|slot| slot.created == self.epoch);
                    }
                    sort_by_order(&mut children, &orders);
                    self.place(
                        node,
                        to_taffy_style(styles.get(&node)),
                        None,
                        &children,
                        children_are_new,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Makes the Taffy node of `node` hold this style, context and children,
    /// creating it if the node is new and writing only what differs if not.
    fn place(
        &mut self,
        node: NodeId,
        style: taffy::Style,
        context: Option<LeafContext>,
        children: &[taffy::NodeId],
        children_are_new: bool,
    ) -> Result<(), taffy::TaffyError> {
        if self.slots.len() <= node {
            self.slots.resize_with(node + 1, || None);
        }
        let epoch = self.epoch;
        let Some(slot) = &mut self.slots[node] else {
            // `new_with_children` links a child to its new parent without
            // leaving the one it had, whose own later `set_children` would then
            // cut the new link. That cannot happen to a child made in this
            // layout, which has no parent yet; any other child goes through
            // `set_children`, which leaves its old parent first.
            let id = match context {
                Some(context) => self.tree.new_leaf_with_context(style, context)?,
                None if children_are_new && !children.is_empty() => {
                    self.tree.new_with_children(style, children)?
                }
                None => {
                    let id = self.tree.new_leaf(style)?;
                    if !children.is_empty() {
                        self.tree.set_children(id, children)?;
                    }
                    id
                }
            };
            self.slots[node] = Some(Slot {
                id,
                children: children.to_vec(),
                epoch,
                created: epoch,
            });
            return Ok(());
        };
        slot.epoch = epoch;
        let id = slot.id;
        if *self.tree.style(id)? != style {
            self.tree.set_style(id, style)?;
        }
        if self.tree.get_node_context(id) != context.as_ref() {
            self.tree.set_node_context(id, context)?;
        }
        if slot.children != children {
            self.tree.set_children(id, children)?;
            slot.children = children.to_vec();
        }
        Ok(())
    }

    /// Removes the Taffy nodes of arena nodes that are not in the tree any
    /// more.
    fn drop_unused(&mut self) -> Result<(), taffy::TaffyError> {
        let epoch = self.epoch;
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|s| s.epoch != epoch)
                && let Some(gone) = slot.take()
            {
                self.tree.remove(gone.id)?;
            }
        }
        while matches!(self.slots.last(), Some(None)) {
            self.slots.pop();
        }
        Ok(())
    }
}

/// What the measure closure needs to size a leaf with no element children, as
/// `build_node` creates it; `None` for a leaf with nothing to measure.
fn leaf_context(
    font: &mut florui_text::Font,
    arena: &Arena,
    styles: &StyleMap,
    node: NodeId,
) -> Option<LeafContext> {
    let style = styles.get(&node);
    if arena.tag(node) == "svg" {
        return Some(crate::svg_context(arena, styles, node));
    }
    if matches!(arena.tag(node), "img" | "icon") {
        return Some(LeafContext::Image(ImageContext {
            intrinsic_size: None,
            aspect_ratio: style.map_or_else(Default::default, |s| s.aspect_ratio),
        }));
    }
    if arena.tag(node) == "textarea" {
        return Some(LeafContext::Fixed(textarea_intrinsic_size(
            font, arena, node, style,
        )));
    }
    let text = leaf_text(arena, node);
    if text.is_empty() {
        return None;
    }
    Some(LeafContext::Text(TextContext {
        text: text.to_string(),
        style: style.map_or_else(default_text_style, text_style_of),
    }))
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_style::InteractionState;

    use super::*;

    const CSS: &str = ".root { display: flex; flex-wrap: wrap; width: 300px; } \
         .col { display: flex; flex-direction: column; padding: 4px; } \
         .c0 { width: 60px; height: 40px; } \
         .c1 { flex-grow: 1; padding: 3px; border: 2px solid red; } \
         .c2 { width: 50%; margin-left: 7.5px; } \
         .c3 { position: absolute; left: 30px; top: 20px; width: 50px; height: 50px; } \
         .c4 { display: inline-block; width: 24px; height: 12px; } \
         .ib { display: inline-block; } \
         .big { font-size: 22px; font-weight: 700; } \
         .over { position: absolute; width: 120px; padding: 5px; }";

    /// A tree of nested boxes, text leaves and a mixed inline paragraph; the
    /// same `seed` and `edit` give the same tree.
    fn tree(seed: u32, edit: u32) -> Element {
        fn next(state: &mut u32) -> u32 {
            *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *state >> 8
        }
        fn node(state: &mut u32, depth: u32, edit: u32, counter: &mut u32) -> Element {
            *counter += 1;
            let class = format!("c{}", next(state) % 5);
            if depth >= 4 || next(state).is_multiple_of(3) {
                let words = 1 + next(state) % 6;
                let mut text = (0..words)
                    .map(|w| format!("word{w}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                if *counter == edit {
                    text.push_str(" edited");
                }
                let class = if (*counter).is_multiple_of(4) {
                    format!("{class} big")
                } else {
                    class
                };
                return Element::node(
                    "div",
                    vec![("class".into(), class)],
                    vec![Element::text(&text)],
                );
            }
            if next(state).is_multiple_of(6) {
                return Element::node(
                    "p",
                    vec![("class".into(), class)],
                    vec![
                        Element::text("Hello "),
                        Element::node("span", vec![], vec![Element::text("world")]),
                        Element::node("span", vec![("class".into(), "c4".into())], vec![]),
                        Element::text("!"),
                    ],
                );
            }
            if next(state).is_multiple_of(5) {
                // Inline-level runs around a block, and an inline-block with content.
                return Element::node(
                    "div",
                    vec![("class".into(), class)],
                    vec![
                        Element::text("before "),
                        Element::node(
                            "span",
                            vec![("class".into(), "ib".into())],
                            vec![
                                Element::node("span", vec![], vec![Element::text("one")]),
                                Element::text(" two"),
                            ],
                        ),
                        Element::node("div", vec![("class".into(), "c0".into())], vec![]),
                        Element::text("after"),
                    ],
                );
            }
            let children = (0..1 + next(state) % 4)
                .map(|_| node(state, depth + 1, edit, counter))
                .collect();
            Element::node(
                "div",
                vec![("class".into(), format!("{class} col"))],
                children,
            )
        }
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(7);
        let mut counter = 0;
        let children = (0..3)
            .map(|_| node(&mut state, 1, edit, &mut counter))
            .collect();
        Element::node("div", vec![("class".into(), "root".into())], children)
    }

    fn styled(arena: &Arena) -> StyleMap {
        let rules = florui_style::parse_stylesheet(CSS).unwrap();
        florui_style::compute(
            arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        )
    }

    /// Lays `arena` out in `cache` and from scratch, and fails if they differ.
    fn assert_same(
        cache: &mut LayoutCache,
        font: &mut florui_text::Font,
        arena: &Arena,
        available: Size<AvailableSpace>,
        what: &str,
    ) {
        let styles = styled(arena);
        let kept = cache.compute(font, arena, &styles, available).unwrap();
        let fresh = compute_layout_with_content_extents(font, arena, &styles, available).unwrap();
        assert_eq!(kept.0.len(), fresh.0.len(), "{what}: node count");
        for (node, layout) in &fresh.0 {
            assert_eq!(kept.0.get(node), Some(layout), "{what}: box of node {node}");
        }
        for (node, extent) in &fresh.1 {
            assert_eq!(
                kept.1.get(node),
                Some(extent),
                "{what}: extent of node {node}"
            );
        }
    }

    #[test]
    fn a_kept_tree_lays_out_as_one_built_anew_through_any_sequence_of_changes() {
        let mut font = florui_text::Font::load_embedded();
        let mut cache = LayoutCache::new();
        let wide = Size::MAX_CONTENT;
        let narrow = Size {
            width: AvailableSpace::Definite(220.0),
            height: AvailableSpace::MaxContent,
        };
        for seed in 0..40u32 {
            // The same tree twice (nothing changed), then one text edited at a
            // few places, then a different tree altogether (ids all move).
            for (edit, what) in [(0, "first"), (0, "unchanged"), (3, "edit 3"), (9, "edit 9")] {
                let arena = Arena::build(&tree(seed, edit));
                assert_same(
                    &mut cache,
                    &mut font,
                    &arena,
                    wide,
                    &format!("seed {seed} {what}"),
                );
            }
            let arena = Arena::build(&tree(seed, 0));
            assert_same(
                &mut cache,
                &mut font,
                &arena,
                narrow,
                &format!("seed {seed} narrow"),
            );
            assert_same(
                &mut cache,
                &mut font,
                &arena,
                wide,
                &format!("seed {seed} wide again"),
            );
        }
    }

    /// [`assert_same`] with a stylesheet of its own.
    fn assert_same_with(
        css: &str,
        cache: &mut LayoutCache,
        font: &mut florui_text::Font,
        arena: &Arena,
        available: Size<AvailableSpace>,
        what: &str,
    ) {
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let kept = cache.compute(font, arena, &styles, available).unwrap();
        let fresh = compute_layout_with_content_extents(font, arena, &styles, available).unwrap();
        assert_eq!(kept.0.len(), fresh.0.len(), "{what}: node count");
        for (node, layout) in &fresh.0 {
            assert_eq!(kept.0.get(node), Some(layout), "{what}: box of node {node}");
        }
    }

    #[test]
    fn order_subgrid_and_fit_content_lay_out_in_a_kept_tree_as_in_one_built_anew() {
        let css = ".flex { display: flex; width: 200px; gap: 4px; } \
             .flex > div { width: 30px; height: 10px; } \
             .a { order: 3; } .b { order: 1; } .c { order: 2; } \
             .p { display: grid; grid-template-columns: 80px 80px; grid-template-rows: auto auto; \
                  gap: 6px; width: 170px; } \
             .card { grid-row: span 2; display: grid; grid-template-rows: subgrid; padding: 3px; } \
             .t { height: 20px; } .u { height: 40px; } \
             .g { display: grid; width: fit-content; grid-template-columns: fit-content(30px) 1fr; } \
             .big { width: 70px; height: 10px; }";
        let flex = |extra: &str| {
            Element::node(
                "div",
                vec![("class".into(), "flex".into())],
                ["a", "b", "c", extra]
                    .iter()
                    .map(|class| {
                        Element::node("div", vec![("class".into(), (*class).into())], vec![])
                    })
                    .collect(),
            )
        };
        let card = |first: &str| {
            Element::node(
                "div",
                vec![("class".into(), "card".into())],
                vec![
                    Element::node("div", vec![("class".into(), first.into())], vec![]),
                    Element::node("div", vec![("class".into(), "u".into())], vec![]),
                ],
            )
        };
        let page = |extra: &str, tall: &str| {
            Element::node(
                "div",
                vec![],
                vec![
                    flex(extra),
                    Element::node(
                        "div",
                        vec![("class".into(), "p".into())],
                        vec![card(tall), card("t")],
                    ),
                    Element::node(
                        "div",
                        vec![("class".into(), "g".into())],
                        vec![
                            Element::node(
                                "div",
                                vec![],
                                vec![Element::node(
                                    "div",
                                    vec![("class".into(), "big".into())],
                                    vec![],
                                )],
                            ),
                            Element::node(
                                "div",
                                vec![],
                                vec![Element::node(
                                    "div",
                                    vec![("class".into(), "big".into())],
                                    vec![],
                                )],
                            ),
                        ],
                    ),
                ],
            )
        };
        let mut font = florui_text::Font::load_embedded();
        let mut cache = LayoutCache::new();
        let available = Size::MAX_CONTENT;
        // Several trees in turn, so the kept tree is changed under the subgrids.
        for (step, (extra, tall)) in [("a", "t"), ("b", "u"), ("c", "t"), ("a", "u")]
            .iter()
            .enumerate()
        {
            let arena = Arena::build(&page(extra, tall));
            assert_same_with(
                css,
                &mut cache,
                &mut font,
                &arena,
                available,
                &format!("step {step}"),
            );
        }
        // And the order it gave is the one asked for.
        let arena = Arena::build(&page("a", "t"));
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let layouts = cache
            .compute(&mut font, &arena, &styles, available)
            .unwrap()
            .0;
        let items = arena.children(arena.children(arena.roots()[0])[0]).to_vec();
        // `b` (order 1) first, then `c`, then the two `a`s (order 3) in source order.
        assert!(layouts[&items[1]].x < layouts[&items[2]].x);
        assert!(layouts[&items[2]].x < layouts[&items[0]].x);
    }

    #[test]
    fn overlays_come_and_go_in_a_kept_tree() {
        let mut font = florui_text::Font::load_embedded();
        let mut cache = LayoutCache::new();
        let document = tree(5, 0);
        let overlay = |n: u32| {
            Element::node(
                "div",
                vec![("class".into(), "over".into())],
                (0..n)
                    .map(|i| Element::text(format!("overlay {i}")))
                    .collect(),
            )
        };
        let available = Size::MAX_CONTENT;
        let none = Arena::build(&document);
        assert_same(&mut cache, &mut font, &none, available, "no overlay");
        for n in [1, 2, 0, 3] {
            let arena = Arena::build_with_overlays(&document, &overlay(n));
            assert_same(
                &mut cache,
                &mut font,
                &arena,
                available,
                &format!("overlay of {n}"),
            );
            assert_same(&mut cache, &mut font, &none, available, "overlay gone");
        }
    }

    const STYLE_CSS: &str = ".a { color: #ff0000; width: 10px; } .b { color: #00ff00; }          .a:hover { color: #0000ff; } .a + .b { width: 20px; }          @media (min-width: 500px) { .b { width: 90px; } }";
    const ANIMATED_CSS: &str = "@keyframes grow { from { width: 10px; } to { width: 90px; } }          .a { animation: grow 2s linear infinite; }";

    fn simple(a_class: &str, text: &str) -> Arena {
        Arena::build(&simple_tree(a_class, text))
    }

    fn simple_tree(a_class: &str, text: &str) -> Element {
        Element::node(
            "div",
            vec![],
            vec![
                Element::node(
                    "p",
                    vec![("class".into(), a_class.into())],
                    vec![Element::text(text)],
                ),
                Element::node(
                    "p",
                    vec![("class".into(), "b".into())],
                    vec![Element::text("fixed")],
                ),
            ],
        )
    }

    /// Runs `cache.styles` and the cascade on the same inputs and fails if they
    /// differ; returns whether the cascade ran in the cache.
    fn styles_match(
        cache: &mut LayoutCache,
        timelines: &mut (
            florui_style::AnimationTimeline,
            florui_style::AnimationTimeline,
        ),
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: florui_style::Viewport,
        what: &str,
    ) -> bool {
        let (kept, ran) = cache.styles(arena, rules, state, viewport, &mut timelines.0);
        let fresh = florui_style::compute(arena, rules, state, viewport, &mut timelines.1);
        assert_eq!(kept, fresh, "{what}");
        ran
    }

    #[test]
    fn the_styles_of_a_tree_that_did_not_change_are_kept_and_nothing_else_is() {
        let rules = florui_style::parse_stylesheet(STYLE_CSS).unwrap();
        let other_rules =
            florui_style::parse_stylesheet(&STYLE_CSS.replace("#ff0000", "#123456")).unwrap();
        let mut cache = LayoutCache::new();
        let mut timelines = Default::default();
        let state = InteractionState::new();
        let viewport = florui_style::Viewport::default();
        let mut run = |cache: &mut LayoutCache,
                       arena: &Arena,
                       rules: &[Rule],
                       state: &InteractionState,
                       viewport,
                       what: &str| {
            styles_match(cache, &mut timelines, arena, rules, state, viewport, what)
        };

        let mut arena = simple("a", "text");
        // The first two calls run the cascade (the second one keeps the styles);
        // from the third on they are kept.
        assert!(run(&mut cache, &arena, &rules, &state, viewport, "first"));
        assert!(run(&mut cache, &arena, &rules, &state, viewport, "second"));
        assert!(!run(&mut cache, &arena, &rules, &state, viewport, "third"));
        assert!(!run(&mut cache, &arena, &rules, &state, viewport, "fourth"));

        // Only the text changed: the styles are the same.
        arena.rebuild(&simple_tree("a", "another text, longer"));
        assert!(!run(
            &mut cache,
            &arena,
            &rules,
            &state,
            viewport,
            "text only"
        ));

        // A class changed: the cascade runs.
        arena.rebuild(&simple_tree("b", "text"));
        assert!(run(
            &mut cache,
            &arena,
            &rules,
            &state,
            viewport,
            "class changed"
        ));
        assert!(run(
            &mut cache,
            &arena,
            &rules,
            &state,
            viewport,
            "class changed, again"
        ));
        assert!(!run(
            &mut cache,
            &arena,
            &rules,
            &state,
            viewport,
            "class kept"
        ));

        // Hover on a node, a different viewport (a media query), other rules.
        let hovered = InteractionState::new().with_hovered(0);
        assert!(run(&mut cache, &arena, &rules, &hovered, viewport, "hover"));
        let wide = florui_style::Viewport {
            width: 800.0,
            height: 600.0,
        };
        assert!(run(&mut cache, &arena, &rules, &state, wide, "viewport"));
        assert!(run(
            &mut cache,
            &arena,
            &rules,
            &state,
            wide,
            "viewport, again"
        ));
        assert!(!run(
            &mut cache,
            &arena,
            &rules,
            &state,
            wide,
            "viewport kept"
        ));
        assert!(run(
            &mut cache,
            &arena,
            &other_rules,
            &state,
            wide,
            "other rules"
        ));
    }

    #[test]
    fn styles_that_change_with_time_are_never_kept() {
        let rules = florui_style::parse_stylesheet(ANIMATED_CSS).unwrap();
        let mut cache = LayoutCache::new();
        let mut timelines: (
            florui_style::AnimationTimeline,
            florui_style::AnimationTimeline,
        ) = Default::default();
        let arena = simple("a", "text");
        let state = InteractionState::new();
        let viewport = florui_style::Viewport::default();
        for step in 0..8 {
            let now = step as f64 * 0.37;
            timelines.0.advance_to(now);
            timelines.1.advance_to(now);
            let ran = styles_match(
                &mut cache,
                &mut timelines,
                &arena,
                &rules,
                &state,
                viewport,
                &format!("animated, step {step}"),
            );
            assert!(
                ran,
                "an animation is running, so the cascade runs (step {step})"
            );
        }
    }

    #[test]
    fn forgetting_the_styles_runs_the_cascade_again() {
        let rules = florui_style::parse_stylesheet(STYLE_CSS).unwrap();
        let mut cache = LayoutCache::new();
        let mut timelines = Default::default();
        let arena = simple("a", "text");
        let state = InteractionState::new();
        let viewport = florui_style::Viewport::default();
        for _ in 0..3 {
            styles_match(
                &mut cache,
                &mut timelines,
                &arena,
                &rules,
                &state,
                viewport,
                "warm",
            );
        }
        cache.forget_styles();
        assert!(styles_match(
            &mut cache,
            &mut timelines,
            &arena,
            &rules,
            &state,
            viewport,
            "forgotten"
        ));
    }

    #[test]
    fn a_registered_font_empties_the_kept_tree() {
        let mut font = florui_text::Font::load_embedded();
        let mut cache = LayoutCache::new();
        let arena = Arena::build(&tree(11, 0));
        assert_same(&mut cache, &mut font, &arena, Size::MAX_CONTENT, "before");
        let generation = font.generation();
        // Registering the same bytes again still bumps the generation.
        let _ = font.register(florui_text::EMBEDDED_SANS_SERIF_FONT);
        if font.generation() != generation {
            assert_same(&mut cache, &mut font, &arena, Size::MAX_CONTENT, "after");
        }
    }
}
