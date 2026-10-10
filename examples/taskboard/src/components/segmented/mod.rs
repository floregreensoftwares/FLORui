//! A row of keys in a recessed rail, one of them pressed in. Shows `selected`
//! and asks for another index when a key is pressed.

use florui::prelude::*;

stylesheet!("./segmented.css");

#[component]
pub fn Segmented(options: Vec<String>, selected: Binding<usize>) -> Element {
    let current = selected.get();
    let keys: Vec<Element> = options
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let choose = selected.clone();
            view! {
                <button
                    class={if index == current { "segment on" } else { "segment" }}
                    onclick={move || choose.request_update(index)}
                >
                    {text}
                </button>
            }
        })
        .collect();
    view! { <div class="segmented well">{keys}</div> }
}
