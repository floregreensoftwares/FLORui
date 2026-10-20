//! Counter styles of the author, written as CSS Counter Styles 3 says: the number a style
//! writes in its symbols, within its range, padded, signed, or else in its fallback.

use florui_style::{CounterRef, CounterStyle, CounterSystem};

use crate::counter_style::{Counted, format};

/// How far styles that extend styles that extend styles are followed.
const DEPTH: usize = 16;

fn decimal(value: i64) -> String {
    value.to_string()
}

/// `value` in the counter style `style` of the author, with its prefix and suffix; a number
/// the style does not write is written in its fallback.
pub(crate) fn format_custom(style: &CounterStyle, value: i64) -> Counted {
    let suffix = descriptor(style, |s| s.suffix.clone()).unwrap_or_else(|| ". ".to_owned());
    let prefix = descriptor(style, |s| s.prefix.clone()).unwrap_or_default();
    Counted {
        text: format!("{prefix}{}", represent(style, value, 0)),
        suffix,
    }
}

/// The descriptor `get` reads, of `style` or else of the style it extends.
fn descriptor<T>(
    style: &CounterStyle,
    get: impl Fn(&CounterStyle) -> Option<T> + Copy,
) -> Option<T> {
    if let Some(own) = get(style) {
        return Some(own);
    }
    match &style.system {
        CounterSystem::Extends(CounterRef::Custom(base)) => descriptor(base, get),
        _ => None,
    }
}

/// What `style` writes for `value`, apart from its prefix and suffix.
fn represent(style: &CounterStyle, value: i64, depth: usize) -> String {
    if depth > DEPTH {
        return decimal(value);
    }
    let root = root_of(style);
    if let CounterSystem::Extends(CounterRef::Builtin(name)) = &root.system {
        // The style extends one the browser has: what it writes, and the fallback if the
        // range that the style states leaves the number out.
        if in_range(style, value, None) {
            return format(name, value).text;
        }
        return fallback(style, value, depth);
    }
    if !in_range(style, value, Some(root)) {
        return fallback(style, value, depth);
    }
    let negative = descriptor(style, |s| s.negative.clone())
        .unwrap_or_else(|| ("-".to_owned(), String::new()));
    let signed = value < 0
        && matches!(
            root.system,
            CounterSystem::Numeric
                | CounterSystem::Alphabetic
                | CounterSystem::Symbolic
                | CounterSystem::Additive
        );
    let magnitude = if signed { -value } else { value };
    let Some(mut text) = algorithm(root, magnitude) else {
        return fallback(style, value, depth);
    };
    if let Some((count, filler)) = descriptor(style, |s| s.pad.clone()) {
        let sign = if signed {
            negative.0.chars().count() + negative.1.chars().count()
        } else {
            0
        };
        let length = text.chars().count() + sign;
        for _ in length..count {
            text.insert_str(0, &filler);
        }
    }
    if signed {
        text = format!("{}{}{}", negative.0, text, negative.1);
    }
    text
}

/// The style that holds the system `style` counts in, through the styles it extends.
fn root_of(style: &CounterStyle) -> &CounterStyle {
    let mut current = style;
    for _ in 0..DEPTH {
        match &current.system {
            CounterSystem::Extends(CounterRef::Custom(base)) => current = base,
            _ => return current,
        }
    }
    current
}

/// Whether `value` is in the range of `style`: what it states, else what its system takes.
fn in_range(style: &CounterStyle, value: i64, root: Option<&CounterStyle>) -> bool {
    if let Some(ranges) = descriptor(style, |s| s.range.clone()) {
        return ranges.iter().any(|(from, to)| {
            from.is_none_or(|from| value >= from) && to.is_none_or(|to| value <= to)
        });
    }
    match root.map(|root| &root.system) {
        Some(CounterSystem::Alphabetic | CounterSystem::Symbolic) => value >= 1,
        Some(CounterSystem::Additive) => value >= 0,
        _ => true,
    }
}

/// The number written by the system of `root` in its symbols, if it can.
fn algorithm(root: &CounterStyle, value: i64) -> Option<String> {
    let symbols = &root.symbols;
    let count = symbols.len() as i64;
    match &root.system {
        CounterSystem::Cyclic => {
            let at = (value - 1).rem_euclid(count.max(1)) as usize;
            symbols.get(at).cloned()
        }
        CounterSystem::Fixed(first) => usize::try_from(value - first)
            .ok()
            .and_then(|at| symbols.get(at).cloned()),
        CounterSystem::Symbolic => {
            if value < 1 || count < 1 {
                return None;
            }
            let at = ((value - 1) % count) as usize;
            let times = (value - 1) / count + 1;
            Some(symbols[at].repeat(times as usize))
        }
        CounterSystem::Alphabetic => {
            if value < 1 || count < 2 {
                return None;
            }
            let mut n = value;
            let mut out: Vec<&str> = Vec::new();
            while n > 0 {
                n -= 1;
                out.push(symbols[(n % count) as usize].as_str());
                n /= count;
            }
            Some(out.iter().rev().copied().collect())
        }
        CounterSystem::Numeric => {
            if count < 2 {
                return None;
            }
            if value == 0 {
                return symbols.first().cloned();
            }
            let mut n = value.unsigned_abs();
            let mut out: Vec<&str> = Vec::new();
            while n > 0 {
                out.push(symbols[(n % count as u64) as usize].as_str());
                n /= count as u64;
            }
            Some(out.iter().rev().copied().collect())
        }
        CounterSystem::Additive => {
            if value == 0 {
                return root
                    .additive
                    .iter()
                    .find(|(weight, _)| *weight == 0)
                    .map(|(_, symbol)| symbol.clone());
            }
            let mut left = value;
            let mut out = String::new();
            for (weight, symbol) in root.additive.iter().filter(|(weight, _)| *weight > 0) {
                let times = left / weight;
                for _ in 0..times {
                    out.push_str(symbol);
                }
                left -= times * weight;
                if left == 0 {
                    break;
                }
            }
            (left == 0).then_some(out)
        }
        CounterSystem::Extends(_) => None,
    }
}

/// What the fallback of `style` writes for `value`: `decimal`, unless it says another.
fn fallback(style: &CounterStyle, value: i64, depth: usize) -> String {
    match descriptor(style, |s| s.fallback.clone()) {
        Some(CounterRef::Custom(other)) => represent(&other, value, depth + 1),
        Some(CounterRef::Builtin(name)) => format(&name, value).text,
        _ => decimal(value),
    }
}

#[cfg(test)]
mod tests {
    use florui_style::CounterStyles;

    use super::*;

    fn style(css: &str, name: &str) -> std::sync::Arc<CounterStyle> {
        let (_, declared) = florui_style::extract_counter_styles(css);
        CounterStyles::from_declared(&declared)
            .get(name)
            .expect("the rule is valid")
    }

    fn text(style: &CounterStyle, value: i64) -> String {
        let counted = format_custom(style, value);
        format!("{}{}", counted.text, counted.suffix)
    }

    #[test]
    fn the_systems_write_numbers_as_the_spec_says() {
        let cyclic = style(
            "@counter-style c { system: cyclic; symbols: A B C; suffix: \"\" }",
            "c",
        );
        assert_eq!(text(&cyclic, 1), "A");
        assert_eq!(text(&cyclic, 4), "A");
        assert_eq!(text(&cyclic, 0), "C");
        assert_eq!(text(&cyclic, -1), "B");
        let symbolic = style(
            "@counter-style s { system: symbolic; symbols: \"*\" \"⁑\"; suffix: \"\" }",
            "s",
        );
        assert_eq!(text(&symbolic, 1), "*");
        assert_eq!(text(&symbolic, 2), "⁑");
        assert_eq!(text(&symbolic, 3), "**");
        assert_eq!(text(&symbolic, 5), "***");
        // Not in 1.. : the fallback, decimal.
        assert_eq!(text(&symbolic, 0), "0");
        let alphabetic = style(
            "@counter-style a { system: alphabetic; symbols: x y z; suffix: \"\" }",
            "a",
        );
        assert_eq!(text(&alphabetic, 3), "z");
        assert_eq!(text(&alphabetic, 4), "xx");
        assert_eq!(text(&alphabetic, 13), "xxx");
        let numeric = style(
            "@counter-style n { system: numeric; symbols: a b c; suffix: \"\" }",
            "n",
        );
        assert_eq!(text(&numeric, 0), "a");
        assert_eq!(text(&numeric, 5), "bc");
        assert_eq!(text(&numeric, -5), "-bc");
        let fixed = style(
            "@counter-style f { system: fixed 3; symbols: x y; suffix: \"\" }",
            "f",
        );
        assert_eq!(text(&fixed, 3), "x");
        assert_eq!(text(&fixed, 4), "y");
        assert_eq!(text(&fixed, 5), "5");
        assert_eq!(text(&fixed, 2), "2");
        let additive = style(
            "@counter-style d { system: additive; additive-symbols: 10 X, 5 V, 1 I; suffix: \"\" }",
            "d",
        );
        assert_eq!(text(&additive, 14), "XIIII");
        assert_eq!(text(&additive, 0), "0");
        // Its range starts at 0: a number below it is in the fallback.
        assert_eq!(text(&additive, -4), "-4");
        let signed = style(
            "@counter-style d { system: additive; additive-symbols: 10 X, 5 V, 1 I; range: infinite infinite; suffix: \"\" }",
            "d",
        );
        assert_eq!(text(&signed, -4), "-IIII");
    }

    #[test]
    fn prefix_suffix_negative_pad_range_and_fallback_are_the_styles() {
        let style = style(
            "@counter-style p { system: numeric; symbols: \"0\" \"1\"; prefix: \"<\"; suffix: \">\"; \
             negative: \"(\" \")\"; pad: 5 \"_\"; range: -9 20; fallback: upper-roman }",
            "p",
        );
        assert_eq!(text(&style, 5), "<__101>");
        assert_eq!(text(&style, -2), "<(_10)>");
        // Out of the range: in the fallback, with the prefix and suffix of the style.
        assert_eq!(text(&style, 21), "<XXI>");
    }

    #[test]
    fn a_style_that_extends_another_takes_what_it_does_not_state() {
        let css = "@counter-style base { system: cyclic; symbols: a b; suffix: \"!\" } \
                   @counter-style more { system: extends base; prefix: \"[\" }";
        let more = style(css, "more");
        assert_eq!(text(&more, 1), "[a!");
        assert_eq!(text(&more, 2), "[b!");
        let roman = style(
            "@counter-style r { system: extends upper-roman; suffix: \")\" }",
            "r",
        );
        assert_eq!(text(&roman, 14), "XIV)");
    }
}
