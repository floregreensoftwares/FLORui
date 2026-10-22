//! [`DirtyFlag`]: notices a [`crate::Signal::set`] the moment it happens,
//! rather than making a host poll for one after specific known events.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct DirtyFlagInner {
    flag: Cell<bool>,
    waker: RefCell<Option<Box<dyn Fn()>>>,
}

/// Shared handle onto one [`ComponentScope`](crate::ComponentScope)'s dirty state. Cloning
/// is cheap and every clone reads/writes the same underlying flag.
pub struct DirtyFlag {
    inner: Rc<DirtyFlagInner>,
}

impl DirtyFlag {
    pub(crate) fn new() -> Self {
        Self {
            inner: Rc::new(DirtyFlagInner {
                flag: Cell::new(false),
                waker: RefCell::new(None),
            }),
        }
    }

    /// Sets the flag and, if a waker is registered, calls it — immediately,
    /// unless a [`crate::batch`] is in progress, in which case the wake is
    /// deferred until that batch finishes (see its module docs) so a host
    /// isn't woken once per write inside one event.
    pub fn mark(&self) {
        self.inner.flag.set(true);
        crate::batch::defer_or_fire(self);
    }

    /// Calls the registered waker, if any, unconditionally — used both by
    /// [`Self::mark`]'s own immediate (non-batched) path and by
    /// [`crate::batch`] once it's ready to flush a deferred wake.
    pub(crate) fn fire_waker(&self) {
        if let Some(waker) = self.inner.waker.borrow().as_ref() {
            waker();
        }
    }

    /// Whether `self` and `other` are the same underlying flag (clones of
    /// one another), not merely two flags that happen to agree on state —
    /// used by [`crate::batch`] to dedupe repeated marks of the same flag
    /// within one batch into a single deferred wake.
    pub(crate) fn same_flag_as(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.inner, &other.inner)
    }

    /// Whether [`Self::mark`] has been called since the last [`Self::clear`].
    pub fn get(&self) -> bool {
        self.inner.flag.get()
    }

    /// Resets the flag, typically right after a host has acted on it.
    pub fn clear(&self) {
        self.inner.flag.set(false);
    }

    /// Registers `waker` to run every time this flag is marked from now
    /// on. Replaces any previously registered waker — one host owns one
    /// flag at a time.
    pub fn on_mark(&self, waker: impl Fn() + 'static) {
        *self.inner.waker.borrow_mut() = Some(Box::new(waker));
    }
}

impl Clone for DirtyFlag {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell as StdCell;

    use super::*;

    #[test]
    fn starts_clear() {
        let flag = DirtyFlag::new();
        assert!(!flag.get());
    }

    #[test]
    fn mark_sets_it_and_clear_resets_it() {
        let flag = DirtyFlag::new();
        flag.mark();
        assert!(flag.get());
        flag.clear();
        assert!(!flag.get());
    }

    #[test]
    fn a_registered_waker_runs_immediately_on_mark() {
        let flag = DirtyFlag::new();
        let woken = Rc::new(StdCell::new(false));
        let woken_in_waker = woken.clone();
        flag.on_mark(move || woken_in_waker.set(true));

        assert!(!woken.get(), "registering a waker must not itself wake it");
        flag.mark();
        assert!(woken.get());
    }

    #[test]
    fn clones_share_the_same_flag_and_waker() {
        let flag = DirtyFlag::new();
        let clone = flag.clone();
        let woken = Rc::new(StdCell::new(0));
        let woken_in_waker = woken.clone();
        flag.on_mark(move || woken_in_waker.set(woken_in_waker.get() + 1));

        clone.mark();
        assert!(flag.get());
        assert_eq!(woken.get(), 1);
    }
}
