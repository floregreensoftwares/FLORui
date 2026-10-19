//! Finding what a `url()` points at, and writing it as SVG markup.

use florui_style::{ComputedStyle, NodeId, Rgba, StyleMap, Tree};

/// The address a stylesheet's relative `url(#id)` resolves to.
const BASE: &str = "about:florui";

/// The SVG properties an element takes from its parent when it has none (the rest
/// are those of the element alone).
const INHERITED: &[&str] = &[
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
    "clip-rule",
    "color-interpolation",
    "color-interpolation-filters",
    "shape-rendering",
    "paint-order",
    "marker-start",
    "marker-mid",
    "marker-end",
];

/// What a `url()` of a mask, clip or filter names: the markup of every definition
/// that may be needed (the content of the document's inline SVGs, or of the file)
/// and the `id` of the one that is applied.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reference {
    pub id: String,
    pub defs: String,
}

impl Reference {
    /// Whether the definitions hold an element `tag` with this reference's `id`.
    pub fn defines(&self, tag: &str) -> bool {
        crate::filter::start_tag(&self.defs, tag, &self.id).is_some()
    }
}

/// The file (none for this document) and the id in a `url()` with a fragment.
fn split(url: &str) -> Option<(Option<&str>, &str)> {
    let url = url.trim();
    let url = url.strip_prefix(BASE).unwrap_or(url);
    let (file, id) = url.rsplit_once('#')?;
    // `#svgView(...)` addresses a view of an image, not an element.
    if id.is_empty() || id.starts_with("svgView(") {
        return None;
    }
    Some(((!file.is_empty()).then_some(file), id))
}

/// Whether `url` addresses an element (`#id`, or `file.svg#id`) rather than an image.
pub fn is_reference(url: &str) -> bool {
    split(url).is_some()
}

/// The reference `url` makes, with the definitions it can use; `None` when it is
/// not one, or its file cannot be read.
pub fn reference(arena: &Tree, styles: &StyleMap, url: &str) -> Option<Reference> {
    let (file, id) = split(url)?;
    let defs = match file {
        None => document_defs(arena, styles),
        Some(path) => file_defs(path)?,
    };
    Some(Reference {
        id: id.to_owned(),
        defs,
    })
}

/// The content of every inline `<svg>` that is not inside another, written as
/// markup: ids are those of the whole document, as in a browser. What CSS gives
/// the elements goes with them.
fn document_defs(arena: &Tree, styles: &StyleMap) -> String {
    let mut out = String::new();
    let tops =
        arena.find_all(|tree, node| tree.tag(node) == "svg" && !has_svg_ancestor(tree, node));
    for node in tops {
        let root = styles.get(&node);
        // What the SVG takes from the page: its colour (for `currentColor`) and
        // the inherited properties its ancestors declare.
        let mut chain = vec![node];
        let mut up = arena.parent(node);
        while let Some(parent) = up {
            chain.push(parent);
            up = arena.parent(parent);
        }
        let mut inherited: Vec<(&str, &str)> = Vec::new();
        for &ancestor in chain.iter().rev() {
            let own = styles
                .get(&ancestor)
                .map_or(&[][..], |s| &s.svg_properties[..]);
            for (name, value) in own {
                if INHERITED.contains(name) {
                    inherited.retain(|(kept, _)| kept != name);
                    inherited.push((name, value.as_str()));
                }
            }
        }
        let mut declarations = Vec::new();
        if let Some(root) = root {
            declarations.push(format!("color:{}", color(root.color)));
        }
        declarations.extend(
            inherited
                .iter()
                .map(|(name, value)| format!("{name}:{value}")),
        );
        out.push_str(&format!(
            "<g style=\"{}\">",
            escape(&declarations.join(";"))
        ));
        for &child in arena.children(node) {
            write_element(arena, styles, child, root.map(|s| s.color), &mut out);
        }
        out.push_str("</g>");
    }
    out
}

fn has_svg_ancestor(tree: &Tree, node: NodeId) -> bool {
    let mut parent = tree.parent(node);
    while let Some(id) = parent {
        if tree.tag(id) == "svg" {
            return true;
        }
        parent = tree.parent(id);
    }
    false
}

fn color(color: Rgba) -> String {
    format!(
        "rgba({},{},{},{})",
        color.r,
        color.g,
        color.b,
        f32::from(color.a) / 255.0
    )
}

/// What CSS gives an element beyond its attributes: the SVG properties its rules
/// declare, its `opacity`, `visibility` and `display`, and its `color` when it is
/// not its parent's.
fn style_declarations(style: &ComputedStyle, parent_color: Option<Rgba>) -> Vec<String> {
    let mut out: Vec<String> = style
        .svg_properties
        .iter()
        .map(|(name, value)| format!("{name}:{value}"))
        .collect();
    if style.opacity < 1.0 {
        out.push(format!("opacity:{}", style.opacity));
    }
    if !style.visibility.is_visible() {
        out.push("visibility:hidden".to_owned());
    }
    if style.display == florui_style::Display::None {
        out.push("display:none".to_owned());
    }
    if parent_color != Some(style.color) {
        out.push(format!("color:{}", color(style.color)));
    }
    out
}

fn write_element(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    parent_color: Option<Rgba>,
    out: &mut String,
) {
    let tag = arena.tag(node);
    let style = styles.get(&node);
    // The rules' declarations come before the element's own `style` attribute,
    // which wins over them.
    let own = style
        .map(|s| style_declarations(s, parent_color))
        .unwrap_or_default();
    out.push('<');
    out.push_str(tag);
    let mut wrote_style = false;
    for (name, value) in arena.attrs(node) {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        if name == "style" && !own.is_empty() {
            escape_into(&own.join(";"), out);
            out.push(';');
            wrote_style = true;
        }
        escape_into(value, out);
        out.push('"');
    }
    if !own.is_empty() && !wrote_style {
        out.push_str(" style=\"");
        escape_into(&own.join(";"), out);
        out.push('"');
    }
    out.push('>');
    let here = style.map(|s| s.color).or(parent_color);
    for &child in arena.children(node) {
        write_element(arena, styles, child, here, out);
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    escape_into(text, &mut out);
    out
}

fn escape_into(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// The text a `data:` URL holds.
fn data_url_text(url: &str) -> Option<String> {
    use base64::Engine as _;
    let (header, payload) = url.strip_prefix("data:")?.split_once(',')?;
    let bytes = if header
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        let cleaned: Vec<u8> = percent_decode(payload)
            .into_iter()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(cleaned)
            .ok()?
    } else {
        percent_decode(payload)
    };
    String::from_utf8(bytes).ok()
}

fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(hex) = text.get(index + 1..index + 3)
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// What is inside the root `<svg>` of a file or a `data:` URL.
fn file_defs(path: &str) -> Option<String> {
    let text = if path.starts_with("data:") {
        data_url_text(path)?
    } else {
        std::fs::read_to_string(path).ok()?
    };
    let open = text.find("<svg")?;
    let start = open + text[open..].find('>')? + 1;
    let end = text.rfind("</svg>")?;
    (start <= end).then(|| text[start..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use florui::prelude::*;
    use florui_style::{Arena, InteractionState, Viewport, compute, parse_stylesheet};

    fn styles_of(arena: &Arena, css: &str) -> StyleMap {
        compute(
            arena,
            &parse_stylesheet(css).unwrap(),
            &InteractionState::new(),
            Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        )
    }

    #[test]
    fn a_fragment_names_an_element_and_a_view_does_not() {
        assert!(is_reference("#m"));
        assert!(is_reference("about:florui#m"));
        assert!(is_reference("masks.svg#m"));
        assert!(!is_reference("masks.svg"));
        assert!(!is_reference("a.svg#svgView(viewBox(0,0,1,1))"));
        assert!(!is_reference("data:image/svg+xml;utf8,<svg/>"));
        assert!(!is_reference("#"));
    }

    #[test]
    fn a_data_url_or_a_file_is_a_document_of_definitions() {
        let arena = Arena::build(&Element::node("div", Vec::new(), Vec::new()));
        let styles = styles_of(&arena, "");
        let plain = "data:image/svg+xml;utf8,%3Csvg xmlns='http://www.w3.org/2000/svg'%3E%3Cmask id='e'/%3E%3C/svg%3E#e";
        let found = reference(&arena, &styles, plain).expect("a reference");
        assert_eq!(found.id, "e");
        assert_eq!(found.defs, "<mask id='e'/>");
        let encoded = format!(
            "data:image/svg+xml;base64,{}#c",
            "PHN2Zz48Y2xpcFBhdGggaWQ9J2MnLz48L3N2Zz4="
        );
        let found = reference(&arena, &styles, &encoded).expect("a reference");
        assert_eq!(
            (found.id.as_str(), found.defs.as_str()),
            ("c", "<clipPath id='c'/>")
        );
        assert!(reference(&arena, &styles, "no-such-file.svg#x").is_none());
    }

    #[test]
    fn the_inline_svgs_of_a_document_are_its_definitions() {
        let tree: Element = view! {
            <div>
                <svg width="0" height="0">
                    <defs>
                        <mask id="m" maskUnits="userSpaceOnUse">
                            <rect width="10" height="10" fill="white" stop-color="a&b" />
                        </mask>
                    </defs>
                </svg>
                <svg><circle id="c" r="1" /></svg>
            </div>
        };
        let arena = Arena::build(&tree);
        let styles = styles_of(&arena, "");
        let found = reference(&arena, &styles, "about:florui#m").expect("a reference");
        assert_eq!(found.id, "m");
        let defs = found.defs;
        assert!(defs.starts_with("<g style=\"color:rgba(0,0,0,1)\"><defs><mask id=\"m\""));
        assert!(defs.contains(
            "<rect width=\"10\" height=\"10\" fill=\"white\" stop-color=\"a&amp;b\"></rect>"
        ));
        assert!(defs.contains("</g><g style=\"color:rgba(0,0,0,1)\"><circle id=\"c\" r=\"1\">"));
    }

    #[test]
    fn what_css_gives_the_elements_goes_with_them() {
        let tree: Element = view! {
            <div class="page">
                <svg class="s">
                    <mask id="m">
                        <rect class="r" style="stroke: blue" />
                    </mask>
                    <mask id="n" style="mask-type: luminance" />
                </svg>
            </div>
        };
        let arena = Arena::build(&tree);
        let styles = styles_of(
            &arena,
            ".page { fill: green; color: #ff0000; } .s { stroke-width: 3; } \
             mask { mask-type: alpha; } .r { fill: white; opacity: 0.5; color: #0000ff; }",
        );
        let defs = reference(&arena, &styles, "#m").expect("a reference").defs;
        // The page's colour and inherited property, and the svg's own, on its root.
        assert!(defs.starts_with("<g style=\"color:rgba(255,0,0,1);fill:green;stroke-width:3\">"));
        // A property a rule declares on the mask, and the inline style that beats it.
        assert!(
            defs.contains("<mask id=\"m\" style=\"mask-type:alpha\">"),
            "{defs}"
        );
        assert!(
            defs.contains("<mask id=\"n\" style=\"mask-type:luminance;mask-type: luminance\">"),
            "{defs}"
        );
        // The rect: its declared properties, its opacity and its own colour, then its `style`.
        let rect = &defs[defs.find("<rect").expect("a rect")..];
        for wanted in [
            "fill:white",
            "opacity:0.5",
            "color:rgba(0,0,255,1)",
            "stroke: blue",
        ] {
            assert!(rect.contains(wanted), "{wanted} in {rect}");
        }
    }
}
