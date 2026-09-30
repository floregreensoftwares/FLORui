//! The text of a control's validation message — what the bubble shows and
//! what `validationMessage` reports. Every string and the order that picks
//! between several failures were measured in Edge.

use florui_style::NodeId;

use crate::form_control::{
    FormContext, Validity, is_valid_email, number_grid, number_limits, sanitize_value, validity,
    value_kind,
};

/// The first failing constraint's message, or `None` for a valid control.
/// Edge's order: custom, bad input, missing, type, pattern, too long, too
/// short, underflow, overflow, step.
pub(crate) fn validation_message(ctx: &FormContext, node: NodeId) -> Option<String> {
    let result = validity(ctx, node);
    if result.is_valid() {
        return None;
    }
    let arena = ctx.arena;
    if result.custom_error {
        return arena.attr(node, "custom_validity").map(str::to_string);
    }
    if result.bad_input {
        return Some("Please enter a number.".to_string());
    }
    if result.value_missing {
        return Some(missing_message(arena, node).to_string());
    }
    if result.type_mismatch {
        let kind = value_kind(arena, node);
        let value = sanitize_value(kind, arena.value_attr(node).unwrap_or(""));
        return Some(email_message(&value, arena.attr_flag(node, "multiple")));
    }
    if result.pattern_mismatch {
        return Some("Please match the requested format.".to_string());
    }
    length_message(ctx, node, result).or_else(|| number_message(ctx, node, result))
}

fn missing_message(arena: &florui_style::Arena, node: NodeId) -> &'static str {
    match (arena.tag(node), arena.input_type(node)) {
        ("input", Some("checkbox")) => "Please check this box if you want to proceed.",
        ("input", Some("radio")) => "Please select one of these options.",
        ("select", _) => "Please select an item in the list.",
        _ => "Please fill out this field.",
    }
}

fn plural(count: usize) -> String {
    format!("{count} character{}", if count == 1 { "" } else { "s" })
}

fn length_message(ctx: &FormContext, node: NodeId, result: Validity) -> Option<String> {
    let arena = ctx.arena;
    let length = arena.value_attr(node).unwrap_or("").encode_utf16().count();
    let bound = |name: &str| {
        arena
            .attr(node, name)
            .and_then(|v| v.trim().parse::<usize>().ok())
    };
    if result.too_long {
        return Some(format!(
            "Please shorten this text to {} or less (you are currently using {}).",
            plural(bound("maxlength")?),
            plural(length)
        ));
    }
    if result.too_short {
        return Some(format!(
            "Please lengthen this text to {} or more (you are currently using {}).",
            plural(bound("minlength")?),
            plural(length)
        ));
    }
    None
}

fn number_message(ctx: &FormContext, node: NodeId, result: Validity) -> Option<String> {
    let (min, max) = number_limits(ctx.arena, node);
    if result.range_underflow {
        return Some(format!("Value must be greater than or equal to {}.", min?));
    }
    if result.range_overflow {
        return Some(format!("Value must be less than or equal to {}.", max?));
    }
    if result.step_mismatch {
        let (below, above) = number_grid(ctx, node)?;
        return Some(match (below, above) {
            (Some(a), Some(b)) => {
                format!("Please enter a valid value. The two nearest valid values are {a} and {b}.")
            }
            (Some(one), None) | (None, Some(one)) => {
                format!("Please enter a valid value. The nearest valid value is {one}.")
            }
            (None, None) => "Please enter a valid value.".to_string(),
        });
    }
    None
}

const LOCAL_SYMBOLS: &str = "!#$%&'*+/=?^_`{|}~-.";

/// The message for a value that is not a valid email address, following
/// Edge's own checks in the order it makes them; with `multiple` it names
/// the first address that fails.
fn email_message(value: &str, multiple: bool) -> String {
    let address = if multiple {
        value
            .split(',')
            .find(|part| part.is_empty() || !is_valid_email(part, false))
            .unwrap_or(value)
    } else {
        value
    };
    if address.is_empty() {
        return "Please enter a non-empty email address.".to_string();
    }
    let Some((local, domain)) = address.split_once('@') else {
        return format!(
            "Please include an '@' in the email address. '{address}' is missing an '@'."
        );
    };
    if local.is_empty() {
        return format!("Please enter a part followed by '@'. '{address}' is incomplete.");
    }
    if domain.is_empty() {
        return format!("Please enter a part following '@'. '{address}' is incomplete.");
    }
    if let Some(bad) = local
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || LOCAL_SYMBOLS.contains(*c)))
    {
        return format!("A part followed by '@' should not contain the symbol '{bad}'.");
    }
    if let Some(bad) = domain
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
    {
        return format!("A part following '@' should not contain the symbol '{bad}'.");
    }
    if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
        return format!("'.' is used at a wrong position in '{domain}'.");
    }
    "Please enter an email address.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row measured in Edge (`validationMessage` on `type=email`).
    #[test]
    fn email_messages_match_what_edge_reported() {
        let cases = [
            (
                "a",
                "Please include an '@' in the email address. 'a' is missing an '@'.",
            ),
            (
                "a@",
                "Please enter a part following '@'. 'a@' is incomplete.",
            ),
            (
                "@b.c",
                "Please enter a part followed by '@'. '@b.c' is incomplete.",
            ),
            (
                "a b@c.d",
                "A part followed by '@' should not contain the symbol ' '.",
            ),
            ("a@b..c", "'.' is used at a wrong position in 'b..c'."),
            ("a@-b.c", "Please enter an email address."),
            (
                "a@b.c,d@e.f",
                "A part following '@' should not contain the symbol ','.",
            ),
            (
                "\u{fc}n\u{ef}@x.de",
                "A part followed by '@' should not contain the symbol '\u{fc}'.",
            ),
            ("a@b.c.", "'.' is used at a wrong position in 'b.c.'."),
        ];
        for (value, expected) in cases {
            assert_eq!(email_message(value, false), expected, "{value:?}");
        }
        assert_eq!(
            email_message("a@b.c,x", true),
            "Please include an '@' in the email address. 'x' is missing an '@'."
        );
        assert_eq!(
            email_message("a@b.c,", true),
            "Please enter a non-empty email address."
        );
    }

    #[test]
    fn plural_only_singularizes_exactly_one() {
        assert_eq!(plural(1), "1 character");
        assert_eq!(plural(0), "0 characters");
        assert_eq!(plural(30), "30 characters");
    }
}
