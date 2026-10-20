//! `display: contents` boxes are not in the tree layout walks: their children
//! take part in the formatting context of the nearest ancestor that has a box,
//! as if they were its own, in the place the element held. The arena stays as
//! written (inheritance, selectors and events go by it); these read it the way
//! layout and painting see it.

use std::borrow::Cow;

use florui_style::{Display, InlineItem, NodeId, StyleMap, Tree};

/// Whether `node` generates no box and leaves only its children.
pub fn is_contents(styles: &StyleMap, node: NodeId) -> bool {
    styles.may_have_contents()
        && styles
            .get(&node)
            .is_some_and(|style| style.display == Display::Contents)
}

/// The children of `node` that have a box, in order, with those of a
/// `display: contents` child in its place (to any depth).
pub fn flow_children<'a>(arena: &'a Tree, styles: &StyleMap, node: NodeId) -> Cow<'a, [NodeId]> {
    let children = arena.children(node);
    if !children.iter().any(|&child| is_contents(styles, child)) {
        return Cow::Borrowed(children);
    }
    let mut flowed = Vec::with_capacity(children.len());
    push_children(arena, styles, children, &mut flowed);
    Cow::Owned(flowed)
}

fn push_children(arena: &Tree, styles: &StyleMap, children: &[NodeId], out: &mut Vec<NodeId>) {
    for &child in children {
        if is_contents(styles, child) {
            push_children(arena, styles, arena.children(child), out);
        } else {
            out.push(child);
        }
    }
}

/// The inline items of a node as laid out: text and the elements that have a
/// box, with those of a `display: contents` child in its place. Text that
/// ends up next to text is one item, as it is when written next to it, unless
/// the two have different owners: the text of a `display: contents` element
/// keeps the style of that element (its font, its colour), which its parent
/// does not have.
pub struct Flowed<'a> {
    pub items: Cow<'a, [InlineItem]>,
    /// The node whose style each item takes, for the items of a node that has
    /// `display: contents` children; empty when they are all the node's own.
    owners: Vec<NodeId>,
    node: NodeId,
}

impl Flowed<'_> {
    /// The node that the text item `index` belongs to.
    pub fn owner(&self, index: usize) -> NodeId {
        self.owners.get(index).copied().unwrap_or(self.node)
    }

    /// The owners of the items in `range`, empty if they are all the node's.
    pub fn owners(&self, range: std::ops::Range<usize>) -> &[NodeId] {
        if self.owners.is_empty() {
            &[]
        } else {
            &self.owners[range]
        }
    }
}

pub fn flow_items<'a>(arena: &'a Tree, styles: &StyleMap, node: NodeId) -> Flowed<'a> {
    let items = arena.inline_items(node);
    let has_contents = items.iter().any(|item| match item {
        InlineItem::Element(child) => is_contents(styles, *child),
        InlineItem::Text(_) => false,
    });
    if !has_contents {
        return Flowed {
            items: Cow::Borrowed(items),
            owners: Vec::new(),
            node,
        };
    }
    let mut flowed = Vec::with_capacity(items.len());
    let mut owners = Vec::with_capacity(items.len());
    push_items(arena, styles, node, items, &mut flowed, &mut owners);
    Flowed {
        items: Cow::Owned(flowed),
        owners,
        node,
    }
}

fn push_items(
    arena: &Tree,
    styles: &StyleMap,
    owner: NodeId,
    items: &[InlineItem],
    out: &mut Vec<InlineItem>,
    owners: &mut Vec<NodeId>,
) {
    for item in items {
        match item {
            InlineItem::Element(child) if is_contents(styles, *child) => {
                push_items(
                    arena,
                    styles,
                    *child,
                    arena.inline_items(*child),
                    out,
                    owners,
                );
            }
            InlineItem::Text(text) => match (out.last_mut(), owners.last()) {
                (Some(InlineItem::Text(previous)), Some(&last)) if last == owner => {
                    previous.push_str(text);
                }
                _ => {
                    out.push(InlineItem::Text(text.clone()));
                    owners.push(owner);
                }
            },
            other => {
                out.push(other.clone());
                owners.push(owner);
            }
        }
    }
}

/// The nearest ancestor of `node` that has a box: its parent unless that is a
/// `display: contents` element.
pub fn flow_parent(arena: &Tree, styles: &StyleMap, node: NodeId) -> Option<NodeId> {
    let mut parent = arena.parent(node)?;
    while is_contents(styles, parent) {
        parent = arena.parent(parent)?;
    }
    Some(parent)
}
