//! [`Switch`]: an on/off toggle built on a real `<input type="checkbox"
//! role="switch">`, so state, focus, Tab order, Enter/Space activation and
//! `disabled` all come from the checkbox wiring, and the accessibility
//! bridge reports it as a switch.
//!
//! The `<input>` is the whole track (it is the real click, focus and
//! accessibility target); the thumb is a later sibling that is purely
//! visual, so it carries `pointer-events: none` inline and a click on it
//! reaches the input. Ships no CSS: size the wrapper and input, position
//! the thumb (`position: absolute`), and move it with
//! `.florui-switch-input:checked + .florui-switch-thumb`. The three
//! `SWITCH_*_CLASS` constants are the public styling points.
//!
//! The switch never flips itself: every toggle request (a click, Space or
//! Enter while focused, an accessibility Click) asks for `!checked`, and
//! the owner decides whether to accept it. The two ways to wire that are
//! [`SwitchValue`]'s variants, mutually exclusive by construction: a
//! [`Binding<bool>`], or a plain value plus a [`BoolHandler`].

use florui::reactive::Binding;
use florui::{BoolHandler, Element, component};

/// The wrapper `<div>` — reserved marker, style it for the track's size.
pub const SWITCH_CLASS: &str = "florui-switch";
/// The `<input>`, which is the track itself.
pub const SWITCH_INPUT_CLASS: &str = "florui-switch-input";
/// The visual thumb, a sibling right after the input.
pub const SWITCH_THUMB_CLASS: &str = "florui-switch-thumb";

/// How a [`Switch`] reads its value and reports a requested change.
#[derive(Clone, Debug, PartialEq)]
pub enum SwitchValue {
    /// A read/write [`Binding<bool>`]: the switch reads it and asks it to
    /// update; the binding's owner may reject the request.
    Bound(Binding<bool>),
    /// The explicit contract: the owner's plain value, and a handler told
    /// the value the switch is asking for.
    Controlled {
        checked: bool,
        on_change: BoolHandler,
    },
}

impl SwitchValue {
    pub fn bound(binding: Binding<bool>) -> Self {
        Self::Bound(binding)
    }

    pub fn controlled(checked: bool, on_change: BoolHandler) -> Self {
        Self::Controlled { checked, on_change }
    }

    fn checked(&self) -> bool {
        match self {
            Self::Bound(binding) => binding.get(),
            Self::Controlled { checked, .. } => *checked,
        }
    }

    /// Asks the owner for the opposite of the current value.
    fn request_toggle(&self) {
        let next = !self.checked();
        match self {
            Self::Bound(binding) => binding.request_update(next),
            Self::Controlled { on_change, .. } => on_change.call(next),
        }
    }
}

/// Renders a switch — see the module doc. `id` is the input's id, so a
/// `<label for={id}>` toggles it; `accessible_label` is the name a screen
/// reader announces. `indeterminate` reports a real mixed state to
/// accessibility tools (measured against Chrome: a `role="switch"`
/// checkbox reports it the same way a plain one does) — it is display
/// only, real HTML's own IDL-only property, so it never changes what a
/// toggle requests.
#[component]
pub fn Switch(
    id: String,
    value: SwitchValue,
    disabled: bool,
    indeterminate: bool,
    accessible_label: String,
) -> Element {
    let checked = value.checked();
    florui::view! {
        <div class={SWITCH_CLASS}>
            <input
                id={id}
                class={SWITCH_INPUT_CLASS}
                type="checkbox"
                role="switch"
                checked={checked}
                disabled={disabled}
                indeterminate={indeterminate}
                accessible_label={accessible_label}
                onclick={move || value.request_toggle()}
            />
            <div class={SWITCH_THUMB_CLASS} style="pointer-events: none"></div>
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

    const CSS: &str = ".florui-switch { position: relative; width: 44px; height: 24px; } \
         .florui-switch-input { width: 44px; height: 24px; } \
         .florui-switch-thumb { position: absolute; top: 2px; left: 2px; \
                                width: 20px; height: 20px; } \
         .florui-switch-input:checked + .florui-switch-thumb { left: 22px; }";

    type Requests = Rc<RefCell<Vec<bool>>>;

    fn viewport() -> Size<AvailableSpace> {
        Size {
            width: AvailableSpace::Definite(200.0),
            height: AvailableSpace::Definite(100.0),
        }
    }

    fn runtime_for(value: SwitchValue, disabled: bool) -> UiRuntime {
        UiRuntime::new(
            CSS,
            move || {
                let value = value.clone();
                view! {
                    <div>
                        <Switch
                            id={"wifi".to_string()}
                            value={value}
                            disabled={disabled}
                            indeterminate={false}
                            accessible_label={"Wi-Fi".to_string()}
                        />
                        <label id="wifi-label" for="wifi">{"Wi-Fi"}</label>
                    </div>
                }
            },
            viewport(),
        )
        .unwrap()
    }

    /// A switch on the explicit contract, recording every requested value
    /// and accepting none of them — so `checked` stays as given.
    fn controlled(checked: bool, disabled: bool) -> (UiRuntime, Requests) {
        let requests: Requests = Rc::default();
        let recorded = requests.clone();
        let value = SwitchValue::controlled(
            checked,
            BoolHandler::new(move |next| recorded.borrow_mut().push(next)),
        );
        (runtime_for(value, disabled), requests)
    }

    fn node_of(runtime: &UiRuntime, class: &str) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap()
    }

    fn center_of(runtime: &UiRuntime, node: NodeId) -> (f32, f32) {
        let (arena, _, layouts) = runtime.geometry();
        let (x, y) = absolute_position(arena, layouts, node);
        let layout = layouts[&node];
        (x + layout.width / 2.0, y + layout.height / 2.0)
    }

    #[test]
    fn an_off_switch_asks_for_on_and_an_on_switch_asks_for_off() {
        let (off, off_requests) = controlled(false, false);
        off.dispatch_click(node_of(&off, SWITCH_INPUT_CLASS));
        assert_eq!(*off_requests.borrow(), vec![true]);

        let (on, on_requests) = controlled(true, false);
        on.dispatch_click(node_of(&on, SWITCH_INPUT_CLASS));
        assert_eq!(*on_requests.borrow(), vec![false]);
    }

    #[test]
    fn a_rejected_request_leaves_the_switch_as_it_was() {
        let (runtime, requests) = controlled(false, false);
        runtime.dispatch_click(node_of(&runtime, SWITCH_INPUT_CLASS));
        assert_eq!(*requests.borrow(), vec![true]);
        let (arena, ..) = runtime.geometry();
        assert!(!arena.is_checked(node_of(&runtime, SWITCH_INPUT_CLASS)));
    }

    #[test]
    fn an_indeterminate_switch_reports_mixed_and_still_toggles_from_checked() {
        let requests: Requests = Rc::default();
        let recorded = requests.clone();
        let value = SwitchValue::controlled(
            false,
            BoolHandler::new(move |next| recorded.borrow_mut().push(next)),
        );
        let runtime = UiRuntime::new(
            CSS,
            move || {
                let value = value.clone();
                view! {
                    <Switch
                        id={"wifi".to_string()}
                        value={value}
                        disabled={false}
                        indeterminate={true}
                        accessible_label={"Wi-Fi".to_string()}
                    />
                }
            },
            viewport(),
        )
        .unwrap();
        let (arena, ..) = runtime.geometry();
        assert!(arena.is_indeterminate(node_of(&runtime, SWITCH_INPUT_CLASS)));
        runtime.dispatch_click(node_of(&runtime, SWITCH_INPUT_CLASS));
        assert_eq!(
            *requests.borrow(),
            vec![true],
            "indeterminate is display-only: a toggle still requests !checked"
        );
    }

    #[test]
    fn a_bound_switch_reads_the_binding_and_requests_the_opposite() {
        let requests: Requests = Rc::default();
        let recorded = requests.clone();
        let binding = Binding::new(true, move |next| recorded.borrow_mut().push(next));
        let runtime = runtime_for(SwitchValue::bound(binding), false);
        let input = node_of(&runtime, SWITCH_INPUT_CLASS);
        let (arena, ..) = runtime.geometry();
        assert!(arena.is_checked(input), "checked comes from the binding");
        runtime.dispatch_click(input);
        assert_eq!(*requests.borrow(), vec![false]);
    }

    #[test]
    fn clicking_its_label_asks_for_a_toggle_and_a_disabled_one_ignores_it() {
        let (runtime, requests) = controlled(false, false);
        let label = node_of_id(&runtime, "wifi-label");
        let target = runtime
            .activation_target(label)
            .expect("the label targets the input");
        runtime.dispatch_click(target);
        assert_eq!(*requests.borrow(), vec![true]);

        let (disabled, _) = controlled(false, true);
        let label = node_of_id(&disabled, "wifi-label");
        assert_eq!(disabled.activation_target(label), None);
    }

    fn node_of_id(runtime: &UiRuntime, id: &str) -> NodeId {
        let (arena, ..) = runtime.geometry();
        arena.find(|a, node| a.id_attr(node) == Some(id)).unwrap()
    }

    #[test]
    fn a_click_landing_on_the_thumb_reaches_the_input() {
        let (runtime, _) = controlled(false, false);
        let input = node_of(&runtime, SWITCH_INPUT_CLASS);
        let thumb = node_of(&runtime, SWITCH_THUMB_CLASS);
        let (x, y) = center_of(&runtime, thumb);
        assert_eq!(
            runtime.hit_test(x, y),
            Some(input),
            "the thumb sits over the track but never takes the click"
        );
    }

    #[test]
    fn checked_moves_the_thumb_through_the_sibling_selector() {
        let (off, _) = controlled(false, false);
        let (on, _) = controlled(true, false);
        let thumb_x = |runtime: &UiRuntime| {
            let (arena, _, layouts) = runtime.geometry();
            absolute_position(arena, layouts, node_of(runtime, SWITCH_THUMB_CLASS)).0
        };
        assert_eq!(thumb_x(&on) - thumb_x(&off), 20.0);
    }

    #[test]
    fn tab_focuses_the_input() {
        let (mut runtime, _) = controlled(false, false);
        assert!(runtime.focus_next());
        assert_eq!(
            runtime.focused(),
            Some(node_of(&runtime, SWITCH_INPUT_CLASS))
        );
    }

    #[test]
    fn a_disabled_switch_ignores_clicks_and_tab() {
        let (mut runtime, requests) = controlled(false, true);
        runtime.dispatch_click(node_of(&runtime, SWITCH_INPUT_CLASS));
        assert!(requests.borrow().is_empty());
        assert!(!runtime.focus_next());
        assert_eq!(runtime.focused(), None);
    }
}
