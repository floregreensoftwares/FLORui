//! Named/optional slots, scoped slots, and bindings through the real
//! `#[component]`/`view!` macro surface.
//!
//! Required-slot compile-time enforcement isn't tested here (a
//! `compile_fail` case belongs in a doctest, not a runtime test) — see
//! the example on `florui::component`'s re-export.

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;

type Log = Rc<RefCell<Vec<String>>>;

// ---- Named and optional slots ----

#[component]
fn Dialog(header: Element, body: Element, footer: Option<Element>) -> Element {
    let footer = footer.unwrap_or_else(|| view! { <div class="footer-default">{"default"}</div> });
    view! {
        <div class="dialog">
            <div class="header">{header}</div>
            <div class="body">{body}</div>
            {footer}
        </div>
    }
}

#[test]
fn named_slots_place_content_where_declared_with_no_extra_wrapper() {
    let (scope, _dirty) = ComponentScope::new();
    let tree = scope.render(|| {
        view! {
            <Dialog
                header={view! { <h1>{"Title"}</h1> }}
                body={view! { <p>{"Body text"}</p> }}
                footer={None}
            />
        }
    });

    let expected = view! {
        <div class="dialog">
            <div class="header"><h1>{"Title"}</h1></div>
            <div class="body"><p>{"Body text"}</p></div>
            <div class="footer-default">{"default"}</div>
        </div>
    };
    assert_eq!(
        tree, expected,
        "a slot's content must land exactly where the component places it, with no wrapper \
         Dialog didn't itself add"
    );
}

#[test]
fn an_explicitly_provided_optional_slot_overrides_the_components_default() {
    let (scope, _dirty) = ComponentScope::new();
    let tree = scope.render(|| {
        view! {
            <Dialog
                header={view! { <h1>{"Title"}</h1> }}
                body={view! { <p>{"Body"}</p> }}
                footer={Some(view! { <div class="custom-footer">{"bye"}</div> })}
            />
        }
    });

    let expected = view! {
        <div class="dialog">
            <div class="header"><h1>{"Title"}</h1></div>
            <div class="body"><p>{"Body"}</p></div>
            <div class="custom-footer">{"bye"}</div>
        </div>
    };
    assert_eq!(tree, expected);
}

// ---- Scoped slots: a typed render callback, an ordinary boxed closure ----

type RenderItem = Box<dyn Fn(&str, Log) -> Element>;

#[component]
fn LoggedItem(id: String, log: Log) -> Element {
    use_effect((), {
        let log = Rc::clone(&log);
        let id = id.clone();
        move || {
            log.borrow_mut().push(format!("mount {id}"));
            Some(Box::new(move || log.borrow_mut().push(format!("unmount {id}"))) as Cleanup)
        }
    });
    view! { <div /> }
}

#[component]
fn ItemList(items: Vec<String>, log: Log, render: RenderItem) -> Element {
    let children: Vec<Element> = items
        .iter()
        .map(|item| (render)(item, log.clone()))
        .collect();
    view! { <div class="list">{children}</div> }
}

fn logged_item_renderer() -> RenderItem {
    Box::new(|item: &str, log: Log| {
        let id = item.to_string();
        view! { <LoggedItem key={id.clone()} id={id} log={log} /> }
    })
}

#[test]
fn a_scoped_slot_creates_distinct_keyed_identities_that_survive_reordering() {
    let log: Log = Rc::new(RefCell::new(Vec::new()));
    let (scope, _dirty) = ComponentScope::new();

    scope.render({
        let log = Rc::clone(&log);
        move || {
            view! {
                <ItemList
                    items={vec!["a".to_string(), "b".to_string()]}
                    log={log}
                    render={logged_item_renderer()}
                />
            }
        }
    });
    assert_eq!(*log.borrow(), vec!["mount a", "mount b"]);

    log.borrow_mut().clear();
    scope.render({
        let log = Rc::clone(&log);
        move || {
            view! {
                <ItemList
                    items={vec!["b".to_string(), "a".to_string()]}
                    log={log}
                    render={logged_item_renderer()}
                />
            }
        }
    });
    assert!(
        log.borrow().is_empty(),
        "reordering the items a scoped slot renders must not remount either one"
    );

    log.borrow_mut().clear();
    scope.render({
        let log = Rc::clone(&log);
        move || {
            view! {
                <ItemList items={vec!["b".to_string()]} log={log} render={logged_item_renderer()} />
            }
        }
    });
    assert_eq!(
        *log.borrow(),
        vec!["unmount a"],
        "an item no longer produced by the scoped slot must dispose its own identity"
    );
}

#[test]
fn a_scoped_slots_closure_captures_the_callers_own_lexical_values() {
    let log: Log = Rc::new(RefCell::new(Vec::new()));
    let (scope, _dirty) = ComponentScope::new();
    let prefix = "captured-".to_string();

    let render: RenderItem = Box::new(move |item: &str, log: Log| {
        let id = format!("{prefix}{item}");
        view! { <LoggedItem key={id.clone()} id={id} log={log} /> }
    });

    scope.render({
        let log = Rc::clone(&log);
        move || view! { <ItemList items={vec!["x".to_string()]} log={log} render={render} /> }
    });
    assert_eq!(*log.borrow(), vec!["mount captured-x"]);
}

// ---- Explicit bindings ----

#[component]
fn TextField(value: Binding<String>) -> Element {
    view! { <div>{value.get()}</div> }
}

#[test]
fn a_binding_reads_the_owners_value_and_writes_through_a_requested_update() {
    // A separate scope purely to own `name`'s storage — the main `scope`
    // below is reserved for the component tree itself, so the two don't
    // collide over the same positional hook slots.
    let (owner, _owner_dirty) = ComponentScope::new();
    let name = owner.render(|| use_signal(|| "Alice".to_string()));

    let (scope, _dirty) = ComponentScope::new();
    let tree = scope.render({
        let name = name.clone();
        move || view! { <TextField value={name.binding()} /> }
    });
    assert_eq!(tree, view! { <div>{"Alice"}</div> });

    // Stands in for a control asking its owner to accept an edit.
    name.binding().request_update("Bob".to_string());
    assert_eq!(name.get(), "Bob");

    let tree = scope.render(move || view! { <TextField value={name.binding()} /> });
    assert_eq!(tree, view! { <div>{"Bob"}</div> });
}

#[component]
fn ValidatedField(value: Binding<i32>) -> Element {
    view! { <div>{value.get().to_string()}</div> }
}

#[test]
fn a_validating_binding_rejects_an_update_and_the_control_reconciles_to_the_accepted_value() {
    // As above: a separate scope purely to own `accepted`'s storage.
    let (owner, _owner_dirty) = ComponentScope::new();
    let accepted = owner.render(|| use_signal(|| 10_i32));

    let (scope, _dirty) = ComponentScope::new();
    let binding = |accepted: Signal<i32>| {
        Binding::new(accepted.get(), move |requested: i32| {
            // Only accept non-negative values.
            if requested >= 0 {
                accepted.set(requested);
            }
        })
    };

    let tree = scope.render({
        let accepted = accepted.clone();
        move || view! { <ValidatedField value={binding(accepted)} /> }
    });
    assert_eq!(tree, view! { <div>{"10"}</div> });

    binding(accepted.clone()).request_update(-5); // rejected
    assert_eq!(
        accepted.get(),
        10,
        "a rejected update must not reach the owner's storage"
    );

    let tree = scope.render({
        let accepted = accepted.clone();
        move || view! { <ValidatedField value={binding(accepted)} /> }
    });
    assert_eq!(
        tree,
        view! { <div>{"10"}</div> },
        "the control must reconcile to the last accepted value, not the rejected one"
    );
}
