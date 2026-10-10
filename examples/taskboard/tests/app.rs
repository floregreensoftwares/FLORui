//! The whole application, mounted headless and used the way a person would:
//! going between screens, picking and adding cards, filtering the stack and
//! changing a setting. Nothing is reached into; every step is a click or a key
//! on something drawn.

use florui_test::{Harness, Mounted, by_class, by_id, by_role_named, by_text};
use taskboard::app::{App, AppProps};

fn app() -> Mounted {
    Harness::new(|| App(AppProps {}))
        .css(&taskboard::stylesheet())
        .viewport(1180.0, 760.0)
        .mount()
}

fn go(mounted: &mut Mounted, id: &str) {
    let key = mounted.get(by_id(id));
    mounted.click(key);
}

fn heading(mounted: &mut Mounted) -> String {
    let title = mounted.get(by_class("title"));
    let _ = title;
    let titles = mounted.all(by_class("title"));
    // The first title is the brand in the lid; the second is the screen's.
    mounted.text(titles[1])
}

#[test]
fn it_starts_on_the_desk_and_the_keys_go_to_the_other_screens() {
    let mut mounted = app();
    assert_eq!(heading(&mut mounted), "On the desk");
    go(&mut mounted, "nav-stack");
    assert_eq!(heading(&mut mounted), "The stack");
    go(&mut mounted, "nav-settings");
    assert_eq!(heading(&mut mounted), "Settings");
    go(&mut mounted, "nav-desk");
    assert_eq!(heading(&mut mounted), "On the desk");
}

#[test]
fn the_key_of_the_screen_you_are_on_is_the_one_pressed_in() {
    let mut mounted = app();
    go(&mut mounted, "nav-stack");
    let pressed = mounted.all(by_class("on"));
    assert_eq!(
        pressed.len(),
        3,
        "the nav key, the status key `All` and the default priority `Normal`"
    );
    assert_eq!(mounted.text(pressed[0]), "Stack");
}

#[test]
fn the_stack_shows_a_few_cards_not_two_thousand() {
    let mut mounted = app();
    go(&mut mounted, "nav-stack");
    let cards = mounted.all(by_class("paper-card"));
    assert!(
        (4..=40).contains(&cards.len()),
        "a screenful of cards is mounted, not the whole stack: {}",
        cards.len()
    );
}

#[test]
fn picking_a_card_shows_it_on_the_index_card() {
    let mut mounted = app();
    go(&mut mounted, "nav-stack");
    assert!(
        mounted
            .query(by_text("Pick a card from the stack to read it here."))
            .is_some()
    );
    let row = mounted.all(by_class("task-row"))[0];
    let title = {
        let heading = mounted.all(by_class("heading"));
        mounted.text(heading[0])
    };
    mounted.click(row);
    assert!(
        mounted
            .query(by_text("Pick a card from the stack to read it here."))
            .is_none(),
        "the placeholder is gone"
    );
    let detail = mounted.get(by_class("detail-title"));
    assert_eq!(
        mounted.text(detail),
        title,
        "the card you picked is the one shown"
    );
}

#[test]
fn a_new_card_goes_to_the_top_and_is_picked() {
    let mut mounted = app();
    go(&mut mounted, "nav-stack");
    let field = mounted.get(by_id("new-title"));
    mounted.click(field);
    mounted.type_text("Wax the workbench");
    let add = mounted.get(by_text("Add to the stack"));
    mounted.click(add);

    let detail = mounted.get(by_class("detail-title"));
    assert_eq!(mounted.text(detail), "Wax the workbench");
    let first = mounted.all(by_class("heading"));
    assert_eq!(
        mounted.text(first[0]),
        "Wax the workbench",
        "at the top of the stack"
    );
    // The line for the next one is empty again.
    let cleared = mounted.get(by_id("new-title"));
    let _ = cleared;
}

#[test]
fn a_blank_title_adds_nothing() {
    let mut mounted = app();
    go(&mut mounted, "nav-stack");
    let field = mounted.get(by_id("new-title"));
    mounted.click(field);
    mounted.type_text("   ");
    let add = mounted.get(by_text("Add to the stack"));
    mounted.click(add);
    assert!(
        mounted.query(by_class("detail-title")).is_none(),
        "nothing was added or picked"
    );
}

#[test]
fn the_status_keys_narrow_the_stack() {
    let mut mounted = app();
    go(&mut mounted, "nav-stack");
    let readout = |mounted: &mut Mounted| {
        let all = mounted.all(by_class("readout"));
        all.iter()
            .map(|node| mounted.text(*node))
            .find(|text| text.contains(" cards"))
            .expect("the readout of how many cards are shown")
    };
    assert_eq!(readout(&mut mounted), "2000 of 2000 cards");
    let done = mounted.get(by_text("Done"));
    mounted.click(done);
    let narrowed = readout(&mut mounted);
    assert!(
        narrowed.ends_with("of 2000 cards") && narrowed != "2000 of 2000 cards",
        "{narrowed}"
    );
    let all = mounted.get(by_text("All"));
    mounted.click(all);
    assert_eq!(
        readout(&mut mounted),
        "2000 of 2000 cards",
        "`All` brings them back"
    );
}

#[test]
fn turning_the_glow_off_changes_the_class_of_the_desk() {
    let mut mounted = app();
    assert_eq!(mounted.all(by_class("glow-on")).len(), 1);
    go(&mut mounted, "nav-settings");
    let glow = mounted.get(by_role_named("Switch", "Lamp glow"));
    mounted.click(glow);
    assert_eq!(mounted.all(by_class("glow-on")).len(), 0);
    assert_eq!(mounted.all(by_class("glow-off")).len(), 1);
}

#[test]
fn the_grain_knob_goes_through_smooth_light_and_heavy() {
    let mut mounted = app();
    go(&mut mounted, "nav-settings");
    let classes = |mounted: &mut Mounted| {
        ["grain-0", "grain-1", "grain-2"]
            .iter()
            .position(|class| !mounted.all(by_class(class)).is_empty())
            .expect("one grain class is on the desk")
    };
    assert_eq!(classes(&mut mounted), 1, "light to begin with");
    let knob = mounted.get(by_class("knob"));
    mounted.click(knob);
    assert_eq!(classes(&mut mounted), 2);
    mounted.click(knob);
    assert_eq!(classes(&mut mounted), 0, "and round again");
}

#[test]
fn the_desk_fits_the_window_at_any_size() {
    let mut mounted = app();
    for (width, height) in [(1180.0, 760.0), (1000.0, 620.0), (1400.0, 900.0)] {
        mounted.resize(width, height);
        for id in ["nav-desk", "nav-stack", "nav-settings"] {
            go(&mut mounted, id);
            let rail = mounted.get(by_class("rail"));
            let bounds = mounted.bounds(rail).expect("the rail is laid out");
            assert!(
                bounds.y + bounds.height <= height,
                "{id} at {width}x{height}: the rail ends at {}, past the window",
                bounds.y + bounds.height
            );
        }
    }
}
