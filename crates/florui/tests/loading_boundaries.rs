//! `loading_boundary` through the real `#[component]`/`view!` macro
//! surface — see `florui_reactive::loading_boundary`.

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;
use florui::reactive::executor::LocalExecutor;
use florui::reactive::testing::{ManualFuture, manual_future};

type PendingFetch = Rc<RefCell<Option<ManualFuture<Result<i32, String>>>>>;
type Log = Rc<RefCell<Vec<String>>>;

#[component]
fn Content(log: Log, refreshing: bool) -> Element {
    log.borrow_mut().push(if refreshing {
        "content-refreshing".to_string()
    } else {
        "content".to_string()
    });
    view! { <div /> }
}

#[component]
fn Fallback(log: Log) -> Element {
    log.borrow_mut().push("fallback".to_string());
    view! { <div /> }
}

#[component]
fn Page(future_slot: PendingFetch, log: Log) -> Element {
    let resource = use_resource("key", move |_| {
        future_slot
            .borrow_mut()
            .take()
            .expect("the fetch only runs once for an unchanged key")
    });
    loading_boundary(
        &[&resource],
        {
            let log = Rc::clone(&log);
            move |refreshing| view! { <Content log={log} refreshing={refreshing} /> }
        },
        move || view! { <Fallback log={log} /> },
    )
}

#[test]
fn reveals_content_through_the_macro_surface_once_the_resource_settles() {
    let executor = Rc::new(LocalExecutor::new());
    let (future, resolver) = manual_future::<Result<i32, String>>();
    let future_slot: PendingFetch = Rc::new(RefCell::new(Some(future)));
    let log: Log = Rc::new(RefCell::new(Vec::new()));
    let (scope, _dirty) = ComponentScope::new();

    let render = |executor: Rc<LocalExecutor>, future_slot: PendingFetch, log: Log| {
        move || {
            provide_context(Rc::clone(&executor) as Rc<dyn Executor>);
            view! { <Page future_slot={future_slot} log={log} /> }
        }
    };

    scope.render(render(
        Rc::clone(&executor),
        Rc::clone(&future_slot),
        Rc::clone(&log),
    ));
    assert_eq!(*log.borrow(), vec!["fallback"]);
    executor.run_until_stalled();

    resolver.resolve(Ok(42));
    executor.run_until_stalled();

    log.borrow_mut().clear();
    scope.render(render(
        Rc::clone(&executor),
        Rc::clone(&future_slot),
        Rc::clone(&log),
    ));
    assert_eq!(*log.borrow(), vec!["content"]);
}
