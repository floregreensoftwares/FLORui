use style::thread_state::{self, ThreadState};

use crate::animation::{AnimationTimeline, TimelineId};
use crate::cascade::Viewport;
use crate::interaction::InteractionState;
use crate::state_reach::StateReach;
use crate::style_map::StyleMap;
use crate::stylesheet_parse::Rule;
use crate::stylo::{StyloTree, compute_with_tree, restyle_nodes, shared_lock};
use crate::tree::{Arena, NodeId};

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
///
/// When only the interaction state differs from the last call (a hover, a
/// focus, a press), nothing is animating and the rules and viewport are the
/// same, only the nodes the change can reach are styled again: the nodes whose
/// state flags changed, what the selectors that name a state lead to from them
/// ([`StateReach`]), and below any node whose new style may change what its
/// children inherit. The styles of the rest are the last call's.
#[derive(Default)]
pub struct StyleSession {
    kept: Option<Kept>,
}

struct Kept {
    shape: u64,
    timeline: TimelineId,
    tree: StyloTree,
    /// What the last call gave and was given, while it can be built on.
    last: Option<Last>,
    /// How far a state change reaches in the rules (pointer and length).
    reach: Option<((usize, usize), StateReach)>,
}

struct Last {
    rules: (usize, usize),
    viewport: Viewport,
    settings: (bool, bool, bool),
    styles: StyleMap,
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
        self.compute_changed(arena, rules, state, viewport, timeline)
            .0
    }

    /// [`Self::compute`], and which nodes may have a style other than the one
    /// the call before gave: `None` when that is not known (anything may),
    /// otherwise the nodes that were styled again.
    pub fn compute_changed(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> (StyleMap, Option<Vec<NodeId>>) {
        thread_state::enter(ThreadState::LAYOUT);
        let styled = self.compute_in_layout_state(arena, rules, state, viewport, timeline);
        thread_state::exit(ThreadState::LAYOUT);
        styled
    }

    fn compute_in_layout_state(
        &mut self,
        arena: &Arena,
        rules: &[Rule],
        state: &InteractionState,
        viewport: Viewport,
        timeline: &mut AnimationTimeline,
    ) -> (StyleMap, Option<Vec<NodeId>>) {
        if arena.roots().is_empty() {
            self.kept = None;
            return (StyleMap::new(), None);
        }
        let shape = arena.style_shape();
        let same = self
            .kept
            .as_ref()
            .is_some_and(|kept| kept.shape == shape && kept.timeline == timeline.id());
        let rules_key = (rules.as_ptr() as usize, rules.len());
        let settings = timeline.settings();
        if same && let Some(kept) = self.kept.as_mut() {
            if timeline.is_still()
                && kept.last.as_ref().is_some_and(|last| {
                    last.rules == rules_key
                        && last.viewport == viewport
                        && last.settings == settings
                })
                && let Some(restyled) =
                    restyle_what_changed(kept, arena, rules, rules_key, state, viewport, timeline)
            {
                return restyled;
            }
            kept.tree.refresh(arena, state);
            timeline.touch(kept.tree.stable_ids());
        } else {
            self.kept = None;
            let (tree, _primary_root) = crate::stylo::StyloTree::new(arena, state, timeline);
            self.kept = Some(Kept {
                shape,
                timeline: timeline.id(),
                tree,
                last: None,
                reach: None,
            });
        }
        let Some(kept) = self.kept.as_mut() else {
            return (StyleMap::new(), None);
        };
        let styles = compute_with_tree(&kept.tree, rules, viewport, timeline, &[]);
        kept.last = timeline.is_still().then(|| Last {
            rules: rules_key,
            viewport,
            settings,
            styles: styles.clone(),
        });
        (styles, None)
    }
}

/// Styles again only what a change of state from the last call can reach, if
/// that is little enough to be worth it. `None` when a whole cascade is needed
/// (the tree is left with the states of `state`; the caller refreshes it).
fn restyle_what_changed(
    kept: &mut Kept,
    arena: &Arena,
    rules: &[Rule],
    rules_key: (usize, usize),
    state: &InteractionState,
    viewport: Viewport,
    timeline: &mut AnimationTimeline,
) -> Option<(StyleMap, Option<Vec<NodeId>>)> {
    let reach = match kept.reach {
        Some((key, reach)) if key == rules_key => reach,
        _ => {
            let lock = shared_lock();
            let reach = StateReach::of(
                rules,
                viewport.height,
                timeline.prefers_reduced_motion(),
                &lock.read(),
            );
            kept.reach = Some((rules_key, reach));
            reach
        }
    };
    if reach.everything {
        return None;
    }
    let flipped = kept.tree.update_states(arena, state);
    let last = kept.last.as_mut()?;
    if flipped.is_empty() {
        return Some((last.styles.clone(), Some(Vec::new())));
    }
    let set = reached(&kept.tree, arena, &flipped, reach)?;
    let styled = restyle_nodes(&kept.tree, arena, &set, rules, viewport, timeline);
    // Every node is still in the tree: the timeline drops what it does not
    // meet.
    timeline.touch(kept.tree.stable_ids());
    timeline.sweep();
    let mut styles = last.styles.clone();
    let mut nodes = Vec::with_capacity(styled.len());
    for (id, style) in styled {
        nodes.push(id);
        styles.insert_shared(id, style);
    }
    last.styles = styles.clone();
    Some((styles, Some(nodes)))
}

/// The slots of the nodes a change of state in `flipped` can reach, or `None`
/// when that is over half the tree, where a whole cascade is cheaper.
fn reached(
    tree: &StyloTree,
    arena: &Arena,
    flipped: &[usize],
    reach: StateReach,
) -> Option<Vec<usize>> {
    let total = tree.len();
    let mut taken = vec![false; total];
    let mut set = Vec::new();
    let mut take = |node: NodeId, set: &mut Vec<usize>| {
        let slot = tree.slot_of(node);
        if !taken[slot] {
            taken[slot] = true;
            set.push(slot);
            true
        } else {
            false
        }
    };
    for &slot in flipped {
        let node = tree.node_of(slot);
        take(node, &mut set);
        if reach.descendants {
            take_below(arena, node, &mut |below| {
                take(below, &mut set);
            });
        }
        if reach.siblings {
            let siblings = match arena.parent(node) {
                Some(parent) => arena.children(parent),
                None => arena.roots(),
            };
            let after = siblings
                .iter()
                .position(|&s| s == node)
                .map_or(0, |at| at + 1);
            for &sibling in &siblings[after..] {
                take(sibling, &mut set);
                take_below(arena, sibling, &mut |below| {
                    take(below, &mut set);
                });
            }
        }
        if set.len() * 2 > total {
            return None;
        }
    }
    Some(set)
}

/// Calls `visit` for every node below `node`.
fn take_below(arena: &Arena, node: NodeId, visit: &mut impl FnMut(NodeId)) {
    let mut stack: Vec<NodeId> = arena.children(node).to_vec();
    while let Some(below) = stack.pop() {
        visit(below);
        stack.extend_from_slice(arena.children(below));
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

    /// Style sheets that differ in how far a change of state reaches.
    const SUITES: &[(&str, &str)] = &[
        (
            "subject only",
            ".row { color: #ff0000; } .row:hover { background-color: #eeeeff; width: 30px; } \
             .row:active { height: 8px; } input:focus { width: 90px; }",
        ),
        (
            "descendants",
            ".cell { padding: 1px; } .row:hover .cell { width: 40px; color: #00ff00; } \
             .row:hover > .cell + .cell { height: 12px; }",
        ),
        (
            "siblings",
            ".row + .row { margin-top: 3px; } .row:hover + .row { margin-top: 9px; } \
             .row:hover ~ .odd { padding-left: 4px; } .row:hover + .row .cell { width: 7px; }",
        ),
        (
            "inheritance",
            ".row { color: #101010; font-size: 14px; } .row:hover { color: #0000ff; font-size: 18px; \
             --gap: 5px; } .cell { width: var(--gap, 1px); } .row:active { font-weight: 700; }",
        ),
        (
            "negation and lists",
            ".row:not(:hover) { opacity: 0.5; } :not(:hover) > .cell { padding: 2px; } \
             :is(.row:hover, .odd:active) .cell { width: 33px; } :where(.row:focus) .cell { height: 4px; }",
        ),
        (
            "inside a media query",
            "@media (min-width: 100px) { .row:hover .cell { width: 21px; } .row:hover { height: 2px; } }",
        ),
        (
            // `:has` is not applied yet (the rule is dropped); this keeps the
            // result checked for when it is.
            "has",
            ".row:has(.cell:hover) { width: 66px; } .row:hover { height: 3px; }",
        ),
        (
            "a transition",
            ".row { color: #ff0000; transition: color 1s linear; } .row:hover { color: #0000ff; } \
             .row:hover .cell { width: 40px; }",
        ),
    ];

    /// A tree of rows (some odd) of cells (some holding a span) and an input,
    /// of a shape that depends on `seed`.
    fn nested(seed: u32) -> Element {
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(11);
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state >> 8
        };
        let class = |c: &str| vec![("class".to_string(), c.to_string())];
        let rows = 5 + next() % 6;
        let mut children: Vec<Element> = (0..rows)
            .map(|i| {
                let cells = 1 + next() % 3;
                Element::node(
                    "div",
                    class(if i % 3 == 0 { "row odd" } else { "row" }),
                    (0..cells)
                        .map(|c| {
                            let inner = if next() % 2 == 0 {
                                vec![Element::node(
                                    "span",
                                    class("tag"),
                                    vec![Element::text("t")],
                                )]
                            } else {
                                vec![Element::text(format!("c{c}"))]
                            };
                            Element::node("span", class("cell"), inner)
                        })
                        .collect(),
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
    fn restyling_what_a_change_of_state_reaches_gives_what_a_whole_cascade_gives() {
        let viewport = Viewport::default();
        let mut partial_steps = 0;
        let mut whole_steps = 0;
        for (name, css) in SUITES {
            let rules = parse_stylesheet(css).unwrap();
            for seed in 0..6u32 {
                let arena = Arena::build(&nested(seed));
                let mut session = StyleSession::new();
                let mut kept_timeline = AnimationTimeline::default();
                let mut fresh_timeline = AnimationTimeline::default();
                let pool = arena.find_all(|a, id| {
                    a.classes(id)
                        .iter()
                        .any(|c| c == "row" || c == "cell" || c == "tag")
                        || a.tag(id) == "input"
                });
                let mut rng = seed.wrapping_mul(97).wrapping_add(5);
                let mut now = 0.0;
                for step in 0..30usize {
                    rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    // Time passes only sometimes, so a transition both runs and ends.
                    now += if step % 4 == 3 { 1.3 } else { 0.0 };
                    kept_timeline.advance_to(now);
                    fresh_timeline.advance_to(now);
                    let mut state = InteractionState::new();
                    if (rng >> 8) % 6 != 0 {
                        let hit = pool[((rng >> 12) as usize) % pool.len()];
                        state = state.with_hovered(hit);
                        let mut up = arena.parent(hit);
                        while let Some(parent) = up {
                            state = state.with_hovered(parent);
                            up = arena.parent(parent);
                        }
                        if (rng >> 20) % 5 == 0 {
                            state = state.with_active(hit);
                        }
                    }
                    if (rng >> 4) % 7 == 0 {
                        let target = pool[((rng >> 16) as usize) % pool.len()];
                        state = state.with_focused(target);
                    }
                    let (kept, changed) = session.compute_changed(
                        &arena,
                        &rules,
                        &state,
                        viewport,
                        &mut kept_timeline,
                    );
                    let fresh = compute(&arena, &rules, &state, viewport, &mut fresh_timeline);
                    let mut differing: Vec<usize> = fresh
                        .iter()
                        .filter(|(id, style)| kept.get(id) != Some(*style))
                        .map(|(&id, _)| id)
                        .collect();
                    differing.sort();
                    if let Some(&first) = differing.first() {
                        let (x, y) = (
                            format!("{:?}", kept.get(&first)),
                            format!("{:?}", fresh.get(&first)),
                        );
                        let at = x
                            .chars()
                            .zip(y.chars())
                            .position(|(p, q)| p != q)
                            .unwrap_or(0);
                        let from = at.saturating_sub(120);
                        eprintln!(
                            "FIRST DIFF node {first}: kept ...{} || fresh ...{}",
                            &x[from..(at + 160).min(x.len())],
                            &y[from..(at + 160).min(y.len())]
                        );
                    }
                    assert!(
                        differing.is_empty() && kept.len() == fresh.len(),
                        "{name}, seed {seed}, step {step}: nodes {:?} differ ({:?}); restyled {:?}; hovered {:?}",
                        differing,
                        differing
                            .iter()
                            .map(|&id| (
                                arena.tag(id),
                                arena.classes(id).to_vec(),
                                arena.parent(id)
                            ))
                            .collect::<Vec<_>>(),
                        changed,
                        (0..arena.find_all(|_, _| true).len())
                            .filter(|&id| state.is_hovered(id))
                            .collect::<Vec<_>>(),
                    );
                    match changed {
                        Some(_) => partial_steps += 1,
                        None => whole_steps += 1,
                    }
                }
            }
        }
        assert!(
            partial_steps > 100,
            "most steps of the narrow stylesheets restyle part of the tree: {partial_steps}"
        );
        assert!(whole_steps > 0);
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
