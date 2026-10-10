//! The index card for the picked task: its title and notes, and the controls
//! that change it (a row of status keys and a priority knob).

use florui::prelude::*;

use super::model::{Priority, Status};
use super::store::TaskStore;
use crate::components::knob::{Knob, KnobProps};
use crate::components::panel::{Panel, PanelProps};
use crate::components::segmented::{Segmented, SegmentedProps};

#[component]
pub fn Detail() -> Element {
    let store = use_context::<TaskStore>().expect("the frame provides the task store");
    let Some(task) = store.selected() else {
        return view! {
            <Panel title={"INDEX CARD".to_string()}>
                <span class="on-dark">{"Pick a card from the stack to read it here."}</span>
            </Panel>
        };
    };

    let id = task.id;
    let status_store = store.clone();
    let status = Binding::new(
        Status::ALL
            .iter()
            .position(|&s| s == task.status)
            .unwrap_or(0),
        move |index: usize| status_store.set_status(id, Status::ALL[index.min(3)]),
    );
    let priority_store = store.clone();
    let priority = Binding::new(task.priority.step(), move |step: usize| {
        priority_store.set_priority(id, Priority::from_step(step))
    });
    let statuses: Vec<String> = Status::ALL
        .iter()
        .map(|s| s.key_label().to_string())
        .collect();
    let notes = if task.notes.is_empty() {
        "No notes yet.".to_string()
    } else {
        task.notes.clone()
    };

    view! {
        <Panel title={"INDEX CARD".to_string()}>
            <h2 class="title detail-title">{task.title.clone()}</h2>
            <span class="on-dark">{task.due_text()}</span>
            <span class="on-dark notes">{notes}</span>
            <div class="control">
                <span class="label">{"STATUS"}</span>
                <Segmented options={statuses} selected={status} />
            </div>
            <div class="control">
                <Knob step={priority} steps={Priority::ALL.len()} label={"PRIORITY".to_string()} />
            </div>
        </Panel>
    }
}
