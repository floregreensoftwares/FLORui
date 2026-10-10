//! The plate of settings: a switch for each of the two options, and the
//! grain knob.

use florui::prelude::*;

use super::preferences::{GRAIN_STEPS, Preferences};
use crate::components::knob::{Knob, KnobProps};
use crate::components::panel::{Panel, PanelProps};
use crate::components::toggle::{Toggle, ToggleProps};

#[component]
pub fn SettingsPanel() -> Element {
    let prefs = use_context::<Preferences>().expect("the frame provides the preferences");
    let grain = prefs.grain();
    let grain_name = Preferences::grain_name(grain.get());

    view! {
        <Panel title={"DESK".to_string()}>
            <div class="setting">
                <div class="setting-text">
                    <span class="heading on-dark">{"Lamp glow"}</span>
                    <span class="on-dark readout">{"The halo around a lit lamp."}</span>
                </div>
                <Toggle id={"glow".to_string()} on={prefs.glow()} label={"Lamp glow".to_string()} />
            </div>
            <div class="setting">
                <div class="setting-text">
                    <span class="heading on-dark">{"Fast glass"}</span>
                    <span class="on-dark readout">
                        {"Blur the glass lid on a smaller copy: quicker, a little softer."}
                    </span>
                </div>
                <Toggle
                    id={"fast-glass".to_string()}
                    on={prefs.fast_glass()}
                    label={"Fast glass".to_string()}
                />
            </div>
            <div class="setting">
                <div class="setting-text">
                    <span class="heading on-dark">{"Paper grain"}</span>
                    <span class="on-dark readout">{format!("The cards are {grain_name}.")}</span>
                </div>
                <Knob step={grain} steps={GRAIN_STEPS} label={"GRAIN".to_string()} />
            </div>
        </Panel>
    }
}
