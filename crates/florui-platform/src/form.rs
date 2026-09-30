//! `<form>` behavior built on the [`crate::form_control`] registry: which
//! form owns a control, what it would submit, whether it may be
//! submitted, and the form pseudo-class state of every control.

use florui::FormData;
use florui_style::{Arena, FormState, NodeId};

use crate::form_control::{
    ButtonKind, FormContext, button_kind, is_required, spec, validity, will_validate,
};

/// The `<form>` `node` belongs to: the one its `form` attribute names by
/// `id`, else its nearest ancestor `<form>`.
pub(crate) fn owner(arena: &Arena, node: NodeId) -> Option<NodeId> {
    if let Some(form_id) = arena.attr(node, "form") {
        return arena
            .find(|arena, id| arena.tag(id) == "form" && arena.id_attr(id) == Some(form_id));
    }
    let mut current = arena.parent(node);
    while let Some(ancestor) = current {
        if arena.tag(ancestor) == "form" {
            return Some(ancestor);
        }
        current = arena.parent(ancestor);
    }
    None
}

/// Every radio in `node`'s group: same non-empty `name`, same form
/// owner. A radio with no name is a group of one.
pub(crate) fn radio_group(arena: &Arena, node: NodeId) -> Vec<NodeId> {
    let Some(name) = arena.name(node).filter(|name| !name.is_empty()) else {
        return vec![node];
    };
    let form = owner(arena, node);
    arena.find_all(|arena, other| {
        arena.tag(other) == "input"
            && arena.input_type(other) == Some("radio")
            && arena.name(other) == Some(name)
            && owner(arena, other) == form
    })
}

/// Every form-associated control `form` owns, in tree order.
pub(crate) fn controls(arena: &Arena, form: NodeId) -> Vec<NodeId> {
    arena.find_all(|arena, node| spec(arena, node).is_some() && owner(arena, node) == Some(form))
}

/// What `form` submits: each enabled, named, submittable control's
/// entries in tree order, plus `submitter`'s own `name`/`value` if it is
/// a named submit button.
pub(crate) fn form_data(ctx: &FormContext, form: NodeId, submitter: Option<NodeId>) -> FormData {
    let arena = ctx.arena;
    let mut entries = Vec::new();
    for node in controls(arena, form) {
        if let Some(name) = arena.name(node).filter(|name| !name.is_empty())
            && let Some(spec) = spec(arena, node)
            && spec.submittable
            && !arena.is_disabled(node)
        {
            entries.extend(
                (spec.entries)(ctx, node)
                    .into_iter()
                    .map(|v| (name.to_string(), v)),
            );
        }
    }
    if let Some(button) = submitter
        && button_kind(arena, button) == Some(ButtonKind::Submit)
        && let Some(name) = arena.name(button).filter(|name| !name.is_empty())
    {
        entries.push((
            name.to_string(),
            arena.value_attr(button).unwrap_or("").to_string(),
        ));
    }
    FormData::new(entries)
}

/// Controls that currently violate a constraint, in tree order.
pub(crate) fn invalid_controls(ctx: &FormContext, form: NodeId) -> Vec<NodeId> {
    controls(ctx.arena, form)
        .into_iter()
        .filter(|&node| !validity(ctx, node).is_valid())
        .collect()
}

/// Whether submitting `form` via `submitter` skips validation:
/// `novalidate` on the form or `formnovalidate` on the submit button.
pub(crate) fn skips_validation(arena: &Arena, form: NodeId, submitter: Option<NodeId>) -> bool {
    arena.attr_flag(form, "novalidate")
        || submitter.is_some_and(|button| arena.attr_flag(button, "formnovalidate"))
}

/// The button that implicit submission (Enter in a text field) clicks:
/// `form`'s first submit button in tree order.
pub(crate) fn default_button(arena: &Arena, form: NodeId) -> Option<NodeId> {
    controls(arena, form)
        .into_iter()
        .find(|&node| button_kind(arena, node) == Some(ButtonKind::Submit))
}

/// Whether Enter in `field` submits its form when the form has no submit
/// button: only when it is the sole field that blocks implicit submission
/// (any text-like field does).
pub(crate) fn is_only_text_field(arena: &Arena, form: NodeId, field: NodeId) -> bool {
    controls(arena, form)
        .into_iter()
        .filter(|&node| spec(arena, node).is_some_and(|spec| spec.text_field))
        .eq(std::iter::once(field))
}

/// Whether `node` gets a [`FormState`] at all: a form control, or a
/// `<form>`/`<fieldset>`, whose validity summarizes their controls.
pub(crate) fn has_form_state(arena: &Arena, node: NodeId) -> bool {
    spec(arena, node).is_some() || matches!(arena.tag(node), "form" | "fieldset")
}

/// The pseudo-class state of `node`, or `None` for anything that has no
/// form state. `user_validated` is whether `:user-valid`/`:user-invalid`
/// may match yet. Matches what Chromium reports: barred controls
/// (disabled, read-only, a reset button) are neither `:valid` nor
/// `:invalid`, and a `<form>`/`<fieldset>` is `:invalid` while any of its
/// controls is.
pub(crate) fn form_state(
    ctx: &FormContext,
    node: NodeId,
    user_validated: bool,
) -> Option<FormState> {
    let arena = ctx.arena;
    let Some(spec) = spec(arena, node) else {
        return container_state(ctx, node);
    };
    let candidate = will_validate(arena, node);
    let valid = candidate && validity(ctx, node).is_valid();
    let invalid = candidate && !valid;
    let (in_range, out_of_range) = range_state(ctx, node, candidate);
    let writable =
        spec.text_field && !arena.attr_flag(node, "readonly") && !arena.is_disabled(node);
    let required = spec.tag != "button" && is_required(arena, node);
    Some(FormState {
        required,
        optional: !required,
        valid,
        invalid,
        user_valid: user_validated && valid,
        user_invalid: user_validated && invalid,
        read_only: !writable,
        read_write: writable,
        default: is_default(arena, node),
        in_range,
        out_of_range,
    })
}

fn container_state(ctx: &FormContext, node: NodeId) -> Option<FormState> {
    let arena = ctx.arena;
    let members = match arena.tag(node) {
        "form" => controls(arena, node),
        "fieldset" => descendants(arena, node)
            .into_iter()
            .filter(|&member| spec(arena, member).is_some())
            .collect(),
        _ => return None,
    };
    let invalid = members
        .into_iter()
        .any(|member| !validity(ctx, member).is_valid());
    Some(FormState {
        valid: !invalid,
        invalid,
        read_only: true,
        ..FormState::default()
    })
}

fn descendants(arena: &Arena, node: NodeId) -> Vec<NodeId> {
    let mut found = Vec::new();
    let mut stack = arena.children(node).to_vec();
    while let Some(id) = stack.pop() {
        found.push(id);
        stack.extend_from_slice(arena.children(id));
    }
    found
}

/// `:default`: a form's default submit button, or a checked checkbox or
/// radio.
fn is_default(arena: &Arena, node: NodeId) -> bool {
    match button_kind(arena, node) {
        Some(ButtonKind::Submit) => {
            owner(arena, node).and_then(|form| default_button(arena, form)) == Some(node)
        }
        Some(_) => false,
        None => {
            arena.tag(node) == "input"
                && matches!(arena.input_type(node), Some("checkbox") | Some("radio"))
                && arena.is_checked(node)
        }
    }
}

/// `(:in-range, :out-of-range)` for `node`. Measured in Edge: a `range`
/// input is always in range; a `number` input is out of range when it
/// violates `min`/`max`, and in range when empty or when it has a limit it
/// respects (a limitless number with a value matches neither); anything
/// else, or a control that is not a validation candidate, matches neither.
fn range_state(ctx: &FormContext, node: NodeId, candidate: bool) -> (bool, bool) {
    let arena = ctx.arena;
    if !candidate {
        return (false, false);
    }
    match arena.input_type(node) {
        Some("range") if arena.tag(node) == "input" => (true, false),
        Some("number") if arena.tag(node) == "input" => {
            let result = validity(ctx, node);
            let out = result.range_underflow || result.range_overflow;
            let empty = arena.value_attr(node).is_none_or(str::is_empty);
            let limited = arena.attr(node, "min").is_some() || arena.attr(node, "max").is_some();
            (!out && (empty || limited), out)
        }
        _ => (false, false),
    }
}
