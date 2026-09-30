//! A menu, a text field and a scrolling list, for checking that the inspector
//! does not change how an application behaves.
//!
//! With no argument the window is paired with the inspector; with `--plain`
//! it runs on the desktop host alone. `scripts/real-session/inspector.ps1`
//! drives both the same way and compares what they did.
//!
//! `cargo run --example inspector_menu -p florui-example-app [-- --plain]`

use florui::prelude::*;
use florui_platform::{Align, Placement, Popover, PopoverProps, Side, use_scroll_offset};
use florui_reactive::use_signal;
use florui_style::Rgba;

const CSS: &str = include_str!("inspector_menu.css");

fn main() {
    let canvas = Rgba::opaque(0x10, 0x10, 0x14);
    let result = if std::env::args().any(|arg| arg == "--plain") {
        florui_platform::run("Florui inspector menu", CSS, canvas, app)
            .map_err(|error| error.to_string())
    } else {
        florui_devtools::live::run("Florui inspector menu", CSS, canvas, app)
            .map_err(|error| error.to_string())
    };
    result.expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let menu_open = use_signal(|| false);
    let (toggle, dismiss, pick) = (menu_open.clone(), menu_open.clone(), menu_open.clone());
    let picked = use_signal(|| "none".to_string());
    let (pick_one, pick_two) = (picked.clone(), picked.clone());
    let typed = use_signal(String::new);
    let typed_in = typed.clone();
    let list = use_scroll_offset("list", |_, _| {});
    let (_, scrolled) = list.offset();

    view! {
        <div class="page">
            <button class="action" onclick={move || toggle.set(!toggle.get())}>
                {"Open menu"}
            </button>
            <Popover
                id={"menu".to_string()}
                open={menu_open.get()}
                placement={Placement::new(Side::Bottom, Align::Start)}
                ondismiss={Handler::new(move || dismiss.set(false))}
                trigger={view! { <span class="anchor">{"menu anchor"}</span> }}
            >
                <div class="menu" role="menu" accessible_label="Actions">
                    <button
                        class="menu-item"
                        role="menuitem"
                        onclick={move || { pick_one.set("one".to_string()); pick.set(false); }}
                    >
                        {"Item one"}
                    </button>
                    <button
                        class="menu-item"
                        role="menuitem"
                        onclick={move || pick_two.set("two".to_string())}
                    >
                        {"Item two"}
                    </button>
                </div>
            </Popover>
            <p class="status">{format!("picked: {}", picked.get())}</p>
            <label class="label" for="name">{"Name"}</label>
            <input
                id="name"
                class="field"
                type="text"
                required="true"
                value={typed.get()}
                oninput={move |value: String| typed_in.set(value)}
            />
            <p class="status">{format!("typed: {}", typed.get())}</p>
            <p class="status">{format!("scrolled: {:.0}", scrolled)}</p>
            <div id="list" class="list">
                {(0..40).map(|i| view! { <p class="row">{format!("Row {i}")}</p> }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}
