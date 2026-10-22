//! The node store Taffy's algorithms lay out.
//!
//! `TaffyTree` hands a leaf's measure function nothing but the sizes it is
//! asked about, but a leaf with text has to know the floats of the formatting
//! context it sits in, and a table has to lay its cells out itself. So the
//! nodes live here instead, and the Taffy algorithms are called through
//! [`Run`], which owns the font and decides, node by node, which algorithm
//! lays the node out. Nodes are never rounded: text is shaped at the exact
//! widths layout found (see `compute_layout_with_content_extents`).

use std::cell::OnceCell;
use std::collections::HashMap;

use taffy::util::{MaybeResolve, ResolveOrZero};
use taffy::{
    AvailableSpace, BlockContext, Cache, CacheTree, ClearState, DetailedGridInfo, Display, Layout,
    LayoutBlockContainer, LayoutFlexboxContainer, LayoutGridContainer, LayoutInput, LayoutOutput,
    LayoutPartialTree, NodeId, RunMode, Size, Style, TaffyError, TraversePartialTree,
    compute_block_layout, compute_cached_layout, compute_flexbox_layout, compute_grid_layout,
    compute_hidden_layout, compute_leaf_layout, compute_root_layout,
};

use super::{LeafContext, LeafScratch, measure_leaf};
use crate::table::Role;
use crate::vertical::{self as flow, Flow};

pub(crate) type TreeResult<T> = Result<T, TaffyError>;

pub(crate) struct Node {
    pub(crate) style: Style,
    /// What the node is in a table.
    pub(crate) role: Role,
    pub(crate) cache: Cache,
    pub(crate) layout: Layout,
    pub(crate) children: Vec<NodeId>,
    pub(crate) parent: Option<NodeId>,
    pub(crate) context: Option<LeafContext>,
    /// What the last grid layout of the node made of its tracks and items.
    pub(crate) grid_info: Option<Box<DetailedGridInfo>>,
    /// The first baseline of the last layout, from the top of the border box.
    pub(crate) baseline: Option<f32>,
    /// The room of each line of the content, when a float made some lines
    /// shorter than the box: what laying the content out again needs.
    pub(crate) slots: Option<std::sync::Arc<[florui_text::LineSlot]>>,
    /// How far a table cell pushed its content down inside its box, so that it sits
    /// where `vertical-align` says: part of the padding above it that the style does
    /// not state.
    pub(crate) shift: f32,
    /// For a table with captions: how far down its box the grid starts, and how tall
    /// it is, which is where its border and its background are.
    pub(crate) grid: Option<(f32, f32)>,
    /// For a part of a table with collapsing borders: how wide its border is on each
    /// side (top, right, bottom, left), which is half of the edges it is on.
    pub(crate) border: Option<[f32; 4]>,
    /// What the image marker outside the item puts above the first baseline of its content.
    pub(crate) marker: Option<MarkerRise>,
    /// The writing mode of the node: the frame its content is laid out in.
    pub(crate) flow: Flow,
    /// The writing mode of the parent, the frame the layout of the node is in.
    pub(crate) parent_flow: Flow,
    /// `style` as the algorithms of the content of the node read it, when its flow is
    /// vertical; and as its parent's read it, when the flow of the parent is.
    content: OnceCell<Box<Style>>,
    item: OnceCell<Box<Style>>,
}

/// The image marker outside a list item whose content is blocks. It stands on the baseline of
/// the first line, which comes down until the marker is above it, and with no line the item
/// is as tall as the line the marker is on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MarkerRise {
    /// How high the image is above the baseline.
    pub(crate) height: f32,
    /// What the strut of the item has above and below the baseline.
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
}

impl Node {
    fn new(style: Style, context: Option<LeafContext>) -> Self {
        Node {
            style,
            role: Role::None,
            cache: Cache::new(),
            layout: Layout::with_order(0),
            children: Vec::new(),
            parent: None,
            context,
            grid_info: None,
            baseline: None,
            slots: None,
            shift: 0.0,
            grid: None,
            border: None,
            marker: None,
            flow: Flow::Horizontal,
            parent_flow: Flow::Horizontal,
            content: OnceCell::new(),
            item: OnceCell::new(),
        }
    }

    /// The style the algorithms that lay out the content of the node read: its own, with
    /// the axes of a vertical flow turned.
    pub(crate) fn content_view(&self) -> &Style {
        view(&self.style, &self.content, self.flow)
    }

    /// The style the algorithm of the parent reads, when the parent's `flow` is the one
    /// the node is laid out in.
    pub(crate) fn item_view(&self, parent_flow: Flow) -> &Style {
        // A box of another flow than its parent's is as wide as its content, as a table is,
        // not as wide as the room: what fills it is its own lines, not the parent's.
        let across = self.flow.is_vertical() != parent_flow.is_vertical();
        if !parent_flow.is_vertical() && !across {
            return &self.style;
        }
        self.item.get_or_init(|| {
            let mut style = if parent_flow.is_vertical() {
                flow::view_of(&self.style, parent_flow)
            } else {
                self.style.clone()
            };
            style.item_is_table |= across;
            Box::new(style)
        })
    }
}

fn view<'a>(style: &'a Style, cell: &'a OnceCell<Box<Style>>, flow: Flow) -> &'a Style {
    if flow.is_vertical() {
        cell.get_or_init(|| Box::new(flow::view_of(style, flow)))
    } else {
        style
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
    /// Whether a node has been given a style with a float, which stays so: until one has,
    /// no block formatting context has to be searched for floats.
    floats: bool,
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
            floats: false,
        }
    }

    fn insert(&mut self, node: Node) -> NodeId {
        self.floats |= node.style.float != taffy::Float::None;
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
    pub(crate) fn node(&self, id: NodeId) -> &Node {
        self.live(id)
    }

    pub(crate) fn live(&self, id: NodeId) -> &Node {
        match self.get(id) {
            Ok(node) => node,
            Err(_) => unreachable!("layout asked for a node that is not in the tree"),
        }
    }

    pub(crate) fn live_mut(&mut self, id: NodeId) -> &mut Node {
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

    /// A node that holds `children` without being their parent: it can be laid
    /// out and forgotten ([`Self::discard`]) and they stay where they are.
    pub(crate) fn new_detached(&mut self, style: Style, children: &[NodeId]) -> NodeId {
        let id = self.insert(Node::new(style, None));
        self.live_mut(id).children = children.to_vec();
        id
    }

    /// [`Self::new_detached`], with something to measure instead of children.
    pub(crate) fn new_detached_leaf(&mut self, style: Style, context: LeafContext) -> NodeId {
        self.insert(Node::new(style, Some(context)))
    }

    /// Forgets a node made by [`Self::new_detached`].
    pub(crate) fn discard(&mut self, id: NodeId) {
        let (index, generation) = split_id(id);
        if let Some(entry) = self.entries.get_mut(index)
            && entry.generation == generation
        {
            entry.node = None;
            entry.generation = entry.generation.wrapping_add(1);
            self.free.push(index as u32);
        }
    }

    /// A node holding `children`, which must not have a parent yet.
    pub(crate) fn new_with_children(
        &mut self,
        style: Style,
        children: &[NodeId],
    ) -> TreeResult<NodeId> {
        let id = self.insert(Node::new(style, None));
        for &child in children {
            let child = self.get_mut(child)?;
            child.parent = Some(id);
            child.parent_flow = Flow::Horizontal;
            child.item = OnceCell::new();
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
                child.parent_flow = Flow::Horizontal;
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
                child.parent_flow = Flow::Horizontal;
            }
        }
        for &child in children {
            if let Some(previous) = self.get(child)?.parent {
                self.get_mut(previous)?.children.retain(|&c| c != child);
                self.mark_dirty(previous)?;
            }
            let flow = self.get(parent)?.flow;
            let node = self.get_mut(child)?;
            node.parent = Some(parent);
            node.parent_flow = flow;
            node.item = OnceCell::new();
        }
        self.get_mut(parent)?.children = children.to_vec();
        self.mark_dirty(parent)
    }

    pub(crate) fn set_style(&mut self, id: NodeId, style: Style) -> TreeResult<()> {
        self.floats |= style.float != taffy::Float::None;
        let node = self.get_mut(id)?;
        node.style = style;
        node.content = OnceCell::new();
        node.item = OnceCell::new();
        self.mark_dirty(id)
    }

    pub(crate) fn set_role(&mut self, id: NodeId, role: Role) -> TreeResult<()> {
        let node = self.get_mut(id)?;
        if node.role != role {
            node.role = role;
            // What the node was laid out as a part of a table says nothing of what it is now.
            node.shift = 0.0;
            node.border = None;
            node.grid = None;
            self.mark_dirty(id)?;
        }
        Ok(())
    }

    /// Sets the writing mode of `id`, which decides the frame its content is laid out in.
    pub(crate) fn set_flow(&mut self, id: NodeId, flow: Flow) -> TreeResult<()> {
        let node = self.get_mut(id)?;
        if node.flow == flow {
            return Ok(());
        }
        node.flow = flow;
        node.content = OnceCell::new();
        node.item = OnceCell::new();
        for child in node.children.clone() {
            if let Ok(child) = self.get_mut(child) {
                child.parent_flow = flow;
                child.item = OnceCell::new();
            }
        }
        self.mark_dirty(id)
    }

    /// The flow of the parent of `id`, the frame the layout of `id` is in.
    pub(crate) fn parent_flow(&self, id: NodeId) -> Flow {
        self.live(id).parent_flow
    }

    pub(crate) fn set_marker(&mut self, id: NodeId, marker: Option<MarkerRise>) -> TreeResult<()> {
        let node = self.get_mut(id)?;
        if node.marker != marker {
            node.marker = marker;
            self.mark_dirty(id)?;
        }
        Ok(())
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

    /// The layout of `id` as it is on the screen: what the last layout made in the frame
    /// of the flow of its parent, turned to the screen's axes.
    pub(crate) fn physical_layout(&self, id: NodeId) -> TreeResult<Layout> {
        let node = self.get(id)?;
        let frame = self.parent_flow(id);
        if !frame.is_vertical() {
            return Ok(node.layout);
        }
        let container = node
            .parent
            .map_or((0.0, 0.0), |parent| self.physical_size(parent));
        Ok(flow::layout_to_physical(&node.layout, frame, container))
    }

    /// How wide and how tall the border box of `id` is on the screen.
    pub(crate) fn physical_size(&self, id: NodeId) -> (f32, f32) {
        let size = self.live(id).layout.size;
        if self.parent_flow(id).is_vertical() {
            (size.height, size.width)
        } else {
            (size.width, size.height)
        }
    }

    /// The room of each line of the inline content of `id`, if a float narrowed any.
    pub(crate) fn line_slots(&self, id: NodeId) -> Option<std::sync::Arc<[florui_text::LineSlot]>> {
        self.get(id).ok()?.slots.clone()
    }

    /// Where the first baseline of `id` lies, from the top of its border box.
    pub(crate) fn first_baseline(&self, id: NodeId) -> Option<f32> {
        self.get(id).ok()?.baseline
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
        let floats = self.floats;
        let mut run = Run {
            tree: self,
            font,
            scratch: HashMap::with_capacity(node_count),
            regions: HashMap::new(),
            floats,
        };
        compute_root_layout(&mut run, root, available);
        Ok(())
    }
}

/// One layout pass over a [`FlorTree`]: the tree, the font the leaves are
/// measured with, and what the leaves kept from the sizes they were asked.
pub(crate) struct Run<'a> {
    pub(crate) tree: &'a mut FlorTree,
    pub(crate) font: &'a mut florui_text::Font,
    /// What a text leaf keeps across the many widths Taffy asks it about in
    /// one pass: only text leaves have an entry.
    pub(crate) scratch: HashMap<NodeId, LeafScratch>,
    /// Whether the context a node roots has floats in it; see `region_has_float`.
    pub(crate) regions: HashMap<NodeId, bool>,
    /// Whether any node of the tree has a float: see [`FlorTree`].
    pub(crate) floats: bool,
}

impl Run<'_> {
    /// The style of `id` as its parent's algorithm reads it.
    fn item_style(&self, id: NodeId) -> &Style {
        let node = self.tree.live(id);
        node.item_view(node.parent_flow)
    }

    pub(crate) fn child_layout(
        &mut self,
        node_id: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        // A node under a `display: none` one is laid out hidden whatever its own display.
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            return compute_hidden_layout(self, node_id);
        }
        let output = compute_cached_layout(self, node_id, inputs, |run, node_id, inputs| {
            // A box whose content flows the other way is laid out in the frame of its own
            // flow, and tells its parent what it made of the room in the parent's.
            if run.tree.live(node_id).flow.is_vertical()
                != run.tree.parent_flow(node_id).is_vertical()
            {
                let output = run.layout_content(node_id, flow::transpose_input(inputs), block_ctx);
                return flow::transpose_output(output);
            }
            run.layout_content(node_id, inputs, block_ctx)
        });
        if inputs.run_mode == RunMode::PerformLayout {
            self.tree.live_mut(node_id).baseline = output.baselines.first;
        }
        output
    }

    /// Lays out the content of a node with the algorithm its display asks for, in the
    /// frame its flow gives and the sizes of that frame.
    fn layout_content(
        &mut self,
        node_id: NodeId,
        inputs: LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        let node = self.tree.live(node_id);
        let (display, has_children) = (node.style.display, !node.children.is_empty());
        if node.role.is_table() {
            return self.compute_table(node_id, inputs);
        }
        let output = if matches!(display, Display::Block | Display::FlowRoot)
            && self.region_has_float(node_id)
        {
            self.flow_region_root(node_id, inputs)
        } else {
            match (display, has_children) {
                (Display::None, _) => compute_hidden_layout(self, node_id),
                (Display::Block, true) => compute_block_layout(self, node_id, inputs, block_ctx),
                (Display::FlowRoot, true) => compute_block_layout(self, node_id, inputs, None),
                (Display::Flex, true) => compute_flexbox_layout(self, node_id, inputs),
                (Display::Grid, true) => compute_grid_layout(self, node_id, inputs),
                (_, false) => self.measure(node_id, inputs),
            }
        };
        self.raise_for_marker(node_id, &inputs, output)
    }

    /// What a list item with an image marker outside it is, which the marker puts down: the
    /// content comes down until its first baseline is below the top of the image, and with
    /// no baseline the item is as tall as the line the marker is on.
    fn raise_for_marker(
        &mut self,
        node_id: NodeId,
        inputs: &LayoutInput,
        mut output: LayoutOutput,
    ) -> LayoutOutput {
        let node = self.tree.live(node_id);
        let Some(marker) = node.marker else {
            return output;
        };
        let style = &node.style;
        let basis = inputs.parent_size.width;
        let edge = |side: taffy::LengthPercentage| side.resolve_or_zero(basis, |_, _| 0.0);
        let above = edge(style.border.top) + edge(style.padding.top);
        let below = edge(style.border.bottom) + edge(style.padding.bottom);
        let definite = inputs.known_dimensions.height.is_some()
            || style
                .size
                .height
                .maybe_resolve(inputs.parent_size.height, |_, _| 0.0)
                .is_some();
        let natural = output.size.height;
        let clamped = |height: f32| {
            let max = style
                .max_size
                .height
                .maybe_resolve(inputs.parent_size.height, |_, _| 0.0);
            match max {
                Some(max) if style.box_sizing == taffy::BoxSizing::ContentBox => {
                    height.min((max + above + below).max(natural))
                }
                Some(max) => height.min(max.max(natural)),
                None => height,
            }
        };
        let Some(baseline) = output.baselines.first else {
            let line = marker.height.max(marker.ascent) + marker.descent;
            let needed = clamped(above + line + below);
            if !definite && output.size.height < needed {
                output.size.height = needed;
                output.margins_can_collapse_through = false;
            }
            return output;
        };
        let delta = (marker.height - (baseline - above)).max(0.0);
        if delta == 0.0 {
            return output;
        }
        if !definite {
            output.size.height = clamped(output.size.height + delta);
        }
        output.scrollable_overflow_rect.bottom += delta;
        output.baselines.first = Some(baseline + delta);
        if inputs.run_mode == RunMode::PerformLayout {
            for child in self.tree.live(node_id).children.clone() {
                if matches!(
                    self.part_of(child),
                    crate::blockflow::Part::Hidden | crate::blockflow::Part::Absolute
                ) {
                    continue;
                }
                self.tree.live_mut(child).layout.location.y += delta;
            }
        }
        output
    }

    /// Lays out a node with no children: a text leaf, an inline formatting
    /// context, a replaced box, or an empty box.
    fn measure(&mut self, node_id: NodeId, inputs: LayoutInput) -> LayoutOutput {
        let Run {
            tree,
            font,
            scratch,
            ..
        } = self;
        if inputs.run_mode == RunMode::PerformLayout {
            // The lines of a leaf in a Taffy layout are the whole width; the room floats left
            // them is of the layout of a block that has them.
            tree.live_mut(node_id).slots = None;
        }
        let Node {
            style,
            context,
            content,
            flow,
            ..
        } = tree.live_mut(node_id);
        let flow = *flow;
        // The sides and the sizes in the frame the content of the leaf is laid out in.
        let style = view(style, content, flow);
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
        // What has a size of its own (a picture, a control) is not turned with the lines: its
        // width is its width on the page, so it is measured as the page has it.
        let upright = flow.is_vertical()
            && matches!(
                context.as_deref(),
                Some(LeafContext::Fixed(_) | LeafContext::Image(_))
            );
        let mut output = compute_leaf_layout(
            inputs,
            style,
            |_, _| 0.0,
            |known_dimensions, available_space| {
                let known_dimensions = Size {
                    width: known_dimensions.width.map(|w| (w - across).max(0.0)),
                    height: known_dimensions.height.map(|h| (h - down).max(0.0)),
                };
                if upright {
                    let size = measure_leaf(
                        font,
                        shaping_cache,
                        context.as_deref_mut(),
                        flow::swapped(known_dimensions),
                        flow::swapped(available_space),
                        &mut measured_baseline,
                    );
                    return flow::swapped(size);
                }
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
        self.tree.live(node_id).content_view()
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
        self.tree.live(node_id).content_view()
    }

    fn get_block_child_style(&self, child_node_id: NodeId) -> Self::BlockItemStyle<'_> {
        self.item_style(child_node_id)
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
        self.tree.live(node_id).content_view()
    }

    fn get_flexbox_child_style(&self, child_node_id: NodeId) -> Self::FlexboxItemStyle<'_> {
        self.item_style(child_node_id)
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
        self.tree.live(node_id).content_view()
    }

    fn get_grid_child_style(&self, child_node_id: NodeId) -> Self::GridItemStyle<'_> {
        self.item_style(child_node_id)
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
