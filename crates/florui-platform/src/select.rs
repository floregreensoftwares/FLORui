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
    pub value: String,
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

    let multiple = attr_bool(&node.attrs, "multiple");
    let is_open = attr_bool(&node.attrs, "open");
    let mut options = std::mem::take(&mut node.children);
    let label = (!multiple).then(|| selected_label(&options)).flatten();
    let id = attr(&node.attrs, "id").unwrap_or_default();
    summaries.insert(id.clone(), option_summaries(&options));
    options.iter_mut().for_each(|child| walk(child, summaries));

    // `multiple` is always a visible, in-flow listbox -- real HTML has no
    // open/closed state for it at all, so it needs neither the collapsed
    // label text nor the Portal overlay below.
    node.children = if multiple {
        options
    } else if is_open {
        let ondismiss = node
            .handlers
            .iter()
            .find(|(name, _)| name == "dismiss")
            .map(|(_, handler)| handler.clone());
        let root_handlers = ondismiss
            .map(|handler| vec![("dismiss".to_string(), handler)])
            .unwrap_or_default();
        options.iter_mut().for_each(inject_optgroup_label);

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

/// Class on the synthesized header the module doc's `inject_optgroup_label`
/// gives an open `<optgroup>`'s own `label` attribute — real HTML has no
/// content-generation mechanism here to reach for, and `<optgroup>`'s own
/// only real children are `<option>`s, so its `label` needs a real
/// sibling element to actually show.
pub const SELECT_OPTGROUP_LABEL_CLASS: &str = "florui-optgroup-label";

/// Gives an `<optgroup>` a real, visible header for its own `label`
/// attribute — one non-`<option>`, non-interactive child, prepended
/// ahead of its real options. A no-op for anything else, so callers can
/// map this over every top-level child unconditionally.
fn inject_optgroup_label(element: &mut Element) {
    let Element::Node(node) = element else {
        return;
    };
    if node.tag != "optgroup" {
        return;
    }
    let Some(label) = attr(&node.attrs, "label") else {
        return;
    };
    let header = Element::node(
        "div",
        vec![("class".to_string(), SELECT_OPTGROUP_LABEL_CLASS.to_string())],
        vec![Element::text(label)],
    );
    node.children.insert(0, header);
}

/// Every real `<option>` among `options`, descending one level into any
/// `<optgroup>` — real HTML only ever nests `<option>` directly inside
/// one, never deeper.
fn flatten_options(options: &[Element]) -> Vec<&ElementNode> {
    let mut flat = Vec::new();
    for el in options {
        match el {
            Element::Node(n) if n.tag == "option" => flat.push(n),
            Element::Node(n) if n.tag == "optgroup" => {
                flat.extend(flatten_options(&n.children));
            }
            _ => {}
        }
    }
    flat
}

fn option_summaries(options: &[Element]) -> Vec<OptionSummary> {
    flatten_options(options)
        .into_iter()
        .map(|n| {
            let label = collect_text(&n.children);
            OptionSummary {
                // Real HTML: an `<option>` with no `value` attribute uses
                // its own text content as the value instead.
                value: attr(&n.attrs, "value").unwrap_or_else(|| label.clone()),
                label,
                selected: attr_bool(&n.attrs, "selected"),
                onclick: n
                    .handlers
                    .iter()
                    .find(|(name, _)| name == "click")
                    .map(|(_, handler)| handler.clone()),
            }
        })
        .collect()
}

/// The text of the first `<option selected="true">` among `options`
/// (descending into any `<optgroup>`), or the first `<option>` at all if
/// none is marked — real HTML's own default-to-first-option behavior
/// when nothing is explicitly selected.
fn selected_label(options: &[Element]) -> Option<String> {
    let flat = flatten_options(options);
    let explicit = flat
        .iter()
        .find(|n| attr_bool(&n.attrs, "selected"))
        .map(|n| collect_text(&n.children));
    explicit.or_else(|| flat.first().map(|n| collect_text(&n.children)))
}

/// `select`'s currently active (keyboard-highlighted) option, if it's
/// open and one of its real, live `<option>` children is marked `active`
/// — used by the accessibility bridge's `active_descendant`. `None` while
/// closed: options aren't real `Arena` nodes then (see the module doc).
pub(crate) fn active_option(arena: &Arena, select: NodeId) -> Option<NodeId> {
    options_of(arena, select)?
        .into_iter()
        .find(|&option| arena.is_active(option))
}

/// The live `<option>` nodes under `select`'s own open content root, in
/// document order, descending through any `<optgroup>` — `None` if
/// `select` is closed or its content root doesn't resolve.
pub(crate) fn options_of(arena: &Arena, select: NodeId) -> Option<Vec<NodeId>> {
    if !arena.is_open(select) {
        return None;
    }
    let select_id = arena.id_attr(select)?;
    let content_id = format!("{select_id}{SELECT_CONTENT_ID_SUFFIX}");
    let content = arena.find(|a, id| a.id_attr(id) == Some(content_id.as_str()))?;
    Some(options_within(arena, content))
}

fn options_within(arena: &Arena, container: NodeId) -> Vec<NodeId> {
    let mut options = Vec::new();
    for child in arena.children(container) {
        match arena.tag(*child) {
            "option" => options.push(*child),
            "optgroup" => options.extend(options_within(arena, *child)),
            _ => {}
        }
    }
    options
}

/// `select`'s nearest ancestor with tag `"select"`, if any — used to find
/// which multi-select an `<option>` click landed inside, since a
/// `<select multiple>`'s options are real, direct-ish `Arena` children
/// (no Portal wrapper, unlike a single-select's open overlay).
pub(crate) fn owning_select(arena: &Arena, option: NodeId) -> Option<NodeId> {
    let mut current = arena.parent(option);
    while let Some(id) = current {
        if arena.tag(id) == "select" {
            return Some(id);
        }
        current = arena.parent(id);
    }
    None
}

/// The computed new selection for a `<select multiple>` after a click on
/// `clicked_value`, given which modifier (if any) was held — real HTML's
/// own multi-select semantics: a plain click replaces the whole selection
/// with just this one; Ctrl toggles it, leaving every other option's own
/// state alone; Shift selects every option between `anchor_value` (the
/// select's last plain/Ctrl click) and this one, inclusive, falling back
/// to a plain click if there is no anchor to range from.
pub(crate) fn compute_multiselect(
    options: &[OptionSummary],
    clicked_value: &str,
    ctrl: bool,
    shift: bool,
    anchor_value: Option<&str>,
) -> Vec<String> {
    let index_of = |value: &str| options.iter().position(|o| o.value == value);
    if shift
        && let (Some(clicked), Some(anchor)) =
            (index_of(clicked_value), anchor_value.and_then(index_of))
    {
        let (start, end) = if anchor <= clicked {
            (anchor, clicked)
        } else {
            (clicked, anchor)
        };
        return options[start..=end]
            .iter()
            .map(|o| o.value.clone())
            .collect();
    }
    if ctrl {
        let mut selected: Vec<String> = options
            .iter()
            .filter(|o| o.selected)
            .map(|o| o.value.clone())
            .collect();
        match selected.iter().position(|v| v == clicked_value) {
            Some(pos) => {
                selected.remove(pos);
            }
            None => selected.push(clicked_value.to_string()),
        }
        return selected;
    }
    vec![clicked_value.to_string()]
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
    fn option_summaries_descend_into_an_optgroup() {
        let tree: Element = view! {
            <select id="size">
                <optgroup label="Small sizes">
                    <option value="s">{"Small"}</option>
                </optgroup>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (_, summaries) = normalized(tree);
        let options = &summaries["size"];
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].label, "Small");
        assert_eq!(options[1].label, "Medium");
    }

    #[test]
    fn an_open_optgroup_gets_a_visible_header_for_its_label_alongside_its_options() {
        let tree: Element = view! {
            <select id="size" open="true">
                <optgroup label="Small sizes">
                    <option value="s">{"Small"}</option>
                </optgroup>
            </select>
        };
        let (arena, _) = normalized(tree);
        let optgroup = arena.find(|a, id| a.tag(id) == "optgroup").unwrap();
        assert_eq!(arena.group_label(optgroup), Some("Small sizes"));
        let children = arena.children(optgroup);
        assert_eq!(children.len(), 2, "the header div, then the real option");
        assert_eq!(arena.text_content(children[0]), "Small sizes");
        assert_eq!(arena.tag(children[1]), "option");
    }

    #[test]
    fn options_of_finds_a_grouped_option() {
        let tree: Element = view! {
            <select id="size" open="true">
                <optgroup label="Small sizes">
                    <option value="s">{"Small"}</option>
                </optgroup>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (arena, _) = normalized(tree);
        let select = arena.find(|a, id| a.tag(id) == "select").unwrap();
        let options = options_of(&arena, select).expect("select is open");
        assert_eq!(options.len(), 2);
        assert_eq!(arena.text_content(options[0]), "Small");
        assert_eq!(arena.text_content(options[1]), "Medium");
    }

    #[test]
    fn a_multiple_select_is_always_a_plain_in_flow_listbox() {
        let tree: Element = view! {
            <select id="sizes" multiple="true">
                <option value="s">{"Small"}</option>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (arena, summaries) = normalized(tree);
        let select = arena.roots()[0];
        assert!(arena.overlay_roots().is_empty(), "no Portal for multiple");
        let children = arena.children(select);
        assert_eq!(children.len(), 2);
        assert_eq!(arena.tag(children[0]), "option");
        let options = &summaries["sizes"];
        assert_eq!(options[0].value, "s");
        assert_eq!(options[1].value, "m");
    }

    #[test]
    fn an_options_value_falls_back_to_its_own_text_when_absent() {
        let tree: Element = view! {
            <select id="sizes">
                <option>{"Medium"}</option>
            </select>
        };
        let (_, summaries) = normalized(tree);
        assert_eq!(summaries["sizes"][0].value, "Medium");
    }

    #[test]
    fn compute_multiselect_plain_click_replaces_the_whole_selection() {
        let options = three_size_options();
        let result = compute_multiselect(&options, "l", false, false, None);
        assert_eq!(result, vec!["l".to_string()]);
    }

    #[test]
    fn compute_multiselect_ctrl_click_toggles_without_touching_others() {
        let options = three_size_options(); // "m" starts selected
        let added = compute_multiselect(&options, "l", true, false, None);
        assert_eq!(added, vec!["m".to_string(), "l".to_string()]);
        let removed = compute_multiselect(&options, "m", true, false, None);
        assert_eq!(removed, Vec::<String>::new());
    }

    #[test]
    fn compute_multiselect_shift_click_selects_the_inclusive_range() {
        let options = three_size_options();
        let forward = compute_multiselect(&options, "l", false, true, Some("s"));
        assert_eq!(forward, vec!["s", "m", "l"]);
        let backward = compute_multiselect(&options, "s", false, true, Some("l"));
        assert_eq!(backward, vec!["s", "m", "l"]);
    }

    #[test]
    fn compute_multiselect_shift_click_without_an_anchor_falls_back_to_plain() {
        let options = three_size_options();
        let result = compute_multiselect(&options, "l", false, true, None);
        assert_eq!(result, vec!["l".to_string()]);
    }

    fn three_size_options() -> Vec<OptionSummary> {
        ["s", "m", "l"]
            .into_iter()
            .map(|value| OptionSummary {
                value: value.to_string(),
                label: value.to_string(),
                selected: value == "m",
                onclick: None,
            })
            .collect()
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
