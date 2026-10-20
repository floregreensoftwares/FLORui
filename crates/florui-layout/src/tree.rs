//! The node store Taffy's algorithms lay out.
//!
//! `TaffyTree` hands a leaf's measure function nothing but the sizes it is
//! asked about, but a leaf with text has to know the floats of the formatting
//! context it sits in, and a table has to lay its cells out itself. So the
//! nodes live here instead, and the Taffy algorithms are called through
//! [`Run`], which owns the font and decides, node by node, which algorithm
//! lays the node out. Nodes are never rounded: text is shaped at the exact
//! widths layout found (see `compute_layout_with_content_extents`).

use std::collections::HashMap;

use taffy::util::ResolveOrZero;
use taffy::{
    AvailableSpace, BlockContext, Cache, CacheTree, ClearState, DetailedGridInfo, Display, Layout,
    LayoutBlockContainer, LayoutFlexboxContainer, LayoutGridContainer, LayoutInput, LayoutOutput,
    LayoutPartialTree, NodeId, RunMode, Size, Style, TaffyError, TraversePartialTree,
    compute_block_layout, compute_cached_layout, compute_flexbox_layout, compute_grid_layout,
    compute_hidden_layout, compute_leaf_layout, compute_root_layout,
};

use super::{LeafContext, LeafScratch, measure_leaf};

pub(crate) type TreeResult<T> = Result<T, TaffyError>;

struct Node {
    style: Style,
    cache: Cache,
    layout: Layout,
    children: Vec<NodeId>,
    parent: Option<NodeId>,
    context: Option<LeafContext>,
    /// What the last grid layout of the node made of its tracks and items.
    grid_info: Option<Box<DetailedGridInfo>>,
}

impl Node {
    fn new(style: Style, context: Option<LeafContext>) -> Self {
        Node {
            style,
            cache: Cache::new(),
            layout: Layout::with_order(0),
            children: Vec::new(),
            parent: None,
            context,
            grid_info: None,
        }
    }
}

/// A slot of the store. The generation tells an id that outlived its node from
/// the node that took the slot after it.
struct Entry {
    generation: u32,
    node: Option<Node>,
}

/// The nodes of one tree, by [`NodeId`]; see the module documentation.
pub(crate) struct FlorTree {
    entries: Vec<Entry>,
    free: Vec<u32>,
}

fn make_id(index: usize, generation: u32) -> NodeId {
    NodeId::new((u64::from(generation) << 32) | index as u64)
}

fn split_id(id: NodeId) -> (usize, u32) {
    let raw: u64 = id.into();
    ((raw & 0xffff_ffff) as usize, (raw >> 32) as u32)
}

impl FlorTree {
    pub(crate) fn new() -> Self {
        Self::with_capacity(16)
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        FlorTree {
            entries: Vec::with_capacity(capacity),
            free: Vec::new(),
        }
    }

    fn insert(&mut self, node: Node) -> NodeId {
        if let Some(index) = self.free.pop() {
            let entry = &mut self.entries[index as usize];
            entry.node = Some(node);
            return make_id(index as usize, entry.generation);
        }
        self.entries.push(Entry {
            generation: 0,
            node: Some(node),
        });
        make_id(self.entries.len() - 1, 0)
    }

    fn get(&self, id: NodeId) -> TreeResult<&Node> {
        let (index, generation) = split_id(id);
        self.entries
            .get(index)
            .filter(|entry| entry.generation == generation)
            .and_then(|entry| entry.node.as_ref())
            .ok_or(TaffyError::InvalidInputNode(id))
    }

    fn get_mut(&mut self, id: NodeId) -> TreeResult<&mut Node> {
        let (index, generation) = split_id(id);
        self.entries
            .get_mut(index)
            .filter(|entry| entry.generation == generation)
            .and_then(|entry| entry.node.as_mut())
            .ok_or(TaffyError::InvalidInputNode(id))
    }

    /// The node of an id that layout itself got from this tree.
    fn live(&self, id: NodeId) -> &Node {
        match self.get(id) {
            Ok(node) => node,
            Err(_) => unreachable!("layout asked for a node that is not in the tree"),
        }
    }

    fn live_mut(&mut self, id: NodeId) -> &mut Node {
        match self.get_mut(id) {
            Ok(node) => node,
            Err(_) => unreachable!("layout asked for a node that is not in the tree"),
        }
    }

    pub(crate) fn new_leaf(&mut self, style: Style) -> TreeResult<NodeId> {
        Ok(self.insert(Node::new(style, None)))
    }

    pub(crate) fn new_leaf_with_context(
        &mut self,
        style: Style,
        context: LeafContext,
    ) -> TreeResult<NodeId> {
        Ok(self.insert(Node::new(style, Some(context))))
    }

    /// A node holding `children`, which must not have a parent yet.
    pub(crate) fn new_with_children(
        &mut self,
        style: Style,
        children: &[NodeId],
    ) -> TreeResult<NodeId> {
        let id = self.insert(Node::new(style, None));
        for &child in children {
            self.get_mut(child)?.parent = Some(id);
        }
        self.live_mut(id).children = children.to_vec();
        Ok(id)
    }

    pub(crate) fn remove(&mut self, id: NodeId) -> TreeResult<NodeId> {
        let node = self.get(id)?;
        let (parent, children) = (node.parent, node.children.clone());
        if let Some(parent) = parent {
            self.get_mut(parent)?.children.retain(|&child| child != id);
            self.mark_dirty(parent)?;
        }
        for child in children {
            if let Ok(child) = self.get_mut(child) {
                child.parent = None;
            }
        }
        let (index, _) = split_id(id);
        let entry = &mut self.entries[index];
        entry.node = None;
        entry.generation = entry.generation.wrapping_add(1);
        self.free.push(index as u32);
        Ok(id)
    }

    pub(crate) fn set_children(&mut self, parent: NodeId, children: &[NodeId]) -> TreeResult<()> {
        for child in self.get(parent)?.children.clone() {
            if let Ok(child) = self.get_mut(child) {
                child.parent = None;
            }
        }
        for &child in children {
            if let Some(previous) = self.get(child)?.parent {
                self.get_mut(previous)?.children.retain(|&c| c != child);
                self.mark_dirty(previous)?;
            }
            self.get_mut(child)?.parent = Some(parent);
        }
        self.get_mut(parent)?.children = children.to_vec();
        self.mark_dirty(parent)
    }

    pub(crate) fn set_style(&mut self, id: NodeId, style: Style) -> TreeResult<()> {
        self.get_mut(id)?.style = style;
        self.mark_dirty(id)
    }

    pub(crate) fn style(&self, id: NodeId) -> TreeResult<&Style> {
        Ok(&self.get(id)?.style)
    }

    pub(crate) fn set_node_context(
        &mut self,
        id: NodeId,
        context: Option<LeafContext>,
    ) -> TreeResult<()> {
        self.get_mut(id)?.context = context;
        self.mark_dirty(id)
    }

    pub(crate) fn get_node_context(&self, id: NodeId) -> Option<&LeafContext> {
        self.get(id).ok()?.context.as_ref()
    }

    /// The children of `id`.
    pub(crate) fn children(&self, id: NodeId) -> TreeResult<Vec<NodeId>> {
        Ok(self.get(id)?.children.clone())
    }

    /// The tracks and items the last grid layout of `id` made.
    pub(crate) fn grid_info(&self, id: NodeId) -> Option<&DetailedGridInfo> {
        self.get(id).ok()?.grid_info.as_deref()
    }

    /// The layout of `id` relative to its parent, as the last layout left it.
    pub(crate) fn layout(&self, id: NodeId) -> TreeResult<&Layout> {
        Ok(&self.get(id)?.layout)
    }

    /// Forgets what was measured for `id` and for the nodes above it.
    pub(crate) fn mark_dirty(&mut self, id: NodeId) -> TreeResult<()> {
        let mut current = Some(id);
        while let Some(id) = current {
            let node = self.get_mut(id)?;
            if matches!(node.cache.clear(), ClearState::AlreadyEmpty) {
                break;
            }
            current = node.parent;
        }
        Ok(())
    }

    /// Lays the tree out from `root` in the room `available` gives it.
    pub(crate) fn compute_layout(
        &mut self,
        root: NodeId,
        available: Size<AvailableSpace>,
        font: &mut florui_text::Font,
        node_count: usize,
    ) -> TreeResult<()> {
        self.get(root)?;
        let mut run = Run {
            tree: self,
            font,
            scratch: HashMap::with_capacity(node_count),
        };
        compute_root_layout(&mut run, root, available);
        Ok(())
    }
}

/// One layout pass over a [`FlorTree`]: the tree, the font the leaves are
/// measured with, and what the leaves kept from the sizes they were asked.
struct Run<'a> {
    tree: &'a mut FlorTree,
    font: &'a mut florui_text::Font,
    /// What a text leaf keeps across the many widths Taffy asks it about in
    /// one pass: only text leaves have an entry.
    scratch: HashMap<NodeId, LeafScratch>,
}

impl Run<'_> {
    fn child_layout(
        &mut self,
        node_id: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        // A node under a `display: none` one is laid out hidden whatever its own display.
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node_id);
        }
        compute_cached_layout(self, node_id, inputs, |run, node_id, inputs| {
            let node = run.tree.live(node_id);
            let (display, has_children) = (node.style.display, !node.children.is_empty());
            match (display, has_children) {
                (Display::None, _) => compute_hidden_layout(run, node_id),
                (Display::Block, true) => compute_block_layout(run, node_id, inputs, block_ctx),
                (Display::FlowRoot, true) => compute_block_layout(run, node_id, inputs, None),
                (Display::Flex, true) => compute_flexbox_layout(run, node_id, inputs),
                (Display::Grid, true) => compute_grid_layout(run, node_id, inputs),
                (_, false) => run.measure(node_id, inputs),
            }
        })
    }

    /// Lays out a node with no children: a text leaf, an inline formatting
    /// context, a replaced box, or an empty box.
    fn measure(&mut self, node_id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let Run {
            tree,
            font,
            scratch,
        } = self;
        let Node { style, context, .. } = tree.live_mut(node_id);
        let mut context = context.as_mut();
        let mut measured_baseline = None;
        // Only a text leaf reads or writes its slot: touching the map for
        // every other leaf too would tax the common case (a sized box with no
        // text) for a cache it never uses.
        let mut throwaway = LeafScratch::default();
        let shaping_cache = match context.as_deref() {
            Some(LeafContext::Text(_)) => scratch.entry(node_id).or_default(),
            _ => &mut throwaway,
        };
        // Taffy hands the measure the size the box is known to have, border box and all,
        // where text wraps in what is left of it.
        let across = style.padding.left.resolve_or_zero(None, |_, _| 0.0)
            + style.padding.right.resolve_or_zero(None, |_, _| 0.0)
            + style.border.left.resolve_or_zero(None, |_, _| 0.0)
            + style.border.right.resolve_or_zero(None, |_, _| 0.0);
        let down = style.padding.top.resolve_or_zero(None, |_, _| 0.0)
            + style.padding.bottom.resolve_or_zero(None, |_, _| 0.0)
            + style.border.top.resolve_or_zero(None, |_, _| 0.0)
            + style.border.bottom.resolve_or_zero(None, |_, _| 0.0);
        let mut output = compute_leaf_layout(
            inputs,
            style,
            |_, _| 0.0,
            |known_dimensions, available_space| {
                let known_dimensions = Size {
                    width: known_dimensions.width.map(|w| (w - across).max(0.0)),
                    height: known_dimensions.height.map(|h| (h - down).max(0.0)),
                };
                measure_leaf(
                    font,
                    shaping_cache,
                    context.as_deref_mut(),
                    known_dimensions,
                    available_space,
                    &mut measured_baseline,
                )
            },
        );

        // `measure_leaf` handed back the baseline of the shaping it did.
        // `compute_leaf_layout` does not call it when both sizes are known (an
        // explicit width and height), but such a box still sits on its text's
        // baseline: that one is measured unwrapped, the wrap width being
        // irrelevant to it (see `florui_text::TextMetrics::baseline`).
        let baseline = measured_baseline.or_else(|| match context.as_deref() {
            Some(LeafContext::Text(t)) => Some(
                shaping_cache
                    .answer(None, |shaping| {
                        font.measure_memoized(shaping, t.style, &t.text, None)
                    })
                    .baseline,
            ),
            Some(LeafContext::Inline(context)) => {
                let items = super::to_inline_items(context);
                Some(
                    font.shape_inline_tree_memoized(&context.elements, &items, None)
                        .baseline,
                )
            }
            // A replaced element has no text baseline to recover, and neither
            // does a fixed-size control.
            _ => None,
        });
        if let Some(baseline) = baseline {
            // Measured from the content box; a box's baseline lies below its
            // own padding and border.
            let inset = style.padding.top.resolve_or_zero(None, |_, _| 0.0)
                + style.border.top.resolve_or_zero(None, |_, _| 0.0);
            output.baselines = taffy::Baselines::from_first(Some(baseline + inset));
        }
        output
    }
}

impl TraversePartialTree for Run<'_> {
    type ChildIter<'a>
        = std::iter::Copied<std::slice::Iter<'a, NodeId>>
    where
        Self: 'a;

    fn child_ids(&self, parent: NodeId) -> Self::ChildIter<'_> {
        self.tree.live(parent).children.iter().copied()
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.tree.live(parent).children.len()
    }

    fn get_child_id(&self, parent: NodeId, index: usize) -> NodeId {
        self.tree.live(parent).children[index]
    }
}

impl LayoutPartialTree for Run<'_> {
    type CoreContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;

    type CustomIdent = String;

    fn get_core_container_style(&self, node_id: NodeId) -> Self::CoreContainerStyle<'_> {
        &self.tree.live(node_id).style
    }

    fn set_unrounded_layout(&mut self, node_id: NodeId, layout: &Layout) {
        self.tree.live_mut(node_id).layout = *layout;
    }

    fn resolve_calc_value(&self, _val: *const (), _basis: f32) -> f32 {
        0.0
    }

    fn compute_child_layout(&mut self, node_id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        self.child_layout(node_id, inputs, None)
    }
}

impl CacheTree for Run<'_> {
    fn cache_get(&mut self, node_id: NodeId, input: &LayoutInput) -> Option<LayoutOutput> {
        self.tree.live_mut(node_id).cache.get(input)
    }

    fn cache_store(&mut self, node_id: NodeId, input: &LayoutInput, output: LayoutOutput) {
        self.tree.live_mut(node_id).cache.store(input, output);
    }

    fn cache_clear(&mut self, node_id: NodeId) {
        self.tree.live_mut(node_id).cache.clear();
    }
}

impl LayoutBlockContainer for Run<'_> {
    type BlockContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type BlockItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_block_container_style(&self, node_id: NodeId) -> Self::BlockContainerStyle<'_> {
        &self.tree.live(node_id).style
    }

    fn get_block_child_style(&self, child_node_id: NodeId) -> Self::BlockItemStyle<'_> {
        &self.tree.live(child_node_id).style
    }

    fn compute_block_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        self.child_layout(node_id, inputs, block_ctx)
    }
}

impl LayoutFlexboxContainer for Run<'_> {
    type FlexboxContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type FlexboxItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, node_id: NodeId) -> Self::FlexboxContainerStyle<'_> {
        &self.tree.live(node_id).style
    }

    fn get_flexbox_child_style(&self, child_node_id: NodeId) -> Self::FlexboxItemStyle<'_> {
        &self.tree.live(child_node_id).style
    }
}

impl LayoutGridContainer for Run<'_> {
    type GridContainerStyle<'a>
        = &'a Style
    where
        Self: 'a;
    type GridItemStyle<'a>
        = &'a Style
    where
        Self: 'a;

    fn get_grid_container_style(&self, node_id: NodeId) -> Self::GridContainerStyle<'_> {
        &self.tree.live(node_id).style
    }

    fn get_grid_child_style(&self, child_node_id: NodeId) -> Self::GridItemStyle<'_> {
        &self.tree.live(child_node_id).style
    }

    fn set_detailed_grid_info(&mut self, node_id: NodeId, info: DetailedGridInfo) {
        self.tree.live_mut(node_id).grid_info = Some(Box::new(info));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::prelude::TaffyMaxContent;
    use taffy::{Dimension, Display};

    fn sized(width: f32) -> Style {
        Style {
            size: Size {
                width: Dimension::length(width),
                height: Dimension::length(10.0),
            },
            ..Style::default()
        }
    }

    fn lay_out(tree: &mut FlorTree, root: NodeId) {
        let mut font = florui_text::Font::load_embedded();
        tree.compute_layout(root, Size::MAX_CONTENT, &mut font, 8)
            .unwrap();
    }

    #[test]
    fn a_removed_node_frees_its_slot_but_its_old_id_stays_stale() {
        let mut tree = FlorTree::new();
        let first = tree.new_leaf(Style::default()).unwrap();
        tree.remove(first).unwrap();
        let second = tree.new_leaf(Style::default()).unwrap();
        assert_ne!(first, second, "the slot is reused under a new generation");
        assert!(tree.style(first).is_err());
        assert!(tree.style(second).is_ok());
    }

    #[test]
    fn setting_children_moves_a_child_out_of_its_old_parent() {
        let mut tree = FlorTree::new();
        let child = tree.new_leaf(Style::default()).unwrap();
        let old = tree.new_with_children(Style::default(), &[child]).unwrap();
        let new = tree.new_leaf(Style::default()).unwrap();
        tree.set_children(new, &[child]).unwrap();
        assert!(tree.get(old).unwrap().children.is_empty());
        assert_eq!(tree.get(new).unwrap().children, vec![child]);
        assert_eq!(tree.get(child).unwrap().parent, Some(new));
    }

    #[test]
    fn a_style_change_is_laid_out_again_through_every_ancestor() {
        let mut tree = FlorTree::new();
        let leaf = tree.new_leaf(sized(20.0)).unwrap();
        let middle = tree
            .new_with_children(
                Style {
                    display: Display::Block,
                    ..Style::default()
                },
                &[leaf],
            )
            .unwrap();
        let root = tree
            .new_with_children(
                Style {
                    display: Display::Block,
                    ..Style::default()
                },
                &[middle],
            )
            .unwrap();
        lay_out(&mut tree, root);
        assert_eq!(tree.layout(leaf).unwrap().size.width, 20.0);

        tree.set_style(leaf, sized(35.0)).unwrap();
        lay_out(&mut tree, root);
        assert_eq!(tree.layout(leaf).unwrap().size.width, 35.0);
        assert_eq!(tree.layout(root).unwrap().size.height, 10.0);
    }
}
