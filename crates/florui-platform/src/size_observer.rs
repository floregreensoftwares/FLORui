//! [`use_committed_size`]: the first real capability plugged into
//! [`florui_reactive::use_attachment`]. A component
//! subscribes to an `id`'d element's *committed* box (after real layout,
//! not the value it rendered with); [`UiRuntime::update`] notifies every
//! subscriber whose box actually changed once layout for that render is
//! known.
//!
//! Deliberately reuses `use_attachment` rather than a parallel lifecycle:
//! `setup` registers this call site's callback in the [`SizeObserverRegistry`]
//! every render provides through context, and its `Cleanup` removes it —
//! ordinary attachment unmount handles a removed or discarded observer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use florui_layout::BoxLayout;
use florui_reactive::{Cleanup, use_attachment, use_context};
use florui_style::{Arena, NodeId};

struct Observer {
    last_notified: Option<(f32, f32)>,
    on_resize: Box<dyn FnMut(f32, f32)>,
}

/// Shared per-[`UiRuntime`] registry of active size observers, provided
/// fresh through context every render the same way the executor is.
#[derive(Default)]
pub struct SizeObserverRegistry {
    observers: RefCell<HashMap<String, Observer>>,
}

impl SizeObserverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn set(&self, id: String, on_resize: Box<dyn FnMut(f32, f32)>) {
        self.observers.borrow_mut().insert(
            id,
            Observer {
                last_notified: None,
                on_resize,
            },
        );
    }

    fn remove(&self, id: &str) {
        self.observers.borrow_mut().remove(id);
    }

    /// Calls every observer whose element's committed box changed since
    /// its last notification (or has never been notified). Called once
    /// per [`UiRuntime::update`], after layout for that render exists —
    /// never from inside layout itself, so a callback's own `Signal::set`
    /// only marks the runtime dirty for a later update, it can't recurse
    /// into this one.
    pub(crate) fn notify(&self, arena: &Arena, layouts: &HashMap<NodeId, BoxLayout>) {
        let ids: Vec<String> = self.observers.borrow().keys().cloned().collect();
        for id in ids {
            let Some(node) = arena.find(|a, candidate| a.id_attr(candidate) == Some(id.as_str()))
            else {
                continue;
            };
            let Some(layout) = layouts.get(&node) else {
                continue;
            };
            let size = (layout.width, layout.height);

            // Removed and reinserted around the call, rather than held
            // borrowed across it, so a callback touching this same
            // registry (setting up a *different* observer, say) can't
            // panic on a re-entrant borrow.
            let Some(mut observer) = self.observers.borrow_mut().remove(&id) else {
                continue;
            };
            if observer.last_notified != Some(size) {
                observer.last_notified = Some(size);
                (observer.on_resize)(size.0, size.1);
            }
            self.observers.borrow_mut().insert(id, observer);
        }
    }
}

/// Subscribes to the committed box of the element whose `id` attribute is
/// `id`: `on_resize(width, height)` runs once layout settles on a size
/// this observer hasn't already reported, including once for whatever
/// size the very first layout produces. Requires a [`SizeObserverRegistry`]
/// in context, which [`UiRuntime`](crate::UiRuntime) provides every
/// render — calling this outside one is a programming error, not a
/// recoverable condition.
///
/// # Panics
///
/// Panics if no [`SizeObserverRegistry`] is in context.
pub fn use_committed_size(id: impl Into<String>, on_resize: impl FnMut(f32, f32) + 'static) {
    let id = id.into();
    let registry = use_context::<Rc<SizeObserverRegistry>>().expect(
        "use_committed_size needs a SizeObserverRegistry in context — only a UiRuntime-hosted \
         render provides one",
    );
    let setup_id = id.clone();
    use_attachment(registry, id, move |registry| {
        registry.set(setup_id.clone(), Box::new(on_resize));
        let registry = Rc::clone(registry);
        let cleanup_id = setup_id;
        Some(Box::new(move || registry.remove(&cleanup_id)) as Cleanup)
    });
}
