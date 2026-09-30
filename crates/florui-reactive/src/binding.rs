//! [`Binding`]: a typed, read/write value exchange for editable component
//! values, kept separate from ordinary read-only props. Receiving a plain `T`
//! never grants mutation rights; only a `Binding<T>` does, and even then
//! the owner decides whether to actually accept a requested update.
//!
//! This type covers the *logical* read/write contract only: what the
//! owner's value currently is, and how a requested change reaches the
//! owner. It does not cover *visual* reconciliation of an editable
//! control — see [`Self::get`]'s doc for exactly what "the owner rejected
//! an update" guarantees and what it leaves to whatever control eventually
//! reads a `Binding`, since that's a real, currently open question rather
//! than something a `Binding` resolves on its own.
//!
//! Not addressed here (tracked gaps, not oversights): a real input
//! control's own editing buffer (distinct from its committed value),
//! commit timing, IME composition, and selection preservation through a
//! rejected edit all need an actual text-input widget, which doesn't
//! exist in this engine yet. This module is the primitive that widget
//! will read and write through, proven here with plain values rather than
//! a real control.

use std::fmt;
use std::rc::Rc;

/// A read/write exchange for one value: [`Self::get`] reads the owner's
/// current value, [`Self::request_update`] asks the owner to change it.
/// The owner remains the source of truth and may reject a request outright
/// (a value that fails validation, for example).
///
/// A `Binding` is a **snapshot**, captured once at construction — `get`
/// does not re-read the owner live, so a `Binding` held past the render
/// (or event handler) that created it keeps returning that original
/// value forever, not whatever the owner holds by the time you call
/// `get` later. A *fresh* `Binding`, derived again from the owner, is
/// what reflects a change — which is exactly how a rejected
/// `request_update` "reconciles": the next fresh `Binding` simply never
/// picked up the rejected value in the first place, the same way it
/// wouldn't pick up any other change nobody accepted. That's the whole
/// logical-value guarantee this type makes.
///
/// It says nothing about what a real editable control does with that
/// guarantee. If nothing the owner does on rejection causes a re-render
/// (no accepted `Signal::set` happened, so nothing is marked dirty), no
/// fresh `Binding` gets derived at all — and even where one is, a native
/// widget that already echoed the rejected keystroke into its own visible
/// text needs to actively resync its displayed content to match, or the
/// rejection stays invisible. Neither half of that exists yet: there's no
/// widget, and nothing here forces a re-render on rejection. A real
/// control built on this type will need to solve both explicitly.
pub struct Binding<T> {
    value: T,
    on_request: Rc<dyn Fn(T)>,
}

impl<T: Clone> Binding<T> {
    /// Builds a binding from `value` (the owner's current value as of now)
    /// and `on_request` (what to do with a requested new value —
    /// typically validate, then write to the owner's own storage only if
    /// accepted). Applications needing validation or controlled acceptance
    /// build a `Binding` this way directly; [`crate::Signal::binding`] is
    /// the convenience adapter for the common "always accept" case.
    pub fn new(value: T, on_request: impl Fn(T) + 'static) -> Self {
        Self {
            value,
            on_request: Rc::new(on_request),
        }
    }

    /// The owner's value as of when this binding was created — a
    /// snapshot, not a live read; see the type-level doc for what that
    /// means for a `Binding` held past its creating render.
    pub fn get(&self) -> T {
        self.value.clone()
    }

    /// Asks the owner to change its value to `value`. Does not itself
    /// change anything this binding can read, and does not by itself
    /// cause anything to re-render — whether a rejection ever becomes
    /// observable depends entirely on what `on_request` does, and, for a
    /// real control, on that control resyncing its own display; see the
    /// type-level doc.
    pub fn request_update(&self, value: T) {
        (self.on_request)(value);
    }
}

impl<T> fmt::Debug for Binding<T> {
    /// Same "identity, not content" reasoning as `PartialEq` -- printing
    /// `value` would need `T: Debug`, a bound this type otherwise never
    /// requires of its callers.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Binding(..)")
    }
}

impl<T: Clone> Clone for Binding<T> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            on_request: Rc::clone(&self.on_request),
        }
    }
}

impl<T> PartialEq for Binding<T> {
    /// Equal only if they share the same owner callback -- comparing
    /// behavior isn't possible, so this is identity, not content,
    /// equality; same rationale as `florui::Handler`'s own `PartialEq`.
    /// `value` is deliberately not compared: two `Binding`s snapshotting
    /// the same owner at different renders carry different `value`s but
    /// are still "the same binding" for `ElementNode`'s own diffing needs.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.on_request, &other.on_request)
    }
}

impl<T> Eq for Binding<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_reads_back_the_value_it_was_built_with() {
        let binding = Binding::new(7, |_| {});
        assert_eq!(binding.get(), 7);
    }

    #[test]
    fn request_update_calls_the_owners_callback_with_the_requested_value() {
        let requested = Rc::new(std::cell::RefCell::new(None));
        let requested_in_binding = Rc::clone(&requested);
        let binding = Binding::new(1, move |value| {
            *requested_in_binding.borrow_mut() = Some(value)
        });
        binding.request_update(2);
        assert_eq!(*requested.borrow(), Some(2));
    }

    #[test]
    fn request_update_does_not_change_what_this_binding_itself_reads() {
        let binding = Binding::new(1, |_| {});
        binding.request_update(2);
        assert_eq!(
            binding.get(),
            1,
            "a Binding is a snapshot — accepting a request is the owner's job, observed by \
             deriving a fresh Binding afterward, not by this one changing in place"
        );
    }

    #[test]
    fn a_rejecting_owner_leaves_the_next_derived_binding_unchanged() {
        let accepted = Rc::new(std::cell::Cell::new(10));
        let make_binding = || {
            let accepted = Rc::clone(&accepted);
            Binding::new(accepted.get(), move |value: i32| {
                // Only accept even numbers — an odd request is rejected.
                if value % 2 == 0 {
                    accepted.set(value);
                }
            })
        };

        make_binding().request_update(11); // rejected
        assert_eq!(
            make_binding().get(),
            10,
            "a rejected update must reconcile back to the last accepted value, not the rejected one"
        );

        make_binding().request_update(12); // accepted
        assert_eq!(make_binding().get(), 12);
    }

    #[test]
    fn clones_of_the_same_binding_are_equal_even_with_different_snapshots() {
        let binding = Binding::new(1, |_| {});
        let mut clone = binding.clone();
        clone.value = 2;
        assert_eq!(
            binding, clone,
            "equality is the owner callback's identity, not the snapshotted value"
        );
    }

    #[test]
    fn independently_constructed_bindings_are_not_equal() {
        let a = Binding::new(1, |_| {});
        let b = Binding::new(1, |_| {});
        assert_ne!(a, b);
    }
}
