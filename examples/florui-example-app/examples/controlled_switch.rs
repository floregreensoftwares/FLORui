//! A real `Switch`, live: click the track or the thumb, or Tab to it and
//! press Space, to flip it. The disabled one never toggles or takes
//! focus, and the platform accessibility bridge (Narrator, on Windows)
//! reports each as a real switch with its name and on/off state.
//!
//! Both ways to wire a switch are shown. "Wi-Fi" uses a
//! `Binding<bool>` (`Signal::binding`, which always accepts). "Sync" uses
//! the explicit contract -- a plain value plus a `BoolHandler` told the
//! value the switch is asking for -- and the owner rejects turning it off.
//!
//! `cargo run --example controlled_switch -p florui-example-app`

use florui::prelude::*;
use florui_platform::{Switch, SwitchProps, SwitchValue};
use florui_reactive::use_signal;
use florui_style::Rgba;

const CONTROLLED_SWITCH_CSS_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/controlled_switch.css"
);

fn main() {
    florui_platform::run_with_css_reload(
        "Florui controlled switch",
        CONTROLLED_SWITCH_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        app,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn app() -> Element {
    let wifi = use_signal(|| false);
    let sync = use_signal(|| false);
    let accept_sync = sync.clone();
    let locked = use_signal(|| true);

    view! {
        <div class="page">
            <p class="instructions">
                {"Click the track, the thumb or the label, or Tab to it and press Space. \
                  Sync can be turned on but refuses to turn off; the disabled one never moves."}
            </p>

            <div class="row">
                <Switch
                    id={"wifi-switch".to_string()}
                    value={SwitchValue::bound(wifi.binding())}
                    disabled={false}
                    accessible_label={"Wi-Fi".to_string()}
                />
                <label class="field-label" for="wifi-switch">{"Wi-Fi (Binding)"}</label>
            </div>
            <p class="status">{format!("Committed: {}", wifi.get())}</p>

            <div class="row">
                <Switch
                    id={"sync-switch".to_string()}
                    value={SwitchValue::controlled(
                        sync.get(),
                        BoolHandler::new(move |next| {
                            if next {
                                accept_sync.set(true);
                            }
                        }),
                    )}
                    disabled={false}
                    accessible_label={"Sync".to_string()}
                />
                <label class="field-label" for="sync-switch">
                    {"Sync (value + handler, refuses to turn off)"}
                </label>
            </div>
            <p class="status">{format!("Committed: {}", sync.get())}</p>

            <div class="row">
                <Switch
                    id={"locked-switch".to_string()}
                    value={SwitchValue::controlled(locked.get(), BoolHandler::new(|_| {}))}
                    disabled={true}
                    accessible_label={"Locked setting".to_string()}
                />
                <label class="field-label" for="locked-switch">
                    {"Locked setting (disabled, stays on)"}
                </label>
            </div>
        </div>
    }
}
