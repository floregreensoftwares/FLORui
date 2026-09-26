//! A real, application-drawn title bar: the same freedom a Claude
//! Code-style custom title bar gives an Electron/Tauri app, where extra
//! app content (icon buttons) lives in the same row as the window's own
//! minimize/maximize/close. `DecorationMode::Custom` removes the OS's own
//! chrome entirely, so everything here — dragging the window, minimizing,
//! maximizing, closing — goes through `florui_platform::use_window_controls`
//! and `WINDOW_DRAG_REGION_ID` instead of coming for free the way it does
//! under system decorations.
//!
//! `cargo run --example custom_titlebar -p florui-example-app`

use florui::prelude::*;
use florui_platform::appearance::DecorationMode;
use florui_platform::{
    Align, Placement, Popover, PopoverProps, Side, WINDOW_DRAG_REGION_ID, WindowOptions,
    use_window_controls,
};
use florui_reactive::{Cleanup, use_effect, use_signal};
use florui_style::Rgba;

const CSS: &str = include_str!("custom_titlebar.css");

fn main() {
    florui_platform::run_with_options(
        "Florui custom title bar",
        CSS,
        Rgba::opaque(0x1e, 0x1e, 0x22),
        WindowOptions {
            decorations: DecorationMode::Custom,
            ..WindowOptions::default()
        },
        title_bar_demo,
    )
    .expect("event loop should not fail on a real desktop session");
}

/// A caption button wrapped in a real `Popover`-based tooltip -- no
/// dedicated tooltip primitive exists yet, and `Popover` already does the
/// positioning/portal work one needs. Shows/hides immediately on
/// `mouseenter`/`mouseleave`; a real hover-delay needs a debounce timer
/// `florui-reactive` doesn't have yet, so that stays a tracked gap.
///
/// A plain function, not `#[component]` -- hooks key off call order, not
/// the Rust call stack, so this works inlined the same way as long as
/// it's called the same fixed number of times in the same order every
/// render (it is: once each for minimize/maximize/close).
fn caption_button(
    id: &str,
    class: &str,
    label: String,
    glyph: &str,
    onclick: impl Fn() + 'static,
) -> Element {
    let tooltip_open = use_signal(|| false);
    let enter = tooltip_open.clone();
    let leave = tooltip_open.clone();
    let dismiss = tooltip_open.clone();
    let class = class.to_owned();
    let label_for_button = label.clone();
    let trigger_id = format!("{id}-trigger");

    view! {
        <Popover
            id={id.to_string()}
            open={tooltip_open.get()}
            placement={Placement::new(Side::Bottom, Align::Center)}
            ondismiss={Handler::new(move || dismiss.set(false))}
            trigger={view! {
                <button
                    id={trigger_id}
                    class={class}
                    accessible_label={label_for_button}
                    onmouseenter={move || enter.set(true)}
                    onmouseleave={move || leave.set(false)}
                    onclick={onclick}
                >
                    {glyph.to_string()}
                </button>
            }}
        >
            <div class="caption-tooltip">{label}</div>
        </Popover>
    }
}

fn title_bar_demo() -> Element {
    let controls = use_window_controls();
    let maximized = controls.as_ref().is_some_and(|c| c.is_maximized());
    let focused = controls.as_ref().is_none_or(|c| c.is_focused());

    // Demonstrates close cancellation: while unsaved, closing (via this
    // button or the real OS close) is vetoed.
    let unsaved = use_signal(|| true);
    {
        let controls = controls.clone();
        let unsaved = unsaved.clone();
        use_effect((), move || {
            if let Some(controls) = &controls {
                let unsaved = unsaved.clone();
                controls.set_close_guard(move || !unsaved.get());
            }
            let controls = controls.clone();
            Some(Box::new(move || {
                if let Some(controls) = &controls {
                    controls.clear_close_guard();
                }
            }) as Cleanup)
        });
    }

    // Real production evidence, not just the standalone `appearance_probe`
    // example: what this actual window got back once `DecorationMode::Custom`
    // reached a live `winit` window, printed once on mount.
    {
        let controls = controls.clone();
        use_effect((), move || {
            if let Some(controls) = &controls {
                eprintln!("appearance report: {:#?}", controls.appearance_report());
            }
            None
        });
    }

    let minimize = controls.clone();
    let toggle_maximize = controls.clone();
    let close = controls.clone();
    let toggle_unsaved = unsaved.clone();

    view! {
        <div class="app">
            <div class={if focused { "titlebar" } else { "titlebar inactive" }}>
                <span class="titlebar-title">{"Florui"}</span>
                <div id={WINDOW_DRAG_REGION_ID} class="drag-region" />
                <div class="titlebar-extra">
                    <button class="icon-button">{"*"}</button>
                    <button class="icon-button">{"?"}</button>
                </div>
                <div class="window-buttons">
                    {caption_button(
                        "minimize",
                        "window-button",
                        "Minimize".to_string(),
                        "_",
                        move || if let Some(controls) = &minimize {
                            controls.minimize();
                        },
                    )}
                    {caption_button(
                        "toggle-maximize",
                        "window-button",
                        if maximized { "Restore".to_string() } else { "Maximize".to_string() },
                        if maximized { "[ ]" } else { "[]" },
                        move || if let Some(controls) = &toggle_maximize {
                            controls.toggle_maximize();
                        },
                    )}
                    {caption_button(
                        "close",
                        "close-button",
                        "Close".to_string(),
                        "x",
                        move || if let Some(controls) = &close {
                            controls.close();
                        },
                    )}
                </div>
            </div>
            <div class="body">
                <p class="body-text">
                    {"Drag from the empty title bar space. Minimize/maximize/close all drive \
                      the real window through WindowControls, not OS chrome."}
                </p>
                <p class="body-text">
                    {if unsaved.get() {
                        "Unsaved changes -- closing is blocked. Try the X or Alt+F4."
                    } else {
                        "Saved -- closing works normally."
                    }}
                </p>
                <button
                    class="window-button"
                    onclick={move || toggle_unsaved.set(!toggle_unsaved.get())}
                >
                    {if unsaved.get() { "Mark saved" } else { "Mark unsaved" }}
                </button>
            </div>
        </div>
    }
}
