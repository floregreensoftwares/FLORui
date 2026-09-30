//! Keyboard model of a [`crate::Popover`] menu, following the WAI-ARIA menu
//! pattern (HTML has no native menu): a popover whose content holds an
//! element with `role="menu"` takes focus when it opens, Arrow keys, Home and
//! End move between its `role="menuitem"` items, Right opens a submenu and
//! Left closes one, and Escape or Tab close it with focus back on the trigger.

use florui_style::{AccessibleRole, Arena, NodeId};

use crate::components::popover::{
    POPOVER_TRIGGER_CLASS, is_self_or_descendant, popover_roots, trigger_for,
};
use crate::focus::focusable_within;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    Next,
    Previous,
    First,
    Last,
    Open,
    Close,
}

/// What a menu key does: move focus, click an item (to open its submenu), or
/// dismiss a popover root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuMove {
    Focus(NodeId),
    Activate(NodeId),
    Dismiss(NodeId),
}

fn menu_in(arena: &Arena, root: NodeId) -> Option<NodeId> {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if arena.role(id) == Some(AccessibleRole::Menu) {
            return Some(id);
        }
        stack.extend(arena.children(id).iter().rev());
    }
    None
}

/// Every open menu as `(its popover root, its menu element)`, outermost first.
pub(crate) fn open_menus(arena: &Arena) -> Vec<(NodeId, NodeId)> {
    popover_roots(arena)
        .into_iter()
        .filter_map(|root| Some((root, menu_in(arena, root)?)))
        .collect()
}

/// The focusable items of `menu`: its `menuitem`s, or every focusable
/// descendant when none is marked.
pub(crate) fn items(arena: &Arena, menu: NodeId) -> Vec<NodeId> {
    let focusable = focusable_within(arena, menu);
    let marked: Vec<NodeId> = focusable
        .iter()
        .copied()
        .filter(|&id| arena.role(id) == Some(AccessibleRole::MenuItem))
        .collect();
    if marked.is_empty() { focusable } else { marked }
}

/// The innermost open menu that holds `focused`.
fn menu_holding(arena: &Arena, focused: NodeId) -> Option<(NodeId, NodeId)> {
    open_menus(arena)
        .into_iter()
        .rev()
        .find(|&(_, menu)| is_self_or_descendant(arena, menu, focused))
}

/// Whether `focused` sits inside an open menu.
pub(crate) fn focus_in_menu(arena: &Arena, focused: NodeId) -> bool {
    menu_holding(arena, focused).is_some()
}

fn trigger_wrapper_of(arena: &Arena, node: NodeId) -> Option<NodeId> {
    let mut current = Some(node);
    while let Some(id) = current {
        if arena
            .classes(id)
            .iter()
            .any(|class| class == POPOVER_TRIGGER_CLASS)
        {
            return Some(id);
        }
        current = arena.parent(id);
    }
    None
}

/// What `key` does with focus on `focused`, or `None` when it isn't in a menu
/// or the key has nothing to do there.
pub(crate) fn menu_move(arena: &Arena, focused: NodeId, key: MenuKey) -> Option<MenuMove> {
    let (root, menu) = menu_holding(arena, focused)?;
    let items = items(arena, menu);
    let position = items.iter().position(|&item| item == focused);
    let last = items.len().checked_sub(1)?;
    match key {
        MenuKey::Next => {
            let next = position.map_or(0, |index| (index + 1) % items.len());
            Some(MenuMove::Focus(items[next]))
        }
        MenuKey::Previous => {
            let previous = position.map_or(last, |index| (index + last) % items.len());
            Some(MenuMove::Focus(items[previous]))
        }
        MenuKey::First => Some(MenuMove::Focus(items[0])),
        MenuKey::Last => Some(MenuMove::Focus(items[last])),
        MenuKey::Open => {
            let wrapper = trigger_wrapper_of(arena, focused)?;
            let submenu = open_menus(arena)
                .into_iter()
                .find(|&(sub_root, _)| trigger_for(arena, sub_root) == Some(wrapper));
            match submenu {
                Some((_, sub_menu)) => items_first(arena, sub_menu).map(MenuMove::Focus),
                None => Some(MenuMove::Activate(focused)),
            }
        }
        MenuKey::Close => {
            let trigger = trigger_for(arena, root)?;
            menu_holding(arena, trigger).map(|_| MenuMove::Dismiss(root))
        }
    }
}

fn items_first(arena: &Arena, menu: NodeId) -> Option<NodeId> {
    items(arena, menu).first().copied()
}

/// Every open menu's popover root, for Tab to close them together.
pub(crate) fn menu_roots(arena: &Arena) -> Vec<NodeId> {
    open_menus(arena)
        .into_iter()
        .map(|(root, _)| root)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use florui::prelude::*;
    use taffy::{AvailableSpace, Size};

    use super::*;
    use crate::UiRuntime;
    use crate::components::popover::{Align, Placement, Popover, PopoverProps, Side};

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(800.0),
            height: AvailableSpace::Definite(600.0),
        }
    }

    fn menu_runtime(open: Rc<Cell<bool>>, submenu_open: Rc<Cell<bool>>) -> UiRuntime {
        UiRuntime::with_rules(
            Vec::new(),
            move || {
                view! {
                    <div>
                        <button id="page-button">{"Page"}</button>
                        <Popover
                            id={"menu".to_string()}
                            open={open.get()}
                            placement={Placement::new(Side::Bottom, Align::Start)}
                            ondismiss={Handler::new(|| {})}
                            trigger={view! { <button id="trigger">{"Open"}</button> }}
                        >
                            <div role="menu" id="menu-content">
                                <button role="menuitem" id="one">{"One"}</button>
                                <button role="menuitem" id="two">{"Two"}</button>
                                <Popover
                                    id={"sub".to_string()}
                                    open={submenu_open.get()}
                                    placement={Placement::new(Side::Right, Align::Start)}
                                    ondismiss={Handler::new(|| {})}
                                    trigger={view! {
                                        <button role="menuitem" id="sub-trigger">{"More"}</button>
                                    }}
                                >
                                    <div role="menu" id="sub-content">
                                        <button role="menuitem" id="sub-one">{"Sub one"}</button>
                                        <button role="menuitem" id="sub-two">{"Sub two"}</button>
                                    </div>
                                </Popover>
                            </div>
                        </Popover>
                    </div>
                }
            },
            viewport(),
        )
    }

    fn id(runtime: &UiRuntime, name: &str) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena
            .find(|a, n| a.id_attr(n) == Some(name))
            .unwrap_or_else(|| panic!("no node with id {name}"))
    }

    fn moved(runtime: &UiRuntime, from: &str, key: MenuKey) -> Option<MenuMove> {
        let from = id(runtime, from);
        let (arena, ..) = runtime.geometry();
        menu_move(arena, from, key)
    }

    #[test]
    fn opening_a_menu_focuses_its_first_item() {
        let (open, sub) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(Rc::clone(&open), sub);
        runtime.set_focused(Some(id(&runtime, "trigger")), true);
        open.set(true);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(id(&runtime, "one")));
    }

    #[test]
    fn closing_a_menu_returns_focus_to_its_trigger() {
        let (open, sub) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(Rc::clone(&open), sub);
        runtime.set_focused(Some(id(&runtime, "trigger")), true);
        open.set(true);
        runtime.update(viewport());
        open.set(false);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(id(&runtime, "trigger")));
    }

    #[test]
    fn a_click_elsewhere_keeps_focus_where_it_landed() {
        let (open, sub) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(Rc::clone(&open), sub);
        runtime.set_focused(Some(id(&runtime, "trigger")), true);
        open.set(true);
        runtime.update(viewport());
        runtime.set_focused(Some(id(&runtime, "page-button")), false);
        open.set(false);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(id(&runtime, "page-button")));
    }

    #[test]
    fn arrows_wrap_and_home_end_jump() {
        let (open, sub) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(open, sub);
        runtime.update(viewport());
        let focus = |name: &str| Some(MenuMove::Focus(id(&runtime, name)));
        assert_eq!(moved(&runtime, "one", MenuKey::Next), focus("two"));
        assert_eq!(moved(&runtime, "sub-trigger", MenuKey::Next), focus("one"));
        assert_eq!(
            moved(&runtime, "one", MenuKey::Previous),
            focus("sub-trigger")
        );
        assert_eq!(moved(&runtime, "two", MenuKey::First), focus("one"));
        assert_eq!(moved(&runtime, "one", MenuKey::Last), focus("sub-trigger"));
    }

    #[test]
    fn right_opens_a_submenu_then_enters_it_and_left_closes_it() {
        let (open, sub) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(Rc::clone(&open), Rc::clone(&sub));
        runtime.update(viewport());
        assert_eq!(
            moved(&runtime, "sub-trigger", MenuKey::Open),
            Some(MenuMove::Activate(id(&runtime, "sub-trigger")))
        );
        assert_eq!(moved(&runtime, "one", MenuKey::Open), None, "not a submenu");

        sub.set(true);
        runtime.update(viewport());
        assert_eq!(
            moved(&runtime, "sub-trigger", MenuKey::Open),
            Some(MenuMove::Focus(id(&runtime, "sub-one")))
        );
        let sub_root = {
            let (arena, ..) = runtime.geometry();
            open_menus(arena)[1].0
        };
        assert_eq!(
            moved(&runtime, "sub-one", MenuKey::Close),
            Some(MenuMove::Dismiss(sub_root))
        );
        assert_eq!(moved(&runtime, "one", MenuKey::Close), None, "top level");
    }

    #[test]
    fn opening_a_submenu_moves_focus_in_and_closing_it_returns_to_its_trigger() {
        let (open, sub) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(open, Rc::clone(&sub));
        runtime.update(viewport());
        runtime.set_focused(Some(id(&runtime, "sub-trigger")), true);
        sub.set(true);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(id(&runtime, "sub-one")));
        sub.set(false);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(id(&runtime, "sub-trigger")));
    }

    #[test]
    fn keys_do_nothing_outside_a_menu() {
        let (open, sub) = (Rc::new(Cell::new(true)), Rc::new(Cell::new(false)));
        let mut runtime = menu_runtime(open, sub);
        runtime.update(viewport());
        assert_eq!(moved(&runtime, "page-button", MenuKey::Next), None);
        assert_eq!(moved(&runtime, "trigger", MenuKey::Next), None);
    }
}
