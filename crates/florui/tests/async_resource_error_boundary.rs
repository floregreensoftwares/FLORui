//! `use_resource` and `error_boundary` composed together through the real
//! `#[component]`/`view!` macro surface: an async failure explicitly
//! reported to the nearest boundary, the fallback it renders, cleanup of
//! the failed subtree, and a reset that restarts the resource fresh: route event or async failures explicitly, and
//! "on failure, dispose the replaced subtree once... mount the fallback."

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;
use florui::reactive::executor::LocalExecutor;
use florui::reactive::testing::{Resolver, manual_future};

type PendingFetches = Rc<RefCell<Vec<Resolver<Result<i32, String>>>>>;

#[component]
fn Content(fetches: PendingFetches, log: Rc<RefCell<Vec<String>>>) -> Element {
    use_effect((), {
        let log = Rc::clone(&log);
        move || {
            log.borrow_mut().push("mount".to_string());
            Some(Box::new(move || log.borrow_mut().push("unmount".to_string())) as Cleanup)
        }
    });

    let resource = use_resource("key", move |_| {
        let (future, resolver) = manual_future::<Result<i32, String>>();
        fetches.borrow_mut().push(resolver);
        future
    });

    if let Some(error) = resource.get().error() {
        let reporter =
            use_context::<ErrorReporter<String>>().expect("a boundary above provided a reporter");
        reporter.report(error.clone());
    }

    view! { <div /> }
}

#[component]
fn Boundary(fetches: PendingFetches, log: Rc<RefCell<Vec<String>>>, retry: bool) -> Element {
    error_boundary::<String, _>(
        {
            let fetches = Rc::clone(&fetches);
            let log = Rc::clone(&log);
            move || view! { <Content fetches={fetches} log={log} /> }
        },
        move |error: String, boundary: &ErrorBoundary<String>| {
            log.borrow_mut().push(format!("fallback:{error}"));
            if retry {
                boundary.reset();
            }
            view! { <div /> }
        },
    )
}

#[test]
fn an_async_failure_reaches_the_boundary_disposes_the_subtree_and_resets_to_a_fresh_fetch() {
    let executor = Rc::new(LocalExecutor::new());
    let fetches: PendingFetches = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::new(RefCell::new(Vec::new()));
    let (scope, _dirty) = ComponentScope::new();

    let render = |fetches: PendingFetches, log: Rc<RefCell<Vec<String>>>, retry: bool| {
        let executor = Rc::clone(&executor);
        move || {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            view! { <Boundary fetches={fetches} log={log} retry={retry} /> }
        }
    };

    // Render 1: mounts, starts the fetch.
    scope.render(render(Rc::clone(&fetches), Rc::clone(&log), false));
    executor.run_until_stalled();
    assert_eq!(*log.borrow(), vec!["mount"]);

    // The fetch fails; the executor commits it, but nothing reads it until
    // the next render.
    fetches
        .borrow_mut()
        .remove(0)
        .resolve(Err("network unreachable".to_string()));
    executor.run_until_stalled();

    // Render 2: Content sees the failure and reports it — but a report
    // made *during* a render only takes effect starting the next one, so
    // this render still shows Content, not the fallback.
    log.borrow_mut().clear();
    scope.render(render(Rc::clone(&fetches), Rc::clone(&log), false));
    assert_eq!(
        *log.borrow(),
        Vec::<String>::new(),
        "the same key/generation must not remount Content just because it reported an error"
    );

    // Render 3: the boundary now sees the reported error and renders the
    // fallback instead — disposing Content's subtree exactly once.
    log.borrow_mut().clear();
    scope.render(render(Rc::clone(&fetches), Rc::clone(&log), false));
    assert_eq!(
        *log.borrow(),
        vec!["fallback:network unreachable", "unmount"],
        "switching to the fallback must render it with the reported error, and dispose \
         Content's subtree once it's no longer part of this render"
    );

    // Render 4: the fallback calls boundary.reset() — takes effect next
    // render, same rule as the error report above.
    log.borrow_mut().clear();
    scope.render(render(Rc::clone(&fetches), Rc::clone(&log), true));
    assert_eq!(*log.borrow(), vec!["fallback:network unreachable"]);

    // Render 5: reset cleared the error and advanced the boundary's
    // generation, so Content mounts fresh — a brand new resource, not the
    // disposed one's leftover state — and starts a brand new fetch.
    log.borrow_mut().clear();
    scope.render(render(Rc::clone(&fetches), Rc::clone(&log), false));
    executor.run_until_stalled();
    assert_eq!(
        *log.borrow(),
        vec!["mount"],
        "reset must remount Content fresh instead of reusing the failed generation's state"
    );
    assert_eq!(
        fetches.borrow().len(),
        1,
        "the fresh mount must have started its own new fetch"
    );

    // That new fetch succeeds, proving the restarted resource is live and
    // not somehow still wired to the disposed generation.
    fetches.borrow_mut().remove(0).resolve(Ok(7));
    executor.run_until_stalled();
    log.borrow_mut().clear();
    scope.render(render(Rc::clone(&fetches), Rc::clone(&log), false));
    assert_eq!(
        *log.borrow(),
        Vec::<String>::new(),
        "a successful resource never reports anything to the boundary"
    );
}
