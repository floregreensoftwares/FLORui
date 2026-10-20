//! What the layout tests of one display value or feature at a time share: a
//! tree built from a few helpers, laid out with the embedded font, and boxes
//! read back in absolute coordinates.

use std::collections::HashMap;

use florui::prelude::Element;
use florui_style::{Arena, InteractionState, NodeId};
use taffy::prelude::TaffyMaxContent;

use crate::{BoxLayout, absolute_position, compute_layout};

pub(crate) fn element(tag: &'static str, class: &str, children: Vec<Element>) -> Element {
    let attrs = if class.is_empty() {
        Vec::new()
    } else {
        vec![("class".to_string(), class.to_string())]
    };
    Element::node(tag, attrs, children)
}

pub(crate) fn div(class: &str, children: Vec<Element>) -> Element {
    element("div", class, children)
}

pub(crate) fn span(class: &str, children: Vec<Element>) -> Element {
    element("span", class, children)
}

pub(crate) fn text(content: &str) -> Element {
    Element::text(content)
}

pub(crate) struct Laid {
    pub(crate) arena: Arena,
    pub(crate) layouts: HashMap<NodeId, BoxLayout>,
}

/// The nodes with a class, in document order, by class name.
impl Laid {
    pub(crate) fn node(&self, class: &str) -> NodeId {
        let found = self.arena.find_all(|arena, id| {
            arena
                .attr(id, "class")
                .is_some_and(|value| value.split_whitespace().any(|name| name == class))
        });
        match found.first() {
            Some(&id) => id,
            None => panic!("no node has the class {class}"),
        }
    }

    /// The border box of the first node with this class: `(x, y, width,
    /// height)` from the layout root.
    pub(crate) fn rect(&self, class: &str) -> (f32, f32, f32, f32) {
        let node = self.node(class);
        let (x, y) = absolute_position(&self.arena, &self.layouts, node);
        let layout = self.layouts[&node];
        (x, y, layout.width, layout.height)
    }
}

pub(crate) fn lay_out(tree: &Element, css: &str) -> Laid {
    lay_out_in(tree, css, taffy::Size::MAX_CONTENT)
}

pub(crate) fn lay_out_in(
    tree: &Element,
    css: &str,
    available: taffy::Size<taffy::AvailableSpace>,
) -> Laid {
    let arena = Arena::build(tree);
    let rules = match florui_style::parse_stylesheet(css) {
        Ok(rules) => rules,
        Err(error) => panic!("the test stylesheet does not parse: {error:?}"),
    };
    let styles = florui_style::compute(
        &arena,
        &rules,
        &InteractionState::new(),
        florui_style::Viewport::default(),
        &mut florui_style::AnimationTimeline::default(),
    );
    let mut font = florui_text::Font::load_embedded();
    let layouts = match compute_layout(&mut font, &arena, &styles, available) {
        Ok(layouts) => layouts,
        Err(error) => panic!("layout failed: {error}"),
    };
    Laid { arena, layouts }
}

/// `actual` is within `tolerance` of `expected`.
#[track_caller]
pub(crate) fn near(actual: f32, expected: f32, tolerance: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not within {tolerance} of {expected}"
    );
}

/// A rectangle `(x, y, width, height)` is within `tolerance` of `expected`
/// on every side.
#[track_caller]
pub(crate) fn rect_near(actual: (f32, f32, f32, f32), expected: (f32, f32, f32, f32), what: &str) {
    for (index, name) in ["x", "y", "width", "height"].into_iter().enumerate() {
        let (a, e) = [
            (actual.0, expected.0),
            (actual.1, expected.1),
            (actual.2, expected.2),
            (actual.3, expected.3),
        ][index];
        near(a, e, 0.5, &format!("{what} {name}"));
    }
}
