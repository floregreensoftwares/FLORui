//! A stored callback for a declarative event-handler attribute
//! (`onclick={move || ...}`) on an [`crate::ElementNode`]. Also
//! [`ValueHandler`]: the same idea for the explicit (non-`Binding`)
//! controlled-value contract — see slots-and-bindings.md's "Optional
//! convenience and explicit control."

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// A click/activation event handed to an `on*` handler. Its only power is
/// [`Event::prevent_default`]: some primitives run a default action after
/// dispatch (e.g. `<a href>` following its target) unless the handler
/// cancels it here — real `<a>` semantics, not just a bare callback.
#[derive(Debug, Default)]
pub struct Event {
    default_prevented: Cell<bool>,
}

impl Event {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels this node's default action, if it has one.
    pub fn prevent_default(&self) {
        self.default_prevented.set(true);
    }

    /// Whether [`Event::prevent_default`] was called.
    pub fn default_prevented(&self) -> bool {
        self.default_prevented.get()
    }
}

#[derive(Clone)]
enum Callback {
    /// The common case: a handler that ignores the event entirely.
    Plain(Rc<dyn Fn()>),
    /// A handler that reads the event (e.g. to call
    /// [`Event::prevent_default`]).
    WithEvent(Rc<dyn Fn(&Event)>),
}

/// A callback captured from an `on*` attribute in `view!`, carried on the
/// `Element` tree so a host can look it up (by node and event name) after
/// a real input event and call it. Two constructors, not one generic
/// `Fn()`-or-`Fn(&Event)` bound: Rust's coherence rules don't allow a
/// single trait to be blanket-implemented for both closure arities at
/// once, and `view!`'s codegen picks between them by inspecting the
/// closure literal's own parameter count (see `florui-macros`), so every
/// existing zero-arg `onclick={move || ...}` in the codebase keeps
/// compiling unchanged.
#[derive(Clone)]
pub struct Handler(Callback);

impl Handler {
    pub fn new(f: impl Fn() + 'static) -> Self {
        Self(Callback::Plain(Rc::new(f)))
    }

    /// A handler that reads the [`Event`] it's called with (e.g. to call
    /// [`Event::prevent_default`]) rather than ignoring it.
    pub fn with_event(f: impl Fn(&Event) + 'static) -> Self {
        Self(Callback::WithEvent(Rc::new(f)))
    }

    /// Runs the callback with the given event.
    pub fn call(&self, event: &Event) {
        match &self.0 {
            Callback::Plain(f) => f(),
            Callback::WithEvent(f) => f(event),
        }
    }
}

impl fmt::Debug for Handler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Handler(..)")
    }
}

impl PartialEq for Handler {
    /// Equal only if they share the same underlying callback — comparing
    /// behavior isn't possible, so this is identity, not content, equality.
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Callback::Plain(a), Callback::Plain(b)) => Rc::ptr_eq(a, b),
            (Callback::WithEvent(a), Callback::WithEvent(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Handler {}

/// A callback captured from a value-change attribute (`oninput={...}` on
/// an editable primitive using the explicit controlled-value contract) —
/// like [`Handler`], but the control reports what the new value *is*
/// rather than nothing. This is the other half of the mutual-exclusivity
/// rule with a `Binding`: `view!`'s own codegen only ever emits one or
/// the other for a given `value` attribute, never both — see
/// `florui-macros`'s own `primitive_element`.
///
/// Real `Binding<String>` combined with `oninput` is rejected at compile
/// time, not silently resolved — `oninput`'s presence makes codegen treat
/// `value` as a plain value (calling `.to_string()` on it), and
/// `Binding<String>` has no such method:
///
/// ```compile_fail
/// use florui::prelude::*;
///
/// fn ambiguous_contract(binding: Binding<String>) -> Element {
///     // `binding` is a real `Binding`, not a plain value — `oninput`'s
///     // presence forces the explicit contract's `.to_string()` call,
///     // which `Binding<String>` does not implement. Must not compile.
///     view! { <input type="text" value={binding} oninput={|_: String| ()} /> }
/// }
/// ```
#[derive(Clone)]
pub struct ValueHandler(Rc<dyn Fn(String)>);

impl ValueHandler {
    pub fn new(f: impl Fn(String) + 'static) -> Self {
        Self(Rc::new(f))
    }

    /// Reports that the value changed to `value`.
    pub fn call(&self, value: String) {
        (self.0)(value);
    }
}

impl fmt::Debug for ValueHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ValueHandler(..)")
    }
}

impl PartialEq for ValueHandler {
    /// Same "identity, not content" reasoning as [`Handler`]'s own.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for ValueHandler {}

/// [`ValueHandler`] for a boolean control (a switch, a checkbox): reports
/// the value the control is *asking* for, so its owner can accept or
/// reject it. The explicit-contract counterpart of a `Binding<bool>`.
#[derive(Clone)]
pub struct BoolHandler(Rc<dyn Fn(bool)>);

impl BoolHandler {
    pub fn new(f: impl Fn(bool) + 'static) -> Self {
        Self(Rc::new(f))
    }

    /// Reports that the control asks to become `value`.
    pub fn call(&self, value: bool) {
        (self.0)(value);
    }
}

impl fmt::Debug for BoolHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BoolHandler(..)")
    }
}

impl PartialEq for BoolHandler {
    /// Same "identity, not content" reasoning as [`Handler`]'s own.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for BoolHandler {}

/// [`ValueHandler`] for a continuous numeric control (a slider): reports
/// the value the control is asking for, so its owner can accept, reject
/// or clamp it. The explicit-contract counterpart of a `Binding<f32>`.
#[derive(Clone)]
pub struct FloatHandler(Rc<dyn Fn(f32)>);

impl FloatHandler {
    pub fn new(f: impl Fn(f32) + 'static) -> Self {
        Self(Rc::new(f))
    }

    /// Reports that the control asks to become `value`.
    pub fn call(&self, value: f32) {
        (self.0)(value);
    }
}

impl fmt::Debug for FloatHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FloatHandler(..)")
    }
}

impl PartialEq for FloatHandler {
    /// Same "identity, not content" reasoning as [`Handler`]'s own.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for FloatHandler {}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[test]
    fn call_runs_the_stored_closure() {
        let called = Rc::new(Cell::new(false));
        let called_in_closure = called.clone();
        let handler = Handler::new(move || called_in_closure.set(true));
        handler.call(&Event::new());
        assert!(called.get());
    }

    #[test]
    fn clones_share_the_same_callback() {
        let calls = Rc::new(Cell::new(0));
        let calls_in_closure = calls.clone();
        let handler = Handler::new(move || calls_in_closure.set(calls_in_closure.get() + 1));
        let clone = handler.clone();
        handler.call(&Event::new());
        clone.call(&Event::new());
        assert_eq!(calls.get(), 2);
        assert_eq!(handler, clone);
    }

    #[test]
    fn independently_constructed_handlers_are_not_equal() {
        let a = Handler::new(|| ());
        let b = Handler::new(|| ());
        assert_ne!(a, b);
    }

    #[test]
    fn with_event_handler_receives_the_event() {
        let prevented = Rc::new(Cell::new(false));
        let prevented_in_closure = prevented.clone();
        let handler = Handler::with_event(move |event| {
            event.prevent_default();
            prevented_in_closure.set(event.default_prevented());
        });
        let event = Event::new();
        handler.call(&event);
        assert!(prevented.get());
        assert!(event.default_prevented());
    }

    #[test]
    fn plain_handler_leaves_the_event_unprevented() {
        let handler = Handler::new(|| ());
        let event = Event::new();
        handler.call(&event);
        assert!(!event.default_prevented());
    }

    #[test]
    fn plain_and_with_event_handlers_are_never_equal() {
        let plain = Handler::new(|| ());
        let with_event = Handler::with_event(|_: &Event| ());
        assert_ne!(plain, with_event);
    }

    #[test]
    fn value_handler_call_passes_the_new_value_through() {
        let received = Rc::new(RefCell::new(None));
        let received_in_closure = Rc::clone(&received);
        let handler =
            ValueHandler::new(move |value| *received_in_closure.borrow_mut() = Some(value));
        handler.call("hello".to_string());
        assert_eq!(*received.borrow(), Some("hello".to_string()));
    }

    #[test]
    fn bool_handler_call_passes_the_requested_value_through() {
        let received = Rc::new(Cell::new(None));
        let received_in_closure = Rc::clone(&received);
        let handler = BoolHandler::new(move |value| received_in_closure.set(Some(value)));
        handler.call(true);
        assert_eq!(received.get(), Some(true));
        let clone = handler.clone();
        assert_eq!(handler, clone);
        assert_ne!(handler, BoolHandler::new(|_| ()));
    }

    #[test]
    fn float_handler_call_passes_the_requested_value_through() {
        let received = Rc::new(Cell::new(None));
        let received_in_closure = Rc::clone(&received);
        let handler = FloatHandler::new(move |value| received_in_closure.set(Some(value)));
        handler.call(2.5);
        assert_eq!(received.get(), Some(2.5));
        let clone = handler.clone();
        assert_eq!(handler, clone);
        assert_ne!(handler, FloatHandler::new(|_| ()));
    }

    #[test]
    fn independently_constructed_value_handlers_are_not_equal() {
        let a = ValueHandler::new(|_| ());
        let b = ValueHandler::new(|_| ());
        assert_ne!(a, b);
    }
}
