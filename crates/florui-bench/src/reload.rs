//! A stylesheet swap, the engine's share of a CSS hot reload: parse the new
//! text, install it, update and paint. Reading the file and the watcher are not
//! here; `florui-bench edit-latency` measures those against a real window.
//!
//! Each sample swaps between two stylesheets that differ in row padding, so
//! layout has to run every time, not only the cascade. Building a workload
//! asserts that the swap changes what is painted.

use std::hint::black_box;

use florui_platform::HeadlessWindow;

use crate::workloads::{Workload, assert_changes_the_frame, list, window};

fn stylesheet(padding: u32) -> String {
    format!(
        ".list {{ display: flex; flex-direction: column; width: 800px; }}
         .row {{ display: flex; flex-direction: row; gap: 8px; padding: {padding}px 8px;
                border-bottom: 1px solid #ddd; background-color: #fff; }}
         .label {{ flex-grow: 1; color: #222; font-size: 14px; }}
         .action {{ padding: 2px 8px; background-color: #36c; color: #fff; }}"
    )
}

fn swap(window: &mut HeadlessWindow, css: &str) {
    let rules = florui_style::parse_stylesheet(css).expect("the benchmark stylesheet is valid");
    window.runtime_mut().set_rules(rules);
    window.update();
}

fn reload_rows(n: usize) -> Box<dyn FnMut()> {
    let (narrow, wide) = (stylesheet(4), stylesheet(6));
    let mut win = window(&narrow, move || list("list", n));
    black_box(win.frame());
    assert_changes_the_frame(&mut win, "stylesheet reload", |w| swap(w, &wide));
    let mut next_is_narrow = true;
    Box::new(move || {
        swap(&mut win, if next_is_narrow { &narrow } else { &wide });
        next_is_narrow = !next_is_narrow;
        black_box(win.frame());
    })
}

pub fn all() -> Vec<Workload> {
    vec![
        Workload {
            name: "css_reload_100_rows",
            description: "A stylesheet swap that changes layout, then paint, 100 rows",
            exercises: "stylesheet parse, restyle, layout, paint",
            build: || reload_rows(100),
        },
        Workload {
            name: "css_reload_1k_rows",
            description: "A stylesheet swap that changes layout, then paint, 1,000 rows",
            exercises: "stylesheet parse, restyle, layout, paint",
            build: || reload_rows(1000),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sample_changes_what_is_painted() {
        for workload in all() {
            let mut op = (workload.build)();
            for _ in 0..3 {
                op();
            }
        }
    }

    #[test]
    fn the_two_stylesheets_differ_in_what_the_window_paints() {
        let (narrow, wide) = (stylesheet(4), stylesheet(6));
        let mut win = window(&narrow, || list("list", 5));
        let before = win.frame().rgba;
        swap(&mut win, &wide);
        assert_ne!(win.frame().rgba, before);
        swap(&mut win, &narrow);
        assert_eq!(win.frame().rgba, before, "and back again");
    }
}
