//! Lets a component follow and steer keyboard focus: [`use_focus_within`]
//! reports whether focus sits inside an element, and [`FocusController`]
//! asks the runtime to focus one by `id`. Same shape as
//! [`crate::use_committed_position`]: a registry the runtime provides through
//! context each render and notifies after layout.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use florui_reactive::{Cleanup, use_attachment, use_context};
use florui_style::{Arena, NodeId};

use crate::components::popover::is_self_or_descendant;

struct Observer {
    last_notified: Option<bool>,
    on_change: Box<dyn FnMut(bool)>,
}

/// Active focus observers, keyed by the `id` they watch.
#[derive(Default)]
pub struct FocusObserverRegistry {
    observers: RefCell<HashMap<String, Observer>>,
}

impl FocusObserverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn set(&self, id: String, on_change: Box<dyn FnMut(bool)>) {
        self.observers.borrow_mut().insert(
            id,
            Observer {
                last_notified: None,
                on_change,
            },
        );
    }

    fn remove(&self, id: &str) {
        self.observers.borrow_mut().remove(id);
    }

    /// Calls every observer whose element's focus-within state changed since
    /// it was last told, `focused` being the node focus is on now.
    pub(crate) fn notify(&self, arena: &Arena, focused: Option<NodeId>) {
        let ids: Vec<String> = self.observers.borrow().keys().cloned().collect();
        for id in ids {
            let Some(node) = arena.find(|a, candidate| a.id_attr(candidate) == Some(id.as_str()))
            else {
                continue;
            };
            let within = focused.is_some_and(|focused| is_self_or_descendant(arena, node, focused));
            let Some(mut observer) = self.observers.borrow_mut().remove(&id) else {
                continue;
            };
            if observer.last_notified != Some(within) {
                observer.last_notified = Some(within);
                (observer.on_change)(within);
            }
            self.observers.borrow_mut().insert(id, observer);
        }
    }
}

/// Runs `on_change(true)` when keyboard focus moves into the element whose
/// `id` attribute is `id` (or one of its descendants) and `on_change(false)`
/// when it leaves, once after the first layout and then only on a change.
///
/// # Panics
///
/// Panics outside a [`crate::UiRuntime`]-hosted render.
pub fn use_focus_within(id: impl Into<String>, on_change: impl FnMut(bool) + 'static) {
    let id = id.into();
    let registry = use_context::<Rc<FocusObserverRegistry>>().expect(
        "use_focus_within needs a FocusObserverRegistry in context — only a \
         UiRuntime-hosted render provides one",
    );
    let setup_id = id.clone();
    use_attachment(registry, id, move |registry| {
        registry.set(setup_id.clone(), Box::new(on_change));
        let registry = Rc::clone(registry);
        let cleanup_id = setup_id;
        Some(Box::new(move || registry.remove(&cleanup_id)) as Cleanup)
    });
}

/// How many renders a focus request waits for its element to appear.
const REQUEST_LIFETIME: u8 = 2;

/// Queues a request to focus the element with a given `id`.
#[derive(Default)]
pub struct FocusController {
    pending: RefCell<Option<(String, u8)>>,
}

impl FocusController {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the runtime to focus the element with this `id` on its next
    /// render, or its first focusable descendant when it isn't focusable
    /// itself. An element that has not mounted yet is waited for over the next
    /// couple of renders; one that never appears is dropped.
    pub fn request_focus(&self, id: impl Into<String>) {
        *self.pending.borrow_mut() = Some((id.into(), REQUEST_LIFETIME));
    }

    /// The queued request, if any, without consuming it.
    pub(crate) fn pending_id(&self) -> Option<String> {
        self.pending.borrow().as_ref().map(|(id, _)| id.clone())
    }

    /// Ends the request: it was satisfied.
    pub(crate) fn fulfil(&self) {
        *self.pending.borrow_mut() = None;
    }

    /// Ends one render of waiting; the request is dropped when none is left.
    pub(crate) fn wait_a_render(&self) {
        let mut pending = self.pending.borrow_mut();
        if let Some((_, renders)) = pending.as_mut() {
            *renders -= 1;
            if *renders == 0 {
                *pending = None;
            }
        }
    }
}

/// The runtime's [`FocusController`].
///
/// # Panics
///
/// Panics outside a [`crate::UiRuntime`]-hosted render.
pub fn use_focus_controller() -> Rc<FocusController> {
    use_context::<Rc<FocusController>>().expect(
        "use_focus_controller needs a FocusController in context — only a \
         UiRuntime-hosted render provides one",
    )
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use florui::prelude::*;
    use taffy::{AvailableSpace, Size};

    use super::*;
    use crate::UiRuntime;

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(400.0),
            height: AvailableSpace::Definite(300.0),
        }
    }

    fn node(runtime: &UiRuntime, id: &str) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena.find(|a, n| a.id_attr(n) == Some(id)).unwrap()
    }

    #[test]
    fn an_observer_hears_focus_enter_and_leave_its_element_once_each() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let log_in = Rc::clone(&log);
        let mut runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                let log = Rc::clone(&log_in);
                use_focus_within("box", move |within| log.borrow_mut().push(within));
                view! {
                    <div>
                        <div id="box"><button id="inside">{"In"}</button></div>
                        <button id="outside">{"Out"}</button>
                    </div>
                }
            },
            viewport(),
        );
        runtime.update(viewport());
        runtime.set_focused(Some(node(&runtime, "inside")), true);
        runtime.update(viewport());
        runtime.update(viewport());
        runtime.set_focused(Some(node(&runtime, "outside")), true);
        runtime.update(viewport());
        assert_eq!(*log.borrow(), [false, true, false]);
    }

    fn controller_runtime(
        show_late: Rc<Cell<bool>>,
    ) -> (UiRuntime, Rc<RefCell<Option<Rc<FocusController>>>>) {
        let slot = Rc::new(RefCell::new(None));
        let slot_in = Rc::clone(&slot);
        let runtime = UiRuntime::with_rules(
            Vec::new(),
            move || {
                *slot_in.borrow_mut() = Some(use_focus_controller());
                let late = if show_late.get() {
                    view! { <button id="late">{"Late"}</button> }
                } else {
                    view! { <span /> }
                };
                view! {
                    <div>
                        <button id="first">{"First"}</button>
                        <div id="group"><p>{"text"}</p><button id="in-group">{"In"}</button></div>
                        {late}
                    </div>
                }
            },
            viewport(),
        );
        (runtime, slot)
    }

    fn request(slot: &Rc<RefCell<Option<Rc<FocusController>>>>, id: &str) {
        slot.borrow().as_ref().unwrap().request_focus(id);
    }

    #[test]
    fn a_request_focuses_the_element_or_its_first_focusable_descendant() {
        let (mut runtime, slot) = controller_runtime(Rc::new(Cell::new(false)));
        runtime.update(viewport());
        request(&slot, "first");
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(node(&runtime, "first")));
        request(&slot, "group");
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(node(&runtime, "in-group")));
    }

    #[test]
    fn a_request_waits_for_an_element_that_mounts_a_render_later() {
        let show = Rc::new(Cell::new(false));
        let (mut runtime, slot) = controller_runtime(Rc::clone(&show));
        runtime.update(viewport());
        request(&slot, "late");
        runtime.update(viewport());
        assert_eq!(runtime.focused(), None, "nothing to focus yet");
        show.set(true);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), Some(node(&runtime, "late")));
    }

    #[test]
    fn a_request_for_an_element_that_never_appears_is_dropped() {
        let show = Rc::new(Cell::new(false));
        let (mut runtime, slot) = controller_runtime(Rc::clone(&show));
        runtime.update(viewport());
        request(&slot, "late");
        runtime.update(viewport());
        runtime.update(viewport());
        runtime.update(viewport());
        show.set(true);
        runtime.update(viewport());
        assert_eq!(runtime.focused(), None);
    }
}
