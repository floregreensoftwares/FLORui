//! How the desk looks and behaves: the lamps' glow, the paper's grain and how
//! the glass lid is blurred. The preferences are state like any other, held
//! once in the frame and read by whatever they change.

use florui::prelude::*;

mod preferences;
mod view;

pub use preferences::Preferences;
pub use view::{SettingsPanel, SettingsPanelProps};

stylesheet!("./settings.css");
