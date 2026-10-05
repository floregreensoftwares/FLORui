//! The counter app with the real inspector, reloading `counter.css` from disk.
//! Press Record in the inspector's profile panel, then edit and save
//! `counter.css`: the frame that reload caused shows where the time went from
//! the watcher's report of the change to the present call returning.
//!
//! `cargo run --example inspector_reload -p florui-example-app`

use florui_example_app::components::counter::{Counter, CounterProps};
use florui_platform::{WindowOptions, WindowSpec};
use florui_style::Rgba;

const COUNTER_CSS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/components/counter.css");

fn main() {
    let spec = WindowSpec::with_css_reload(
        "Florui counter",
        COUNTER_CSS_PATH,
        Rgba::opaque(0x10, 0x10, 0x14),
        WindowOptions::default(),
        || Counter(CounterProps {}),
    )
    .expect("the stylesheet next to this example should load");
    florui_devtools::live::run_windows(vec![spec])
        .expect("event loop should not fail on a real desktop session");
}
