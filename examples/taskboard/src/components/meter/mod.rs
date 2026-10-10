//! A bar meter in a recessed slot, filled with brass up to `fraction`
//! (0 to 1) and marked off in tenths.

use florui::prelude::*;

stylesheet!("./meter.css");

/// The width of the fill as a percentage, kept inside 0 to 100.
pub fn fill_percent(fraction: f32) -> f32 {
    (fraction.clamp(0.0, 1.0) * 1000.0).round() / 10.0
}

#[component]
pub fn Meter(fraction: f32, label: String) -> Element {
    let percent = fill_percent(fraction);
    view! {
        <div class="meter" accessible_label={format!("{label}: {percent}%")}>
            <span class="label">{label}</span>
            <div class="slot well">
                <div class="fill" style={format!("width: {percent}%;")}></div>
                <div class="marks"></div>
            </div>
        </div>
    }
}
