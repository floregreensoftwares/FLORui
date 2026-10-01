//! [`UpdateTrace`]: what happened, when, and why, without recording the value
//! written. A trace names the component whose render or effect was active, says
//! whether it is a signal write, an event dispatch or a resource starting or
//! completing, and carries the [`Origin`] that was active when it happened (a
//! write inside a click handler has the click as its origin).
//!
//! `#[component]` wraps every component's body in [`with_component`]
//! automatically, so a plain [`crate::Signal::set`] anywhere in ordinary
//! component code is attributed with no extra effort from whoever wrote
//! it. A write that commits well after the render that started it — an
//! async resource's eventual completion — needs [`with_component`] called
//! again around that later commit; [`crate::use_resource`] does this
//! itself, using whichever component was active when the fetch started.
//!
//! Recording is on in debug builds and, in release, with the `profiling`
//! feature ([`ENABLED`]); otherwise nothing is recorded and a write pays
//! nothing for it. Component attribution is always tracked, since element
//! source locations read it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

/// Whether this build records traces.
pub const ENABLED: bool = florui_profile::ENABLED;

/// How many recent traces [`recent`] keeps — bounded, not a log that grows for
/// as long as the app runs.
const RETAINED: usize = 256;

/// What a trace records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceKind {
    /// A [`crate::Signal::set`].
    SignalWrite,
    /// An event handler was invoked on an element.
    EventDispatched,
    /// A resource began fetching.
    ResourceStarted,
    /// A resource's fetch finished and its result is being committed.
    ResourceCompleted,
}

/// What was running when a trace was recorded, beyond the component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Ordinary component code: a render, an effect, or a write from outside
    /// any of the cases below.
    Code,
    /// A handler for `name` on the element with arena index `target`.
    Event { name: &'static str, target: usize },
    /// The writes a resource makes as it starts.
    ResourceStart,
    /// The writes a resource makes as it commits its result.
    ResourceCompletion,
}

/// One recorded step. Carries no value, only enough to point a developer at
/// where an update came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateTrace {
    /// Orders traces relative to each other.
    pub sequence: u64,
    /// When it happened, on the clock [`now`] reads.
    pub at: Duration,
    /// The component whose render or effect was active, if any.
    pub component: Option<&'static str>,
    pub kind: TraceKind,
    pub origin: Origin,
}

type Listener = Rc<dyn Fn(UpdateTrace)>;

thread_local! {
    static COMPONENT_STACK: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static ORIGIN_STACK: RefCell<Vec<Origin>> = const { RefCell::new(Vec::new()) };
    static NEXT_SEQUENCE: Cell<u64> = const { Cell::new(0) };
    static RECENT: RefCell<VecDeque<UpdateTrace>> = const { RefCell::new(VecDeque::new()) };
    static LISTENERS: RefCell<Vec<(u64, Listener)>> = const { RefCell::new(Vec::new()) };
    static NEXT_LISTENER: Cell<u64> = const { Cell::new(0) };
    static LEGACY_LISTENER: Cell<Option<u64>> = const { Cell::new(None) };
}

/// The time since this process first asked, on a monotonic clock. Traces are
/// stamped with it, and anything that times work alongside them should read
/// the same clock so the two can be laid side by side.
pub fn now() -> Duration {
    florui_profile::now()
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

/// Runs `f` as the handler of `name` on element `target`: records the
/// dispatch, and every trace recorded while `f` runs has that event as its
/// origin. A host calls this around each handler it invokes.
pub fn with_event<T>(name: &'static str, target: usize, f: impl FnOnce() -> T) -> T {
    let origin = Origin::Event { name, target };
    record_with_origin(TraceKind::EventDispatched, origin);
    with_origin(origin, f)
}

pub(crate) fn with_origin<T>(origin: Origin, f: impl FnOnce() -> T) -> T {
    if !ENABLED {
        return f();
    }
    struct PopGuard;
    impl Drop for PopGuard {
        fn drop(&mut self) {
            ORIGIN_STACK.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }

    ORIGIN_STACK.with(|stack| stack.borrow_mut().push(origin));
    let _guard = PopGuard;
    f()
}

fn current_origin() -> Origin {
    ORIGIN_STACK.with(|stack| stack.borrow().last().copied().unwrap_or(Origin::Code))
}

/// Records a write attributed to [`current_component`] — called by
/// [`crate::Signal::set`]; every other write path (a resource's state, an
/// error boundary's reported error) is built on `Signal` and so is
/// already covered without calling this itself.
pub(crate) fn record() {
    record_kind(TraceKind::SignalWrite);
}

pub(crate) fn record_kind(kind: TraceKind) {
    if ENABLED {
        record_with_origin(kind, current_origin());
    }
}

fn record_with_origin(kind: TraceKind, origin: Origin) {
    if !ENABLED {
        return;
    }
    let trace = UpdateTrace {
        sequence: NEXT_SEQUENCE.with(|next| {
            let value = next.get();
            next.set(value + 1);
            value
        }),
        at: now(),
        component: current_component(),
        kind,
        origin,
    };
    RECENT.with(|recent| {
        let mut recent = recent.borrow_mut();
        if recent.len() == RETAINED {
            recent.pop_front();
        }
        recent.push_back(trace);
    });
    // Listeners are cloned out first so one may subscribe or unsubscribe
    // while it runs.
    // Nothing is allocated when none is registered, the common case.
    if LISTENERS.with(|all| all.borrow().is_empty()) {
        return;
    }
    let listeners: Vec<Listener> =
        LISTENERS.with(|all| all.borrow().iter().map(|(_, l)| Rc::clone(l)).collect());
    for listener in listeners {
        listener(trace);
    }
}

/// The most recent traces, oldest first — bounded to the last
/// [`RETAINED`] writes regardless of how many actually happened.
pub fn recent() -> Vec<UpdateTrace> {
    RECENT.with(|recent| recent.borrow().iter().copied().collect())
}

/// The sequence number the next trace will get. Remember it, and
/// [`since`] later returns everything recorded after.
pub fn next_sequence() -> u64 {
    NEXT_SEQUENCE.with(Cell::get)
}

/// Retained traces with `sequence` at or after the given one, oldest first.
/// Traces that were evicted before the call are not returned.
pub fn since(sequence: u64) -> Vec<UpdateTrace> {
    RECENT.with(|recent| {
        recent
            .borrow()
            .iter()
            .filter(|t| t.sequence >= sequence)
            .copied()
            .collect()
    })
}

/// Keeps a listener registered until dropped.
#[must_use = "the listener is removed when the subscription is dropped"]
pub struct Subscription {
    id: u64,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        remove_listener(self.id);
    }
}

fn add_listener(listener: Listener) -> u64 {
    let id = NEXT_LISTENER.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    LISTENERS.with(|all| all.borrow_mut().push((id, listener)));
    id
}

fn remove_listener(id: u64) {
    LISTENERS.with(|all| all.borrow_mut().retain(|(existing, _)| *existing != id));
}

/// Registers `listener` to run on every future trace until the returned
/// subscription is dropped. Any number may be registered at once.
pub fn subscribe(listener: impl Fn(UpdateTrace) + 'static) -> Subscription {
    Subscription {
        id: add_listener(Rc::new(listener)),
    }
}

/// Registers `listener` to run on every future trace. Replaces whatever an
/// earlier call to this function registered, and leaves [`subscribe`]d
/// listeners alone.
pub fn on_trace(listener: impl Fn(UpdateTrace) + 'static) {
    if let Some(previous) = LEGACY_LISTENER.with(Cell::take) {
        remove_listener(previous);
    }
    let id = add_listener(Rc::new(listener));
    LEGACY_LISTENER.with(|slot| slot.set(Some(id)));
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

    #[cfg(any(debug_assertions, feature = "profiling"))]
    mod recording {
        use super::*;

        fn capture() -> (Rc<RefCell<Vec<UpdateTrace>>>, Subscription) {
            let captured = Rc::new(RefCell::new(Vec::new()));
            let sink = Rc::clone(&captured);
            let subscription = subscribe(move |t| sink.borrow_mut().push(t));
            (captured, subscription)
        }

        #[test]
        fn record_attributes_to_the_current_component() {
            let (captured, _subscription) = capture();

            record();
            with_component("Labeled", record);

            let traces = captured.borrow();
            assert_eq!(traces.len(), 2);
            assert_eq!(traces[0].component, None);
            assert_eq!(traces[1].component, Some("Labeled"));
            assert_eq!(traces[0].kind, TraceKind::SignalWrite);
            assert!(
                traces[1].sequence > traces[0].sequence,
                "sequence must increase with each record"
            );
        }

        #[test]
        fn traces_are_stamped_in_order_on_one_clock() {
            let (captured, _subscription) = capture();
            record();
            record();
            record();
            let traces = captured.borrow();
            assert!(traces[0].at <= traces[1].at && traces[1].at <= traces[2].at);
            assert!(traces[2].at <= now());
        }

        #[test]
        fn a_write_inside_an_event_has_that_event_as_its_origin() {
            let (captured, _subscription) = capture();
            record();
            with_event("click", 7, record);
            record();

            let traces = captured.borrow();
            let kinds: Vec<_> = traces.iter().map(|t| (t.kind, t.origin)).collect();
            let click = Origin::Event {
                name: "click",
                target: 7,
            };
            assert_eq!(
                kinds,
                [
                    (TraceKind::SignalWrite, Origin::Code),
                    (TraceKind::EventDispatched, click),
                    (TraceKind::SignalWrite, click),
                    (TraceKind::SignalWrite, Origin::Code),
                ]
            );
        }

        #[test]
        fn an_event_origin_is_restored_after_a_panic() {
            let result = std::panic::catch_unwind(|| with_event("click", 1, || panic!("boom")));
            assert!(result.is_err());
            assert_eq!(current_origin(), Origin::Code);
        }

        #[test]
        fn several_listeners_all_hear_a_trace_and_a_dropped_one_stops() {
            let (first, first_subscription) = capture();
            let (second, _second_subscription) = capture();
            record();
            drop(first_subscription);
            record();
            assert_eq!(first.borrow().len(), 1);
            assert_eq!(second.borrow().len(), 2);
        }

        #[test]
        fn on_trace_replaces_only_its_own_earlier_listener() {
            let (kept, _subscription) = capture();
            let old = Rc::new(Cell::new(0));
            let new = Rc::new(Cell::new(0));
            let old_in = Rc::clone(&old);
            on_trace(move |_| old_in.set(old_in.get() + 1));
            record();
            let new_in = Rc::clone(&new);
            on_trace(move |_| new_in.set(new_in.get() + 1));
            record();
            assert_eq!((old.get(), new.get()), (1, 1));
            assert_eq!(kept.borrow().len(), 2, "a subscription is not replaced");
            // Thread-local state outlives a test on a shared worker thread.
            on_trace(|_| {});
        }

        #[test]
        fn a_listener_may_subscribe_while_it_runs() {
            let inner = Rc::new(RefCell::new(Vec::new()));
            let held: Rc<RefCell<Vec<Subscription>>> = Rc::new(RefCell::new(Vec::new()));
            let (inner_in, held_in) = (Rc::clone(&inner), Rc::clone(&held));
            let outer = subscribe(move |_| {
                let sink = Rc::clone(&inner_in);
                held_in
                    .borrow_mut()
                    .push(subscribe(move |t| sink.borrow_mut().push(t)));
            });
            record();
            record();
            drop(outer);
            assert_eq!(
                inner.borrow().len(),
                1,
                "the second record reached the first inner"
            );
        }

        #[test]
        fn since_returns_only_later_traces() {
            record();
            let mark = next_sequence();
            record();
            record();
            let later = since(mark);
            assert_eq!(later.len(), 2);
            assert!(later.iter().all(|t| t.sequence >= mark));
        }

        #[test]
        fn recent_is_bounded() {
            for _ in 0..(RETAINED + 10) {
                record();
            }
            assert_eq!(recent().len(), RETAINED);
        }
    }

    #[cfg(not(any(debug_assertions, feature = "profiling")))]
    #[test]
    fn a_release_build_without_profiling_records_nothing() {
        record();
        with_event("click", 1, record);
        assert!(recent().is_empty());
    }
}
