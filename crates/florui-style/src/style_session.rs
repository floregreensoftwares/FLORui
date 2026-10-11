use style::thread_state::{self, ThreadState};

use crate::animation::{AnimationTimeline, TimelineId};
use crate::cascade::Viewport;
use crate::interaction::InteractionState;
use crate::style_map::StyleMap;
use crate::stylesheet_parse::Rule;
use crate::stylo::{StyloTree, compute_with_tree};
use crate::tree::Arena;

/// The cascade for a caller that runs it again and again on the same tree.
///
/// Building the tree the cascade walks (a copy of every node's tag, attributes
/// and classes, linked to its parent and children) was over a third of its cost,
/// and it is the same for as long as the elements are: [`Arena::style_shape`]
/// says when they are. A session keeps that tree and, on each call, only sets
/// the state flags of its nodes again and clears what the last cascade left
/// in them. A different arena shape, or a different [`AnimationTimeline`] (the
/// identities of the nodes are the timeline's), builds it anew. The styles are
/// what [`crate::compute`] gives.
#[derive(Default)]
pub struct StyleSession {
    kept: Option<Kept>,
}

struct Kept {
    shape: u64,
    timeline: TimelineId,
    tree: StyloTree,
}

impl StyleSession {
    pub fn new() -> Self {
        Self::default()
    }

    /// [`crate::compute`] on the tree kept from the last call when it is still
    /// the same.
    pub fn compute(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> StyleMap {
        thread_state::enter(ThreadState::LAYOUT);
        let styles = self.compute_in_layout_state(arena, rules, state, viewport, timeline);
        thread_state::exit(ThreadState::LAYOUT);
        styles
    }

    fn compute_in_layout_state(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> StyleMap {
        if arena.roots().is_empty() {
            self.kept = None;
            return StyleMap::new();
        }
        let shape = arena.style_shape();
        let same = self
            .kept
            .as_ref()
            .is_some_and(|kept| kept.shape == shape && kept.timeline == timeline.id());
        if same {
            if let Some(kept) = &self.kept {
                kept.tree.refresh(arena, state);
                timeline.touch(kept.tree.stable_ids());
            }
        } else {
            self.kept = None;
            let (tree, _primary_root) = StyloTree::new(arena, state, timeline);
            self.kept = Some(Kept {
                shape,
                timeline: timeline.id(),
                tree,
            });
        }
        match &self.kept {
            Some(kept) => compute_with_tree(&kept.tree, rules, viewport, timeline, &[]),
            None => StyleMap::new(),
        }
    }
}

impl Drop for StyleSession {
    /// A tree is dropped where Stylo expects the styles in it to be.
    fn drop(&mut self) {
        if self.kept.is_some() {
            thread_state::enter(ThreadState::LAYOUT);
            self.kept = None;
            thread_state::exit(ThreadState::LAYOUT);
        }
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;
    use crate::{compute, parse_stylesheet};

    const CSS: &str = ".row { color: #ff0000; width: 10px; transition: color 1s linear; } \
         .row:hover { color: #0000ff; } \
         .row:hover .cell { width: 40px; } \
         .cell { padding: 2px; } \
         .row + .row { margin-top: 3px; } \
         .odd { font-size: 20px; } \
         input:focus { width: 90px; }";

    /// A list of `rows` rows of two cells each, some of them odd, one input;
    /// `text` is written in the first cell of each.
    fn list(rows: usize, text: &str) -> Element {
        let class = |c: &str| vec![("class".to_string(), c.to_string())];
        let mut children: Vec<Element> = (0..rows)
            .map(|i| {
                Element::node(
                    "div",
                    class(if i % 3 == 0 { "row odd" } else { "row" }),
                    vec![
                        Element::node(
                            "span",
                            class("cell"),
                            vec![Element::text(format!("{text}{i}"))],
                        ),
                        Element::node("span", class("cell"), vec![]),
                    ],
                )
            })
            .collect();
        children.push(Element::node(
            "input",
            vec![("type".into(), "text".into())],
            vec![],
        ));
        Element::node("div", class("list"), children)
    }

    #[test]
    fn a_kept_tree_styles_as_a_tree_built_anew_through_states_text_and_time() {
        let rules = parse_stylesheet(CSS).unwrap();
        let viewport = Viewport::default();
        let mut session = StyleSession::new();
        let mut kept_timeline = AnimationTimeline::default();
        let mut fresh_timeline = AnimationTimeline::default();
        let mut arena = Arena::build(&list(6, "a"));
        let mut calls = 0;
        let mut step = |arena: &Arena, state: &InteractionState, now: f64, what: &str| {
            kept_timeline.advance_to(now);
            fresh_timeline.advance_to(now);
            let kept = session.compute(arena, &rules, state, viewport, &mut kept_timeline);
            let fresh = compute(arena, &rules, state, viewport, &mut fresh_timeline);
            assert_eq!(kept, fresh, "{what}");
            calls += 1;
        };

        let rows = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"));
        let input = arena.find(|a, id| a.tag(id) == "input").unwrap();
        let mut now = 0.0;
        // The same elements, over and over, with the state moving about and
        // time passing (a transition starts, runs and ends).
        for round in 0..24usize {
            now += 0.21;
            let mut state = InteractionState::new();
            if round % 5 != 4 {
                let row = rows[round % rows.len()];
                state = state.with_hovered(row);
                let mut up = arena.parent(row);
                while let Some(parent) = up {
                    state = state.with_hovered(parent);
                    up = arena.parent(parent);
                }
            }
            if round % 7 == 3 {
                state = state.with_focused(input);
            }
            step(
                &arena,
                &state,
                now,
                &format!("same elements, round {round}"),
            );
        }
        // Other text in the same elements: the elements are kept.
        arena.rebuild(&list(6, "b"));
        step(&arena, &InteractionState::new(), now + 0.1, "other text");
        // Other elements: built anew.
        arena.rebuild(&list(9, "b"));
        let hovered = InteractionState::new().with_hovered(0);
        step(&arena, &hovered, now + 0.2, "more rows");
        step(&arena, &hovered, now + 0.3, "more rows, again");
        assert_eq!(calls, 24 + 3);
    }

    #[test]
    fn another_timeline_gets_a_tree_of_its_own() {
        let rules = parse_stylesheet(CSS).unwrap();
        let viewport = Viewport::default();
        let arena = Arena::build(&list(4, "a"));
        let state = InteractionState::new().with_hovered(1);
        let mut session = StyleSession::new();
        for _ in 0..3 {
            let mut timeline = AnimationTimeline::default();
            let kept = session.compute(&arena, &rules, &state, viewport, &mut timeline);
            let fresh = compute(
                &arena,
                &rules,
                &state,
                viewport,
                &mut AnimationTimeline::default(),
            );
            assert_eq!(kept, fresh);
        }
    }
}
