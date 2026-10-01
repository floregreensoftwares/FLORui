//! The measured workloads. Each builds a headless window through the code a
//! real window runs (`update`, then `frame`, which paints and builds the
//! accessibility tree) and returns the one operation to time. Trees are
//! ordinary rows, paragraphs and cards, not empty boxes.

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use florui::Element;
use florui_platform::{HeadlessOptions, HeadlessWindow, use_scroll_offset};
use florui_reactive::{Signal, use_signal};

/// One thing to measure: building it is the setup, calling the returned
/// closure once is one sample.
pub struct Workload {
    pub name: &'static str,
    pub description: &'static str,
    /// The features the workload exercises, so a result says what it covers.
    pub exercises: &'static str,
    pub build: fn() -> Box<dyn FnMut()>,
}

const CSS: &str = "
    .list { display: flex; flex-direction: column; width: 800px; }
    .scroll { height: 600px; overflow: auto; display: flex; flex-direction: column; }
    .row { display: flex; flex-direction: row; gap: 8px; padding: 4px 8px;
           border-bottom: 1px solid #ddd; background-color: #fff; }
    .row:hover { background-color: #eef; }
    .label { flex-grow: 1; color: #222; font-size: 14px; }
    .action { padding: 2px 8px; background-color: #36c; color: #fff; }
    .nest { padding: 1px; }
    .doc { display: flex; flex-direction: column; width: 100%; padding: 12px; }
    .para { margin: 0 0 8px 0; color: #222; font-size: 14px; line-height: 1.4; }
    .em { font-size: 16px; color: #36c; }
    .small { font-size: 11px; color: #666; }
    .cards { display: flex; flex-direction: column; width: 800px; }
    .card { overflow: hidden; border-radius: 8px; margin: 6px; padding: 8px;
            background-color: #fff; box-shadow: 0px 2px 8px rgba(0, 0, 0, 0.3); }
    .inner { overflow: hidden; border-radius: 4px; padding: 4px; background-color: #f4f4f8; }
";

fn class(name: &str) -> Vec<(String, String)> {
    vec![("class".into(), name.into())]
}

fn row(i: usize) -> Element {
    Element::node(
        "div",
        class("row"),
        vec![
            Element::node(
                "span",
                class("label"),
                vec![Element::text(format!("Item number {i}"))],
            ),
            Element::node("button", class("action"), vec![Element::text("Open")]),
        ],
    )
}

fn list(name: &str, n: usize) -> Element {
    Element::node("div", class(name), (0..n).map(row).collect())
}

fn paragraph(i: usize) -> Element {
    let words = [
        "layout",
        "shaping",
        "cascade",
        "baseline",
        "glyph",
        "wrapping",
        "paragraph",
        "inline",
        "measure",
        "cache",
    ];
    let sentence = |count: usize, seed: usize| -> String {
        (0..count)
            .map(|w| words[(seed * 7 + w * 3) % words.len()])
            .collect::<Vec<_>>()
            .join(" ")
    };
    Element::node(
        "p",
        class("para"),
        vec![
            Element::text(sentence(8 + i % 20, i)),
            Element::node("span", class("em"), vec![Element::text(sentence(3, i + 1))]),
            Element::text(format!(" {}", sentence(10 + i % 30, i + 2))),
            Element::node(
                "span",
                class("small"),
                vec![Element::text(sentence(4, i + 3))],
            ),
        ],
    )
}

fn card(i: usize) -> Element {
    Element::node(
        "div",
        class("card"),
        vec![Element::node(
            "div",
            class("inner"),
            vec![Element::text(format!(
                "Card {i} with a clipped, shadowed body"
            ))],
        )],
    )
}

fn window(css: &str, root: impl Fn() -> Element + 'static) -> HeadlessWindow {
    HeadlessWindow::new(css, root, HeadlessOptions::default())
        .expect("the benchmark stylesheet is valid")
}

fn rows_window(class_name: &'static str, n: usize) -> HeadlessWindow {
    window(CSS, move || list(class_name, n))
}

/// A scroll box of `n` rows that the wheel can move: the engine scrolls only an
/// element that has an `id` and is registered with `use_scroll_offset`, so a
/// plain `overflow: auto` box never scrolls.
fn scroll_window(n: usize) -> HeadlessWindow {
    window(CSS, move || {
        let _scroll = use_scroll_offset("scroll", |_, _| {});
        Element::node(
            "div",
            vec![
                ("id".into(), "scroll".into()),
                ("class".into(), "scroll".into()),
            ],
            (0..n).map(row).collect(),
        )
    })
}

/// Asserts `window` paints differently after `act`, so a workload that is meant
/// to move something cannot quietly measure a window where nothing moved.
fn assert_changes_the_frame(
    window: &mut HeadlessWindow,
    what: &str,
    act: impl FnOnce(&mut HeadlessWindow),
) {
    let before = window.frame().rgba;
    act(window);
    assert!(
        window.frame().rgba != before,
        "the {what} workload changed nothing on screen, so it would measure a window that did not move"
    );
}

/// A list whose root keeps a signal, returned with its handle.
fn signal_window(n: usize) -> (HeadlessWindow, Signal<u32>) {
    let handle: Rc<RefCell<Option<Signal<u32>>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&handle);
    let win = window(CSS, move || {
        let count = use_signal(|| 0u32);
        *sink.borrow_mut() = Some(count.clone());
        Element::node(
            "div",
            class("list"),
            std::iter::once(Element::text(format!("Count {}", count.get())))
                .chain((0..n).map(row))
                .collect(),
        )
    });
    let signal = handle
        .borrow()
        .clone()
        .expect("the root renders once on creation");
    (win, signal)
}

fn update_rows(n: usize) -> Box<dyn FnMut()> {
    let mut win = rows_window("list", n);
    Box::new(move || win.update())
}

fn frame_rows(n: usize) -> Box<dyn FnMut()> {
    let mut win = rows_window("list", n);
    Box::new(move || {
        win.update();
        black_box(win.frame());
    })
}

fn signal_update(n: usize) -> Box<dyn FnMut()> {
    let (mut win, signal) = signal_window(n);
    let mut value = 0u32;
    Box::new(move || {
        value += 1;
        signal.set(value);
        win.update();
    })
}

fn signal_frame(n: usize) -> Box<dyn FnMut()> {
    let (mut win, signal) = signal_window(n);
    let mut value = 0u32;
    Box::new(move || {
        value += 1;
        signal.set(value);
        win.update();
        black_box(win.frame());
    })
}

pub fn all() -> Vec<Workload> {
    vec![
        Workload {
            name: "update_100_rows",
            description: "A re-render of 100 rows with nothing changed",
            exercises: "render, arena build, cascade, layout",
            build: || update_rows(100),
        },
        Workload {
            name: "update_1k_rows",
            description: "A re-render of 1,000 rows with nothing changed",
            exercises: "render, arena build, cascade, layout",
            build: || update_rows(1000),
        },
        Workload {
            name: "update_10k_rows",
            description: "A re-render of 10,000 rows with nothing changed",
            exercises: "render, arena build, cascade, layout",
            build: || update_rows(10_000),
        },
        Workload {
            name: "frame_100_rows",
            description: "Update and paint of 100 rows",
            exercises: "update, paint parts, rasterization, accessibility tree",
            build: || frame_rows(100),
        },
        Workload {
            name: "frame_1k_rows",
            description: "Update and paint of 1,000 rows",
            exercises: "update, paint parts, rasterization, accessibility tree",
            build: || frame_rows(1000),
        },
        Workload {
            name: "frame_10k_rows",
            description: "Update and paint of 10,000 rows",
            exercises: "update, paint parts, rasterization, accessibility tree",
            build: || frame_rows(10_000),
        },
        Workload {
            name: "signal_update_100_rows",
            description: "One signal write read by the root, then the update, 100 rows",
            exercises: "signal write, re-render, whole-tree cascade and layout",
            build: || signal_update(100),
        },
        Workload {
            name: "signal_update_1k_rows",
            description: "One signal write read by the root, then the update, 1,000 rows",
            exercises: "signal write, re-render, whole-tree cascade and layout",
            build: || signal_update(1000),
        },
        Workload {
            name: "signal_update_10k_rows",
            description: "One signal write read by the root, then the update, 10,000 rows",
            exercises: "signal write, re-render, whole-tree cascade and layout",
            build: || signal_update(10_000),
        },
        Workload {
            name: "signal_frame_1k_rows",
            description: "A local state change shown on screen: write, update, paint, 1,000 rows",
            exercises: "the whole path from a signal write to a painted frame",
            build: || signal_frame(1000),
        },
        Workload {
            name: "resize_1k_rows",
            description: "Alternate between two window widths and paint, 1,000 rows",
            exercises: "layout at a new width, paint at a new size",
            build: || {
                let mut win = rows_window("list", 1000);
                let mut wide = false;
                Box::new(move || {
                    wide = !wide;
                    win.resize(if wide { 800.0 } else { 640.0 }, 600.0);
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "scroll_1k_rows",
            description: "One 100 px wheel tick over a scroll box of 1,000 rows, then paint",
            exercises: "wheel input, scroll offset update, repaint-only scroll, paint with clipping",
            build: || {
                let mut win = scroll_window(1000);
                win.pointer_move(400.0, 300.0);
                assert_changes_the_frame(&mut win, "scroll", |w| w.wheel(0.0, -100.0, false));
                let mut ticks = 0usize;
                Box::new(move || {
                    // Forward for 100 ticks, then back, so it never reaches either end.
                    let delta = if (ticks / 100).is_multiple_of(2) {
                        -100.0
                    } else {
                        100.0
                    };
                    ticks += 1;
                    win.wheel(0.0, delta, false);
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "hover_1k_rows",
            description: "Move the pointer to another row and paint, 1,000 rows",
            exercises: "hit test, hover state, restyle of the changed rows, paint",
            build: || {
                let mut win = rows_window("list", 1000);
                // Over the row's own padding (x = 4), not over a child: the engine
                // styles only the node under the pointer, so a pointer over the
                // label would restyle nothing.
                assert_changes_the_frame(&mut win, "hover", |w| {
                    w.pointer_move(4.0, 10.0);
                    w.pointer_move(4.0, 200.0);
                });
                let mut tick = 0usize;
                Box::new(move || {
                    tick += 1;
                    win.pointer_move(4.0, 10.0 + ((tick * 37) % 580) as f32);
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "deep_100_levels",
            description: "Update and paint of a row nested 100 levels deep",
            exercises: "deep cascade inheritance, deep layout, deep paint",
            build: || {
                let mut win = window(CSS, || {
                    let mut node = row(0);
                    for _ in 0..100 {
                        node = Element::node("div", class("nest"), vec![node]);
                    }
                    node
                });
                Box::new(move || {
                    win.update();
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "text_200_paragraphs",
            description: "Update and paint of 200 wrapped paragraphs with inline spans",
            exercises: "inline shaping, line breaking, text paint",
            build: || {
                let mut win = window(CSS, || {
                    Element::node("div", class("doc"), (0..200).map(paragraph).collect())
                });
                Box::new(move || {
                    win.update();
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "text_reflow_200_paragraphs",
            description: "Alternate between two widths so 200 paragraphs reflow, then paint",
            exercises: "line breaking at a new width, text paint",
            build: || {
                let mut win = window(CSS, || {
                    Element::node("div", class("doc"), (0..200).map(paragraph).collect())
                });
                let mut wide = false;
                Box::new(move || {
                    wide = !wide;
                    win.resize(if wide { 800.0 } else { 520.0 }, 600.0);
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "clips_shadows_500_cards",
            description: "Update and paint of 500 cards with nested clips, radii and shadows",
            exercises: "clip masks, rounded corners, box-shadow blur",
            build: || {
                let mut win = window(CSS, || {
                    Element::node("div", class("cards"), (0..500).map(card).collect())
                });
                Box::new(move || {
                    win.update();
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "startup_1k_rows",
            description: "Create a window with 1,000 rows and paint its first frame",
            exercises: "stylesheet parse, first render, first layout, first paint, font and cache warm-up",
            build: || {
                Box::new(|| {
                    let mut win = rows_window("list", 1000);
                    black_box(win.frame());
                })
            },
        },
    ]
}

pub fn find(name: &str) -> Option<Workload> {
    all().into_iter().find(|w| w.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scroll_and_hover_workloads_really_move_something() {
        // Building each one asserts the window paints differently after its
        // first action; one that moved nothing would panic here.
        for name in ["scroll_1k_rows", "hover_1k_rows"] {
            let build = find(name).expect("the workload exists").build;
            let mut op = build();
            op();
        }
    }
}
