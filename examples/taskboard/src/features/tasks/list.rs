//! The stack of cards: every task that passes the filter, as a ruled paper
//! card. The list is virtualized, so a couple of thousand cards cost the same
//! as the few on screen.

use std::rc::Rc;

use florui::prelude::*;
use florui_platform::{ItemHeight, Overscan, use_virtual_list};

use super::model::{Status, Task};
use super::store::TaskStore;
use crate::components::led::{Led, LedProps, Tone};
use crate::components::paper_card::{PaperCard, PaperCardProps};

/// The height of one card and the gap under it, in CSS pixels: the list
/// virtualizes on a fixed height.
const ROW_HEIGHT: f32 = 66.0;

fn tone(status: Status) -> Tone {
    match status {
        Status::Todo => Tone::Amber,
        Status::Doing => Tone::Blue,
        Status::Blocked => Tone::Red,
        Status::Done => Tone::Green,
    }
}

fn row(store: &TaskStore, tasks: &Rc<Vec<Task>>, index: usize, selected: Option<u32>) -> Element {
    let task = &tasks[index];
    let id = task.id;
    let pick = store.clone();
    let late = task.is_late();
    view! {
        <div class="task-row" onclick={move || pick.select(id)}>
            <PaperCard
                title={task.title.clone()}
                subtitle={format!("{} - {}", task.due_text(), task.priority.label())}
                selected={selected == Some(id)}
            >
                <Led tone={Tone::Red} lit={late} />
                <Led tone={tone(task.status)} lit={true} />
            </PaperCard>
        </div>
    }
}

#[component]
pub fn TaskList() -> Element {
    let store = use_context::<TaskStore>().expect("the frame provides the task store");
    let tasks = store.tasks();
    let shown = Rc::new(store.visible());
    let selected = store.selected().map(|task| task.id);

    let (rows, _handle) = use_virtual_list(
        "task-list",
        shown.len(),
        (store.filter(), Rc::as_ptr(&tasks) as usize, selected),
        ItemHeight::Fixed(ROW_HEIGHT),
        Overscan::default(),
        {
            let shown = Rc::clone(&shown);
            let tasks = Rc::clone(&tasks);
            move |position| Key::from(tasks[shown[position]].id as usize)
        },
        {
            let store = store.clone();
            let shown = Rc::clone(&shown);
            let tasks = Rc::clone(&tasks);
            move |position| row(&store, &tasks, shown[position], selected)
        },
    );

    view! {
        <div id="task-list" class="task-list" role="list" accessible_label="Task cards">{rows}</div>
    }
}
