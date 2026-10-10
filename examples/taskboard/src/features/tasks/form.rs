//! The plate for adding a task: a line for the title, the priority keys and a
//! brass key to add it.

use florui::prelude::*;

use super::model::Priority;
use super::store::TaskStore;
use crate::components::button::{KeyButton, KeyButtonProps};
use crate::components::field::{Field, FieldProps};
use crate::components::panel::{Panel, PanelProps};
use crate::components::segmented::{Segmented, SegmentedProps};

#[component]
pub fn NewTask() -> Element {
    let store = use_context::<TaskStore>().expect("the frame provides the task store");
    let title = use_signal(String::new);
    let priority = use_signal(|| 1usize);
    let options: Vec<String> = Priority::ALL
        .iter()
        .map(|p| p.label().to_string())
        .collect();

    let add = {
        let title = title.clone();
        let priority = priority.clone();
        Handler::new(move || {
            if store
                .add(&title.get(), Priority::from_step(priority.get()))
                .is_some()
            {
                title.set(String::new());
            }
        })
    };

    view! {
        <Panel title={"NEW CARD".to_string()}>
            <Field
                id={"new-title".to_string()}
                label={"TITLE".to_string()}
                value={title.binding()}
            />
            <div class="control">
                <span class="label">{"PRIORITY"}</span>
                <Segmented options={options} selected={priority.binding()} />
            </div>
            <KeyButton label={"Add to the stack".to_string()} on_press={add} accent={true} />
        </Panel>
    }
}
