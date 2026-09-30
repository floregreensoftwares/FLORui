//! A real anchor-tracked, non-modal `Popover` used as a keyboard menu, live:
//!
//! - "Open menu" opens a popover anchored below it with its first item
//!   focused. Up/Down move between items and wrap, Home/End jump to the
//!   ends, Enter activates, Escape closes and returns focus to the button,
//!   and Tab closes the menu and continues from the button.
//! - "Submenu" (inside the menu): Right opens it and enters it, Left closes
//!   it and returns to the row. Clicking inside it never closes the parent;
//!   only a click genuinely outside both does. Resize the window small and
//!   reopen near an edge to see the menu flip above the button or shift
//!   sideways, rather than running off the window.
//! - Clicking "elsewhere" (or anywhere else outside every open popover)
//!   dismisses whatever is open.
//! - Unlike `Dialog` (see the `portal` example), Tab/Shift+Tab are never
//!   trapped inside an open popover.
//!
//! `cargo run --example popover -p florui-example-app`

use florui::prelude::*;
use florui_platform::{Align, Placement, Popover, PopoverProps, Side};
use florui_reactive::use_signal;
use florui_style::Rgba;

const POPOVER_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/popover.css");

fn main() {
    florui_platform::run_with_css_reload(
        "Florui popover",
        POPOVER_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let menu_open = use_signal(|| false);
    let menu_toggle = menu_open.clone();
    let menu_dismiss = menu_open.clone();
    let item_one = menu_open.clone();
    let item_two = menu_open.clone();

    let submenu_open = use_signal(|| false);
    let submenu_toggle = submenu_open.clone();
    let submenu_dismiss = submenu_open.clone();
    let submenu_item = submenu_open.clone();

    view! {
        <div class="page">
            <p class="instructions">
                {"Open the menu, then use the arrow keys, Home and End. Right opens the \
                  submenu and Left closes it; Escape closes the menu and Tab leaves it. \
                  Clicking inside the submenu never closes the parent -- only a click \
                  genuinely outside both does. Resize the window small and reopen near an \
                  edge to see the menu flip/shift instead of running off it."}
            </p>
            <Popover
                id={"menu".to_string()}
                open={menu_open.get()}
                placement={Placement::new(Side::Bottom, Align::Start)}
                ondismiss={Handler::new(move || menu_dismiss.set(false))}
                trigger={view! {
                    <button
                        class="action"
                        onclick={move || menu_toggle.set(!menu_toggle.get())}
                    >
                        {"Open menu"}
                    </button>
                }}
            >
                <div class="menu" role="menu" accessible_label="Actions">
                    <button
                        class="menu-item"
                        role="menuitem"
                        onclick={move || item_one.set(false)}
                    >
                        {"Item one"}
                    </button>
                    <button
                        class="menu-item"
                        role="menuitem"
                        onclick={move || item_two.set(false)}
                    >
                        {"Item two"}
                    </button>
                    <Popover
                        id={"submenu".to_string()}
                        open={submenu_open.get()}
                        placement={Placement::new(Side::Right, Align::Start)}
                        ondismiss={Handler::new(move || submenu_dismiss.set(false))}
                        trigger={view! {
                            <button
                                class="menu-item"
                                role="menuitem"
                                onclick={move || submenu_toggle.set(!submenu_toggle.get())}
                            >
                                {"Submenu \u{25B8}"}
                            </button>
                        }}
                    >
                        <div class="menu" role="menu" accessible_label="More actions">
                            <button
                                class="menu-item"
                                role="menuitem"
                                onclick={move || submenu_item.set(false)}
                            >
                                {"Submenu item"}
                            </button>
                        </div>
                    </Popover>
                </div>
            </Popover>
            <p class="elsewhere">{"Click here (or anywhere outside) to dismiss"}</p>
        </div>
    }
}
