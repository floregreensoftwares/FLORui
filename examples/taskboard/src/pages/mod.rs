//! One module per screen. A page only arranges features on the desk; it holds
//! no state and knows no component.

use florui::prelude::*;

pub mod overview;
pub mod settings;
pub mod tasks;

stylesheet!("./pages.css");
