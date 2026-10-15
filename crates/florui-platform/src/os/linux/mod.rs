//! Linux implementations of the platform services. Each module has the same
//! name and entry points as its `windows` counterpart.

pub mod accessibility;
pub mod file_dialog;
pub mod menu;
pub mod open_url;
pub mod overlay;
pub mod single_instance;
pub mod tray;
mod x11;
