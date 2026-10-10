//! A plate on the desk: stitched leather in a brass rim, with a screw at each
//! corner and a small engraved title. What goes on it is its children.

use florui::prelude::*;

stylesheet!("./panel.css");

#[component]
pub fn Panel(title: String, children: Children) -> Element {
    view! {
        <section class="panel leather brass-rim">
            <div class="stitching"></div>
            <span class="screw tl"></span>
            <span class="screw tr"></span>
            <span class="screw bl"></span>
            <span class="screw br"></span>
            <header class="plate-title"><span class="label">{title}</span></header>
            <div class="plate-body">{children}</div>
        </section>
    }
}
