//! [`UpdateTrace`]: which component a reactive write came from, without
//! recording the value written — see reactivity.md's "Expose state update
//! provenance to the development trace without unnecessarily recording
//! user-entered values" and "verify traces identify
//! the initiating component without logging sensitive payloads."
//!
//! `#[component]` wraps every component's body in [`with_component`]
//! automatically, so a plain [`crate::Signal::set`] anywhere in ordinary
//! component code is attributed with no extra effort from whoever wrote
//! it. A write that commits well after the render that started it — an
//! async resource's eventual completion — needs [`with_component`] called
//! again around that later commit; [`crate::use_resource`] does this
//! itself, using whichever component was active when the fetch started.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

/// How many recent traces [`recent`] keeps — bounded (trace retention is
/// capped), not a log that
/// grows for as long as the app runs.
const RETAINED: usize = 256;

/// One reactive write: which component's render or effect was active when
/// it committed, if any. Carries no value — only enough to point a
/// developer at *where* an update came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateTrace {
    /// Orders traces relative to each other; not a wall-clock timestamp.
    pub sequence: u64,
    pub component: Option<&'static str>,
}

type Listener = Box<dyn Fn(UpdateTrace)>;

thread_local! {
    static COMPONENT_STACK: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static NEXT_SEQUENCE: Cell<u64> = const { Cell::new(0) };
    static RECENT: RefCell<VecDeque<UpdateTrace>> = const { RefCell::new(VecDeque::new()) };
    static LISTENER: RefCell<Option<Listener>> = const { RefCell::new(None) };
}

/// Attributes any write [`record`] observes while `f` runs — directly, or
/// later through a [`crate::use_effect`] `f`'s own render queues, since
/// that effect still runs before `f` returns (see [`crate::ComponentScope::render`])
/// — to `component`. Nested calls attribute to the innermost one active,
/// matching how a child component's own writes are its own, not its
/// parent's.
pub fn with_component<T>(component: &'static str, f: impl FnOnce() -> T) -> T {
    struct PopGuard;
    impl Drop for PopGuard {
        fn drop(&mut self) {
            COMPONENT_STACK.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }

    COMPONENT_STACK.with(|stack| stack.borrow_mut().push(component));
    let _guard = PopGuard;
    f()
}

/// The innermost component currently active via [`with_component`], if
/// any.
pub fn current_component() -> Option<&'static str> {
    COMPONENT_STACK.with(|stack| stack.borrow().last().copied())
}

/// Records a write attributed to [`current_component`] — called by
/// [`crate::Signal::set`]; every other write path (a resource's state, an
/// error boundary's reported error) is built on `Signal` and so is
/// already covered without calling this itself.
pub(crate) fn record() {
    let trace = UpdateTrace {
        sequence: NEXT_SEQUENCE.with(|next| {
            let value = next.get();
            next.set(value + 1);
            value
        }),
        component: current_component(),
    };
    RECENT.with(|recent| {
        let mut recent = recent.borrow_mut();
        if recent.len() == RETAINED {
            recent.pop_front();
        }
        recent.push_back(trace);
    });
    LISTENER.with(|listener| {
        if let Some(listener) = listener.borrow().as_ref() {
            listener(trace);
        }
    });
}

/// The most recent traces, oldest first — bounded to the last
/// [`RETAINED`] writes regardless of how many actually happened.
pub fn recent() -> Vec<UpdateTrace> {
    RECENT.with(|recent| recent.borrow().iter().copied().collect())
}

/// Registers `listener` to run on every future [`record`] — a devtool's
/// live view uses this instead of polling [`recent`]. Replaces any
/// previously registered listener.
pub fn on_trace(listener: impl Fn(UpdateTrace) + 'static) {
    LISTENER.with(|slot| *slot.borrow_mut() = Some(Box::new(listener)));
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn current_component_is_none_outside_with_component() {
        assert_eq!(current_component(), None);
    }

    #[test]
    fn with_component_sets_and_restores_current_component() {
        assert_eq!(current_component(), None);
        with_component("Foo", || {
            assert_eq!(current_component(), Some("Foo"));
        });
        assert_eq!(current_component(), None);
    }

    #[test]
    fn nested_with_component_reflects_the_innermost() {
        with_component("Outer", || {
            assert_eq!(current_component(), Some("Outer"));
            with_component("Inner", || {
                assert_eq!(current_component(), Some("Inner"));
            });
            assert_eq!(
                current_component(),
                Some("Outer"),
                "leaving the inner call must restore the outer one, not clear it"
            );
        });
    }

    #[test]
    fn with_component_restores_on_panic() {
        let result = std::panic::catch_unwind(|| {
            with_component("Doomed", || {
                panic!("boom");
            });
        });
        assert!(result.is_err());
        assert_eq!(
            current_component(),
            None,
            "a panic inside with_component must not leave a stale entry on the stack"
        );
    }

    #[test]
    fn record_attributes_to_the_current_component() {
        let captured = Rc::new(RefCell::new(Vec::new()));
        let captured_in_listener = Rc::clone(&captured);
        on_trace(move |trace| captured_in_listener.borrow_mut().push(trace));

        record();
        with_component("Labeled", record);

        let traces = captured.borrow();
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].component, None);
        assert_eq!(traces[1].component, Some("Labeled"));
        assert!(
            traces[1].sequence > traces[0].sequence,
            "sequence must increase with each record"
        );
        drop(traces);

        // LISTENER is thread-local, and the test harness's worker threads
        // can outlive any single test — leaving this one registered could
        // call into `captured` (or panic on a poisoned borrow) from an
        // unrelated later test sharing the same thread.
        on_trace(|_| {});
    }

    #[test]
    fn recent_is_bounded() {
        for _ in 0..(RETAINED + 10) {
            record();
        }
        assert_eq!(recent().len(), RETAINED);
    }
}
