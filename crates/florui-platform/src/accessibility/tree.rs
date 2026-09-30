//! Builds a real AccessKit tree from a florui `Arena` every render. Host
//! OS wiring (the `winit` adapter, inbound `ActionRequest` dispatch) stays
//! in `desktop.rs`; this module only turns an `Arena` into a
//! [`accesskit::TreeUpdate`], so it never needs to know about `winit`.
//!
//! Node identity: `florui_style::NodeId` is just preorder position in a
//! freshly rebuilt `Arena` (`Arena::build` reruns from scratch every
//! render) and does not survive across renders — the same problem
//! [`florui_style::FocusPath`] already solves for "the one focused
//! element". This tree interns each node's `FocusPath` into a monotonic
//! `accesskit::NodeId` — zero collision risk, unlike hashing the path.
//! `AccessKitId(0)` is reserved for a synthetic `Role::Window` root over
//! `arena.roots()` (document *and* portal-overlay roots, in order —
//! AccessKit needs exactly one tree root; `Arena` can hand back several).
//!
//! Like `ScrollRegistry`/`TextInputRegistry`, this rebuilds the whole
//! tree every call rather than diffing against the previous one —
//! AccessKit's own docs allow sending a complete tree every update, just
//! not optimally; incremental updates are a later, measured-cost
//! optimization, not this slice's concern.
//!
//! Inbound `Action::Focus`/`Action::Click`/`Action::SetValue`/
//! `Action::ReplaceSelectedText` are wired up (see `desktop.rs`'s own
//! dispatch, which routes the latter two through `TextInputRegistry`'s
//! ordinary `TextEditOp` path). Every other action stays unhandled.

use std::collections::{HashMap, HashSet};

use accesskit::{
    Action, Node, NodeId as AccessKitId, Rect, Role, Toggled, TreeId, TreeInfo, TreeUpdate,
};
use florui_style::{AccessibleRole, Arena, FocusPath, InteractionState, NodeId};

use crate::focus::is_focusable;

const ROOT_ID: AccessKitId = AccessKitId(0);

/// A node's real, on-screen, window-relative physical box — the same
/// geometry hit-testing/painting already use. Supplied by the caller
/// (`desktop.rs`'s own `physical_layouts`); this module has no scroll/DPI
/// knowledge of its own.
pub(crate) type NodeBounds = HashMap<NodeId, (f32, f32, f32, f32)>;

pub(crate) struct AccessibilityTree {
    interner: HashMap<FocusPath, u64>,
    next_id: u64,
}

impl AccessibilityTree {
    pub(crate) fn new() -> Self {
        Self {
            interner: HashMap::new(),
            next_id: 1,
        }
    }

    fn stable_id(&mut self, path: &FocusPath) -> AccessKitId {
        if let Some(&id) = self.interner.get(path) {
            return AccessKitId(id);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.interner.insert(path.clone(), id);
        AccessKitId(id)
    }

    /// Rebuilds the whole tree from `arena`. Returns the update plus a
    /// side table translating an inbound `ActionRequest.target_node`
    /// back to a real `florui_style::NodeId` for this same render (the
    /// interning above is one-way; a fresh reverse table is cheaper than
    /// keeping one live across renders when nodes come and go).
    pub(crate) fn build(
        &mut self,
        arena: &Arena,
        focused: Option<NodeId>,
        bounds: &NodeBounds,
        interaction: &InteractionState,
    ) -> (TreeUpdate, HashMap<AccessKitId, NodeId>) {
        // `for="some-id"` can point forward (a label written before its
        // control) or backward -- resolved against every `id`-attributed
        // node up front, not discovered mid-walk.
        let mut id_index: HashMap<&str, NodeId> = HashMap::new();
        for &node in &arena.find_all(|a, id| a.id_attr(id).is_some()) {
            if let Some(id_attr) = arena.id_attr(node) {
                id_index.insert(id_attr, node);
            }
        }

        let mut nodes = Vec::new();
        let mut reverse = HashMap::new();
        let mut forward = HashMap::new();
        let mut index_by_ak_id = HashMap::new();
        let mut seen = HashSet::new();
        // `(label's own id, the control's florui id)` -- the control's own
        // `AccessKitId` isn't known until the whole walk finishes (it may
        // not have been visited yet), so association is a second pass.
        let mut label_targets: Vec<(AccessKitId, NodeId)> = Vec::new();
        // `(select's own ak_id, its currently active option's florui id)` --
        // same deferred-association reason as `label_targets`.
        let mut active_targets: Vec<(AccessKitId, NodeId)> = Vec::new();

        let root_children: Vec<AccessKitId> = arena
            .roots()
            .iter()
            .map(|&child| {
                self.build_node(
                    arena,
                    child,
                    bounds,
                    interaction,
                    &id_index,
                    &mut nodes,
                    &mut reverse,
                    &mut forward,
                    &mut index_by_ak_id,
                    &mut seen,
                    &mut label_targets,
                    &mut active_targets,
                )
            })
            .collect();

        let mut root = Node::new(Role::Window);
        root.set_children(root_children);
        let root_index = nodes.len();
        nodes.push((ROOT_ID, root));
        index_by_ak_id.insert(ROOT_ID, root_index);

        for (label_ak_id, target_florui_id) in label_targets {
            if let Some(&target_ak_id) = forward.get(&target_florui_id)
                && let Some(&target_index) = index_by_ak_id.get(&target_ak_id)
            {
                nodes[target_index].1.push_labelled_by(label_ak_id);
            }
        }
        for (select_ak_id, active_florui_id) in active_targets {
            if let Some(&active_ak_id) = forward.get(&active_florui_id)
                && let Some(&select_index) = index_by_ak_id.get(&select_ak_id)
            {
                nodes[select_index].1.set_active_descendant(active_ak_id);
            }
        }

        self.interner.retain(|path, _| seen.contains(path));

        let focus = focused
            .map(|id| self.stable_id(&FocusPath::of(arena, id)))
            .unwrap_or(ROOT_ID);

        let update = TreeUpdate {
            nodes,
            tree: Some(TreeInfo::new(ROOT_ID)),
            tree_id: TreeId::ROOT,
            focus,
        };
        (update, reverse)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_node(
        &mut self,
        arena: &Arena,
        id: NodeId,
        bounds: &NodeBounds,
        interaction: &InteractionState,
        id_index: &HashMap<&str, NodeId>,
        nodes: &mut Vec<(AccessKitId, Node)>,
        reverse: &mut HashMap<AccessKitId, NodeId>,
        forward: &mut HashMap<NodeId, AccessKitId>,
        index_by_ak_id: &mut HashMap<AccessKitId, usize>,
        seen: &mut HashSet<FocusPath>,
        label_targets: &mut Vec<(AccessKitId, NodeId)>,
        active_targets: &mut Vec<(AccessKitId, NodeId)>,
    ) -> AccessKitId {
        let path = FocusPath::of(arena, id);
        seen.insert(path.clone());
        let ak_id = self.stable_id(&path);
        reverse.insert(ak_id, id);
        forward.insert(id, ak_id);

        let children = arena.children(id);
        let mut node = Node::new(Role::GenericContainer);

        match arena.tag(id) {
            "button" => {
                node.set_role(Role::Button);
                // An icon-only button's own text (a glyph like "x") is
                // useless as a spoken name -- `accessible_label` overrides
                // it when the author declared one.
                node.set_label(
                    arena
                        .accessible_label(id)
                        .unwrap_or_else(|| arena.text_content(id)),
                );
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                    node.add_action(Action::Click);
                }
            }
            "a" => {
                node.set_role(Role::Link);
                node.set_label(
                    arena
                        .accessible_label(id)
                        .unwrap_or_else(|| arena.text_content(id)),
                );
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                    node.add_action(Action::Click);
                }
            }
            // Never focusable -- `img` is deliberately absent from
            // `is_focusable`'s own match (falls to its `_ => false` arm),
            // so no Focus/Click action is ever added here, matching real
            // `<img>` (content, not a control). `alt=""` is real HTML's
            // own explicit "this is decorative" signal -- hidden from the
            // AT tree entirely (`set_hidden`, `aria-hidden`'s
            // equivalent), not just given an empty name, so it can't
            // still interrupt screen-reader navigation with a silent
            // stop. A present, non-empty `alt` becomes the accessible
            // name; an *absent* `alt` (author never declared one) is left
            // unlabeled rather than inventing one (e.g. from `src`) --
            // an honest gap, not a fabricated name.
            "img" => {
                node.set_role(Role::Image);
                match arena.alt_attr(id) {
                    Some("") => node.set_hidden(),
                    Some(alt) => node.set_label(alt),
                    None => {}
                }
            }
            // Always decorative -- no `alt`-equivalent attribute at all
            // (see `crate::icon`'s own module doc): a themable glyph like
            // a chevron or check exists to decorate a real control, which
            // already has its own accessible name; exposing the icon too
            // would duplicate it. Never focusable for the same reason
            // `img` isn't (absent from `is_focusable`'s own match).
            "icon" => {
                node.set_hidden();
            }
            "input" if matches!(arena.input_type(id), Some("checkbox") | Some("radio")) => {
                // `role="switch"` only applies to a checkbox; a radio stays
                // a radio.
                node.set_role(match (arena.input_type(id), arena.role(id)) {
                    (Some("radio"), _) => Role::RadioButton,
                    (_, Some(AccessibleRole::Switch)) => Role::Switch,
                    (_, None) => Role::CheckBox,
                });
                if let Some(label) = arena.accessible_label(id) {
                    node.set_label(label);
                }
                // A checkbox's `indeterminate` overrides `checked` for the
                // reported state -- real HTML's own IDL-property behavior
                // (`indeterminate` never applies to a radio). Measured
                // against Chrome: this holds for a plain checkbox and for
                // one with `role="switch"` alike.
                let indeterminate =
                    arena.input_type(id) == Some("checkbox") && arena.is_indeterminate(id);
                node.set_toggled(if indeterminate {
                    Toggled::Mixed
                } else {
                    arena.is_checked(id).into()
                });
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                    node.add_action(Action::Click);
                }
            }
            "input" if arena.input_type(id) == Some("range") => {
                node.set_role(Role::Slider);
                node.set_numeric_value(arena.range_value(id) as f64);
                node.set_min_numeric_value(arena.range_min(id) as f64);
                node.set_max_numeric_value(arena.range_max(id) as f64);
                node.set_numeric_value_step(arena.range_step(id) as f64);
                if let Some(label) = arena.accessible_label(id) {
                    node.set_label(label);
                }
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                    node.add_action(Action::Increment);
                    node.add_action(Action::Decrement);
                }
            }
            "input" => {
                let role = match arena.input_type(id) {
                    Some("password") => Role::PasswordInput,
                    _ => Role::TextInput,
                };
                node.set_role(role);
                node.set_value(arena.value_attr(id).unwrap_or_default());
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                    node.add_action(Action::SetValue);
                    node.add_action(Action::ReplaceSelectedText);
                }
            }
            // `multiple` is always a visible listbox, never a collapsible
            // combobox -- no `Expanded`/Expand/Collapse, and its own
            // `<option>` children are real descendants here already
            // (never inside a synthesized overlay), so `active_option`
            // (which only ever looks inside one) has nothing to find.
            "select" if arena.is_multiple(id) => {
                node.set_role(Role::ListBox);
                node.set_multiselectable();
                if let Some(label) = arena.accessible_label(id) {
                    node.set_label(label);
                }
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                }
            }
            "select" => {
                node.set_role(Role::ComboBox);
                node.set_value(arena.text_content(id));
                node.set_expanded(arena.is_open(id));
                if let Some(label) = arena.accessible_label(id) {
                    node.set_label(label);
                }
                if is_focusable(arena, id) {
                    node.add_action(Action::Focus);
                    node.add_action(Action::Click);
                    if arena.is_open(id) {
                        node.add_action(Action::Collapse);
                    } else {
                        node.add_action(Action::Expand);
                    }
                }
                if let Some(active) = crate::select::active_option(arena, id) {
                    active_targets.push((ak_id, active));
                }
            }
            "option" => {
                node.set_role(Role::ListBoxOption);
                node.set_label(arena.text_content(id));
                node.set_selected(arena.is_selected(id));
                node.add_action(Action::Click);
            }
            "optgroup" => {
                node.set_role(Role::Group);
                if let Some(label) = arena.group_label(id) {
                    node.set_label(label);
                }
            }
            // The synthesized root div wrapping an open select's own
            // options -- see `crate::select::SELECT_CONTENT_ID_SUFFIX`.
            "div"
                if arena.id_attr(id).is_some_and(|value| {
                    value.ends_with(crate::select::SELECT_CONTENT_ID_SUFFIX)
                }) =>
            {
                node.set_role(Role::ListBox);
            }
            "label" => {
                let text = arena.text_content(id);
                if !text.is_empty() {
                    node.set_role(Role::Label);
                    node.set_value(text);
                }
                if let Some(target) = arena.label_for(id).and_then(|for_id| id_index.get(for_id)) {
                    label_targets.push((ak_id, *target));
                }
            }
            // A form is a landmark only when it has a name, as in a
            // browser.
            "form" if arena.accessible_label(id).is_some() => {
                node.set_role(Role::Form);
                if let Some(label) = arena.accessible_label(id) {
                    node.set_label(label);
                }
            }
            "fieldset" => {
                node.set_role(Role::Group);
                if let Some(&legend) = children.iter().find(|&&child| arena.tag(child) == "legend")
                {
                    node.set_label(arena.text_content(legend));
                }
            }
            _ if children.is_empty() => {
                let text = arena.text_content(id);
                if !text.is_empty() {
                    node.set_role(Role::Label);
                    node.set_value(text);
                }
            }
            _ => {}
        }

        if arena.is_disabled(id) {
            node.set_disabled();
        }
        if let Some(state) = interaction.form_state(id) {
            if state.required && arena.tag(id) != "form" {
                node.set_required();
            }
            if state.user_invalid {
                node.set_invalid(accesskit::Invalid::True);
            }
        }

        if let Some(&(x, y, width, height)) = bounds.get(&id) {
            node.set_bounds(Rect {
                x0: x as f64,
                y0: y as f64,
                x1: (x + width) as f64,
                y1: (y + height) as f64,
            });
        }

        let child_ids: Vec<AccessKitId> = children
            .iter()
            .copied()
            .map(|child| {
                self.build_node(
                    arena,
                    child,
                    bounds,
                    interaction,
                    id_index,
                    nodes,
                    reverse,
                    forward,
                    index_by_ak_id,
                    seen,
                    label_targets,
                    active_targets,
                )
            })
            .collect();
        node.set_children(child_ids);

        index_by_ak_id.insert(ak_id, nodes.len());
        nodes.push((ak_id, node));
        ak_id
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    fn build(
        tree: &Element,
        focused: Option<NodeId>,
    ) -> (TreeUpdate, HashMap<AccessKitId, NodeId>, Arena) {
        let arena = Arena::build(tree);
        let mut ak_tree = AccessibilityTree::new();
        let bounds = NodeBounds::new();
        let (update, reverse) = ak_tree.build(&arena, focused, &bounds, &InteractionState::new());
        (update, reverse, arena)
    }

    fn build_select(mut tree: Element) -> (TreeUpdate, HashMap<AccessKitId, NodeId>, Arena) {
        crate::select::normalize(&mut tree);
        build(&tree, None)
    }

    fn role_of(update: &TreeUpdate, id: AccessKitId) -> Role {
        update
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| node.role())
            .expect("node must be present in the update")
    }

    #[test]
    fn a_label_with_a_for_attribute_labels_its_targets_accesskit_node() {
        let tree: Element = view! {
            <div>
                <label for="name-input">{"Name"}</label>
                <input id="name-input" type="text" value="hi" />
            </div>
        };
        let (update, reverse, arena) = build(&tree, None);
        let label = arena.find_all(|a, id| a.tag(id) == "label")[0];
        let input = arena.find_all(|a, id| a.tag(id) == "input")[0];
        let label_ak_id = *reverse.iter().find(|&(_, &n)| n == label).unwrap().0;
        let input_ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let input_node = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == input_ak_id)
            .unwrap()
            .1;
        assert_eq!(input_node.labelled_by(), &[label_ak_id]);
    }

    #[test]
    fn a_label_with_no_matching_for_target_associates_nothing() {
        let tree: Element = view! { <label for="missing">{"Name"}</label> };
        let (update, reverse, arena) = build(&tree, None);
        let label = arena.roots()[0];
        let label_ak_id = *reverse.iter().find(|&(_, &n)| n == label).unwrap().0;
        let label_node = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == label_ak_id)
            .unwrap()
            .1;
        assert!(label_node.labelled_by().is_empty());
    }

    #[test]
    fn a_button_gets_the_button_role_and_its_text_as_label() {
        let tree: Element = view! { <button>{"Go"}</button> };
        let (update, reverse, arena) = build(&tree, None);
        let button = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == button).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Button);
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert_eq!(node.1.label(), Some("Go"));
    }

    #[test]
    fn an_accessible_label_overrides_an_icon_only_buttons_own_glyph_text() {
        let tree: Element = view! { <button accessible_label="Close">{"x"}</button> };
        let (update, reverse, arena) = build(&tree, None);
        let button = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == button).unwrap().0;
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert_eq!(node.1.label(), Some("Close"));
    }

    #[test]
    fn an_img_with_alt_gets_the_image_role_and_alt_as_its_label() {
        let tree: Element = view! { <img src="photo.png" alt="A red bicycle" /> };
        let (update, reverse, arena) = build(&tree, None);
        let img = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == img).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Image);
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert_eq!(node.1.label(), Some("A red bicycle"));
        assert!(!node.1.is_hidden());
    }

    #[test]
    fn an_img_with_an_empty_alt_is_hidden_from_the_accessibility_tree() {
        let tree: Element = view! { <img src="decoration.png" alt="" /> };
        let (update, reverse, arena) = build(&tree, None);
        let img = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == img).unwrap().0;
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert!(
            node.1.is_hidden(),
            "alt=\"\" is real HTML's own decorative marker"
        );
        assert_eq!(
            node.1.label(),
            None,
            "no fabricated name for a decorative image"
        );
    }

    #[test]
    fn an_img_with_no_alt_attribute_at_all_gets_no_fabricated_label() {
        let tree: Element = view! { <img src="photo.png" /> };
        let (update, reverse, arena) = build(&tree, None);
        let img = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == img).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Image);
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert_eq!(node.1.label(), None);
        assert!(
            !node.1.is_hidden(),
            "missing alt is an honest gap, not decorative"
        );
    }

    #[test]
    fn an_img_is_never_focusable_even_with_alt_text() {
        let tree: Element = view! { <img src="photo.png" alt="A red bicycle" /> };
        let (update, reverse, arena) = build(&tree, None);
        let img = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == img).unwrap().0;
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert!(!node.1.supports_action(Action::Focus));
        assert!(!node.1.supports_action(Action::Click));
    }

    #[test]
    fn a_text_input_gets_the_text_input_role_and_its_value() {
        let tree: Element = view! { <input type="text" value="hi" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::TextInput);
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert_eq!(node.1.value(), Some("hi"));
    }

    #[test]
    fn a_password_input_gets_the_password_input_role() {
        let tree: Element = view! { <input type="password" value="secret" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::PasswordInput);
    }

    #[test]
    fn a_checked_checkbox_gets_the_checkbox_role_and_is_toggled() {
        let tree: Element = view! { <input type="checkbox" checked="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::CheckBox);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::True));
    }

    #[test]
    fn an_open_select_gets_the_combo_box_role_its_value_and_expanded_state() {
        let tree: Element = view! {
            <select id="size" open="true">
                <option value="s">{"Small"}</option>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (update, reverse, arena) = build_select(tree);
        let select = arena.find(|a, id| a.tag(id) == "select").unwrap();
        let ak_id = *reverse.iter().find(|&(_, &n)| n == select).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::ComboBox);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.value(), Some("Medium"));
        assert_eq!(node.is_expanded(), Some(true));
    }

    #[test]
    fn an_open_selects_option_list_gets_list_box_and_list_box_option_roles() {
        let tree: Element = view! {
            <select id="size" open="true">
                <option value="s">{"Small"}</option>
                <option value="m" selected="true">{"Medium"}</option>
            </select>
        };
        let (update, reverse, arena) = build_select(tree);
        let listbox = arena
            .find(|a, id| a.tag(id) == "div" && a.id_attr(id) == Some("size-select-content"))
            .unwrap();
        let ak_id = *reverse.iter().find(|&(_, &n)| n == listbox).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::ListBox);

        let medium = arena
            .find(|a, id| a.tag(id) == "option" && a.is_selected(id))
            .unwrap();
        let ak_id = *reverse.iter().find(|&(_, &n)| n == medium).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::ListBoxOption);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.is_selected(), Some(true));
    }

    #[test]
    fn a_named_form_is_a_landmark_and_an_unnamed_one_is_not() {
        let tree: Element = view! {
            <div>
                <form id="named" accessible_label="Sign up"></form>
                <form id="plain"></form>
            </div>
        };
        let (update, reverse, arena) = build(&tree, None);
        let role = |dom_id: &str| {
            let node = arena.find(|a, id| a.id_attr(id) == Some(dom_id)).unwrap();
            let ak_id = *reverse.iter().find(|&(_, &n)| n == node).unwrap().0;
            role_of(&update, ak_id)
        };
        assert_eq!(role("named"), Role::Form);
        assert_ne!(role("plain"), Role::Form);
    }

    #[test]
    fn a_fieldset_is_a_group_named_by_its_legend() {
        let tree: Element = view! {
            <fieldset id="fs"><legend>{"Contact"}</legend></fieldset>
        };
        let (update, reverse, arena) = build(&tree, None);
        let fieldset = arena.find(|a, id| a.tag(id) == "fieldset").unwrap();
        let ak_id = *reverse.iter().find(|&(_, &n)| n == fieldset).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Group);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.label(), Some("Contact"));
    }

    #[test]
    fn a_required_control_is_marked_required_and_a_user_invalid_one_invalid() {
        let tree: Element = view! {
            <form>
                <input id="req" type="text" required="true" />
                <input id="opt" type="text" />
            </form>
        };
        let arena = Arena::build(&tree);
        let req = arena.find(|a, id| a.id_attr(id) == Some("req")).unwrap();
        let interaction = InteractionState::new().with_form_state(
            req,
            florui_style::FormState {
                required: true,
                invalid: true,
                user_invalid: true,
                ..Default::default()
            },
        );
        let mut ak_tree = AccessibilityTree::new();
        let (update, reverse) = ak_tree.build(&arena, None, &NodeBounds::new(), &interaction);
        let flags = |dom_id: &str| {
            let node = arena.find(|a, id| a.id_attr(id) == Some(dom_id)).unwrap();
            let ak_id = *reverse.iter().find(|&(_, &n)| n == node).unwrap().0;
            let n = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
            (n.is_required(), n.invalid().is_some())
        };
        assert_eq!(flags("req"), (true, true));
        assert_eq!(flags("opt"), (false, false));
    }

    #[test]
    fn a_radio_gets_the_radio_button_role_and_its_checked_state() {
        let tree: Element = view! {
            <div>
                <input type="radio" name="g" checked="true" />
                <input type="radio" name="g" />
            </div>
        };
        let (update, reverse, arena) = build(&tree, None);
        let radios = arena.find_all(|a, id| a.tag(id) == "input");
        let toggled = |input| {
            let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
            assert_eq!(role_of(&update, ak_id), Role::RadioButton);
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == ak_id)
                .unwrap()
                .1
                .toggled()
        };
        assert_eq!(toggled(radios[0]), Some(Toggled::True));
        assert_eq!(toggled(radios[1]), Some(Toggled::False));
    }

    #[test]
    fn role_switch_on_a_radio_is_ignored_and_it_stays_a_radio() {
        let tree: Element = view! { <input type="radio" role="switch" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::RadioButton);
    }

    #[test]
    fn a_role_switch_checkbox_gets_the_switch_role_state_name_and_actions() {
        let tree: Element = view! {
            <input type="checkbox" role="switch" checked="true" accessible_label="Wi-Fi" />
        };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Switch);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::True));
        assert_eq!(node.label(), Some("Wi-Fi"));
        assert!(node.supports_action(Action::Focus));
        assert!(node.supports_action(Action::Click));
    }

    #[test]
    fn a_disabled_switch_is_marked_disabled_and_gets_no_actions() {
        let tree: Element = view! { <input type="checkbox" role="switch" disabled="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.role(), Role::Switch);
        assert!(node.is_disabled());
        assert!(!node.supports_action(Action::Click));
    }

    #[test]
    fn an_unsupported_role_on_a_checkbox_falls_back_to_the_checkbox_role() {
        let tree: Element = view! { <input type="checkbox" role="slider" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::CheckBox);
    }

    #[test]
    fn an_indeterminate_checkbox_reports_mixed_regardless_of_checked() {
        let tree: Element = view! { <input type="checkbox" checked="true" indeterminate="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::Mixed));
    }

    #[test]
    fn an_indeterminate_role_switch_also_reports_mixed() {
        let tree: Element = view! { <input type="checkbox" role="switch" indeterminate="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Switch);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::Mixed));
    }

    #[test]
    fn indeterminate_on_a_radio_is_ignored() {
        let tree: Element = view! { <input type="radio" checked="true" indeterminate="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::True));
    }

    #[test]
    fn a_range_input_gets_the_slider_role_and_its_numeric_values() {
        let tree: Element = view! {
            <input type="range" min="0" max="50" step="5" value="20" accessible_label="Volume" />
        };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Slider);
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.numeric_value(), Some(20.0));
        assert_eq!(node.min_numeric_value(), Some(0.0));
        assert_eq!(node.max_numeric_value(), Some(50.0));
        assert_eq!(node.numeric_value_step(), Some(5.0));
        assert_eq!(node.label(), Some("Volume"));
        assert!(node.supports_action(Action::Focus));
        assert!(node.supports_action(Action::Increment));
        assert!(node.supports_action(Action::Decrement));
    }

    #[test]
    fn a_disabled_range_input_is_marked_disabled_and_gets_no_actions() {
        let tree: Element = view! { <input type="range" disabled="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.role(), Role::Slider);
        assert!(node.is_disabled());
        assert!(!node.supports_action(Action::Increment));
        assert!(!node.supports_action(Action::Decrement));
    }

    #[test]
    fn an_unchecked_checkbox_is_not_toggled() {
        let tree: Element = view! { <input type="checkbox" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert_eq!(node.toggled(), Some(Toggled::False));
    }

    #[test]
    fn a_disabled_checkbox_is_marked_disabled_and_gets_no_actions() {
        let tree: Element = view! { <input type="checkbox" disabled="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert!(node.is_disabled());
        assert!(!node.supports_action(Action::Focus));
        assert!(!node.supports_action(Action::Click));
    }

    #[test]
    fn a_leaf_text_node_gets_the_label_role() {
        let tree: Element = view! { <span>{"hello"}</span> };
        let (update, reverse, arena) = build(&tree, None);
        let span = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == span).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::Label);
        let node = update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap();
        assert_eq!(node.1.value(), Some("hello"));
    }

    #[test]
    fn a_div_gets_the_generic_container_role() {
        let tree: Element = view! { <div><span>{"x"}</span></div> };
        let (update, reverse, arena) = build(&tree, None);
        let div = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == div).unwrap().0;
        assert_eq!(role_of(&update, ak_id), Role::GenericContainer);
    }

    #[test]
    fn a_disabled_button_is_marked_disabled_and_gets_no_actions() {
        let tree: Element = view! { <button disabled="true">{"Go"}</button> };
        let (update, reverse, arena) = build(&tree, None);
        let button = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == button).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert!(node.is_disabled());
    }

    #[test]
    fn an_editable_input_advertises_set_value_and_replace_selected_text() {
        let tree: Element = view! { <input type="text" value="hi" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert!(node.supports_action(Action::SetValue));
        assert!(node.supports_action(Action::ReplaceSelectedText));
    }

    #[test]
    fn a_disabled_input_advertises_neither_set_value_nor_replace_selected_text() {
        let tree: Element = view! { <input type="text" value="hi" disabled="true" /> };
        let (update, reverse, arena) = build(&tree, None);
        let input = arena.roots()[0];
        let ak_id = *reverse.iter().find(|&(_, &n)| n == input).unwrap().0;
        let node = &update.nodes.iter().find(|(id, _)| *id == ak_id).unwrap().1;
        assert!(!node.supports_action(Action::SetValue));
        assert!(!node.supports_action(Action::ReplaceSelectedText));
    }

    #[test]
    fn the_root_is_a_window_containing_every_top_level_root() {
        let tree: Element = view! {
            <div class="a" />
            <div class="b" />
        };
        let (update, _, _) = build(&tree, None);
        let root = &update
            .nodes
            .iter()
            .find(|(id, _)| *id == ROOT_ID)
            .unwrap()
            .1;
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.children().len(), 2);
    }

    #[test]
    fn focus_defaults_to_the_root_when_nothing_is_focused() {
        let tree: Element = view! { <button>{"Go"}</button> };
        let (update, _, _) = build(&tree, None);
        assert_eq!(update.focus, ROOT_ID);
    }

    #[test]
    fn focus_points_at_the_focused_nodes_stable_id() {
        let tree: Element = view! { <button>{"Go"}</button> };
        let arena = Arena::build(&tree);
        let button = arena.roots()[0];
        let mut ak_tree = AccessibilityTree::new();
        let bounds = NodeBounds::new();
        let (update, reverse) =
            ak_tree.build(&arena, Some(button), &bounds, &InteractionState::new());
        let focused_id = *reverse.iter().find(|&(_, &n)| n == button).unwrap().0;
        assert_eq!(update.focus, focused_id);
    }

    #[test]
    fn the_same_structural_position_gets_the_same_id_across_a_rebuild() {
        let tree: Element = view! {
            <div>
                <button>{"First"}</button>
                <button>{"Second"}</button>
            </div>
        };
        let mut ak_tree = AccessibilityTree::new();
        let bounds = NodeBounds::new();

        let arena1 = Arena::build(&tree);
        let second1 = arena1.find_all(|a, id| a.tag(id) == "button")[1];
        let (_, reverse1) = ak_tree.build(&arena1, None, &bounds, &InteractionState::new());
        let id1 = *reverse1.iter().find(|&(_, &n)| n == second1).unwrap().0;

        let arena2 = Arena::build(&tree);
        let second2 = arena2.find_all(|a, id| a.tag(id) == "button")[1];
        let (_, reverse2) = ak_tree.build(&arena2, None, &bounds, &InteractionState::new());
        let id2 = *reverse2.iter().find(|&(_, &n)| n == second2).unwrap().0;

        assert_eq!(id1, id2);
    }

    #[test]
    fn a_removed_nodes_id_is_swept_and_reused_by_nothing_else_incorrectly() {
        let with_button: Element = view! { <button>{"Go"}</button> };
        let mut ak_tree = AccessibilityTree::new();
        let bounds = NodeBounds::new();
        ak_tree.build(
            &Arena::build(&with_button),
            None,
            &bounds,
            &InteractionState::new(),
        );
        assert_eq!(ak_tree.interner.len(), 1);

        let empty: Element = view! { <div /> };
        ak_tree.build(
            &Arena::build(&empty),
            None,
            &bounds,
            &InteractionState::new(),
        );
        assert_eq!(
            ak_tree.interner.len(),
            1,
            "the div's own path replaces the swept button entry"
        );
    }
}
