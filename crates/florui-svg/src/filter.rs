//! The region a `<filter>` works in, which for the default units is a fraction
//! of the box it filters. The picture a filter is given is the whole surface the
//! box was painted on (its shadows and overflow included), so its own bounding
//! box would not be the box's: the region is resolved against the box here and
//! written back as plain user-space numbers.

use crate::document::Reference;
/// A start tag found in markup: where it begins and ends, and its attributes.
pub(crate) struct StartTag {
    pub begin: usize,
    pub end: usize,
    pub attrs: Vec<(String, String)>,
}

/// The attributes of the start tag of the element `tag` with this `id` in `defs`,
/// and where that tag begins and ends in `defs`.
/// and where that tag begins and ends in `defs`.
pub(crate) fn start_tag(defs: &str, tag: &str, id: &str) -> Option<StartTag> {
    let open = format!("<{tag}");
    let mut from = 0;
    while let Some(found) = defs[from..].find(&open) {
        let begin = from + found;
        let after = begin + open.len();
        // `<filter` and not `<filterX`; the same for any tag.
        let next = defs[after..].chars().next()?;
        from = after;
        if !(next.is_whitespace() || next == '>' || next == '/') {
            continue;
        }
        let end = after + defs[after..].find('>')? + 1;
        let attrs = attributes(&defs[after..end - 1]);
        if attrs
            .iter()
            .any(|(name, value)| name == "id" && value == id)
        {
            return Some(StartTag { begin, end, attrs });
        }
        from = end;
    }
    None
}

/// `name="value"` pairs of the inside of a start tag.
fn attributes(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text.trim_end_matches('/');
    while let Some(equals) = rest.find('=') {
        let name = rest[..equals].trim().to_owned();
        let after = rest[equals + 1..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(close) = after[1..].find(quote) else {
            break;
        };
        out.push((name, after[1..1 + close].to_owned()));
        rest = &after[close + 2..];
    }
    out
}

/// One of the four region values against a box side of `basis`: a number or a
/// percentage, as a fraction of the box when the units are the box's, and as
/// pixels otherwise.
fn length(text: Option<&str>, default: f32, basis: f32, box_units: bool) -> f32 {
    let Some(text) = text.map(str::trim) else {
        return default * basis;
    };
    if let Some(percent) = text.strip_suffix('%')
        && let Ok(n) = percent.trim().parse::<f32>()
    {
        return n / 100.0 * basis;
    }
    let number = text.trim_end_matches("px").trim();
    match number.parse::<f32>() {
        Ok(n) if box_units => n * basis,
        Ok(n) => n,
        Err(_) => default * basis,
    }
}

/// The region of the filter `reference` for a box of `width` by `height` CSS
/// pixels, as `(left, top, right, bottom)` from the box's top left; `None` when
/// the filter is not found in the definitions.
pub fn filter_region(
    reference: &Reference,
    width: f32,
    height: f32,
) -> Option<(f32, f32, f32, f32)> {
    let attrs = start_tag(&reference.defs, "filter", &reference.id)?.attrs;
    let get = |name: &str| {
        attrs
            .iter()
            .find(|(attr, _)| attr == name)
            .map(|(_, value)| value.as_str())
    };
    let box_units = get("filterUnits") != Some("userSpaceOnUse");
    // The defaults are -10% and 120% of the box, in either units.
    let (default_origin, default_size) = (-0.1, 1.2);
    let x = length(get("x"), default_origin, width, box_units);
    let y = length(get("y"), default_origin, height, box_units);
    let w = length(get("width"), default_size, width, box_units);
    let h = length(get("height"), default_size, height, box_units);
    // Rounded to a thousandth, as an authored number is, so the sums are exact.
    let round = |v: f32| (v * 1000.0).round() / 1000.0;
    Some((round(x), round(y), round(x + w), round(y + h)))
}

fn round(n: f32) -> f32 {
    (n * 1000.0).round() / 1000.0
}

/// A number or a percentage as a fraction (`0.1` and `10%` are the same).
fn fraction(text: &str) -> Option<f32> {
    let text = text.trim();
    match text.strip_suffix('%') {
        Some(percent) => percent.trim().parse::<f32>().ok().map(|n| n / 100.0),
        None => text.parse::<f32>().ok(),
    }
}

/// `value`, one or two numbers (`"2 3"`), each multiplied by the box's `scales`,
/// the second by the second of them (and by the first when there is only one).
fn scaled_pair(value: &str, (horizontal, vertical): (f32, f32)) -> Option<String> {
    let numbers: Vec<f32> = value
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f32>().ok())
        .collect::<Option<_>>()?;
    match numbers[..] {
        [one] => Some(format!(
            "{} {}",
            round(one * horizontal),
            round(one * vertical)
        )),
        [a, b] => Some(format!("{} {}", round(a * horizontal), round(b * vertical))),
        _ => None,
    }
}

/// The attributes of a filter primitive, written in the box's units (fractions of
/// its `width` and `height`), as numbers of user space.
fn in_user_space(
    tag: &str,
    attrs: &[(String, String)],
    width: f32,
    height: f32,
) -> Vec<(String, String)> {
    let diagonal = ((width * width + height * height) / 2.0).sqrt();
    attrs
        .iter()
        .map(|(name, value)| {
            let converted = match name.as_str() {
                "x" | "dx" | "pointsAtX" => fraction(value).map(|n| n * width),
                "y" | "dy" | "pointsAtY" => fraction(value).map(|n| n * height),
                "width" => fraction(value).map(|n| n * width),
                "height" => fraction(value).map(|n| n * height),
                "z" | "pointsAtZ" | "scale" => fraction(value).map(|n| n * diagonal),
                "stdDeviation" | "radius" if tag != "feConvolveMatrix" => {
                    return (
                        name.clone(),
                        scaled_pair(value, (width, height)).unwrap_or_else(|| value.clone()),
                    );
                }
                _ => None,
            };
            // To a thousandth, as an authored number is.
            let value = converted.map_or_else(
                || value.clone(),
                |n| ((n * 1000.0).round() / 1000.0).to_string(),
            );
            (name.clone(), value)
        })
        .collect()
}

/// The primitives in `body` with their attributes put in user space.
fn primitives_in_user_space(body: &str, width: f32, height: f32) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find("<fe") {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find('>') else {
            rest = after;
            break;
        };
        let inside = &after[1..close];
        let name_end = inside
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(inside.len());
        let (tag, attributes_text) = inside.split_at(name_end);
        let self_closing = attributes_text.trim_end().ends_with('/');
        let attrs = in_user_space(tag, &attributes(attributes_text), width, height);
        out.push('<');
        out.push_str(tag);
        for (name, value) in attrs {
            out.push_str(&format!(" {name}=\"{value}\""));
        }
        out.push_str(if self_closing { "/>" } else { ">" });
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}
/// `reference` with the region of its filter written as numbers of the box's own
/// user space (`filterUnits="userSpaceOnUse"`), for a box of `width` by `height`.
pub(crate) fn with_resolved_region(reference: &Reference, width: f32, height: f32) -> Reference {
    let Some((left, top, right, bottom)) = filter_region(reference, width, height) else {
        return reference.clone();
    };
    let Some(StartTag { begin, end, attrs }) = start_tag(&reference.defs, "filter", &reference.id)
    else {
        return reference.clone();
    };
    // The primitives' own units, when they are the box's, are put in user space too.
    let box_primitives = attrs
        .iter()
        .any(|(name, value)| name == "primitiveUnits" && value == "objectBoundingBox");
    let mut tag = String::from("<filter");
    for (name, value) in &attrs {
        if !matches!(
            name.as_str(),
            "x" | "y" | "width" | "height" | "filterUnits" | "primitiveUnits"
        ) {
            tag.push_str(&format!(" {name}=\"{value}\""));
        }
    }
    tag.push_str(&format!(
        " x=\"{left}\" y=\"{top}\" width=\"{}\" height=\"{}\" filterUnits=\"userSpaceOnUse\" primitiveUnits=\"userSpaceOnUse\">",
        right - left,
        bottom - top
    ));
    let mut defs = String::with_capacity(reference.defs.len() + 64);
    defs.push_str(&reference.defs[..begin]);
    defs.push_str(&tag);
    let body_end = reference.defs[end..]
        .find("</filter>")
        .map_or(reference.defs.len(), |close| end + close);
    if box_primitives {
        defs.push_str(&primitives_in_user_space(
            &reference.defs[end..body_end],
            width,
            height,
        ));
    } else {
        defs.push_str(&reference.defs[end..body_end]);
    }
    defs.push_str(&reference.defs[body_end..]);
    Reference {
        id: reference.id.clone(),
        defs,
    }
}

/// `style` without the declarations of `property`.
fn without_declaration(style: &str, property: &str) -> String {
    style
        .split(';')
        .filter(|declaration| {
            declaration
                .split(':')
                .next()
                .is_none_or(|name| !name.trim().eq_ignore_ascii_case(property))
                && !declaration.trim().is_empty()
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// `reference` with its `<mask>` made of the given type, `"alpha"` or
/// `"luminance"`, whatever it said: `mask-mode` on the layer decides.
pub(crate) fn with_mask_type(reference: &Reference, kind: &str) -> Reference {
    let Some(StartTag { begin, end, attrs }) = start_tag(&reference.defs, "mask", &reference.id)
    else {
        return reference.clone();
    };
    let mut tag = String::from("<mask");
    for (name, value) in &attrs {
        match name.as_str() {
            "mask-type" => {}
            "style" => {
                let kept = without_declaration(value, "mask-type");
                if !kept.is_empty() {
                    tag.push_str(&format!(" style=\"{kept}\""));
                }
            }
            _ => tag.push_str(&format!(" {name}=\"{value}\"")),
        }
    }
    tag.push_str(&format!(" mask-type=\"{kind}\">"));
    let mut defs = String::with_capacity(reference.defs.len() + 32);
    defs.push_str(&reference.defs[..begin]);
    defs.push_str(&tag);
    defs.push_str(&reference.defs[end..]);
    Reference {
        id: reference.id.clone(),
        defs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(attrs: &str) -> Reference {
        Reference {
            id: "f".to_owned(),
            defs: format!("<g/><filter id=\"f\" {attrs}><feOffset dx=\"1\"/></filter>"),
        }
    }

    #[test]
    fn the_default_region_is_a_tenth_beyond_the_box_on_every_side() {
        let region = filter_region(&filter(""), 100.0, 50.0).expect("a region");
        assert_eq!(region, (-10.0, -5.0, 110.0, 55.0));
    }

    #[test]
    fn a_region_is_read_in_the_boxs_units_or_in_pixels() {
        let boxed = filter("x=\"-0.5\" y=\"0\" width=\"2\" height=\"150%\"");
        assert_eq!(
            filter_region(&boxed, 100.0, 50.0),
            Some((-50.0, 0.0, 150.0, 75.0))
        );
        let pixels =
            filter("filterUnits=\"userSpaceOnUse\" x=\"-4\" y=\"-4\" width=\"108\" height=\"60\"");
        assert_eq!(
            filter_region(&pixels, 100.0, 50.0),
            Some((-4.0, -4.0, 104.0, 56.0))
        );
    }

    #[test]
    fn a_filter_is_found_by_its_id_among_the_others() {
        let reference = Reference {
            id: "g".to_owned(),
            defs: "<filter id=\"f\" x=\"0\"></filter><filterX id=\"g\"/><filter id=\"g\" x=\"1\" width=\"1\"></filter>"
                .to_owned(),
        };
        let region = filter_region(&reference, 10.0, 10.0).expect("a region");
        assert_eq!(region.0, 10.0);
    }

    #[test]
    fn primitives_in_the_boxs_units_are_written_in_user_space() {
        let reference = Reference {
            id: "f".to_owned(),
            defs: "<filter id=\"f\" primitiveUnits=\"objectBoundingBox\"><feOffset dx=\"0.2\" dy=\"0.3\"/>\
                   <feGaussianBlur stdDeviation=\"0.05 0.1\"></feGaussianBlur>\
                   <feFlood x=\"10%\" width=\"0.5\" flood-color=\"red\"/></filter>"
                .to_owned(),
        };
        let rewritten = with_resolved_region(&reference, 100.0, 50.0);
        assert!(rewritten.defs.contains("primitiveUnits=\"userSpaceOnUse\""));
        assert!(
            rewritten.defs.contains("<feOffset dx=\"20\" dy=\"15\"/>"),
            "{}",
            rewritten.defs
        );
        assert!(
            rewritten
                .defs
                .contains("<feGaussianBlur stdDeviation=\"5 5\">"),
            "{}",
            rewritten.defs
        );
        assert!(
            rewritten
                .defs
                .contains("x=\"10\" width=\"50\" flood-color=\"red\""),
            "{}",
            rewritten.defs
        );
        // Left alone when they are in user space already.
        let plain = Reference {
            id: "f".to_owned(),
            defs: "<filter id=\"f\"><feOffset dx=\"0.2\"/></filter>".to_owned(),
        };
        assert!(
            with_resolved_region(&plain, 100.0, 50.0)
                .defs
                .contains("<feOffset dx=\"0.2\"/>")
        );
    }
    #[test]
    fn the_resolved_region_is_written_back_in_user_space() {
        let rewritten =
            with_resolved_region(&filter("color-interpolation-filters=\"sRGB\""), 100.0, 50.0);
        assert!(rewritten.defs.contains("filterUnits=\"userSpaceOnUse\""));
        assert!(
            rewritten
                .defs
                .contains("x=\"-10\" y=\"-5\" width=\"120\" height=\"60\"")
        );
        assert!(
            rewritten
                .defs
                .contains("color-interpolation-filters=\"sRGB\"")
        );
        assert!(rewritten.defs.starts_with("<g/><filter id=\"f\""));
        assert!(rewritten.defs.ends_with("<feOffset dx=\"1\"/></filter>"));
    }
}
