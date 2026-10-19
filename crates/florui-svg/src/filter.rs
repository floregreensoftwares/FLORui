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
    let mut tag = String::from("<filter");
    for (name, value) in &attrs {
        if !matches!(
            name.as_str(),
            "x" | "y" | "width" | "height" | "filterUnits"
        ) {
            tag.push_str(&format!(" {name}=\"{value}\""));
        }
    }
    tag.push_str(&format!(
        " x=\"{left}\" y=\"{top}\" width=\"{}\" height=\"{}\" filterUnits=\"userSpaceOnUse\">",
        right - left,
        bottom - top
    ));
    let mut defs = String::with_capacity(reference.defs.len() + 64);
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
