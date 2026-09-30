//! Non-reactive persistent storage via [`use_ref`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::scope::active_slot;

/// Persistent local storage that never marks its [`ComponentScope`](crate::ComponentScope)
/// dirty — unlike [`Signal`](crate::Signal), writing to a `Ref` never
/// schedules a render on its own.
pub struct Ref<T> {
    value: Rc<RefCell<T>>,
}

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        Self {
            value: Rc::clone(&self.value),
        }
    }
}

impl<T: Clone> Ref<T> {
    /// Reads the current value.
    pub fn get(&self) -> T {
        self.value.borrow().clone()
    }
}

impl<T> Ref<T> {
    /// Writes a new value. Does not schedule a render.
    pub fn set(&self, value: T) {
        *self.value.borrow_mut() = value;
    }

    /// Reads the current value by reference.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.value.borrow())
    }

    /// Mutates the current value in place. Does not schedule a render.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.value.borrow_mut())
    }
}

/// Persistent local storage: `init` runs once, the first render this call
/// appears in; every later render returns that same [`Ref`] untouched.
///
/// # Panics
///
/// Panics outside a [`ComponentScope::render`](crate::ComponentScope::render) pass, or if
/// hooks ran in a different order or count than last render.
pub fn use_ref<T: 'static>(init: impl FnOnce() -> T) -> Ref<T> {
    let (scope, index) = active_slot("use_ref");
    let mut slots = scope.slots.borrow_mut();
    if index == slots.len() {
        let cell = Ref {
            value: Rc::new(RefCell::new(init())),
        };
        crate::live::ref_created(&cell.value);
        slots.push(Box::new(cell.clone()));
        cell
    } else {
        slots[index]
            .downcast_ref::<Ref<T>>()
            .unwrap_or_else(|| {
                panic!(
                    "hook order changed between renders at call position {index} — \
                     hooks must run unconditionally, in the same order, every render"
                )
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ComponentScope;

    #[test]
    fn a_ref_reads_back_the_value_it_was_initialized_with() {
        let (scope, _dirty) = ComponentScope::new();
        let value = scope.render(|| use_ref(|| 42).get());
        assert_eq!(value, 42);
    }

    #[test]
    fn a_ref_persists_its_value_across_renders_of_the_same_scope() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| use_ref(|| 0).set(7));
        let value = scope.render(|| use_ref(|| 0).get());
        assert_eq!(
            value, 7,
            "the second render's initializer (0) must not overwrite the first render's set(7)"
        );
    }

    #[test]
    fn setting_a_ref_does_not_mark_the_scope_dirty() {
        let (scope, dirty) = ComponentScope::new();
        scope.render(|| use_ref(|| 0).set(1));
        assert!(
            !dirty.get(),
            "a Ref write must not schedule a render, unlike Signal::set"
        );
    }

    #[test]
    fn with_mut_mutates_in_place_and_persists() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            use_ref(Vec::<i32>::new).with_mut(|v| v.push(1));
        });
        scope.render(|| {
            use_ref(Vec::<i32>::new).with_mut(|v| v.push(2));
        });
        let value = scope.render(|| use_ref(Vec::<i32>::new).with(|v| v.clone()));
        assert_eq!(value, vec![1, 2]);
    }

    #[test]
    #[should_panic(expected = "use_ref called outside of ComponentScope::render")]
    fn use_ref_outside_a_render_panics() {
        use_ref(|| 0);
    }

    #[test]
    #[should_panic(expected = "hook order changed between renders")]
    fn a_different_type_at_the_same_call_position_panics() {
        let (scope, _dirty) = ComponentScope::new();
        scope.render(|| {
            use_ref(|| 0_i32);
        });
        scope.render(|| {
            use_ref(|| "not an i32");
        });
    }
}
