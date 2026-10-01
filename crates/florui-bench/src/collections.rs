//! Workloads for collections and for what a component owns: a virtualized
//! list at several data sizes and with fixed and variable row heights, and a
//! tree of components mounted and unmounted again and again.
//!
//! The virtualized lists answer one question the plain workloads cannot: does
//! the cost of a frame follow the data or the rows on screen? The same row
//! view is mounted over 1,000, 10,000 and 100,000 items, so a difference
//! between those workloads is the cost of the data size alone; what a frame
//! mounts is the same in all of them (the tests check it, and the profiler's
//! node counters report it, see `florui-bench phases`). Fixed and variable
//! heights are separate workloads because a variable list measures rows after
//! they mount and corrects its estimates.
//!
//! The mount cycle owns what rows own in a real application: a signal, a
//! memo and an effect with a cleanup in each of a few hundred components. The
//! tests check that every one of them is released when the tree unmounts,
//! which timing alone cannot show.

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;

use florui::Element;
use florui_platform::{
    HeadlessOptions, HeadlessWindow, ItemHeight, Overscan, VirtualListHandle, use_virtual_list,
};
use florui_reactive::{
    Cleanup, Key, Signal, use_child_scope_keyed, use_effect, use_memo, use_signal,
};

use crate::workloads::Workload;

const CSS: &str = "
    .vscroll { width: 800px; height: 600px; overflow-y: auto; }
    .vrow { display: flex; flex-direction: row; gap: 8px; height: 31px; padding: 0px 8px;
            border-bottom: 1px solid #ddd; background-color: #fff; align-items: center; }
    .vtall { width: 400px; padding: 4px 8px; border-bottom: 1px solid #ddd;
             background-color: #fff; color: #222; font-size: 14px; }
    .vlabel { flex-grow: 1; color: #222; font-size: 14px; }
    .vaction { padding: 2px 8px; background-color: #36c; color: #fff; }
";

/// How tall a fixed row is: its height plus the 1 px border under it.
const FIXED_ROW: f32 = 32.0;

fn class(name: &str) -> Vec<(String, String)> {
    vec![("class".into(), name.into())]
}

fn window(root: impl Fn() -> Element + 'static) -> HeadlessWindow {
    HeadlessWindow::new(CSS, root, HeadlessOptions::default())
        .expect("the benchmark stylesheet is valid")
}

/// A row of fixed height: a label and a button, like the plain list's rows.
fn fixed_row(i: usize) -> Element {
    Element::node(
        "div",
        class("vrow"),
        vec![
            Element::node(
                "span",
                class("vlabel"),
                vec![Element::text(format!("Item number {i}"))],
            ),
            Element::node("button", class("vaction"), vec![Element::text("Open")]),
        ],
    )
}

/// A row whose height depends on its text, which wraps to one to four lines.
fn variable_row(i: usize) -> Element {
    let words = [
        "layout",
        "shaping",
        "cascade",
        "baseline",
        "glyph",
        "wrapping",
        "paragraph",
        "inline",
    ];
    let count = 3 + (i * 7) % 22;
    let text = (0..count)
        .map(|w| words[(i * 5 + w * 3) % words.len()])
        .collect::<Vec<_>>()
        .join(" ");
    Element::node("div", class("vtall"), vec![Element::text(text)])
}

/// A scrollable virtualized list of `count` items. Returns the window and the
/// list's handle.
fn virtual_window(count: usize, variable: bool) -> (HeadlessWindow, VirtualListHandle) {
    let handle: Rc<RefCell<Option<VirtualListHandle>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&handle);
    let win = window(move || {
        let height = if variable {
            ItemHeight::Variable { estimate: 48.0 }
        } else {
            ItemHeight::Fixed(FIXED_ROW)
        };
        let (content, list) = use_virtual_list(
            "vlist",
            count,
            (),
            height,
            Overscan::default(),
            Key::from,
            move |i| {
                if variable {
                    variable_row(i)
                } else {
                    fixed_row(i)
                }
            },
        );
        *sink.borrow_mut() = Some(list);
        Element::node(
            "div",
            vec![
                ("id".into(), "vlist".into()),
                ("class".into(), "vscroll".into()),
                ("role".into(), "list".into()),
                ("accessible_label".into(), "Items".into()),
            ],
            vec![content],
        )
    });
    let list = handle
        .borrow()
        .clone()
        .expect("the root renders once on creation");
    (win, list)
}

/// How many rows are mounted right now.
fn mounted_rows(win: &HeadlessWindow) -> usize {
    let (arena, ..) = win.runtime().geometry();
    arena
        .find_all(|a, id| a.classes(id).iter().any(|c| c == "vrow" || c == "vtall"))
        .len()
}

/// A virtualized list under a wheel: one 100 px tick, then a paint. It goes
/// forward for 100 ticks and then back, so it never reaches either end.
fn virtual_scroll(count: usize, variable: bool) -> Box<dyn FnMut()> {
    let (mut win, _list) = virtual_window(count, variable);
    win.pointer_move(400.0, 300.0);
    let before = win.frame().rgba;
    win.wheel(0.0, -100.0, false);
    assert!(
        win.frame().rgba != before,
        "the virtual list workload did not move on a wheel tick, so it would measure a still list"
    );
    let mounted = mounted_rows(&win);
    assert!(
        (5..=80).contains(&mounted),
        "a list of {count} mounted {mounted} rows; it should mount a bounded window"
    );
    let mut ticks = 0usize;
    Box::new(move || {
        let delta = if (ticks / 100).is_multiple_of(2) {
            -100.0
        } else {
            100.0
        };
        ticks += 1;
        win.wheel(0.0, delta, false);
        black_box(win.frame());
    })
}

/// A far jump in a list of variable rows: scroll to an item thousands of rows
/// away, where nothing has been measured, then paint, so the estimates are
/// corrected as the rows mount.
fn virtual_jump(count: usize) -> Box<dyn FnMut()> {
    let (mut win, list) = virtual_window(count, true);
    win.pointer_move(400.0, 300.0);
    let before = win.frame().rgba;
    list.scroll_to_item(count / 2);
    win.update();
    assert!(
        win.frame().rgba != before,
        "the jump workload did not move the list"
    );
    let mut next = 0usize;
    Box::new(move || {
        // Far from the last stop each time, spread over the whole dataset.
        next = (next + count * 3 / 7 + 11) % count;
        list.scroll_to_item(next);
        win.update();
        black_box(win.frame());
    })
}

const COMPONENTS: usize = 300;

/// One component with what a row of a real list owns: a signal, a memo and an
/// effect with a cleanup.
fn component(i: usize) -> Element {
    let value = use_signal(move || i);
    let label = use_memo(i, |i| format!("Item number {i}"));
    use_effect((), || Some(Box::new(|| {}) as Cleanup));
    Element::node(
        "div",
        class("vrow"),
        vec![
            Element::node("span", class("vlabel"), vec![Element::text(label)]),
            Element::node(
                "button",
                class("vaction"),
                vec![Element::text(value.get().to_string())],
            ),
        ],
    )
}

/// A window whose root shows `n` keyed components, with the signal that
/// shows and hides them.
fn component_window(n: usize) -> (HeadlessWindow, Signal<bool>) {
    let handle: Rc<RefCell<Option<Signal<bool>>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&handle);
    let win = window(move || {
        let shown = use_signal(|| true);
        *sink.borrow_mut() = Some(shown.clone());
        let children = if shown.get() {
            (0..n)
                .map(|i| use_child_scope_keyed(Key::from(i), || component(i)))
                .collect()
        } else {
            Vec::new()
        };
        Element::node("div", class("vscroll"), children)
    });
    let shown = handle
        .borrow()
        .clone()
        .expect("the root renders once on creation");
    (win, shown)
}

/// Unmounts the components and mounts them again, painting each time.
fn mount_cycle() -> Box<dyn FnMut()> {
    let (mut win, shown) = component_window(COMPONENTS);
    let before = win.frame().rgba;
    shown.set(false);
    win.update();
    assert!(
        win.frame().rgba != before,
        "unmounting the components changed nothing on screen"
    );
    shown.set(true);
    win.update();
    // One sample is a whole cycle, so each is the same work; alternating
    // between the two halves would make the median a mix of a cheap and an
    // expensive one.
    Box::new(move || {
        shown.set(false);
        win.update();
        shown.set(true);
        win.update();
        black_box(win.frame());
    })
}

pub fn all() -> Vec<Workload> {
    vec![
        Workload {
            name: "virtual_list_1k_fixed",
            description: "One 100 px wheel tick over a virtualized list of 1,000 fixed-height rows, then paint",
            exercises: "wheel input, mounting and unmounting the rows that enter and leave, keyed row scopes, paint",
            build: || virtual_scroll(1_000, false),
        },
        Workload {
            name: "virtual_list_10k_fixed",
            description: "The same list over 10,000 items",
            exercises: "as the 1,000-item list; a difference between the two is the cost of the data size",
            build: || virtual_scroll(10_000, false),
        },
        Workload {
            name: "virtual_list_100k_fixed",
            description: "The same list over 100,000 items",
            exercises: "as the 1,000-item list; a difference between the two is the cost of the data size",
            build: || virtual_scroll(100_000, false),
        },
        Workload {
            name: "virtual_list_10k_variable",
            description: "One 100 px wheel tick over a virtualized list of 10,000 rows whose height depends on their text, then paint",
            exercises: "as the fixed list, plus measuring rows after they mount and correcting the estimates",
            build: || virtual_scroll(10_000, true),
        },
        Workload {
            name: "virtual_list_jump_10k_variable",
            description: "Jump to an item thousands of rows away in a list of 10,000 variable rows, then paint",
            exercises: "scroll-to-item, a viewport of rows that were never measured, estimate corrections",
            build: || virtual_jump(10_000),
        },
        Workload {
            name: "mount_unmount_300_components",
            description: "Unmount 300 components that each keep a signal, a memo and an effect, mount them again, then paint",
            exercises: "mounting and disposing keyed component scopes, hooks, effect cleanups, render of the rows",
            build: mount_cycle,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use florui_reactive::live::live_counts;

    /// The row at `index` among the mounted ones, in document order.
    fn row_boxes(win: &mut HeadlessWindow) -> Vec<(f32, f32)> {
        let rows: Vec<usize> = {
            let (arena, ..) = win.runtime().geometry();
            arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "vrow"))
        };
        let bounds = win.frame().bounds;
        rows.iter()
            .map(|id| {
                let (_, y, _, h) = bounds[id];
                (y, h)
            })
            .collect()
    }

    #[test]
    fn a_fixed_row_is_as_tall_as_the_list_assumes() {
        // The virtual list places rows by the height it was told; a row that
        // measures differently would make every workload below drift.
        let (mut win, _list) = virtual_window(1_000, false);
        win.update();
        let boxes = row_boxes(&mut win);
        assert!(boxes.len() >= 5, "{} rows", boxes.len());
        for pair in boxes.windows(2) {
            assert_eq!(pair[1].0 - pair[0].0, FIXED_ROW, "{boxes:?}");
        }
        assert_eq!(
            boxes[0].1, FIXED_ROW,
            "the border box is 31 px plus the 1 px border"
        );
    }

    #[test]
    fn a_virtual_list_mounts_a_window_that_does_not_depend_on_the_data_size() {
        let mut seen = Vec::new();
        for count in [1_000, 10_000, 100_000] {
            let before = live_counts();
            let (mut win, _list) = virtual_window(count, false);
            win.update();
            let owned = live_counts().since(before);
            seen.push((count, mounted_rows(&win), owned.scopes));
        }
        let (_, rows, scopes) = seen[0];
        assert!((15..=40).contains(&rows), "{seen:?}");
        for &(count, r, s) in &seen {
            assert_eq!(
                r, rows,
                "{count} items mount a different number of rows: {seen:?}"
            );
            assert_eq!(
                s, scopes,
                "{count} items keep a different number of scopes: {seen:?}"
            );
        }
    }

    #[test]
    fn scrolling_away_and_back_does_not_accumulate_row_state() {
        let before = live_counts();
        let (mut win, _list) = virtual_window(10_000, false);
        win.pointer_move(400.0, 300.0);
        win.update();
        let at_rest = live_counts().since(before);
        let mut peak = at_rest;
        for tick in 0..400 {
            let delta = if tick < 200 { -100.0 } else { 100.0 };
            win.wheel(0.0, delta, false);
            win.frame();
            let now = live_counts().since(before);
            peak.scopes = peak.scopes.max(now.scopes);
            peak.signals = peak.signals.max(now.signals);
            peak.effect_cleanups = peak.effect_cleanups.max(now.effect_cleanups);
        }
        let end = live_counts().since(before);
        // Rows enter and leave, so the count moves with the overscan, but it is
        // the mounted window that is held, never every row that scrolled by.
        assert!(
            peak.scopes <= at_rest.scopes + 12,
            "scopes grew from {} to {} over 400 ticks",
            at_rest.scopes,
            peak.scopes
        );
        assert!(
            end.scopes <= at_rest.scopes + 12 && end.signals <= at_rest.signals + 12,
            "after scrolling away and back: {end:?} against {at_rest:?}"
        );
    }

    #[test]
    fn unmounting_components_releases_everything_they_owned() {
        // What an unmounted tree leaves behind, measured with nothing mounted.
        let empty = {
            let before = live_counts();
            let (mut win, _shown) = component_window(0);
            win.update();
            live_counts().since(before)
        };

        let before = live_counts();
        let (mut win, shown) = component_window(50);
        win.update();
        let mounted = live_counts().since(before);
        assert!(
            mounted.scopes >= empty.scopes + 50,
            "50 components own scopes: {mounted:?} against {empty:?}"
        );
        assert_eq!(
            mounted.effect_cleanups, 50,
            "one effect cleanup each: {mounted:?}"
        );

        for cycle in 0..5 {
            shown.set(false);
            win.update();
            assert_eq!(
                live_counts().since(before),
                empty,
                "cycle {cycle}: after unmounting, everything but the root is released"
            );
            shown.set(true);
            win.update();
            assert_eq!(
                live_counts().since(before),
                mounted,
                "cycle {cycle}: mounting again owns what it owned the first time"
            );
        }
    }

    #[test]
    fn every_collection_workload_builds_and_runs() {
        // Building each asserts that it moves something and that a list mounts a
        // bounded window.
        for workload in all() {
            let mut op = (workload.build)();
            op();
            op();
        }
    }
}
