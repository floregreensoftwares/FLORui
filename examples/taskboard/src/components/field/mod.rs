//! A line of text to write in: a recess in the leather with a small label
//! above it. It shows `value` and asks for the text the person typed.

use florui::prelude::*;

stylesheet!("./field.css");

#[component]
pub fn Field(value: Binding<String>, label: String, id: String) -> Element {
    view! {
        <div class="field">
            <label class="label" for={id.clone()}>{label}</label>
            <input id={id} class="field-input well" type="text" value={value} />
        </div>
    }
}
