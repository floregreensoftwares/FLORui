//! The registry of form-associated controls. Submission, validation, the
//! form pseudo-classes and radio grouping read a control only through its
//! [`ControlSpec`], so a new control or `<input type>` is one entry in
//! [`CONTROLS`].

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
    /// What each text field was typed with, for `badInput`.
    pub text_inputs: &'a crate::text_input::TextInputRegistry,
}

/// Which constraints a control currently violates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Validity {
    pub value_missing: bool,
    pub too_short: bool,
    pub too_long: bool,
    pub pattern_mismatch: bool,
    pub type_mismatch: bool,
    pub bad_input: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
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
        entries: sanitized_entries,
        constraints: email_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("url"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: sanitized_entries,
        constraints: url_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("tel"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: text_entries,
        constraints: text_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("search"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: text_entries,
        constraints: text_constraints,
    },
    ControlSpec {
        tag: "input",
        input_type: Some("number"),
        submittable: true,
        validatable: true,
        text_field: true,
        entries: number_entries,
        constraints: number_constraints,
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
    ControlSpec {
        tag: "textarea",
        input_type: None,
        submittable: true,
        validatable: true,
        text_field: true,
        entries: textarea_entries,
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

/// A textarea's value with every line break as a single `\n`.
fn textarea_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    let value = text_value(ctx, node);
    vec![value.replace("\r\n", "\n").replace('\r', "\n")]
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
        bad_input: false,
        range_underflow: false,
        range_overflow: false,
        step_mismatch: false,
        custom_error: false,
    }
}

/// `pattern` must match the whole value. One that doesn't compile is ignored,
/// as in a browser; that includes JavaScript-only syntax `regex` lacks
/// (lookaround, backreferences).
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
    Url,
    Number,
}

pub(crate) fn value_kind(arena: &Arena, node: NodeId) -> ValueKind {
    match (arena.tag(node), arena.input_type(node)) {
        ("input", Some("number")) => ValueKind::Number,
        ("input", Some("url")) => ValueKind::Url,
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
        ValueKind::Number => parse_number(raw).map_or_else(String::new, |_| raw.to_string()),
        ValueKind::Url => raw
            .chars()
            .filter(|c| !matches!(c, '\r' | '\n'))
            .collect::<String>()
            .trim_matches(|c: char| c.is_ascii_whitespace())
            .to_string(),
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

fn sanitized_value(ctx: &FormContext, node: NodeId) -> String {
    sanitize_value(value_kind(ctx.arena, node), text_value(ctx, node))
}

fn sanitized_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    vec![sanitized_value(ctx, node)]
}

fn email_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let value = sanitized_value(ctx, node);
    let multiple = ctx.arena.attr_flag(node, "multiple");
    Validity {
        value_missing: is_required(ctx.arena, node) && value.is_empty(),
        type_mismatch: !is_valid_email(&value, multiple),
        ..text_constraints(ctx, node)
    }
}

/// Whether a sanitized `type=url` value parses as an absolute URL, as
/// measured in Edge: a scheme (a letter, then letters, digits, `+`, `-` or
/// `.`) and a colon, and for `http`, `https`, `ftp`, `ws` and `wss` a
/// non-empty host after the slashes. An empty value is never a mismatch.
pub(crate) fn is_valid_url(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let Some((scheme, rest)) = value.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    let valid_scheme = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return false;
    }
    let special = ["http", "https", "ftp", "ws", "wss"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s));
    if !special {
        return true;
    }
    let after_slashes = rest.trim_start_matches(['/', '\\']);
    let host = after_slashes
        .split(['/', '\\', '?', '#'])
        .next()
        .unwrap_or("");
    !host.is_empty()
}

fn url_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let value = sanitized_value(ctx, node);
    Validity {
        value_missing: is_required(ctx.arena, node) && value.is_empty(),
        type_mismatch: !is_valid_url(&value),
        ..text_constraints(ctx, node)
    }
}

/// Characters a `type=number` field lets through when typed or pasted
/// (measured in Edge: letters and commas are dropped, `e`/`E`, sign and
/// point are kept even where they make the text invalid).
pub(crate) fn filter_typed_text(kind: ValueKind, text: &str) -> String {
    match kind {
        ValueKind::Number => text
            .chars()
            .filter(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e' | 'E'))
            .collect(),
        _ => text.to_string(),
    }
}

static FLOAT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^-?(?:[0-9]+|[0-9]*\.[0-9]+)(?:[eE][-+]?[0-9]+)?$")
        .expect("the float pattern is a constant, valid regex")
});

/// `raw` as a number if it is a valid HTML floating-point number (no
/// leading `+`, no surrounding whitespace, `.5` fine, `5.` not).
fn parse_number(raw: &str) -> Option<f64> {
    FLOAT
        .is_match(raw)
        .then(|| raw.parse::<f64>().ok())
        .flatten()
        .filter(|n| n.is_finite())
}

fn number_attr(arena: &Arena, node: NodeId, name: &str) -> Option<f64> {
    arena
        .attr(node, name)
        .and_then(|raw| parse_number(raw.trim()))
}

/// `step="any"` disables stepping; otherwise a positive `step`, else 1.
fn number_step(arena: &Arena, node: NodeId) -> Option<f64> {
    match arena.attr(node, "step").map(str::trim) {
        Some("any") => None,
        Some(raw) => Some(parse_number(raw).filter(|s| *s > 0.0).unwrap_or(1.0)),
        None => Some(1.0),
    }
}

fn number_value(ctx: &FormContext, node: NodeId) -> String {
    sanitize_value(ValueKind::Number, text_value(ctx, node))
}

fn number_entries(ctx: &FormContext, node: NodeId) -> Vec<String> {
    vec![number_value(ctx, node)]
}

/// A number field's `min` and `max`, when they are valid numbers.
pub(crate) fn number_limits(arena: &Arena, node: NodeId) -> (Option<f64>, Option<f64>) {
    (
        number_attr(arena, node, "min"),
        number_attr(arena, node, "max"),
    )
}

/// The valid values just below and just above a number field's off-grid
/// value, each `None` when it would fall outside `min`/`max`; `None` when
/// the field has no step grid or no number to place on it.
pub(crate) fn number_grid(ctx: &FormContext, node: NodeId) -> Option<(Option<f64>, Option<f64>)> {
    let arena = ctx.arena;
    let number = parse_number(&number_value(ctx, node))?;
    let step = number_step(arena, node)?;
    let (min, max) = number_limits(arena, node);
    let base = min.unwrap_or(0.0);
    let below = base + ((number - base) / step).floor() * step;
    let above = below + step;
    let round = |value: f64| (value * 1e9).round() / 1e9;
    let (below, above) = (round(below), round(above));
    Some((
        min.is_none_or(|min| below >= min).then_some(below),
        max.is_none_or(|max| above <= max).then_some(above),
    ))
}

/// Whether `value` sits on the step grid anchored at `base`.
fn on_step_grid(value: f64, base: f64, step: f64) -> bool {
    let ratio = (value - base) / step;
    (ratio - ratio.round()).abs() <= 1e-9 * ratio.abs().max(1.0)
}

fn number_constraints(ctx: &FormContext, node: NodeId) -> Validity {
    let arena = ctx.arena;
    let value = number_value(ctx, node);
    let bad_input = arena
        .id_attr(node)
        .is_some_and(|id| ctx.text_inputs.is_bad_input(id));
    let mut result = Validity {
        // Measured in Edge: typing `e` into a required number field reports
        // bad input, not a missing value.
        value_missing: is_required(arena, node) && value.is_empty() && !bad_input,
        bad_input,
        ..Validity::default()
    };
    let Some(number) = parse_number(&value) else {
        return result;
    };
    let (min, max) = (
        number_attr(arena, node, "min"),
        number_attr(arena, node, "max"),
    );
    result.range_underflow = min.is_some_and(|min| number < min);
    result.range_overflow = max.is_some_and(|max| number > max);
    if let Some(step) = number_step(arena, node) {
        result.step_mismatch = !on_step_grid(number, min.unwrap_or(0.0), step);
    }
    result
}

/// The value one arrow-key step from `current` (empty counts as 0) in
/// `direction` (`1` up, `-1` down); `None` at a limit. Measured in Edge: an
/// off-grid value snaps to the next grid point (2.5 up gives 3).
pub(crate) fn stepped_number(
    arena: &Arena,
    node: NodeId,
    current: &str,
    direction: i32,
) -> Option<String> {
    let step = number_step(arena, node).unwrap_or(1.0);
    let (min, max) = (
        number_attr(arena, node, "min"),
        number_attr(arena, node, "max"),
    );
    let base = min.unwrap_or(0.0);
    let start = parse_number(current).unwrap_or(0.0);
    let position = (start - base) / step;
    let eps = 1e-9 * position.abs().max(1.0);
    let index = if direction >= 0 {
        (position + eps).floor() + 1.0
    } else {
        (position - eps).ceil() - 1.0
    };
    let mut next = base + index * step;
    if let Some(max) = max
        && next > max
    {
        next = base + ((max - base) / step + eps).floor() * step;
    }
    if let Some(min) = min
        && next < min
    {
        next = min;
    }
    let next = (next * 1e9).round() / 1e9;
    let as_text = next.to_string();
    (parse_number(current) != Some(next)).then_some(as_text)
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

    /// Rows measured in Edge for `type=number`.

    #[test]
    fn urls_are_valid_or_not_as_edge_measured() {
        for valid in [
            "",
            "http://a",
            "https://example.com/path?q=1",
            "a:b",
            "http:/x",
            "ftp://x",
            "mailto:me@x.com",
            "javascript:1",
            "HTTP://A.COM",
            "file:///c:/x",
            "data:text/plain,hi",
            "http://[::1]",
            "http://a..b",
            "http://-a.com",
            "http://exa mple.com",
        ] {
            assert!(is_valid_url(valid), "{valid:?} is a valid URL");
        }
        for invalid in [
            "foo", "//x.com", "http://", "://x", "1:2", "x y:z", "h@st:1",
        ] {
            assert!(!is_valid_url(invalid), "{invalid:?} is not a URL");
        }
    }

    #[test]
    fn url_sanitizing_trims_and_drops_line_breaks() {
        assert_eq!(
            sanitize_value(ValueKind::Url, "  http://a.com \r\n"),
            "http://a.com"
        );
    }
    #[test]
    fn number_sanitizing_keeps_only_valid_floats() {
        for keep in ["", "1", "1.5", "-2", "1e3", ".5", "1e5"] {
            assert_eq!(sanitize_value(ValueKind::Number, keep), keep, "{keep:?}");
        }
        for drop in ["abc", "1e", "+1", "1,5", " 4 ", "-", ".", "5."] {
            assert_eq!(sanitize_value(ValueKind::Number, drop), "", "{drop:?}");
        }
    }

    #[test]
    fn number_typing_keeps_digits_sign_point_and_exponent_only() {
        let filter = |t: &str| filter_typed_text(ValueKind::Number, t);
        assert_eq!(filter("12abc"), "12");
        assert_eq!(filter("1,5"), "15");
        assert_eq!(filter("-e.+3"), "-e.+3");
        assert_eq!(filter_typed_text(ValueKind::Plain, "12abc"), "12abc");
    }

    fn number_arena(attrs: &str) -> (florui_style::Arena, NodeId) {
        let tree: florui::Element = match attrs {
            "min1max5step2" => florui::prelude::view! {
                <input type="number" min="1" max="5" step="2" />
            },
            "min0.5" => florui::prelude::view! { <input type="number" min="0.5" step="1" /> },
            _ => florui::prelude::view! { <input type="number" /> },
        };
        let arena = florui_style::Arena::build(&tree);
        let node = arena.roots()[0];
        (arena, node)
    }

    #[test]
    fn number_arrow_steps_match_what_edge_measured() {
        let (arena, node) = number_arena("default");
        let up = |from: &str| stepped_number(&arena, node, from, 1);
        let down = |from: &str| stepped_number(&arena, node, from, -1);
        assert_eq!(up("").as_deref(), Some("1"));
        assert_eq!(down("2").as_deref(), Some("1"));
        assert_eq!(down("0").as_deref(), Some("-1"));
        assert_eq!(up("2.5").as_deref(), Some("3"));
        assert_eq!(up("5").as_deref(), Some("6"));

        let (arena, node) = number_arena("min1max5step2");
        let up = |from: &str| stepped_number(&arena, node, from, 1);
        let down = |from: &str| stepped_number(&arena, node, from, -1);
        assert_eq!(up("3").as_deref(), Some("5"));
        assert_eq!(up("5"), None, "already at the maximum");
        assert_eq!(down("5").as_deref(), Some("3"));
        assert_eq!(down("3").as_deref(), Some("1"));
        assert_eq!(down("1"), None, "already at the minimum");
        assert_eq!(
            down("").as_deref(),
            Some("1"),
            "empty steps down to the minimum"
        );
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
