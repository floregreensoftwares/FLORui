//! The overview: how the tasks stand (`Summary`) and which ones are next
//! (`NextUp`), both read from the task store and never changing it.

use florui::prelude::*;

mod next_up;
mod summary;

pub use next_up::{NextUp, NextUpProps};
pub use summary::{Summary, SummaryProps};

stylesheet!("./overview.css");
