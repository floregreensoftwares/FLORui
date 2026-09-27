//! Which nodes participate in keyboard focus traversal, and in what order.
//!
//! v1 is deliberately narrow: `<button>`, an editable `<input>`,
//! `<input type="checkbox">` and `<input type="radio">`, not disabled,
//! document order, no `tabindex`. Radios sharing a `name` are one Tab
//! stop (see [`tab_stops`]) and move among themselves with the arrow
//! keys (see [`radio_sibling`]). Extending this further
//! (`a`/`select`/`textarea`) later is one more clause here, not a
//! redesign.

use florui_style::{Arena, NodeId};

/// `<input>` types this slice gives real text-editing behavior — the
/// same gate [`is_focusable`] and [`crate::text_input::TextInputRegistry`]
/// both check.
pub(crate) fn is_editable_input_type(input_type: Option<&str>) -> bool {
    matches!(input_type, Some("text") | Some("password"))
}

/// Whether `input_type` carries a `checked` state — the same gate
/// [`is_focusable`] and the disabled-click gates all check.
pub(crate) fn is_checkable_input_type(input_type: Option<&str>) -> bool {
    matches!(input_type, Some("checkbox") | Some("radio"))
}

/// Whether a `<label>` can hand its click to `id`: an `<input>` or a
/// `<button>`.
pub(crate) fn is_labelable(arena: &Arena, id: NodeId) -> bool {
    matches!(arena.tag(id), "input" | "button")
}

fn is_radio(arena: &Arena, id: NodeId) -> bool {
    arena.tag(id) == "input" && arena.input_type(id) == Some("radio")
}

pub(crate) fn is_focusable(arena: &Arena, id: NodeId) -> bool {
    if arena.is_disabled(id) {
        return false;
    }
    match arena.tag(id) {
        "button" => true,
        "input" => {
            is_editable_input_type(arena.input_type(id))
                || is_checkable_input_type(arena.input_type(id))
        }
        _ => false,
    }
}

/// Whether the Enter key activates `id`. As in real HTML a checkbox or
/// radio (and so a switch) is toggled by Space only; Enter activates a
/// button.
pub(crate) fn activates_on_enter(arena: &Arena, id: NodeId) -> bool {
    !(arena.tag(id) == "input" && is_checkable_input_type(arena.input_type(id)))
}

/// Every focusable node, in document order — both the tab-traversal
/// sequence and the candidate list [`florui_style::FocusPath::resolve`]
/// needs.
pub(crate) fn focus_order(arena: &Arena) -> Vec<NodeId> {
    arena.find_all(is_focusable)
}

/// The focusable radios that share `id`'s `name`, in document order. A
/// radio with no (or an empty) `name` is a group of one. Group scope is
/// the whole document, not a `<form>` — there is no form element yet.
fn radio_group(arena: &Arena, id: NodeId) -> Vec<NodeId> {
    match arena.name(id).filter(|name| !name.is_empty()) {
        Some(name) => arena.find_all(|arena, other| {
            is_radio(arena, other) && is_focusable(arena, other) && arena.name(other) == Some(name)
        }),
        None => vec![id],
    }
}

/// `candidates` narrowed to real Tab stops: a radio group contributes
/// one — the node holding focus if the group has it, else the checked
/// radio, else the first.
pub(crate) fn tab_stops(
    arena: &Arena,
    candidates: &[NodeId],
    focused: Option<NodeId>,
) -> Vec<NodeId> {
    candidates
        .iter()
        .copied()
        .filter(|&id| {
            if !is_radio(arena, id) {
                return true;
            }
            let group = radio_group(arena, id);
            let stop = focused
                .filter(|node| group.contains(node))
                .or_else(|| {
                    group
                        .iter()
                        .copied()
                        .find(|&member| arena.is_checked(member))
                })
                .or_else(|| group.first().copied());
            stop == Some(id)
        })
        .collect()
}

/// The radio one arrow-key step from `from` within its group, wrapping.
/// `None` if `from` isn't a radio or has no other enabled group member.
pub(crate) fn radio_sibling(arena: &Arena, from: NodeId, direction: isize) -> Option<NodeId> {
    if !is_radio(arena, from) {
        return None;
    }
    let group = radio_group(arena, from);
    if group.len() < 2 {
        return None;
    }
    let index = group.iter().position(|&member| member == from)?;
    let next = (index as isize + direction).rem_euclid(group.len() as isize) as usize;
    Some(group[next])
}

/// The currently open modal [`crate::components::dialog::Dialog`]'s own root, if
/// any — the first node (document order) carrying
/// [`crate::components::dialog::MODAL_ROOT_CLASS`]. Single-modal-at-a-time is this
/// slice's own deliberate scope limit; a second, nested `Dialog` would
/// silently lose to whichever is found first here, not a real stacking
/// contract yet.
pub(crate) fn modal_root(arena: &Arena) -> Option<NodeId> {
    arena.find(|arena, id| {
        arena
            .classes(id)
            .iter()
            .any(|class| class == crate::components::dialog::MODAL_ROOT_CLASS)
    })
}

/// Every focusable descendant of `root`, in document order — same shape
/// as [`florui_style::Arena::find_all`]'s own DFS, just seeded at `root`
/// instead of the arena's own roots (no existing "descendants of an
/// arbitrary node" primitive to reuse here).
pub(crate) fn focusable_within(arena: &Arena, root: NodeId) -> Vec<NodeId> {
    let mut matches = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if is_focusable(arena, id) {
            matches.push(id);
        }
        stack.extend(arena.children(id).iter().rev());
    }
    matches
}

/// The real candidate list for Tab traversal and focus resolution this
/// render — every focusable node in the whole document, unless a modal
/// [`crate::components::dialog::Dialog`] is currently open, in which case focus is
/// contained to its own descendants only (real modal focus-trap
/// behavior; a non-modal overlay must never do this — see
/// [`crate::components::dialog`]'s own doc for why only `Dialog` triggers it).
pub(crate) fn focus_candidates(arena: &Arena) -> Vec<NodeId> {
    match modal_root(arena) {
        Some(root) => focusable_within(arena, root),
        None => focus_order(arena),
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    #[test]
    fn focus_order_collects_only_buttons_in_document_order() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <span>{"Not focusable"}</span>
                <button>{"Second"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let order = focus_order(&arena);
        assert_eq!(order.len(), 2);
        assert_eq!(arena.text_content(order[0]), "First");
        assert_eq!(arena.text_content(order[1]), "Second");
    }

    #[test]
    fn a_span_is_not_focusable() {
        let tree: Element = view! { <span>{"text"}</span> };
        let arena = Arena::build(&tree);
        let span = arena.roots()[0];
        assert!(!is_focusable(&arena, span));
    }

    #[test]
    fn a_disabled_button_is_not_focusable() {
        let tree: Element = view! { <button disabled="true">{"Go"}</button> };
        let arena = Arena::build(&tree);
        assert!(!is_focusable(&arena, arena.roots()[0]));
    }

    #[test]
    fn an_editable_input_is_focusable() {
        let tree: Element = view! { <input type="text" value="hi" /> };
        let arena = Arena::build(&tree);
        assert!(is_focusable(&arena, arena.roots()[0]));
    }

    #[test]
    fn a_checkbox_input_is_focusable() {
        let tree: Element = view! { <input type="checkbox" /> };
        let arena = Arena::build(&tree);
        assert!(is_focusable(&arena, arena.roots()[0]));
    }

    #[test]
    fn enter_activates_a_button_but_not_a_checkbox_or_radio() {
        let tree: Element = view! {
            <div>
                <button>{"go"}</button>
                <input type="checkbox" />
                <input type="radio" />
                <input type="checkbox" role="switch" />
            </div>
        };
        let arena = Arena::build(&tree);
        let button = arena.find(|a, id| a.tag(id) == "button").unwrap();
        assert!(activates_on_enter(&arena, button));
        for input in arena.find_all(|a, id| a.tag(id) == "input") {
            assert!(!activates_on_enter(&arena, input));
        }
    }

    #[test]
    fn a_radio_input_is_focusable_unless_disabled() {
        let tree: Element = view! {
            <div>
                <input type="radio" name="g" />
                <input type="radio" name="g" disabled="true" />
            </div>
        };
        let arena = Arena::build(&tree);
        let radios = arena.find_all(|a, id| a.tag(id) == "input");
        assert!(is_focusable(&arena, radios[0]));
        assert!(!is_focusable(&arena, radios[1]));
    }

    fn radios(tree: &Element) -> (Arena, Vec<NodeId>) {
        let arena = Arena::build(tree);
        let ids = arena.find_all(|a, id| a.tag(id) == "input");
        (arena, ids)
    }

    #[test]
    fn a_radio_group_is_one_tab_stop_the_first_when_none_is_checked() {
        let tree: Element = view! {
            <div>
                <input type="radio" name="g" />
                <input type="radio" name="g" />
                <input type="radio" name="g" />
            </div>
        };
        let (arena, ids) = radios(&tree);
        assert_eq!(tab_stops(&arena, &focus_order(&arena), None), vec![ids[0]]);
    }

    #[test]
    fn a_radio_groups_tab_stop_is_its_checked_member() {
        let tree: Element = view! {
            <div>
                <input type="radio" name="g" />
                <input type="radio" name="g" checked="true" />
                <input type="radio" name="h" />
            </div>
        };
        let (arena, ids) = radios(&tree);
        assert_eq!(
            tab_stops(&arena, &focus_order(&arena), None),
            vec![ids[1], ids[2]]
        );
    }

    #[test]
    fn a_radio_groups_tab_stop_follows_focus_within_the_group() {
        let tree: Element = view! {
            <div>
                <input type="radio" name="g" checked="true" />
                <input type="radio" name="g" />
            </div>
        };
        let (arena, ids) = radios(&tree);
        assert_eq!(
            tab_stops(&arena, &focus_order(&arena), Some(ids[1])),
            vec![ids[1]]
        );
    }

    #[test]
    fn unnamed_radios_are_each_their_own_tab_stop() {
        let tree: Element = view! {
            <div>
                <input type="radio" />
                <input type="radio" />
            </div>
        };
        let (arena, ids) = radios(&tree);
        assert_eq!(tab_stops(&arena, &focus_order(&arena), None), ids);
    }

    #[test]
    fn radio_sibling_wraps_and_skips_disabled_and_other_groups() {
        let tree: Element = view! {
            <div>
                <input type="radio" name="g" />
                <input type="radio" name="h" />
                <input type="radio" name="g" disabled="true" />
                <input type="radio" name="g" />
            </div>
        };
        let (arena, ids) = radios(&tree);
        assert_eq!(radio_sibling(&arena, ids[0], 1), Some(ids[3]));
        assert_eq!(radio_sibling(&arena, ids[3], 1), Some(ids[0]));
        assert_eq!(radio_sibling(&arena, ids[0], -1), Some(ids[3]));
        assert_eq!(radio_sibling(&arena, ids[1], 1), None, "a group of one");
    }

    #[test]
    fn a_disabled_checkbox_is_not_focusable() {
        let tree: Element = view! { <input type="checkbox" disabled="true" /> };
        let arena = Arena::build(&tree);
        assert!(!is_focusable(&arena, arena.roots()[0]));
    }

    #[test]
    fn a_disabled_input_is_not_focusable() {
        let tree: Element = view! { <input type="text" value="hi" disabled="true" /> };
        let arena = Arena::build(&tree);
        assert!(!is_focusable(&arena, arena.roots()[0]));
    }

    #[test]
    fn focus_order_excludes_disabled_buttons() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <button disabled="true">{"Second"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        let order = focus_order(&arena);
        assert_eq!(order.len(), 1);
        assert_eq!(arena.text_content(order[0]), "First");
    }

    #[test]
    fn modal_root_finds_the_marked_div() {
        let tree: Element = view! {
            <div>
                <button>{"Trigger"}</button>
                <div class={crate::components::dialog::MODAL_ROOT_CLASS}>
                    <button>{"Inside"}</button>
                </div>
            </div>
        };
        let arena = Arena::build(&tree);
        let root = modal_root(&arena).expect("a modal root is present");
        assert_eq!(arena.tag(root), "div");
        assert_eq!(
            arena.classes(root),
            &[crate::components::dialog::MODAL_ROOT_CLASS]
        );
    }

    #[test]
    fn modal_root_is_none_without_the_marker_class() {
        let tree: Element = view! { <div><button>{"Go"}</button></div> };
        let arena = Arena::build(&tree);
        assert_eq!(modal_root(&arena), None);
    }

    #[test]
    fn focusable_within_only_collects_the_roots_own_descendants() {
        let tree: Element = view! {
            <div>
                <button>{"Outside"}</button>
                <div class={crate::components::dialog::MODAL_ROOT_CLASS}>
                    <button>{"First inside"}</button>
                    <button>{"Second inside"}</button>
                </div>
            </div>
        };
        let arena = Arena::build(&tree);
        let root = modal_root(&arena).unwrap();
        let inside = focusable_within(&arena, root);
        assert_eq!(inside.len(), 2);
        assert_eq!(arena.text_content(inside[0]), "First inside");
        assert_eq!(arena.text_content(inside[1]), "Second inside");
    }

    #[test]
    fn focus_candidates_is_the_whole_document_without_a_modal() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <button>{"Second"}</button>
            </div>
        };
        let arena = Arena::build(&tree);
        assert_eq!(focus_candidates(&arena), focus_order(&arena));
    }

    #[test]
    fn focus_candidates_is_restricted_to_the_modal_while_one_is_open() {
        let tree: Element = view! {
            <div>
                <button>{"Outside"}</button>
                <div class={crate::components::dialog::MODAL_ROOT_CLASS}>
                    <button>{"Inside"}</button>
                </div>
            </div>
        };
        let arena = Arena::build(&tree);
        let candidates = focus_candidates(&arena);
        assert_eq!(candidates.len(), 1);
        assert_eq!(arena.text_content(candidates[0]), "Inside");
    }
}
