use std::collections::HashMap;
use std::collections::hash_map;
use std::ops::Index;
use std::sync::Arc;

use crate::cascade::{BlockAlign, ComputedStyle, Display, Float};
use crate::tree::NodeId;

/// The style of every node of a tree.
///
/// Most nodes of a real tree share a few distinct styles (the rows of a list
/// are alike), and a style is large, so this holds each one once and hands the
/// same one to every node that has it: cloning the map, or giving a node the
/// style of another, copies a pointer. It reads like a map of styles; a style
/// that is written through [`Self::get_mut`] is copied first if another node
/// holds it.
#[derive(Clone, Default, PartialEq)]
pub struct StyleMap {
    styles: HashMap<NodeId, Arc<ComputedStyle>>,
    kinds: Kinds,
}

/// Which rare kinds of box the styles of a map may hold, so that code which asks of
/// every child whether it is one can skip the question when none can be. A kind is set
/// when a style that is one is put in, and by any write through [`StyleMap::get_mut`],
/// which cannot be told apart; it is never cleared. Two maps are equal whatever they
/// hint.
#[derive(Clone, Copy, Default)]
struct Kinds {
    contents: bool,
    floats: bool,
    list_items: bool,
    block_align: bool,
}

impl PartialEq for Kinds {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Kinds {
    fn note(&mut self, style: &ComputedStyle) {
        self.contents |= style.display == Display::Contents;
        self.floats |= style.float != Float::None;
        self.list_items |= style.display == Display::ListItem;
        self.block_align |= style.block_align != BlockAlign::None;
    }

    fn any(&mut self) {
        *self = Kinds {
            contents: true,
            floats: true,
            list_items: true,
            block_align: true,
        };
    }
}

impl StyleMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            styles: HashMap::with_capacity(capacity),
            kinds: Kinds::default(),
        }
    }

    /// Whether a style may be `display: contents`.
    pub fn may_have_contents(&self) -> bool {
        self.kinds.contents
    }

    /// Whether a style may be a float.
    pub fn may_have_floats(&self) -> bool {
        self.kinds.floats
    }

    /// Whether a style may align the blocks in it (`text-align: -webkit-center` and the like).
    pub fn may_have_block_align(&self) -> bool {
        self.kinds.block_align
    }

    /// Whether a style may be a list item.
    pub fn may_have_list_items(&self) -> bool {
        self.kinds.list_items
    }

    pub fn len(&self) -> usize {
        self.styles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.styles.is_empty()
    }

    pub fn get(&self, id: &NodeId) -> Option<&ComputedStyle> {
        self.styles.get(id).map(|style| &**style)
    }

    /// The style of `id` to change. It is copied first if another node has the
    /// same one, so the change is this node's alone.
    pub fn get_mut(&mut self, id: &NodeId) -> Option<&mut ComputedStyle> {
        self.kinds.any();
        self.styles.get_mut(id).map(Arc::make_mut)
    }

    /// [`Self::get_mut`] for a write that changes neither the `display` nor the `float` of the
    /// style, which leaves what the map may hold as it was.
    pub fn get_mut_same_kind(&mut self, id: &NodeId) -> Option<&mut ComputedStyle> {
        self.styles.get_mut(id).map(Arc::make_mut)
    }

    pub fn contains_key(&self, id: &NodeId) -> bool {
        self.styles.contains_key(id)
    }

    pub fn insert(&mut self, id: NodeId, style: ComputedStyle) {
        self.kinds.note(&style);
        self.styles.insert(id, Arc::new(style));
    }

    /// Gives `id` a style another node already has, without copying it.
    pub fn insert_shared(&mut self, id: NodeId, style: Arc<ComputedStyle>) {
        self.kinds.note(&style);
        self.styles.insert(id, style);
    }

    pub fn remove(&mut self, id: &NodeId) -> Option<ComputedStyle> {
        self.styles
            .remove(id)
            .map(|style| Arc::try_unwrap(style).unwrap_or_else(|shared| (*shared).clone()))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&NodeId, &ComputedStyle)> {
        self.styles.iter().map(|(id, style)| (id, &**style))
    }

    pub fn keys(&self) -> impl Iterator<Item = &NodeId> {
        self.styles.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &ComputedStyle> {
        self.styles.values().map(|style| &**style)
    }
}

impl Index<&NodeId> for StyleMap {
    type Output = ComputedStyle;

    fn index(&self, id: &NodeId) -> &ComputedStyle {
        &self.styles[id]
    }
}

impl std::fmt::Debug for StyleMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl FromIterator<(NodeId, ComputedStyle)> for StyleMap {
    fn from_iter<I: IntoIterator<Item = (NodeId, ComputedStyle)>>(iter: I) -> Self {
        let mut map = Self::default();
        map.extend(iter);
        map
    }
}

impl Extend<(NodeId, ComputedStyle)> for StyleMap {
    fn extend<I: IntoIterator<Item = (NodeId, ComputedStyle)>>(&mut self, iter: I) {
        for (id, style) in iter {
            self.insert(id, style);
        }
    }
}

impl<'a> IntoIterator for &'a StyleMap {
    type Item = (&'a NodeId, &'a ComputedStyle);
    type IntoIter = std::iter::Map<
        hash_map::Iter<'a, NodeId, Arc<ComputedStyle>>,
        fn((&'a NodeId, &'a Arc<ComputedStyle>)) -> (&'a NodeId, &'a ComputedStyle),
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn deref<'a>(
            (id, style): (&'a NodeId, &'a Arc<ComputedStyle>),
        ) -> (&'a NodeId, &'a ComputedStyle) {
            (id, &**style)
        }
        self.styles.iter().map(deref)
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;
    use crate::{Arena, InteractionState, Viewport};

    /// The style of a plain `<div>` and the same with a width.
    fn two_styles() -> (ComputedStyle, ComputedStyle) {
        let tree: Element = Element::node("div", vec![], vec![]);
        let arena = Arena::build(&tree);
        let styles = crate::compute(
            &arena,
            &[],
            &InteractionState::new(),
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        let plain = styles[&0].clone();
        let mut wide = plain.clone();
        wide.width = Some(40.0);
        (plain, wide)
    }

    #[test]
    fn a_style_written_through_get_mut_is_the_nodes_alone() {
        let (plain, _) = two_styles();
        let shared = Arc::new(plain.clone());
        let mut map = StyleMap::new();
        map.insert_shared(1, Arc::clone(&shared));
        map.insert_shared(2, Arc::clone(&shared));

        map.get_mut(&1).unwrap().width = Some(10.0);

        assert_eq!(map.get(&1).unwrap().width, Some(10.0));
        assert_eq!(map.get(&2), Some(&plain), "the other node keeps its style");
        assert_eq!(shared.width, plain.width, "and so does the shared style");
    }

    #[test]
    fn a_clone_of_the_map_shares_the_styles_and_reads_the_same() {
        let (plain, wide) = two_styles();
        let mut map = StyleMap::new();
        map.insert(1, plain.clone());
        map.insert(2, wide.clone());
        let copy = map.clone();
        assert_eq!(copy, map);
        assert_eq!(copy.len(), 2);
        assert_eq!(copy[&2], wide);
        assert_eq!(copy.get(&3), None);
        assert!(copy.contains_key(&1));
        let mut seen: Vec<_> = copy.iter().map(|(&id, style)| (id, style.width)).collect();
        seen.sort_by_key(|&(id, _)| id);
        assert_eq!(seen, vec![(1, plain.width), (2, Some(40.0))]);
    }

    #[test]
    fn the_map_says_which_rare_kinds_of_box_it_may_hold() {
        let (plain, _) = two_styles();
        let mut map = StyleMap::new();
        map.insert(1, plain.clone());
        assert!(!map.may_have_contents() && !map.may_have_floats() && !map.may_have_list_items());

        let mut floated = plain.clone();
        floated.float = Float::Left;
        map.insert(2, floated);
        assert!(map.may_have_floats());
        assert!(!map.may_have_contents() && !map.may_have_list_items());

        let mut contents = plain.clone();
        contents.display = Display::Contents;
        map.insert_shared(3, Arc::new(contents));
        assert!(map.may_have_contents());

        let mut aligned = plain.clone();
        aligned.block_align = BlockAlign::Center;
        map.insert(4, aligned);
        assert!(map.may_have_block_align());

        let mut item = plain;
        item.display = Display::ListItem;
        let collected: StyleMap = [(1, item)].into_iter().collect();
        assert!(collected.may_have_list_items());
    }

    #[test]
    fn a_write_that_may_change_the_kind_of_a_box_makes_every_kind_possible() {
        let (plain, _) = two_styles();
        let mut map = StyleMap::new();
        map.insert(1, plain);
        map.get_mut_same_kind(&1).unwrap().width = Some(3.0);
        assert!(!map.may_have_contents());
        map.get_mut(&1).unwrap().display = Display::Contents;
        assert!(map.may_have_contents() && map.may_have_floats() && map.may_have_list_items());
    }

    #[test]
    fn maps_with_the_same_styles_are_equal_whether_or_not_they_share_them() {
        let (plain, wide) = two_styles();
        let by_value: StyleMap = [(1, plain.clone()), (2, wide.clone())]
            .into_iter()
            .collect();
        let mut shared = StyleMap::new();
        shared.insert_shared(1, Arc::new(plain));
        shared.insert_shared(2, Arc::new(wide));
        assert_eq!(by_value, shared);
        shared.get_mut(&1).unwrap().width = Some(1.0);
        assert_ne!(by_value, shared);
    }

    #[test]
    fn removing_a_style_gives_it_back_even_when_it_is_shared() {
        let (plain, _) = two_styles();
        let shared = Arc::new(plain.clone());
        let mut map = StyleMap::new();
        map.insert_shared(1, Arc::clone(&shared));
        map.insert_shared(2, shared);
        assert_eq!(map.remove(&1), Some(plain.clone()));
        assert_eq!(map.remove(&2), Some(plain));
        assert!(map.is_empty());
    }
}
