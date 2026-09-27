//! `onclick`-style attributes: real callbacks carried on the `Element`
//! tree, not just another string attribute.

use std::cell::Cell;
use std::rc::Rc;

use florui::prelude::*;

#[test]
fn an_event_handler_attribute_is_not_a_string_attr() {
    let el: Element = view! { <button onclick={|| ()}>{"Go"}</button> };
    let Element::Node(node) = &el else {
        panic!("expected a node");
    };
    assert!(
        node.attrs.is_empty(),
        "onclick must not show up as a plain string attribute"
    );
    assert_eq!(node.handlers.len(), 1);
    assert_eq!(node.handlers[0].0, "click");
}

#[test]
fn calling_the_stored_handler_runs_the_original_closure() {
    let clicked = Rc::new(Cell::new(false));
    let clicked_in_handler = clicked.clone();
    let el: Element = view! { <button onclick={move || clicked_in_handler.set(true)} /> };

    let Element::Node(node) = &el else {
        panic!("expected a node");
    };
    let (_name, handler) = &node.handlers[0];
    handler.call(&Event::new());

    assert!(clicked.get());
}

#[test]
fn a_one_arg_onclick_closure_receives_the_event_and_can_prevent_default() {
    let el: Element = view! {
        <button onclick={|event: &Event| event.prevent_default()}>{"Go"}</button>
    };
    let Element::Node(node) = &el else {
        panic!("expected a node");
    };
    let (_name, handler) = &node.handlers[0];
    let event = Event::new();
    handler.call(&event);

    assert!(event.default_prevented());
}

#[test]
fn a_signal_set_from_an_onclick_handler_persists_across_a_real_render_cycle() {
    let (scope, _dirty) = ComponentScope::new();

    #[component]
    fn Counter() -> Element {
        let count = use_signal(|| 0);
        let clicked = count.clone();
        view! {
            <button onclick={move || clicked.set(clicked.get() + 1)}>
                {count.get().to_string()}
            </button>
        }
    }

    let first = scope.render(|| Counter(CounterProps {}));
    let Element::Node(node) = &first else {
        panic!("expected a node");
    };
    let handler = node.handlers[0].1.clone();
    handler.call(&Event::new());

    let second = scope.render(|| Counter(CounterProps {}));
    let Element::Node(node) = &second else {
        panic!("expected a node");
    };
    assert_eq!(node.children, vec![Element::text("1")]);
}
