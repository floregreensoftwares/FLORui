//! `::placeholder` of a text field through a window: the hint drawn in the color
//! and size of its rule.

use florui::prelude::*;
use florui_test::Harness;

fn field() -> Element {
    view! { <input id="f" type="text" placeholder="Hint" /> }
}

fn count(frame: &florui_platform::HeadlessFrame, accepts: impl Fn([u8; 3]) -> bool) -> usize {
    frame
        .rgba
        .chunks_exact(4)
        .filter(|p| accepts([p[0], p[1], p[2]]))
        .count()
}

fn harness(css: &str) -> Harness {
    Harness::new(field)
        .css(&format!(
            "input {{ width: 140px; height: 30px; border: none; padding: 0px; \
             background-color: #ffffff; color: #000000; }} {css}"
        ))
        .viewport(160.0, 40.0)
}

#[test]
fn a_placeholder_rule_changes_the_color_and_the_size_of_the_hint() {
    let mut plain = harness("").mount();
    let mut styled = harness("input::placeholder { color: #ff0000; font-size: 24px }").mount();
    let plain_frame = plain.frame();
    let styled_frame = styled.frame();
    let red = |[r, g, b]: [u8; 3]| r > 200 && g < 60 && b < 60;
    assert_eq!(count(&plain_frame, red), 0, "the default is gray");
    assert!(
        count(&styled_frame, red) > 40,
        "{}",
        count(&styled_frame, red)
    );
    let ink = |[r, g, b]: [u8; 3]| u16::from(r) + u16::from(g) + u16::from(b) < 600;
    assert!(
        count(&styled_frame, ink) > count(&plain_frame, ink),
        "a 24px hint covers more than a 16px one"
    );
}
