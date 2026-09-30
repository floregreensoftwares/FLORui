//! [`ComponentScope`]: where a tree's hook state lives across repeated re-renders.

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::{DirtyFlag, Key};

thread_local! {
    pub(crate) static ACTIVE_SCOPES: RefCell<Vec<Rc<ComponentScopeInner>>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct ComponentScopeInner {
    pub(crate) slots: RefCell<Vec<Box<dyn Any>>>,
    pub(crate) cursor: Cell<usize>,
    pub(crate) dirty: DirtyFlag,
    pub(crate) context: RefCell<HashMap<TypeId, Box<dyn Any>>>,
    pub(crate) pending_effects: RefCell<Vec<PendingEffect>>,
    /// Queued the same way `pending_effects` is, but kept separate so
    /// [`crate::attachment`] can dispose its own slots in the reverse of
    /// their declaration order on unmount (attachments clean up in
    /// reverse order), without changing plain effects' own (forward)
    /// disposal order.
    pub(crate) pending_attachments: RefCell<Vec<PendingAttachment>>,
    /// Child scopes addressed by [`Key`] instead of call position — see
    /// [`use_child_scope_keyed`]. Separate from `slots`: a keyed child's
    /// identity must survive its position changing between renders, which
    /// a positional slot index cannot express.
    pub(crate) keyed_children: RefCell<HashMap<Key, ComponentScope>>,
    /// Which keys `use_child_scope_keyed` was actually called with during
    /// the render pass in progress — reset at the start of each
    /// [`ComponentScope::render`], consulted at the end to prune any
    /// `keyed_children` entry not touched this time (its item was removed
    /// or its key changed), disposing it the same way dropping any other
    /// scope does.
    pub(crate) keys_seen_this_render: RefCell<HashSet<Key>>,
}

impl Drop for ComponentScopeInner {
    /// Removing an identity disposes its hooks: every effect and
    /// attachment this scope (and, as the field drop cascades into any
    /// stored child `ComponentScope`, every scope nested inside it) still owns runs
    /// its cleanup here.
    fn drop(&mut self) {
        crate::effect::dispose(self);
        crate::attachment::dispose(self);
    }
}

/// An effect queued during a render, to run once that render commits.
pub(crate) struct PendingEffect {
    pub(crate) index: usize,
    pub(crate) run: Box<dyn FnOnce() -> Option<crate::effect::Cleanup>>,
}

/// An attachment queued during a render, to run once that render commits
/// — see [`crate::attachment`].
pub(crate) struct PendingAttachment {
    pub(crate) index: usize,
    pub(crate) run: Box<dyn FnOnce() -> Option<crate::effect::Cleanup>>,
}

/// Where a tree's hook state lives across repeated re-renders — replaying
/// `use_signal`/`use_memo` calls against the same slots, in the same
/// order, is what lets a plain Rust function call keep state instead of
/// starting fresh every time.
pub struct ComponentScope {
    inner: Rc<ComponentScopeInner>,
}

impl ComponentScope {
    /// A fresh scope, plus the [`DirtyFlag`] a host uses to know when a
    /// [`Signal::set`](crate::Signal::set) inside it (or inside any
    /// [`use_child_scope`] nested within it) means "render again" —
    /// either by polling [`DirtyFlag::get`] or, better, registering a
    /// [`DirtyFlag::on_mark`] callback to hear about it immediately.
    pub fn new() -> (Self, DirtyFlag) {
        let dirty = DirtyFlag::new();
        (Self::with_dirty_flag(dirty.clone()), dirty)
    }

    /// A fresh scope sharing an existing dirty flag, so a write anywhere
    /// under it is still visible to whoever holds that flag.
    fn with_dirty_flag(dirty: DirtyFlag) -> Self {
        let inner = Rc::new(ComponentScopeInner {
            slots: RefCell::new(Vec::new()),
            cursor: Cell::new(0),
            dirty,
            context: RefCell::new(HashMap::new()),
            pending_effects: RefCell::new(Vec::new()),
            pending_attachments: RefCell::new(Vec::new()),
            keyed_children: RefCell::new(HashMap::new()),
            keys_seen_this_render: RefCell::new(HashSet::new()),
        });
        crate::live::scope_created(&inner);
        Self { inner }
    }

    /// Runs `render` with this scope active, so hook calls inside see the
    /// slots this scope left behind last time. Provided context is
    /// cleared first — a render that wants it visible must provide it
    /// again, the same way it re-runs every other line of the component.
    /// Once `render` returns, any [`use_effect`](crate::use_effect) queued
    /// during it runs — after commit, same as a real hook model requires.
    pub fn render<T>(&self, render: impl FnOnce() -> T) -> T {
        self.inner.cursor.set(0);
        self.inner.context.borrow_mut().clear();
        self.inner.keys_seen_this_render.borrow_mut().clear();
        ACTIVE_SCOPES.with(|scopes| scopes.borrow_mut().push(self.inner.clone()));
        let result = render();
        let popped = ACTIVE_SCOPES.with(|scopes| scopes.borrow_mut().pop());
        debug_assert!(
            popped.is_some_and(|popped| Rc::ptr_eq(&popped, &self.inner)),
            "ComponentScope::render must pop the exact scope it pushed"
        );
        // A keyed child not touched this render had its item removed, or
        // its key changed — either way it's gone, and dropping its ComponentScope
        // here (if this was the last reference to it) disposes it, same
        // as any other scope going away.
        let seen = self.inner.keys_seen_this_render.borrow();
        self.inner
            .keyed_children
            .borrow_mut()
            .retain(|key, _| seen.contains(key));
        drop(seen);
        crate::effect::run_pending(&self.inner);
        crate::attachment::run_pending(&self.inner);
        result
    }
}

impl Clone for ComponentScope {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl Default for ComponentScope {
    fn default() -> Self {
        Self::new().0
    }
}

/// Renders `render` once, in a fresh, throwaway [`ComponentScope`] — every
/// `#[component]` call needs one active somewhere up the call stack, and
/// this is the convenient choice for a one-shot render (a test, a static
/// capture) that never needs its hook state to persist afterward. A host
/// that renders repeatedly should keep its own [`ComponentScope`] and call
/// [`ComponentScope::render`] directly instead, so state actually survives between
/// renders.
pub fn render_once<T>(render: impl FnOnce() -> T) -> T {
    ComponentScope::new().0.render(render)
}

/// Gives each call site of this function its own persistent [`ComponentScope`],
/// nested inside whichever scope is currently active — this is the
/// mechanism `#[component]` needs so every component gets its own hook
/// state, not one shared list for the whole tree; it is not meant to be
/// called directly from ordinary component code. The child shares its
/// parent's dirty flag, so a write anywhere under it still reaches
/// whoever is watching the root.
///
/// # Panics
///
/// Panics if called outside a [`ComponentScope::render`] pass, or if this call's
/// position held something other than a child scope last render.
pub fn use_child_scope<T>(render: impl FnOnce() -> T) -> T {
    let (parent, index) = active_slot("use_child_scope");
    let mut slots = parent.slots.borrow_mut();
    if index == slots.len() {
        slots.push(Box::new(ComponentScope::with_dirty_flag(
            parent.dirty.clone(),
        )));
    }
    let child = slots[index]
        .downcast_ref::<ComponentScope>()
        .unwrap_or_else(|| {
            panic!(
                "hook order changed between renders at call position {index} — \
                 hooks must run unconditionally, in the same order, every render"
            )
        })
        .clone();
    drop(slots);
    child.render(render)
}

/// Gives the child at `key` (within whichever [`ComponentScope`] is currently
/// active) its own persistent hook state, addressed by that key instead
/// of call position — unlike [`use_child_scope`], a keyed child keeps its
/// state across renders even if its position among siblings changes
/// (items reordering in a list), as long as the same key is used. A key
/// no longer passed on a later render is treated as removed: its scope
/// (and everything nested inside it) is disposed the next time its
/// parent renders — see [`ComponentScope::render`].
///
/// # Panics
///
/// Panics if called outside a [`ComponentScope::render`] pass, or if `key` was
/// already used by an earlier sibling in this same render — keys are
/// scoped to siblings, and a duplicate would otherwise silently reuse one
/// item's state for another.
pub fn use_child_scope_keyed<K: Into<Key>, T>(key: K, render: impl FnOnce() -> T) -> T {
    let key = key.into();
    let parent = active_scope("use_child_scope_keyed");
    let first_use_this_render = parent
        .keys_seen_this_render
        .borrow_mut()
        .insert(key.clone());
    assert!(
        first_use_this_render,
        "duplicate key {key:?} used by more than one sibling in the same render — \
         keys must be unique among siblings"
    );
    let child = parent
        .keyed_children
        .borrow_mut()
        .entry(key)
        .or_insert_with(|| ComponentScope::with_dirty_flag(parent.dirty.clone()))
        .clone();
    child.render(render)
}

/// The currently active [`ComponentScope`], without reserving a positional slot in
/// it — for hooks (like [`use_child_scope_keyed`]) whose identity comes
/// from somewhere other than call order.
///
/// # Panics
///
/// Panics if called outside a [`ComponentScope::render`] pass.
fn active_scope(hook_name: &str) -> Rc<ComponentScopeInner> {
    ACTIVE_SCOPES.with(|scopes| {
        scopes.borrow().last().cloned().unwrap_or_else(|| {
            panic!(
                "{hook_name} called outside of ComponentScope::render — hooks must run \
                 during a component tree's render pass"
            )
        })
    })
}

/// Reserves the next call-order slot in the currently active [`ComponentScope`] —
/// shared by every order-sensitive hook. `hook_name` names the caller in
/// the panic message.
///
/// # Panics
///
/// Panics if called outside a [`ComponentScope::render`] pass.
pub(crate) fn active_slot(hook_name: &str) -> (Rc<ComponentScopeInner>, usize) {
    let scope = active_scope(hook_name);
    let index = scope.cursor.get();
    scope.cursor.set(index + 1);
    (scope, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::use_signal;

    #[test]
    fn independent_scopes_never_share_state() {
        let (a, _) = ComponentScope::new();
        let (b, _) = ComponentScope::new();
        a.render(|| use_signal(|| 0).set(1));
        let b_value = b.render(|| use_signal(|| 0).get());
        assert_eq!(b_value, 0, "scope b's signal is its own, not scope a's");
    }

    #[test]
    fn nested_scope_renders_restore_the_outer_scope_afterward() {
        let (outer, _) = ComponentScope::new();
        let (inner, _) = ComponentScope::new();
        outer.render(|| {
            use_signal(|| "outer-before").get();
            inner.render(|| {
                use_signal(|| "inner").get();
            });
            // Must land on the outer scope's second slot, unaffected by inner's render.
            let value = use_signal(|| "outer-after").get();
            assert_eq!(value, "outer-after");
        });
    }

    #[test]
    fn a_child_scope_persists_its_own_state_across_renders_of_the_parent() {
        let (root, _dirty) = ComponentScope::new();
        root.render(|| {
            use_child_scope(|| use_signal(|| 0).set(5));
        });
        let value = root.render(|| use_child_scope(|| use_signal(|| 0).get()));
        assert_eq!(
            value, 5,
            "the child scope, and its signal, must be the same instance across parent renders"
        );
    }

    #[test]
    fn two_child_scopes_at_different_positions_are_independent() {
        let (root, _dirty) = ComponentScope::new();
        let (a, b) = root.render(|| {
            let a = use_child_scope(|| use_signal(|| "a").get());
            let b = use_child_scope(|| use_signal(|| "b").get());
            (a, b)
        });
        assert_eq!(a, "a");
        assert_eq!(b, "b");
    }

    #[test]
    fn setting_a_signal_in_a_child_scope_marks_the_shared_dirty_flag() {
        let (root, dirty) = ComponentScope::new();
        root.render(|| {
            use_child_scope(|| use_signal(|| 0).set(1));
        });
        assert!(
            dirty.get(),
            "a host watching only the root's flag must still see a write deep in a child scope"
        );
    }

    #[test]
    fn a_waker_registered_on_the_root_flag_fires_from_a_signal_set_in_a_child_scope() {
        let (root, dirty) = ComponentScope::new();
        let woken = Rc::new(std::cell::Cell::new(false));
        let woken_in_waker = Rc::clone(&woken);
        dirty.on_mark(move || woken_in_waker.set(true));

        root.render(|| {
            use_child_scope(|| use_signal(|| 0).set(1));
        });

        assert!(
            woken.get(),
            "a host should learn about the write immediately, not just by polling later"
        );
    }

    #[test]
    fn a_grandchild_scope_also_shares_the_root_dirty_flag() {
        let (root, dirty) = ComponentScope::new();
        root.render(|| {
            use_child_scope(|| {
                use_child_scope(|| use_signal(|| 0).set(1));
            });
        });
        assert!(dirty.get());
    }

    #[test]
    #[should_panic(expected = "use_child_scope called outside of ComponentScope::render")]
    fn use_child_scope_outside_a_render_panics() {
        use_child_scope(|| ());
    }

    #[test]
    #[should_panic(expected = "hook order changed between renders")]
    fn a_non_child_scope_hook_at_the_same_position_panics() {
        let (root, _dirty) = ComponentScope::new();
        root.render(|| {
            use_child_scope(|| ());
        });
        root.render(|| {
            use_signal(|| 0);
        });
    }

    #[test]
    fn a_keyed_child_persists_its_state_across_renders_with_the_same_key() {
        let (root, _dirty) = ComponentScope::new();
        root.render(|| {
            use_child_scope_keyed("a", || use_signal(|| 0).set(5));
        });
        let value = root.render(|| use_child_scope_keyed("a", || use_signal(|| 0).get()));
        assert_eq!(
            value, 5,
            "the keyed child, and its signal, must be the same instance across renders"
        );
    }

    #[test]
    fn reordering_keyed_children_keeps_each_ones_own_state() {
        let (root, _dirty) = ComponentScope::new();

        // First render: item "a" then item "b", each setting its own signal.
        root.render(|| {
            use_child_scope_keyed("a", || use_signal(|| "").set("a-state"));
            use_child_scope_keyed("b", || use_signal(|| "").set("b-state"));
        });

        // Second render: same two items, but "b" now comes first — a pure
        // positional model would have "b"'s render see "a"'s old slot (and
        // vice versa); a keyed one must not.
        let (b_value, a_value) = root.render(|| {
            let b = use_child_scope_keyed("b", || use_signal(|| "").get());
            let a = use_child_scope_keyed("a", || use_signal(|| "").get());
            (b, a)
        });

        assert_eq!(
            a_value, "a-state",
            "item \"a\" kept its own state after reordering"
        );
        assert_eq!(
            b_value, "b-state",
            "item \"b\" kept its own state after reordering"
        );
    }

    #[test]
    fn a_key_no_longer_rendered_disposes_its_scope() {
        let (root, _dirty) = ComponentScope::new();
        let disposed = Rc::new(std::cell::Cell::new(false));

        root.render(|| {
            use_child_scope_keyed("temporary", || {
                let disposed = Rc::clone(&disposed);
                crate::use_effect((), move || Some(Box::new(move || disposed.set(true)) as _));
            });
        });
        assert!(
            !disposed.get(),
            "cleanup must not run while the key is still present"
        );

        // Second render omits "temporary" entirely — it was removed from
        // whatever list produced it.
        root.render(|| {});
        assert!(
            disposed.get(),
            "a key no longer rendered must dispose its scope, running its effect cleanup"
        );
    }

    #[test]
    fn two_keyed_children_with_different_keys_are_independent() {
        let (root, _dirty) = ComponentScope::new();
        let (a, b) = root.render(|| {
            let a = use_child_scope_keyed("a", || use_signal(|| "a").get());
            let b = use_child_scope_keyed("b", || use_signal(|| "b").get());
            (a, b)
        });
        assert_eq!(a, "a");
        assert_eq!(b, "b");
    }

    #[test]
    #[should_panic(expected = "use_child_scope_keyed called outside of ComponentScope::render")]
    fn use_child_scope_keyed_outside_a_render_panics() {
        use_child_scope_keyed("a", || ());
    }

    #[test]
    #[should_panic(expected = "duplicate key")]
    fn a_duplicate_key_among_siblings_in_the_same_render_panics() {
        let (root, _dirty) = ComponentScope::new();
        root.render(|| {
            use_child_scope_keyed("a", || ());
            use_child_scope_keyed("a", || ());
        });
    }

    #[test]
    fn the_same_key_reused_across_separate_renders_is_not_a_duplicate() {
        let (root, _dirty) = ComponentScope::new();
        root.render(|| use_child_scope_keyed("a", || ()));
        // A second, later render reusing "a" is exactly the point of a
        // keyed child persisting — not a same-render duplicate.
        root.render(|| use_child_scope_keyed("a", || ()));
    }
}
