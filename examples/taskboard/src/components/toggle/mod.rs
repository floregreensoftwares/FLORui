//! A switch with a brass thumb in a recessed track, dressed over the
//! framework's own `Switch`: a real checkbox with the switch role, so Tab,
//! Space and the screen reader come with it. It does not own its state: it
//! shows `on` and asks for the opposite when pressed.

use florui::prelude::*;
use florui_platform::{Switch, SwitchProps, SwitchValue};

stylesheet!("./toggle.css");

#[component]
pub fn Toggle(id: String, on: Binding<bool>, label: String) -> Element {
    view! {
        <div class="toggle">
            <Switch
                id={id}
                value={SwitchValue::bound(on)}
                disabled={false}
                indeterminate={false}
                accessible_label={label}
            />
        </div>
    }
}
