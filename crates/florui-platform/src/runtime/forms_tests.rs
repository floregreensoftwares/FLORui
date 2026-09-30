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

/// Measured in Edge with real key presses: a control the user edited into an
/// invalid state stays neutral while focused, matches `:user-invalid` once it
/// loses focus, and also when the whole window loses focus. Focusing and
/// leaving a control without editing it never matches.
#[test]
fn user_invalid_follows_edit_then_blur_and_window_blur_but_not_a_bare_focus_change() {
    let mut rt = runtime("", || {
        view! {
            <form>
                <input id="edited" type="text" required="true" oninput={move |_v: String| {}} />
                <input id="touched" type="text" required="true" oninput={move |_v: String| {}} />
                <input id="windowed" type="text" required="true" oninput={move |_v: String| {}} />
            </form>
        }
    });
    let user_invalid =
        |rt: &UiRuntime, id: &str| rt.form_state_of(node(rt, id)).unwrap().user_invalid;
    let (edited, touched, windowed) = (
        node(&rt, "edited"),
        node(&rt, "touched"),
        node(&rt, "windowed"),
    );

    rt.set_focused(Some(edited), true);
    rt.commit_value(edited, "x".to_string());
    rt.commit_value(edited, String::new());
    assert!(!user_invalid(&rt, "edited"), "still focused");
    rt.set_focused(Some(touched), true);
    assert!(user_invalid(&rt, "edited"), "edited, then blurred");

    rt.set_focused(Some(windowed), true);
    assert!(!user_invalid(&rt, "touched"), "focus and leave, no edit");

    rt.commit_value(windowed, "y".to_string());
    rt.commit_value(windowed, String::new());
    assert!(!user_invalid(&rt, "windowed"), "still focused");
    rt.window_focus_lost();
    assert!(user_invalid(&rt, "windowed"), "window blur counts as blur");
}

/// Measured in Edge: a hidden input has no box, is not focusable, is
/// submitted with its value, and is never validated even when `required`.
#[test]
fn a_hidden_input_is_submitted_but_never_focusable_or_validated() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <input id="token" type="hidden" name="token" value="secret" required="true" />
                <input id="after" type="text" name="after" value="x" />
            </form>
        }
    });
    let (form, token) = (node(&rt, "f"), node(&rt, "token"));
    let (arena, styles, layouts) = rt.geometry();
    assert!(!crate::focus::is_focusable(arena, token));
    assert_eq!(styles[&token].display, florui_style::Display::None);
    assert_eq!(layouts[&token].width, 0.0);
    assert_eq!(rt.submit_form(form, None, false), SubmitOutcome::Submitted);
    assert_eq!(
        pairs(&log.borrow()[0]),
        [entry("token", "secret"), entry("after", "x")]
    );
    let state = rt.form_state_of(node(&rt, "token")).unwrap();
    assert!(!state.valid && !state.invalid, "not a validation candidate");
}

#[test]
fn an_email_field_submits_its_trimmed_value_and_a_type_mismatch_blocks_submission() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <input id="mail" type="email" name="mail" value="  ada@example.com " />
            </form>
        }
    });
    let form = node(&rt, "f");
    assert_eq!(rt.submit_form(form, None, false), SubmitOutcome::Submitted);
    assert_eq!(pairs(&log.borrow()[0]), [entry("mail", "ada@example.com")]);

    let mut bad = runtime("", || {
        view! {
            <form id="f"><input id="mail" type="email" name="mail" value="not-an-email" /></form>
        }
    });
    let form = node(&bad, "f");
    assert_eq!(bad.submit_form(form, None, false), SubmitOutcome::Blocked);
    assert!(bad.form_state_of(node(&bad, "mail")).unwrap().invalid);
}

#[test]
fn typing_into_an_email_field_hands_the_owner_the_trimmed_value_but_keeps_the_typed_text() {
    let rt = runtime("", || {
        view! { <form><input id="mail" type="email" name="mail" value="" oninput={move |_v: String| {}} /></form> }
    });
    let (arena, ..) = rt.geometry();
    let field = arena.find(|a, id| a.tag(id) == "input").unwrap();
    let id = arena.id_attr(field).unwrap().to_string();
    let registry = rt.text_input_registry();
    let mut font = florui_text::Font::load_embedded();
    let committed = registry.apply(
        &id,
        florui_text::editing::TextEditOp::InsertOrReplace("  a@b.c ".to_string()),
        &mut font,
    );
    assert_eq!(committed.as_deref(), Some("a@b.c"));
    registry.apply(&id, florui_text::editing::TextEditOp::SelectAll, &mut font);
    assert_eq!(registry.selected_text(&id).as_deref(), Some("  a@b.c "));
}

/// Every row measured in Edge (`validity` on `type=number`): which of
/// range underflow, range overflow and step mismatch each value trips.
#[test]
fn number_range_and_step_validity_matches_what_edge_measured() {
    let violations = |min: &'static str, max: &'static str, step: &'static str, value: &str| {
        let value = value.to_string();
        let rt = runtime("", move || {
            let value = value.clone();
            view! {
                <form>
                    <input id="n" type="number" min={min} max={max} step={step}
                        value={value} oninput={move |_v: String| {}} />
                </form>
            }
        });
        let invalid = rt.form_state_of(node(&rt, "n")).unwrap().invalid;
        let field = node(&rt, "n");
        let ctx_violations = rt.with_form_context(|ctx| crate::form_control::validity(ctx, field));
        (
            invalid,
            ctx_violations.range_underflow,
            ctx_violations.range_overflow,
            ctx_violations.step_mismatch,
        )
    };
    // (invalid, underflow, overflow, step) for min=1 max=5 step=2.
    for (value, expected) in [
        ("0", (true, true, false, true)),
        ("1", (false, false, false, false)),
        ("2", (true, false, false, true)),
        ("3", (false, false, false, false)),
        ("5", (false, false, false, false)),
        ("6", (true, false, true, true)),
        ("4", (true, false, false, true)),
        ("1.5", (true, false, false, true)),
    ] {
        assert_eq!(
            violations("1", "5", "2", value),
            expected,
            "min1 max5 step2, {value}"
        );
    }
    // min=0.5 step=1 has no maximum; the grid is anchored at the minimum.
    for (value, expected) in [
        ("0.3", (true, true, false, true)),
        ("2.5", (false, false, false, false)),
        ("7", (true, false, false, true)),
        ("0.5", (false, false, false, false)),
        ("1.5", (false, false, false, false)),
    ] {
        assert_eq!(
            violations("0.5", "", "1", value),
            expected,
            "min0.5 step1, {value}"
        );
    }
    assert_eq!(
        violations("", "", "any", "7.77"),
        (false, false, false, false),
        "step=any"
    );
}

#[test]
fn a_number_field_typed_into_a_bad_input_blocks_submission_until_it_is_a_number() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <input id="qty" type="number" name="qty" value="" oninput={move |_v: String| {}} />
            </form>
        }
    });
    let (arena, ..) = rt.geometry();
    let field = arena.find(|a, id| a.tag(id) == "input").unwrap();
    let id = arena.id_attr(field).unwrap().to_string();
    let registry = rt.text_input_registry();
    let mut font = florui_text::Font::load_embedded();
    let insert =
        |registry: &crate::text_input::TextInputRegistry, font: &mut florui_text::Font, t: &str| {
            registry.apply(
                &id,
                florui_text::editing::TextEditOp::InsertOrReplace(t.to_string()),
                font,
            )
        };
    assert_eq!(
        insert(&registry, &mut font, "1e").as_deref(),
        Some(""),
        "value stays empty"
    );
    assert!(registry.is_bad_input(&id));
    assert!(rt.form_state_of(node(&rt, "qty")).unwrap().invalid);
    let form = node(&rt, "f");
    assert_eq!(rt.submit_form(form, None, false), SubmitOutcome::Blocked);

    assert_eq!(insert(&registry, &mut font, "5").as_deref(), Some("1e5"));
    assert!(!registry.is_bad_input(&id));
    assert!(
        log.borrow().is_empty(),
        "nothing was reported while it was invalid"
    );
    assert!(!rt.form_state_of(node(&rt, "qty")).unwrap().invalid);
}

/// Rows measured in Edge for `:in-range`/`:out-of-range`.
#[test]
fn number_in_range_and_out_of_range_match_what_edge_measured() {
    let rt = runtime("", || {
        view! {
            <form>
                <input id="a" type="number" />
                <input id="b" type="number" value="3" oninput={move |_v: String| {}} />
                <input id="c" type="number" min="1" max="5" value="3" oninput={move |_v: String| {}} />
                <input id="d" type="number" min="1" max="5" value="9" oninput={move |_v: String| {}} />
                <input id="e" type="number" min="1" value="0" oninput={move |_v: String| {}} />
                <input id="f" type="number" max="5" value="6" oninput={move |_v: String| {}} />
                <input id="g" type="number" min="1" max="5" />
                <input id="h" type="number" min="1" max="5" value="3" disabled="true" />
                <input id="i" type="number" min="1" max="5" value="3" readonly="true" />
                <input id="j" type="range" />
                <input id="k" type="text" />
            </form>
        }
    });
    let ranges = |id: &str| {
        let s = rt.form_state_of(node(&rt, id)).unwrap();
        (s.in_range, s.out_of_range)
    };
    let (yes, no, out) = ((true, false), (false, false), (false, true));
    for (id, expected) in [
        ("a", yes),
        ("b", no),
        ("c", yes),
        ("d", out),
        ("e", out),
        ("f", out),
        ("g", yes),
        ("h", no),
        ("i", no),
        ("j", yes),
        ("k", no),
    ] {
        assert_eq!(ranges(id), expected, "input {id}");
    }
}

/// Rows measured in Edge with a real Enter key: which forms submit on Enter
/// in a single-line field when they have no submit button.
#[test]
fn a_textarea_does_not_block_implicit_submission_but_number_and_email_do() {
    let rt = runtime("", || {
        view! {
            <div>
                <form id="with-textarea"><input id="a" type="text" /><textarea></textarea></form>
                <form id="two-and-textarea">
                    <input id="b" type="text" /><input type="text" /><textarea></textarea>
                </form>
                <form id="with-number"><input id="c" type="text" /><input type="number" /></form>
                <form id="with-email"><input id="d" type="text" /><input type="email" /></form>
                <form id="with-hidden"><input id="e" type="text" /><input type="hidden" /></form>
            </div>
        }
    });
    let submits = |id: &str| {
        matches!(
            rt.implicit_submission(node(&rt, id)),
            Some(ImplicitSubmit::Submit(_))
        )
    };
    assert!(submits("a"), "input + textarea submits");
    assert!(!submits("b"), "two inputs still block");
    assert!(!submits("c"), "a number field blocks");
    assert!(!submits("d"), "an email field blocks");
    assert!(submits("e"), "a hidden input does not count");
}

#[test]
fn a_textarea_submits_its_value_with_line_breaks_normalized() {
    let (log, on_submit) = submitted();
    let mut rt = runtime("", move || {
        let on_submit = on_submit.clone();
        view! {
            <form id="f" onsubmit={move |data: FormData| on_submit(data)}>
                <textarea name="note" value="one\r\ntwo\rthree"
                    oninput={move |_v: String| {}}></textarea>
            </form>
        }
    });
    let form = node(&rt, "f");
    assert_eq!(rt.submit_form(form, None, false), SubmitOutcome::Submitted);
    assert_eq!(pairs(&log.borrow()[0]), [entry("note", "one\ntwo\nthree")]);
}

#[test]
fn a_textarea_keeps_line_breaks_when_typed_and_a_single_line_field_drops_them() {
    let rt = runtime("", || {
        view! {
            <form>
                <textarea id="ta" name="a"></textarea>
                <input id="one" type="text" name="b" />
            </form>
        }
    });
    let (area, line) = (node(&rt, "ta"), node(&rt, "one"));
    assert_eq!(rt.clean_typed(area, "a\r\nb\rc\n"), "a\nb\nc\n");
    assert_eq!(rt.clean_typed(line, "a\r\nb\tc"), "abc");
}

fn textarea_runtime(rows: &'static str) -> UiRuntime {
    runtime("", move || {
        view! {
            <form>
                <textarea id="ta" name="note" rows={rows} cols="20" value=""
                    oninput={move |_v: String| {}}></textarea>
            </form>
        }
    })
}

fn textarea_id(rt: &UiRuntime) -> String {
    let (arena, ..) = rt.geometry();
    let field = arena.find(|a, id| a.tag(id) == "textarea").unwrap();
    arena.id_attr(field).unwrap().to_string()
}

fn type_into(rt: &mut UiRuntime, id: &str, text: &str) {
    let registry = rt.text_input_registry();
    let (_, _, _, font) = rt.geometry_and_font_mut();
    registry.apply(
        id,
        florui_text::editing::TextEditOp::InsertOrReplace(text.to_string()),
        font,
    );
}

#[test]
fn typing_past_the_last_visible_row_scrolls_the_caret_into_view() {
    let mut rt = textarea_runtime("2");
    let id = textarea_id(&rt);
    let registry = rt.text_input_registry();
    assert_eq!(registry.scroll_offset(&id), (0.0, 0.0));
    type_into(&mut rt, &id, "1\n2\n3\n4\n5\n6");
    let (_, y) = registry.scroll_offset(&id);
    assert!(y > 0.0, "six lines do not fit in two rows, so it scrolled");
    let (_, _, _, font) = rt.geometry_and_font_mut();
    let (_, top, _, bottom) = registry.paint_data(&id, font).unwrap().1.unwrap();
    let viewport_height = {
        let (arena, styles, layouts) = rt.geometry();
        let node = arena.find(|a, n| a.tag(n) == "textarea").unwrap();
        layouts[&node].height
            - styles[&node].border.top.width * 2.0
            - styles[&node].padding.top * 2.0
    };
    assert!(
        top >= y - 0.5 && bottom <= y + viewport_height + 0.5,
        "the caret row is visible"
    );
}

#[test]
fn the_wheel_scrolls_a_textarea_until_a_limit_then_lets_go() {
    let mut rt = textarea_runtime("2");
    let id = textarea_id(&rt);
    type_into(&mut rt, &id, "1\n2\n3\n4\n5\n6\n7\n8");
    let registry = rt.text_input_registry();
    let (_, _, _, font) = rt.geometry_and_font_mut();
    assert!(
        registry.scroll_by(&id, -1000.0, font),
        "scrolls up from the caret's position"
    );
    assert_eq!(registry.scroll_offset(&id).1, 0.0, "clamped at the top");
    assert!(
        !registry.scroll_by(&id, -10.0, font),
        "already at the top: pass it on"
    );
    assert!(registry.scroll_by(&id, 1000.0, font));
    let bottom = registry.scroll_offset(&id).1;
    assert!(bottom > 0.0);
    assert!(
        !registry.scroll_by(&id, 10.0, font),
        "at the bottom: pass it on"
    );
}

#[test]
fn page_down_moves_the_caret_by_the_field_height_less_one_line() {
    let mut rt = textarea_runtime("4");
    let id = textarea_id(&rt);
    let lines: Vec<String> = (1..=14).map(|n| format!("L{n:02}")).collect();
    type_into(&mut rt, &id, &lines.join("\n"));
    let registry = rt.text_input_registry();
    let (_, _, _, font) = rt.geometry_and_font_mut();
    registry.apply(&id, florui_text::editing::TextEditOp::MoveTextStart, font);
    registry.page(&id, 1, false, font);
    registry.apply(&id, florui_text::editing::TextEditOp::SelectLineEnd, font);
    assert_eq!(
        registry.selected_text(&id).as_deref(),
        Some("L04"),
        "three rows down in a four-row field, as Edge does"
    );
}

#[test]
fn a_long_line_wraps_at_the_content_width_instead_of_running_past_the_box() {
    let mut rt = textarea_runtime("2");
    let id = textarea_id(&rt);
    let registry = rt.text_input_registry();
    type_into(&mut rt, &id, &"word ".repeat(30));
    let (_, _, _, font) = rt.geometry_and_font_mut();
    let (runs, ..) = registry.paint_data(&id, font).unwrap();
    let widest = runs
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|g| g.x))
        .fold(0.0_f32, f32::max);
    let (arena, styles, layouts) = rt.geometry();
    let node = arena.find(|a, n| a.tag(n) == "textarea").unwrap();
    let content_width = layouts[&node].width - styles[&node].padding.left * 2.0 - 2.0;
    assert!(
        widest <= content_width,
        "glyphs stay inside the box ({widest} <= {content_width})"
    );
    let ys: std::collections::BTreeSet<i32> = runs
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|g| g.y as i32))
        .collect();
    assert!(ys.len() > 2, "the text wrapped onto several rows");
}

/// A read-only field can be focused, selected and copied but never edited,
/// whether typed into, cut, deleted, undone or composed into.
#[test]
fn a_readonly_field_selects_but_never_edits() {
    use florui_text::editing::TextEditOp;
    let mut rt = runtime("", || {
        view! {
            <form>
                <input id="one" type="text" name="a" readonly="true" value="fixed" />
                <textarea id="many" name="b" readonly="true" value="fixed"></textarea>
            </form>
        }
    });
    let registry = rt.text_input_registry();
    for id in ["one", "many"] {
        let id = {
            let (arena, ..) = rt.geometry();
            let node = arena.find(|a, n| a.id_attr(n) == Some(id)).unwrap();
            arena.id_attr(node).unwrap().to_string()
        };
        let (_, _, _, font) = rt.geometry_and_font_mut();
        registry.apply(&id, TextEditOp::SelectAll, font);
        assert_eq!(registry.selected_text(&id).as_deref(), Some("fixed"));
        for op in [
            TextEditOp::InsertOrReplace("x".to_string()),
            TextEditOp::InsertOrReplace(String::new()),
            TextEditOp::Delete,
            TextEditOp::Backdelete,
            TextEditOp::DeleteWord,
            TextEditOp::BackdeleteWord,
        ] {
            assert_eq!(registry.apply(&id, op, font), None, "{id} refuses edits");
        }
        registry.set_compose(&id, "ime", None, font);
        assert_eq!(registry.undo(&id, font), None);
        assert_eq!(
            registry.selected_text(&id).as_deref(),
            Some("fixed"),
            "{id} kept its text"
        );
    }
}

fn bubble_runtime(width: &'static str) -> UiRuntime {
    runtime("", move || {
        view! {
            <div>
                <form id="f">
                    <input id="name" type="text" name="name" required="true"
                        style={format!("width: {width}px; height: 30px;")} />
                    <button id="go">{"Go"}</button>
                </form>
            </div>
        }
    })
}

fn bubble_nodes(rt: &UiRuntime) -> Option<(NodeId, NodeId)> {
    let (arena, ..) = rt.geometry();
    let root = arena.find(|a, id| a.id_attr(id) == Some("florui-validation-bubble"))?;
    let arrow = arena.children(root).iter().copied().find(|&c| {
        arena
            .classes(c)
            .iter()
            .any(|k| k == crate::BUBBLE_ARROW_CLASS)
    })?;
    Some((root, arrow))
}

fn rect(rt: &UiRuntime, node: NodeId) -> (f32, f32, f32, f32) {
    let (arena, _, layouts) = rt.geometry();
    let (x, y) = florui_layout::absolute_position(arena, layouts, node);
    (x, y, layouts[&node].width, layouts[&node].height)
}

#[test]
fn a_blocked_submit_shows_the_first_invalid_controls_message_and_nothing_else_does() {
    let mut rt = bubble_runtime("220");
    assert!(bubble_nodes(&rt).is_none(), "no bubble before a submit");
    let (form, go) = (node(&rt, "f"), node(&rt, "go"));
    rt.submit_form(form, Some(go), false);
    rt.update(viewport());
    let (root, _) = bubble_nodes(&rt).expect("the bubble is in the tree");
    let (arena, ..) = rt.geometry();
    assert!(
        arena
            .text_content(arena.children(root)[1])
            .contains("Please fill out this field.")
    );
}

/// Measured in Edge: the box is centred on the field and starts 8px below
/// it; the arrow sits 17px in from the box's left edge, but never less than
/// 7px in from the field's own left edge.
#[test]
fn the_bubble_sits_eight_pixels_below_its_field_centred_with_the_arrow_rule() {
    let mut rt = bubble_runtime("400");
    let (form, go, field) = (node(&rt, "f"), node(&rt, "go"), node(&rt, "name"));
    rt.submit_form(form, Some(go), false);
    rt.update(viewport());
    let (root, arrow) = bubble_nodes(&rt).unwrap();
    let (fx, fy, fw, fh) = rect(&rt, field);
    let (bx, by, bw, _) = rect(&rt, root);
    assert!((by - (fy + fh) - 8.0).abs() < 0.01, "8px below the field");
    assert!(
        ((bx + bw / 2.0) - (fx + fw / 2.0)).abs() < 0.5,
        "centred on the field"
    );
    let (ax, _, aw, _) = rect(&rt, arrow);
    let tip_x = ax + aw / 2.0;
    assert!(
        (tip_x - (bx + 17.0)).abs() < 0.5,
        "17px in from the box's left edge"
    );
}

#[test]
fn a_narrow_field_keeps_the_arrow_over_itself_not_over_the_clamped_box() {
    let mut rt = bubble_runtime("60");
    let (form, go, field) = (node(&rt, "f"), node(&rt, "go"), node(&rt, "name"));
    rt.submit_form(form, Some(go), false);
    rt.update(viewport());
    let (root, arrow) = bubble_nodes(&rt).unwrap();
    let (fx, ..) = rect(&rt, field);
    let (bx, ..) = rect(&rt, root);
    let (ax, _, aw, _) = rect(&rt, arrow);
    assert!(bx >= 0.0, "kept inside the window");
    assert!(
        ax + aw / 2.0 >= fx + 7.0 - 0.5,
        "the arrow is at least 7px in from the field's left"
    );
}

#[test]
fn typing_moving_focus_or_removing_the_field_dismisses_the_bubble_but_a_caret_move_does_not() {
    let mut rt = bubble_runtime("220");
    let show = |rt: &mut UiRuntime| {
        let (form, go) = (node(rt, "f"), node(rt, "go"));
        rt.submit_form(form, Some(go), false);
        rt.update(viewport());
        assert!(bubble_nodes(rt).is_some());
    };
    let field = node(&rt, "name");
    show(&mut rt);
    let registry = rt.text_input_registry();
    let field_id = node_id_string(&rt, "name");
    let (_, _, _, font) = rt.geometry_and_font_mut();
    registry.apply(&field_id, florui_text::editing::TextEditOp::MoveLeft, font);
    rt.update(viewport());
    assert!(bubble_nodes(&rt).is_some(), "a caret move keeps it");

    rt.commit_value(field, "x".to_string());
    rt.update(viewport());
    assert!(bubble_nodes(&rt).is_none(), "an edit dismisses it");

    rt.commit_value(field, String::new());
    rt.update(viewport());
    show(&mut rt);
    let go = node(&rt, "go");
    rt.set_focused(Some(go), true);
    rt.update(viewport());
    assert!(
        bubble_nodes(&rt).is_none(),
        "focus moving away dismisses it"
    );
}

fn node_id_string(rt: &UiRuntime, id: &str) -> String {
    let (arena, ..) = rt.geometry();
    let node = arena.find(|a, n| a.id_attr(n) == Some(id)).unwrap();
    arena.id_attr(node).unwrap().to_string()
}

fn message_of(rt: &UiRuntime, id: &str) -> Option<String> {
    let field = node(rt, id);
    rt.with_form_context(|ctx| crate::validation_message::validation_message(ctx, field))
}

/// Every string measured in Edge (`validationMessage`).
#[test]
fn validation_messages_match_what_edge_reported() {
    let rt = runtime("", || {
        view! {
            <form>
                <input id="req" type="text" required="true" />
                <input id="chk" type="checkbox" required="true" />
                <input id="rad" type="radio" name="g" required="true" />
                <select id="sel" required="true"><option value="">{"--"}</option><option>{"x"}</option></select>
                <textarea id="ta" required="true"></textarea>
                <input id="mail" type="email" required="true" />
                <input id="pat" type="text" pattern="[a-z]+" value="A1" />
                <input id="custom" type="text" required="true" custom_validity="Taken!" />
                <input id="ok" type="text" value="fine" />
                <input id="n-under" type="number" min="1" step="2" value="0"
                    oninput={move |_v: String| {}} />
                <input id="n-over" type="number" min="1" max="10" step="3" value="12"
                    oninput={move |_v: String| {}} />
                <input id="n-step" type="number" min="1" max="10" step="3" value="5"
                    oninput={move |_v: String| {}} />
                <input id="n-half" type="number" min="0.5" step="1" value="7"
                    oninput={move |_v: String| {}} />
                <input id="n-edge" type="number" min="1" max="5" step="2" value="6"
                    oninput={move |_v: String| {}} />
            </form>
        }
    });
    let expected = [
        ("req", "Please fill out this field."),
        ("chk", "Please check this box if you want to proceed."),
        ("rad", "Please select one of these options."),
        ("sel", "Please select an item in the list."),
        ("ta", "Please fill out this field."),
        ("mail", "Please fill out this field."),
        ("pat", "Please match the requested format."),
        ("custom", "Taken!"),
        ("n-under", "Value must be greater than or equal to 1."),
        ("n-over", "Value must be less than or equal to 10."),
        (
            "n-step",
            "Please enter a valid value. The two nearest valid values are 4 and 7.",
        ),
        (
            "n-half",
            "Please enter a valid value. The two nearest valid values are 6.5 and 7.5.",
        ),
        ("n-edge", "Value must be less than or equal to 5."),
    ];
    for (id, message) in expected {
        assert_eq!(message_of(&rt, id).as_deref(), Some(message), "{id}");
    }
    assert_eq!(message_of(&rt, "ok"), None, "a valid field has no message");
}

#[test]
fn length_messages_singularize_and_the_pattern_is_reported_before_minlength() {
    let rt = runtime("", || {
        view! {
            <form>
                <input id="one" type="text" minlength="2" value="a" />
                <input id="both" type="text" minlength="5" pattern="[a-z]+" value="AB" />
                <textarea id="area" minlength="5" value="abc"></textarea>
            </form>
        }
    });
    // Length limits only apply once the user has edited the value.
    for id in ["one", "both", "area"] {
        let field = node(&rt, id);
        rt.commit_value(field, String::new());
    }
    assert_eq!(
        message_of(&rt, "one").as_deref(),
        Some(
            "Please lengthen this text to 2 characters or more (you are currently using 1 character)."
        )
    );
    assert_eq!(
        message_of(&rt, "both").as_deref(),
        Some("Please match the requested format.")
    );
    assert_eq!(
        message_of(&rt, "area").as_deref(),
        Some(
            "Please lengthen this text to 5 characters or more (you are currently using 3 characters)."
        )
    );
}

#[test]
fn a_number_typed_into_bad_input_reports_a_number_is_needed_even_when_required() {
    let mut rt = runtime("", || {
        view! {
            <form>
                <input id="n" type="number" required="true" value="" oninput={move |_v: String| {}} />
            </form>
        }
    });
    let key = node_id_string(&rt, "n");
    let registry = rt.text_input_registry();
    let (_, _, _, font) = rt.geometry_and_font_mut();
    registry.apply(
        &key,
        florui_text::editing::TextEditOp::InsertOrReplace("e".to_string()),
        font,
    );
    assert_eq!(
        message_of(&rt, "n").as_deref(),
        Some("Please enter a number.")
    );
}

/// A new textarea's caret starts at the start of its text; a single-line
/// field's starts at the end (both measured in Edge).
#[test]
fn a_new_textarea_starts_with_its_caret_at_the_start_and_an_input_at_the_end() {
    let rt = runtime("", || {
        view! {
            <form>
                <textarea id="area" value="one\ntwo"></textarea>
                <input id="line" type="text" value="hello" />
            </form>
        }
    });
    let registry = rt.text_input_registry();
    let mut rt = rt;
    for (id, expected) in [("area", 0), ("line", 5)] {
        let key = node_id_string(&rt, id);
        let (_, _, _, font) = rt.geometry_and_font_mut();
        registry.apply(&key, florui_text::editing::TextEditOp::SelectLineEnd, font);
        let selected = registry.selected_text(&key).unwrap_or_default();
        // From the caret to the end of its line: everything for the start,
        // nothing for the end.
        assert_eq!(selected.is_empty(), expected != 0, "{id}");
    }
}

/// Rows measured in Edge with real key presses and a real paste.
#[test]
fn maxlength_blocks_typing_and_cuts_a_paste_to_fit() {
    use florui_text::editing::TextEditOp;
    let mut rt = runtime("", || {
        view! {
            <form>
                <input id="typed" type="text" maxlength="5" value="" />
                <input id="pasted" type="text" maxlength="5" value="ab" />
                <textarea id="area" maxlength="5" value=""></textarea>
                <input id="over" type="text" maxlength="3" value="abcdef" />
                <input id="num" type="number" maxlength="2" value="" />
                <input id="picked" type="text" maxlength="5" value="abcde" />
            </form>
        }
    });
    let registry = rt.text_input_registry();
    let insert = |rt: &mut UiRuntime, id: &str, text: &str| -> Option<String> {
        let key = node_id_string(rt, id);
        let (_, _, _, font) = rt.geometry_and_font_mut();
        registry.apply(&key, TextEditOp::InsertOrReplace(text.to_string()), font)
    };
    let mut committed = String::new();
    for c in "abcdefgh".chars() {
        if let Some(text) = insert(&mut rt, "typed", &c.to_string()) {
            committed = text;
        }
    }
    assert_eq!(committed, "abcde", "typing stops at the limit");

    assert_eq!(
        insert(&mut rt, "pasted", "123456789").as_deref(),
        Some("ab123"),
        "a paste is cut to what still fits"
    );

    let mut area = String::new();
    for piece in ["a", "\n", "b", "\n", "c", "\n", "d"] {
        if let Some(text) = insert(&mut rt, "area", piece) {
            area = text;
        }
    }
    assert_eq!(area, "a\nb\nc", "a line break counts as one character");

    assert_eq!(
        insert(&mut rt, "over", "X"),
        None,
        "a value already over the limit takes nothing more"
    );

    assert_eq!(
        insert(&mut rt, "num", "12345").as_deref(),
        Some("12345"),
        "a number field ignores maxlength"
    );

    let key = node_id_string(&rt, "picked");
    let (_, _, _, font) = rt.geometry_and_font_mut();
    registry.apply(&key, TextEditOp::SelectAll, font);
    assert_eq!(
        registry
            .apply(
                &key,
                TextEditOp::InsertOrReplace("123456".to_string()),
                font
            )
            .as_deref(),
        Some("12345"),
        "replacing a selection frees its room"
    );
}
