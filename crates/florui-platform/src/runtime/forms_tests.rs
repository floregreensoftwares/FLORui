use std::cell::{Cell, RefCell};
use std::rc::Rc;

use florui::FormData;
use florui::prelude::*;
use florui_style::{NodeId, Rgba};
use taffy::prelude::*;

use super::UiRuntime;
use super::forms::{ImplicitSubmit, SubmitOutcome};

fn viewport() -> Size<AvailableSpace> {
    Size {
        width: AvailableSpace::Definite(400.0),
        height: AvailableSpace::Definite(400.0),
    }
}

fn node(runtime: &UiRuntime, id: &str) -> NodeId {
    let (arena, ..) = runtime.geometry();
    arena
        .find(|arena, node| arena.id_attr(node) == Some(id))
        .unwrap()
}

fn runtime(css: &str, root: impl Fn() -> Element + 'static) -> UiRuntime {
    UiRuntime::new(css, root, viewport()).unwrap()
}

type Log = Rc<RefCell<Vec<FormData>>>;

fn submitted() -> (Log, Rc<dyn Fn(FormData)>) {
    let log: Log = Rc::new(RefCell::new(Vec::new()));
    let sink = log.clone();
    (log, Rc::new(move |data| sink.borrow_mut().push(data)))
}

fn pairs(data: &FormData) -> Vec<(String, String)> {
    data.iter().map(|(k, v)| (k.into(), v.into())).collect()
}

fn entry(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

#[test]
fn form_data_follows_tree_order_and_the_browsers_inclusion_rules() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <div>
                <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                    <input id="t" type="text" name="t" value="hello" />
                    <input type="text" value="anonymous" />
                    <input type="password" name="pw" value="secret" />
                    <input type="checkbox" name="on" checked="true" />
                    <input type="checkbox" name="off" />
                    <input type="checkbox" name="yes" value="yes" checked="true" />
                    <input type="radio" name="r" value="a" />
                    <input type="radio" name="r" value="b" checked="true" />
                    <input type="range" name="rg" min="0" max="10" value="4" />
                    <input type="text" name="dis" value="d" disabled="true" />
                    <fieldset disabled="true">
                        <input type="text" name="fs" value="inside" />
                    </fieldset>
                    <button id="go" name="go" value="1">{"Go"}</button>
                    <button type="button" name="plain" value="1">{"Plain"}</button>
                </form>
                <input type="text" name="ext" form="f" value="external" />
            </div>
        }
    });
    let form = node(&rt, "f");
    let go = node(&rt, "go");
    rt.submit_form(form, Some(go), false);
    assert_eq!(
        pairs(&log.borrow()[0]),
        [
            entry("t", "hello"),
            entry("pw", "secret"),
            entry("on", "on"),
            entry("yes", "yes"),
            entry("r", "b"),
            entry("rg", "4"),
            entry("ext", "external"),
            entry("go", "1"),
        ]
    );
}

#[test]
fn a_select_contributes_its_selected_options_and_multiple_contributes_each() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <select name="one">
                    <option value="o1">{"1"}</option>
                    <option value="o2" selected="true">{"2"}</option>
                </select>
                <select name="first">
                    <option value="f1">{"1"}</option>
                    <option value="f2">{"2"}</option>
                </select>
                <select name="many" multiple="true">
                    <option value="m1" selected="true">{"1"}</option>
                    <option value="m2" selected="true">{"2"}</option>
                    <option value="m3">{"3"}</option>
                </select>
            </form>
        }
    });
    let form = node(&rt, "f");
    rt.submit_form(form, None, false);
    assert_eq!(
        pairs(&log.borrow()[0]),
        [
            entry("one", "o2"),
            entry("first", "f1"),
            entry("many", "m1"),
            entry("many", "m2"),
        ]
    );
}

fn required_form(novalidate: bool, formnovalidate: bool) -> (UiRuntime, Log, Rc<Cell<u32>>) {
    let (log, on_submit) = submitted();
    let invalid_events = Rc::new(Cell::new(0));
    let counter = invalid_events.clone();
    let rt = runtime("", move || {
        let on_submit = on_submit.clone();
        let counter = counter.clone();
        view! {
            <form id="f" novalidate={novalidate}
                onsubmit={move |data: FormData| on_submit(data)}>
                <input id="name" type="text" name="name" required="true"
                    oninvalid={move || counter.set(counter.get() + 1)} />
                <input id="pat" type="text" name="pat" pattern="[a-z]+" value="A1" />
                <button id="go" formnovalidate={formnovalidate}>{"Go"}</button>
            </form>
        }
    });
    (rt, log, invalid_events)
}

#[test]
fn an_invalid_form_reports_nothing_and_focuses_its_first_invalid_control() {
    let (mut rt, log, invalid_events) = required_form(false, false);
    let (form, go, name) = (node(&rt, "f"), node(&rt, "go"), node(&rt, "name"));
    let outcome = rt.submit_form(form, Some(go), true);
    assert_eq!(outcome, SubmitOutcome::Blocked);
    assert!(log.borrow().is_empty());
    assert_eq!(rt.focused(), Some(name));
    assert_eq!(invalid_events.get(), 1);
}

#[test]
fn novalidate_and_formnovalidate_each_submit_an_invalid_form() {
    for (novalidate, formnovalidate) in [(true, false), (false, true)] {
        let (mut rt, log, _) = required_form(novalidate, formnovalidate);
        let (form, go) = (node(&rt, "f"), node(&rt, "go"));
        assert_eq!(
            rt.submit_form(form, Some(go), false),
            SubmitOutcome::Submitted
        );
        assert_eq!(log.borrow().len(), 1);
    }
}

#[test]
fn a_valid_form_submits() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <input type="text" name="name" required="true" value="ok" />
                <input type="checkbox" name="terms" required="true" checked="true" />
                <input type="text" name="ro" required="true" readonly="true" />
            </form>
        }
    });
    let form = node(&rt, "f");
    assert_eq!(rt.submit_form(form, None, false), SubmitOutcome::Submitted);
    assert_eq!(log.borrow().len(), 1);
}

#[test]
fn validity_covers_pattern_required_select_radio_and_custom_errors() {
    let rt = runtime("", || {
        view! {
            <form id="f">
                <input id="pat" type="text" pattern="[a-z]+" value="A1" />
                <input id="patok" type="text" pattern="[a-z]+" value="abc" />
                <input id="empty-pat" type="text" pattern="[a-z]+" />
                <input id="badpat" type="text" pattern="(" value="x" />
                <select id="sel" required="true">
                    <option value="">{"--"}</option>
                    <option value="x">{"x"}</option>
                </select>
                <input id="r1" type="radio" name="g" required="true" />
                <input id="r2" type="radio" name="g" />
                <input id="custom" type="text" custom_validity="taken" value="a" />
            </form>
        }
    });
    let invalid = |id: &str| rt.form_state_of(node(&rt, id)).unwrap().invalid;
    assert!(invalid("pat"));
    assert!(!invalid("patok"));
    assert!(!invalid("empty-pat"));
    assert!(!invalid("badpat"));
    assert!(invalid("sel"));
    assert!(invalid("r1") && invalid("r2"));
    assert!(invalid("custom"));
}

#[test]
fn length_bounds_only_apply_once_the_user_has_edited_the_value() {
    let rt = runtime("", || {
        view! {
            <form>
                <input id="len" type="text" minlength="3" value="ab"
                    oninput={move |_value: String| {}} />
            </form>
        }
    });
    let len = node(&rt, "len");
    assert!(!rt.form_state_of(len).unwrap().invalid);
    rt.commit_value(len, "ab".to_string());
    assert!(rt.form_state_of(len).unwrap().invalid);
}

#[test]
fn a_submit_button_click_reports_and_a_reset_button_reports_a_reset() {
    let resets = Rc::new(Cell::new(0));
    let counter = resets.clone();
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let (counter, on_submit) = (counter.clone(), on_submit.clone());
        view! {
            <form id="f"
                onsubmit={move |data: FormData| on_submit(data)}
                onreset={move || counter.set(counter.get() + 1)}>
                <input type="text" name="a" value="1" />
                <button id="reset" type="reset">{"R"}</button>
                <button id="submit">{"S"}</button>
            </form>
        }
    });
    let (reset, submit) = (node(&rt, "reset"), node(&rt, "submit"));
    assert!(rt.activate_form_button(reset, false));
    assert_eq!(resets.get(), 1);
    assert!(log.borrow().is_empty());
    assert!(rt.activate_form_button(submit, false));
    assert_eq!(log.borrow().len(), 1);
}

#[test]
fn a_disabled_or_formless_or_plain_button_does_nothing() {
    let mut rt = runtime("", || {
        view! {
            <div>
                <form>
                    <button id="off" disabled="true">{"x"}</button>
                    <button id="plain" type="button">{"x"}</button>
                </form>
                <button id="loose">{"x"}</button>
            </div>
        }
    });
    for id in ["off", "plain", "loose"] {
        let button = node(&rt, id);
        assert!(!rt.activate_form_button(button, false), "{id}");
    }
}

#[test]
fn implicit_submission_clicks_the_first_submit_button_or_submits_a_lone_field() {
    let rt = runtime("", || {
        view! {
            <div>
                <form id="with-button">
                    <input id="a" type="text" />
                    <input id="b" type="text" />
                    <button id="first">{"1"}</button>
                    <button>{"2"}</button>
                </form>
                <form id="lone"><input id="only" type="text" /></form>
                <form id="two">
                    <input id="x" type="text" />
                    <input id="y" type="text" />
                </form>
                <form id="disabled-default">
                    <input id="z" type="text" />
                    <button id="dd" disabled="true">{"d"}</button>
                </form>
            </div>
        }
    });
    let target = |id: &str| rt.implicit_submission(node(&rt, id));
    assert_eq!(target("a"), Some(ImplicitSubmit::Click(node(&rt, "first"))));
    assert_eq!(
        target("only"),
        Some(ImplicitSubmit::Submit(node(&rt, "lone")))
    );
    assert_eq!(target("x"), None);
    assert_eq!(target("z"), None);
}

#[test]
fn radio_groups_never_span_two_forms() {
    let rt = runtime("", || {
        view! {
            <div>
                <form>
                    <input id="a1" type="radio" name="r" checked="true" />
                    <input id="a2" type="radio" name="r" />
                </form>
                <form><input id="b1" type="radio" name="r" /></form>
            </div>
        }
    });
    let (arena, ..) = rt.geometry();
    let group = crate::form::radio_group(arena, node(&rt, "a1"));
    assert_eq!(group, [node(&rt, "a1"), node(&rt, "a2")]);
    assert_eq!(
        crate::form::radio_group(arena, node(&rt, "b1")),
        [node(&rt, "b1")]
    );
}

#[test]
fn form_pseudo_classes_drive_real_css() {
    let rt = runtime(
        "input:invalid { color: #ff0000; } input:valid { color: #00ff00; } \
         input:required { background-color: #0000ff; } input:read-only { opacity: 0.5; }",
        || {
            view! {
                <form>
                    <input id="bad" type="text" required="true" />
                    <input id="good" type="text" value="x" />
                    <input id="ro" type="text" readonly="true" />
                </form>
            }
        },
    );
    let (_, styles, _) = rt.geometry();
    assert_eq!(styles[&node(&rt, "bad")].color, Rgba::opaque(255, 0, 0));
    assert_eq!(styles[&node(&rt, "good")].color, Rgba::opaque(0, 255, 0));
    assert_eq!(
        styles[&node(&rt, "bad")].background_color,
        Rgba::opaque(0, 0, 255)
    );
    assert_ne!(
        styles[&node(&rt, "good")].background_color,
        Rgba::opaque(0, 0, 255)
    );
    assert_eq!(styles[&node(&rt, "ro")].opacity, 0.5);
    assert_ne!(styles[&node(&rt, "good")].opacity, 0.5);
}

#[test]
fn user_invalid_waits_for_a_submit_attempt_or_a_blur_after_an_edit() {
    let (mut rt, _, _) = required_form(false, false);
    let (name, go, form) = (node(&rt, "name"), node(&rt, "go"), node(&rt, "f"));
    let state = |rt: &UiRuntime| rt.form_state_of(name).unwrap();
    assert!(state(&rt).invalid && !state(&rt).user_invalid);
    rt.submit_form(form, Some(go), false);
    assert!(state(&rt).user_invalid);
}

#[test]
fn a_fieldset_disables_its_controls_except_those_in_its_first_legend() {
    let rt = runtime("", || {
        view! {
            <form>
                <fieldset id="fs" disabled="true">
                    <legend><input id="in-legend" type="checkbox" /></legend>
                    <input id="inside" type="checkbox" />
                    <div><input id="nested" type="checkbox" /></div>
                </fieldset>
                <input id="outside" type="checkbox" />
            </form>
        }
    });
    let (arena, ..) = rt.geometry();
    assert!(!arena.is_disabled(node(&rt, "in-legend")));
    assert!(arena.is_disabled(node(&rt, "inside")));
    assert!(arena.is_disabled(node(&rt, "nested")));
    assert!(!arena.is_disabled(node(&rt, "outside")));
}

/// Measured in Chromium: which of `:valid :invalid :required :optional
/// :read-only :read-write :default` each control matches.
#[test]
fn pseudo_class_states_match_chromium() {
    let rt = runtime("", || {
        view! {
            <form id="f1">
                <fieldset id="fs">
                    <input id="bad" type="text" required="true" />
                    <input id="ok" type="text" value="x" />
                </fieldset>
                <button id="btn">{"b"}</button>
                <button id="reset" type="reset">{"r"}</button>
                <select id="sel"><option value="a">{"a"}</option></select>
                <input id="rg" type="range" />
                <input id="cb" type="checkbox" />
                <input id="dis" type="text" required="true" disabled="true" />
                <input id="ro" type="text" readonly="true" />
            </form>
        }
    });
    let flags = |id: &str| {
        let s = rt.form_state_of(node(&rt, id)).unwrap();
        let mut on = Vec::new();
        for (set, name) in [
            (s.valid, "valid"),
            (s.invalid, "invalid"),
            (s.required, "required"),
            (s.optional, "optional"),
            (s.read_only, "read-only"),
            (s.read_write, "read-write"),
            (s.default, "default"),
        ] {
            if set {
                on.push(name);
            }
        }
        on
    };
    assert_eq!(flags("f1"), ["invalid", "read-only"]);
    assert_eq!(flags("fs"), ["invalid", "read-only"]);
    assert_eq!(flags("btn"), ["valid", "optional", "read-only", "default"]);
    assert_eq!(flags("reset"), ["optional", "read-only"]);
    assert_eq!(flags("sel"), ["valid", "optional", "read-only"]);
    assert_eq!(flags("rg"), ["valid", "optional", "read-only"]);
    assert_eq!(flags("cb"), ["valid", "optional", "read-only"]);
    assert_eq!(flags("dis"), ["required", "read-only"]);
    assert_eq!(flags("ro"), ["optional", "read-only"]);
    assert_eq!(flags("bad"), ["invalid", "required", "read-write"]);
    assert_eq!(flags("ok"), ["valid", "optional", "read-write"]);
}

#[test]
fn an_input_with_no_id_and_no_type_is_a_focusable_editable_text_field() {
    let rt = runtime("", || {
        view! {
            <form><input name="q" value="hello" /></form>
        }
    });
    let (arena, ..) = rt.geometry();
    let field = arena.find(|a, id| a.tag(id) == "input").unwrap();
    assert!(crate::focus::is_focusable(arena, field));
    let id = arena.id_attr(field).expect("a generated id");
    let registry = rt.text_input_registry();
    let (_, _, _, mut font) = (0, 0, 0, florui_text::Font::load_embedded());
    registry.apply(id, florui_text::editing::TextEditOp::SelectAll, &mut font);
    assert_eq!(registry.selected_text(id).as_deref(), Some("hello"));
}

#[test]
fn form_pseudo_classes_follow_a_rerender_with_no_hover_or_focus_change() {
    let slot: Rc<RefCell<Option<Signal<String>>>> = Rc::new(RefCell::new(None));
    let capture = slot.clone();
    let mut rt = runtime(
        "input:invalid { color: #ff0000; } input:valid { color: #00ff00; }",
        move || {
            let value = use_signal(String::new);
            *capture.borrow_mut() = Some(value.clone());
            view! {
                <form>
                    <input id="f" type="text" required="true"
                        value={value.get()} oninput={move |_v: String| {}} />
                </form>
            }
        },
    );
    let color = |rt: &UiRuntime| rt.geometry().1[&node(rt, "f")].color;
    assert_eq!(color(&rt), Rgba::opaque(255, 0, 0));
    slot.borrow().as_ref().unwrap().set("filled".to_string());
    rt.update(viewport());
    assert_eq!(color(&rt), Rgba::opaque(0, 255, 0));
    slot.borrow().as_ref().unwrap().set(String::new());
    rt.update(viewport());
    assert_eq!(color(&rt), Rgba::opaque(255, 0, 0));
}

#[test]
fn an_edit_the_owner_rejects_reaches_neither_form_data_nor_validity() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        let value = use_signal(|| "ada".to_string());
        let owner = value.clone();
        let binding = Binding::new(value.get(), move |requested: String| {
            if requested.chars().count() <= 3 {
                owner.set(requested);
            }
        });
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <input id="name" type="text" name="name" minlength="3" value={binding} />
            </form>
        }
    });
    let (form, name) = (node(&rt, "f"), node(&rt, "name"));
    rt.commit_value(name, "adalovelace".to_string());
    rt.update(viewport());
    assert!(!rt.form_state_of(node(&rt, "name")).unwrap().invalid);
    rt.submit_form(form, None, false);
    assert_eq!(pairs(&log.borrow()[0]), [entry("name", "ada")]);
}
