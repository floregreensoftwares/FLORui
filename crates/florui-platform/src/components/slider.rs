//! [`Slider`]: a continuous value control built on a real
//! `<input type="range">`, so focus, Tab order, keyboard stepping,
//! pointer dragging, disabled gating and `Role::Slider` all come from the
//! range-input wiring in `focus.rs`/`runtime.rs`/`desktop.rs`/
//! `accessibility::tree`.
//!
//! The `<input>` is the whole track (it is the real click/drag/focus/
//! accessibility target); the thumb is a later sibling that is purely
//! visual, so it carries `pointer-events: none` and a computed inline
//! `left:` percentage — a continuous position, unlike `Switch`'s own
//! binary `:checked` thumb, so it cannot be a CSS state and has to be
//! computed here. Ships no CSS: size the wrapper and input, position the
//! thumb (`position: absolute`).
//!
//! Controlled, like `Switch`: every step (a key, a click-jump, a drag
//! move) requests a value and the owner decides. `on_commit` fires once
//! when an interaction ends (pointer release, or a keyboard step, each
//! already a complete interaction on its own) — see
//! [`crate::UiRuntime::end_range_drag`] and `desktop.rs`'s own keyboard
//! dispatch. It carries no value: the owner already holds whichever
//! request it last accepted.

use florui::{Element, FloatHandler, Handler, component};
use florui_reactive::Binding;

/// The wrapper `<div>` — reserved marker, style it for the track's size.
pub const SLIDER_CLASS: &str = "florui-slider";
/// The `<input>`, which is the track itself.
pub const SLIDER_INPUT_CLASS: &str = "florui-slider-input";
/// The visual thumb, a sibling right after the input.
pub const SLIDER_THUMB_CLASS: &str = "florui-slider-thumb";

/// How a [`Slider`] reads its value and reports a requested change — the
/// same shape as `Switch`'s own `SwitchValue`.
#[derive(Clone, Debug, PartialEq)]
pub enum SliderValue {
    /// A read/write [`Binding<f32>`]: the slider reads it and asks it to
    /// update; the binding's owner may reject or clamp the request.
    Bound(Binding<f32>),
    /// The explicit contract: the owner's plain value, and a handler told
    /// the value the slider is asking for.
    Controlled { value: f32, on_change: FloatHandler },
}

impl SliderValue {
    pub fn bound(binding: Binding<f32>) -> Self {
        Self::Bound(binding)
    }

    pub fn controlled(value: f32, on_change: FloatHandler) -> Self {
        Self::Controlled { value, on_change }
    }

    fn value(&self) -> f32 {
        match self {
            Self::Bound(binding) => binding.get(),
            Self::Controlled { value, .. } => *value,
        }
    }

    /// Asks the owner for `requested`.
    fn request(&self, requested: f32) {
        match self {
            Self::Bound(binding) => binding.request_update(requested),
            Self::Controlled { on_change, .. } => on_change.call(requested),
        }
    }
}

/// Renders a slider — see the module doc. `id` is the input's id, so a
/// `<label for={id}>` activates it; `accessible_label` is the name a
/// screen reader announces.
#[component]
pub fn Slider(
    id: String,
    value: SliderValue,
    min: f32,
    max: f32,
    step: f32,
    disabled: bool,
    accessible_label: String,
    on_commit: Handler,
) -> Element {
    let current = value.value().clamp(min.min(max), min.max(max));
    let percent = if max > min {
        (current - min) / (max - min) * 100.0
    } else {
        0.0
    };
    florui::view! {
        <div class={SLIDER_CLASS}>
            <input
                id={id}
                class={SLIDER_INPUT_CLASS}
                type="range"
                min={min.to_string()}
                max={max.to_string()}
                step={step.to_string()}
                value={current.to_string()}
                disabled={disabled}
                accessible_label={accessible_label}
                oninput={move |raw: String| {
                    if let Ok(parsed) = raw.parse::<f32>() {
                        value.request(parsed);
                    }
                }}
                oncommit={move || on_commit.call()}
            />
            <div
                class={SLIDER_THUMB_CLASS}
                style={format!("left: {percent}%; pointer-events: none")}
            ></div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use florui::prelude::*;
    use florui_layout::absolute_position;
    use florui_style::NodeId;
    use taffy::{AvailableSpace, Size};

    use super::*;
    use crate::UiRuntime;

    const CSS: &str = ".florui-slider { position: relative; width: 100px; height: 20px; } \
         .florui-slider-input { width: 100px; height: 20px; \
                                 border-width: 0px; padding-top: 0px; padding-right: 0px; \
                                 padding-bottom: 0px; padding-left: 0px; } \
         .florui-slider-thumb { position: absolute; top: 0px; width: 10px; height: 20px; }";

    type Requests = Rc<RefCell<Vec<f32>>>;

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(200.0),
            height: AvailableSpace::Definite(100.0),
        }
    }

    /// A slider on the explicit contract, recording every requested value
    /// and accepting none of them — so `value` stays as given.
    fn controlled(value: f32, disabled: bool) -> (UiRuntime, Requests, Rc<RefCell<u32>>) {
        let requests: Requests = Rc::default();
        let recorded = requests.clone();
        let commits = Rc::new(RefCell::new(0));
        let recorded_commits = commits.clone();
        let slider_value = SliderValue::controlled(
            value,
            FloatHandler::new(move |v| recorded.borrow_mut().push(v)),
        );
        let runtime = UiRuntime::new(
            CSS,
            move || {
                let slider_value = slider_value.clone();
                let recorded_commits = recorded_commits.clone();
                view! {
                    <div>
                        <Slider
                            id={"vol".to_string()}
                            value={slider_value}
                            min={0.0}
                            max={100.0}
                            step={1.0}
                            disabled={disabled}
                            accessible_label={"Volume".to_string()}
                            on_commit={Handler::new(move || {
                                *recorded_commits.borrow_mut() += 1;
                            })}
                        />
                    </div>
                }
            },
            viewport(),
        )
        .unwrap();
        (runtime, requests, commits)
    }

    fn node_of(runtime: &UiRuntime, class: &str) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap()
    }

    fn thumb_left(runtime: &UiRuntime) -> f32 {
        let (arena, _, layouts) = runtime.geometry();
        absolute_position(arena, layouts, node_of(runtime, SLIDER_THUMB_CLASS)).0
    }

    #[test]
    fn a_click_jump_requests_the_value_at_that_position() {
        let (mut runtime, requests, _) = controlled(30.0, false);
        let input = node_of(&runtime, SLIDER_INPUT_CLASS);
        assert_eq!(runtime.start_range_drag(input, 70.0), Some(70.0));
        assert_eq!(*requests.borrow(), vec![70.0]);
    }

    #[test]
    fn arrow_keys_step_the_value() {
        let (runtime, requests, _) = controlled(30.0, false);
        let input = node_of(&runtime, SLIDER_INPUT_CLASS);
        assert_eq!(
            runtime.step_range_value(input, crate::runtime::RangeStep::SmallIncrement),
            Some(31.0)
        );
        assert_eq!(*requests.borrow(), vec![31.0]);
    }

    #[test]
    fn ending_a_drag_fires_commit_exactly_once() {
        let (mut runtime, _, commits) = controlled(30.0, false);
        let input = node_of(&runtime, SLIDER_INPUT_CLASS);
        runtime.start_range_drag(input, 40.0);
        runtime.continue_range_drag(60.0);
        assert_eq!(*commits.borrow(), 0);
        runtime.end_range_drag();
        assert_eq!(*commits.borrow(), 1);
    }

    #[test]
    fn a_disabled_slider_ignores_clicks_and_keyboard_and_is_not_focusable() {
        let (mut runtime, requests, _) = controlled(30.0, true);
        let input = node_of(&runtime, SLIDER_INPUT_CLASS);
        assert_eq!(runtime.start_range_drag(input, 70.0), None);
        assert_eq!(
            runtime.step_range_value(input, crate::runtime::RangeStep::SmallIncrement),
            None
        );
        assert!(requests.borrow().is_empty());
        assert!(!runtime.focus_next());
    }

    #[test]
    fn the_thumbs_position_follows_the_value_as_a_percentage_of_the_track() {
        let (at_min, ..) = controlled(0.0, false);
        let (at_quarter, ..) = controlled(25.0, false);
        let (at_max, ..) = controlled(100.0, false);
        assert_eq!(thumb_left(&at_min), 0.0);
        assert_eq!(thumb_left(&at_quarter), 25.0);
        assert_eq!(thumb_left(&at_max), 100.0);
    }
}
