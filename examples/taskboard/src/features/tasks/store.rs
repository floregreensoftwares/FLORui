//! The state the tasks screens share: the tasks, what is being filtered for,
//! and which one is picked. Created once, in the frame around the screens, and
//! handed down through context.

use std::rc::Rc;

use florui::prelude::*;

use super::filter::{Filter, visible};
use super::model::{Priority, Status, Task, sample_tasks};

#[derive(Clone)]
pub struct TaskStore {
    tasks: Signal<Rc<Vec<Task>>>,
    status_index: Signal<usize>,
    priority_step: Signal<usize>,
    query: Signal<String>,
    selected: Signal<Option<u32>>,
}

impl TaskStore {
    /// Creates the store with `count` sample tasks. A hook: call it from a
    /// component, in the same place on every render.
    pub fn use_new(count: usize) -> Self {
        Self {
            tasks: use_signal(move || Rc::new(sample_tasks(count))),
            status_index: use_signal(|| 0),
            priority_step: use_signal(|| 0),
            query: use_signal(String::new),
            selected: use_signal(|| None),
        }
    }

    pub fn tasks(&self) -> Rc<Vec<Task>> {
        self.tasks.get()
    }

    pub fn filter(&self) -> Filter {
        let status_index = self.status_index.get();
        Filter {
            status: status_index.checked_sub(1).map(|i| Status::ALL[i.min(3)]),
            min_priority: Priority::from_step(self.priority_step.get()),
            query: self.query.get(),
        }
    }

    /// The indices into [`Self::tasks`] that pass the current filter.
    pub fn visible(&self) -> Vec<usize> {
        visible(&self.tasks.get(), &self.filter())
    }

    /// The status filter as a segmented control's index: 0 is "all", then the
    /// statuses in order.
    pub fn status_binding(&self) -> Binding<usize> {
        self.status_index.binding()
    }

    pub fn priority_binding(&self) -> Binding<usize> {
        self.priority_step.binding()
    }

    pub fn query_binding(&self) -> Binding<String> {
        self.query.binding()
    }

    pub fn selected(&self) -> Option<Task> {
        let id = self.selected.get()?;
        self.tasks.get().iter().find(|task| task.id == id).cloned()
    }

    pub fn select(&self, id: u32) {
        self.selected.set(Some(id));
    }

    /// Adds a task to the front of the list and picks it. A title that is only
    /// blanks adds nothing.
    pub fn add(&self, title: &str, priority: Priority) -> Option<u32> {
        let title = title.trim();
        if title.is_empty() {
            return None;
        }
        let mut tasks = (*self.tasks.get()).clone();
        let id = tasks.iter().map(|task| task.id).max().unwrap_or(0) + 1;
        tasks.insert(
            0,
            Task {
                id,
                title: title.to_string(),
                notes: String::new(),
                status: Status::Todo,
                priority,
                due_in_days: 7,
            },
        );
        self.tasks.set(Rc::new(tasks));
        self.selected.set(Some(id));
        Some(id)
    }

    pub fn set_status(&self, id: u32, status: Status) {
        self.update(id, |task| task.status = status);
    }

    pub fn set_priority(&self, id: u32, priority: Priority) {
        self.update(id, |task| task.priority = priority);
    }

    fn update(&self, id: u32, change: impl FnOnce(&mut Task)) {
        let mut tasks = (*self.tasks.get()).clone();
        if let Some(task) = tasks.iter_mut().find(|task| task.id == id) {
            change(task);
            self.tasks.set(Rc::new(tasks));
        }
    }
}
