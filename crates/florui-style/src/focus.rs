//! Structural identity for "the currently focused element" — stable
//! across renders, unlike [`NodeId`] itself.
//!
//! [`Arena::build`] reruns from scratch on every render and `NodeId` is
//! just preorder position, so it can't survive across renders the way
//! focus needs to (typing, async updates, anything). This mirrors
//! [`crate::animation`]'s own `PathKey` scheme (`{parent, tag, ordinal}`),
//! simplified: there's only ever one focused element at a time, so no
//! interning table or per-call GC is needed — just a path to walk back
//! down next render.

use crate::tree::{Arena, NodeId};

/// One step of a [`FocusPath`]: an element with an `id` is identified by it,
/// so it survives siblings appearing or disappearing around it (a scrolled
/// virtual list); one without is identified by its place among same-tag
/// siblings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PathSegment {
    tag: &'static str,
    id: Option<String>,
    ordinal: usize,
}

/// A focused element's identity, independent of any single [`Arena`]
/// generation. See the module doc for why [`NodeId`] alone can't serve
/// this role.
///
/// `Hash` lets a path key a `HashMap` — e.g. an accessibility-tree
/// identity interner, which needs the same stable-across-renders scheme.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FocusPath(Vec<PathSegment>);

impl FocusPath {
    /// Captures `id`'s path in `arena`, root to leaf.
    pub fn of(arena: &Arena, id: NodeId) -> Self {
        let mut segments = Vec::new();
        let mut current = Some(id);
        while let Some(node) = current {
            let element_id = arena.id_attr(node).map(str::to_owned);
            segments.push(PathSegment {
                tag: arena.tag(node),
                ordinal: if element_id.is_some() {
                    0
                } else {
                    arena.tag_ordinal(node)
                },
                id: element_id,
            });
            current = arena.parent(node);
        }
        segments.reverse();
        Self(segments)
    }

    /// Finds the node in a fresh `arena` that this path still identifies,
    /// if any — a linear scan against `candidates` (the focusable set,
    /// not every node), once per render rather than a hot path.
    pub fn resolve(&self, arena: &Arena, candidates: &[NodeId]) -> Option<NodeId> {
        candidates
            .iter()
            .copied()
            .find(|&id| &Self::of(arena, id) == self)
    }

    /// The ordinal found by scanning the siblings before `id`, which is what
    /// [`Arena::tag_ordinal`] has to equal: earlier same-tag siblings only,
    /// class-insensitive, the same tie-breaker [`crate::stylo`]'s
    /// `StyloTree::sibling_ordinal` uses. Linear in the siblings, so a node
    /// among a thousand rows cost a thousand comparisons, and building every
    /// node's path in a long list was quadratic.
    #[cfg(test)]
    fn sibling_ordinal_by_scan(arena: &Arena, id: NodeId) -> usize {
        let siblings: &[NodeId] = match arena.parent(id) {
            Some(parent_id) => arena.children(parent_id),
            None => arena.roots(),
        };
        let tag = arena.tag(id);
        siblings
            .iter()
            .take_while(|&&sibling| sibling != id)
            .filter(|&&sibling| arena.tag(sibling) == tag)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    #[test]
    fn of_and_resolve_round_trip_across_a_fresh_arena() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <button>{"Second"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let second = arena.find_all(|a, id| a.tag(id) == "button")[1];
        let path = FocusPath::of(&arena, second);

        // A structurally identical, independently rebuilt arena — the
        // same relationship a real render-to-render `update()` has.
        let rebuilt = Arena::build(&tree);
        let candidates = rebuilt.find_all(|a, id| a.tag(id) == "button");
        let resolved = path.resolve(&rebuilt, &candidates).unwrap();
        assert_eq!(rebuilt.text_content(resolved), "Second");
    }

    #[test]
    fn an_element_with_an_id_is_followed_through_its_siblings_shifting() {
        let before: Element = view! {
            <div>
                <button id="a">{"A"}</button>
                <button id="b">{"B"}</button>
            </div>
        };
        let after: Element = view! {
            <div>
                <button id="z">{"Z"}</button>
                <button id="a">{"A"}</button>
                <button id="b">{"B"}</button>
            </div>
        };
        let arena = Arena::build(&before);
        let b = arena.find(|a, id| a.id_attr(id) == Some("b")).unwrap();
        let path = FocusPath::of(&arena, b);

        let shifted = Arena::build(&after);
        let candidates = shifted.find_all(|a, id| a.tag(id) == "button");
        let resolved = path.resolve(&shifted, &candidates).unwrap();
        assert_eq!(
            shifted.text_content(resolved),
            "B",
            "not the button now in B's old place"
        );
    }

    #[test]
    fn the_counted_ordinal_equals_the_scanned_one_for_every_node() {
        // Repeated tags at several levels, mixed tags among siblings, several
        // roots (a fragment), and a portal's content as later roots.
        let tree: Element = view! {
            <div>
                <button>{"a"}</button>
                <span>{"b"}</span>
                <button>{"c"}</button>
                <div>
                    <p>{"x"}</p>
                    <p>{"y"}</p>
                    <button>{"z"}</button>
                    <p>{"w"}</p>
                </div>
                <button>{"d"}</button>
            </div>
            <div>{"second root"}</div>
            <div>{"third root"}</div>
        };
        let arena = Arena::build(&tree);
        assert!(arena.roots().len() >= 3, "several roots");
        let mut checked = 0;
        for id in 0..arena.find_all(|_, _| true).len() {
            assert_eq!(
                arena.tag_ordinal(id),
                FocusPath::sibling_ordinal_by_scan(&arena, id),
                "node {id} <{}>",
                arena.tag(id)
            );
            checked += 1;
        }
        assert!(checked >= 12);
        // The ordinals really count: `d` is the third button among its siblings,
        // while `z`, the third in document order, is the first in its own div.
        let buttons = arena.find_all(|a, id| a.tag(id) == "button");
        assert_eq!(arena.tag_ordinal(buttons[3]), 2);
        assert_eq!(arena.tag_ordinal(buttons[2]), 0);
    }

    #[test]
    fn the_counted_ordinal_continues_across_a_portals_roots() {
        let div = |text: &str| Element::node("div", vec![], vec![Element::text(text)]);
        let tree = Element::Fragment(vec![
            div("first"),
            Element::Portal(vec![div("in a portal"), div("also in it")]),
            div("second"),
        ]);
        let arena = Arena::build(&tree);
        // Two document roots, then the portal's content as later roots.
        assert_eq!(arena.roots().len(), 4);
        let ordinals: Vec<usize> = arena
            .roots()
            .iter()
            .map(|&id| arena.tag_ordinal(id))
            .collect();
        assert_eq!(ordinals, vec![0, 1, 2, 3], "one count over every root");
        for &id in arena.roots() {
            assert_eq!(
                arena.tag_ordinal(id),
                FocusPath::sibling_ordinal_by_scan(&arena, id)
            );
        }
    }

    #[test]
    fn an_element_without_an_id_is_still_found_by_its_place() {
        let before: Element = view! { <div><button>{"One"}</button><button>{"Two"}</button></div> };
        let after: Element =
            view! { <div><button>{"Other"}</button><button>{"Moved"}</button></div> };
        let arena = Arena::build(&before);
        let second = arena.find_all(|a, id| a.tag(id) == "button")[1];
        let path = FocusPath::of(&arena, second);
        let rebuilt = Arena::build(&after);
        let candidates = rebuilt.find_all(|a, id| a.tag(id) == "button");
        let resolved = path.resolve(&rebuilt, &candidates).unwrap();
        assert_eq!(rebuilt.text_content(resolved), "Moved");
    }
    #[test]
    fn resolve_returns_none_once_the_element_is_gone() {
        let tree: Element = view! {
            <div>
                <button>{"Only"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let button = arena.find_all(|a, id| a.tag(id) == "button")[0];
        let path = FocusPath::of(&arena, button);

        let empty: Element = view! { <div /> };
        let rebuilt = Arena::build(&empty);
        let candidates = rebuilt.find_all(|a, id| a.tag(id) == "button");
        assert_eq!(path.resolve(&rebuilt, &candidates), None);
    }

    #[test]
    fn distinct_positions_produce_distinct_paths() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <button>{"Second"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let buttons = arena.find_all(|a, id| a.tag(id) == "button");
        let first_path = FocusPath::of(&arena, buttons[0]);
        let second_path = FocusPath::of(&arena, buttons[1]);
        assert_ne!(first_path, second_path);
    }
}
