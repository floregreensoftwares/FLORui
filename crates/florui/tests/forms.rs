//! `onsubmit` on `<form>`: a real `SubmitHandler` carried on the tree,
//! not another string attribute or click handler.

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;

#[test]
fn onsubmit_is_a_submit_handler_not_a_string_attr_or_click_handler() {
    let el: Element = view! { <form onsubmit={|_data: FormData| ()}></form> };
    let Element::Node(node) = &el else {
        panic!("expected a node");
    };
    assert!(node.attrs.is_empty());
    assert!(node.handlers.is_empty());
    assert_eq!(node.submit_handlers.len(), 1);
    assert_eq!(node.submit_handlers[0].0, "submit");
}

#[test]
fn the_stored_handler_receives_the_form_data_it_is_called_with() {
    let seen = Rc::new(RefCell::new(None));
    let sink = seen.clone();
    let el: Element = view! {
        <form onsubmit={move |data: FormData| *sink.borrow_mut() = Some(data)}></form>
    };
    let Element::Node(node) = &el else {
        panic!("expected a node");
    };
    node.submit_handlers[0]
        .1
        .call(FormData::new(vec![("name".into(), "Ada".into())]));
    assert_eq!(
        seen.borrow().as_ref().and_then(|d| d.get("name")),
        Some("Ada")
    );
}

#[test]
fn onsubmit_composes_with_other_handlers_on_the_same_form() {
    let el: Element = view! {
        <form onsubmit={|_data: FormData| ()} onreset={|| ()} onclick={|| ()}></form>
    };
    let Element::Node(node) = &el else {
        panic!("expected a node");
    };
    assert_eq!(node.submit_handlers.len(), 1);
    let names: Vec<&str> = node.handlers.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["reset", "click"]);
}
