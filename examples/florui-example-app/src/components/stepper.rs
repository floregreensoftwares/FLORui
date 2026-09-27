//! A genuinely *controlled* component: `Stepper` never owns the value it
//! shows. It receives a [`Binding<i32>`], reads it for display, and asks
//! for a change through it — the owner (whoever calls [`Stepper`]) decides
//! whether that change actually happens, the same way a real text input
//! bound to validated state would.

use florui::prelude::*;

stylesheet!("./stepper.css");

#[component]
pub fn Stepper(value: Binding<i32>) -> Element {
    let current = value.get();
    let decrement = value.clone();
    let increment = value.clone();

    view! {
        <div class="stepper">
            <button
                class="decrement"
                onclick={move || decrement.request_update(decrement.get() - 1)}
            >
                {"-1"}
            </button>
            <span class="value">{current.to_string()}</span>
            <button
                class="increment"
                onclick={move || increment.request_update(increment.get() + 1)}
            >
                {"+1"}
            </button>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use florui_reactive::{ComponentScope, use_signal};
    use florui_style::Arena;

    use super::*;

    fn text_of_class(arena: &Arena, class: &str) -> String {
        let node = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap_or_else(|| panic!("no node with class {class:?}"));
        arena.text_content(node).to_string()
    }

    fn click(arena: &Arena, class: &str) {
        let node = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap_or_else(|| panic!("no node with class {class:?}"));
        arena.handler(node, "click").unwrap().call(&Event::new());
    }

    /// The owner clamps to `min..=max`, rejecting anything outside it —
    /// proving `Stepper` is driven by the caller's own validation, not
    /// just `Signal::binding()`'s default always-accept adapter.
    ///
    /// Each call derives a *fresh* `Binding` from the owner's current
    /// value, same as a real re-render would; `Binding::get()` is a
    /// snapshot (see `binding.rs`), so two `click`s against the *same*
    /// returned `Arena` both read the value as of that one render, not a
    /// live count — a caller that wants each click to see the effect of
    /// the last one must call `render` again in between, exactly like the
    /// real desktop host re-renders on every dirty mark.
    fn render(scope: &ComponentScope, initial: i32, min: i32, max: i32) -> Arena {
        let tree = scope.render(|| {
            let count = use_signal(|| initial);
            let value = Binding::new(count.get(), move |requested: i32| {
                if (min..=max).contains(&requested) {
                    count.set(requested);
                }
            });
            Stepper(StepperProps { value })
        });
        Arena::build(&tree)
    }

    #[test]
    fn starts_at_the_owners_initial_value_and_survives_a_render_with_no_click() {
        let (scope, _dirty) = ComponentScope::new();
        assert_eq!(text_of_class(&render(&scope, 0, 0, 10), "value"), "0");
        assert_eq!(
            text_of_class(&render(&scope, 0, 0, 10), "value"),
            "0",
            "a render with no click must not reset the owner's value"
        );
    }

    #[test]
    fn incrementing_requests_and_the_owner_accepts_within_range() {
        let (scope, _dirty) = ComponentScope::new();
        let arena = render(&scope, 0, 0, 10);
        click(&arena, "increment");
        let arena = render(&scope, 0, 0, 10);
        assert_eq!(text_of_class(&arena, "value"), "1");

        click(&arena, "increment");
        let arena = render(&scope, 0, 0, 10);
        assert_eq!(text_of_class(&arena, "value"), "2");
    }

    #[test]
    fn decrementing_requests_and_the_owner_accepts_within_range() {
        let (scope, _dirty) = ComponentScope::new();
        let arena = render(&scope, 0, 0, 10);
        click(&arena, "increment");
        let arena = render(&scope, 0, 0, 10);
        click(&arena, "increment");
        let arena = render(&scope, 0, 0, 10);
        assert_eq!(text_of_class(&arena, "value"), "2");

        click(&arena, "decrement");
        let arena = render(&scope, 0, 0, 10);
        assert_eq!(text_of_class(&arena, "value"), "1");
    }

    #[test]
    fn the_owner_rejects_a_request_below_its_declared_minimum() {
        let (scope, _dirty) = ComponentScope::new();
        let arena = render(&scope, 0, 0, 10);
        click(&arena, "decrement"); // would go to -1, below the minimum
        let arena = render(&scope, 0, 0, 10);
        assert_eq!(
            text_of_class(&arena, "value"),
            "0",
            "a Binding built with real validation must actually reject an out-of-range \
             request, not just the default always-accept adapter"
        );
    }

    #[test]
    fn the_owner_rejects_a_request_above_its_declared_maximum() {
        let (scope, _dirty) = ComponentScope::new();
        let arena = render(&scope, 10, 0, 10);
        click(&arena, "increment"); // would go to 11, above the maximum
        let arena = render(&scope, 10, 0, 10);
        assert_eq!(
            text_of_class(&arena, "value"),
            "10",
            "a Binding built with real validation must actually reject an out-of-range \
             request, not just the default always-accept adapter"
        );
    }
}
