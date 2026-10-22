//! `writing-mode` through a window: where the lines of vertical boxes are drawn.

use florui::prelude::*;
use florui_test::Harness;

/// The pixels that are not the background (the colour of the last pixel of the frame), as
/// `(left, top, right, bottom)` inclusive.
fn ink(rgba: &[u8], width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
    let corner = (((height - 1) * width + width - 1) * 4) as usize;
    let background = rgba[corner..corner + 3].to_vec();
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..height {
        for x in 0..width {
            let at = ((y * width + x) * 4) as usize;
            if rgba[at..at + 3] != background[..] {
                found = Some(match found {
                    Some((l, t, r, b)) => (l.min(x), t.min(y), r.max(x), b.max(y)),
                    None => (x, y, x, y),
                });
            }
        }
    }
    found
}

fn ink_of(css: &str, view: impl Fn() -> Element + 'static) -> (u32, u32, u32, u32) {
    let mut mounted = Harness::new(view).css(css).viewport(160.0, 160.0).mount();
    let frame = mounted.frame();
    ink(&frame.rgba, frame.width, frame.height).expect("something is drawn")
}

const PAGE: &str = "body { margin: 0; } \
    .box { height: 100px; font-size: 14px; line-height: 18px; color: #000000; }";

#[test]
fn vertical_text_runs_down_the_page_and_is_tall_rather_than_wide() {
    let (left, top, right, bottom) = ink_of(
        &format!("{PAGE} .box {{ writing-mode: vertical-rl; width: 18px; }}"),
        || view! { <div class="box">{"abc def ghi"}</div> },
    );
    assert!(
        right < 20 && left < 6,
        "inside the 18px column: {left}..{right}"
    );
    assert!(top < 4, "starts at the top: {top}");
    assert!(
        bottom - top > 50 && bottom < 100,
        "runs down: {top}..{bottom}"
    );
}

#[test]
fn the_lines_of_a_vertical_rl_box_stack_from_the_right_and_the_lr_ones_from_the_left() {
    let rl = ink_of(
        &format!("{PAGE} .box {{ writing-mode: vertical-rl; width: 100px; }}"),
        || view! { <div class="box">{"abc"}</div> },
    );
    let lr = ink_of(
        &format!("{PAGE} .box {{ writing-mode: vertical-lr; width: 100px; }}"),
        || view! { <div class="box">{"abc"}</div> },
    );
    assert!(rl.0 > 70, "against the right edge: {rl:?}");
    assert!(lr.2 < 30, "against the left edge: {lr:?}");
}

#[test]
fn upright_letters_stand_one_under_the_other() {
    let sideways = ink_of(
        &format!("{PAGE} .box {{ writing-mode: vertical-rl; width: 18px; }}"),
        || view! { <div class="box">{"upright"}</div> },
    );
    let upright = ink_of(
        &format!(
            "{PAGE} .box {{ writing-mode: vertical-rl; width: 18px; text-orientation: upright; }}"
        ),
        || view! { <div class="box">{"upright"}</div> },
    );
    // Seven letters, each in a cell as tall as the font: a good deal longer than the word.
    assert!(
        upright.3 - upright.1 > 2 * (sideways.3 - sideways.1),
        "{upright:?} {sideways:?}"
    );
    // And narrow, as a column of letters is.
    assert!(upright.2 - upright.0 < 12, "{upright:?}");
}

#[test]
fn the_lines_of_a_sideways_lr_box_start_at_the_bottom() {
    let (left, top, right, bottom) = ink_of(
        &format!("{PAGE} .box {{ writing-mode: sideways-lr; width: 18px; }}"),
        || view! { <div class="box">{"abc"}</div> },
    );
    assert!(bottom > 90, "starts at the bottom: {bottom}");
    assert!(top > 60, "and runs up a short way: {top}");
    assert!(right < 20 && left < 6, "inside the column: {left}..{right}");
}
