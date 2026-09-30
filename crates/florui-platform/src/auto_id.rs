//! Gives every stateful control a stable `id` when the author wrote none:
//! text editing and a `<select>`'s options are keyed by it.
//!
//! The id is the element's path (each ancestor's tag and its index among
//! same-tag siblings), so unrelated insertions never shift it. `Fragment`s
//! are transparent; each `Portal` starts its own path.

use florui::{Element, ElementNode};

use crate::focus;

/// Prefix of every generated id, so one can't collide with an authored id.
pub(crate) const AUTO_ID_PREFIX: &str = "florui-auto-";

/// Fills in a missing `id` on every editable `<input>`, `<select>` and
/// `<textarea>` in `tree`.
pub(crate) fn assign_missing_ids(tree: &mut Element) {
    walk_siblings(std::slice::from_mut(tree), "", &mut 0);
}

fn walk_siblings(elements: &mut [Element], path: &str, portals: &mut usize) {
    let mut ordinals: Vec<(&'static str, usize)> = Vec::new();
    visit(elements, path, &mut ordinals, portals);
}

fn visit(
    elements: &mut [Element],
    path: &str,
    ordinals: &mut Vec<(&'static str, usize)>,
    portals: &mut usize,
) {
    for element in elements {
        match element {
            Element::Node(node) => {
                let ordinal = next_ordinal(ordinals, node.tag);
                let own_path = format!("{path}/{}:{ordinal}", node.tag);
                if needs_id(node) {
                    node.attrs.push((
                        "id".to_string(),
                        format!("{AUTO_ID_PREFIX}{}", own_path.trim_start_matches('/')),
                    ));
                }
                walk_siblings(&mut node.children, &own_path, portals);
            }
            Element::Fragment(children) => visit(children, path, ordinals, portals),
            Element::Portal(children) => {
                let index = *portals;
                *portals += 1;
                walk_siblings(children, &format!("/portal:{index}"), portals);
            }
            Element::Text(_) => {}
        }
    }
}

fn next_ordinal(ordinals: &mut Vec<(&'static str, usize)>, tag: &'static str) -> usize {
    match ordinals.iter_mut().find(|(seen, _)| *seen == tag) {
        Some((_, count)) => {
            *count += 1;
            *count
        }
        None => {
            ordinals.push((tag, 0));
            0
        }
    }
}

fn needs_id(node: &ElementNode) -> bool {
    if node.attrs.iter().any(|(name, _)| name == "id") {
        return false;
    }
    match node.tag {
        "select" | "textarea" => true,
        "input" => {
            let input_type = node
                .attrs
                .iter()
                .find(|(name, _)| name == "type")
                .map(|(_, value)| value.as_str());
            focus::is_editable_input_type(input_type)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    fn ids(tree: &Element) -> Vec<String> {
        let arena = florui_style::Arena::build(tree);
        arena
            .find_all(|a, id| a.id_attr(id).is_some())
            .into_iter()
            .filter_map(|id| arena.id_attr(id).map(str::to_string))
            .collect()
    }

    #[test]
    fn an_editable_input_and_a_select_get_a_path_id_and_authored_ids_are_kept() {
        let mut tree: Element = view! {
            <form>
                <input type="text" />
                <input type="text" id="mine" />
                <input type="checkbox" />
                <select></select>
            </form>
        };
        assign_missing_ids(&mut tree);
        assert_eq!(
            ids(&tree),
            [
                "florui-auto-form:0/input:0",
                "mine",
                "florui-auto-form:0/select:0"
            ]
        );
    }

    #[test]
    fn inserting_an_unrelated_element_does_not_shift_a_generated_id() {
        let mut before: Element = view! { <div><input type="text" /></div> };
        let mut after: Element = view! { <div><p>{"x"}</p><input type="text" /></div> };
        assign_missing_ids(&mut before);
        assign_missing_ids(&mut after);
        assert_eq!(ids(&before), ids(&after));
    }

    #[test]
    fn same_tag_siblings_get_distinct_ids() {
        let mut tree: Element = view! { <div><input type="text" /><input type="text" /></div> };
        assign_missing_ids(&mut tree);
        let found = ids(&tree);
        assert_eq!(found.len(), 2);
        assert_ne!(found[0], found[1]);
    }
}
