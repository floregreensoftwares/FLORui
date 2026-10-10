//! The physical parts of the desk. A component here is a folder with its
//! markup (`mod.rs`) and its stylesheet next to it, takes what it shows and
//! reports what the person did (a `Binding` or a `Handler`), and knows nothing
//! about tasks: the features decide what a switch or a knob means.

pub mod button;
pub mod field;
pub mod knob;
pub mod led;
pub mod meter;
pub mod panel;
pub mod paper_card;
pub mod segmented;
pub mod toggle;
