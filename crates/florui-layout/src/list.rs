//! List items: the number each is given by the `list-item` counter, and what its
//! marker is. The markers are placed by [`crate::list_marker`] once the boxes are.

use std::collections::HashMap;

use florui_style::{Display, NodeId, StyleMap, Tree};

/// One counter of a name, which lives for as long as its scope: the children of the
/// node `scope`, and what is in them.
struct Counter {
    value: i64,
    scope: NodeId,
}

#[derive(Default)]
struct Counters {
    by_name: HashMap<String, Vec<Counter>>,
    /// What each node's children made, to end with the node.
    made: HashMap<NodeId, Vec<String>>,
}

impl Counters {
    fn instantiate(&mut self, name: &str, value: i64, scope: NodeId) {
        let stack = self.by_name.entry(name.to_owned()).or_default();
        // One a sibling made is replaced, not nested in.
        if let Some(top) = stack.last_mut()
            && top.scope == scope
        {
            top.value = value;
            return;
        }
        stack.push(Counter { value, scope });
        self.made.entry(scope).or_default().push(name.to_owned());
    }

    /// The innermost counter of `name`, made on the spot when there is none.
    fn innermost(&mut self, name: &str, scope: NodeId) -> &mut Counter {
        if self.by_name.get(name).is_none_or(Vec::is_empty) {
            self.instantiate(name, 0, scope);
        }
        match self
            .by_name
            .get_mut(name)
            .and_then(|stack| stack.last_mut())
        {
            Some(counter) => counter,
            None => unreachable!("the counter was just made"),
        }
    }

    fn end(&mut self, scope: NodeId) {
        for name in self.made.remove(&scope).unwrap_or_default() {
            if let Some(stack) = self.by_name.get_mut(&name) {
                stack.pop();
            }
        }
    }
}

fn integer_attr(arena: &Tree, node: NodeId, name: &str) -> Option<i64> {
    arena
        .attr(node, name)
        .and_then(|value| value.trim().parse::<i64>().ok())
}

/// A scope for nodes with no parent.
const ROOT: NodeId = usize::MAX;

/// The number the marker of each list item has: the value of the `list-item` counter
/// there, and what the other counters do on the way (`counter-reset`,
/// `counter-increment`, `counter-set`, `<ol start reversed>`, `<li value>`).
pub(crate) fn numbering(arena: &Tree, styles: &StyleMap) -> HashMap<NodeId, i64> {
    let mut numbers = HashMap::new();
    if !styles
        .values()
        .any(|style| style.display == Display::ListItem)
    {
        return numbers;
    }
    let mut counters = Counters::default();
    // The nodes to enter (`true`) and to leave (`false`), in a depth-first walk.
    let mut stack: Vec<(NodeId, bool)> = arena
        .roots()
        .iter()
        .rev()
        .map(|&root| (root, true))
        .collect();
    while let Some((node, entering)) = stack.pop() {
        let scope = arena.parent(node).unwrap_or(ROOT);
        if !entering {
            counters.end(node);
            continue;
        }
        let Some(style) = styles.get(&node) else {
            continue;
        };
        if style.display == Display::None {
            continue;
        }
        stack.push((node, false));
        stack.extend(
            crate::flow_children(arena, styles, node)
                .iter()
                .rev()
                .map(|&child| (child, true)),
        );

        // What the element does to the counters: the ones it states, in that order, and
        // what its list and its number say.
        let tag = arena.tag(node);
        let names_list_item =
            |list: &[(String, i32)]| list.iter().any(|(name, _)| name == "list-item");
        let mut resets: Vec<(String, i64)> = style
            .counter_reset
            .iter()
            .map(|(name, value)| (name.clone(), i64::from(*value)))
            .collect();
        if matches!(tag, "ul" | "menu" | "dir") && !names_list_item(&style.counter_reset) {
            resets.push(("list-item".to_owned(), 0));
        }
        if tag == "ol" && !names_list_item(&style.counter_reset) {
            let reversed = arena.attr(node, "reversed").is_some();
            let start = integer_attr(arena, node, "start");
            let initial = if reversed {
                let items = crate::flow_children(arena, styles, node)
                    .iter()
                    .filter(|&&child| {
                        styles
                            .get(&child)
                            .is_some_and(|s| s.display == Display::ListItem)
                    })
                    .count() as i64;
                start.unwrap_or(items) + 1
            } else {
                start.unwrap_or(1) - 1
            };
            resets.push(("list-item".to_owned(), initial));
        }
        for (name, value) in resets {
            counters.instantiate(&name, value, scope);
        }
        let mut increments: Vec<(String, i64)> = style
            .counter_increment
            .iter()
            .map(|(name, value)| (name.clone(), i64::from(*value)))
            .collect();
        let item = style.display == Display::ListItem;
        if item && !names_list_item(&style.counter_increment) {
            let in_reversed = arena.parent(node).is_some_and(|parent| {
                arena.tag(parent) == "ol" && arena.attr(parent, "reversed").is_some()
            });
            increments.push(("list-item".to_owned(), if in_reversed { -1 } else { 1 }));
        }
        for (name, by) in increments {
            counters.innermost(&name, scope).value += by;
        }
        for (name, value) in &style.counter_set {
            counters.innermost(name, scope).value = i64::from(*value);
        }
        if item
            && !names_list_item(&style.counter_set)
            && let Some(value) = integer_attr(arena, node, "value")
        {
            counters.innermost("list-item", scope).value = value;
        }
        if item {
            let value = counters.innermost("list-item", scope).value;
            numbers.insert(node, value);
        }
    }
    numbers
}

#[cfg(test)]
mod tests {
    use florui::prelude::Element;
    use florui_style::{Arena, InteractionState};

    use super::numbering;
    use crate::test_support::{div, element, text};

    fn with(attrs: &[(&str, &str)], tag: &'static str, children: Vec<Element>) -> Element {
        Element::node(
            tag,
            attrs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            children,
        )
    }

    fn numbers(tree: &Element, css: &str) -> Vec<i64> {
        let arena = Arena::build(tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let numbered = numbering(&arena, &styles);
        arena
            .find_all(|a, id| a.tag(id) == "li")
            .into_iter()
            .map(|id| numbered.get(&id).copied().unwrap_or(i64::MIN))
            .collect()
    }

    fn li() -> Element {
        element("li", "", vec![text("x")])
    }

    #[test]
    fn a_list_counts_from_one_and_a_nested_list_has_its_own_count() {
        let tree = element(
            "ol",
            "",
            vec![
                li(),
                element("li", "", vec![element("ol", "", vec![li(), li()])]),
                li(),
            ],
        );
        assert_eq!(numbers(&tree, ""), vec![1, 2, 1, 2, 3]);
    }

    #[test]
    fn start_reversed_and_value_set_the_numbers_of_an_ordered_list() {
        let tree = with(&[("start", "5")], "ol", vec![li(), li()]);
        assert_eq!(numbers(&tree, ""), vec![5, 6]);
        let tree = with(&[("reversed", "")], "ol", vec![li(), li(), li()]);
        assert_eq!(numbers(&tree, ""), vec![3, 2, 1]);
        let tree = with(&[("reversed", ""), ("start", "10")], "ol", vec![li(), li()]);
        assert_eq!(numbers(&tree, ""), vec![10, 9]);
        let tree = element(
            "ol",
            "",
            vec![li(), with(&[("value", "7")], "li", vec![text("x")]), li()],
        );
        assert_eq!(numbers(&tree, ""), vec![1, 7, 8]);
    }

    #[test]
    fn the_counter_properties_change_what_the_list_item_counter_does() {
        let tree = element(
            "ul",
            "",
            vec![
                element("li", "a", vec![text("x")]),
                element("li", "b", vec![text("x")]),
                element("li", "", vec![text("x")]),
            ],
        );
        let css = ".a { counter-increment: list-item 4; } .b { counter-set: list-item 20; }";
        assert_eq!(numbers(&tree, css), vec![4, 20, 21]);
        let css = "ul { counter-reset: list-item 9; }";
        assert_eq!(numbers(&tree, css), vec![10, 11, 12]);
    }

    #[test]
    fn a_list_item_that_is_in_no_list_counts_with_its_siblings() {
        let tree = div("", vec![li(), li()]);
        assert_eq!(numbers(&tree, ""), vec![1, 2]);
    }
}

/// What a list item is marked with, drawn as a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerShape {
    Disc,
    Circle,
    Square,
}

/// What a list item is marked with.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkerKind {
    Shape(MarkerShape),
    /// Text in the font of the list item: a number and what follows it, or a string.
    Text(String),
    /// The image `list-style-image` names, which stands for the type when it cannot be
    /// read.
    Image(florui_style::BackgroundImage),
}

/// The marker of a list item numbered `number`, if it has one.
pub fn marker_of(style: &florui_style::ComputedStyle, number: i64) -> Option<MarkerKind> {
    use florui_style::ListStyleType;
    if let Some(image) = &style.list_style_image {
        return Some(MarkerKind::Image(image.clone()));
    }
    Some(match &style.list_style_type {
        ListStyleType::None => return None,
        ListStyleType::Disc => MarkerKind::Shape(MarkerShape::Disc),
        ListStyleType::Circle => MarkerKind::Shape(MarkerShape::Circle),
        ListStyleType::Square => MarkerKind::Shape(MarkerShape::Square),
        // Counter styles of one symbol, written in the font like any text.
        ListStyleType::DisclosureOpen => MarkerKind::Text("\u{25BE} ".to_owned()),
        ListStyleType::DisclosureClosed => MarkerKind::Text("\u{25B8} ".to_owned()),
        ListStyleType::Counter(name) => {
            let counted = crate::counter_style::format(name, number);
            MarkerKind::Text(format!("{}{}", counted.text, counted.suffix))
        }
        ListStyleType::String(text) => MarkerKind::Text(text.clone()),
    })
}

/// How a shape marker is laid out for a font whose ascent is `ascent` (a whole number
/// of pixels), in CSS pixels, with `x` from where the content box of the list item
/// starts and `y` from its first baseline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapeMetrics {
    /// The side of the square the shape is in.
    pub size: f32,
    /// Where it is: the left edge and the top.
    pub x: f32,
    pub y: f32,
    /// The room of the marker in a line when it is inside, and the space after it.
    pub advance: f32,
}

/// Chrome 153: the shape is `(ascent * 2 / 3 + 1) / 2` wide (whole numbers) in a box one
/// pixel wider, which ends `ascent * 2 / 3 + 8` before the content box; inside, it is
/// followed by a space of the width of the font size; its top is `3 * (ascent -
/// ascent * 2 / 3) / 2` below the top of the font on the line, which is the ascent
/// above the baseline.
pub fn shape_metrics(ascent: f32, font_size: f32) -> ShapeMetrics {
    let ascent = ascent as i32;
    let offset = ascent * 2 / 3;
    let size = (offset + 1) / 2;
    ShapeMetrics {
        size: size as f32,
        x: -((offset + 8) as f32) + 1.0,
        y: (3 * (ascent - offset) / 2 - ascent) as f32,
        advance: size as f32 + 1.0 + font_size,
    }
}

/// How far an image marker is from the content box when it is outside: its width
/// and the gap of 7px.
pub const IMAGE_GAP: f32 = 7.0;

#[cfg(test)]
mod marker_tests {
    use crate::test_support::*;

    use super::{MarkerKind, MarkerShape, marker_of, shape_metrics};

    fn list(css: &str) -> Laid {
        let tree = div(
            "p",
            vec![element(
                "ul",
                "u",
                vec![
                    element("li", "i", vec![text("Hello")]),
                    element("li", "j", vec![text("World")]),
                ],
            )],
        );
        lay_out(
            &tree,
            &format!(".p {{ width: 300px; }} .u {{ margin: 0; padding: 0 0 0 40px; }} {css}"),
        )
    }

    #[test]
    fn a_list_item_reports_what_its_marker_is_placed_by() {
        let laid = list("");
        let first = crate::marker_anchor(&laid.layouts, laid.node("i"), 1.0).unwrap();
        let second = crate::marker_anchor(&laid.layouts, laid.node("j"), 1.0).unwrap();
        // Chrome 153: the first line of Open Sans 16px has its baseline 17px down.
        assert_eq!(first.baseline, Some(17.0));
        assert_eq!((first.number, second.number), (1, 2));
        assert_eq!(first.content_left, 0.0);
        // None for a list item that has no marker.
        let laid = list(".u { list-style-type: none; }");
        assert!(crate::marker_anchor(&laid.layouts, laid.node("i"), 1.0).is_none());
    }

    #[test]
    fn the_shape_of_a_marker_has_the_size_and_the_place_chrome_gives_it() {
        // 16px Open Sans, ascent 17: 6px wide, 18px before the content box less one.
        let metrics = shape_metrics(17.0, 16.0);
        assert_eq!(metrics.size, 6.0);
        assert_eq!(metrics.x, -18.0);
        assert_eq!(metrics.y, -8.0);
        assert_eq!(metrics.advance, 23.0);
        // 32px, ascent 34.
        let metrics = shape_metrics(34.0, 32.0);
        assert_eq!((metrics.size, metrics.y), (11.0, -16.0));
    }

    #[test]
    fn a_marker_inside_takes_room_in_the_first_line() {
        let laid = list(".u { list-style-position: inside; }");
        let (x, ..) = laid.rect("i");
        let mut font = florui_text::Font::load_embedded();
        let shaped = crate::shape_inline_formatting_context(
            &mut font,
            &laid.arena,
            &laid.styles,
            (&laid.layouts, 1.0),
            laid.node("i"),
            260.0,
        )
        .unwrap();
        let first = shaped
            .layout
            .runs
            .iter()
            .filter(|run| run.line == 0)
            .map(|run| run.x)
            .fold(f32::MAX, f32::min);
        // Chrome: the text starts 23px in after a disc.
        assert!((first - 23.0).abs() < 0.01, "{first}");
        assert_eq!(x, 40.0);
        // The item with an inside marker is as tall as its line.
        assert_eq!(laid.rect("i").3, 22.0);
        assert_eq!(laid.rect("p").3, 44.0);
    }

    #[test]
    fn markers_are_shapes_text_or_images() {
        let laid = list(
            ".i { list-style-type: lower-roman; } .j { list-style-type: \"> \"; } \n             .u { list-style-image: none; }",
        );
        let style = |class: &str| laid.styles.get(&laid.node(class)).unwrap().clone();
        assert_eq!(
            marker_of(&style("i"), 4),
            Some(MarkerKind::Text("iv. ".into()))
        );
        assert_eq!(
            marker_of(&style("j"), 4),
            Some(MarkerKind::Text("> ".into()))
        );
        let laid = list("");
        let item = laid.styles.get(&laid.node("i")).unwrap();
        assert_eq!(
            marker_of(item, 1),
            Some(MarkerKind::Shape(MarkerShape::Disc))
        );
        let laid = list(".u { list-style-type: none; }");
        assert_eq!(
            marker_of(laid.styles.get(&laid.node("i")).unwrap(), 1),
            None
        );
    }
}
