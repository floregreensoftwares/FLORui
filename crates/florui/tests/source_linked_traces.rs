//! `florui_reactive::trace` through the real `#[component]`/`view!` macro
//! surface: a `Signal::set` inside a component is attributed to that
//! component's name, a nested component's own write is attributed to
//! itself rather than its parent, and an async resource's eventual
//! completion is still attributed to the component that started it even
//! though it commits outside any render (traces identify
//! the initiating component).

// Traces are recorded in a debug build, and in release with `profiling`.
#![cfg(any(debug_assertions, feature = "profiling"))]

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;
use florui::reactive::executor::LocalExecutor;
use florui::reactive::testing::manual_future;
use florui::reactive::trace::{self, Origin, TraceKind, UpdateTrace};

fn capture_traces() -> (Rc<RefCell<Vec<UpdateTrace>>>, impl Fn()) {
    let captured = Rc::new(RefCell::new(Vec::new()));
    let captured_in_listener = Rc::clone(&captured);
    trace::on_trace(move |t| captured_in_listener.borrow_mut().push(t));
    // Thread-local, and the test harness's worker threads can outlive any
    // one test — leaving a listener registered risks it firing into a
    // dropped `captured` from an unrelated later test on the same thread.
    (captured, || trace::on_trace(|_| {}))
}

#[component]
fn Counter() -> Element {
    let count = use_signal(|| 0);
    count.set(count.get() + 1);
    view! { <div /> }
}

#[test]
fn a_signal_set_inside_a_component_is_attributed_to_its_name() {
    let (captured, reset) = capture_traces();
    let (scope, _dirty) = ComponentScope::new();

    scope.render(|| view! { <Counter /> });

    assert!(
        captured
            .borrow()
            .iter()
            .any(|t| t.component == Some("Counter"))
    );
    reset();
}

#[component]
fn Child() -> Element {
    let count = use_signal(|| 0);
    count.set(count.get() + 1);
    view! { <div /> }
}

#[component]
fn Parent() -> Element {
    view! { <div><Child /></div> }
}

#[test]
fn a_nested_components_write_is_attributed_to_itself_not_its_parent() {
    let (captured, reset) = capture_traces();
    let (scope, _dirty) = ComponentScope::new();

    scope.render(|| view! { <Parent /> });

    let traces = captured.borrow();
    assert!(traces.iter().any(|t| t.component == Some("Child")));
    assert!(
        !traces.iter().any(|t| t.component == Some("Parent")),
        "Parent never itself calls Signal::set, so it must never appear as a trace source"
    );
    drop(traces);
    reset();
}

type PendingFetch =
    Rc<RefCell<Option<florui::reactive::testing::ManualFuture<Result<i32, String>>>>>;

#[component]
fn Fetcher(future_slot: PendingFetch) -> Element {
    let _resource = use_resource("key", move |_| {
        future_slot
            .borrow_mut()
            .take()
            .expect("the fetch only runs once for an unchanged key")
    });
    view! { <div /> }
}

#[test]
fn an_async_resource_completion_is_attributed_to_the_component_that_started_it() {
    let (captured, reset) = capture_traces();
    let executor = Rc::new(LocalExecutor::new());
    let (future, resolver) = manual_future::<Result<i32, String>>();
    let future_slot: PendingFetch = Rc::new(RefCell::new(Some(future)));
    let (scope, _dirty) = ComponentScope::new();

    let executor_for_render = Rc::clone(&executor);
    scope.render(move || {
        provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
        view! { <Fetcher future_slot={future_slot} /> }
    });
    executor.run_until_stalled();

    // Only the trace from the completion below matters here — the render
    // above already produced its own (Pending) trace attributed to
    // Fetcher, which isn't what this test is trying to isolate.
    captured.borrow_mut().clear();

    resolver.resolve(Ok(42));
    // This commits Resource::Ready from inside the spawned task, outside
    // any component's render call entirely.
    executor.run_until_stalled();

    assert!(
        captured
            .borrow()
            .iter()
            .any(|t| t.component == Some("Fetcher")),
        "a completion committed well after Fetcher's render must still be attributed to it"
    );
    reset();
}

#[test]
fn a_resource_records_its_start_and_completion_with_the_writes_each_made() {
    let (captured, reset) = capture_traces();
    let executor = Rc::new(LocalExecutor::new());
    let (future, resolver) = manual_future::<Result<i32, String>>();
    let future_slot: PendingFetch = Rc::new(RefCell::new(Some(future)));
    let (scope, _dirty) = ComponentScope::new();

    let executor_for_render = Rc::clone(&executor);
    scope.render(move || {
        provide_context(Rc::clone(&executor_for_render) as Rc<dyn Executor>);
        view! { <Fetcher future_slot={future_slot} /> }
    });
    executor.run_until_stalled();
    resolver.resolve(Ok(7));
    executor.run_until_stalled();

    let steps: Vec<(TraceKind, Origin)> = captured
        .borrow()
        .iter()
        .filter(|t| t.component == Some("Fetcher"))
        .map(|t| (t.kind, t.origin))
        .collect();
    assert!(
        steps.contains(&(TraceKind::ResourceStarted, Origin::Code)),
        "{steps:?}"
    );
    assert!(
        steps.contains(&(TraceKind::SignalWrite, Origin::ResourceStart)),
        "{steps:?}"
    );
    assert!(
        steps.contains(&(TraceKind::ResourceCompleted, Origin::Code)),
        "{steps:?}"
    );
    assert!(
        steps.contains(&(TraceKind::SignalWrite, Origin::ResourceCompletion)),
        "{steps:?}"
    );
    reset();
}
