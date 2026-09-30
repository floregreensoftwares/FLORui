//! Synchronizing with something outside the element tree via [`use_effect`].

use std::any::Any;

use crate::scope::{ComponentScopeInner, PendingEffect, active_slot};

pub type Cleanup = Box<dyn FnOnce()>;

struct EffectSlot {
    deps: Box<dyn Any>,
    cleanup: Option<Cleanup>,
}

/// Runs `effect` after this render commits, but only the first time this
/// call site is reached or when `deps` differs from the previous render's.
/// `effect`'s return value, if any, is run as cleanup before the next time
/// it re-runs, and when the owning [`ComponentScope`](crate::ComponentScope) is dropped.
///
/// # Panics
///
/// Panics outside a [`ComponentScope::render`](crate::ComponentScope::render) pass, or if
/// hooks ran in a different order or count than last render.
pub fn use_effect<D>(deps: D, effect: impl FnOnce() -> Option<Cleanup> + 'static)
where
    D: PartialEq + 'static,
{
    let (scope, index) = active_slot("use_effect");
    let mut slots = scope.slots.borrow_mut();
    let changed = if index == slots.len() {
        slots.push(Box::new(EffectSlot {
            deps: Box::new(deps),
            cleanup: None,
        }));
        true
    } else {
        let slot = slots[index]
            .downcast_mut::<EffectSlot>()
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
        scope.pending_effects.borrow_mut().push(PendingEffect {
            index,
            run: Box::new(effect),
        });
    }
}

/// Runs every effect queued during the render just finished: previous
/// cleanup first (if any), then the new run. Called by
/// [`ComponentScope::render`](crate::ComponentScope::render) right after a render commits.
pub(crate) fn run_pending(scope: &ComponentScopeInner) {
    let pending: Vec<PendingEffect> = std::mem::take(&mut *scope.pending_effects.borrow_mut());
    for pending in pending {
        let old_cleanup = scope.slots.borrow_mut()[pending.index]
            .downcast_mut::<EffectSlot>()
            .expect("effect slots are only ever populated as EffectSlot")
            .cleanup
            .take();
        if let Some(cleanup) = old_cleanup {
            crate::live::effect_cleanup_ran();
            cleanup();
        }

        let new_cleanup = (pending.run)();
        if new_cleanup.is_some() {
            crate::live::effect_cleanup_set();
        }
        scope.slots.borrow_mut()[pending.index]
            .downcast_mut::<EffectSlot>()
            .expect("effect slots are only ever populated as EffectSlot")
            .cleanup = new_cleanup;
    }
}

/// Runs every remaining effect's cleanup, in slot order — called when a
/// scope is dropped: removing an identity disposes its hooks.
pub(crate) fn dispose(scope: &ComponentScopeInner) {
    for slot in scope.slots.borrow_mut().iter_mut() {
        if let Some(effect) = slot.downcast_mut::<EffectSlot>()
            && let Some(cleanup) = effect.cleanup.take()
        {
            crate::live::effect_cleanup_ran();
            cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::ComponentScope;

    #[test]
    fn runs_on_first_render_and_skips_unchanged_dependencies() {
        let (scope, _dirty) = ComponentScope::new();
        let runs = Rc::new(Cell::new(0));

        for _ in 0..2 {
            let runs = runs.clone();
            scope.render(move || {
                use_effect(1, move || {
                    runs.set(runs.get() + 1);
                    None
                });
            });
        }

        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn reruns_when_dependencies_change() {
        let (scope, _dirty) = ComponentScope::new();
        let runs = Rc::new(Cell::new(0));

        for dep in [1, 1, 2] {
            let runs = runs.clone();
            scope.render(move || {
                use_effect(dep, move || {
                    runs.set(runs.get() + 1);
                    None
                });
            });
        }

        assert_eq!(runs.get(), 2);
    }

    #[test]
    fn cleanup_runs_before_the_effect_reruns_on_changed_dependencies() {
        let (scope, _dirty) = ComponentScope::new();
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));

        for dep in [1, 2] {
            let events_run = events.clone();
            let events_cleanup = events.clone();
            scope.render(move || {
                use_effect(dep, move || {
                    events_run.borrow_mut().push("run");
                    Some(Box::new(move || events_cleanup.borrow_mut().push("cleanup")) as Cleanup)
                });
            });
        }

        assert_eq!(*events.borrow(), vec!["run", "cleanup", "run"]);
    }

    #[test]
    fn cleanup_runs_when_the_scope_is_dropped() {
        let events = Rc::new(RefCell::new(Vec::<&'static str>::new()));
        let (scope, _dirty) = ComponentScope::new();

        let events_run = events.clone();
        let events_cleanup = events.clone();
        scope.render(move || {
            use_effect(1, move || {
                events_run.borrow_mut().push("run");
                Some(Box::new(move || events_cleanup.borrow_mut().push("cleanup")) as Cleanup)
            });
        });
        assert_eq!(*events.borrow(), vec!["run"]);

        drop(scope);
        assert_eq!(*events.borrow(), vec!["run", "cleanup"]);
    }

    #[test]
    #[should_panic(expected = "use_effect called outside of ComponentScope::render")]
    fn use_effect_outside_a_render_panics() {
        use_effect(1, || None);
    }

    #[test]
    #[should_panic(expected = "hook order changed between renders")]
    fn a_non_effect_hook_at_the_same_position_panics() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            use_effect(1, || None);
        });
        scope.render(|| {
            crate::use_signal(|| 0);
        });
    }
}
