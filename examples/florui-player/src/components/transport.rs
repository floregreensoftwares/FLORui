//! Play/pause/prev/next, shuffle/repeat (real `Switch` components -- the
//! same control the checkbox/radio/switch work elsewhere in this project
//! landed), a step-adjusted volume bar, and the playback-position bar.
//!
//! No real `<input type="range">` slider exists yet as of this file, so
//! volume is stepped by two buttons rather than dragged -- swap in the
//! real slider here once that work merges, matching this project's own
//! "don't block on in-flight framework work" rule.

use florui::prelude::*;
use florui_platform::{Switch, SwitchProps, SwitchValue};

#[component]
pub fn Transport(
    paused: bool,
    on_play_pause: Handler,
    on_previous: Handler,
    on_next: Handler,
    volume: f32,
    on_volume_down: Handler,
    on_volume_up: Handler,
    shuffle: bool,
    on_shuffle: Handler,
    repeat: bool,
    on_repeat: Handler,
    progress: f32,
) -> Element {
    let volume_percent = (volume * 100.0).round() as i32;
    let progress_percent = (progress * 100.0).clamp(0.0, 100.0);

    view! {
        <div class="transport">
            <div class="progress-track">
                <div class="progress-fill" style={format!("width: {progress_percent}%;")} />
            </div>

            <div class="transport-row">
                <button class="transport-button" accessible_label="Previous" onclick={move || on_previous.call(&Event::new())}>
                    {"|<"}
                </button>
                <button class="transport-button transport-primary" accessible_label={if paused { "Play" } else { "Pause" }} onclick={move || on_play_pause.call(&Event::new())}>
                    {if paused { ">" } else { "||" }}
                </button>
                <button class="transport-button" accessible_label="Next" onclick={move || on_next.call(&Event::new())}>
                    {">|"}
                </button>

                <div class="volume">
                    <button class="volume-step" accessible_label="Volume down" onclick={move || on_volume_down.call(&Event::new())}>
                        {"-"}
                    </button>
                    <div class="volume-track">
                        <div class="volume-fill" style={format!("width: {volume_percent}%;")} />
                    </div>
                    <button class="volume-step" accessible_label="Volume up" onclick={move || on_volume_up.call(&Event::new())}>
                        {"+"}
                    </button>
                </div>
            </div>

            <div class="toggles-row">
                <label class="toggle-label" for="shuffle-switch">
                    <Switch
                        id={"shuffle-switch".to_string()}
                        value={SwitchValue::controlled(shuffle, BoolHandler::new(move |_| on_shuffle.call(&Event::new())))}
                        disabled={false}
                        indeterminate={false}
                        accessible_label={"Shuffle".to_string()}
                    />
                    <span>{"Shuffle"}</span>
                </label>
                <label class="toggle-label" for="repeat-switch">
                    <Switch
                        id={"repeat-switch".to_string()}
                        value={SwitchValue::controlled(repeat, BoolHandler::new(move |_| on_repeat.call(&Event::new())))}
                        disabled={false}
                        indeterminate={false}
                        accessible_label={"Repeat".to_string()}
                    />
                    <span>{"Repeat"}</span>
                </label>
            </div>
        </div>
    }
}
