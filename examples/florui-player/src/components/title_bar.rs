//! The app's own drawn title bar -- mirrors
//! `examples/florui-example-app/examples/custom_titlebar.rs`'s
//! drag-region/caption-button pattern, trimmed to just what this player
//! needs (no tooltips): a drag region, a skin-cycle button, and the
//! standard minimize/maximize/close trio.

use std::rc::Rc;

use florui::prelude::*;
use florui_platform::{WINDOW_DRAG_REGION_ID, WindowControls};

use crate::theme::Skin;

#[component]
pub fn TitleBar(
    focused: bool,
    skin: Skin,
    on_cycle_skin: Handler,
    controls: Option<Rc<WindowControls>>,
) -> Element {
    let minimize = controls.clone();
    let toggle_maximize = controls.clone();
    let close = controls;

    view! {
        <div class={if focused { "titlebar" } else { "titlebar inactive" }}>
            <span class="titlebar-title">{"florui-player"}</span>
            <div id={WINDOW_DRAG_REGION_ID} class="drag-region" />
            <button class="skin-button" onclick={move || on_cycle_skin.call(&Event::new())}>
                {skin.label()}
            </button>
            <div class="window-buttons">
                <button
                    class="window-button"
                    accessible_label="Minimize"
                    onclick={move || if let Some(c) = &minimize { c.minimize(); }}
                >
                    {"_"}
                </button>
                <button
                    class="window-button"
                    accessible_label="Maximize"
                    onclick={move || if let Some(c) = &toggle_maximize { c.toggle_maximize(); }}
                >
                    {"[]"}
                </button>
                <button
                    class="window-button close-button"
                    accessible_label="Close"
                    onclick={move || if let Some(c) = &close { c.close(); }}
                >
                    {"x"}
                </button>
            </div>
        </div>
    }
}
