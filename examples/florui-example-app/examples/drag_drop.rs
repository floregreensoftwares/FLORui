//! Real native drag-and-drop: drag files or folders from Explorer onto
//! the drop zone below. Only exercises accept/reject and delivery -- no
//! automatic hover styling from the framework itself (see
//! `florui_platform::WindowControls::on_drag_event`'s own doc for why);
//! this example drives its own drop-zone styling from ordinary signals,
//! the same way any other app-level interaction state would.
//!
//! The accept policy here only accepts a payload where every dropped
//! path ends in `.txt`, so both the accept and reject paths are visibly
//! exercised -- drag a `.txt` file for a green zone, anything else for
//! red.
//!
//! `cargo run --example drag_drop -p florui-example-app`

use florui::prelude::*;
use florui_platform::{DragEvent, DragPayload, use_window_controls};
use florui_reactive::{Cleanup, use_effect, use_signal};
use florui_style::Rgba;

const CSS: &str = include_str!("drag_drop.css");

fn main() {
    florui_platform::run(
        "Florui -- native drag-and-drop",
        CSS,
        Rgba::opaque(0x1e, 0x1e, 0x22),
        root,
    )
    .expect("event loop should not fail on a real desktop session");
}

fn accepts_only_text_files(payload: &DragPayload) -> bool {
    let DragPayload::Files(paths) = payload;
    !paths.is_empty()
        && paths.iter().all(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        })
}

#[derive(Clone, Copy, PartialEq)]
enum HoverState {
    None,
    Accepting,
    Rejecting,
}

fn root() -> Element {
    let controls = use_window_controls();
    let hover = use_signal(|| HoverState::None);
    let dropped = use_signal(Vec::<String>::new);
    let pointer = use_signal(|| "(not over the window)".to_owned());

    {
        let controls = controls.clone();
        let hover = hover.clone();
        let dropped = dropped.clone();
        let pointer = pointer.clone();
        use_effect((), move || {
            let controls = controls.clone()?;
            controls.set_drag_accept_policy(accepts_only_text_files);

            let hover_for_event = hover.clone();
            let dropped_for_event = dropped.clone();
            let pointer_for_event = pointer.clone();
            controls.on_drag_event(move |event| match event {
                DragEvent::Enter {
                    accepted, position, ..
                }
                | DragEvent::Over {
                    accepted, position, ..
                } => {
                    pointer_for_event.set(format!("{:.0}, {:.0}", position.x, position.y));
                    hover_for_event.set(if accepted {
                        HoverState::Accepting
                    } else {
                        HoverState::Rejecting
                    });
                }
                DragEvent::Leave => hover_for_event.set(HoverState::None),
                DragEvent::Drop {
                    payload: DragPayload::Files(paths),
                    position,
                } => {
                    pointer_for_event
                        .set(format!("dropped at {:.0}, {:.0}", position.x, position.y));
                    hover_for_event.set(HoverState::None);
                    dropped_for_event.set(
                        paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect(),
                    );
                }
            });

            let cleanup_controls = controls;
            Some(Box::new(move || {
                cleanup_controls.clear_drag_accept_policy();
                cleanup_controls.clear_drag_event_handler();
            }) as Cleanup)
        });
    }

    let zone_class = match hover.get() {
        HoverState::None => "drop-zone".to_owned(),
        HoverState::Accepting => "drop-zone accepting".to_owned(),
        HoverState::Rejecting => "drop-zone rejecting".to_owned(),
    };
    let dropped_list = dropped.get();
    let dropped_text = if dropped_list.is_empty() {
        "(nothing dropped yet)".to_owned()
    } else {
        dropped_list.join("\n")
    };

    view! {
        <div class="page">
            <p class="label">{"Native drag-and-drop"}</p>
            <p class="hint">
                {"Drag a .txt file from Explorer onto the zone below -- it turns green \
                  and accepts. Anything else is rejected (no color change, no drop)."}
            </p>
            <div class={zone_class}>{"Drop a .txt file here"}</div>
            <p class="hint">{format!("Pointer: {}", pointer.get())}</p>
            <p class="hint">{format!("Last drop:\n{dropped_text}")}</p>
        </div>
    }
}
