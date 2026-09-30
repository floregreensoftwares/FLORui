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
