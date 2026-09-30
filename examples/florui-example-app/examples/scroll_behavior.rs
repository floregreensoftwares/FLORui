//! `scroll-behavior: smooth`, live: two identical scroll boxes, the left one
//! with the property in its CSS and the right one without. "Jump to row 15"
//! calls the same `ScrollHandle::scroll_to` on each; the left animates the
//! way a browser's `scrollTo` does, the right jumps. "Jump instantly" is
//! `scroll_to_instant`, which ignores the CSS. The wheel animates in both.
//!
//! `cargo run --example scroll_behavior -p florui-example-app`

use florui::prelude::*;
use florui_platform::use_scroll_offset;
use florui_reactive::use_signal;
use florui_style::Rgba;

const CSS: &str = include_str!("scroll_behavior.css");

fn main() {
    florui_platform::run(
        "Florui scroll-behavior",
        CSS,
        Rgba::opaque(0x10, 0x10, 0x14),
        widget,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn column(
    title: &'static str,
    id: &'static str,
    class: &'static str,
    offset: florui_reactive::Signal<f32>,
) -> Element {
    let reported = offset.clone();
    let handle = use_scroll_offset(id, move |_, y| reported.set(y));
    let (jump, instant, top) = (handle.clone(), handle.clone(), handle);
    view! {
        <div class="column">
            <div class="status">{format!("{title}: {:.0}", offset.get())}</div>
            <div class="buttons">
                <button class="button" onclick={move || jump.scroll_to(0.0, 600.0)}>
                    {"Jump to row 15"}
                </button>
                <button class="button" onclick={move || instant.scroll_to_instant(0.0, 0.0)}>
                    {"Jump instantly to top"}
                </button>
                <button class="button" onclick={move || top.scroll_to(0.0, 0.0)}>
                    {"Back to top"}
                </button>
            </div>
            <div id={id} class={class}>
                {(0..40).map(|i| view! {
                    <div class="row">{format!("Row {i}")}</div>
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}

fn widget() -> Element {
    let smooth = use_signal(|| 0.0f32);
    let plain = use_signal(|| 0.0f32);
    view! {
        <div class="page">
            {column("scroll-behavior: smooth", "smooth-box", "scroll-box smooth", smooth)}
            {column("default", "plain-box", "scroll-box", plain)}
        </div>
    }
}
