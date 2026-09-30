//! The message bubble shown under the first invalid control when a submit
//! is blocked: an overlay in the tree, restyled by class.
//!
//! Measured in Edge: the box sits 8px from the field, centred and kept in
//! the window, flipping above it when there is no room below; the arrow's
//! tip touches the field, 17px in from the box's left edge but never less
//! than 7px in from the field's.

use std::collections::HashMap;

use florui::Element;
use florui_layout::BoxLayout;
use florui_style::{Arena, NodeId};

pub(crate) const BUBBLE_ID: &str = "florui-validation-bubble";
pub const BUBBLE_CLASS: &str = "florui-validation-bubble";
pub const BUBBLE_ICON_CLASS: &str = "florui-validation-bubble-icon";
pub const BUBBLE_ICON_STEM_CLASS: &str = "florui-validation-bubble-stem";
pub const BUBBLE_ICON_DOT_CLASS: &str = "florui-validation-bubble-dot";
pub const BUBBLE_TEXT_CLASS: &str = "florui-validation-bubble-text";
pub const BUBBLE_ARROW_CLASS: &str = "florui-validation-bubble-arrow";
pub const BUBBLE_ARROW_BELOW_CLASS: &str = "florui-validation-bubble-arrow-below";

/// Gap between the field's bottom edge and the box's top edge.
const GAP: f32 = 8.0;
/// Horizontal space the default stylesheet spends around the text: box
/// padding and border, the icon and its margin.
const TEXT_CHROME: f32 = 50.0;
/// The default text width limit, before the window narrows it.
const TEXT_MAX_WIDTH: f32 = 317.0;
const ARROW_FROM_BOX_LEFT: f32 = 17.0;
const ARROW_MIN_FROM_FIELD_LEFT: f32 = 7.0;

fn node(classes: &str, children: Vec<Element>) -> Element {
    Element::node(
        "div",
        vec![("class".to_string(), classes.to_string())],
        children,
    )
}

/// The text size the default stylesheet gives the message, for measuring it.
pub(crate) const TEXT_FONT_SIZE: f32 = 14.0;

/// The width the message text may take: a field near the window's right edge
/// gets a narrow box that wraps (Edge: 217px across for a field 220px from
/// the edge).
pub(crate) fn max_text_width(field_left: f32, viewport_width: f32) -> f32 {
    (viewport_width - field_left - 3.0 - TEXT_CHROME).clamp(0.0, TEXT_MAX_WIDTH)
}

/// The bubble overlay carrying `message`; `wrap_width` fixes the text's width
/// so a message longer than that wraps and the box hugs it.
pub(crate) fn element(message: &str, wrap_width: Option<f32>) -> Element {
    let icon = node(
        BUBBLE_ICON_CLASS,
        vec![
            node(BUBBLE_ICON_STEM_CLASS, vec![]),
            node(BUBBLE_ICON_DOT_CLASS, vec![]),
        ],
    );
    let mut text_attrs = vec![("class".to_string(), BUBBLE_TEXT_CLASS.to_string())];
    if let Some(width) = wrap_width {
        text_attrs.push(("style".to_string(), format!("width: {width}px")));
    }
    let text = Element::node("div", text_attrs, vec![Element::text(message)]);
    let arrow = node(BUBBLE_ARROW_CLASS, vec![]);
    let arrow_below = node(BUBBLE_ARROW_BELOW_CLASS, vec![]);
    let attrs = vec![
        ("id".to_string(), BUBBLE_ID.to_string()),
        ("class".to_string(), BUBBLE_CLASS.to_string()),
    ];
    let root = Element::node("div", attrs, vec![icon, text, arrow, arrow_below]);
    Element::Portal(vec![root])
}

/// Where the box and its arrow go for a field at `field`, once layout has
/// given both their sizes: patches their layouts in place, as an open
/// select's dropdown is. `viewport` is the window's size. The box flips
/// above the field when it doesn't fit below and does above.
pub(crate) fn place(
    arena: &Arena,
    layouts: &mut HashMap<NodeId, BoxLayout>,
    field: NodeId,
    viewport: (f32, f32),
) {
    let Some(root) = arena.find(|arena, id| arena.id_attr(id) == Some(BUBBLE_ID)) else {
        return;
    };
    let child_with = |class: &str| {
        arena
            .children(root)
            .iter()
            .copied()
            .find(|&child| arena.classes(child).iter().any(|c| c == class))
    };
    let (Some(arrow_above), Some(arrow_below)) = (
        child_with(BUBBLE_ARROW_CLASS),
        child_with(BUBBLE_ARROW_BELOW_CLASS),
    ) else {
        return;
    };
    let (Some(&field_box), Some(&root_box)) = (layouts.get(&field), layouts.get(&root)) else {
        return;
    };
    let (fx, fy) = florui_layout::absolute_position(arena, layouts, field);
    let left = (fx + field_box.width / 2.0 - root_box.width / 2.0)
        .min(viewport.0 - root_box.width)
        .max(0.0)
        .round();
    let below = (fy.round() + field_box.height.round() + GAP).round();
    let height = root_box.height.round();
    let above = fy.round() - GAP - height;
    let flipped = below + height > viewport.1 && above >= 0.0;
    let (arrow, hidden) = if flipped {
        (arrow_below, arrow_above)
    } else {
        (arrow_above, arrow_below)
    };
    let Some(&arrow_box) = layouts.get(&arrow) else {
        return;
    };
    let centre = (left + ARROW_FROM_BOX_LEFT)
        .max(fx + ARROW_MIN_FROM_FIELD_LEFT)
        .min(left + root_box.width - arrow_box.width);
    if let Some(layout) = layouts.get_mut(&root) {
        layout.x = left;
        layout.y = if flipped { above } else { below };
        layout.height = height;
    }
    if let Some(layout) = layouts.get_mut(&arrow) {
        // Centred on the border line it points out of, so the rotated
        // square's outer half is the arrow and its inner half sits inside.
        layout.x = centre - left - arrow_box.width / 2.0;
        layout.y = if flipped {
            height - 0.5 - arrow_box.height / 2.0
        } else {
            0.5 - arrow_box.height / 2.0
        };
    }
    if let Some(layout) = layouts.get_mut(&hidden) {
        layout.width = 0.0;
        layout.height = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::max_text_width;

    #[test]
    fn a_field_near_the_right_edge_gets_a_narrow_box() {
        assert_eq!(max_text_width(516.0, 736.0), 167.0);
        assert_eq!(max_text_width(100.0, 736.0), 317.0);
        assert_eq!(max_text_width(900.0, 736.0), 0.0);
    }
}
