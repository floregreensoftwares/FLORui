//! Keeps a resized `<textarea>`'s size, as a browser's inline `width`/
//! `height` does: the runtime remembers the content-box size per `id` and
//! this stamps it after the author's own `style`, so it wins.

use std::collections::HashMap;

use florui::Element;

/// The smallest content box a drag can shrink a textarea to.
pub(crate) const MIN_CONTENT_SIZE: (f32, f32) = (30.0, 20.0);

/// Stamps every remembered size in `sizes` onto its textarea in `tree`.
pub(crate) fn apply(tree: &mut Element, sizes: &HashMap<String, (f32, f32)>) {
    if sizes.is_empty() {
        return;
    }
    visit(std::slice::from_mut(tree), sizes);
}

fn visit(elements: &mut [Element], sizes: &HashMap<String, (f32, f32)>) {
    for element in elements {
        match element {
            Element::Node(node) => {
                if node.tag == "textarea"
                    && let Some(&(width, height)) = node
                        .attrs
                        .iter()
                        .find(|(name, _)| name == "id")
                        .and_then(|(_, id)| sizes.get(id))
                {
                    let stamp = format!("width: {width}px; height: {height}px;");
                    match node.attrs.iter_mut().find(|(name, _)| name == "style") {
                        Some((_, style)) => {
                            style.push(' ');
                            style.push_str(&stamp);
                        }
                        None => node.attrs.push(("style".to_string(), stamp)),
                    }
                }
                visit(&mut node.children, sizes);
            }
            Element::Fragment(children) | Element::Portal(children) => visit(children, sizes),
            Element::Text(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;

    use super::*;

    fn style_of(tree: &Element, id: &str) -> Option<String> {
        let arena = florui_style::Arena::build(tree);
        let node = arena.find(|a, n| a.id_attr(n) == Some(id))?;
        arena.attr(node, "style").map(str::to_string)
    }

    #[test]
    fn a_remembered_size_lands_after_the_authors_own_style_and_only_on_its_textarea() {
        let mut tree: Element = view! {
            <div>
                <textarea id="a" style="color: red;"></textarea>
                <textarea id="b"></textarea>
                <textarea id="c" style="color: blue;"></textarea>
            </div>
        };
        let sizes = HashMap::from([
            ("a".to_string(), (120.0, 60.0)),
            ("b".to_string(), (80.0, 40.0)),
        ]);
        apply(&mut tree, &sizes);
        assert_eq!(
            style_of(&tree, "a").as_deref(),
            Some("color: red; width: 120px; height: 60px;")
        );
        assert_eq!(
            style_of(&tree, "b").as_deref(),
            Some("width: 80px; height: 40px;")
        );
        assert_eq!(style_of(&tree, "c").as_deref(), Some("color: blue;"));
    }
}
