//! The few cards that are due first among those not done yet, laid out as
//! paper cards.

use florui::prelude::*;

use crate::components::panel::{Panel, PanelProps};
use crate::components::paper_card::{PaperCard, PaperCardProps};
use crate::features::tasks::{Status, TaskStore};

/// How many cards the plate shows.
const SHOWN: usize = 5;

#[component]
pub fn NextUp() -> Element {
    let store = use_context::<TaskStore>().expect("the frame provides the task store");
    let tasks = store.tasks();
    let mut open: Vec<_> = tasks
        .iter()
        .filter(|task| task.status != Status::Done)
        .collect();
    open.sort_by_key(|task| (task.due_in_days, std::cmp::Reverse(task.priority), task.id));
    let cards: Vec<Element> = open
        .into_iter()
        .take(SHOWN)
        .map(|task| {
            view! {
                <div class="next-card">
                    <PaperCard
                        title={task.title.clone()}
                        subtitle={format!("{} - {}", task.due_text(), task.priority.label())}
                        selected={false}
                    >

                    </PaperCard>
                </div>
            }
        })
        .collect();
    view! { <Panel title={"NEXT UP".to_string()}>{cards}</Panel> }
}
