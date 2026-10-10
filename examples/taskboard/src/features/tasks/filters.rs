//! The plate with the controls that decide which tasks are shown: a line to
//! search, the status keys and a knob for the lowest priority.

use florui::prelude::*;

use super::model::{Priority, Status};
use super::store::TaskStore;
use crate::components::field::{Field, FieldProps};
use crate::components::knob::{Knob, KnobProps};
use crate::components::panel::{Panel, PanelProps};
use crate::components::segmented::{Segmented, SegmentedProps};

#[component]
pub fn Filters() -> Element {
    let store = use_context::<TaskStore>().expect("the frame provides the task store");
    let mut statuses = vec!["All".to_string()];
    statuses.extend(
        Status::ALL
            .iter()
            .map(|status| status.key_label().to_string()),
    );
    let shown = store.visible().len();
    let total = store.tasks().len();
    let lowest = Priority::from_step(store.priority_binding().get());

    view! {
        <Panel title={"FILTERS".to_string()}>
            <Field
                id={"search".to_string()}
                label={"SEARCH".to_string()}
                value={store.query_binding()}
            />
            <div class="control">
                <span class="label">{"STATUS"}</span>
                <Segmented options={statuses} selected={store.status_binding()} />
            </div>
            <div class="control knob-row">
                <Knob
                    step={store.priority_binding()}
                    steps={Priority::ALL.len()}
                    label={"PRIORITY".to_string()}
                />
                <span class="on-dark readout">{format!("{} or above", lowest.label())}</span>
            </div>
            <span class="on-dark readout">{format!("{shown} of {total} cards")}</span>
        </Panel>
    }
}
