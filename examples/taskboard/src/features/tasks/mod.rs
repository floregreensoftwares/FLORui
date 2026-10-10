//! The tasks: what a task is (`model`), which ones to show (`filter`), the
//! state the screens share (`store`), and the parts of the tasks screen
//! (`filters`, `list`, `detail`, `form`).

use florui::prelude::*;

mod detail;
mod filter;
mod filters;
mod form;
mod list;
mod model;
mod store;

pub use detail::{Detail, DetailProps};
pub use filter::{Counts, Filter, counts, matches, visible};
pub use filters::{Filters, FiltersProps};
pub use form::{NewTask, NewTaskProps};
pub use list::{TaskList, TaskListProps};
pub use model::{Priority, Status, Task, sample_tasks};
pub use store::TaskStore;

stylesheet!("./tasks.css");
