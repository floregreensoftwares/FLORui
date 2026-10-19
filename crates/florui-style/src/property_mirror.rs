//! Properties the style engine's servo build does not know, which would make
//! a declaration invalid and be dropped, are carried instead as internal
//! custom properties of the same value, `--florui-<name>`, written in their
//! place before the engine parses the text. Authors keep writing the real
//! property; [`crate::stylo`] reads the custom property back.
//!
//! Two shorthands the engine only half knows are expanded here as well:
//! `font-variant` into `font-variant-caps` (which it has) and the mirrored
//! ligatures, numeric, east-asian and position properties, and
//! `text-decoration` into line, style and color (which it has) and the
//! mirrored thickness.
//!
//! A custom property inherits, and `text-decoration-thickness` does not: a
//! `text-decoration` shorthand always sets it, to `auto` when it names none, as a
//! browser's own shorthand resets it, and the style reader takes it only from
//! the rules of the element itself.
//!
//! Uses `cssparser`, so comments, strings and selectors that merely contain a
//! name are never touched.

use std::borrow::Cow;

use cssparser::{Parser, ParserInput, Token};

/// The properties carried as `--florui-<name>` as written.
pub(crate) const MIRRORED: &[&str] = &[
    "font-feature-settings",
    "font-variant-caps",
    "font-variation-settings",
    "font-kerning",
    "font-variant-ligatures",
    "font-variant-numeric",
    "font-variant-east-asian",
    "font-variant-position",
    "font-optical-sizing",
    "font-synthesis",
    "font-synthesis-style",
    "font-synthesis-small-caps",
    "font-synthesis-position",
    "text-decoration-thickness",
    "text-underline-offset",
    "text-underline-position",
    "text-decoration-skip-ink",
    "clip-path",
    "mask-mode",
    "mask-repeat",
    "mask-position",
    "mask-size",
    "mask-origin",
    "mask-clip",
    "mask-composite",
    "background-blend-mode",
    "background-clip",
    "transform-box",
    "mask-box-image-source",
    "mask-box-image-slice",
    "mask-box-image-width",
    "mask-box-image-outset",
    "mask-box-image-repeat",
    "filter-urls",
    "backdrop-filter-urls",
    "marker-end",
    "marker-mid",
    "marker-start",
    "vector-effect",
    "paint-order",
    "mask-type",
    "shape-rendering",
    "color-interpolation-filters",
    "color-interpolation",
    "clip-rule",
    "lighting-color",
    "flood-opacity",
    "flood-color",
    "stop-opacity",
    "stop-color",
    "stroke-dashoffset",
    "stroke-dasharray",
    "stroke-miterlimit",
    "stroke-linejoin",
    "stroke-linecap",
    "stroke-opacity",
    "stroke-width",
    "stroke",
    "fill-rule",
    "fill-opacity",
    "fill",
];

/// The properties an SVG is drawn by that the engine's servo build lacks, kept
/// (as `--florui-<name>`) so that CSS can style the inline SVGs that masks, clips
/// and filters are made of.
pub(crate) const SVG_PROPERTIES: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stop-color",
    "stop-opacity",
    "flood-color",
    "flood-opacity",
    "lighting-color",
    "clip-rule",
    "color-interpolation",
    "color-interpolation-filters",
    "shape-rendering",
    "mask-type",
    "paint-order",
    "vector-effect",
    "marker-start",
    "marker-mid",
    "marker-end",
];
const GLOBAL_KEYWORDS: [&str; 5] = ["inherit", "initial", "unset", "revert", "revert-layer"];

/// The custom property standing for `name`.
pub(crate) fn mirror_name(name: &str) -> String {
    format!("--florui-{name}")
}

struct Replacement {
    span: (usize, usize),
    text: String,
}

/// Rewrites a stylesheet's declarations (those inside `{...}` blocks).
/// Returns `css` borrowed and unchanged when it names none of the properties.
pub(crate) fn rewrite_stylesheet(css: &str) -> Cow<'_, str> {
    rewrite(css, false)
}

/// The same for a bare declaration list, such as a `style` attribute.
pub(crate) fn rewrite_declarations(css: &str) -> Cow<'_, str> {
    rewrite(css, true)
}

fn mentions(css: &str) -> bool {
    let lower = css.to_ascii_lowercase();
    lower.contains("font-variant")
        || lower.contains("text-decoration")
        || lower.contains("mask")
        || lower.contains("background")
        || lower.contains("filter")
        || MIRRORED.iter().any(|name| lower.contains(name))
}

fn rewrite(css: &str, is_declaration_list: bool) -> Cow<'_, str> {
    if !mentions(css) {
        return Cow::Borrowed(css);
    }
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut replacements = Vec::new();
    scan(css, &mut parser, is_declaration_list, &mut replacements);
    if replacements.is_empty() {
        return Cow::Borrowed(css);
    }
    let mut out = String::with_capacity(css.len() + 64 * replacements.len());
    let mut cursor = 0;
    for replacement in replacements {
        out.push_str(&css[cursor..replacement.span.0]);
        out.push_str(&replacement.text);
        cursor = replacement.span.1;
    }
    out.push_str(&css[cursor..]);
    Cow::Owned(out)
}

fn scan<'i>(
    css: &str,
    parser: &mut Parser<'i, '_>,
    declarations_here: bool,
    replacements: &mut Vec<Replacement>,
) {
    let mut at_declaration_start = declarations_here;
    loop {
        let start = parser.position().byte_index();
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => break,
        };
        match token {
            Token::WhiteSpace(_) | Token::Comment(_) => continue,
            Token::Semicolon => at_declaration_start = declarations_here,
            Token::Ident(ref name) if at_declaration_start && is_handled(name) => {
                let lower = name.to_ascii_lowercase();
                if let Some(replacement) = read_declaration(css, parser, start, &lower) {
                    replacements.push(replacement);
                }
                at_declaration_start = false;
            }
            Token::CurlyBracketBlock => {
                let _: Result<(), cssparser::ParseError<'_, ()>> =
                    parser.parse_nested_block(|inner| {
                        scan(css, inner, true, replacements);
                        Ok(())
                    });
                at_declaration_start = declarations_here;
            }
            _ => at_declaration_start = false,
        }
    }
}

/// `mask` (a shorthand the engine's servo build lacks) and the `-webkit-` forms
/// of the mask properties, which Chrome still accepts.
fn is_mask_name(name: &str) -> bool {
    [
        "mask",
        "-webkit-mask",
        "-webkit-mask-image",
        "-webkit-mask-size",
        "-webkit-mask-position",
        "-webkit-mask-repeat",
        "-webkit-mask-origin",
        "-webkit-mask-clip",
        "-webkit-mask-composite",
    ]
    .iter()
    .any(|mask| name.eq_ignore_ascii_case(mask))
}

/// `-webkit-mask-box-image` and its parts.
fn is_mask_box_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "-webkit-mask-box-image"
        || lower
            .strip_prefix("-webkit-mask-box-image-")
            .is_some_and(|part| ["source", "slice", "width", "outset", "repeat"].contains(&part))
}

fn is_handled(name: &str) -> bool {
    name.eq_ignore_ascii_case("filter")
        || name.eq_ignore_ascii_case("backdrop-filter")
        || is_mask_box_image_name(name)
        || name.eq_ignore_ascii_case("background")
        || name.eq_ignore_ascii_case("-webkit-background-clip")
        || name.eq_ignore_ascii_case("font-variant")
        || name.eq_ignore_ascii_case("text-decoration")
        || is_mask_name(name)
        || MIRRORED
            .iter()
            .any(|mirrored| name.eq_ignore_ascii_case(mirrored))
}

/// After the property name: `: <value> [!important]`, ending at `;`, the end
/// of the block or nothing. On failure restores the parser and returns `None`,
/// leaving the declaration for the engine to drop or keep.
fn read_declaration(
    css: &str,
    parser: &mut Parser<'_, '_>,
    name_start: usize,
    name: &str,
) -> Option<Replacement> {
    let state = parser.state();
    let attempt: Result<Replacement, cssparser::ParseError<'_, ()>> = parser.try_parse(|p| {
        p.expect_colon()?;
        p.skip_whitespace();
        let value_start = p.position().byte_index();
        let mut value_end = value_start;
        // Where the value ends, and where the declaration does (after `!important`).
        let mut span_end = value_start;
        let mut important = false;
        loop {
            let before = p.state();
            let token = match p.next_including_whitespace_and_comments() {
                Ok(token) => token.clone(),
                Err(_) => break,
            };
            match token {
                Token::Semicolon => {
                    p.reset(&before);
                    break;
                }
                Token::Delim('!') => {
                    p.expect_ident_matching("important")?;
                    important = true;
                    span_end = p.position().byte_index();
                    let after = p.state();
                    match p.next() {
                        Ok(Token::Semicolon) | Err(_) => p.reset(&after),
                        Ok(_) => return Err(p.new_custom_error(())),
                    }
                    break;
                }
                Token::WhiteSpace(_) | Token::Comment(_) => {}
                // A function or a bracket ends only where its block does.
                Token::Function(_)
                | Token::ParenthesisBlock
                | Token::SquareBracketBlock
                | Token::CurlyBracketBlock => {
                    p.parse_nested_block(|inner| {
                        while inner.next_including_whitespace_and_comments().is_ok() {}
                        Ok::<(), cssparser::ParseError<'_, ()>>(())
                    })?;
                    value_end = p.position().byte_index();
                    span_end = value_end;
                }
                _ => {
                    value_end = p.position().byte_index();
                    span_end = value_end;
                }
            }
        }
        let value = css[value_start..value_end].trim();
        if value.is_empty() {
            return Err(p.new_custom_error(()));
        }
        let suffix = if important { " !important" } else { "" };
        let text = expand(name, value, suffix).ok_or_else(|| p.new_custom_error(()))?;
        Ok(Replacement {
            span: (name_start, span_end),
            text,
        })
    });
    match attempt {
        Ok(replacement) => Some(replacement),
        Err(_) => {
            parser.reset(&state);
            None
        }
    }
}

fn is_global(word: &str) -> bool {
    GLOBAL_KEYWORDS.iter().any(|k| word.eq_ignore_ascii_case(k))
}

/// What replaces a `name: value` declaration; `None` leaves it as written.
fn expand(name: &str, value: &str, important: &str) -> Option<String> {
    match name {
        "font-variant" => expand_font_variant(value, important),
        "text-decoration" => expand_text_decoration(value, important),
        "clip-path" => Some(expand_clip_path(value, important)),
        "background" => Some(expand_background(value, important)),
        "filter" | "backdrop-filter" => expand_filter(name, value, important),
        "transform-box" => {
            let valid = is_global(value)
                || [
                    "content-box",
                    "border-box",
                    "fill-box",
                    "stroke-box",
                    "view-box",
                ]
                .iter()
                .any(|keyword| value.eq_ignore_ascii_case(keyword));
            valid.then(|| format!("{}: {value}{important}", mirror_name(name)))
        }
        "background-clip" | "-webkit-background-clip" => expand_background_clip(value, important),
        _ if is_mask_box_image_name(name) => expand_mask_box_image(name, value, important),
        _ if is_mask_name(name) => expand_mask(name, value, important),
        "mask-mode" | "mask-repeat" | "mask-position" | "mask-size" | "mask-composite"
        | "mask-origin" | "mask-clip" => expand_mask(name, value, important),
        "background-blend-mode" => (is_global(value.trim())
            || value.to_ascii_lowercase().contains("var(")
            || crate::mask_values::parse_blend_modes(value).is_some())
        .then(|| format!("{}: {value}{important}", mirror_name(name))),
        _ => Some(format!("{}: {value}{important}", mirror_name(name))),
    }
}

/// Whether `word` is a non-negative number.
fn is_number(word: &str) -> bool {
    word.parse::<f32>().is_ok_and(|n| n >= 0.0 && n.is_finite())
        && !word.starts_with(['i', 'I', 'n', 'N'])
}

/// Whether `word` is a non-negative percentage.
fn is_percentage(word: &str) -> bool {
    word.strip_suffix('%').is_some_and(is_number)
}

/// Whether `value` is a valid value of the part of `-webkit-mask-box-image` named
/// by `part`, so that an invalid one is left for the engine to drop, as a browser
/// drops it, and the declaration before it stays.
fn is_valid_mask_box_image_part(part: &str, value: &str) -> bool {
    let value = value.trim();
    if is_global(value) {
        return true;
    }
    let all = words(value);
    let sides = |count: usize| (1..=4).contains(&count);
    match part {
        "source" => {
            let lower = value.to_ascii_lowercase();
            lower == "none"
                || lower.starts_with("url(")
                || lower.contains("gradient(")
                || lower.starts_with("image-set(")
                || lower.starts_with("-webkit-image-set(")
                || lower.starts_with("cross-fade(")
        }
        "slice" => {
            let fills = all
                .iter()
                .filter(|w| w.eq_ignore_ascii_case("fill"))
                .count();
            let numbers: Vec<_> = all
                .iter()
                .filter(|w| !w.eq_ignore_ascii_case("fill"))
                .collect();
            fills <= 1
                && sides(numbers.len())
                && numbers.iter().all(|w| is_number(w) || is_percentage(w))
        }
        "width" => {
            sides(all.len())
                && all.iter().all(|w| {
                    w.eq_ignore_ascii_case("auto")
                        || is_number(w)
                        || is_percentage(w)
                        || is_length_percentage(w)
                })
        }
        "outset" => sides(all.len()) && all.iter().all(|w| is_number(w) || is_length_percentage(w)),
        "repeat" => {
            (1..=2).contains(&all.len())
                && all.iter().all(|w| {
                    ["stretch", "repeat", "round", "space"]
                        .iter()
                        .any(|keyword| w.eq_ignore_ascii_case(keyword))
                })
        }
        _ => false,
    }
}
/// `-webkit-mask-box-image` and its parts, which the engine's servo build lacks:
/// each part goes to its custom property, the shorthand setting all five.
fn expand_mask_box_image(name: &str, value: &str, important: &str) -> Option<String> {
    let longhand = name.strip_prefix("-webkit-").unwrap_or(name);
    if longhand != "mask-box-image" {
        let part = longhand.strip_prefix("mask-box-image-").unwrap_or(longhand);
        return is_valid_mask_box_image_part(part, value)
            .then(|| format!("{}: {value}{important}", mirror_name(longhand)));
    }
    const PARTS: [&str; 5] = [
        "mask-box-image-source",
        "mask-box-image-slice",
        "mask-box-image-width",
        "mask-box-image-outset",
        "mask-box-image-repeat",
    ];
    let value = value.trim();
    if is_global(value) {
        return Some(
            PARTS
                .iter()
                .map(|part| format!("{}: {value}{important}", mirror_name(part)))
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    let is_image = |word: &str| {
        let lower = word.to_ascii_lowercase();
        lower == "none"
            || lower.starts_with("url(")
            || lower.contains("gradient(")
            || lower.starts_with("image-set(")
            || lower.starts_with("-webkit-image-set(")
            || lower.starts_with("cross-fade(")
    };
    let is_repeat = |word: &str| {
        ["stretch", "repeat", "round", "space"]
            .iter()
            .any(|keyword| word.eq_ignore_ascii_case(keyword))
    };
    let spaced = space_slashes(value);
    let mut source = None;
    let mut repeat = Vec::new();
    // Slice, width and outset: what comes before the first `/`, between the
    // two, and after the second.
    let mut sections: [Vec<&str>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut section = 0;
    for word in words(&spaced) {
        if word == "/" {
            section += 1;
            if section > 2 {
                return None;
            }
        } else if is_image(word) {
            if source.replace(word).is_some() {
                return None;
            }
        } else if is_repeat(word) {
            repeat.push(word);
        } else {
            sections[section].push(word);
        }
    }
    if repeat.len() > 2 || (section >= 1 && sections[1].is_empty()) {
        return None;
    }
    if section == 2 && sections[2].is_empty() {
        return None;
    }
    let part = |items: &[&str], initial: &str| {
        if items.is_empty() {
            initial.to_owned()
        } else {
            items.join(" ")
        }
    };
    let values = [
        source.unwrap_or("none").to_owned(),
        // The shorthand fills the middle unless told otherwise (Chrome 153), which the
        // slice longhand does not.
        if sections[0].is_empty() {
            "0 fill".to_owned()
        } else if sections[0].iter().any(|w| w.eq_ignore_ascii_case("fill")) {
            sections[0].join(" ")
        } else {
            format!("{} fill", sections[0].join(" "))
        },
        part(&sections[1], "auto"),
        part(&sections[2], "0"),
        part(&repeat, "stretch"),
    ];
    let names = ["source", "slice", "width", "outset", "repeat"];
    if !names
        .iter()
        .zip(&values)
        .all(|(name, value)| is_valid_mask_box_image_part(name, value))
    {
        return None;
    }
    Some(
        PARTS
            .iter()
            .zip(values)
            .map(|(part, value)| format!("{}: {value}{important}", mirror_name(part)))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Whether `word` is a `url(...)` item of a filter list.
fn is_url_item(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    lower.starts_with("url(") && lower.ends_with(')')
}

/// The address in a `url(...)`: what is inside the parentheses, without quotes.
pub(crate) fn url_address(item: &str) -> Option<String> {
    let inner = item
        .trim()
        .get(4..item.trim().len().checked_sub(1)?)?
        .trim();
    let unquoted = match inner.chars().next() {
        Some(quote @ ('"' | '\'')) => inner.strip_prefix(quote)?.strip_suffix(quote)?,
        _ => inner,
    };
    (!unquoted.is_empty()).then(|| unquoted.to_owned())
}

/// Whether `item` is a filter function the engine takes, with an argument of
/// the right kind, so that a list the engine will drop as invalid is not
/// mirrored (the declaration before it must stay).
fn is_valid_filter_function(item: &str) -> bool {
    let lower = item.to_ascii_lowercase();
    let Some((name, rest)) = lower.split_once('(') else {
        return false;
    };
    let Some(argument) = rest.strip_suffix(')') else {
        return false;
    };
    let argument = argument.trim();
    let amount = |text: &str| {
        text.is_empty()
            || text.parse::<f32>().is_ok_and(|n| n >= 0.0)
            || text
                .strip_suffix('%')
                .is_some_and(|n| n.parse::<f32>().is_ok_and(|n| n >= 0.0))
    };
    let is_length = |text: &str| is_length_percentage(text) && !text.ends_with('%');
    match name {
        "brightness" | "contrast" | "saturate" | "grayscale" | "sepia" | "invert" | "opacity" => {
            amount(argument)
        }
        "blur" => argument.is_empty() || is_length(argument),
        "hue-rotate" => {
            argument.is_empty()
                || argument == "0"
                || ["deg", "grad", "rad", "turn"].iter().any(|unit| {
                    argument
                        .strip_suffix(unit)
                        .is_some_and(|n| n.parse::<f32>().is_ok())
                })
        }
        "drop-shadow" => words(argument).len() >= 2,
        _ => false,
    }
}

/// `filter` and `backdrop-filter`, whose `url()` items the engine's servo build
/// cannot hold. The engine gets the list without them, and the custom property
/// the shape of the whole list (`n` for an item the engine has, `url(...)` for
/// the others), or `none` when there is no `url()`. Written for every value, so
/// that a later rule without one takes the urls back.
fn expand_filter(name: &str, value: &str, important: &str) -> Option<String> {
    let mirror = mirror_name(&format!("{name}-urls"));
    let value = value.trim();
    let items = if is_global(value) || value.eq_ignore_ascii_case("none") {
        Vec::new()
    } else {
        words(value)
    };
    if !items.iter().any(|item| is_url_item(item)) {
        return Some(format!(
            "{name}: {value}{important}; {mirror}: none{important}"
        ));
    }
    let mut native = Vec::new();
    // What a `var()` comes to is not known here, and no `url()` can come of it for
    // the engine: the whole list is then the engine's.
    if items
        .iter()
        .any(|item| item.to_ascii_lowercase().contains("var("))
    {
        return Some(format!(
            "{name}: {value}{important}; {mirror}: none{important}"
        ));
    }
    let mut shape = Vec::new();
    for item in items {
        if is_url_item(item) {
            shape.push(item);
        } else {
            if !is_valid_filter_function(item) {
                return None;
            }
            native.push(item);
            shape.push("n");
        }
    }
    let list = if native.is_empty() {
        "none".to_owned()
    } else {
        native.join(" ")
    };
    Some(format!(
        "{name}: {list}{important}; {mirror}: {}{important}",
        shape.join(", ")
    ))
}

fn is_box_keyword(word: &str) -> bool {
    ["border-box", "padding-box", "content-box"]
        .iter()
        .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

/// `background-clip`, whose `text` the engine's servo build does not know: the
/// engine gets a box in its place and the list as written goes to the custom
/// property. Written for every value, so that a later rule without `text` takes
/// the clip back. `-webkit-background-clip` is the same property.
fn expand_background_clip(value: &str, important: &str) -> Option<String> {
    let value = value.trim();
    let mirror = mirror_name("background-clip");
    if is_global(value) {
        return Some(format!(
            "background-clip: {value}{important}; {mirror}: {value}{important}"
        ));
    }
    let mut native = Vec::new();
    for layer in split_commas(value) {
        let word = layer.trim();
        if word.eq_ignore_ascii_case("text") {
            native.push("border-box");
        } else if is_box_keyword(word) {
            native.push(word);
        } else {
            return None;
        }
    }
    Some(format!(
        "background-clip: {}{important}; {mirror}: {value}{important}",
        native.join(", ")
    ))
}

/// The `background` shorthand, which takes `text` as a clip: the engine gets a
/// box in its place, and the custom property says which layers were clipped to
/// the text (`text`) and which were not (`x`), so that a shorthand without
/// `text` takes the clip back too.
fn expand_background(value: &str, important: &str) -> String {
    let value = value.trim();
    let mirror = mirror_name("background-clip");
    if is_global(value) {
        return format!("background: {value}{important}; {mirror}: {value}{important}");
    }
    let mut native = Vec::new();
    let mut marks = Vec::new();
    for layer in split_commas(value) {
        let layer_words = words(layer);
        if layer_words.iter().any(|w| w.eq_ignore_ascii_case("text")) {
            // The one box that is left stands for the other box the text took.
            let boxed = layer_words
                .iter()
                .copied()
                .find(|w| is_box_keyword(w))
                .unwrap_or("border-box");
            let replaced: Vec<&str> = layer_words
                .iter()
                .map(|&w| {
                    if w.eq_ignore_ascii_case("text") {
                        boxed
                    } else {
                        w
                    }
                })
                .collect();
            native.push(replaced.join(" "));
            marks.push("text");
        } else {
            native.push(layer.trim().to_owned());
            marks.push("x");
        }
    }
    format!(
        "background: {}{important}; {mirror}: {}{important}",
        native.join(", "),
        marks.join(", ")
    )
}

const CAPS: [&str; 6] = [
    "small-caps",
    "all-small-caps",
    "petite-caps",
    "all-petite-caps",
    "unicase",
    "titling-caps",
];
const LIGATURES: [&str; 9] = [
    "common-ligatures",
    "no-common-ligatures",
    "discretionary-ligatures",
    "no-discretionary-ligatures",
    "historical-ligatures",
    "no-historical-ligatures",
    "contextual",
    "no-contextual",
    "none",
];
const NUMERIC: [&str; 8] = [
    "lining-nums",
    "oldstyle-nums",
    "proportional-nums",
    "tabular-nums",
    "diagonal-fractions",
    "stacked-fractions",
    "ordinal",
    "slashed-zero",
];
const EAST_ASIAN: [&str; 9] = [
    "jis78",
    "jis83",
    "jis90",
    "jis04",
    "simplified",
    "traditional",
    "full-width",
    "proportional-width",
    "ruby",
];
const POSITION: [&str; 2] = ["sub", "super"];

/// Splits a value into words at top-level whitespace (a function stays whole).
fn words(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = None;
    for (index, c) in value.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => {
                if let Some(begin) = start.take() {
                    out.push(&value[begin..index]);
                }
                continue;
            }
            _ => {}
        }
        start.get_or_insert(index);
    }
    if let Some(begin) = start {
        out.push(&value[begin..]);
    }
    out
}

fn expand_font_variant(value: &str, important: &str) -> Option<String> {
    let words = words(value);
    let mirrored =
        |property: &str, setting: &str| format!("{}: {setting}{important}", mirror_name(property));
    if let [word] = words.as_slice()
        && is_global(word)
    {
        return Some(format!(
            "{}; {}; {}; {}; {}",
            mirrored("font-variant-caps", word),
            mirrored("font-variant-ligatures", word),
            mirrored("font-variant-numeric", word),
            mirrored("font-variant-east-asian", word),
            mirrored("font-variant-position", word),
        ));
    }
    if let [word] = words.as_slice()
        && word.eq_ignore_ascii_case("normal")
    {
        return Some(format!(
            "{}; {}; {}; {}; {}",
            mirrored("font-variant-caps", "normal"),
            mirrored("font-variant-ligatures", "normal"),
            mirrored("font-variant-numeric", "normal"),
            mirrored("font-variant-east-asian", "normal"),
            mirrored("font-variant-position", "normal"),
        ));
    }
    let (mut caps, mut ligatures, mut numeric, mut east_asian, mut position) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for word in &words {
        let lower = word.to_ascii_lowercase();
        let lower = lower.as_str();
        if CAPS.contains(&lower) {
            caps.push(lower.to_owned());
        } else if LIGATURES.contains(&lower) {
            ligatures.push(lower.to_owned());
        } else if NUMERIC.contains(&lower) {
            numeric.push(lower.to_owned());
        } else if EAST_ASIAN.contains(&lower) {
            east_asian.push(lower.to_owned());
        } else if POSITION.contains(&lower) {
            position.push(lower.to_owned());
        } else {
            // `normal` among others, or a keyword this does not know: invalid.
            return None;
        }
    }
    let or_normal = |list: Vec<String>| {
        if list.is_empty() {
            "normal".to_owned()
        } else {
            list.join(" ")
        }
    };
    if caps.len() > 1 {
        return None;
    }
    Some(format!(
        "{}; {}; {}; {}; {}",
        mirrored("font-variant-caps", &or_normal(caps)),
        mirrored("font-variant-ligatures", &or_normal(ligatures)),
        mirrored("font-variant-numeric", &or_normal(numeric)),
        mirrored("font-variant-east-asian", &or_normal(east_asian)),
        mirrored("font-variant-position", &or_normal(position)),
    ))
}

const DECORATION_LINES: [&str; 4] = ["underline", "overline", "line-through", "blink"];
const DECORATION_STYLES: [&str; 5] = ["solid", "double", "dotted", "dashed", "wavy"];

fn expand_text_decoration(value: &str, important: &str) -> Option<String> {
    let words = words(value);
    if let [word] = words.as_slice()
        && is_global(word)
    {
        return Some(format!(
            "text-decoration-line: {word}{important}; text-decoration-style: {word}{important}; \
             text-decoration-color: {word}{important}; {}: {word}{important}",
            mirror_name("text-decoration-thickness"),
        ));
    }
    let (mut lines, mut style, mut color, mut thickness): (
        Vec<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = (Vec::new(), None, None, None);
    for word in &words {
        let lower = word.to_ascii_lowercase();
        if lower == "none" || DECORATION_LINES.contains(&lower.as_str()) {
            lines.push(lower);
        } else if DECORATION_STYLES.contains(&lower.as_str()) {
            if style.replace(lower).is_some() {
                return None;
            }
        } else if lower == "auto" || lower == "from-font" || is_length_percentage(word) {
            if thickness.replace((*word).to_owned()).is_some() {
                return None;
            }
        } else if is_color(word) {
            if color.replace((*word).to_owned()).is_some() {
                return None;
            }
        } else {
            // A word that is no part of the value makes the whole shorthand
            // invalid, as in a browser.
            return None;
        }
    }
    if lines.is_empty() && style.is_none() && color.is_none() && thickness.is_none() {
        return None;
    }
    Some(format!(
        "text-decoration-line: {}{important}; text-decoration-style: {}{important}; \
         text-decoration-color: {}{important}; {}: {}{important}",
        if lines.is_empty() {
            "none".to_owned()
        } else {
            lines.join(" ")
        },
        style.as_deref().unwrap_or("solid"),
        color.as_deref().unwrap_or("currentcolor"),
        mirror_name("text-decoration-thickness"),
        thickness.as_deref().unwrap_or("auto"),
    ))
}

/// A color: a keyword, `#rrggbb` or a color function.
fn is_color(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if let Some(digits) = lower.strip_prefix('#') {
        return digits.chars().all(|c| c.is_ascii_hexdigit())
            && matches!(digits.len(), 3 | 4 | 6 | 8);
    }
    if let Some((name, _)) = lower.split_once('(') {
        return [
            "rgb",
            "rgba",
            "hsl",
            "hsla",
            "hwb",
            "lab",
            "lch",
            "oklab",
            "oklch",
            "color",
            "color-mix",
            "light-dark",
        ]
        .contains(&name);
    }
    lower == "currentcolor"
        || lower == "transparent"
        || cssparser::color::parse_named_color(&lower).is_ok()
}

/// A number with a unit or `%`, or a math function: a thickness.
fn is_length_percentage(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if ["calc(", "min(", "max(", "clamp("]
        .iter()
        .any(|function| lower.starts_with(function))
    {
        return true;
    }
    let numeric = lower.trim_start_matches(['+', '-']);
    if numeric == "0" {
        return true;
    }
    numeric
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || c == '.')
        && lower.chars().any(|c| c.is_ascii_alphabetic() || c == '%')
        && !lower.starts_with('#')
}

/// `clip-path`: the engine reads every form of it but `shape()`, which it drops.
/// A `shape()` is written as `none` for the engine and as the mirrored custom
/// property for [`crate::shape_function`]; any other value is written as itself
/// and the custom property is reset, so the last declaration wins whichever
/// form it has.
fn expand_clip_path(value: &str, important: &str) -> String {
    let mirror = mirror_name("clip-path");
    if value.to_ascii_lowercase().contains("shape(") {
        format!("clip-path: none{important}; {mirror}: {value}{important}")
    } else {
        format!("clip-path: {value}{important}; {mirror}: initial{important}")
    }
}

/// Whether `text` is a valid value of the `mask-*` longhand, so that an invalid
/// declaration is dropped and the one before it stays, as in a browser. Global
/// keywords and anything with a `var()` are not known here and pass.
fn mask_longhand_is_valid(longhand: &str, text: &str) -> bool {
    use crate::mask_values as v;
    use crate::shape_function::Units;
    let text = text.trim();
    if is_global(text) || text.to_ascii_lowercase().contains("var(") {
        return true;
    }
    // Only the syntax is read: no length is resolved.
    let units = Units::new(16.0, (1.0, 1.0));
    match longhand {
        "mask-mode" => v::parse_modes(text).is_some(),
        "mask-repeat" => v::parse_repeats(text).is_some(),
        "mask-position" => v::parse_positions(text, units).is_some(),
        "mask-size" => v::parse_sizes(text, units).is_some(),
        "mask-composite" => v::parse_composites(text).is_some(),
        "mask-origin" => v::parse_boxes(text, false).is_some(),
        "mask-clip" => v::parse_boxes(text, true).is_some(),
        _ => true,
    }
}

/// `mask` and its `-webkit-` forms: the engine's servo build knows `mask-image`
/// only, so the others are written as the mirrored custom properties
/// [`crate::mask_values`] reads. The shorthand is split into its layers here,
/// each longhand getting the whole list (an omitted part is its initial value),
/// as a browser's own shorthand resets what it does not name.
fn expand_mask(name: &str, value: &str, important: &str) -> Option<String> {
    let longhand = name.strip_prefix("-webkit-").unwrap_or(name);
    let prefixed = longhand.len() != name.len();
    let mirror = |what: &str, text: &str| format!("{}: {text}{important}", mirror_name(what));
    match longhand {
        "mask" => expand_mask_shorthand(value, important),
        "mask-image" => Some(format!("mask-image: {value}{important}")),
        "mask-composite" => {
            // The prefixed form names the operators the way canvas does.
            let text = value
                .split(',')
                .map(|word| match word.trim().to_ascii_lowercase().as_str() {
                    "source-over" if prefixed => "add".to_owned(),
                    "source-out" if prefixed => "subtract".to_owned(),
                    "source-in" if prefixed => "intersect".to_owned(),
                    "xor" if prefixed => "exclude".to_owned(),
                    _ => word.trim().to_owned(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            mask_longhand_is_valid("mask-composite", &text).then(|| mirror("mask-composite", &text))
        }
        "mask-origin" | "mask-clip" => {
            // The prefixed form also takes `border`, `padding` and `content`.
            let text = value
                .split(',')
                .map(|word| match word.trim().to_ascii_lowercase().as_str() {
                    "border" if prefixed => "border-box".to_owned(),
                    "padding" if prefixed => "padding-box".to_owned(),
                    "content" if prefixed => "content-box".to_owned(),
                    _ => word.trim().to_owned(),
                })
                .collect::<Vec<_>>()
                .join(", ");
            mask_longhand_is_valid(longhand, &text).then(|| mirror(longhand, &text))
        }
        _ => mask_longhand_is_valid(longhand, value).then(|| mirror(longhand, value)),
    }
}

/// Puts a space around every top-level `/`, so it is a word of its own.
fn space_slashes(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 8);
    let (mut depth, mut quote) = (0usize, None::<char>);
    for c in value.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            ('(', None) => depth += 1,
            (')', None) => depth = depth.saturating_sub(1),
            _ => {}
        }
        if c == '/' && depth == 0 && quote.is_none() {
            out.push_str(" / ");
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits at the top-level commas.
pub(crate) fn split_commas(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quote, mut start) = (0usize, None::<char>, 0usize);
    for (index, c) in value.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            ('(', None) => depth += 1,
            (')', None) => depth = depth.saturating_sub(1),
            (',', None) if depth == 0 => {
                out.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(&value[start..]);
    out
}

fn expand_mask_shorthand(value: &str, important: &str) -> Option<String> {
    let value = value.trim();
    if is_global(value) {
        // A custom property that is `unset` inherits, which the property does not.
        let mirrored = if value.eq_ignore_ascii_case("unset") {
            "initial"
        } else {
            value
        };
        let mut out = format!("mask-image: {value}{important}");
        for what in [
            "mask-mode",
            "mask-repeat",
            "mask-position",
            "mask-size",
            "mask-origin",
            "mask-clip",
            "mask-composite",
        ] {
            out.push_str(&format!("; {}: {mirrored}{important}", mirror_name(what)));
        }
        return Some(out);
    }
    #[derive(Default)]
    struct Layer {
        image: Option<String>,
        position: Vec<String>,
        size: Vec<String>,
        repeat: Vec<String>,
        boxes: Vec<String>,
        composite: Option<String>,
        mode: Option<String>,
    }
    let mut layers = Vec::new();
    for text in split_commas(value) {
        let spaced = space_slashes(text);
        let mut layer = Layer::default();
        let mut in_size = false;
        for word in words(&spaced) {
            let lower = word.to_ascii_lowercase();
            let numeric = word
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | '+' | '-'))
                || ["calc(", "min(", "max(", "clamp("]
                    .iter()
                    .any(|f| lower.starts_with(f));
            let is_function = lower.contains('(') && lower.ends_with(')');
            match lower.as_str() {
                "/" if !in_size && !layer.position.is_empty() => in_size = true,
                "none" => {
                    if layer.image.replace("none".to_owned()).is_some() {
                        return None;
                    }
                }
                "left" | "right" | "top" | "bottom" | "center" if !in_size => {
                    layer.position.push(lower)
                }
                "auto" | "cover" | "contain" if in_size => layer.size.push(lower),
                _ if numeric && in_size => layer.size.push(word.to_owned()),
                _ if numeric => layer.position.push(word.to_owned()),
                "repeat-x" | "repeat-y" | "repeat" | "space" | "round" | "no-repeat" => {
                    in_size = false;
                    layer.repeat.push(lower)
                }
                "border-box" | "padding-box" | "content-box" | "margin-box" | "fill-box"
                | "stroke-box" | "view-box" | "no-clip" => {
                    in_size = false;
                    layer.boxes.push(lower)
                }
                "add" | "subtract" | "intersect" | "exclude" => {
                    if layer.composite.replace(lower).is_some() {
                        return None;
                    }
                }
                "alpha" | "luminance" | "match-source" => {
                    if layer.mode.replace(lower).is_some() {
                        return None;
                    }
                }
                _ if is_function => {
                    in_size = false;
                    if layer.image.replace(word.to_owned()).is_some() {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        if layer.size.len() > 2 || layer.repeat.len() > 2 || layer.boxes.len() > 2 {
            return None;
        }
        layers.push(layer);
    }
    let join =
        |each: &dyn Fn(&Layer) -> String| layers.iter().map(each).collect::<Vec<_>>().join(", ");
    let or = |words: &[String], initial: &str| {
        if words.is_empty() {
            initial.to_owned()
        } else {
            words.join(" ")
        }
    };
    let origin = |l: &Layer| {
        l.boxes
            .first()
            .cloned()
            .unwrap_or_else(|| "border-box".into())
    };
    let clip = |l: &Layer| match l.boxes.as_slice() {
        [] => "border-box".into(),
        [one] if one != "no-clip" => one.clone(),
        [one] => one.clone(),
        [_, second] => second.clone(),
        _ => unreachable!("at most two boxes"),
    };
    // `no-clip` alone is a clip; as the first of two it is not valid.
    if layers
        .iter()
        .any(|l| l.boxes.len() == 2 && l.boxes[0] == "no-clip")
    {
        return None;
    }
    let mut out = format!(
        "mask-image: {}{important}",
        join(&|l| l.image.clone().unwrap_or_else(|| "none".into()))
    );
    let mut push = |what: &str, text: String| {
        out.push_str(&format!("; {}: {text}{important}", mirror_name(what)));
    };
    push(
        "mask-mode",
        join(&|l| l.mode.clone().unwrap_or_else(|| "match-source".into())),
    );
    push("mask-repeat", join(&|l| or(&l.repeat, "repeat")));
    push("mask-position", join(&|l| or(&l.position, "0% 0%")));
    push("mask-size", join(&|l| or(&l.size, "auto")));
    push(
        "mask-origin",
        join(&|l| {
            let origin = origin(l);
            if origin == "no-clip" {
                "border-box".into()
            } else {
                origin
            }
        }),
    );
    push("mask-clip", join(&clip));
    push(
        "mask-composite",
        join(&|l| l.composite.clone().unwrap_or_else(|| "add".into())),
    );
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(css: &str) -> String {
        rewrite_stylesheet(css).into_owned()
    }

    #[test]
    fn a_sheet_that_names_none_of_them_is_returned_unchanged_and_unallocated() {
        assert!(matches!(
            rewrite_stylesheet(".a { color: red; }"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn a_mirrored_property_moves_to_its_custom_property_as_written() {
        assert_eq!(
            sheet(".a { font-feature-settings: \"liga\" 0, \"ss01\"; color: red }"),
            ".a { --florui-font-feature-settings: \"liga\" 0, \"ss01\"; color: red }"
        );
        assert_eq!(
            sheet(".a { text-underline-offset: 3px !important }"),
            ".a { --florui-text-underline-offset: 3px !important }"
        );
        assert_eq!(
            sheet(".a{FONT-KERNING:none}"),
            ".a{--florui-font-kerning: none}"
        );
    }

    #[test]
    fn background_clip_gives_the_engine_a_box_for_text_and_keeps_the_list() {
        assert_eq!(
            sheet(".a { background-clip: text }"),
            ".a { background-clip: border-box; --florui-background-clip: text }"
        );
        assert_eq!(
            sheet(".a { -webkit-background-clip: text, padding-box !important }"),
            ".a { background-clip: border-box, padding-box !important; \
             --florui-background-clip: text, padding-box !important }"
        );
        // Every value is mirrored, so a later one without `text` takes the clip back.
        assert_eq!(
            sheet(".a { background-clip: content-box }"),
            ".a { background-clip: content-box; --florui-background-clip: content-box }"
        );
        // An invalid value is left for the engine to drop.
        assert_eq!(
            sheet(".a { background-clip: sideways }"),
            ".a { background-clip: sideways }"
        );
    }

    #[test]
    fn mask_box_image_is_split_into_its_parts_with_their_own_initial_values() {
        let mirror = |part: &str, value: &str| format!("--florui-mask-box-image-{part}: {value}");
        assert_eq!(
            sheet(".a { -webkit-mask-box-image: url(a.png) 20 / 10px / 2 round }"),
            format!(
                ".a {{ {}; {}; {}; {}; {} }}",
                mirror("source", "url(a.png)"),
                mirror("slice", "20 fill"),
                mirror("width", "10px"),
                mirror("outset", "2"),
                mirror("repeat", "round")
            )
        );
        assert_eq!(
            sheet(".a { -webkit-mask-box-image: linear-gradient(red, blue) }"),
            format!(
                ".a {{ {}; {}; {}; {}; {} }}",
                mirror("source", "linear-gradient(red, blue)"),
                mirror("slice", "0 fill"),
                mirror("width", "auto"),
                mirror("outset", "0"),
                mirror("repeat", "stretch")
            )
        );
        assert_eq!(
            sheet(".a { -webkit-mask-box-image-slice: 10% 20 fill !important }"),
            ".a { --florui-mask-box-image-slice: 10% 20 fill !important }"
        );
        // An invalid part, or a whole invalid shorthand, is left for the engine to drop.
        assert_eq!(
            sheet(".a { -webkit-mask-box-image-slice: sideways }"),
            ".a { -webkit-mask-box-image-slice: sideways }"
        );
        assert_eq!(
            sheet(".a { -webkit-mask-box-image: url(a.png) 20 / bogus }"),
            ".a { -webkit-mask-box-image: url(a.png) 20 / bogus }"
        );
    }
    #[test]
    fn the_background_shorthand_says_which_layers_are_clipped_to_text() {
        assert_eq!(
            sheet(".a { background: linear-gradient(red, blue) text }"),
            ".a { background: linear-gradient(red, blue) border-box; --florui-background-clip: text }"
        );
        assert_eq!(
            sheet(".a { background: url(a.png) padding-box text, red }"),
            ".a { background: url(a.png) padding-box padding-box, red; \
             --florui-background-clip: text, x }"
        );
        assert_eq!(
            sheet(".a { background: red }"),
            ".a { background: red; --florui-background-clip: x }"
        );
        assert_eq!(
            sheet(".a { background: inherit }"),
            ".a { background: inherit; --florui-background-clip: inherit }"
        );
        // Longhands of the shorthand are not touched.
        assert_eq!(
            sheet(".a { background-color: red }"),
            ".a { background-color: red }"
        );
    }
    #[test]
    fn font_variant_is_split_into_the_one_property_the_engine_has_and_the_rest() {
        assert_eq!(
            sheet(".a { font-variant: small-caps tabular-nums }"),
            ".a { --florui-font-variant-caps: small-caps; --florui-font-variant-ligatures: normal; \
             --florui-font-variant-numeric: tabular-nums; --florui-font-variant-east-asian: normal; \
             --florui-font-variant-position: normal }"
        );
        assert!(
            sheet(".a { font-variant: none }").contains("--florui-font-variant-ligatures: none")
        );
        assert!(
            sheet(".a { font-variant: inherit }").contains("--florui-font-variant-caps: inherit")
        );
    }

    #[test]
    fn text_decoration_always_sets_the_thickness_too() {
        assert_eq!(
            sheet(".a { text-decoration: underline wavy #c0392b 2px }"),
            ".a { text-decoration-line: underline; text-decoration-style: wavy; \
             text-decoration-color: #c0392b; --florui-text-decoration-thickness: 2px }"
        );
        assert_eq!(
            sheet(".a { text-decoration: underline }"),
            ".a { text-decoration-line: underline; text-decoration-style: solid; \
             text-decoration-color: currentcolor; --florui-text-decoration-thickness: auto }"
        );
        assert!(
            sheet(".a { text-decoration: overline line-through rgb(1 2 3) }")
                .contains("text-decoration-line: overline line-through")
        );
    }

    #[test]
    fn an_invalid_value_is_left_for_the_engine() {
        for css in [
            ".a { font-variant: small-caps all-small-caps }",
            ".a { font-variant: bogus }",
            ".a { text-decoration: }",
            ".a { text-decoration: underline wavy dotted }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn a_value_that_ends_in_a_function_is_kept_whole() {
        assert_eq!(
            sheet(".a { text-decoration: underline rgb(1 2 3) }"),
            ".a { text-decoration-line: underline; text-decoration-style: solid; \
             text-decoration-color: rgb(1 2 3); --florui-text-decoration-thickness: auto }"
        );
        assert_eq!(
            sheet(".a { text-underline-offset: calc(1px + 2px) !important }"),
            ".a { --florui-text-underline-offset: calc(1px + 2px) !important }"
        );
        assert_eq!(
            sheet(".a { font-feature-settings: \"liga\" 0, \"ss01\" }"),
            ".a { --florui-font-feature-settings: \"liga\" 0, \"ss01\" }"
        );
    }
    #[test]
    fn a_word_that_is_no_part_of_the_value_leaves_the_whole_shorthand_to_the_engine() {
        for css in [
            ".a { text-decoration: underlin }",
            ".a { text-decoration: underline foo }",
            ".a { text-decoration: underline #12 }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
        assert!(sheet(".a { text-decoration: underline AliceBlue }").contains("AliceBlue"));
        assert!(sheet(".a { text-decoration: underline rgb(1 2 3) 0 }").contains("-thickness: 0"));
        assert!(sheet(".a { text-decoration: auto }").contains("-thickness: auto"));
    }

    #[test]
    fn text_in_comments_strings_and_selectors_is_never_touched() {
        for css in [
            "/* font-kerning: none; */ .a { color: red; }",
            ".a { content: \"text-decoration: underline;\"; }",
            ".text-decoration { color: red; }",
            ".a { transition-property: text-decoration-thickness; }",
        ] {
            assert_eq!(sheet(css), css, "{css}");
        }
    }

    #[test]
    fn a_style_attribute_is_rewritten_at_the_top_level() {
        assert_eq!(
            rewrite_declarations("color: red; font-kerning: none").into_owned(),
            "color: red; --florui-font-kerning: none"
        );
    }
}
