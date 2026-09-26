//! Which nodes participate in keyboard focus traversal, and in what order.
//!
//! v1 is deliberately narrow: `<button>`, an editable `<input>`, and
//! `<input type="checkbox">`, not disabled, document order, no
//! `tabindex`. `<input type="radio">` stays out for now — real radio
//! behavior needs group-exclusive selection by `name`, not just a focus
//! stop, and that's separate, later work. Extending this further
//! (`a`/`select`/`textarea`) later is one more clause here, not a
//! redesign.

use florui_style::{Arena, NodeId};

/// `<input>` types this slice gives real text-editing behavior — the
/// same gate [`is_focusable`] and [`crate::text_input::TextInputRegistry`]
/// both check.
pub(crate) fn is_editable_input_type(input_type: Option<&str>) -> bool {
    matches!(input_type, Some("text") | Some("password"))
}

/// Whether `input_type` is a checkbox — the same gate [`is_focusable`]
/// and the accessibility bridge's own checkbox arm both check.
pub(crate) fn is_checkbox_input_type(input_type: Option<&str>) -> bool {
    input_type == Some("checkbox")
}

pub(crate) fn is_focusable(arena: &Arena, id: NodeId) -> bool {
    if arena.is_disabled(id) {
        return false;
    }
    match arena.tag(id) {
        "button" => true,
        "input" => {
            is_editable_input_type(arena.input_type(id))
                || is_checkbox_input_type(arena.input_type(id))
        }
        _ => false,
    }
}

/// Every focusable node, in document order — both the tab-traversal
/// sequence and the candidate list [`florui_style::FocusPath::resolve`]
/// needs.
pub(crate) fn focus_order(arena: &Arena) -> Vec<NodeId> {
    arena.find_all(is_focusable)
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
