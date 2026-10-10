//! The plate with a lamp and a count for each state a task can be in, and a
//! meter of how much is done.

use florui::prelude::*;

use crate::components::led::{Led, LedProps, Tone};
use crate::components::meter::{Meter, MeterProps};
use crate::components::panel::{Panel, PanelProps};
use crate::features::tasks::{TaskStore, counts};

fn line(tone: Tone, label: &str, count: usize) -> Element {
    view! {
        <div class="count-line">
            <Led tone={tone} lit={count > 0} />
            <span class="on-dark count-label">{label.to_string()}</span>
            <span class="on-dark count-value">{count.to_string()}</span>
        </div>
    }
}

#[component]
pub fn Summary() -> Element {
    let store = use_context::<TaskStore>().expect("the frame provides the task store");
    let counts = counts(&store.tasks());
    view! {
        <Panel title={"ON THE DESK".to_string()}>
            {line(Tone::Amber, "To do", counts.todo)}
            {line(Tone::Blue, "Doing", counts.doing)}
            {line(Tone::Red, "Blocked", counts.blocked)}
            {line(Tone::Green, "Done", counts.done)}
            <Meter fraction={counts.completion()} label={"COMPLETED".to_string()} />
            <span class="on-dark readout">{format!("{} cards are late.", counts.late)}</span>
        </Panel>
    }
}
