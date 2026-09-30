//! `use_attachment` through the real `#[component]`/`view!` macro
//! surface — see `florui_reactive::use_attachment`.

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;

type Log = Rc<RefCell<Vec<String>>>;

#[component]
fn Widget(log: Log) -> Element {
    use_attachment((), (), {
        let log = Rc::clone(&log);
        move |_handle| {
            log.borrow_mut().push("setup a".to_string());
            let log = Rc::clone(&log);
            Some(Box::new(move || log.borrow_mut().push("cleanup a".to_string())) as Cleanup)
        }
    });
    use_attachment((), (), {
        let log = Rc::clone(&log);
        move |_handle| {
            log.borrow_mut().push("setup b".to_string());
            let log = Rc::clone(&log);
            Some(Box::new(move || log.borrow_mut().push("cleanup b".to_string())) as Cleanup)
        }
    });
    view! { <div /> }
}

#[test]
fn attachments_set_up_in_order_and_clean_up_in_reverse_through_the_macro_surface() {
    let log: Log = Rc::new(RefCell::new(Vec::new()));
    let (scope, _dirty) = ComponentScope::new();

    scope.render({
        let log = Rc::clone(&log);
        move || view! { <Widget log={log} /> }
    });
    assert_eq!(*log.borrow(), vec!["setup a", "setup b"]);

    drop(scope);
    assert_eq!(
        *log.borrow(),
        vec!["setup a", "setup b", "cleanup b", "cleanup a"],
        "unmounting a real component must clean up its attachments in reverse declaration order"
    );
}
