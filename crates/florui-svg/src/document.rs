//! Finding what a `url()` points at, and writing it as SVG markup.

use florui_style::{ComputedStyle, InlineItem, NodeId, Rgba, StyleMap, Tree};

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
        out.push_str(&content(arena, styles, node));
    }
    out
}

/// What `node` and its ancestors give what is inside it, as declarations: its colour,
/// font and the inherited SVG properties.
fn carried(arena: &Tree, styles: &StyleMap, node: NodeId) -> String {
    let root = styles.get(&node);
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
        declarations.extend(font_declarations(root, None));
    }
    declarations.extend(
        inherited
            .iter()
            .map(|(name, value)| format!("{name}:{value}")),
    );
    declarations.join(";")
}

/// What the SVG `node` holds, in a `<g>` carrying what it takes from the page: its
/// colour (for `currentColor`) and the inherited properties its ancestors declare.
fn content(arena: &Tree, styles: &StyleMap, node: NodeId) -> String {
    let root = styles.get(&node);
    let mut out = format!("<g style=\"{}\">", escape(&carried(arena, styles, node)));
    write_children(arena, styles, node, root, view_of(arena, node), &mut out);
    out.push_str("</g>");
    out
}

/// The text and elements in `node`, in order.
fn write_children(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    parent: Option<&ComputedStyle>,
    view: (f32, f32),
    out: &mut String,
) {
    for item in arena.inline_items(node) {
        match item {
            InlineItem::Text(text) => escape_into(text, out),
            InlineItem::Element(child) => {
                write_element(arena, styles, *child, parent, view, true, out);
            }
        }
    }
}

/// The whole SVG `node` as a document of `width` by `height` CSS pixels: its own
/// content, styled as CSS gives, after the definitions of the document's other
/// SVGs, which it may point at. `None` for an element that is not an `<svg>`.
pub(crate) fn svg_document(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    width: f32,
    height: f32,
) -> Option<String> {
    if arena.tag(node) != "svg" {
        return None;
    }
    let others: String = arena
        .find_all(|tree, other| {
            other != node && tree.tag(other) == "svg" && !has_svg_ancestor(tree, other)
        })
        .into_iter()
        .map(|other| content(arena, styles, other))
        .collect();
    let keep = |name: &str| {
        arena
            .attr(node, name)
            .map(|value| format!(" {name}=\"{}\"", escape(value)))
            .unwrap_or_default()
    };
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{width}\" height=\"{height}\"{}{}><defs>{others}</defs>{}</svg>",
        keep("viewBox"),
        keep("preserveAspectRatio"),
        content(arena, styles, node)
    ))
}

/// The size of the viewport an `<svg>` shows, in its own units: its `viewBox`, or its
/// size.
fn view_of(arena: &Tree, node: NodeId) -> (f32, f32) {
    let numbers = |name: &str| -> Vec<f32> {
        arena
            .attr(node, name)
            .map(|value| {
                value
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter_map(|part| part.trim_end_matches("px").parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    if let [_, _, width, height] = numbers("viewBox")[..] {
        return (width, height);
    }
    let length = |name: &str, default: f32| numbers(name).first().copied().unwrap_or(default);
    (length("width", 300.0), length("height", 150.0))
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
fn style_declarations(style: &ComputedStyle, parent: Option<&ComputedStyle>) -> Vec<String> {
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
    if style.author_hidden {
        out.push("display:none".to_owned());
    }
    if parent.map(|p| p.color) != Some(style.color) {
        out.push(format!("color:{}", color(style.color)));
    }
    out.extend(font_declarations(style, parent));
    out
}

/// The font of an element, as `font-*` declarations, where it is not its parent's:
/// text in an SVG takes its font from the page as any text does.
fn font_declarations(style: &ComputedStyle, parent: Option<&ComputedStyle>) -> Vec<String> {
    use florui_style::{FontFamily, FontStyle};
    let mut out = Vec::new();
    let differs = |same: &dyn Fn(&ComputedStyle) -> bool| !parent.is_some_and(same);
    if differs(&|p| p.font_size == style.font_size) {
        out.push(format!("font-size:{}px", style.font_size));
    }
    if differs(&|p| p.font_family == style.font_family) {
        out.push(match &style.font_family {
            FontFamily::SansSerif => "font-family:sans-serif".to_owned(),
            FontFamily::Monospace => "font-family:monospace".to_owned(),
            FontFamily::Stack(list) => format!("font-family:{list}"),
        });
    }
    if differs(&|p| p.font_weight == style.font_weight) {
        out.push(format!("font-weight:{}", style.font_weight));
    }
    if differs(&|p| p.font_style == style.font_style) {
        out.push(match style.font_style {
            FontStyle::Normal => "font-style:normal".to_owned(),
            FontStyle::Italic => "font-style:italic".to_owned(),
            FontStyle::Oblique(degrees) => format!("font-style:oblique {degrees}deg"),
        });
    }
    if differs(&|p| p.letter_spacing == style.letter_spacing) {
        out.push(format!("letter-spacing:{}px", style.letter_spacing));
    }
    if differs(&|p| p.word_spacing == style.word_spacing) {
        out.push(format!("word-spacing:{}px", style.word_spacing));
    }
    out
}

/// The box `transform-box` names, `(x, y, width, height)` in the element's user
/// space: the viewport (`view-box`, the initial value), the box of its geometry
/// (`fill-box`, and `content-box`) or that box with the stroke (`stroke-box`, and
/// `border-box`).
fn reference_box(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    style: &ComputedStyle,
    parent: Option<&ComputedStyle>,
    view: (f32, f32),
) -> (f64, f64, f64, f64) {
    use florui_style::TransformBox as B;
    let measured = |stroke: bool| {
        let bounds = element_bounds(arena, styles, node, parent, view);
        let (x, y, w, h) =
            bounds.map_or(
                (0.0, 0.0, 0.0, 0.0),
                |b| {
                    if stroke { b.stroke } else { b.fill }
                },
            );
        (f64::from(x), f64::from(y), f64::from(w), f64::from(h))
    };
    match style.transform_box {
        B::View => (0.0, 0.0, f64::from(view.0), f64::from(view.1)),
        B::Fill | B::Content => measured(false),
        B::Stroke | B::Border => measured(true),
    }
}

/// What an element of the SVG draws takes up, measured on its own: the element
/// written without its own transform, with what it inherits, drawn by the renderer
/// in a document of the size of the viewport.
fn element_bounds(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    parent: Option<&ComputedStyle>,
    view: (f32, f32),
) -> Option<florui_assets::ObjectBounds> {
    let mut inner = String::new();
    write_element(arena, styles, node, parent, view, false, &mut inner);
    let carried = arena
        .parent(node)
        .map(|up| carried(arena, styles, up))
        .unwrap_or_default();
    let text = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" \
         width=\"{}\" height=\"{}\"><g style=\"{}\">{inner}</g></svg>",
        view.0.max(1.0),
        view.1.max(1.0),
        escape(&carried)
    );
    crate::render::object_bounds(&text)
}

/// The transform CSS gives an SVG element, as the value of its `transform`
/// attribute, which is what the SVG renderer reads: its `transform`, `translate`,
/// `rotate` and `scale` about the origin of its `transform-box`. A third
/// dimension is drawn as Chrome draws it, flat (see
/// [`florui_style::Matrix4::flattened_affine`]). The origin is `0 0` of the box
/// when none was given: the style sheet gives that to the elements of an SVG.
fn transform_attribute(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    style: &ComputedStyle,
    parent: Option<&ComputedStyle>,
    view: (f32, f32),
) -> Option<String> {
    if !style.has_transform() {
        return None;
    }
    let reference = reference_box(arena, styles, node, style, parent, view);
    let matrix = florui_style::transform_matrix(style, reference, style.transform_origin, 1.0);
    if matrix.is_identity() {
        return None;
    }
    let [a, b, c, d, e, f] = matrix.flattened_affine();
    Some(format!("matrix({a} {b} {c} {d} {e} {f})"))
}

/// The element `node` as markup. `own_transform` is whether what CSS gives it, or
/// its attribute, goes with it.
fn write_element(
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    parent: Option<&ComputedStyle>,
    view: (f32, f32),
    own_transform: bool,
    out: &mut String,
) {
    let tag = arena.tag(node);
    let style = styles.get(&node);
    // The rules' declarations come before the element's own `style` attribute,
    // which wins over them.
    let own = style
        .map(|s| style_declarations(s, parent))
        .unwrap_or_default();
    let transform = style
        .filter(|_| own_transform)
        .and_then(|s| transform_attribute(arena, styles, node, s, parent, view));
    out.push('<');
    out.push_str(tag);
    let mut wrote_style = false;
    for (name, value) in arena.attrs(node) {
        // What CSS gives wins over the attribute, and the attribute of what is
        // measured without its transform does not apply.
        if name == "transform" && (transform.is_some() || !own_transform) {
            continue;
        }
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
    if let Some(transform) = &transform {
        out.push_str(" transform=\"");
        escape_into(transform, out);
        out.push('"');
    }
    out.push('>');
    write_children(arena, styles, node, style.or(parent), view, out);
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
        assert!(defs.starts_with("<g style=\"color:rgba(0,0,0,1);font-size:16px;"));
        assert!(defs.contains("><defs><mask id=\"m\""));
        assert!(defs.contains(
            "<rect width=\"10\" height=\"10\" fill=\"white\" stop-color=\"a&amp;b\"></rect>"
        ));
        assert!(defs.contains("><circle id=\"c\" r=\"1\">"));
    }

    /// The `matrix(...)` an element carries in `defs`, from the first one after
    /// `marker`.
    fn matrix_after(defs: &str, marker: &str) -> [f64; 6] {
        let rest = &defs[defs
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} in {defs}"))..];
        let start = rest.find("transform=\"matrix(").expect("a matrix") + 18;
        let end = start + rest[start..].find(')').expect("its end");
        let numbers: Vec<f64> = rest[start..end]
            .split_whitespace()
            .map(|n| n.parse().expect("a number"))
            .collect();
        numbers.try_into().expect("six numbers")
    }

    fn assert_close(found: [f64; 6], expected: [f64; 6]) {
        for (a, b) in found.iter().zip(expected) {
            assert!((a - b).abs() < 1e-4, "{found:?} against {expected:?}");
        }
    }

    /// A rotation by `degrees` about `(x, y)`, as `matrix()` writes it.
    fn rotation_about(degrees: f64, x: f64, y: f64) -> [f64; 6] {
        let (sin, cos) = degrees.to_radians().sin_cos();
        [
            cos,
            sin,
            -sin,
            cos,
            x - cos * x + sin * y,
            y - sin * x - cos * y,
        ]
    }

    #[test]
    fn a_css_transform_of_an_svg_element_is_its_transform_attribute() {
        let tree: Element = view! {
            <div><svg>
                <rect class="a" transform="rotate(5)" />
                <rect class="b" />
                <rect class="c" />
            </svg></div>
        };
        let arena = Arena::build(&tree);
        let styles = styles_of(
            &arena,
            ".a { transform: translate(20px, 5px) scale(1.5); } \n             .b { transform: rotate(30deg); transform-origin: 25px 25px; } \n             .c { transform: translate(50%, 0) rotateX(20deg); }",
        );
        let defs = reference(&arena, &styles, "#x").expect("a reference").defs;
        // The default origin of an SVG element is its own corner, and a rule wins
        // over the attribute.
        assert_close(
            matrix_after(&defs, "class=\"a\""),
            [1.5, 0.0, 0.0, 1.5, 20.0, 5.0],
        );
        assert!(!defs.contains("rotate(5)"), "{defs}");
        assert_close(
            matrix_after(&defs, "class=\"b\""),
            rotation_about(30.0, 25.0, 25.0),
        );
        // A percentage is of the viewport (300 by 150 here), and a rotation about x
        // shows as the squeeze of the plane it leaves: the third dimension is
        // projected away, as Chrome draws it.
        let cos = 20.0f64.to_radians().cos();
        assert_close(
            matrix_after(&defs, "class=\"c\""),
            [1.0, 0.0, 0.0, cos, 150.0, 0.0],
        );
    }

    #[test]
    fn the_transform_box_of_an_svg_element_is_the_box_of_its_geometry_or_its_stroke() {
        let tree: Element = view! {
            <div><svg>
                <rect class="r f" x="60" y="40" width="40" height="30" />
                <rect class="r s" x="60" y="40" width="40" height="30" stroke="blue" stroke-width="20" />
                <g class="g"><rect x="60" y="40" width="20" height="20" /><rect x="90" y="50" width="20" height="20" /></g>
                <rect class="r v" x="60" y="40" width="40" height="30" />
            </svg></div>
        };
        let arena = Arena::build(&tree);
        let styles = styles_of(
            &arena,
            ".f { transform: rotate(45deg); transform-box: fill-box; transform-origin: center; } \n\
             .s { transform: rotate(45deg); transform-box: stroke-box; transform-origin: left top; } \n\
             .g { transform: scale(2); transform-box: fill-box; transform-origin: center; } \n\
             .v { transform: rotate(45deg); transform-box: view-box; transform-origin: 50% 50%; }",
        );
        let defs = reference(&arena, &styles, "#x").expect("a reference").defs;
        assert_close(
            matrix_after(&defs, "class=\"r f\""),
            rotation_about(45.0, 80.0, 55.0),
        );
        // The stroke of 20 widens the box by 10 each way: it starts at (50, 30).
        assert_close(
            matrix_after(&defs, "class=\"r s\""),
            rotation_about(45.0, 50.0, 30.0),
        );
        // A group is the union of what is in it, (60, 40) to (110, 70).
        assert_close(
            matrix_after(&defs, "class=\"g\""),
            [2.0, 0.0, 0.0, 2.0, -85.0, -55.0],
        );
        // The viewport is 300 by 150.
        assert_close(
            matrix_after(&defs, "class=\"r v\""),
            rotation_about(45.0, 150.0, 75.0),
        );
    }

    #[test]
    fn a_perspective_on_an_svg_element_drops_its_row_as_chrome_does() {
        let tree: Element = view! {
            <div><svg>
                <rect class="p" x="40" y="25" width="40" height="30" />
                <rect class="z" x="60" y="30" width="80" height="60" />
            </svg></div>
        };
        let arena = Arena::build(&tree);
        let styles = styles_of(
            &arena,
            ".p { transform: perspective(100px) rotateY(40deg); transform-box: fill-box; transform-origin: center; } \n\
             .z { transform: perspective(100px) translateZ(30px); transform-box: fill-box; transform-origin: center; }",
        );
        let defs = reference(&arena, &styles, "#x").expect("a reference").defs;
        // Measured on Chrome 153: the entries of the matrix with the origin folded
        // in, not divided by the perspective. The first scales x by cos(40) plus
        // the 60 of the origin times the 0.00643 the perspective gives x, and
        // shears y by the 40 of the origin times the same.
        let (sin, cos) = 40.0f64.to_radians().sin_cos();
        let k = sin / 100.0;
        let [a, b, c, d, _, _] = matrix_after(&defs, "class=\"p\"");
        assert!((a - (cos + 60.0 * k)).abs() < 1e-4, "{a}");
        assert!((b - 40.0 * k).abs() < 1e-4, "{b}");
        assert!(c.abs() < 1e-9 && (d - 1.0).abs() < 1e-9);
        // A translation in depth moves the box by 0.3 of its origin and does not
        // scale it: (-30, -18) for the origin (100, 60).
        assert_close(
            matrix_after(&defs, "class=\"z\""),
            [1.0, 0.0, 0.0, 1.0, -30.0, -18.0],
        );
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
        let wrapper = &defs[..defs.find('>').expect("a tag")];
        assert!(wrapper.contains("color:rgba(255,0,0,1)"), "{wrapper}");
        assert!(
            wrapper.ends_with("fill:green;stroke-width:3\""),
            "{wrapper}"
        );
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
