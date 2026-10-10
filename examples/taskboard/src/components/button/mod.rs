//! A key: a raised cap that sinks when pressed. `accent` makes it a brass one,
//! for the single thing a form is for.

use florui::prelude::*;

stylesheet!("./button.css");

#[component]
pub fn KeyButton(label: String, on_press: Handler, accent: bool) -> Element {
    view! {
        <button
            class={if accent { "key accent" } else { "key" }}
            onclick={move |event: &Event| on_press.call(event)}
        >
            {label}
        </button>
    }
}
