//! `direction` through a window: where the text and the markers are drawn.

use florui::prelude::*;
use florui_test::Harness;

/// The columns of the first 200 pixels of the frame, the width of the box under test, that
/// hold a pixel other than the background, as `(first, last)`. The background is the
/// colour of the last pixel of the frame, which is outside the box.
fn ink_columns(rgba: &[u8], width: u32, height: u32) -> Option<(u32, u32)> {
    let corner = (((height - 1) * width + width - 1) * 4) as usize;
    let background = rgba[corner..corner + 3].to_vec();
    let mut found: Option<(u32, u32)> = None;
    for y in 0..height {
        for x in 0..width.min(200) {
            let at = ((y * width + x) * 4) as usize;
            if rgba[at..at + 3] != background[..] {
                found = Some(match found {
                    Some((first, last)) => (first.min(x), last.max(x)),
                    None => (x, x),
                });
            }
        }
    }
    found
}

fn frame_of(css: &str, view: impl Fn() -> Element + 'static) -> (Vec<u8>, u32, u32) {
    let mut mounted = Harness::new(view).css(css).viewport(240.0, 60.0).mount();
    let frame = mounted.frame();
    (frame.rgba, frame.width, frame.height)
}

const PAGE: &str = "body { margin: 0; } \
    .row { width: 200px; font-size: 14px; color: #000000; }";

#[test]
fn right_to_left_text_is_drawn_against_the_right_edge_of_its_box() {
    let (ltr, w, h) = frame_of(PAGE, || view! { <div class="row">{"abc def"}</div> });
    let (rtl, _, _) = frame_of(&format!("{PAGE} .row {{ direction: rtl; }}"), || {
        view! { <div class="row">{"abc def"}</div> }
    });
    let (l_first, l_last) = ink_columns(&ltr, w, h).unwrap();
    let (r_first, r_last) = ink_columns(&rtl, w, h).unwrap();
    assert!(
        l_first < 4,
        "left to right text starts at the left edge: {l_first}"
    );
    assert!(l_last < 80, "{l_last}");
    assert!(
        r_last > 195 && r_last <= 200,
        "ends at the right edge: {r_last}"
    );
    assert!(r_first > 120, "{r_first}");
}

#[test]
fn text_align_left_is_the_left_edge_even_in_a_right_to_left_box() {
    let (rtl, w, h) = frame_of(
        &format!("{PAGE} .row {{ direction: rtl; text-align: left; }}"),
        || view! { <div class="row">{"abc def"}</div> },
    );
    let (first, _) = ink_columns(&rtl, w, h).unwrap();
    assert!(first < 4, "{first}");
}

#[test]
fn the_bullets_of_a_right_to_left_list_are_on_the_right() {
    let css =
        format!("{PAGE} ul {{ margin: 0; padding: 0 24px 0 0; direction: rtl; width: 200px; }}");
    let (frame, w, h) = frame_of(&css, || {
        view! { <ul><li>{"first item"}</li></ul> }
    });
    let (_, last) = ink_columns(&frame, w, h).unwrap();
    // The marker is in the padding on the right of the text, which ends 24px in.
    assert!(last > 176, "{last}");
}
