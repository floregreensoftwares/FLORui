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
    if !styles.may_have_list_items()
        || !styles
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
        ListStyleType::Custom(style) => {
            let counted = crate::counter_style_custom::format_custom(style, number);
            MarkerKind::Text(format!("{}{}", counted.text, counted.suffix))
        }
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

/// What `list-style-image` is drawn as: the box it fills, which stands on the baseline of
/// the first line. A `url()` that cannot be read has no height and a width that depends
/// only on the font, as in Chrome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageMarkerSize {
    pub width: f32,
    pub height: f32,
    pub broken: bool,
}

/// The size of the image `image` as a marker for a font whose ascent is `ascent` (a whole
/// number of pixels). An image with sides of its own has them; one that has none is a
/// square of half the ascent, a side or a ratio it has fills in the rest of it.
pub fn image_marker_size(image: &florui_style::BackgroundImage, ascent: f32) -> ImageMarkerSize {
    use florui_assets::url_image::Natural;
    let side = ascent / 2.0;
    let sized = |width, height| ImageMarkerSize {
        width,
        height,
        broken: false,
    };
    let florui_style::BackgroundImage::Url(url) = image else {
        // A gradient has no size of its own.
        return sized(side, side);
    };
    let Some(loaded) = florui_assets::url_image::load(url) else {
        return ImageMarkerSize {
            width: shape_metrics(ascent, 0.0).size + 2.0,
            height: 0.0,
            broken: true,
        };
    };
    let Natural {
        width,
        height,
        ratio,
    } = loaded.natural;
    match (width, height, ratio) {
        (Some(width), Some(height), _) => sized(width, height),
        (Some(width), None, Some(ratio)) => sized(width, width / ratio),
        (Some(width), None, None) => sized(width, side),
        (None, Some(height), Some(ratio)) => sized(height * ratio, height),
        (None, Some(height), None) => sized(side, height),
        // Contained by the square.
        (None, None, Some(ratio)) if ratio >= 1.0 => sized(side, side / ratio),
        (None, None, Some(ratio)) => sized(side * ratio, side),
        (None, None, None) => sized(side, side),
    }
}

#[cfg(test)]
mod marker_tests {
    use crate::test_support::*;

    use florui::prelude::Element;

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

    /// A box of `width` x `height` as the `list-style-image` of the item.
    fn image(width: u32, height: u32) -> String {
        format!(
            "list-style-image: url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}'%3E%3Crect width='{width}' height='{height}'/%3E%3C/svg%3E\");"
        )
    }

    /// The list of one item of `content`, in 16px Open Sans, laid out with `css` for its item.
    fn image_item(content: Vec<Element>, css: &str) -> Laid {
        image_item_with(content, css, "")
    }

    /// [`image_item`], with `more` rules for the rest of the list.
    fn image_item_with(content: Vec<Element>, css: &str, more: &str) -> Laid {
        lay_out(
            &div(
                "p",
                vec![element("ul", "u", vec![element("li", "i", content)])],
            ),
            &format!(
                ".p {{ width: 360px; }} .u {{ margin: 0; padding: 0 0 0 60px; }} .q {{ margin: 0; }} .i {{ {css} }} {more}"
            ),
        )
    }

    /// Where the text of the first line of the item starts, from its content box.
    fn first_text_x(laid: &Laid) -> f32 {
        let mut font = florui_text::Font::load_embedded();
        let shaped = crate::shape_inline_formatting_context(
            &mut font,
            &laid.arena,
            &laid.styles,
            (&laid.layouts, 1.0),
            laid.node("i"),
            240.0,
        )
        .unwrap();
        shaped
            .layout
            .runs
            .iter()
            .filter(|run| run.line == 0)
            .map(|run| run.x)
            .fold(f32::MAX, f32::min)
    }

    #[test]
    fn an_image_marker_stands_on_the_baseline_and_makes_the_first_line_taller() {
        // Chrome 153, 16px Open Sans: a line is 22 tall, 17 above its baseline.
        for (width, height, tall) in [
            (12, 12, 22.0),
            (40, 40, 45.0),
            (8, 30, 35.0),
            (100, 20, 25.0),
        ] {
            for position in ["outside", "inside"] {
                let laid = image_item(
                    vec![text("Hello")],
                    &format!("list-style-position: {position}; {}", image(width, height)),
                );
                assert_eq!(laid.rect("i").3, tall, "{width}x{height} {position}");
            }
        }
        // The strut of a line of 30px has a half-leading: 4px above the 17, 4 below the 5.
        let laid = image_item(
            vec![text("Hello")],
            &format!("line-height: 30px; {}", image(40, 40)),
        );
        assert_eq!(laid.rect("i").3, 49.0);
    }

    #[test]
    fn an_image_marker_inside_takes_its_width_and_seven_pixels() {
        // Chrome 153: the text of an inside marker starts 7px after the image.
        for (width, height, start) in [
            (12, 12, 19.0),
            (40, 40, 47.0),
            (8, 30, 15.0),
            (100, 20, 107.0),
        ] {
            let laid = image_item(
                vec![text("Hello")],
                &format!("list-style-position: inside; {}", image(width, height)),
            );
            assert_eq!(first_text_x(&laid), start, "{width}x{height}");
        }
        // An image outside is beside the content, which starts where it would.
        let laid = image_item(vec![text("Hello")], &image(40, 40));
        assert_eq!(first_text_x(&laid), 0.0);
        let laid = image_item(
            vec![text(
                "Hello wrapped text that is long enough to wrap onto two lines at three hundred pixels wide ok",
            )],
            &format!("list-style-position: inside; {}", image(40, 40)),
        );
        // The first of three lines of 22 holds the image: 45 and two of 22.
        assert_eq!(laid.rect("i").3, 89.0);
    }

    #[test]
    fn an_image_with_no_size_of_its_own_is_a_square_of_half_the_ascent() {
        use florui_style::BackgroundImage;
        let url = |attributes: &str| {
            BackgroundImage::Url(format!(
                "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' {attributes}><rect width='30' height='10'/></svg>"
            ))
        };
        let size = |image: &BackgroundImage| {
            let size = super::image_marker_size(image, 17.0);
            (size.width, size.height)
        };
        // Chrome 153, 16px Open Sans: the ascent is 17.
        let laid = image_item(
            vec![text("x")],
            "list-style-image: linear-gradient(red, blue);",
        );
        let gradient = laid
            .styles
            .get(&laid.node("i"))
            .and_then(|style| style.list_style_image.clone())
            .expect("a gradient");
        assert_eq!(size(&gradient), (8.5, 8.5));
        assert_eq!(size(&url("")), (8.5, 8.5));
        assert_eq!(size(&url("viewBox='0 0 20 10'")), (8.5, 4.25));
        assert_eq!(size(&url("viewBox='0 0 10 20'")), (4.25, 8.5));
        assert_eq!(size(&url("width='30'")), (30.0, 8.5));
        assert_eq!(size(&url("height='30'")), (8.5, 30.0));
        assert_eq!(size(&url("width='30' viewBox='0 0 20 10'")), (30.0, 15.0));
        assert_eq!(size(&url("width='30' height='7'")), (30.0, 7.0));
        // One that cannot be read has no height and the width of the bullet and 2px.
        let broken = super::image_marker_size(&BackgroundImage::Url("no/such.png".into()), 17.0);
        assert_eq!(
            (broken.width, broken.height, broken.broken),
            (8.0, 0.0, true)
        );
    }

    #[test]
    fn an_image_marker_beside_blocks_is_where_chrome_puts_it() {
        let paragraph = || element("p", "q", vec![text("Hello")]);
        let item = |content, position: &str| {
            image_item(
                content,
                &format!("list-style-position: {position}; {}", image(40, 40)),
            )
        };
        // Inside, with a block first: the marker is alone on a line of its own.
        let laid = item(vec![paragraph()], "inside");
        assert_eq!(laid.rect("i").3, 67.0);
        assert_eq!(laid.rect("q").1 - laid.rect("i").1, 45.0);
        // After text, it is in the line of the text.
        for position in ["inside", "outside"] {
            let laid = item(vec![text("Hi"), paragraph()], position);
            assert_eq!(laid.rect("i").3, 67.0, "{position}");
            assert_eq!(laid.rect("q").1 - laid.rect("i").1, 45.0, "{position}");
        }
        // Outside, with a block first: the block comes down until its first baseline is the
        // marker's.
        let laid = item(vec![paragraph()], "outside");
        assert_eq!(laid.rect("i").3, 45.0);
        assert_eq!(laid.rect("q").1 - laid.rect("i").1, 23.0);
        // An item with nothing in it has a line for its marker.
        let laid = item(vec![], "outside");
        assert_eq!(laid.rect("i").3, 45.0);
    }

    #[test]
    fn the_content_comes_down_until_its_first_baseline_is_below_the_image() {
        // Chrome 153: the baseline of the content is 17 from its top and the image is 40
        // high, so it comes down 23, below the padding and the border of the item.
        let marker = format!("list-style-position: outside; {}", image(40, 40));
        let paragraph = |class| element("p", class, vec![text("Hello")]);
        let down = |laid: &Laid| laid.rect("q").1 - laid.rect("i").1;
        let laid = image_item(
            vec![paragraph("q")],
            &format!("padding-top: 10px; {marker}"),
        );
        assert_eq!((laid.rect("i").3, down(&laid)), (55.0, 33.0));
        let laid = image_item(
            vec![paragraph("q")],
            &format!("border-top: 5px solid red; padding-top: 10px; {marker}"),
        );
        assert_eq!((laid.rect("i").3, down(&laid)), (60.0, 38.0));
        // The margin above the paragraph still collapses out of the item.
        let laid = image_item_with(vec![paragraph("q")], &marker, ".q { margin-top: 16px; }");
        assert_eq!(
            (laid.rect("i").1, laid.rect("i").3, down(&laid)),
            (16.0, 45.0, 23.0)
        );
        // The paragraph has a baseline of its own, 27 down: it comes down 13.
        let laid = image_item_with(vec![paragraph("q")], &marker, ".q { padding-top: 10px; }");
        assert_eq!((laid.rect("i").3, down(&laid)), (45.0, 13.0));
        // One whose baseline is below the image does not come down.
        let laid = image_item_with(
            vec![paragraph("q")],
            &marker,
            ".q { font-size: 40px; line-height: 50px; }",
        );
        assert_eq!((laid.rect("i").3, down(&laid)), (50.0, 0.0));
        // Two come down together, once.
        let laid = image_item(vec![paragraph("q"), paragraph("q")], &marker);
        assert_eq!((laid.rect("i").3, down(&laid)), (67.0, 23.0));
        // With no baseline in the content the item is as tall as the line of the image.
        let laid = image_item_with(
            vec![element("div", "q", vec![])],
            &marker,
            ".q { height: 30px; }",
        );
        assert_eq!((laid.rect("i").3, down(&laid)), (45.0, 0.0));
        // The height the item may have is still the most it has.
        let laid = image_item(vec![paragraph("q")], &format!("max-height: 30px; {marker}"));
        assert_eq!(laid.rect("i").3, 30.0);
        // A box of its own is no different.
        let laid = image_item(vec![paragraph("q")], &format!("overflow: hidden; {marker}"));
        assert_eq!((laid.rect("i").3, down(&laid)), (45.0, 23.0));
    }

    #[test]
    fn an_image_that_cannot_be_read_is_its_type_with_the_gap_of_an_image() {
        // Chrome 153: the text starts 7px after what the type takes, which for a bullet
        // is its box, 2px wider than the bullet.
        let at = |css: &str| {
            first_text_x(&image_item(
                vec![text("Hello")],
                &format!("list-style-position: inside; {css}"),
            ))
        };
        let broken = "list-style-image: url(\"no/such/file.png\");";
        for kind in [
            "decimal",
            "lower-roman",
            "none",
            "\"-> \"",
            "disc",
            "square",
        ] {
            let kind = format!("list-style-type: {kind};");
            let own = at(&kind);
            let bullet = if kind.contains("disc") || kind.contains("square") {
                // The marker of a shape takes the width of the bullet, the space after it,
                // and (inside) the font size.
                own - (6.0 + 1.0 + 16.0)
            } else {
                own
            };
            let expected = if kind.contains("disc") || kind.contains("square") {
                6.0 + 2.0 + 7.0
            } else {
                bullet + 7.0
            };
            near(at(&format!("{kind} {broken}")), expected, 0.01, &kind);
        }
        // Outside, it is nothing: the item is as it was.
        let laid = image_item(vec![text("Hello")], broken);
        assert_eq!(laid.rect("i").3, 22.0);
    }
}
