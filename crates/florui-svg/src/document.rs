//! Finding what a `url()` points at, and writing it as SVG markup.

use florui_style::{NodeId, Tree};

/// The address a stylesheet's relative `url(#id)` resolves to.
const BASE: &str = "about:florui";

/// What a `url()` of a mask, clip or filter names: the markup of every definition
/// that may be needed (the content of the document's inline SVGs, or of the file)
/// and the `id` of the one that is applied.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reference {
    pub id: String,
    pub defs: String,
}

/// The file (none for this document) and the id in a `url()` with a fragment.
fn split(url: &str) -> Option<(Option<&str>, &str)> {
    let url = url.trim();
    let url = url.strip_prefix(BASE).unwrap_or(url);
    let (file, id) = url.split_once('#')?;
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

impl Reference {
    /// Whether the definitions hold an element `tag` with this reference's `id`.
    pub fn defines(&self, tag: &str) -> bool {
        crate::filter::start_tag(&self.defs, tag, &self.id).is_some()
    }
}

/// The reference `url` makes, with the definitions it can use; `None` when it is
/// not one, or its file cannot be read.
pub fn reference(arena: &Tree, url: &str) -> Option<Reference> {
    let (file, id) = split(url)?;
    let defs = match file {
        None => document_defs(arena),
        Some(path) => file_defs(path)?,
    };
    Some(Reference {
        id: id.to_owned(),
        defs,
    })
}

/// The content of every inline `<svg>` that is not inside another, written as
/// markup: ids are those of the whole document, as in a browser.
fn document_defs(arena: &Tree) -> String {
    let mut out = String::new();
    let tops =
        arena.find_all(|tree, node| tree.tag(node) == "svg" && !has_svg_ancestor(tree, node));
    for node in tops {
        for &child in arena.children(node) {
            write_element(arena, child, &mut out);
        }
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

fn write_element(arena: &Tree, node: NodeId, out: &mut String) {
    let tag = arena.tag(node);
    out.push('<');
    out.push_str(tag);
    for (name, value) in arena.attrs(node) {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        escape_into(value, out);
        out.push('"');
    }
    out.push('>');
    for &child in arena.children(node) {
        write_element(arena, child, out);
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
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

/// What is inside the root `<svg>` of a file.
fn file_defs(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let open = text.find("<svg")?;
    let start = open + text[open..].find('>')? + 1;
    let end = text.rfind("</svg>")?;
    (start <= end).then(|| text[start..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use florui::prelude::*;
    use florui_style::Arena;

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
        let found = reference(&arena, "about:florui#m").expect("a reference");
        assert_eq!(found.id, "m");
        assert_eq!(
            found.defs,
            "<defs><mask id=\"m\" maskUnits=\"userSpaceOnUse\"><rect width=\"10\" height=\"10\" \
             fill=\"white\" stop-color=\"a&amp;b\"></rect></mask></defs><circle id=\"c\" r=\"1\"></circle>"
        );
    }
}
