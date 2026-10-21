//! Counter styles written by the author in `@counter-style` rules. The numbers are written
//! in them by layout. (Chrome does not take the `symbols()` function, so neither does this.)

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// A counter style an `extends` or a `fallback` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterRef {
    /// Before the rules are put together: a name.
    Named(String),
    /// One the browser has (`decimal`, `lower-roman`, ...), by its name in lower case.
    Builtin(String),
    /// One of the author's.
    Custom(Arc<CounterStyle>),
}

/// How a counter style writes a number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterSystem {
    Cyclic,
    Numeric,
    Alphabetic,
    Symbolic,
    Additive,
    /// The first symbol is the number it holds.
    Fixed(i64),
    /// Another style, with the descriptors written here on it.
    Extends(CounterRef),
}

/// The ranges of a counter style: each end is `None` when it is `infinite`.
pub type Ranges = Vec<(Option<i64>, Option<i64>)>;

/// A counter style of a `@counter-style` rule. What is `None` is
/// what the style does not state, which an `extends` takes from the style it extends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterStyle {
    /// Its name.
    pub name: String,
    pub system: CounterSystem,
    pub symbols: Vec<String>,
    /// The weight and the symbol of each, the largest weight first.
    pub additive: Vec<(i64, String)>,
    /// What goes before and after a number below zero.
    pub negative: Option<(String, String)>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    /// The numbers it writes, as `(from, to)` with `None` where there is no limit.
    pub range: Option<Ranges>,
    /// At least this many characters, filled in front with the symbol.
    pub pad: Option<(usize, String)>,
    pub fallback: Option<CounterRef>,
}

/// A name no `@counter-style` can take.
fn is_reserved(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "none"
            | "decimal"
            | "disc"
            | "circle"
            | "square"
            | "disclosure-open"
            | "disclosure-closed"
            | "inherit"
            | "initial"
            | "unset"
            | "default"
    )
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Str(String),
    Word(String),
    Comma,
    Open,
    Close,
}

fn tokens(value: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            ',' => out.push(Token::Comma),
            '(' => out.push(Token::Open),
            ')' => out.push(Token::Close),
            '"' | '\'' => {
                let mut text = String::new();
                while let Some(next) = chars.next() {
                    if next == c {
                        break;
                    }
                    if next == '\\' {
                        match chars.next() {
                            // A line break after a backslash is left out.
                            Some('\n') | None => {}
                            Some(first) if first.is_ascii_hexdigit() => {
                                let mut digits = String::from(first);
                                while digits.len() < 6
                                    && chars.peek().is_some_and(char::is_ascii_hexdigit)
                                {
                                    if let Some(digit) = chars.next() {
                                        digits.push(digit);
                                    }
                                }
                                if chars.peek().is_some_and(|c| c.is_whitespace()) {
                                    chars.next();
                                }
                                if let Some(code) = u32::from_str_radix(&digits, 16)
                                    .ok()
                                    .and_then(char::from_u32)
                                {
                                    text.push(code);
                                }
                            }
                            Some(other) => text.push(other),
                        }
                    } else {
                        text.push(next);
                    }
                }
                out.push(Token::Str(text));
            }
            _ => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || matches!(next, ',' | '(' | ')' | '"' | '\'') {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                out.push(Token::Word(word));
            }
        }
    }
    out
}

/// A symbol: a string or an identifier.
fn symbol(token: &Token) -> Option<String> {
    match token {
        Token::Str(text) => Some(text.clone()),
        Token::Word(word) if is_identifier(word) => Some(word.clone()),
        _ => None,
    }
}

fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    let first = chars.next();
    first.is_some_and(|c| c.is_alphabetic() || c == '_' || c == '-' || !c.is_ascii())
        && !word.starts_with("-\"")
        && word != "-"
        && word
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || !c.is_ascii())
}

fn integer(token: &Token) -> Option<i64> {
    match token {
        Token::Word(word) => word.parse::<i64>().ok(),
        _ => None,
    }
}

/// The symbols of a descriptor that lists them.
fn symbols_of(tokens: &[Token]) -> Option<Vec<String>> {
    let list: Option<Vec<String>> = tokens.iter().map(symbol).collect();
    list.filter(|list| !list.is_empty())
}

fn single_symbol(tokens: &[Token]) -> Option<String> {
    match tokens {
        [one] => symbol(one),
        _ => None,
    }
}

fn blank(name: &str) -> CounterStyle {
    CounterStyle {
        name: name.to_owned(),
        system: CounterSystem::Symbolic,
        symbols: Vec::new(),
        additive: Vec::new(),
        negative: None,
        prefix: None,
        suffix: None,
        range: None,
        pad: None,
        fallback: None,
    }
}

fn parse_system(tokens: &[Token]) -> Option<CounterSystem> {
    let first = match tokens.first()? {
        Token::Word(word) => word.to_ascii_lowercase(),
        _ => return None,
    };
    let rest = &tokens[1..];
    Some(match (first.as_str(), rest) {
        ("cyclic", []) => CounterSystem::Cyclic,
        ("numeric", []) => CounterSystem::Numeric,
        ("alphabetic", []) => CounterSystem::Alphabetic,
        ("symbolic", []) => CounterSystem::Symbolic,
        ("additive", []) => CounterSystem::Additive,
        ("fixed", []) => CounterSystem::Fixed(1),
        ("fixed", [number]) => CounterSystem::Fixed(integer(number)?),
        ("extends", [Token::Word(name)]) if is_identifier(name) && !is_reserved_none(name) => {
            CounterSystem::Extends(CounterRef::Named(name.clone()))
        }
        _ => return None,
    })
}

fn is_reserved_none(name: &str) -> bool {
    name.eq_ignore_ascii_case("none")
}

fn parse_range(tokens: &[Token]) -> Option<Option<Ranges>> {
    if let [Token::Word(word)] = tokens
        && word.eq_ignore_ascii_case("auto")
    {
        return Some(None);
    }
    let bound = |token: &Token| -> Option<Option<i64>> {
        match token {
            Token::Word(word) if word.eq_ignore_ascii_case("infinite") => Some(None),
            other => integer(other).map(Some),
        }
    };
    let mut ranges = Vec::new();
    for part in tokens.split(|token| *token == Token::Comma) {
        let [from, to] = part else {
            return None;
        };
        let (from, to) = (bound(from)?, bound(to)?);
        if let (Some(a), Some(b)) = (from, to)
            && a > b
        {
            return None;
        }
        ranges.push((from, to));
    }
    Some(Some(ranges))
}

fn parse_pad(tokens: &[Token]) -> Option<(usize, String)> {
    let [a, b] = tokens else {
        return None;
    };
    let (count, pad) = match (integer(a), integer(b)) {
        (Some(count), None) => (count, symbol(b)?),
        (None, Some(count)) => (count, symbol(a)?),
        _ => return None,
    };
    usize::try_from(count).ok().map(|count| (count, pad))
}

fn parse_additive(tokens: &[Token]) -> Option<Vec<(i64, String)>> {
    let mut out: Vec<(i64, String)> = Vec::new();
    for part in tokens.split(|token| *token == Token::Comma) {
        let [a, b] = part else {
            return None;
        };
        let (weight, text) = match (integer(a), integer(b)) {
            (Some(weight), None) => (weight, symbol(b)?),
            (None, Some(weight)) => (weight, symbol(a)?),
            _ => return None,
        };
        if weight < 0 || out.last().is_some_and(|(previous, _)| *previous <= weight) {
            return None;
        }
        out.push((weight, text));
    }
    (!out.is_empty()).then_some(out)
}

/// Whether the descriptors that a system needs are there.
fn is_valid(style: &CounterStyle) -> bool {
    match &style.system {
        CounterSystem::Cyclic | CounterSystem::Symbolic | CounterSystem::Fixed(_) => {
            !style.symbols.is_empty()
        }
        CounterSystem::Numeric | CounterSystem::Alphabetic => style.symbols.len() >= 2,
        CounterSystem::Additive => !style.additive.is_empty(),
        CounterSystem::Extends(_) => true,
    }
}

/// The counter style a `@counter-style` rule `name` with the declarations `body` makes, if
/// it makes one.
fn parse_rule(name: &str, body: &str) -> Option<CounterStyle> {
    if !is_identifier(name) || is_reserved(name) {
        return None;
    }
    let mut style = blank(name);
    for declaration in split_declarations(body) {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value.trim().trim_end_matches("!important").trim();
        let list = tokens(value);
        match property.as_str() {
            "system" => {
                if let Some(system) = parse_system(&list) {
                    style.system = system;
                }
            }
            "symbols" => {
                if let Some(symbols) = symbols_of(&list) {
                    style.symbols = symbols;
                }
            }
            "additive-symbols" => {
                if let Some(additive) = parse_additive(&list) {
                    style.additive = additive;
                }
            }
            "negative" => {
                style.negative = match list.as_slice() {
                    [one] => symbol(one).map(|before| (before, String::new())),
                    [one, two] => symbol(one).zip(symbol(two)),
                    _ => style.negative,
                };
            }
            "prefix" => {
                if let Some(text) = single_symbol(&list) {
                    style.prefix = Some(text);
                }
            }
            "suffix" => {
                if let Some(text) = single_symbol(&list) {
                    style.suffix = Some(text);
                }
            }
            "range" => {
                if let Some(range) = parse_range(&list) {
                    style.range = range;
                }
            }
            "pad" => {
                if let Some(pad) = parse_pad(&list) {
                    style.pad = Some(pad);
                }
            }
            "fallback" => {
                if let [Token::Word(name)] = list.as_slice()
                    && is_identifier(name)
                    && !is_reserved_none(name)
                {
                    style.fallback = Some(CounterRef::Named(name.clone()));
                }
            }
            _ => {}
        }
    }
    is_valid(&style).then_some(style)
}

/// The declarations of a block, apart at the semicolons that are not in a string or in
/// parentheses.
fn split_declarations(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut depth = 0usize;
    let mut escaped = false;
    let mut start = 0;
    for (index, c) in body.char_indices() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => {
                out.push(&body[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&body[start..]);
    out
}

/// Takes every `@counter-style` rule out of `css` and returns what is left, with the counter
/// styles the rules made, as they were written (their `extends` and `fallback` are names).
pub fn extract_rules(css: &str) -> (std::borrow::Cow<'_, str>, Vec<CounterStyle>) {
    const AT: &str = "@counter-style";
    if !css.contains(AT) && !css.to_ascii_lowercase().contains(AT) {
        return (std::borrow::Cow::Borrowed(css), Vec::new());
    }
    let lower = css.to_ascii_lowercase();
    let mut left = String::with_capacity(css.len());
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(relative) = lower[at..].find(AT) {
        let begin = at + relative;
        // Not in a comment or a string, nor in the block of an at-rule (a condition that is not
        // evaluated here): the cheap check is that the text before it on its
        // line is no unfinished comment or quote.
        if in_comment_or_string(&css[..begin]) || inside_block(&css[..begin]) {
            left.push_str(&css[at..begin + AT.len()]);
            at = begin + AT.len();
            continue;
        }
        left.push_str(&css[at..begin]);
        let Some(open) = css[begin..].find('{').map(|i| begin + i) else {
            at = css.len();
            break;
        };
        let name = css[begin + AT.len()..open].trim();
        // The matching closing brace.
        let mut depth = 0usize;
        let mut quote: Option<char> = None;
        let mut end = css.len();
        for (index, c) in css[open..].char_indices() {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '"' | '\'' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + index + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = &css[open + 1..end.saturating_sub(1).max(open + 1)];
        if let Some(style) = parse_rule(name, body) {
            found.push(style);
        }
        at = end;
    }
    left.push_str(&css[at.min(css.len())..]);
    (std::borrow::Cow::Owned(left), found)
}

fn in_comment_or_string(before: &str) -> bool {
    let mut in_comment = false;
    let mut quote: Option<char> = None;
    let mut chars = before.chars().peekable();
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_comment = false;
            }
        } else if let Some(open) = quote {
            if c == '\\' {
                chars.next();
            } else if c == open {
                quote = None;
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_comment = true;
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        }
    }
    in_comment || quote.is_some()
}

/// Whether `before` leaves a `{` unclosed, outside comments and strings.
fn inside_block(before: &str) -> bool {
    let mut depth = 0usize;
    let mut in_comment = false;
    let mut quote: Option<char> = None;
    let mut chars = before.chars().peekable();
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_comment = false;
            }
        } else if let Some(open) = quote {
            if c == '\\' {
                chars.next();
            } else if c == open {
                quote = None;
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_comment = true;
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth = depth.saturating_sub(1);
        }
    }
    depth > 0
}

/// The counter styles of a set of rules, by name; one written later takes the place of one
/// written earlier.
#[derive(Debug, Default)]
pub struct CounterStyles {
    by_name: HashMap<String, Arc<CounterStyle>>,
}

impl CounterStyles {
    /// Puts together the styles the rules made, with every name an `extends` or a `fallback`
    /// has that a rule defines turned into that style.
    pub fn from_declared(declared: &[CounterStyle]) -> Self {
        let mut by_name: HashMap<&str, &CounterStyle> = HashMap::new();
        for style in declared {
            by_name.insert(style.name.as_str(), style);
        }
        let built = by_name
            .keys()
            .filter_map(|&name| {
                build(name, &by_name, &mut Vec::new()).map(|style| (name.to_owned(), style))
            })
            .collect();
        CounterStyles { by_name: built }
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<Arc<CounterStyle>> {
        self.by_name.get(name).cloned()
    }
}

/// `name` with the styles it extends and falls back to built in. A loop of styles ends in
/// `decimal` where it closes, and each name is built from its own start, so the order of the
/// rules never changes what a style is.
fn build(
    name: &str,
    declared: &HashMap<&str, &CounterStyle>,
    path: &mut Vec<String>,
) -> Option<Arc<CounterStyle>> {
    let style = declared.get(name)?;
    path.push(name.to_owned());
    let mut out = (*style).clone();
    if let CounterSystem::Extends(CounterRef::Named(other)) = &style.system {
        out.system = CounterSystem::Extends(target(other, declared, path));
    }
    if let Some(CounterRef::Named(other)) = &style.fallback {
        out.fallback = Some(target(other, declared, path));
    }
    path.pop();
    Some(Arc::new(out))
}

/// The style `other` names: `decimal` in a loop, else the rule of that name, else the style
/// of the browser that has it.
fn target(
    other: &str,
    declared: &HashMap<&str, &CounterStyle>,
    path: &mut Vec<String>,
) -> CounterRef {
    if path.iter().any(|name| name == other) {
        return CounterRef::Builtin("decimal".to_owned());
    }
    match build(other, declared, path) {
        Some(custom) => CounterRef::Custom(custom),
        None => CounterRef::Builtin(other.to_ascii_lowercase()),
    }
}

/// The counter styles the rules of a cascade make together, if any do.
pub(crate) fn collect(rules: &[crate::stylesheet_parse::Rule]) -> Option<Arc<CounterStyles>> {
    if rules.iter().all(|rule| rule.counter_styles().is_empty()) {
        return None;
    }
    let declared: Vec<CounterStyle> = rules
        .iter()
        .flat_map(|rule| rule.counter_styles().iter().cloned())
        .collect();
    Some(Arc::new(CounterStyles::from_declared(&declared)))
}

thread_local! {
    /// The counter styles of the cascade being made on this thread.
    static ACTIVE: RefCell<Option<Arc<CounterStyles>>> = const { RefCell::new(None) };
}

/// Makes `styles` the counter styles [`lookup`] finds, until it is dropped.
pub struct Installed(Option<Arc<CounterStyles>>);

pub fn install(styles: Option<Arc<CounterStyles>>) -> Installed {
    Installed(ACTIVE.with(|active| active.replace(styles)))
}

impl Drop for Installed {
    fn drop(&mut self) {
        ACTIVE.with(|active| *active.borrow_mut() = self.0.take());
    }
}

/// The counter style of the author called `name`, while a cascade runs.
pub fn lookup(name: &str) -> Option<Arc<CounterStyle>> {
    ACTIVE.with(|active| active.borrow().as_ref().and_then(|styles| styles.get(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_counter_style_rule_is_taken_out_of_the_css_and_read() {
        let css = "a { color: red } @counter-style stars { system: cyclic; symbols: \"*\" \"x\"; suffix: \" \"; } b { margin: 0 }";
        let (left, found) = extract_rules(css);
        assert!(
            !left.contains("counter-style")
                && left.contains("a { color: red }")
                && left.contains("b { margin: 0 }")
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "stars");
        assert_eq!(found[0].system, CounterSystem::Cyclic);
        assert_eq!(found[0].symbols, vec!["*".to_owned(), "x".to_owned()]);
        assert_eq!(found[0].suffix.as_deref(), Some(" "));
    }

    #[test]
    fn a_rule_that_lacks_what_its_system_needs_is_dropped() {
        let (_, found) = extract_rules(
            "@counter-style a { system: numeric; symbols: \"0\"; } @counter-style b { system: additive; } @counter-style c { symbols: x }",
        );
        assert_eq!(
            found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["c"]
        );
    }

    #[test]
    fn a_name_that_a_style_cannot_have_is_dropped_and_the_names_of_the_others_are_kept() {
        let (_, found) = extract_rules(
            "@counter-style decimal { symbols: x } @counter-style none { symbols: x } @counter-style Mine { symbols: x }",
        );
        assert_eq!(
            found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["Mine"]
        );
    }

    #[test]
    fn a_rule_in_the_block_of_another_at_rule_is_left_alone() {
        let css = "@media (min-width: 900px) { @counter-style a { symbols: x; } } \
                   @counter-style b { symbols: y; }";
        let (left, found) = extract_rules(css);
        assert_eq!(
            found.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["b"]
        );
        assert!(left.contains("@media") && left.contains("@counter-style a"));
    }
    #[test]
    fn extends_and_fallback_are_turned_into_the_styles_they_name() {
        let (_, found) = extract_rules(
            "@counter-style a { system: extends b; prefix: \"<\" } @counter-style b { symbols: x y; fallback: a } @counter-style c { system: extends lower-roman }",
        );
        let styles = CounterStyles::from_declared(&found);
        let a = styles.get("a").unwrap();
        assert!(
            matches!(&a.system, CounterSystem::Extends(CounterRef::Custom(b)) if b.name == "b")
        );
        let c = styles.get("c").unwrap();
        assert_eq!(
            c.system,
            CounterSystem::Extends(CounterRef::Builtin("lower-roman".to_owned()))
        );
    }

    #[test]
    fn a_loop_of_styles_is_the_same_whichever_rule_comes_first() {
        let rules = [
            "@counter-style a { system: extends b; prefix: \"<\" }",
            "@counter-style b { symbols: x y; fallback: a }",
        ];
        for order in [[0, 1], [1, 0]] {
            let css = format!("{} {}", rules[order[0]], rules[order[1]]);
            let (_, found) = extract_rules(&css);
            let styles = CounterStyles::from_declared(&found);
            let a = styles.get("a").unwrap();
            assert!(
                matches!(&a.system, CounterSystem::Extends(CounterRef::Custom(b)) if b.name == "b"),
                "{css}"
            );
            let b = styles.get("b").unwrap();
            assert!(matches!(&b.fallback, Some(CounterRef::Custom(a)) if a.name == "a"));
        }
        let (_, found) = extract_rules(
            "@counter-style p { system: extends q } @counter-style q { system: extends p }",
        );
        let styles = CounterStyles::from_declared(&found);
        for name in ["p", "q"] {
            let CounterSystem::Extends(CounterRef::Custom(next)) =
                &styles.get(name).unwrap().system
            else {
                panic!("{name} extends the other");
            };
            assert_eq!(
                next.system,
                CounterSystem::Extends(CounterRef::Builtin("decimal".to_owned()))
            );
        }
    }

    #[test]
    fn the_descriptors_that_state_ranges_pads_and_signs_are_read() {
        let (_, found) = extract_rules(
            "@counter-style s { system: numeric; symbols: \"0\" \"1\"; range: 0 9, 20 infinite; pad: 3 \"_\"; negative: \"(\" \")\"; }",
        );
        let s = &found[0];
        assert_eq!(s.range, Some(vec![(Some(0), Some(9)), (Some(20), None)]));
        assert_eq!(s.pad, Some((3, "_".to_owned())));
        assert_eq!(s.negative, Some(("(".to_owned(), ")".to_owned())));
    }
}
