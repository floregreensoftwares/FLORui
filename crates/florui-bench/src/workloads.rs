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
    .tag { padding: 0 6px; border: 1px solid #36c; margin: 0 4px; background-color: #dfe6ff; border-radius: 4px; }
    .outer { font-size: 17px; background-color: #fff3d0; text-decoration: underline; }
    .inner-span { font-size: 12px; color: #a30; vertical-align: super; }
    .chip { display: inline-block; padding: 2px 6px; background-color: #eef; }
    .bar { height: 6px; background-color: #36c; }
    .justified { text-align: justify; text-indent: 24px; }
    .cards { display: flex; flex-direction: column; width: 800px; }
    .card { overflow: hidden; border-radius: 8px; margin: 6px; padding: 8px;
            background-color: #fff; box-shadow: 0px 2px 8px rgba(0, 0, 0, 0.3); }
    .inner { overflow: hidden; border-radius: 4px; padding: 4px; background-color: #f4f4f8; }
    .pgroups { display: flex; flex-direction: column; width: 100%; }
    .pgroup { display: grid; grid-template-columns: repeat(3, 1fr); grid-template-rows: auto auto auto;
              gap: 6px 8px; margin-bottom: 8px; }
    .pcard { grid-row: span 3; display: grid; padding: 4px; background-color: #f4f4f8; font-size: 13px; }
    .pcard.shared { grid-template-rows: subgrid; }
    .ptitle { font-weight: 700; } .pbody { color: #333; } .pfoot { color: #666; }
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

pub(crate) fn list(name: &str, n: usize) -> Element {
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

/// A paragraph of nested spans with boxes of their own: padding, border,
/// margin, background, underline and a raised inner span.
fn nested_paragraph(i: usize) -> Element {
    Element::node(
        "p",
        class("para justified"),
        vec![
            Element::text(format!(
                "Paragraph {i} opens with some plain words and then "
            )),
            Element::node(
                "span",
                class("outer"),
                vec![
                    Element::text("an outer span that holds "),
                    Element::node(
                        "span",
                        class("inner-span"),
                        vec![Element::text("a raised one")],
                    ),
                    Element::text(" and more text"),
                ],
            ),
            Element::text(" before a "),
            Element::node(
                "span",
                class("tag"),
                vec![Element::text("tagged phrase that wraps")],
            ),
            Element::text(" and a closing run of words to fill the line out."),
        ],
    )
}

/// A paragraph with an inline-block holding spans, and a block child between
/// two runs of text, so the context has atomic boxes with content and anonymous blocks.
fn mixed_paragraph(i: usize) -> Element {
    Element::node(
        "div",
        class("para"),
        vec![
            Element::text(format!("Item {i} starts with text and ")),
            Element::node(
                "span",
                class("chip"),
                vec![
                    Element::node("span", class("em"), vec![Element::text("big")]),
                    Element::text(" small chip"),
                ],
            ),
            Element::text(" then more words follow"),
            Element::node("div", class("bar"), vec![]),
            Element::text(
                "and the text after the bar wraps over a couple of lines of the paragraph",
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

/// A grid of three cards, each of three rows of text, whose rows are its own or,
/// with `shared`, the rows of the grid.
fn card_group(i: usize, shared: bool) -> Element {
    let card = |n: usize| {
        let lines = |k: usize| "a few words of the card ".repeat(1 + (i + n + k) % 3);
        Element::node(
            "div",
            class(if shared { "pcard shared" } else { "pcard" }),
            vec![
                Element::node(
                    "div",
                    class("ptitle"),
                    vec![Element::text(format!("Card {i}.{n}"))],
                ),
                Element::node("div", class("pbody"), vec![Element::text(lines(1))]),
                Element::node("div", class("pfoot"), vec![Element::text(lines(2))]),
            ],
        )
    };
    Element::node("div", class("pgroup"), (0..3).map(card).collect())
}

pub(crate) fn window(css: &str, root: impl Fn() -> Element + 'static) -> HeadlessWindow {
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
pub(crate) fn assert_changes_the_frame(
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
    let mut all = frame_pipeline();
    all.extend(crate::collections::all());
    all.extend(crate::effects::all());
    all.extend(crate::reload::all());
    all
}

fn frame_pipeline() -> Vec<Workload> {
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
                // Over the row's own padding (x = 4), so the row itself is the hit node
                // and a row change is what the pointer moves between.
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
            name: "text_nested_spans_200_paragraphs",
            description: "Update and paint of 200 justified paragraphs with nested spans that have padding, borders and backgrounds",
            exercises: "nested inline elements, per-element boxes, vertical-align, justify, text-indent",
            build: || {
                let mut win = window(CSS, || {
                    Element::node(
                        "div",
                        class("doc"),
                        (0..200).map(nested_paragraph).collect(),
                    )
                });
                Box::new(move || {
                    win.update();
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "text_mixed_blocks_200_paragraphs",
            description: "Update and paint of 200 paragraphs with an inline-block holding spans and a block between two runs of text",
            exercises: "inline-block with content, anonymous blocks, nested layout",
            build: || {
                let mut win = window(CSS, || {
                    Element::node("div", class("doc"), (0..200).map(mixed_paragraph).collect())
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
            name: "grid_cards_200_groups",
            description: "Resize and paint 200 grids of three cards, each with rows of its own",
            exercises: "grid layout of nested grids, text in grid items",
            build: || {
                let mut win = window(CSS, || {
                    Element::node(
                        "div",
                        class("pgroups"),
                        (0..200).map(|i| card_group(i, false)).collect(),
                    )
                });
                let mut wide = false;
                Box::new(move || {
                    wide = !wide;
                    win.resize(if wide { 800.0 } else { 790.0 }, 600.0);
                    black_box(win.frame());
                })
            },
        },
        Workload {
            name: "subgrid_cards_200_groups",
            description: "Resize and paint 200 grids of three cards that share the rows of their grid through subgrid",
            exercises: "subgrid: the probe and the relayouts around it",
            build: || {
                let mut win = window(CSS, || {
                    Element::node(
                        "div",
                        class("pgroups"),
                        (0..200).map(|i| card_group(i, true)).collect(),
                    )
                });
                let mut wide = false;
                Box::new(move || {
                    wide = !wide;
                    win.resize(if wide { 800.0 } else { 790.0 }, 600.0);
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
