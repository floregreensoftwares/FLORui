//! What the properties of lists and counters resolve to: how a list item is
//! marked, and the counters an element resets, increments and sets.

/// `list-style-type`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ListStyleType {
    /// No marker.
    None,
    #[default]
    Disc,
    Circle,
    Square,
    DisclosureOpen,
    DisclosureClosed,
    /// A counter style by its name in lower case (`decimal`, `lower-roman`, ...),
    /// which layout knows how to count in. A name nothing counts in is `decimal`.
    Counter(String),
    /// A string, which is the marker as it is.
    String(String),
    /// A counter style of the author (a `@counter-style` rule).
    Custom(std::sync::Arc<crate::counter_style::CounterStyle>),
}

/// `list-style-position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListStylePosition {
    /// Outside the box of the list item, before it.
    #[default]
    Outside,
    /// Inline, at the start of its first line.
    Inside,
}

/// Reads the value of `list-style-type` as it is written, with no counter style of the author.
pub fn parse_list_style_type(value: &str) -> ListStyleType {
    parse_list_style_type_with(value, &|_| None)
}

/// [`parse_list_style_type`], where `lookup` finds the counter styles of the author by name.
pub fn parse_list_style_type_with(
    value: &str,
    lookup: &dyn Fn(&str) -> Option<std::sync::Arc<crate::counter_style::CounterStyle>>,
) -> ListStyleType {
    let value = value.trim();
    if let Some(string) = unquote(value) {
        return ListStyleType::String(string);
    }
    match value.to_ascii_lowercase().as_str() {
        "none" => ListStyleType::None,
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "disclosure-open" => ListStyleType::DisclosureOpen,
        "disclosure-closed" => ListStyleType::DisclosureClosed,
        _ if !value.is_empty() && value.chars().all(is_ident_char) && lookup(value).is_some() => {
            lookup(value).map_or_else(ListStyleType::default, ListStyleType::Custom)
        }
        name if !name.is_empty() && name.chars().all(is_ident_char) => {
            ListStyleType::Counter(name.to_owned())
        }
        _ => ListStyleType::default(),
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// The contents of a string written in quotes, with its escapes read.
fn unquote(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let quote = chars.next().filter(|c| matches!(c, '"' | '\''))?;
    let mut out = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Reads `counter-reset`, `counter-increment` or `counter-set`: names, each with
/// an integer that is `default` when it is missing. `none` is no counter.
pub fn parse_counters(value: &str, default: i32) -> Vec<(String, i32)> {
    let mut counters: Vec<(String, i32)> = Vec::new();
    let mut words = value.split_whitespace().peekable();
    while let Some(name) = words.next() {
        if name.eq_ignore_ascii_case("none") || !name.chars().all(is_ident_char) {
            continue;
        }
        let amount = words
            .next_if(|next| next.parse::<i32>().is_ok())
            .and_then(|next| next.parse::<i32>().ok())
            .unwrap_or(default);
        counters.push((name.to_owned(), amount));
    }
    counters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_style_type_is_a_keyword_a_counter_style_or_a_string() {
        assert_eq!(parse_list_style_type("disc"), ListStyleType::Disc);
        assert_eq!(parse_list_style_type(" Circle "), ListStyleType::Circle);
        assert_eq!(
            parse_list_style_type("lower-roman"),
            ListStyleType::Counter("lower-roman".to_owned())
        );
        assert_eq!(
            parse_list_style_type("\"-> \""),
            ListStyleType::String("-> ".to_owned())
        );
        assert_eq!(parse_list_style_type("none"), ListStyleType::None);
    }

    #[test]
    fn counters_have_the_amount_they_are_given_or_the_default() {
        assert_eq!(
            parse_counters("a b 3 c -2", 0),
            vec![
                ("a".to_owned(), 0),
                ("b".to_owned(), 3),
                ("c".to_owned(), -2)
            ]
        );
        assert_eq!(parse_counters("item", 1), vec![("item".to_owned(), 1)]);
        assert!(parse_counters("none", 1).is_empty());
    }
}
