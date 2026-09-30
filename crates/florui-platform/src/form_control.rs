//! The registry of form-associated controls. Submission, validation,
//! the `:valid`/`:required`/... pseudo-classes and radio grouping
//! all read a control through its [`ControlSpec`] and never match on a
//! tag or `type` themselves, so supporting a new control (`<textarea>`
//! editing, another `<input type>`) is one entry in [`CONTROLS`].

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use florui_style::{Arena, FocusPath, NodeId};

use crate::select::OptionSummary;

/// What a control's current attributes say, in the units the platform
/// can't read off the [`Arena`] alone.
pub(crate) struct FormContext<'a> {
    pub arena: &'a Arena,
    /// A closed `<select>`'s options aren't in the arena, so its value
    /// comes from here, keyed by the select's `id`.
    pub options: &'a HashMap<String, Vec<OptionSummary>>,
    /// Text controls the user has typed into. Only a user
    /// edit can trip `minlength`/`maxlength`, as in a browser.
    pub edited: &'a HashSet<FocusPath>,
}

/// Which constraints a control currently violates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Validity {
    pub value_missing: bool,
    pub too_short: bool,
    pub too_long: bool,
    pub pattern_mismatch: bool,
    pub type_mismatch: bool,
    pub custom_error: bool,
}

impl Validity {
    pub fn is_valid(self) -> bool {
        self == Self::default()
    }
}

pub(crate) struct ControlSpec {
    pub tag: &'static str,
    /// The effective `type` for an `<input>` (`"text"` when absent);
    /// `None` for every other tag.
    pub input_type: Option<&'static str>,
    /// Contributes [`Self::entries`] to `FormData` when named and enabled.
    pub submittable: bool,
    /// A candidate for constraint validation: enabled and not read-only.
    pub validatable: bool,
    /// A text field: `readonly`, `minlength`, `maxlength`, `pattern`, and
    /// `:read-write` apply.
    pub text_field: bool,
    pub entries: fn(&FormContext, NodeId) -> Vec<String>,
    pub constraints: fn(&FormContext, NodeId) -> Validity,
}

fn no_entries(_: &FormContext, _: NodeId) -> Vec<String> {
    Vec::new()
}

fn unconstrained(_: &FormContext, _: NodeId) -> Validity {
    Validity::default()
}

pub(crate) static CONTROLS: &[ControlSpec] = &[
    ControlSpec {
        tag: "input",
        input_type: Some("text"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: text_entries,
        constraints: text_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("password"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: text_entries,
        constraints: text_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("checkbox"),
        submittable: true,
        validatable: true,
        text_field: false,
        entries: checkable_entries,
        constraints: checkbox_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("radio"),
        submittable: true,
        validatable: true,
        text_field: false,
        entries: checkable_entries,
        constraints: radio_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("range"),
        submittable: true,
        validatable: true,
        text_field: false,
        entries: range_entries,
        constraints: unconstrained,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("email"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: email_entries,
        constraints: email_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("hidden"),
        submittable: true,
        validatable: false,
        text_field: false,
        entries: text_entries,
        constraints: unconstrained,
    },
    ControlSpec {
        tag: "select",
        input_type: None,
        submittable: true,
        validatable: true,
        text_field: false,
        entries: select_entries,
        constraints: select_constraints,
    },
    // No editing behavior yet, so it only takes part in the tree; its
    // registry entry is what a real implementation builds on.
    ControlSpec {
        tag: "textarea",
        input_type: None,
        submittable: true,
        validatable: true,
        text_field: true,
        entries: text_entries,
        constraints: text_constraints,
    },
    ControlSpec {
        tag: "button",
        input_type: None,
        submittable: false,
        validatable: true,
        text_field: false,
        entries: no_entries,
        constraints: unconstrained,
    },
];

/// The spec for `node`, if it is a form-associated control.
pub(crate) fn spec(arena: &Arena, node: NodeId) -> Option<&'static ControlSpec> {
    let tag = arena.tag(node);
    let input_type = (tag == "input").then(|| arena.input_type(node).unwrap_or("text"));
    CONTROLS
        .iter()
        .find(|spec| spec.tag == tag && spec.input_type == input_type)
}

/// What a `<button>` does when activated: `type` is `submit` unless it
/// says `reset` or `button`; anything else falls back to `submit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    Submit,
    Reset,
    Plain,
}

pub(crate) fn button_kind(arena: &Arena, node: NodeId) -> Option<ButtonKind> {
    (arena.tag(node) == "button").then(|| match arena.input_type(node) {
        Some("reset") => ButtonKind::Reset,
        Some("button") => ButtonKind::Plain,
        _ => ButtonKind::Submit,
    })
}

/// Whether `node` takes part in constraint validation right now.
pub(crate) fn will_validate(arena: &Arena, node: NodeId) -> bool {
    let Some(spec) = spec(arena, node) else {
        return false;
    };
    spec.validatable
        && !arena.is_disabled(node)
        && !(spec.text_field && arena.attr_flag(node, "readonly"))
        && button_kind(arena, node).is_none_or(|kind| kind == ButtonKind::Submit)
}

/// Constraint violations of `node`; valid for anything that isn't a
/// validation candidate.
pub(crate) fn validity(ctx: &FormContext, node: NodeId) -> Validity {
    match spec(ctx.arena, node) {
        Some(spec) if will_validate(ctx.arena, node) => {
            let mut result = (spec.constraints)(ctx, node);
            result.custom_error = ctx
                .arena
                .attr(node, "custom_validity")
                .is_some_and(|message| !message.is_empty());
            result
        }
        _ => Validity::default(),
    }
}

/// The `type`-independent `required` flag.
pub(crate) fn is_required(arena: &Arena, node: NodeId) -> bool {
    arena.attr_flag(node, "required")
}

fn text_value<'a>(ctx: &'a FormContext, node: NodeId) -> &'a str {
    ctx.arena.value_attr(node).unwrap_or("")
}

fn text_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    vec![text_value(ctx, node).to_string()]
}

fn text_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let arena = ctx.arena;
    let value = text_value(ctx, node);
    let length = value.encode_utf16().count();
    let edited = ctx.edited.contains(&FocusPath::of(arena, node));
    let bound = |name: &str| {
        arena
            .attr(node, name)
            .and_then(|v| v.trim().parse::<usize>().ok())
    };
    Validity {
        value_missing: is_required(arena, node) && value.is_empty(),
        too_short: edited && length > 0 && bound("minlength").is_some_and(|min| length < min),
        too_long: edited && bound("maxlength").is_some_and(|max| length > max),
        pattern_mismatch: !value.is_empty()
            && arena
                .attr(node, "pattern")
                .is_some_and(|pattern| !matches_pattern(pattern, value)),
        type_mismatch: false,
        custom_error: false,
    }
}

/// `pattern` must match the whole value. A pattern that doesn't compile is
/// ignored, as in a browser. The `regex` crate's syntax is a subset of
/// JavaScript's (no lookaround or backreferences), so those patterns are
/// ignored here rather than mismatching.
fn matches_pattern(pattern: &str, value: &str) -> bool {
    match regex::Regex::new(&format!("^(?:{pattern})$")) {
        Ok(regex) => regex.is_match(value),
        Err(_) => true,
    }
}

fn checkable_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    if ctx.arena.is_checked(node) {
        vec![ctx.arena.value_attr(node).unwrap_or("on").to_string()]
    } else {
        Vec::new()
    }
}

fn checkbox_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    Validity {
        value_missing: is_required(ctx.arena, node) && !ctx.arena.is_checked(node),
        ..Validity::default()
    }
}

fn radio_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let group = crate::form::radio_group(ctx.arena, node);
    let required = group.iter().any(|&member| is_required(ctx.arena, member));
    let any_checked = group.iter().any(|&member| ctx.arena.is_checked(member));
    Validity {
        value_missing: required && !any_checked,
        ..Validity::default()
    }
}

fn range_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    let arena = ctx.arena;
    let (min, max, step) = (
        arena.range_min(node),
        arena.range_max(node),
        arena.range_step(node),
    );
    let mut value = arena.range_value(node).clamp(min.min(max), min.max(max));
    if step > 0.0 {
        value = (min + ((value - min) / step).round() * step).clamp(min.min(max), min.max(max));
    }
    vec![value.to_string()]
}

/// The values a select currently holds: every selected option, or for a
/// single select with none selected, its first option — what a browser
/// shows and submits.
fn selected_values(ctx: &FormContext, node: NodeId) -> Vec<String> {
    let Some(options) = ctx.arena.id_attr(node).and_then(|id| ctx.options.get(id)) else {
        return Vec::new();
    };
    let selected: Vec<String> = options
        .iter()
        .filter(|option| option.selected)
        .map(|option| option.value.clone())
        .collect();
    if ctx.arena.is_multiple(node) {
        return selected;
    }
    match selected.into_iter().next_back() {
        Some(value) => vec![value],
        None => options
            .first()
            .map(|option| vec![option.value.clone()])
            .unwrap_or_default(),
    }
}

fn select_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    selected_values(ctx, node)
}

fn select_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let values = selected_values(ctx, node);
    let placeholder_only = !ctx.arena.is_multiple(node) && values.iter().all(String::is_empty);
    Validity {
        value_missing: is_required(ctx.arena, node) && (values.is_empty() || placeholder_only),
        ..Validity::default()
    }
}

/// How a text field turns what the user typed into its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValueKind {
    Plain,
    Email { multiple: bool },
}

pub(crate) fn value_kind(arena: &Arena, node: NodeId) -> ValueKind {
    match (arena.tag(node), arena.input_type(node)) {
        ("input", Some("email")) => ValueKind::Email {
            multiple: arena.attr_flag(node, "multiple"),
        },
        _ => ValueKind::Plain,
    }
}

/// The value a field holds for `raw` typed text: an email drops line breaks
/// and surrounding whitespace, and with `multiple` each comma-separated
/// address is trimmed (measured in Edge).
pub(crate) fn sanitize_value(kind: ValueKind, raw: &str) -> String {
    match kind {
        ValueKind::Plain => raw.to_string(),
        ValueKind::Email { multiple } => {
            let flat: String = raw.chars().filter(|c| !matches!(c, '\r' | '\n')).collect();
            if multiple {
                flat.split(',')
                    .map(|part| part.trim_matches(|c: char| c.is_ascii_whitespace()))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                flat.trim_matches(|c: char| c.is_ascii_whitespace())
                    .to_string()
            }
        }
    }
}

/// The address grammar HTML defines for `type=email`.
static EMAIL_ADDRESS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$",
    )
    .expect("the email address pattern is a constant, valid regex")
});

/// Whether a sanitized email value is well formed; an empty value is never
/// a mismatch (that is `required`'s job).
pub(crate) fn is_valid_email(value: &str, multiple: bool) -> bool {
    if value.is_empty() {
        return true;
    }
    if multiple {
        value.split(',').all(|part| EMAIL_ADDRESS.is_match(part))
    } else {
        EMAIL_ADDRESS.is_match(value)
    }
}

fn email_value(ctx: &FormContext, node: NodeId) -> String {
    sanitize_value(value_kind(ctx.arena, node), text_value(ctx, node))
}

fn email_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    vec![email_value(ctx, node)]
}

fn email_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let value = email_value(ctx, node);
    let multiple = ctx.arena.attr_flag(node, "multiple");
    Validity {
        value_missing: is_required(ctx.arena, node) && value.is_empty(),
        type_mismatch: !is_valid_email(&value, multiple),
        ..text_constraints(ctx, node)
    }
}

#[cfg(test)]
mod tests {
    use florui_primitives::INITIAL_INPUT_TYPES;

    use super::*;

    #[test]
    fn every_supported_input_type_and_form_tag_has_a_registry_entry() {
        for input_type in INITIAL_INPUT_TYPES {
            assert!(
                CONTROLS
                    .iter()
                    .any(|spec| spec.tag == "input" && spec.input_type == Some(*input_type)),
                "input type `{input_type}` has no form control entry"
            );
        }
        for tag in ["button", "select", "textarea"] {
            assert!(CONTROLS.iter().any(|spec| spec.tag == tag), "`{tag}`");
        }
    }

    /// Every row measured in Edge (`validity.typeMismatch` on `type=email`).
    #[test]
    fn email_validity_matches_what_edge_measured() {
        let single = |raw: &str| {
            is_valid_email(
                &sanitize_value(ValueKind::Email { multiple: false }, raw),
                false,
            )
        };
        for ok in ["", "a@b", "a@b.c", " a@b.c "] {
            assert!(single(ok), "{ok:?} should be valid");
        }
        for bad in [
            "a",
            "a@",
            "@b.c",
            "a b@c.d",
            "a@b..c",
            "a@-b.c",
            "a@b.c,d@e.f",
            "\u{fc}n\u{ef}@x.de",
            "a@b.c.",
        ] {
            assert!(!single(bad), "{bad:?} should be a type mismatch");
        }
        let multiple = |raw: &str| {
            is_valid_email(
                &sanitize_value(ValueKind::Email { multiple: true }, raw),
                true,
            )
        };
        assert!(multiple("a@b.c,d@e.f") && multiple("a@b.c, d@e.f"));
        assert!(!multiple("a@b.c,x") && !multiple("a@b.c,"));
    }

    #[test]
    fn email_sanitizing_trims_and_drops_line_breaks() {
        let one = ValueKind::Email { multiple: false };
        let many = ValueKind::Email { multiple: true };
        assert_eq!(sanitize_value(one, "  a@b.c \n"), "a@b.c");
        assert_eq!(sanitize_value(many, "a@b.c,  d@e.f "), "a@b.c,d@e.f");
        assert_eq!(sanitize_value(ValueKind::Plain, "  keep  "), "  keep  ");
    }

    #[test]
    fn no_two_entries_claim_the_same_control() {
        for (index, a) in CONTROLS.iter().enumerate() {
            for b in &CONTROLS[index + 1..] {
                assert!(
                    (a.tag, a.input_type) != (b.tag, b.input_type),
                    "duplicate entry for {:?}",
                    (a.tag, a.input_type)
                );
            }
        }
    }
}
