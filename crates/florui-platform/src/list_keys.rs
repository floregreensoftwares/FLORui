//! Keyboard movement between the rows of a virtualized list. A list registers
//! a handler under its scroll container's `id`; the desktop host offers a key
//! to the nearest registered list around the focused element. Same shape as
//! [`crate::FocusObserverRegistry`]: provided through context each render.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use florui_style::{Arena, NodeId};

/// A key that moves focus between rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListKey {
    Previous,
    Next,
    PageUp,
    PageDown,
    First,
    Last,
}

type Handler = Rc<dyn Fn(ListKey) -> bool>;

/// Registered list handlers, keyed by the list's container `id`.
#[derive(Default)]
pub struct ListKeyRegistry {
    handlers: RefCell<HashMap<String, Handler>>,
}

impl ListKeyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(&self, id: String, handler: Handler) {
        self.handlers.borrow_mut().insert(id, handler);
    }

    pub(crate) fn remove(&self, id: &str) {
        self.handlers.borrow_mut().remove(id);
    }

    /// Offers `key` to the nearest registered list at or above `focused`;
    /// `true` when that list consumed it.
    pub(crate) fn handle(&self, arena: &Arena, focused: NodeId, key: ListKey) -> bool {
        let mut node = Some(focused);
        while let Some(current) = node {
            let handler = arena
                .id_attr(current)
                .and_then(|id| self.handlers.borrow().get(id).cloned());
            if let Some(handler) = handler {
                return handler(key);
            }
            node = arena.parent(current);
        }
        false
    }
}
