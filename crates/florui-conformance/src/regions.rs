//! Which elements lie behind a region of differing pixels. A failed comparison
//! says where the pixels differ; this names the elements drawn there, deepest
//! first, with where each was written when the tree recorded it, so a
//! difference points at code and not only at a rectangle.

use std::collections::HashMap;

use florui_layout::BoxLayout;
use florui_style::{Arena, ComputedStyle, NodeId};

use crate::engine::box_geometry_of;
use crate::geometry::BoxGeometryPx;
use crate::pixels::PixelRegion;

/// One element of a rendered tree, as a comparison report describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct ElementInfo {
    /// Selector-like place in the tree: `div > div.card > h2`.
    pub path: String,
    pub tag: &'static str,
    /// Where the element is drawn, in CSS pixels.
    pub box_css_px: BoxGeometryPx,
    /// Where it was written, component and place included, when the tree
    /// came from `view!` in a build that records it.
    pub source: Option<String>,
}

/// Describes every element under the synthetic wrapper a fixture render adds
/// (`skip_roots` of them at the top are left out), in document order.
pub fn describe_elements(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    skip_roots: usize,
) -> Vec<ElementInfo> {
    let mut elements = Vec::new();
    let mut pending: Vec<NodeId> = arena.roots().iter().rev().copied().collect();
    while let Some(node) = pending.pop() {
        pending.extend(arena.children(node).iter().rev());
        if is_skipped(arena, node, skip_roots) {
            continue;
        }
        let Some(box_css_px) = box_geometry_of(arena, styles, layouts, node) else {
            continue;
        };
        elements.push(ElementInfo {
            path: path_of(arena, node, skip_roots),
            tag: arena.tag(node),
            box_css_px,
            source: arena.source(node).map(|source| source.to_string()),
        });
    }
    elements
}

fn is_skipped(arena: &Arena, node: NodeId, skip_roots: usize) -> bool {
    arena
        .roots()
        .iter()
        .take(skip_roots)
        .any(|&root| root == node)
}

fn path_of(arena: &Arena, node: NodeId, skip_roots: usize) -> String {
    let mut segments = Vec::new();
    let mut current = Some(node);
    while let Some(id) = current {
        if is_skipped(arena, id, skip_roots) {
            break;
        }
        let mut segment = arena.tag(id).to_string();
        if let Some(element_id) = arena.id_attr(id) {
            segment.push('#');
            segment.push_str(element_id);
        }
        for class in arena.classes(id) {
            segment.push('.');
            segment.push_str(class);
        }
        segments.push(segment);
        current = arena.parent(id);
    }
    segments.reverse();
    segments.join(" > ")
}

/// The elements whose box overlaps `region` (physical pixels at
/// `device_pixel_ratio`), deepest first: the innermost element drawn there
/// comes before the ones that contain it.
pub fn elements_at(
    elements: &[ElementInfo],
    region: PixelRegion,
    device_pixel_ratio: f64,
) -> Vec<&ElementInfo> {
    let left = f64::from(region.x) / device_pixel_ratio;
    let top = f64::from(region.y) / device_pixel_ratio;
    let right = f64::from(region.x + region.width) / device_pixel_ratio;
    let bottom = f64::from(region.y + region.height) / device_pixel_ratio;
    let mut found: Vec<(usize, &ElementInfo)> = elements
        .iter()
        .enumerate()
        .filter(|(_, element)| {
            let b = element.box_css_px;
            b.x < right && b.x + b.width > left && b.y < bottom && b.y + b.height > top
        })
        .collect();
    let depth = |element: &ElementInfo| element.path.matches(" > ").count();
    found.sort_by(|(ai, a), (bi, b)| depth(b).cmp(&depth(a)).then(bi.cmp(ai)));
    found.into_iter().map(|(_, element)| element).collect()
}

/// One line per element behind `region`, at most `limit` of them:
/// `div > div.card > h2  (Card src/card.rs:14:9 div > h2)` where the tree
/// recorded where the element was written.
pub fn culprit_lines(
    elements: &[ElementInfo],
    region: PixelRegion,
    device_pixel_ratio: f64,
    limit: usize,
) -> Vec<String> {
    elements_at(elements, region, device_pixel_ratio)
        .into_iter()
        .take(limit)
        .map(|element| match &element.source {
            Some(source) => format!("{}  ({source})", element.path),
            None => element.path.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_style::{InteractionState, Viewport, parse_stylesheet};
    use taffy::prelude::*;

    use super::*;

    fn info(path: &str, x: f64, y: f64, width: f64, height: f64) -> ElementInfo {
        ElementInfo {
            path: path.to_string(),
            tag: "div",
            box_css_px: BoxGeometryPx {
                x,
                y,
                width,
                height,
            },
            source: None,
        }
    }

    fn region(x: u32, y: u32, width: u32, height: u32) -> PixelRegion {
        PixelRegion {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_innermost_element_behind_a_region_comes_first() {
        let elements = vec![
            info("div", 0.0, 0.0, 100.0, 100.0),
            info("div > div.card", 10.0, 10.0, 80.0, 60.0),
            info("div > div.card > h2", 10.0, 10.0, 80.0, 20.0),
            info("div > div.card > p", 10.0, 30.0, 80.0, 40.0),
        ];
        let found = elements_at(&elements, region(12, 12, 4, 4), 1.0);
        let paths: Vec<_> = found.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["div > div.card > h2", "div > div.card", "div"]);
    }

    #[test]
    fn a_region_is_converted_from_physical_pixels() {
        let elements = vec![
            info("div > a", 0.0, 0.0, 10.0, 10.0),
            info("div > b", 50.0, 50.0, 10.0, 10.0),
        ];
        // At a ratio of 2, physical (100, 100) is CSS (50, 50).
        let found = elements_at(&elements, region(100, 100, 4, 4), 2.0);
        let paths: Vec<_> = found.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["div > b"]);
    }

    #[test]
    fn a_region_touching_only_an_edge_does_not_overlap() {
        let elements = vec![info("div > a", 0.0, 0.0, 10.0, 10.0)];
        assert!(elements_at(&elements, region(10, 0, 5, 5), 1.0).is_empty());
    }

    #[test]
    fn a_mismatch_in_a_real_tree_names_the_element_and_where_it_was_written() {
        let marker = line!();
        let tree = view! {
            <div class="card">
                <h2>{"Title"}</h2>
                <p>{"Body text"}</p>
            </div>
        };
        let wrapper = Element::node("div", vec![], vec![tree]);
        let arena = Arena::build(&wrapper);
        let rules = parse_stylesheet(
            ".card { width: 120px; } h2 { height: 30px; margin-top: 0px; margin-bottom: 0px; } \
             p { height: 40px; }",
        )
        .unwrap();
        let mut font = florui_text::Font::load_embedded();
        let florui_layout::LayoutResult {
            styles, layouts, ..
        } = florui_layout::compute_with_style(
            &mut font,
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 200.0,
                height: 200.0,
            },
            &mut florui_style::AnimationTimeline::default(),
            Size {
                width: AvailableSpace::Definite(200.0),
                height: AvailableSpace::Definite(200.0),
            },
        )
        .unwrap();

        let elements = describe_elements(&arena, &styles, &layouts, 1);
        let paths: Vec<_> = elements.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["div.card", "div.card > h2", "div.card > p"]);

        // A difference in the top rows of the card is the heading.
        let lines = culprit_lines(&elements, region(2, 2, 6, 6), 1.0, 5);
        assert!(lines[0].starts_with("div.card > h2  ("), "{lines:?}");
        assert!(
            lines[0].contains(&format!("regions.rs:{}:", marker + 3)),
            "{lines:?}"
        );
        assert!(lines[1].starts_with("div.card  ("), "{lines:?}");
    }

    #[test]
    fn a_limit_keeps_only_the_innermost_lines() {
        let elements = vec![
            info("div > a", 0.0, 0.0, 10.0, 10.0),
            info("div > a > b", 0.0, 0.0, 10.0, 10.0),
            info("div > a > b > c", 0.0, 0.0, 10.0, 10.0),
        ];
        let lines = culprit_lines(&elements, region(1, 1, 2, 2), 1.0, 2);
        assert_eq!(lines, ["div > a > b > c", "div > a > b"]);
    }
}
