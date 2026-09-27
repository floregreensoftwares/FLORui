//! Gives raw `<select>`/`<option>` tags real open/closed behavior.
//!
//! `Arena::build` extracts any `Element::Portal` it finds into its own
//! overlay root, but a `<select>` parsed from markup has no component body
//! to author a `<Portal>` from — [`normalize`] synthesizes one when open,
//! so `Arena::build` needs no special case for `select` at all. When
//! closed, a select's children collapse to its selected option's label
//! text, reusing ordinary leaf-text rendering instead of new paint code.
//!
//! The synthesized root shares [`POPOVER_ROOT_CLASS`], so click-outside/
//! Escape dismissal is `components::popover`'s existing logic, unchanged.
//!
//! [`normalize`] also returns every select's full option list — label,
//! `selected`, and its `onclick` — regardless of open/closed: real HTML
//! lets a *closed* select's Up/Down change the value directly (see
//! `UiRuntime::step_closed_select`), which needs to know about every
//! option even though only the selected one is ever in the built `Arena`
//! while closed.

use std::collections::HashMap;

use florui::{Element, ElementNode, Handler};
use florui_style::{Arena, NodeId};

use crate::components::popover::POPOVER_ROOT_CLASS;

/// Suffix for the synthesized overlay root's `id`, matching `Popover`'s
/// own convention so `popover::trigger_for` resolves it unmodified.
pub(crate) const SELECT_ROOT_ID_SUFFIX: &str = "-select-root";

/// The overlay root's one child — see `UiRuntime::position_open_selects`.
pub(crate) const SELECT_CONTENT_ID_SUFFIX: &str = "-select-content";

/// Class on the content div, for an app to style the dropdown panel
/// itself (it has no author-supplied class of its own to hang one on).
pub const SELECT_OPTIONS_CLASS: &str = "florui-select-options";

/// One `<option>`, captured at normalize time regardless of whether its
/// select is open — see the module doc.
#[derive(Clone)]
pub(crate) struct OptionSummary {
    pub label: String,
    pub selected: bool,
    pub onclick: Option<Handler>,
}

/// Every select's options, keyed by the select's own `id`. Rewrites every
/// `<select>` in `tree` in place, however deeply nested.
pub(crate) fn normalize(tree: &mut Element) -> HashMap<String, Vec<OptionSummary>> {
    let mut summaries = HashMap::new();
    walk(tree, &mut summaries);
    summaries
}

fn walk(tree: &mut Element, summaries: &mut HashMap<String, Vec<OptionSummary>>) {
    match tree {
        Element::Node(node) => normalize_node(node, summaries),
        Element::Fragment(children) | Element::Portal(children) => {
            children.iter_mut().for_each(|child| walk(child, summaries))
        }
        Element::Text(_) => {}
    }
}

fn normalize_node(node: &mut ElementNode, summaries: &mut HashMap<String, Vec<OptionSummary>>) {
    if node.tag != "select" {
        node.children
            .iter_mut()
            .for_each(|child| walk(child, summaries));
        return;
    }

    let is_open = attr_bool(&node.attrs, "open");
    let mut options = std::mem::take(&mut node.children);
    let label = selected_label(&options);
    let id = attr(&node.attrs, "id").unwrap_or_default();
    summaries.insert(id.clone(), option_summaries(&options));
    options.iter_mut().for_each(|child| walk(child, summaries));

    node.children = if is_open {
        let ondismiss = node
            .handlers
            .iter()
            .find(|(name, _)| name == "dismiss")
            .map(|(_, handler)| handler.clone());
        let root_handlers = ondismiss
            .map(|handler| vec![("dismiss".to_string(), handler)])
            .unwrap_or_default();

        let content = Element::node(
            "div",
            vec![
                ("id".to_string(), format!("{id}{SELECT_CONTENT_ID_SUFFIX}")),
                ("class".to_string(), SELECT_OPTIONS_CLASS.to_string()),
                (
                    "style".to_string(),
                    "position: absolute; top: 0px; left: 0px;".to_string(),
                ),
            ],
            options,
        );
        let root = Element::node_with_handlers(
            "div",
            vec![
                ("id".to_string(), format!("{id}{SELECT_ROOT_ID_SUFFIX}")),
                ("class".to_string(), POPOVER_ROOT_CLASS.to_string()),
            ],
            root_handlers,
            vec![content],
        );

        let mut children = Vec::new();
        if let Some(label) = label {
            children.push(Element::text(label));
        }
        children.push(Element::Portal(vec![root]));
        children
    } else {
        label.map(Element::text).into_iter().collect()
    };
}

fn option_summaries(options: &[Element]) -> Vec<OptionSummary> {
    options
        .iter()
        .filter_map(|el| match el {
            Element::Node(n) if n.tag == "option" => Some(OptionSummary {
                label: collect_text(&n.children),
                selected: attr_bool(&n.attrs, "selected"),
                onclick: n
                    .handlers
                    .iter()
                    .find(|(name, _)| name == "click")
                    .map(|(_, handler)| handler.clone()),
            }),
            _ => None,
        })
        .collect()
}

/// The text of the first `<option selected="true">` among `options`, or
/// the first `<option>` at all if none is marked — real HTML's own
/// default-to-first-option behavior when nothing is explicitly selected.
fn selected_label(options: &[Element]) -> Option<String> {
    let explicit = options.iter().find_map(|el| match el {
        Element::Node(n) if n.tag == "option" && attr_bool(&n.attrs, "selected") => {
            Some(collect_text(&n.children))
        }
        _ => None,
    });
    explicit.or_else(|| {
        options.iter().find_map(|el| match el {
            Element::Node(n) if n.tag == "option" => Some(collect_text(&n.children)),
            _ => None,
        })
    })
}

/// `select`'s currently active (keyboard-highlighted) option, if it's
/// open and one of its real, live `<option>` children is marked `active`
/// — used by the accessibility bridge's `active_descendant`. `None` while
/// closed: options aren't real `Arena` nodes then (see the module doc).
pub(crate) fn active_option(arena: &Arena, select: NodeId) -> Option<NodeId> {
    if !arena.is_open(select) {
        return None;
    }
    let select_id = arena.id_attr(select)?;
    let content_id = format!("{select_id}{SELECT_CONTENT_ID_SUFFIX}");
    let content = arena.find(|a, id| a.id_attr(id) == Some(content_id.as_str()))?;
    arena
        .children(content)
        .iter()
        .copied()
        .find(|&option| arena.is_active(option))
}

fn collect_text(children: &[Element]) -> String {
    let mut text = String::new();
    for child in children {
        append_text(child, &mut text);
    }
    text
}

fn append_text(element: &Element, out: &mut String) {
    match element {
        Element::Text(value) => out.push_str(value),
        Element::Fragment(children) => {
            for child in children {
                append_text(child, out);
            }
        }
        Element::Node(_) | Element::Portal(_) => {}
    }
}

fn attr(attrs: &[(String, String)], name: &str) -> Option<String> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn attr_bool(attrs: &[(String, String)], name: &str) -> bool {
    attr(attrs, name).as_deref() == Some("true")
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_style::Arena;

    use super::*;

    fn normalized(mut tree: Element) -> (Arena, HashMap<String, Vec<OptionSummary>>) {
        let summaries = normalize(&mut tree);
        (Arena::build(&tree), summaries)
    }

    #[test]
    fn a_closed_select_collapses_to_its_selected_options_label() {
        let tree: Element = view! {
            <select id="size">
                <option value="s">{"Small"}</option>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (arena, _) = normalized(tree);
        let select = arena.roots()[0];
        assert_eq!(arena.tag(select), "select");
        assert_eq!(arena.text_content(select), "Medium");
        assert!(arena.children(select).is_empty());
    }

    #[test]
    fn a_closed_select_with_nothing_selected_falls_back_to_the_first_option() {
        let tree: Element = view! {
            <select id="size">
                <option value="s">{"Small"}</option>
                <option value="m">{"Medium"}</option>
            </select>
        };
        let (arena, _) = normalized(tree);
        assert_eq!(arena.text_content(arena.roots()[0]), "Small");
    }

    #[test]
    fn an_open_select_gets_a_portal_overlay_root_alongside_its_own_label() {
        let tree: Element = view! {
            <select id="size" open="true">
                <option value="s">{"Small"}</option>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (arena, _) = normalized(tree);
        let select = arena.roots()[0];
        assert_eq!(arena.text_content(select), "Medium");
        assert!(arena.overlay_roots().len() == 1);
        let root = arena.overlay_roots()[0];
        assert_eq!(arena.id_attr(root), Some("size-select-root"));
        assert_eq!(arena.classes(root), &[POPOVER_ROOT_CLASS.to_string()]);
        let content = arena.children(root)[0];
        assert_eq!(arena.id_attr(content), Some("size-select-content"));
        let options = arena.find_all(|a, id| a.tag(id) == "option");
        assert_eq!(options.len(), 2);
    }

    #[test]
    fn normalize_descends_into_ordinary_ancestors_to_find_a_nested_select() {
        let tree: Element = view! {
            <div>
                <select id="size" open="true">
                    <option value="s">{"Small"}</option>
                </select>
            </div>
        };
        let (arena, _) = normalized(tree);
        assert_eq!(arena.overlay_roots().len(), 1);
    }

    #[test]
    fn option_summaries_are_collected_for_a_closed_select_too() {
        let tree: Element = view! {
            <select id="size">
                <option value="s">{"Small"}</option>
                <option value="m" selected="true">{"Medium"}</option>
                <option value="l">{"Large"}</option>
            </select>
        };
        let (_, summaries) = normalized(tree);
        let options = &summaries["size"];
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].label, "Small");
        assert!(!options[0].selected);
        assert_eq!(options[1].label, "Medium");
        assert!(options[1].selected);
    }

    #[test]
    fn active_option_is_found_only_while_open() {
        let tree: Element = view! {
            <select id="size" open="true">
                <option value="s">{"Small"}</option>
                <option value="m" active="true">{"Medium"}</option>
            </select>
        };
        let (arena, _) = normalized(tree);
        let select = arena.find(|a, id| a.tag(id) == "select").unwrap();
        let active = active_option(&arena, select).expect("an active option");
        assert!(arena.is_active(active));

        let closed: Element = view! {
            <select id="size">
                <option value="s" active="true">{"Small"}</option>
            </select>
        };
        let (arena, _) = normalized(closed);
        let select = arena.roots()[0];
        assert_eq!(active_option(&arena, select), None);
    }
}
