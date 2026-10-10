//! A rotary knob with detents. Pressing it moves to the next detent (and back
//! to the first after the last); a row of lamps under it shows how far it is
//! turned. Like the switch it owns nothing: it shows `step` and asks for the
//! next one.

use florui::prelude::*;

use crate::components::led::{Led, LedProps, Tone};

stylesheet!("./knob.css");

/// How far the pointer swings between the first and the last detent.
const SWEEP_DEGREES: f32 = 270.0;

/// Where the pointer points at `step` of `steps`, in degrees clockwise from the
/// top: the first detent is at -135 and the last at +135.
pub fn pointer_angle(step: usize, steps: usize) -> f32 {
    if steps < 2 {
        return 0.0;
    }
    let step = step.min(steps - 1);
    -SWEEP_DEGREES / 2.0 + SWEEP_DEGREES * step as f32 / (steps - 1) as f32
}

#[component]
pub fn Knob(step: Binding<usize>, steps: usize, label: String) -> Element {
    let current = step.get().min(steps.saturating_sub(1));
    let advance = step.clone();
    let angle = pointer_angle(current, steps);
    let lamps: Vec<Element> = (0..steps)
        .map(|index| view! { <Led tone={Tone::Amber} lit={index <= current} /> })
        .collect();
    view! {
        <div class="knob-group">
            <button
                class="knob"
                accessible_label={format!("{label}, {} of {steps}", current + 1)}
                onclick={move || advance.request_update((advance.get() + 1) % steps.max(1))}
            >
                <span class="cap" style={format!("transform: rotate({angle}deg);")}>
                    <span class="pointer"></span>
                </span>
            </button>
            <div class="lamps">{lamps}</div>
            <span class="label">{label}</span>
        </div>
    }
}
