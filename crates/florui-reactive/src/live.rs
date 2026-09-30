//! Read-only accounting of the state that mounted components keep alive:
//! scopes, signals, refs and the cleanups of effects and attachments. A test
//! harness reads it before mounting and after disposing to find what a
//! component left behind, such as a signal kept alive by a callback that
//! captures it.
//!
//! Off unless the `live-counts` feature is enabled (or in this crate's own
//! tests): without it every hook below compiles to nothing.

#[cfg(any(test, feature = "live-counts"))]
mod counting {
    use std::any::Any;
    use std::cell::{Cell, RefCell};
    use std::rc::{Rc, Weak};

    /// How much live state there is on this thread.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct LiveCounts {
        pub scopes: usize,
        pub signals: usize,
        pub refs: usize,
        /// Effects whose cleanup is registered and has not run yet.
        pub effect_cleanups: usize,
        /// Attachments whose cleanup is registered and has not run yet.
        pub attachment_cleanups: usize,
    }

    impl LiveCounts {
        /// What is alive now that was not in `before`, per kind.
        pub fn since(self, before: LiveCounts) -> LiveCounts {
            LiveCounts {
                scopes: self.scopes.saturating_sub(before.scopes),
                signals: self.signals.saturating_sub(before.signals),
                refs: self.refs.saturating_sub(before.refs),
                effect_cleanups: self.effect_cleanups.saturating_sub(before.effect_cleanups),
                attachment_cleanups: self
                    .attachment_cleanups
                    .saturating_sub(before.attachment_cleanups),
            }
        }

        pub fn is_empty(self) -> bool {
            self == LiveCounts::default()
        }
    }

    #[derive(Default)]
    struct Tracked {
        scopes: Vec<Weak<dyn Any>>,
        signals: Vec<Weak<dyn Any>>,
        refs: Vec<Weak<dyn Any>>,
        /// Length at which dead entries are next dropped.
        prune_at: usize,
    }

    thread_local! {
        static TRACKED: RefCell<Tracked> = RefCell::new(Tracked::default());
        static EFFECT_CLEANUPS: Cell<isize> = const { Cell::new(0) };
        static ATTACHMENT_CLEANUPS: Cell<isize> = const { Cell::new(0) };
    }

    fn alive(list: &[Weak<dyn Any>]) -> usize {
        list.iter().filter(|weak| weak.strong_count() > 0).count()
    }

    fn push(pick: fn(&mut Tracked) -> &mut Vec<Weak<dyn Any>>, weak: Weak<dyn Any>) {
        TRACKED.with(|tracked| {
            let mut tracked = tracked.borrow_mut();
            let list = pick(&mut tracked);
            list.push(weak);
            let len = list.len();
            if len >= tracked.prune_at.max(1024) {
                tracked.scopes.retain(|weak| weak.strong_count() > 0);
                tracked.signals.retain(|weak| weak.strong_count() > 0);
                tracked.refs.retain(|weak| weak.strong_count() > 0);
                tracked.prune_at =
                    2 * (tracked.scopes.len() + tracked.signals.len() + tracked.refs.len());
            }
        });
    }

    pub(crate) fn scope_created<T: 'static>(scope: &Rc<T>) {
        push(|t| &mut t.scopes, Rc::downgrade(scope) as Weak<dyn Any>);
    }

    pub(crate) fn signal_created<T: 'static>(value: &Rc<T>) {
        push(|t| &mut t.signals, Rc::downgrade(value) as Weak<dyn Any>);
    }

    pub(crate) fn ref_created<T: 'static>(value: &Rc<T>) {
        push(|t| &mut t.refs, Rc::downgrade(value) as Weak<dyn Any>);
    }

    pub(crate) fn effect_cleanup_set() {
        EFFECT_CLEANUPS.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn effect_cleanup_ran() {
        EFFECT_CLEANUPS.with(|count| count.set(count.get() - 1));
    }

    pub(crate) fn attachment_cleanup_set() {
        ATTACHMENT_CLEANUPS.with(|count| count.set(count.get() + 1));
    }

    pub(crate) fn attachment_cleanup_ran() {
        ATTACHMENT_CLEANUPS.with(|count| count.set(count.get() - 1));
    }

    /// The live state on the calling thread right now.
    pub fn live_counts() -> LiveCounts {
        TRACKED.with(|tracked| {
            let tracked = tracked.borrow();
            LiveCounts {
                scopes: alive(&tracked.scopes),
                signals: alive(&tracked.signals),
                refs: alive(&tracked.refs),
                effect_cleanups: EFFECT_CLEANUPS.with(|c| c.get().max(0) as usize),
                attachment_cleanups: ATTACHMENT_CLEANUPS.with(|c| c.get().max(0) as usize),
            }
        })
    }
}

#[cfg(any(test, feature = "live-counts"))]
pub use counting::{LiveCounts, live_counts};
#[cfg(any(test, feature = "live-counts"))]
pub(crate) use counting::{
    attachment_cleanup_ran, attachment_cleanup_set, effect_cleanup_ran, effect_cleanup_set,
    ref_created, scope_created, signal_created,
};

#[cfg(not(any(test, feature = "live-counts")))]
mod off {
    use std::rc::Rc;

    #[inline(always)]
    pub(crate) fn scope_created<T: 'static>(_: &Rc<T>) {}
    #[inline(always)]
    pub(crate) fn signal_created<T: 'static>(_: &Rc<T>) {}
    #[inline(always)]
    pub(crate) fn ref_created<T: 'static>(_: &Rc<T>) {}
    #[inline(always)]
    pub(crate) fn effect_cleanup_set() {}
    #[inline(always)]
    pub(crate) fn effect_cleanup_ran() {}
    #[inline(always)]
    pub(crate) fn attachment_cleanup_set() {}
    #[inline(always)]
    pub(crate) fn attachment_cleanup_ran() {}
}

#[cfg(not(any(test, feature = "live-counts")))]
pub(crate) use off::{
    attachment_cleanup_ran, attachment_cleanup_set, effect_cleanup_ran, effect_cleanup_set,
    ref_created, scope_created, signal_created,
};

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;
    use crate::{ComponentScope, use_attachment, use_effect, use_ref, use_signal};

    #[test]
    fn everything_a_scope_created_is_gone_once_it_is_dropped() {
        let before = live_counts();
        {
            let (scope, _dirty) = ComponentScope::new();
            scope.render(|| {
                let _signal = use_signal(|| 1);
                let _cell = use_ref(|| 2);
                use_effect((), || Some(Box::new(|| {}) as crate::Cleanup));
                use_attachment((), (), |_| Some(Box::new(|| {}) as crate::Cleanup));
            });
            let during = live_counts().since(before);
            assert_eq!(
                (during.scopes, during.signals, during.refs),
                (1, 1, 1),
                "{during:?}"
            );
            assert_eq!((during.effect_cleanups, during.attachment_cleanups), (1, 1));
        }
        assert!(live_counts().since(before).is_empty());
    }

    #[test]
    fn a_signal_kept_alive_by_a_callback_that_captures_it_is_counted_after_disposal() {
        type Callback = Box<dyn Fn() -> i32>;
        let before = live_counts();
        {
            let (scope, _dirty) = ComponentScope::new();
            scope.render(|| {
                let signal: crate::Signal<Option<Rc<Callback>>> = use_signal(|| None);
                let captured = signal.clone();
                // A cycle: the signal holds a callback that holds the signal.
                let callback: Callback = Box::new(move || captured.get().map_or(0, |_| 1));
                signal.set(Some(Rc::new(callback)));
            });
        }
        let leaked = live_counts().since(before);
        assert_eq!(leaked.signals, 1, "{leaked:?}");
        assert_eq!(leaked.scopes, 0, "the scope itself is released");
    }

    #[test]
    fn a_cleanup_that_already_ran_is_not_counted() {
        let before = live_counts();
        let ran = Rc::new(RefCell::new(false));
        {
            let (scope, _dirty) = ComponentScope::new();
            let flag = Rc::clone(&ran);
            scope.render(move || {
                use_effect((), move || {
                    Some(Box::new(move || *flag.borrow_mut() = true) as crate::Cleanup)
                });
            });
            assert_eq!(live_counts().since(before).effect_cleanups, 1);
        }
        assert!(*ran.borrow());
        assert_eq!(live_counts().since(before).effect_cleanups, 0);
    }
}
