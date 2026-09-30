//! [`use_committed_position`]: same [`florui_reactive::use_attachment`]
//! shape as [`crate::use_committed_size`], reporting
//! [`crate::UiRuntime`]'s drawn position (scroll offsets applied) instead of box size. Kept as its
//! own registry rather than folded into `SizeObserverRegistry` since a
//! caller wanting only one shouldn't pay for computing the other.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use florui_layout::BoxLayout;
use florui_reactive::{Cleanup, use_attachment, use_context};
use florui_style::{Arena, NodeId};

struct Observer {
    last_notified: Option<(f32, f32)>,
    on_move: Box<dyn FnMut(f32, f32)>,
}

/// Shared per-[`crate::UiRuntime`] registry of active position observers,
/// provided fresh through context every render the same way
/// [`crate::SizeObserverRegistry`] is.
#[derive(Default)]
pub struct PositionObserverRegistry {
    observers: RefCell<HashMap<String, Observer>>,
}

impl PositionObserverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn set(&self, id: String, on_move: Box<dyn FnMut(f32, f32)>) {
        self.observers.borrow_mut().insert(
            id,
            Observer {
                last_notified: None,
                on_move,
            },
        );
    }

    fn remove(&self, id: &str) {
        self.observers.borrow_mut().remove(id);
    }

    /// Calls every observer whose position changed since last notified —
    /// same remove/call/reinsert shape as
    /// [`crate::SizeObserverRegistry::notify`].
    pub(crate) fn notify(
        &self,
        arena: &Arena,
        layouts: &HashMap<NodeId, BoxLayout>,
        position_of: &dyn Fn(NodeId) -> (f32, f32),
    ) {
        let ids: Vec<String> = self.observers.borrow().keys().cloned().collect();
        for id in ids {
            let Some(node) = arena.find(|a, candidate| a.id_attr(candidate) == Some(id.as_str()))
            else {
                continue;
            };
            if !layouts.contains_key(&node) {
                continue;
            }
            let position = position_of(node);

            let Some(mut observer) = self.observers.borrow_mut().remove(&id) else {
                continue;
            };
            if observer.last_notified != Some(position) {
                observer.last_notified = Some(position);
                (observer.on_move)(position.0, position.1);
            }
            self.observers.borrow_mut().insert(id, observer);
        }
    }
}

/// Subscribes to the drawn position (scroll offsets applied) of the element whose `id`
/// attribute is `id`: `on_move(x, y)` runs once layout settles on a
/// position this observer hasn't already reported, including once for
/// wherever the very first layout places it. Requires a
/// [`PositionObserverRegistry`] in context, which
/// [`crate::UiRuntime`](crate::UiRuntime) provides every render — calling
/// this outside one is a programming error, not a recoverable condition.
///
/// # Panics
///
/// Panics if no [`PositionObserverRegistry`] is in context.
pub fn use_committed_position(id: impl Into<String>, on_move: impl FnMut(f32, f32) + 'static) {
    let id = id.into();
    let registry = use_context::<Rc<PositionObserverRegistry>>().expect(
        "use_committed_position needs a PositionObserverRegistry in context — only a \
         UiRuntime-hosted render provides one",
    );
    let setup_id = id.clone();
    use_attachment(registry, id, move |registry| {
        registry.set(setup_id.clone(), Box::new(on_move));
        let registry = Rc::clone(registry);
        let cleanup_id = setup_id;
        Some(Box::new(move || registry.remove(&cleanup_id)) as Cleanup)
    });
}
