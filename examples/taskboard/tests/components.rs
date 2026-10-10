//! Each component on its own, mounted headless and driven the way a person
//! would: pressed through its drawn elements, with nothing reached into.

use florui::prelude::*;
use florui_test::{Harness, Mounted, by_class, by_role_named};
use taskboard::components::button::{KeyButton, KeyButtonProps};
use taskboard::components::knob::{Knob, KnobProps, pointer_angle};
use taskboard::components::led::{Led, LedProps, Tone};
use taskboard::components::meter::{Meter, MeterProps, fill_percent};
use taskboard::components::paper_card::{PaperCard, PaperCardProps};
use taskboard::components::segmented::{Segmented, SegmentedProps};
use taskboard::components::toggle::{Toggle, ToggleProps};

fn stylesheet() -> String {
    taskboard::stylesheet()
}

fn mount(root: impl Fn() -> Element + 'static) -> Mounted {
    Harness::new(root)
        .css(&stylesheet())
        .viewport(400.0, 300.0)
        .mount()
}

/// A switch and the signal it is bound to, with a line that shows the signal.
fn switch_with(initial: bool) -> impl Fn() -> Element + 'static {
    move || {
        let on = use_signal(move || initial);
        view! {
            <div class="desk">
                <Toggle id={"glow".to_string()} on={on.binding()} label={"Lamp glow".to_string()} />
                <span class="state">{if on.get() { "on" } else { "off" }}</span>
            </div>
        }
    }
}

fn text(mounted: &mut Mounted, class: &str) -> String {
    let node = mounted.get(by_class(class));
    mounted.text(node)
}

#[test]
fn a_switch_that_starts_off_is_drawn_off_and_turns_on_when_pressed() {
    let mut mounted = mount(switch_with(false));
    assert_eq!(text(&mut mounted, "state"), "off");
    let switch = mounted.get(by_role_named("Switch", "Lamp glow"));
    mounted.click(switch);
    assert_eq!(text(&mut mounted, "state"), "on");
    let switch = mounted.get(by_role_named("Switch", "Lamp glow"));
    mounted.click(switch);
    assert_eq!(
        text(&mut mounted, "state"),
        "off",
        "pressing it again turns it off"
    );
}

#[test]
fn a_switch_that_starts_on_is_drawn_on() {
    let mut mounted = mount(switch_with(true));
    assert_eq!(text(&mut mounted, "state"), "on");
}

#[test]
fn a_switch_asked_to_stay_put_stays_put() {
    // The owner decides: this one refuses every request.
    let mut mounted = mount(|| {
        let on = use_signal(|| false);
        let refusing = Binding::new(on.get(), |_requested: bool| {});
        view! {
            <div class="desk">
                <Toggle id={"locked".to_string()} on={refusing} label={"Locked".to_string()} />
                <span class="state">{if on.get() { "on" } else { "off" }}</span>
            </div>
        }
    });
    let switch = mounted.get(by_role_named("Switch", "Locked"));
    mounted.click(switch);
    assert_eq!(text(&mut mounted, "state"), "off");
}

#[test]
fn the_knob_moves_to_the_next_detent_and_wraps_to_the_first() {
    let mut mounted = mount(|| {
        let step = use_signal(|| 0usize);
        view! {
            <div class="desk">
                <Knob step={step.binding()} steps={4} label={"PRIORITY".to_string()} />
                <span class="state">{step.get().to_string()}</span>
            </div>
        }
    });
    let mut seen = Vec::new();
    for _ in 0..5 {
        seen.push(text(&mut mounted, "state"));
        let knob = mounted.get(by_class("knob"));
        mounted.click(knob);
    }
    assert_eq!(seen, ["0", "1", "2", "3", "0"]);
}

#[test]
fn the_pointer_swings_from_minus_135_to_plus_135_degrees() {
    assert_eq!(pointer_angle(0, 4), -135.0);
    assert_eq!(pointer_angle(3, 4), 135.0);
    assert_eq!(pointer_angle(1, 4), -45.0);
    assert_eq!(
        pointer_angle(9, 4),
        135.0,
        "a step past the last is the last"
    );
    assert_eq!(
        pointer_angle(0, 1),
        0.0,
        "a knob with one detent has nowhere to turn"
    );
}

#[test]
fn the_segment_pressed_becomes_the_selected_one() {
    let mut mounted = mount(|| {
        let selected = use_signal(|| 0usize);
        view! {
            <div class="desk">
                <Segmented
                    options={vec!["All".to_string(), "Open".to_string(), "Done".to_string()]}
                    selected={selected.binding()}
                />
                <span class="state">{selected.get().to_string()}</span>
            </div>
        }
    });
    assert_eq!(text(&mut mounted, "state"), "0");
    let done = mounted.get(florui_test::by_text("Done"));
    mounted.click(done);
    assert_eq!(text(&mut mounted, "state"), "2");
    // Exactly one segment is marked on, and it is the one pressed.
    let on = mounted.get(by_class("on"));
    assert_eq!(mounted.text(on), "Done");
}

#[test]
fn a_key_runs_its_handler_once_per_press() {
    let mut mounted = mount(|| {
        let presses = use_signal(|| 0u32);
        let counted = presses.clone();
        view! {
            <div class="desk">
                <KeyButton
                    label={"Add".to_string()}
                    accent={true}
                    on_press={Handler::new(move || counted.set(counted.get() + 1))}
                />
                <span class="state">{presses.get().to_string()}</span>
            </div>
        }
    });
    let key = mounted.get(by_class("key"));
    mounted.click(key);
    mounted.click(key);
    assert_eq!(text(&mut mounted, "state"), "2");
}

#[test]
fn a_lamp_is_drawn_lit_only_when_it_is_asked_to_be() {
    let mut mounted = mount(|| {
        view! {
            <div class="desk">
                <Led tone={Tone::Amber} lit={true} />
                <Led tone={Tone::Green} lit={false} />
            </div>
        }
    });
    assert_eq!(mounted.all(by_class("led")).len(), 2);
    assert_eq!(
        mounted.all(by_class("lit")).len(),
        1,
        "only the lamp asked to be lit"
    );
    assert_eq!(mounted.all(by_class("amber")).len(), 1);
}

#[test]
fn a_meter_fills_to_its_fraction_and_never_past_its_slot() {
    assert_eq!(fill_percent(0.0), 0.0);
    assert_eq!(fill_percent(0.5), 50.0);
    assert_eq!(fill_percent(0.123), 12.3);
    assert_eq!(fill_percent(-3.0), 0.0);
    assert_eq!(fill_percent(7.0), 100.0);

    let mut mounted = mount(|| {
        view! {
            <div class="desk" style="width: 300px;">
                <Meter fraction={0.5} label={"COMPLETED".to_string()} />
            </div>
        }
    });
    let slot = mounted.get(by_class("slot"));
    let fill = mounted.get(by_class("fill"));
    let slot_width = mounted.bounds(slot).expect("the slot is drawn").width;
    let fill_width = mounted.bounds(fill).expect("the fill is drawn").width;
    assert!(
        (fill_width / slot_width - 0.5).abs() < 0.01,
        "half the slot: {fill_width} of {slot_width}"
    );
}

#[test]
fn a_paper_card_shows_its_title_and_the_line_under_it_and_marks_when_selected() {
    let mut mounted = mount(|| {
        view! {
            <div class="desk">
                <PaperCard
                    title={"Oil the hinges".to_string()}
                    subtitle={"due tomorrow".to_string()}
                    selected={true}
                >

                </PaperCard>
            </div>
        }
    });
    assert_eq!(text(&mut mounted, "heading"), "Oil the hinges");
    assert_eq!(text(&mut mounted, "line"), "due tomorrow");
    assert_eq!(
        mounted.all(by_class("selected")).len(),
        1,
        "a selected card is marked"
    );
}
