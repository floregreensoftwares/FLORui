//! A sheet of ruled paper with a punched hole and a margin line, holding a
//! title and a line under it, and whatever the caller puts at its right edge.

use florui::prelude::*;

stylesheet!("./paper_card.css");

#[component]
pub fn PaperCard(title: String, subtitle: String, selected: bool, children: Children) -> Element {
    view! {
        <div class={if selected { "paper-card paper selected" } else { "paper-card paper" }}>
            <div class="ruled">
                <span class="hole"></span>
                <div class="writing">
                    <span class="heading">{title}</span>
                    <span class="muted line">{subtitle}</span>
                </div>
                <div class="aside">{children}</div>
            </div>
        </div>
    }
}
