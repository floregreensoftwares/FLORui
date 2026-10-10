use florui::prelude::*;

use crate::features::overview::{NextUp, NextUpProps, Summary, SummaryProps};

pub fn page() -> Element {
    view! {
        <div class="page">
            <h1 class="title">{"On the desk"}</h1>
            <div class="columns">
                <div class="column narrow"><Summary /></div>
                <div class="column"><NextUp /></div>
            </div>
        </div>
    }
}
