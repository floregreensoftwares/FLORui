use florui::prelude::*;

use crate::features::settings::{SettingsPanel, SettingsPanelProps};

pub fn page() -> Element {
    view! {
        <div class="page">
            <h1 class="title">{"Settings"}</h1>
            <div class="columns"><div class="column"><SettingsPanel /></div></div>
        </div>
    }
}
