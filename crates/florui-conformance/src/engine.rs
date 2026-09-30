//! Renders a [`FloruiSpec`] through Florui's own real style/layout/paint
//! pipeline — `Arena::build` → `compute_with_style` → `paint_to_buffer` —
//! replacing the `florui_devtools::scene` stand-in
//! this crate used before real style/layout/paint existed (see
//! [`crate::reference_fixture`]'s own module doc for that history).
//!
//! The tested element is wrapped in a synthetic `<div>`, matching the
//! Chromium HTML fixture's own `<body>` — without it, a bare `<span>`
//! would sit at the tree root, where Stylo's real blockification rule
//! (root elements are always block-level, `<https://drafts.csswg.org/css-display/#blockify>`)
//! would force it block regardless of `display: inline`, a false mismatch
//! against Chromium's own *non-root* `<span>` staying genuinely inline.
//!
//! `device_pixel_ratio != 1.0` (a "scale changes" fixture): style and
//! layout still run in CSS pixels — `getBoundingClientRect()` on the
//! Chromium side always reports CSS pixels regardless of DPR, so
//! `element_box_css_px` has to match that unit to stay comparable, not
//! a re-run of style/layout at scaled font-size/dimensions. Only the
//! painted canvas differs: every committed box is scaled up by DPR
//! immediately before painting, into a canvas sized at the DPR-scaled
//! physical resolution — the same split [`florui_platform::desktop`]'s
//! own `DesktopHost` uses for a real HiDPI window (`layout_viewport`/
//! `scale_layouts` there), reimplemented locally here since this crate
//! has no dependency on `florui-platform` and the scaling itself is a
//! few lines, not worth a new shared crate for.

use std::collections::HashMap;
use std::fmt;

use florui::Element;
use florui_layout::{BoxLayout, LayoutError, absolute_position};
use florui_paint::paint_to_buffer;
use florui_style::{Arena, ComputedStyle, InteractionState, NodeId, Rgba, parse_stylesheet};
use image::RgbaImage;
use taffy::prelude::{AvailableSpace, Size};

use crate::geometry::BoxGeometryPx;
use crate::reference_fixture::{FloruiChild, FloruiSpec};

#[derive(Debug)]
pub enum EngineError {
    /// `spec.tag` isn't one of the fixed tags this harness knows how to
    /// build a real `florui::Element` for — see [`to_static_tag`].
    UnknownTag(String),
    Layout(LayoutError),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::UnknownTag(tag) => write!(f, "unknown florui fixture tag: {tag:?}"),
            EngineError::Layout(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EngineError::UnknownTag(_) => None,
            EngineError::Layout(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub struct EngineRender {
    pub image: RgbaImage,
    pub element_box_css_px: BoxGeometryPx,
    /// Every element under the synthetic wrapper, in document order, for
    /// naming what lies behind a differing region.
    pub elements: Vec<crate::regions::ElementInfo>,
}

/// The fixed set of tags this harness's fixtures build — a `&'static str`
/// is what [`Element::node`] needs, which a JSON-loaded `String` can't
/// borrow from; matching against this small, known set is simpler than a
/// generic (and here unneeded) leak-and-intern scheme.
fn to_static_tag(tag: &str) -> Result<&'static str, EngineError> {
    Ok(match tag {
        "div" => "div",
        "span" => "span",
        "p" => "p",
        "button" => "button",
        "h1" => "h1",
        "h2" => "h2",
        "h3" => "h3",
        "h4" => "h4",
        "h5" => "h5",
        "h6" => "h6",
        other => return Err(EngineError::UnknownTag(other.to_owned())),
    })
}

/// Builds a real, possibly-nested `Element` from `spec`, recursively —
/// `spec.text` (a leaf shorthand) and `spec.children` (nested elements
/// interleaved with literal text runs, for a card's own heading/
/// paragraph or mixed inline content) both lower to the same
/// `Element::node`/`Element::text` primitives a `view!` invocation
/// would produce; `children` wins when both are set on the same spec (a
/// fixture author's error to write both, not a runtime condition worth
/// its own error variant).
fn build_element(spec: &FloruiSpec, id: Option<&str>) -> Result<Element, EngineError> {
    let tag = to_static_tag(&spec.tag)?;
    let mut attrs = Vec::new();
    if let Some(id) = id {
        attrs.push(("id".to_string(), id.to_string()));
    }
    if !spec.class.is_empty() {
        attrs.push(("class".to_string(), spec.class.clone()));
    }

    let kids = if spec.children.is_empty() {
        vec![Element::text(spec.text.clone())]
    } else {
        spec.children
            .iter()
            .map(|child| match child {
                FloruiChild::Text(text) => Ok(Element::text(text.clone())),
                FloruiChild::Element(nested) => build_element(nested, None),
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    Ok(Element::node(tag, attrs, kids))
}

/// Renders `spec` inside a `width_css_px`x`height_css_px` root (matching
/// the Chromium fixture's own viewport), returning the whole canvas plus
/// the tested element's own box. `device_pixel_ratio` scales only the
/// painted canvas, not layout itself — see this module's own doc.
///
/// # Panics
///
/// Panics if `css` isn't parseable as real CSS (a fixture's own author
/// error, not a runtime condition to recover from) or `canvas_color`
/// isn't valid hex.
pub fn render_fixture(
    spec: &FloruiSpec,
    css: &str,
    canvas_color: &str,
    width_css_px: u32,
    height_css_px: u32,
    device_pixel_ratio: f64,
) -> Result<EngineRender, EngineError> {
    let element = build_element(spec, Some("el"))?;
    // The synthetic wrapper this module's own doc explains — real CSS's
    // own body-equivalent, not part of the fixture's own declared markup.
    let tree = Element::node("div", vec![], vec![element]);

    let arena = Arena::build(&tree);
    let rules = parse_stylesheet(css).expect("fixture CSS must be valid");
    let available = Size {
        width: AvailableSpace::Definite(width_css_px as f32),
        height: AvailableSpace::Definite(height_css_px as f32),
    };
    let mut font = florui_text::Font::load_embedded();
    let florui_layout::LayoutResult {
        styles, layouts, ..
    } = florui_layout::compute_with_style(
        &mut font,
        &arena,
        &rules,
        &InteractionState::new(),
        florui_style::Viewport {
            width: width_css_px as f32,
            height: height_css_px as f32,
        },
        &mut florui_style::AnimationTimeline::default(),
        available,
    )
    .map_err(EngineError::Layout)?;

    let wrapper = arena.roots()[0];
    let node = arena.children(wrapper)[0];
    // A plain `display: inline` element with no content of its own (a
    // bare `<span>`) has no individual `BoxLayout` yet — a documented
    // bound (`florui_layout`'s module doc): only `InlineBlock`
    // children get an exact box from a real inline formatting context,
    // not a plain `Inline` one. Falling back to the wrapper's own box is
    // honest about that gap rather than panicking a fixture that hits it.
    let element_box_css_px =
        box_geometry_of(&arena, &styles, &layouts, node).unwrap_or_else(|| {
            box_geometry_of(&arena, &styles, &layouts, wrapper)
                .expect("the wrapper always has its own box")
        });

    let canvas = Rgba::opaque(0, 0, 0);
    let canvas = florui_style::parse_hex_color(canvas_color).unwrap_or(canvas);
    let physical_layouts = scale_layouts(&layouts, device_pixel_ratio as f32);
    let width_physical_px = (width_css_px as f64 * device_pixel_ratio).round() as u32;
    let height_physical_px = (height_css_px as f64 * device_pixel_ratio).round() as u32;
    let image_pixmap = paint_to_buffer(
        &mut font,
        width_physical_px,
        height_physical_px,
        canvas,
        &arena,
        &styles,
        &physical_layouts,
        device_pixel_ratio as f32,
    );
    let image = RgbaImage::from_raw(
        width_physical_px,
        height_physical_px,
        image_pixmap.data().to_vec(),
    )
    .expect("paint_to_buffer's own dimensions must match what was requested");

    Ok(EngineRender {
        image,
        element_box_css_px,
        elements: crate::regions::describe_elements(&arena, &styles, &layouts, 1),
    })
}

/// Scales every committed box from the CSS pixels layout ran against up
/// to physical pixels — see this module's own doc for why layout itself
/// stays in CSS pixels while only painting needs the scaled version.
fn scale_layouts(layouts: &HashMap<NodeId, BoxLayout>, factor: f32) -> HashMap<NodeId, BoxLayout> {
    layouts
        .iter()
        .map(|(&id, layout)| {
            (
                id,
                BoxLayout {
                    x: layout.x * factor,
                    y: layout.y * factor,
                    width: layout.width * factor,
                    height: layout.height * factor,
                },
            )
        })
        .collect()
}

/// A node's own box, in the fixture's logical CSS pixels — after applying
/// its own `transform`, so a transformed element compares against
/// Chromium's `getBoundingClientRect()` (which already reports the
/// *transformed* border box) on the same terms. `scale_factor` is always
/// `1.0` here regardless of the fixture's own `device_pixel_ratio`: see
/// this module's own doc for why `element_box_css_px` stays
/// DPR-independent.
pub(crate) fn box_geometry_of(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
) -> Option<BoxGeometryPx> {
    let layout = *layouts.get(&node)?;
    let (x, y) = absolute_position(arena, layouts, node);
    let style = styles.get(&node);
    let (x, y, width, height) = match style {
        Some(style) => florui_paint::transformed_bounding_box(style, &layout, x, y, 1.0),
        None => (x, y, layout.width, layout.height),
    };
    Some(BoxGeometryPx {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(width),
        height: f64::from(height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(tag: &str) -> FloruiSpec {
        FloruiSpec {
            tag: tag.to_string(),
            class: String::new(),
            text: String::new(),
            css: String::new(),
            children: Vec::new(),
        }
    }

    #[test]
    fn renders_a_bare_div_at_the_wrapper_origin() {
        let render = render_fixture(&spec("div"), "", "#000000", 200, 100, 1.0).unwrap();
        assert_eq!(render.element_box_css_px.x, 0.0);
        assert_eq!(render.element_box_css_px.y, 0.0);
        assert_eq!(render.image.dimensions(), (200, 100));
    }

    #[test]
    fn an_unknown_tag_is_a_reported_error_not_a_panic() {
        let error = render_fixture(&spec("marquee"), "", "#000000", 200, 100, 1.0).unwrap_err();
        assert!(matches!(error, EngineError::UnknownTag(tag) if tag == "marquee"));
    }

    #[test]
    fn a_span_nested_under_the_synthetic_wrapper_is_not_blockified() {
        // The whole reason for the synthetic `<div>` wrapper this module's
        // own doc explains: a bare `<span>` at the tree *root* would be
        // blockified (Stylo's real root-element rule), a false mismatch
        // against Chromium's own non-root `<span>`. This only proves the
        // wrapper exists and layout succeeds; `florui_style`'s own tests
        // already prove the blockification distinction itself.
        let render = render_fixture(&spec("span"), "", "#000000", 200, 100, 1.0);
        assert!(render.is_ok());
    }

    #[test]
    fn heading_text_measures_a_nonzero_box() {
        let mut spec = spec("h2");
        spec.text = "Hello".to_string();
        let render = render_fixture(&spec, "", "#000000", 200, 100, 1.0).unwrap();
        assert!(render.element_box_css_px.width > 0.0);
        assert!(render.element_box_css_px.height > 0.0);
    }

    #[test]
    fn nested_children_build_a_real_tree_a_card_heading_plus_paragraph_sizes_taller_than_either_alone()
     {
        let mut heading = spec("h2");
        heading.text = "Title".to_string();
        let mut paragraph = spec("p");
        paragraph.text = "Body copy.".to_string();

        let mut card = spec("div");
        card.css = "#el { display: flex; flex-direction: column; width: 200px; }".to_string();
        card.children = vec![
            FloruiChild::Element(heading.clone()),
            FloruiChild::Element(paragraph.clone()),
        ];

        let card_render = render_fixture(&card, &card.css, "#000000", 200, 300, 1.0).unwrap();
        let heading_alone = render_fixture(&heading, "", "#000000", 200, 300, 1.0).unwrap();
        let paragraph_alone = render_fixture(&paragraph, "", "#000000", 200, 300, 1.0).unwrap();

        assert!(
            card_render.element_box_css_px.height
                > heading_alone
                    .element_box_css_px
                    .height
                    .max(paragraph_alone.element_box_css_px.height),
            "a card containing both a heading and a paragraph must be taller than either child \
             measured alone — proof the children actually landed in the real tree, not just the \
             top-level spec"
        );
    }

    #[test]
    fn mixed_inline_children_interleave_text_and_an_element_in_source_order() {
        let mut span = spec("span");
        span.text = "world".to_string();

        let mut paragraph = spec("p");
        paragraph.children = vec![
            FloruiChild::Text("Hello ".to_string()),
            FloruiChild::Element(span),
            FloruiChild::Text("!".to_string()),
        ];

        let with_span = render_fixture(&paragraph, "", "#000000", 300, 100, 1.0).unwrap();

        let mut plain = spec("p");
        plain.text = "Hello world!".to_string();
        let without_span = render_fixture(&plain, "", "#000000", 300, 100, 1.0).unwrap();

        // Both read the same visible text, so both should measure to
        // (approximately) the same width — proof the interleaved Text/
        // Element children actually flowed as one run, not that the
        // Element child's own text simply went missing.
        assert!(
            (with_span.element_box_css_px.width - without_span.element_box_css_px.width).abs()
                < 2.0,
            "interleaved text+span+text ({}) must measure close to the equivalent flat text \
             ({})",
            with_span.element_box_css_px.width,
            without_span.element_box_css_px.width
        );
    }

    #[test]
    fn a_device_pixel_ratio_above_one_scales_the_canvas_but_not_the_css_px_geometry() {
        let mut heading = spec("h2");
        heading.text = "Hello".to_string();

        let at_1x = render_fixture(&heading, "", "#000000", 200, 100, 1.0).unwrap();
        let at_2x = render_fixture(&heading, "", "#000000", 200, 100, 2.0).unwrap();

        assert_eq!(
            at_2x.image.dimensions(),
            (400, 200),
            "the canvas itself is physical pixels"
        );
        assert_eq!(
            at_1x.element_box_css_px, at_2x.element_box_css_px,
            "getBoundingClientRect-equivalent geometry stays in CSS pixels regardless of DPR, \
             matching Chromium's own real behavior"
        );
    }

    #[test]
    fn a_translated_elements_geometry_reflects_the_transform_like_a_real_getboundingclientrect() {
        // Real CSS: `transform` never moves layout itself, but
        // `getBoundingClientRect()` reports the *transformed* border box
        // regardless — so this crate's own `element_box_css_px` must
        // apply the same transform, or every transformed fixture would
        // show a geometry mismatch against Chromium that isn't a real
        // rendering bug.
        let mut moved = spec("div");
        moved.class = "moved".to_string();
        let render = render_fixture(
            &moved,
            ".moved { width: 20px; height: 20px; transform: translate(30px, 40px); }",
            "#000000",
            200,
            200,
            1.0,
        )
        .unwrap();
        assert_eq!(render.element_box_css_px.x, 30.0);
        assert_eq!(render.element_box_css_px.y, 40.0);
        assert_eq!(render.element_box_css_px.width, 20.0);
        assert_eq!(render.element_box_css_px.height, 20.0);
    }
}
