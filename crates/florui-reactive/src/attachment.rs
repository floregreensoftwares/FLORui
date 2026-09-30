//! [`use_attachment`]: associates reusable, typed setup/cleanup behavior
//! with whatever capability handle this call site owns, without inserting
//! a wrapper. This crate has no notion of a mounted
//! native element yet (refs and layout snapshots arrive in a later
//! stage), so `handle` is whatever typed capability the caller already
//! has; the first real capability (a size observer) plugs into this same
//! lifecycle once that exists, rather than needing a new one.
//!
//! Not addressed here (tracked gaps, not oversights): coalescing
//! duplicate geometry notifications, diagnosing observer feedback loops,
//! and arbitrating exclusive capabilities like pointer capture all need a
//! real capability to arbitrate over — none exists yet. A setup that
//! kicks off async work and later tries to touch a disposed capability
//! isn't guarded against generically here; a real capability's own handle
//! should carry that guard when one exists.

use std::any::Any;

use crate::Cleanup;
use crate::scope::{ComponentScopeInner, PendingAttachment, active_slot};

struct AttachmentSlot {
    deps: Box<dyn Any>,
    cleanup: Option<Cleanup>,
}

/// Sets up (or replaces) an attachment on `handle`, keyed to this call
/// site. Runs `setup(&handle)` after this render commits — never during
/// the render itself, and never with a hook reachable from inside it,
/// since by the time it runs no [`crate::ComponentScope::render`] pass is active
/// — the first time this call site is reached, or whenever `deps` differs
/// from the previous render's.
///
/// Its returned cleanup runs before a changed attachment's replacement
/// setup, and once when this call site's owning [`crate::ComponentScope`] is
/// dropped. Unlike [`crate::use_effect`], when several attachments share
/// one scope their setups run in declaration order and their unmount
/// cleanups run in the *reverse* of that order — matching how nested
/// setup/teardown is conventionally expected to nest, and letting a later
/// attachment safely assume an earlier one is still active during its own
/// cleanup.
///
/// # Panics
///
/// Panics outside a [`crate::ComponentScope::render`] pass, or if hooks ran in a
/// different order or count than last render.
pub fn use_attachment<H: 'static, D: PartialEq + 'static>(
    handle: H,
    deps: D,
    setup: impl FnOnce(&H) -> Option<Cleanup> + 'static,
) {
    let (scope, index) = active_slot("use_attachment");
    let mut slots = scope.slots.borrow_mut();
    let changed = if index == slots.len() {
        slots.push(Box::new(AttachmentSlot {
            deps: Box::new(deps),
            cleanup: None,
        }));
        true
    } else {
        let slot = slots[index]
            .downcast_mut::<AttachmentSlot>()
            .unwrap_or_else(|| {
                panic!(
                    "hook order changed between renders at call position {index} — \
                     hooks must run unconditionally, in the same order, every render"
                )
            });
        let changed = slot.deps.downcast_ref::<D>() != Some(&deps);
        if changed {
            slot.deps = Box::new(deps);
        }
        changed
    };
    drop(slots);

    if changed {
        scope
            .pending_attachments
            .borrow_mut()
            .push(PendingAttachment {
                index,
                run: Box::new(move || setup(&handle)),
            });
    }
}

/// Runs every attachment queued during the render just finished, in the
/// order they were declared: previous cleanup first (if any), then the
/// new setup. Called by [`crate::ComponentScope::render`] right after a render
/// commits, after ordinary effects.
pub(crate) fn run_pending(scope: &ComponentScopeInner) {
    let pending: Vec<PendingAttachment> =
        std::mem::take(&mut *scope.pending_attachments.borrow_mut());
    for pending in pending {
        let old_cleanup = scope.slots.borrow_mut()[pending.index]
            .downcast_mut::<AttachmentSlot>()
            .expect("attachment slots are only ever populated as AttachmentSlot")
            .cleanup
            .take();
        if let Some(cleanup) = old_cleanup {
            crate::live::attachment_cleanup_ran();
            cleanup();
        }

        let new_cleanup = (pending.run)();
        if new_cleanup.is_some() {
            crate::live::attachment_cleanup_set();
        }
        scope.slots.borrow_mut()[pending.index]
            .downcast_mut::<AttachmentSlot>()
            .expect("attachment slots are only ever populated as AttachmentSlot")
            .cleanup = new_cleanup;
    }
}

/// Runs every remaining attachment's cleanup in the *reverse* of slot
/// order — called when a scope is dropped: removing an identity disposes
/// its hooks, and attachments are required specifically to
/// unwind in the opposite order they were set up in, unlike plain
/// effects.
pub(crate) fn dispose(scope: &ComponentScopeInner) {
    for slot in scope.slots.borrow_mut().iter_mut().rev() {
        if let Some(attachment) = slot.downcast_mut::<AttachmentSlot>()
            && let Some(cleanup) = attachment.cleanup.take()
        {
            crate::live::attachment_cleanup_ran();
            cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::ComponentScope;

    #[test]
    fn setup_runs_on_first_render_and_skips_unchanged_dependencies() {
        let (scope, _dirty) = ComponentScope::new();
        let runs = Rc::new(RefCell::new(0));

        for _ in 0..2 {
            let runs = Rc::clone(&runs);
            scope.render(move || {
                use_attachment((), 1, move |_handle| {
                    *runs.borrow_mut() += 1;
                    None
                });
            });
        }

        assert_eq!(*runs.borrow(), 1);
    }

    #[test]
    fn changed_dependencies_run_cleanup_then_the_new_setup() {
        let (scope, _dirty) = ComponentScope::new();
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        for dep in [1, 2] {
            let events_setup = Rc::clone(&events);
            let events_cleanup = Rc::clone(&events);
            scope.render(move || {
                use_attachment((), dep, move |_handle| {
                    events_setup.borrow_mut().push("setup");
                    Some(Box::new(move || events_cleanup.borrow_mut().push("cleanup")) as Cleanup)
                });
            });
        }

        assert_eq!(*events.borrow(), vec!["setup", "cleanup", "setup"]);
    }

    #[test]
    fn multiple_attachments_set_up_in_order_and_clean_up_in_reverse_on_unmount() {
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));
        let (scope, _dirty) = ComponentScope::new();

        let events_for_render = Rc::clone(&events);
        scope.render(move || {
            let events_a_setup = Rc::clone(&events_for_render);
            let events_a_cleanup = Rc::clone(&events_for_render);
            use_attachment((), (), move |_| {
                events_a_setup.borrow_mut().push("setup a");
                Some(Box::new(move || events_a_cleanup.borrow_mut().push("cleanup a")) as Cleanup)
            });

            let events_b_setup = Rc::clone(&events_for_render);
            let events_b_cleanup = Rc::clone(&events_for_render);
            use_attachment((), (), move |_| {
                events_b_setup.borrow_mut().push("setup b");
                Some(Box::new(move || events_b_cleanup.borrow_mut().push("cleanup b")) as Cleanup)
            });
        });
        assert_eq!(*events.borrow(), vec!["setup a", "setup b"]);

        drop(scope);
        assert_eq!(
            *events.borrow(),
            vec!["setup a", "setup b", "cleanup b", "cleanup a"],
            "unmount cleanup must run in the reverse of declaration order"
        );
    }

    #[test]
    fn a_discarded_keyed_child_disposes_its_attachment() {
        let (root, _dirty) = ComponentScope::new();
        let disposed = Rc::new(RefCell::new(false));

        root.render(|| {
            crate::use_child_scope_keyed("temporary", || {
                let disposed = Rc::clone(&disposed);
                use_attachment((), (), move |_| {
                    Some(Box::new(move || *disposed.borrow_mut() = true) as Cleanup)
                });
            });
        });
        assert!(
            !*disposed.borrow(),
            "cleanup must not run while the key is still present"
        );

        // Second render omits "temporary" entirely — a discarded tree.
        root.render(|| {});
        assert!(
            *disposed.borrow(),
            "a key no longer rendered must dispose its attachment"
        );
    }

    #[test]
    #[should_panic(expected = "use_signal called outside of ComponentScope::render")]
    fn hooks_cannot_be_called_inside_a_deferred_setup_callback() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            use_attachment((), (), |_| {
                crate::use_signal(|| 0);
                None
            });
        });
    }

    #[test]
    #[should_panic(expected = "use_attachment called outside of ComponentScope::render")]
    fn use_attachment_outside_a_render_panics() {
        use_attachment((), (), |_| None);
    }

    #[test]
    #[should_panic(expected = "hook order changed between renders")]
    fn a_non_attachment_hook_at_the_same_position_panics() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            use_attachment((), (), |_| None);
        });
        scope.render(|| {
            crate::use_signal(|| 0);
        });
    }
}
