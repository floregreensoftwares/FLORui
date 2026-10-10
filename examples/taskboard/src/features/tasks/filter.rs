//! Which tasks to show, and how many there are of each kind. Plain functions
//! over plain data, so they are tested without a window or a signal.

use super::model::{Priority, Status, Task};

/// What the person has asked to see.
#[derive(Debug, Clone, PartialEq)]
pub struct Filter {
    pub status: Option<Status>,
    /// Tasks below this priority are hidden.
    pub min_priority: Priority,
    /// Matched against the title and the notes, ignoring case; empty matches all.
    pub query: String,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            status: None,
            min_priority: Priority::Low,
            query: String::new(),
        }
    }
}

pub fn matches(task: &Task, filter: &Filter) -> bool {
    if filter.status.is_some_and(|status| status != task.status) {
        return false;
    }
    if task.priority < filter.min_priority {
        return false;
    }
    let query = filter.query.trim();
    if query.is_empty() {
        return true;
    }
    let query = query.to_lowercase();
    task.title.to_lowercase().contains(&query) || task.notes.to_lowercase().contains(&query)
}

/// The indices into `tasks` of the ones that match, in order.
pub fn visible(tasks: &[Task], filter: &Filter) -> Vec<usize> {
    tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| matches(task, filter))
        .map(|(index, _)| index)
        .collect()
}

/// How many tasks there are of each kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub todo: usize,
    pub doing: usize,
    pub blocked: usize,
    pub done: usize,
    pub late: usize,
}

impl Counts {
    pub fn total(&self) -> usize {
        self.todo + self.doing + self.blocked + self.done
    }

    /// The share of tasks that are done, 0 to 1; 0 when there are none.
    pub fn completion(&self) -> f32 {
        match self.total() {
            0 => 0.0,
            total => self.done as f32 / total as f32,
        }
    }
}

pub fn counts(tasks: &[Task]) -> Counts {
    let mut counts = Counts::default();
    for task in tasks {
        match task.status {
            Status::Todo => counts.todo += 1,
            Status::Doing => counts.doing += 1,
            Status::Blocked => counts.blocked += 1,
            Status::Done => counts.done += 1,
        }
        if task.is_late() {
            counts.late += 1;
        }
    }
    counts
}
