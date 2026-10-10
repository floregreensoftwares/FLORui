use florui::prelude::*;

use crate::features::tasks::{
    Detail, DetailProps, Filters, FiltersProps, NewTask, NewTaskProps, TaskList, TaskListProps,
};

pub fn page() -> Element {
    view! {
        <div class="page">
            <h1 class="title">{"The stack"}</h1>
            <div class="tasks-layout">
                <div class="tasks-side">
                    <Filters />
                    <Detail />
                </div>
                <div class="tasks-stack">
                    <div class="new-task"><NewTask /></div>
                    <TaskList />
                </div>
            </div>
        </div>
    }
}
