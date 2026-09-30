//! A real virtualized list: 10,000 rows, but only a bounded window ever
//! mounted at once -- scroll with a real mouse wheel or trackpad and watch
//! the status line above (which reads `VirtualListHandle::offset`) move,
//! while the rest of the dataset never materializes as real `Element`s or
//! `ComponentScope`s at all.
//!
//! Every row is a button. Focus one, then scroll far away: the focused row
//! stays mounted (and keeps focus) until focus leaves it. "Focus row 7500"
//! brings a row that is not mounted into view and focuses it.
//!
//! With a row focused, Up and Down move between rows, Page Up and Page Down by a
//! screenful, Home and End to the first and last row.
//!
//! `cargo run --example virtualized_list -p florui-example-app`

use florui::prelude::*;
use florui_platform::{ItemHeight, Overscan, use_virtual_list};
use florui_reactive::Key;
use florui_style::Rgba;

const CSS: &str = include_str!("virtualized_list.css");
const ITEM_COUNT: usize = 10_000;
const ITEM_HEIGHT: f32 = 32.0;
const JUMP_TARGET: usize = 7_500;

fn main() {
    florui_platform::run(
        "Florui virtualized list",
        CSS,
        Rgba::opaque(0x10, 0x10, 0x14),
        widget,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn row(i: usize) -> Element {
    view! { <button class="row">{format!("Row {i}")}</button> }
}

fn widget() -> Element {
    let (content, handle) = use_virtual_list(
        "virtual-list",
        ITEM_COUNT,
        (),
        ItemHeight::Fixed(ITEM_HEIGHT),
        Overscan::default(),
        Key::from,
        row,
    );

    let (_, scroll_top) = handle.offset();
    let status = format!(
        "near row {} of {ITEM_COUNT} -- scroll offset: {scroll_top:.0}",
        (scroll_top / ITEM_HEIGHT) as usize
    );
    view! {
        <div class="page">
            <div class="status">{status}</div>
            <button class="jump" onclick={move || handle.focus_item(JUMP_TARGET)}>
                {format!("Focus row {JUMP_TARGET}")}
            </button>
            <div id="virtual-list" class="scroll-box" role="list" accessible_label="Rows">
                {content}
            </div>
        </div>
    }
}
