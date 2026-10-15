//! Turns a [`florui_style::Arena`] plus its computed styles into geometry,
//! via [Taffy](https://github.com/DioxusLabs/taffy) — validated against
//! Taffy 0.14's actual current API rather than assumed from memory.
//!
//! # Scope
//!
//! Block and flex layout — a node's own `display` selects which algorithm
//! lays out *its children*; a node's own box within its *parent* additionally
//! depends on `flex-grow`/`flex-shrink`/`flex-basis`/`align-self` when that
//! parent is a flex container, regardless of the node's own `display`. Grid
//! is not implemented yet, even though Taffy itself already supports it —
//! `florui-style` has no `grid-*` properties to translate from.
//!
//! `width`/`height` follow `box-sizing`: content-box by default, which is real
//! CSS's actual default and padding adding to a declared size (Taffy's own
//! default is border-box, so the box sizing is always set explicitly), and
//! border-box when the style says so, where padding and border are carved out
//! of the declared size instead.
//!
//! A leaf node (no element children) with its own direct text is measured
//! via [`florui_text`] — real shaping, not a guess — and that intrinsic
//! size is used wherever the node's own `width`/`height` don't already
//! settle it. A leaf with no text still lays out at `0x0` when it has no
//! explicit size, since there is nothing to measure it from.
//!
//! Real inline formatting context, for a container whose children mix
//! text with `display: inline`/`inline-block` elements
//! (`<p>Hello <span>world</span>!</p>`) — Taffy itself has no inline
//! display mode at all, so this is a genuinely separate algorithm built on
//! [`florui_text::Font::shape_inline`]'s real multi-style text wrapping
//! and in-flow boxes, not a translation onto an existing Taffy algorithm
//! the way flex was. See [`needs_inline_layout`]'s own doc for what it
//! covers: inline elements nest to any depth and each gets its own
//! [`BoxLayout`] (the bounding box of its pieces on each line; padding,
//! border and margin sit in the line), and an `InlineBlock` child, whether a
//! leaf (a `<button>`) or holding content of its own, is laid out as one atomic
//! box, sized to its content and sitting on its last line. Inline content
//! mixed with block-level children (`text<div>block</div>more text`) is
//! wrapped in anonymous blocks, one per run, which are reported next to the
//! arena nodes under ids from [`anonymous_id`].
//!
//! Taffy positions are relative to the parent's content box, matching
//! Taffy's own convention; see [`absolute_position`] to accumulate them
//! into a position relative to the layout root.
//!
//! A very deep tree (hundreds of nested levels) no longer *crashes* —
//! `compute_layout` grows its own stack via `stacker` before running
//! Taffy's real layout algorithms, which recurse once per tree depth
//! internally and would otherwise overflow the default stack well under
//! 1,000 levels.

use florui_style::StyleMap;
use std::collections::HashMap;

use florui_style::{
    AnimationTimeline, Arena, BoxSizing as StyleBoxSizing, ComputedStyle, ContentAlignment,
    ContentBoxSize, Display as StyleDisplay, FlexDirection as StyleFlexDirection,
    FlexWrap as StyleFlexWrap, InlineItem as StyleInlineItem, InteractionState, ItemAlignment,
    NodeId, Position as StylePosition, RoundedRect, Rule, Tree, Viewport,
};
use taffy::prelude::*;
use taffy::{Baselines, compute_leaf_layout};

mod cache;
mod transform;

pub use cache::LayoutCache;
pub use transform::{Affine, resolve_transform, screen_bounds};

/// Checked before Taffy's own recursive layout algorithms run — if less
/// than this much stack remains, `stacker` allocates a fresh
/// [`RECURSION_STACK_SIZE`]-byte segment first rather than let a deep
/// tree overflow the one it was already on. Deliberately larger than a
/// typical thread's whole default stack (a few MB), so this effectively
/// always grows rather than gambling that whatever happened to be left
/// over from the caller's own stack is enough for Taffy's own recursion —
/// a real, measured need: 2 MB of headroom still wasn't enough for it at
/// only ~1,200 levels.
const RECURSION_RED_ZONE: usize = 8 * 1024 * 1024;
/// Comfortably deeper than any tree a real UI produces — sized against
/// this crate's own 20,000-deep test elsewhere in the workspace, not
/// picked arbitrarily.
const RECURSION_STACK_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxLayout {
    /// Relative to the parent's content box (`(0, 0)` for a root).
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// A node's real scrollable content extent — its reachable size along each
/// axis, which for a node with overflowing children is larger than its own
/// [`BoxLayout`] box. Comes straight from Taffy's own
/// `Layout::scrollable_overflow_rect` (already computed as part of every
/// layout pass; this is a read of existing data, not new layout work), so
/// it is always at least the node's own box size — a non-overflowing node
/// simply reports its own size back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContentExtent {
    pub width: f32,
    pub height: f32,
}

/// [`compute_with_style`]'s own return shape — resolved styles and geometry
/// alongside each node's real scrollable content extent, the same trio
/// `UiRuntime::geometry` hands callers.
pub struct LayoutResult {
    pub styles: StyleMap,
    pub layouts: HashMap<NodeId, BoxLayout>,
    pub content_extents: HashMap<NodeId, ContentExtent>,
}

#[derive(Debug)]
pub struct LayoutError(taffy::TaffyError);

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "layout failed: {}", self.0)
    }
}

impl std::error::Error for LayoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// A leaf's own text plus what's needed to measure it — see
/// [`florui_text`]'s own scope notes for what "measure" does and doesn't
/// cover yet (no rasterization).
#[derive(PartialEq)]
struct TextContext {
    text: String,
    style: florui_text::TextStyle,
}

/// What the measure closure keeps for one text leaf across its calls in one
/// layout pass. Taffy asks about a leaf a dozen times with different inputs
/// (min-content and max-content in each axis, then the widths a flex line
/// settles on), but a text leaf's answer depends only on the width it is
/// wrapped at, which in a typical leaf takes two values: unconstrained and
/// the one it ends up with. The answers already given are kept by wrap width
/// so a repeat does not go back to the text memo.
#[derive(Default)]
struct LeafScratch {
    /// The shaped layout, reused across widths (see `measure_cached`).
    shaping: Option<florui_text::CachedLayout>,
    /// Wrap width bits (`None` for unwrapped) and what that width measured.
    answers: [Option<(Option<u32>, florui_text::TextMetrics)>; 4],
}

impl LeafScratch {
    /// What `width` measures to, from an earlier answer or from `measure`
    /// (which gets this leaf's shaped layout to reuse). A leaf asked at more
    /// than four widths still measures correctly; it just stops being kept.
    fn answer(
        &mut self,
        width: Option<f32>,
        measure: impl FnOnce(&mut Option<florui_text::CachedLayout>) -> florui_text::TextMetrics,
    ) -> florui_text::TextMetrics {
        let key = width.map(f32::to_bits);
        for slot in &self.answers {
            if let Some((known, metrics)) = slot
                && *known == key
            {
                return *metrics;
            }
        }
        let metrics = measure(&mut self.shaping);
        if let Some(free) = self.answers.iter_mut().find(|slot| slot.is_none()) {
            *free = Some((key, metrics));
        }
        metrics
    }
}

/// A childless leaf's own text to measure/shape — `<input>`'s `value`
/// attribute (it can never have `Element::Text` children at all;
/// `Content::Void` forbids it) for every other childless tag,
/// `Arena::text_content` exactly as before. Every leaf-intrinsic-size call
/// site (a leaf built directly, an inline-block child's own measurement,
/// `flex-basis`'s own text fallback) needs this instead of
/// `text_content` alone, or a real `<input>` never gets a real size.
///
/// An `<input>` with an empty `value` measures as a single space, not an
/// empty string: unlike an ordinary empty element (a `<div></div>`
/// legitimately collapses to zero), a real text input keeps its own
/// line-height-driven box even with nothing typed in it yet — real
/// `Font::measure`'s own documented behavior treats a truly empty string
/// as occupying no line at all, which would otherwise collapse an
/// `<input>`'s height to zero the instant its value is cleared.
fn leaf_text(arena: &Tree, node: NodeId) -> &str {
    if arena.tag(node) == "input" {
        match arena.value_attr(node) {
            Some(value) if !value.is_empty() => value,
            _ => " ",
        }
    } else {
        arena.text_content(node)
    }
}

/// `pub`, not private: `florui-platform`'s own text-editing registry needs
/// this exact translation too (an editable input's caret/selection ops
/// reshape through the same font a plain text node would) — one shared
/// mapping, not a second copy reimplementing it.
pub fn to_text_font_family(value: florui_style::FontFamily) -> florui_text::FontFamily {
    match value {
        florui_style::FontFamily::SansSerif => florui_text::FontFamily::SansSerif,
        florui_style::FontFamily::Monospace => florui_text::FontFamily::Monospace,
    }
}

/// What a node's own text is shaped with: the one place a computed style
/// becomes a text style.
pub fn text_style_of(style: &ComputedStyle) -> florui_text::TextStyle {
    let line_height = match style.line_height {
        florui_style::LineHeight::Normal => florui_text::LineHeight::Normal,
        florui_style::LineHeight::Number(n) => florui_text::LineHeight::Number(n),
        florui_style::LineHeight::Length(px) => florui_text::LineHeight::Length(px),
    };
    // The text of a flex or grid container is an anonymous item as wide as the
    // text, so there is nothing for `text-align` to align in it.
    let align = match style.text_align {
        _ if matches!(style.display, StyleDisplay::Flex | StyleDisplay::Grid) => {
            florui_text::TextAlign::Start
        }
        florui_style::TextAlign::Start => florui_text::TextAlign::Start,
        florui_style::TextAlign::End => florui_text::TextAlign::End,
        florui_style::TextAlign::Center => florui_text::TextAlign::Center,
        florui_style::TextAlign::Justify => florui_text::TextAlign::Justify,
    };
    let vertical_align = match style.vertical_align {
        florui_style::VerticalAlign::Baseline => florui_text::VerticalAlign::Baseline,
        florui_style::VerticalAlign::Sub => florui_text::VerticalAlign::Sub,
        florui_style::VerticalAlign::Super => florui_text::VerticalAlign::Super,
        florui_style::VerticalAlign::TextTop => florui_text::VerticalAlign::TextTop,
        florui_style::VerticalAlign::TextBottom => florui_text::VerticalAlign::TextBottom,
        florui_style::VerticalAlign::Middle => florui_text::VerticalAlign::Middle,
        florui_style::VerticalAlign::Top => florui_text::VerticalAlign::Top,
        florui_style::VerticalAlign::Bottom => florui_text::VerticalAlign::Bottom,
        florui_style::VerticalAlign::Raise(amount) => florui_text::VerticalAlign::Raise {
            length: amount.length,
            percent: amount.percentage,
        },
    };
    florui_text::TextStyle::new(
        to_text_font_family(style.font_family),
        style.font_size,
        style.font_weight,
    )
    .with_line_height(line_height)
    .with_spacing(style.letter_spacing, style.word_spacing)
    .with_align(align)
    .with_indent(florui_text::TextIndent {
        length: style.text_indent.length.length,
        percent: style.text_indent.length.percentage,
        hanging: style.text_indent.hanging,
        each_line: style.text_indent.each_line,
    })
    .with_vertical_align(vertical_align)
    .with_wrapping(
        match style.white_space_collapse {
            florui_style::WhiteSpaceCollapse::Collapse => florui_text::WhiteSpaceCollapse::Collapse,
            florui_style::WhiteSpaceCollapse::Preserve => florui_text::WhiteSpaceCollapse::Preserve,
            florui_style::WhiteSpaceCollapse::PreserveBreaks => {
                florui_text::WhiteSpaceCollapse::PreserveBreaks
            }
            florui_style::WhiteSpaceCollapse::BreakSpaces => {
                florui_text::WhiteSpaceCollapse::BreakSpaces
            }
        },
        style.text_wrap,
        match style.word_break {
            florui_style::WordBreak::Normal | florui_style::WordBreak::BreakWord => {
                florui_text::WordBreak::Normal
            }
            florui_style::WordBreak::BreakAll => florui_text::WordBreak::BreakAll,
            florui_style::WordBreak::KeepAll => florui_text::WordBreak::KeepAll,
        },
        match style.overflow_wrap {
            florui_style::OverflowWrap::Normal => florui_text::OverflowWrap::Normal,
            florui_style::OverflowWrap::BreakWord => florui_text::OverflowWrap::BreakWord,
            florui_style::OverflowWrap::Anywhere => florui_text::OverflowWrap::Anywhere,
        },
    )
}

/// What text is shaped with when its node has no computed style.
pub(crate) fn default_text_style() -> florui_text::TextStyle {
    florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0)
}

/// [`text_style_of`], or the default for a node with no computed style.
pub fn node_text_style(style: Option<&ComputedStyle>) -> florui_text::TextStyle {
    style.map_or_else(default_text_style, text_style_of)
}

/// One piece of a container's inline formatting context, already resolved
/// down to what [`florui_text::Font::shape_inline_tree`] needs — built once
/// in [`build_node`], reused by both the measure closure and the post-layout
/// pass that places each inline element's and box's own [`BoxLayout`].
#[derive(Clone, PartialEq)]
enum InlineContentItem {
    Text {
        text: String,
        element: usize,
    },
    /// The horizontal padding and border inside an element's box, and its
    /// margin outside it, on its start side or, with `end`, its end side.
    Spacer {
        element: usize,
        inside: f32,
        margin: f32,
        end: bool,
    },
    /// A real `display: inline-block` child, flowing as one atomic box —
    /// `child` is the arena node this box's own [`BoxLayout`] belongs to.
    Box {
        child: NodeId,
        width: f32,
        height: f32,
        baseline: f32,
        element: usize,
        vertical_align: florui_text::VerticalAlign,
        /// The laid out content of an inline-block that has children of its
        /// own, which are not Taffy nodes of the container's tree.
        nested: Option<std::sync::Arc<NestedBlock>>,
    },
}

/// What an `inline-block` with children holds, laid out on its own: its
/// descendants' boxes, relative to their parents.
#[derive(PartialEq)]
struct NestedBlock {
    layouts: HashMap<NodeId, BoxLayout>,
    extents: HashMap<NodeId, ContentExtent>,
}

/// A container's inline formatting context: the tree of its inline elements
/// (element `0` is the container, whose own text style gives every line at
/// least the height of an empty one, the CSS strut), the items of text,
/// spacers and boxes that belong to them, and the arena node of each element.
#[derive(Clone, PartialEq)]
struct InlineContext {
    elements: Vec<florui_text::InlineElement>,
    items: Vec<InlineContentItem>,
    nodes: Vec<NodeId>,
    /// Children with `display: none`, which take no part in the lines but
    /// still have an empty box.
    hidden: Vec<NodeId>,
}

fn to_inline_items(context: &InlineContext) -> Vec<florui_text::InlineItem<'_>> {
    context
        .items
        .iter()
        .map(|item| match item {
            InlineContentItem::Text { text, element } => florui_text::InlineItem::Text {
                text,
                element: *element,
            },
            InlineContentItem::Spacer {
                element,
                inside,
                margin,
                end,
            } => florui_text::InlineItem::Spacer {
                element: *element,
                inside: *inside,
                margin: *margin,
                end: *end,
            },
            InlineContentItem::Box {
                child,
                width,
                height,
                baseline,
                element,
                vertical_align,
                ..
            } => florui_text::InlineItem::Box {
                id: *child as u64,
                width: *width,
                height: *height,
                baseline: *baseline,
                element: *element,
                vertical_align: *vertical_align,
            },
        })
        .collect()
}

/// A Taffy leaf's own context: either plain single-style text (unchanged
/// from before this slice), or a real inline formatting context — mixed
/// text and inline-level element children, laid out as wrapped lines via
/// [`florui_text::Font::shape_inline`] rather than Taffy's own block
/// algorithm, which has no inline display mode at all (see this module's
/// own top-level doc).
#[derive(PartialEq)]
enum LeafContext {
    Text(TextContext),
    Inline(InlineContext),
    Image(ImageContext),
    /// A control whose content box has a fixed intrinsic size independent of
    /// what it holds (a `<textarea>`: `cols` by `rows`). Explicit CSS
    /// dimensions still win, as with any element.
    Fixed(Size<f32>),
}

/// Width a `<textarea>`'s default size reserves for its scrollbar (measured
/// in Edge on Windows: 20 columns of a 7.33px monospace advance measure
/// 162px wide, which is 146.7 + 15).
const TEXTAREA_SCROLLBAR_WIDTH: f32 = 15.0;

/// A `<textarea>`'s default content size: `cols` (default 20) average
/// characters wide plus its scrollbar, `rows` (default 2) lines tall.
fn textarea_intrinsic_size(
    font: &mut florui_text::Font,
    arena: &Tree,
    node: NodeId,
    style: Option<&ComputedStyle>,
) -> Size<f32> {
    let text_style = style.map_or_else(
        || florui_text::TextStyle::new(florui_text::FontFamily::Monospace, 13.3333, 400.0),
        text_style_of,
    );
    let attr_count = |name: &str, default: f32| {
        arena
            .attr(node, name)
            .and_then(|raw| raw.trim().parse::<u32>().ok())
            .filter(|&count| count > 0)
            .map_or(default, |count| count as f32)
    };
    let advance = font.measure(text_style, "0").width;
    let line_height = font.measure(text_style, " ").height;
    Size {
        width: attr_count("cols", 20.0) * advance + TEXTAREA_SCROLLBAR_WIDTH,
        height: attr_count("rows", 2.0) * line_height,
    }
}

/// A replaced element's own sizing inputs, pre-resolved at build time the
/// same way [`TextContext`] pre-resolves font/text state — `measure_leaf`
/// itself never looks anything back up in `styles`.
///
/// `intrinsic_size` is `None` for the entire lifetime of one
/// `compute_layout` call in this delivery: there is no channel yet from
/// an async-loaded image's real dimensions into this crate (that's
/// `florui-platform`'s follow-on work, patching a resolved width/height
/// into `ComputedStyle` the same way [`compute_layout_with_content_extents`]'s
/// own doc already describes for `<select>`'s auto-width). `None` is not
/// a placeholder/approximation, though: it is real CSS's own "no
/// intrinsic size yet" case (an image that hasn't finished loading, or
/// never will) with its own well-defined sizing behavior — see
/// [`resolve_replaced_size`]'s own doc.
#[derive(PartialEq)]
struct ImageContext {
    intrinsic_size: Option<Size<f32>>,
    aspect_ratio: florui_style::AspectRatio,
}

/// Whether `node`'s own children should be laid out as a real inline
/// formatting context (mixed text and inline-level elements sharing
/// wrapped lines) rather than Taffy's own block/flex algorithm — real
/// mixed content (`<p>Hello <span>world</span>!</p>`), not a block
/// container that merely happens to have an inline-display child among
/// otherwise block/flex siblings.
///
/// Every element child must itself be `Inline`/`InlineBlock` (an inline
/// element's own children included, to any depth). A node that also has
/// block-level children is not one context but a container of blocks and
/// anonymous blocks: see [`mixed_parts`].
fn needs_inline_layout(arena: &Tree, styles: &StyleMap, node: NodeId) -> bool {
    layout_kind(arena, styles, node) == LayoutKind::Inline
}

/// How a node with children lays them out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    /// Its children are blocks, flex items or grid cells: Taffy's own.
    Blocks,
    /// Text and inline-level elements only: one inline formatting context.
    Inline,
    /// Inline-level runs between block-level children: see [`mixed_parts`].
    Mixed,
}

/// Which of the three `node` is, in one pass over its children (most nodes
/// are asked at every layout, so the pass stops as soon as it knows).
fn layout_kind(arena: &Tree, styles: &StyleMap, node: NodeId) -> LayoutKind {
    let (mut block, mut inline) = (false, false);
    for item in arena.inline_items(node) {
        match item {
            StyleInlineItem::Text(text) => inline |= !text.trim().is_empty(),
            StyleInlineItem::Element(child) => match styles.get(child).map(|s| s.display) {
                Some(StyleDisplay::Inline) if inline_subtree_is_inline(arena, styles, *child) => {
                    inline = true;
                }
                Some(StyleDisplay::InlineBlock) => inline = true,
                Some(StyleDisplay::None) => {}
                _ => block = true,
            },
        }
        if block && inline {
            return LayoutKind::Mixed;
        }
    }
    match (block, inline) {
        (false, true) => LayoutKind::Inline,
        _ => LayoutKind::Blocks,
    }
}

/// One piece of a container whose content is inline-level runs between
/// block-level children: a child laid out as a block, or a run of text and
/// inline-level elements (by their indices in [`Arena::inline_items`]) that
/// CSS wraps in an anonymous block of its own.
#[derive(Clone, PartialEq)]
enum Part {
    Block(NodeId),
    Run(std::ops::Range<usize>),
}

/// The pieces of `node` when its content mixes inline-level runs with
/// block-level children (text, a block, more text), or `None`
/// when it does not and the container is laid out as it always was. A run of
/// whitespace between blocks is not a piece; a `display: none` element is
/// a block of no size unless it sits in a run that has content.
fn mixed_parts(arena: &Tree, styles: &StyleMap, node: NodeId) -> Option<Vec<Part>> {
    (layout_kind(arena, styles, node) == LayoutKind::Mixed)
        .then(|| mixed_parts_of(arena, styles, node))
}

/// [`mixed_parts`] of a node already known to be [`LayoutKind::Mixed`].
fn mixed_parts_of(arena: &Tree, styles: &StyleMap, node: NodeId) -> Vec<Part> {
    let items = arena.inline_items(node);
    let mut parts = Vec::new();
    let mut start = 0;
    let mut content = false;
    let close = |parts: &mut Vec<Part>, start: usize, end: usize, content: bool| {
        if content {
            parts.push(Part::Run(start..end));
            return;
        }
        for item in &items[start..end] {
            if let StyleInlineItem::Element(child) = item {
                parts.push(Part::Block(*child));
            }
        }
    };
    for (index, item) in items.iter().enumerate() {
        match item {
            StyleInlineItem::Text(text) => content |= !text.trim().is_empty(),
            StyleInlineItem::Element(child) => match styles.get(child).map(|s| s.display) {
                Some(StyleDisplay::Inline) if inline_subtree_is_inline(arena, styles, *child) => {
                    content = true;
                }
                Some(StyleDisplay::InlineBlock) => content = true,
                Some(StyleDisplay::None) => {}
                _ => {
                    close(&mut parts, start, index, content);
                    parts.push(Part::Block(*child));
                    start = index + 1;
                    content = false;
                }
            },
        }
    }
    close(&mut parts, start, items.len(), content);
    parts
}

/// Marks a node id that is not an arena node but the anonymous block around a
/// run of inline content: its container and its place among the container's
/// runs are in the rest of the bits (see [`anonymous_id`]).
const ANONYMOUS: NodeId = 1 << 62;

/// Whether `node` is the id of an anonymous block (see [`anonymous_id`])
/// rather than an arena node.
pub fn is_anonymous(node: NodeId) -> bool {
    node & ANONYMOUS != 0
}

/// The id under which the layout of the `run`th anonymous block of
/// `container` is reported, next to the arena nodes'.
pub fn anonymous_id(container: NodeId, run: usize) -> NodeId {
    ANONYMOUS | (container << 16) | run.min(0xffff)
}

/// The arena node whose content has the anonymous block `node`.
pub fn anonymous_container(node: NodeId) -> NodeId {
    (node & !ANONYMOUS) >> 16
}

/// Marks the id of one piece of an inline element that wraps over lines.
const PIECE: NodeId = 1 << 61;

/// The id under which the `piece`th line's box of the inline element `node`
/// is reported, relative to the element's own box, when it wraps over
/// several. [`hit_test`] hits the element only inside them.
pub fn piece_id(node: NodeId, piece: usize) -> NodeId {
    PIECE | (node << 16) | piece.min(0xffff)
}

/// Marks the id under which the baseline of an inline-block with content is
/// reported (as the `y` of its entry), so painting does not lay it out again.
const BASELINE: NodeId = 1 << 60;

fn baseline_id(node: NodeId) -> NodeId {
    BASELINE | node
}

/// What painting knows of the boxes layout placed: their layouts, in physical
/// pixels at the scale given, so it does not lay an inline-block out again.
type Laid<'a> = Option<(&'a HashMap<NodeId, BoxLayout>, f32)>;

/// Whether everything inside the inline element `node` is inline-level, so
/// it can flow in its container's lines.
fn inline_subtree_is_inline(arena: &Tree, styles: &StyleMap, node: NodeId) -> bool {
    arena.inline_items(node).iter().all(|item| match item {
        StyleInlineItem::Text(_) => true,
        StyleInlineItem::Element(child) => match styles.get(child).map(|s| s.display) {
            Some(StyleDisplay::Inline) => inline_subtree_is_inline(arena, styles, *child),
            Some(StyleDisplay::InlineBlock | StyleDisplay::None) => true,
            _ => false,
        },
    })
}

/// An inline-block child's own intrinsic *border-box* size: its own
/// explicit `width`/`height` where set, falling back to its own unwrapped
/// text measurement — a bounded shrink-to-fit (real CSS's actual
/// shrink-to-fit algorithm also considers the line's own remaining space;
/// this crate does not yet, the same kind of documented bound as
/// [`needs_inline_layout`]'s own one-level restriction) — plus the
/// child's own padding and border, since both `width`/`height` are
/// content-box (this crate's own top-level scope doc) and this function's
/// result becomes the child's *whole* [`BoxLayout`] directly (line
/// formatting has no separate box-model resolution pass the way Taffy's
/// own block/flex algorithms do for every other node). Omitting this
/// addition was a real bug: two buttons with different declared padding
/// came out the exact same height, since only their identical text
/// metrics were ever counted. `measure_inline_block_intrinsic_size` only
/// runs for a child [`needs_inline_layout`] already required to be a
/// leaf, so its own direct text (not `inline_items`) is exactly what it
/// has to measure.
fn measure_inline_block_intrinsic_size(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    child: NodeId,
) -> (f32, f32, f32) {
    let style = styles.get(&child);
    let text = leaf_text(arena, child);
    let text_style = style.map_or_else(default_text_style, text_style_of);
    let measured = if arena.tag(child) == "textarea" {
        let size = textarea_intrinsic_size(font, arena, child, style);
        florui_text::TextMetrics {
            width: size.width,
            height: size.height,
            baseline: 0.0,
        }
    } else if text.is_empty() {
        florui_text::TextMetrics {
            width: 0.0,
            height: 0.0,
            baseline: 0.0,
        }
    } else if let Some(width) = definite_content_width(style) {
        // A declared width wraps the text at it.
        font.measure_wrapped(text_style, text, width)
    } else {
        font.measure(text_style, text)
    };
    let (padding_x, padding_y) = style.map_or((0.0, 0.0), |s| {
        (
            s.padding.left + s.padding.right,
            s.padding.top + s.padding.bottom,
        )
    });
    let (border_x, border_y) = style.map_or((0.0, 0.0), |s| {
        (
            s.border.left.width + s.border.right.width,
            s.border.top.width + s.border.bottom.width,
        )
    });
    // A declared size measures the content box, or the whole box under
    // `border-box` (it then cannot be smaller than its own padding and
    // border); an undeclared one is the text's, plus the padding and border.
    let border_box = style.is_some_and(|s| s.box_sizing == StyleBoxSizing::BorderBox);
    let outer = |declared: Option<f32>, measured: f32, chrome: f32| match declared {
        Some(size) if border_box => size.max(chrome),
        Some(size) => size + chrome,
        None => measured + chrome,
    };
    let width = outer(
        style.and_then(|s| s.width),
        measured.width,
        padding_x + border_x,
    );
    let height = outer(
        style.and_then(|s| s.height),
        measured.height,
        padding_y + border_y,
    );
    // The box sits on the baseline of its last line of text, and on its
    // bottom edge when it has none or clips what overflows.
    let baseline = if text.is_empty()
        || arena.tag(child) == "textarea"
        || style.is_some_and(|s| s.overflow_clips)
    {
        height
    } else {
        let line = font.measure(text_style, " ");
        let top = style.map_or(0.0, |s| s.padding.top + s.border.top.width);
        top + measured.height - (line.height - line.baseline)
    };
    (width, height, baseline)
}

/// The content-box width an element declares in pixels, if it does.
fn definite_content_width(style: Option<&ComputedStyle>) -> Option<f32> {
    let style = style?;
    let width = style.width?;
    Some(if style.box_sizing == StyleBoxSizing::BorderBox {
        (width
            - style.padding.left
            - style.padding.right
            - style.border.left.width
            - style.border.right.width)
            .max(0.0)
    } else {
        width
    })
}

/// Lays out the `inline-block` `node` that has children, on its own: its
/// width is the shrink-to-fit one (its content's width, or the line's if that
/// is narrower and the content can wrap), and its baseline is the last line
/// of the text it holds. Returns its border box size, that baseline and the
/// boxes of everything inside it.
fn nested_block(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    line_width: Option<f32>,
) -> Result<(f32, f32, f32, NestedBlock), LayoutError> {
    type Laid = (
        HashMap<NodeId, BoxLayout>,
        HashMap<NodeId, ContentExtent>,
        Vec<(NodeId, InlineContext)>,
    );
    let lay = |font: &mut florui_text::Font, width: AvailableSpace| -> Result<Laid, LayoutError> {
        let mut tree: TaffyTree<LeafContext> = TaffyTree::new();
        tree.disable_rounding();
        let mut ids = HashMap::new();
        let mut leaves = Vec::new();
        let id = build_node(font, arena, styles, node, &mut tree, &mut ids, &mut leaves)
            .map_err(LayoutError)?;
        let root = tree
            .new_with_children(
                taffy::Style {
                    display: Display::Block,
                    ..Default::default()
                },
                &[id],
            )
            .map_err(LayoutError)?;
        run_layout(
            font,
            &mut tree,
            root,
            Size {
                width,
                height: AvailableSpace::MaxContent,
            },
            ids.len(),
        )?;
        let (layouts, extents) = collect_layouts(
            font,
            arena,
            styles,
            &tree,
            ids.iter().map(|(&node, &id)| (node, id)),
            &leaves,
            |node| ids[&node],
        )?;
        Ok((layouts, extents, leaves))
    };
    let mut laid = lay(font, AvailableSpace::MaxContent)?;
    let natural = laid.0[&node].width;
    if let Some(line_width) = line_width
        && natural > line_width
    {
        let narrowest = lay(font, AvailableSpace::MinContent)?.0[&node].width;
        let target = line_width.max(narrowest);
        if target < natural {
            laid = lay(font, AvailableSpace::Definite(target))?;
        }
    }
    let (mut layouts, mut extents, leaves) = laid;
    let own = layouts[&node];
    let contexts: HashMap<NodeId, &InlineContext> = leaves
        .iter()
        .map(|(node, context)| (*node, context))
        .collect();
    let baseline =
        last_line_baseline(font, arena, styles, &layouts, &contexts, node).unwrap_or(own.height);
    layouts.remove(&node);
    extents.remove(&node);
    Ok((
        own.width,
        own.height,
        baseline,
        NestedBlock { layouts, extents },
    ))
}

/// How far down `node`'s border box the baseline of its last line of text
/// is, or `None` when it has no line (or clips its overflow, which makes its
/// bottom edge the baseline instead).
fn last_line_baseline(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    contexts: &HashMap<NodeId, &InlineContext>,
    node: NodeId,
) -> Option<f32> {
    let style = styles.get(&node);
    if style.is_some_and(|s| s.overflow_clips) {
        return None;
    }
    let layout = layouts.get(&node)?;
    let edge = |pick: fn(&ComputedStyle) -> f32| style.map_or(0.0, pick);
    let top = edge(|s| s.padding.top + s.border.top.width);
    let bottom = edge(|s| s.padding.bottom + s.border.bottom.width);
    if let Some(context) = contexts.get(&node) {
        let across =
            edge(|s| s.padding.left + s.padding.right + s.border.left.width + s.border.right.width);
        let items = to_inline_items(context);
        let shaped = font.shape_inline_tree_memoized(
            &context.elements,
            &items,
            Some((layout.width - across).max(0.0)),
        );
        return Some(top + shaped.last_baseline);
    }
    if is_anonymous(node) {
        return None;
    }
    let children = arena.children(node);
    if children.is_empty() {
        let text = leaf_text(arena, node);
        if text.is_empty() || arena.tag(node) == "textarea" {
            return None;
        }
        let line = font.measure(style.map_or_else(default_text_style, text_style_of), " ");
        return Some(layout.height - bottom - (line.height - line.baseline));
    }
    let blocks: Vec<NodeId> = match mixed_parts(arena, styles, node) {
        Some(parts) => {
            let mut run_number = 0;
            parts
                .iter()
                .map(|part| match part {
                    Part::Block(child) => *child,
                    Part::Run(_) => {
                        run_number += 1;
                        anonymous_id(node, run_number - 1)
                    }
                })
                .collect()
        }
        None => children.to_vec(),
    };
    blocks.iter().rev().find_map(|&child| {
        let child_style = styles.get(&child);
        if child_style.is_some_and(|s| {
            s.display == StyleDisplay::None || s.position == StylePosition::Absolute
        }) {
            return None;
        }
        let at = layouts.get(&child)?.y;
        last_line_baseline(font, arena, styles, layouts, contexts, child).map(|b| at + b)
    })
}

/// Builds `node`'s inline formatting context from [`Arena::inline_items`]:
/// a bare text item inherits its element's font (real CSS: a text node has no
/// style of its own), an `Inline` element child is an element of its own with
/// its own resolved style, nested to any depth, and an `InlineBlock` child is
/// one atomic box.
fn build_inline_content_items(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    laid: Laid<'_>,
) -> InlineContext {
    let mut context = InlineContext {
        elements: vec![florui_text::InlineElement {
            parent: 0,
            style: node_text_style(styles.get(&node)),
        }],
        items: Vec::new(),
        nodes: vec![node],
        hidden: Vec::new(),
    };
    flatten_inline(font, arena, styles, node, 0, &mut context, laid);
    context
}

/// [`build_inline_content_items`] for one run of `node`'s content, which is
/// an anonymous block of its own whose text style is `node`'s.
fn build_run_content_items(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    run: std::ops::Range<usize>,
    laid: Laid<'_>,
) -> InlineContext {
    let mut context = InlineContext {
        elements: vec![florui_text::InlineElement {
            parent: 0,
            style: node_text_style(styles.get(&node)),
        }],
        items: Vec::new(),
        nodes: vec![node],
        hidden: Vec::new(),
    };
    flatten_items(
        font,
        arena,
        styles,
        &arena.inline_items(node)[run],
        0,
        &mut context,
        laid,
    );
    context
}

/// Adds the inline items of `node`, which is element `element` of `context`.
fn flatten_inline(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    node: NodeId,
    element: usize,
    context: &mut InlineContext,
    laid: Laid<'_>,
) {
    flatten_items(
        font,
        arena,
        styles,
        arena.inline_items(node),
        element,
        context,
        laid,
    );
}

/// Adds `items`, which belong to element `element` of `context`.
fn flatten_items(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    items: &[StyleInlineItem],
    element: usize,
    context: &mut InlineContext,
    laid: Laid<'_>,
) {
    for item in items {
        match item {
            StyleInlineItem::Text(text) => context.items.push(InlineContentItem::Text {
                text: text.clone(),
                element,
            }),
            StyleInlineItem::Element(child) => {
                let child_style = styles.get(child);
                match child_style.map(|s| s.display) {
                    Some(StyleDisplay::None) => context.hidden.push(*child),
                    Some(StyleDisplay::InlineBlock) => {
                        let (width, height, baseline, nested) = if arena.children(*child).is_empty()
                        {
                            let (width, height, baseline) =
                                measure_inline_block_intrinsic_size(font, arena, styles, *child);
                            (width, height, baseline, None)
                        } else {
                            // Painting finds the box where layout put it.
                            let known = laid.and_then(|(boxes, scale)| {
                                let own = boxes.get(child)?;
                                let marker = boxes.get(&baseline_id(*child))?;
                                Some((own.width / scale, own.height / scale, marker.y / scale))
                            });
                            if let Some((width, height, baseline)) = known {
                                (width, height, baseline, None)
                            } else {
                                let line_width =
                                    definite_content_width(styles.get(&context.nodes[0]));
                                match nested_block(font, arena, styles, *child, line_width) {
                                    Ok((width, height, baseline, nested)) => {
                                        (width, height, baseline, Some(std::sync::Arc::new(nested)))
                                    }
                                    Err(_) => (0.0, 0.0, 0.0, None),
                                }
                            }
                        };
                        context.items.push(InlineContentItem::Box {
                            child: *child,
                            width,
                            height,
                            baseline,
                            element,
                            vertical_align: node_text_style(child_style).vertical_align,
                            nested,
                        });
                    }
                    _ => {
                        context.elements.push(florui_text::InlineElement {
                            parent: element,
                            style: node_text_style(child_style),
                        });
                        let own = context.elements.len() - 1;
                        context.nodes.push(*child);
                        let sides = |style: &ComputedStyle, end: bool| {
                            if end {
                                (
                                    style.padding.right + style.border.right.width,
                                    style.margin.right.unwrap_or(0.0),
                                )
                            } else {
                                (
                                    style.padding.left + style.border.left.width,
                                    style.margin.left.unwrap_or(0.0),
                                )
                            }
                        };
                        if let Some(style) = child_style {
                            let (inside, margin) = sides(style, false);
                            if inside + margin > 0.0 {
                                context.items.push(InlineContentItem::Spacer {
                                    element: own,
                                    inside,
                                    margin,
                                    end: false,
                                });
                            }
                        }
                        flatten_inline(font, arena, styles, *child, own, context, laid);
                        if let Some(style) = child_style {
                            let (inside, margin) = sides(style, true);
                            if inside + margin > 0.0 {
                                context.items.push(InlineContentItem::Spacer {
                                    element: own,
                                    inside,
                                    margin,
                                    end: true,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Whether `node` is a real inline formatting context per this crate's own
/// layout decision — `florui-paint` needs the identical predicate so its
/// own painting matches exactly what [`compute_layout`] actually did for
/// this node, rather than duplicating (and risking drifting from) this
/// logic in a second crate.
pub fn is_inline_formatting_context(arena: &Tree, styles: &StyleMap, node: NodeId) -> bool {
    needs_inline_layout(arena, styles, node)
}

/// An inline formatting context shaped for painting: the lines, and the arena
/// node each element of them (the container is element `0`) belongs to.
pub struct InlineShaped {
    pub layout: std::sync::Arc<florui_text::InlineLayout>,
    pub nodes: Vec<NodeId>,
    /// The parent element of each element; the container's own is `0`.
    pub parents: Vec<usize>,
}

/// Whether `node` is an inline element that flows in the lines of an inline
/// formatting context, which paints its box and text itself.
pub fn is_flowed_inline(arena: &Tree, styles: &StyleMap, node: NodeId) -> bool {
    if styles.get(&node).map(|s| s.display) != Some(StyleDisplay::Inline) {
        return false;
    }
    // The outermost inline element on the way up is the one that is a child
    // of the container; it flows only if everything in it is inline-level.
    let mut top = node;
    let mut parent = arena.parent(node);
    while let Some(candidate) = parent {
        if styles.get(&candidate).map(|s| s.display) == Some(StyleDisplay::Inline) {
            top = candidate;
            parent = arena.parent(candidate);
        } else {
            return needs_inline_layout(arena, styles, candidate)
                || (inline_subtree_is_inline(arena, styles, top)
                    && mixed_parts(arena, styles, candidate).is_some());
        }
    }
    false
}

/// The ids of the anonymous blocks around the runs of inline content of
/// `node`, in order; empty unless its content mixes such runs with
/// block-level children.
pub fn anonymous_blocks(arena: &Tree, styles: &StyleMap, node: NodeId) -> Vec<NodeId> {
    let Some(parts) = mixed_parts(arena, styles, node) else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|part| matches!(part, Part::Run(_)))
        .enumerate()
        .map(|(run, _)| anonymous_id(node, run))
        .collect()
}

/// [`shape_inline_formatting_context`] for the anonymous block `block` of a
/// container, from [`anonymous_blocks`].
pub fn shape_anonymous_block(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    laid: (&HashMap<NodeId, BoxLayout>, f32),
    block: NodeId,
    wrap_width: f32,
) -> Option<InlineShaped> {
    let container = anonymous_container(block);
    let run = block & 0xffff;
    let parts = mixed_parts(arena, styles, container)?;
    let Part::Run(range) = parts
        .iter()
        .filter(|part| matches!(part, Part::Run(_)))
        .nth(run)?
    else {
        return None;
    };
    let context =
        build_run_content_items(font, arena, styles, container, range.clone(), Some(laid));
    let items = to_inline_items(&context);
    Some(InlineShaped {
        layout: font.shape_inline_tree_memoized(&context.elements, &items, Some(wrap_width)),
        nodes: context.nodes.clone(),
        parents: context.elements.iter().map(|e| e.parent).collect(),
    })
}

/// Shapes `node`'s own real inline formatting context at `wrap_width` —
/// the same content [`compute_layout`] itself measured this node with,
/// rebuilt fresh here since `florui-paint` doesn't share layout's own
/// internal `TaffyTree` — so `florui-paint` can paint its glyph runs and the
/// boxes of its elements. Only meaningful when [`is_inline_formatting_context`]
/// is true for `node`; returns `None` otherwise (nothing to shape).
pub fn shape_inline_formatting_context(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    laid: (&HashMap<NodeId, BoxLayout>, f32),
    node: NodeId,
    wrap_width: f32,
) -> Option<InlineShaped> {
    if !needs_inline_layout(arena, styles, node) {
        return None;
    }
    let context = build_inline_content_items(font, arena, styles, node, Some(laid));
    let items = to_inline_items(&context);
    Some(InlineShaped {
        layout: font.shape_inline_tree_memoized(&context.elements, &items, Some(wrap_width)),
        nodes: context.nodes.clone(),
        parents: context.elements.iter().map(|e| e.parent).collect(),
    })
}

/// A fragment's border box as `(x0, y0, x1, y1)` in content coordinates: its
/// content area grown by the element's vertical padding and border.
fn fragment_rect(
    styles: &StyleMap,
    node: NodeId,
    fragment: &florui_text::InlineFragment,
) -> (f32, f32, f32, f32) {
    let (above, below) = styles.get(&node).map_or((0.0, 0.0), |s| {
        (
            s.padding.top + s.border.top.width,
            s.padding.bottom + s.border.bottom.width,
        )
    });
    (
        fragment.x,
        fragment.y - above,
        fragment.x + fragment.width,
        fragment.y + fragment.ascent + fragment.descent + below,
    )
}

/// Computes block-layout geometry for every node in `arena`, using
/// `styles` for sizing/spacing. `available` is the space the layout root
/// itself is given (e.g. the preview window's content area). `font` is the
/// caller's own long-lived instance — loading one builds a whole Parley
/// `FontContext` (and, with fontique's default `system_fonts: true`,
/// enumerates the system's installed fonts), too expensive to redo on every
/// call; a caller with more than one render should build it once and reuse
/// it, passing the very same instance to [`florui_paint::paint_to_buffer`]
/// too since that shapes and rasterizes the identical glyphs this crate
/// measured.
pub fn compute_layout(
    font: &mut florui_text::Font,
    arena: &Arena,
    styles: &StyleMap,
    available: Size<AvailableSpace>,
) -> Result<HashMap<NodeId, BoxLayout>, LayoutError> {
    compute_layout_with_content_extents(font, arena, styles, available).map(|(layouts, _)| layouts)
}

type LayoutsAndContentExtents = (HashMap<NodeId, BoxLayout>, HashMap<NodeId, ContentExtent>);

/// Same as [`compute_layout`], plus each node's real [`ContentExtent`] —
/// kept as a separate function, rather than changing [`compute_layout`]'s
/// own return shape, since content extent is only needed by
/// [`compute_with_style`]'s own callers (a scroll container) and
/// [`compute_layout`] alone already has dozens of call sites across this
/// workspace's own tests and benches that have no use for it.
///
/// `pub`, not crate-private, for the same reason [`compute_with_style`]'s
/// own container-query branch calls this a second time with adjusted
/// `styles` rather than re-deriving them: a caller that already has a
/// correct `styles` map from one real [`compute_with_style`] call (e.g.
/// `florui-platform`'s own select-width fix-up, which patches one node's
/// resolved `width` after measuring text) can get corrected geometry
/// without re-running the cascade or touching an [`AnimationTimeline`].
pub fn compute_layout_with_content_extents(
    font: &mut florui_text::Font,
    arena: &Arena,
    styles: &StyleMap,
    available: Size<AvailableSpace>,
) -> Result<LayoutsAndContentExtents, LayoutError> {
    // The tree is built and laid out inside one grown stack: an inline-block's
    // own content is laid out while the tree is built, and would otherwise find
    // the small stack of the thread too short, and grow its own each time.
    stacker::maybe_grow(RECURSION_RED_ZONE, RECURSION_STACK_SIZE, || {
        compute_layout_in_place(font, arena, styles, available)
    })
}

fn compute_layout_in_place(
    font: &mut florui_text::Font,
    arena: &Arena,
    styles: &StyleMap,
    available: Size<AvailableSpace>,
) -> Result<LayoutsAndContentExtents, LayoutError> {
    let mut tree: TaffyTree<LeafContext> = TaffyTree::with_capacity(styles.len() + 2);
    // Text is measured at its exact width and paint reshapes it at the laid
    // out width, so a width rounded down (a 36.26px label laid out at 36px)
    // wraps it at its only break opportunity. Browsers lay out in
    // fractional units and snap at paint; paint here already snaps fills.
    tree.disable_rounding();
    let mut taffy_ids: HashMap<NodeId, taffy::NodeId> = HashMap::with_capacity(styles.len());
    // Every inline-formatting-context leaf built below, so the second pass
    // after layout can derive its `Box` items' own `BoxLayout` entries —
    // see that pass's own comment for why they aren't ordinary Taffy nodes.
    let mut inline_leaves: Vec<(NodeId, InlineContext)> = Vec::new();

    layout_root_group(
        font,
        arena,
        styles,
        arena.document_roots(),
        available,
        &mut tree,
        &mut taffy_ids,
        &mut inline_leaves,
    )?;

    // A second, independent layout pass, its own synthetic wrapper -- so
    // Taffy treats it as its own top of computation, landing at (0, 0)
    // relative to the real viewport `available` describes, not shifted by
    // anything the document pass above computed. Skipped entirely when
    // there's nothing to portal, the common case, at the cost of one
    // `is_empty` check.
    if !arena.overlay_roots().is_empty() {
        layout_root_group(
            font,
            arena,
            styles,
            arena.overlay_roots(),
            available,
            &mut tree,
            &mut taffy_ids,
            &mut inline_leaves,
        )?;
    }

    collect_layouts(
        font,
        arena,
        styles,
        &tree,
        taffy_ids.iter().map(|(&node, &id)| (node, id)),
        &inline_leaves,
        |node| taffy_ids[&node],
    )
}

/// Reads every node's box and content extent back out of a laid-out `tree`,
/// then places the boxes of each inline formatting context's own items (see
/// the second pass below). `id_of` finds the Taffy node of an arena node.
fn collect_layouts(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    tree: &TaffyTree<LeafContext>,
    nodes: impl Iterator<Item = (NodeId, taffy::NodeId)>,
    inline_leaves: &[(NodeId, InlineContext)],
    id_of: impl Fn(NodeId) -> taffy::NodeId,
) -> Result<LayoutsAndContentExtents, LayoutError> {
    let node_count = nodes.size_hint().1.unwrap_or_default();
    let mut result = HashMap::with_capacity(node_count);
    let mut content_extents = HashMap::with_capacity(node_count);
    for (node_id, tid) in nodes {
        let layout = tree.layout(tid).map_err(LayoutError)?;
        result.insert(
            node_id,
            BoxLayout {
                x: layout.location.x,
                y: layout.location.y,
                width: layout.size.width,
                height: layout.size.height,
            },
        );
        content_extents.insert(
            node_id,
            ContentExtent {
                width: layout.scrollable_overflow_rect.right - layout.scrollable_overflow_rect.left,
                height: layout.scrollable_overflow_rect.bottom
                    - layout.scrollable_overflow_rect.top,
            },
        );
    }

    // Second pass: the boxes of an inline formatting context's elements and
    // atomic boxes are not Taffy nodes of their own — they were absorbed into
    // their container's single leaf above, since Taffy has no inline display
    // mode to give them one — so their own `BoxLayout` is derived here
    // instead, by rerunning the exact same inline layout at the container's
    // now-final content width. Deterministic, not a guess: the same width
    // always produces the same wrap points and positions, and this is the
    // width the measure closure used for Taffy's own final "perform layout"
    // call on this same leaf.
    for (container, context) in inline_leaves {
        for &hidden in &context.hidden {
            let mut stack = vec![hidden];
            while let Some(node) = stack.pop() {
                result.entry(node).or_insert(BoxLayout {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                });
                content_extents.entry(node).or_insert(ContentExtent {
                    width: 0.0,
                    height: 0.0,
                });
                stack.extend(arena.children(node));
            }
        }
        let nothing_to_place = context.nodes.len() == 1
            && !context
                .items
                .iter()
                .any(|item| matches!(item, InlineContentItem::Box { .. }));
        if nothing_to_place {
            continue;
        }
        let tid = id_of(*container);
        let layout = tree.layout(tid).map_err(LayoutError)?;
        let container_style = styles.get(container);
        let edge =
            |pick: fn(&ComputedStyle) -> (f32, f32)| container_style.map_or((0.0, 0.0), pick);
        let (padding_left, padding_right) = edge(|s| (s.padding.left, s.padding.right));
        let (border_left, border_right) = edge(|s| (s.border.left.width, s.border.right.width));
        let (padding_top, border_top) = edge(|s| (s.padding.top, s.border.top.width));
        let content_width =
            (layout.size.width - padding_left - padding_right - border_left - border_right)
                .max(0.0);
        let items = to_inline_items(context);
        let shaped =
            font.shape_inline_tree_memoized(&context.elements, &items, Some(content_width));
        // Where the content starts, in the box of the container: its content
        // box, or where an anonymous block is among its children.
        let origin = if is_anonymous(*container) {
            (layout.location.x, layout.location.y)
        } else {
            (border_left + padding_left, border_top + padding_top)
        };

        // Each element's border box in content coordinates: its fragments'
        // content areas grown by the vertical padding and border, merged.
        let count = context.elements.len();
        let mut rects: Vec<Option<(f32, f32, f32, f32)>> = vec![None; count];
        let merge = |slot: &mut Option<(f32, f32, f32, f32)>, rect: (f32, f32, f32, f32)| {
            *slot = Some(match *slot {
                None => rect,
                Some((x0, y0, x1, y1)) => (
                    x0.min(rect.0),
                    y0.min(rect.1),
                    x1.max(rect.2),
                    y1.max(rect.3),
                ),
            });
        };
        let mut pieces: Vec<Vec<(f32, f32, f32, f32)>> = vec![Vec::new(); count];
        for fragment in &shaped.fragments {
            let rect = fragment_rect(styles, context.nodes[fragment.element], fragment);
            merge(&mut rects[fragment.element], rect);
            pieces[fragment.element].push(rect);
        }
        let origin_of = |element: usize, rects: &[Option<(f32, f32, f32, f32)>]| {
            if element == 0 {
                (origin.0, origin.1)
            } else {
                rects[element].map_or(origin, |r| (r.0, r.1))
            }
        };
        for element in 1..count {
            let Some((x0, y0, x1, y1)) = rects[element] else {
                continue;
            };
            let parent = origin_of(context.elements[element].parent, &rects);
            let node = context.nodes[element];
            let parent_is_container = context.elements[element].parent == 0;
            let (rx, ry) = if parent_is_container {
                (x0 + parent.0, y0 + parent.1)
            } else {
                (x0 - parent.0, y0 - parent.1)
            };
            result.insert(
                node,
                BoxLayout {
                    x: rx,
                    y: ry,
                    width: x1 - x0,
                    height: y1 - y0,
                },
            );
            // An element on several lines is hit only inside its piece of each.
            if pieces[element].len() > 1 {
                for (number, piece) in pieces[element].iter().enumerate() {
                    result.insert(
                        piece_id(node, number),
                        BoxLayout {
                            x: piece.0 - x0,
                            y: piece.1 - y0,
                            width: piece.2 - piece.0,
                            height: piece.3 - piece.1,
                        },
                    );
                }
            }
            content_extents.insert(
                node,
                ContentExtent {
                    width: x1 - x0,
                    height: y1 - y0,
                },
            );
        }

        type Placed<'a> = (f32, f32, usize, Option<(&'a NestedBlock, f32)>);
        let box_sizes: HashMap<NodeId, Placed<'_>> = context
            .items
            .iter()
            .filter_map(|item| match item {
                InlineContentItem::Box {
                    child,
                    width,
                    height,
                    element,
                    nested,
                    baseline,
                    ..
                } => Some((
                    *child,
                    (
                        *width,
                        *height,
                        *element,
                        nested.as_deref().map(|nested| (nested, *baseline)),
                    ),
                )),
                _ => None,
            })
            .collect();
        for positioned in &shaped.boxes {
            let child = positioned.id as NodeId;
            if let Some(&(width, height, element, nested)) = box_sizes.get(&child) {
                if let Some((nested, baseline)) = nested {
                    result.insert(
                        baseline_id(child),
                        BoxLayout {
                            x: 0.0,
                            y: baseline,
                            width: 0.0,
                            height: 0.0,
                        },
                    );
                    result.extend(nested.layouts.iter().map(|(&node, &layout)| (node, layout)));
                    content_extents
                        .extend(nested.extents.iter().map(|(&node, &extent)| (node, extent)));
                }
                let parent = origin_of(element, &rects);
                let (x, y) = if element == 0 {
                    (positioned.x + parent.0, positioned.y + parent.1)
                } else {
                    (positioned.x - parent.0, positioned.y - parent.1)
                };
                result.insert(
                    child,
                    BoxLayout {
                        x,
                        y,
                        width,
                        height,
                    },
                );
                // Not an independent Taffy node, so it has no
                // `scrollable_overflow_rect` of its own; its content extent is
                // just its own box size, the correct value for a leaf that
                // never overflows itself.
                content_extents.insert(child, ContentExtent { width, height });
            }
        }
    }

    Ok((result, content_extents))
}

/// Builds `roots` (via [`build_node`], already root-agnostic) and lays
/// them out together against `available`, wrapped in one synthetic block
/// container so multiple siblings (`view!` can produce a `Fragment`, and
/// so can a portal registry) have somewhere to stack — the exact
/// behavior [`compute_layout`] always had for its one root group,
/// factored out so it can run a second, independent time for overlay
/// roots. Each call's own synthetic wrapper is its own top of
/// computation as far as Taffy is concerned, so a second call's roots
/// land at `(0, 0)` relative to `available` regardless of what an
/// earlier call already computed — not nested inside it, not offset by
/// it.
#[allow(clippy::too_many_arguments)]
fn layout_root_group(
    font: &mut florui_text::Font,
    arena: &Arena,
    styles: &StyleMap,
    roots: &[NodeId],
    available: Size<AvailableSpace>,
    tree: &mut TaffyTree<LeafContext>,
    taffy_ids: &mut HashMap<NodeId, taffy::NodeId>,
    inline_leaves: &mut Vec<(NodeId, InlineContext)>,
) -> Result<(), LayoutError> {
    for &root in roots {
        build_node(font, arena, styles, root, tree, taffy_ids, inline_leaves)
            .map_err(LayoutError)?;
    }

    // A synthetic block container wraps every root in this group so
    // multiple top-level elements have somewhere to stack; its own id is
    // never looked up, only real `arena` nodes are.
    let root_children: Vec<taffy::NodeId> = roots.iter().map(|id| taffy_ids[id]).collect();
    let synthetic_root = tree
        .new_with_children(
            taffy::Style {
                display: Display::Block,
                ..Default::default()
            },
            &root_children,
        )
        .map_err(LayoutError)?;

    run_layout(font, tree, synthetic_root, available, taffy_ids.len())
}

/// Lays out the tree under `synthetic_root` against `available`; the half of
/// [`layout_root_group`] that is the same whether the tree was just built or
/// kept from the last layout (see [`LayoutCache`]).
fn run_layout(
    font: &mut florui_text::Font,
    tree: &mut TaffyTree<LeafContext>,
    synthetic_root: taffy::NodeId,
    available: Size<AvailableSpace>,
    node_count: usize,
) -> Result<(), LayoutError> {
    // Taffy queries a leaf's intrinsic size multiple times during one
    // layout pass (an intrinsic min/max-content query, then a final
    // definite pass), often landing on more than one width and sometimes
    // the identical one more than once — keyed by the leaf's own Taffy
    // node id since that's what `compute_layout_with_measure` hands the
    // closure below on every call, this lets `measure_leaf` reuse the one
    // Parley layout each leaf actually needs across every one of those
    // calls instead of reshaping from scratch each time. Scoped to this
    // one call: a fresh, empty map every call, nothing persisted across
    // renders or between this group and any other.
    let mut shaping_caches: HashMap<taffy::NodeId, LeafScratch> =
        HashMap::with_capacity(node_count);

    // Taffy's own block/flex/grid algorithms recurse once per tree depth
    // internally — third-party code this crate doesn't control, and deep
    // enough to overflow the default stack well under 1,000 levels.
    stacker::maybe_grow(RECURSION_RED_ZONE, RECURSION_STACK_SIZE, || {
        tree.compute_layout_with_measure(
            synthetic_root,
            available,
            |inputs, node_id, mut context, style| {
                let mut measured_baseline = None;
                // Only a real `LeafContext::Text` leaf ever reads or writes
                // a slot in `shaping_caches` — touching the map (hashing
                // `node_id`, inserting an entry) for every other leaf too
                // would tax the common case (an explicitly-sized `<div>`
                // with no text, needing no shaping cache at all) for a
                // cache it never uses. `throwaway_cache` is a cheap
                // stack-local stand-in for that common case; it's read and
                // discarded, never reused across calls, since there's
                // nothing to reuse.
                let mut throwaway_cache = LeafScratch::default();
                let shaping_cache = match context.as_deref() {
                    Some(LeafContext::Text(_)) => shaping_caches.entry(node_id).or_default(),
                    _ => &mut throwaway_cache,
                };
                let mut output = compute_leaf_layout(
                    inputs,
                    style,
                    |_, _| 0.0,
                    |known_dimensions, available_space| {
                        measure_leaf(
                            font,
                            shaping_cache,
                            context.as_deref_mut(),
                            known_dimensions,
                            available_space,
                            &mut measured_baseline,
                        )
                    },
                );

                // `measure_leaf` already shaped this text once above and,
                // via `measured_baseline`, handed back the baseline that
                // came out of that same shaping — reused here instead of
                // shaping the identical text a second time just to read
                // `.baseline` off it. Only falls back to a fresh (unwrapped)
                // measurement when `compute_leaf_layout` never called the
                // closure above at all: it skips calling its own measure
                // closure when both dimensions are already known (an
                // explicit width *and* height), but a baseline is still
                // meaningful there — real CSS still aligns an
                // explicitly-sized text box by its text's baseline, not by
                // treating it as baseline-less. Wrap width is irrelevant
                // either way: see `florui_text::TextMetrics::baseline`'s own
                // doc for why.
                let baseline = measured_baseline.or_else(|| match context.as_deref() {
                    Some(LeafContext::Text(t)) => Some(
                        shaping_cache
                            .answer(None, |shaping| {
                                font.measure_memoized(shaping, t.style, &t.text, None)
                            })
                            .baseline,
                    ),
                    Some(LeafContext::Inline(context)) => {
                        let items = to_inline_items(context);
                        Some(
                            font.shape_inline_tree_memoized(&context.elements, &items, None)
                                .baseline,
                        )
                    }
                    // A replaced element has no text baseline to recover,
                    // and neither does a fixed-size control.
                    _ => None,
                });
                if let Some(baseline) = baseline {
                    output.baselines = Baselines::from_first(Some(baseline));
                }
                output
            },
        )
    })
    .map_err(LayoutError)?;

    Ok(())
}

/// Same job as calling [`florui_style::compute`] then [`compute_layout`] in
/// sequence — which is exactly what this does when `rules` has no
/// `@container` block at all (`Rule::has_container_queries`), at no extra
/// cost. When it does, a `@container` condition's match depends on real,
/// already-laid-out geometry `florui-style` alone can't produce (see
/// `florui_style::container_query_adapter`'s own module doc, not public
/// from here — this is the orchestration its doc points callers to), so
/// this runs a bounded, three-step sequence instead of the naive one:
///
/// 1. A base style+layout pass with every `@container` condition treated
///    as non-matching (real CSS doesn't let a container's own
///    `container-type` be gated behind a query on itself either).
/// 2. Every node's own real per-block signature, resolved against that
///    base pass's real sizes ([`florui_style::resolve_container_query_signatures`]);
///    grouped by distinct signature, one more style-only pass per group
///    ([`florui_style::compute_with_container_query_signature`]), merged
///    by node.
/// 3. One final layout pass with the merged, now-correct styles.
///
/// Never more than two layout passes, regardless of how many distinct
/// `@container` signatures exist in the tree — there is no fixed-point
/// iteration here to bound, only this fixed sequence. That's exact only
/// when a container's own size on its contained axis doesn't itself
/// depend on its query-gated descendants (true for the recommended,
/// common case: a definite or stretched container size, not
/// `width: fit-content`/`flex-basis: auto` shrink-to-fit sizing driven by
/// query-gated children) — a container that violates this is a documented
/// gap, not silently assumed correct.
///
/// `transition`/`@keyframes` animation only threads through `timeline` for
/// the base pass (step 1) — a node whose resolved declarations actually
/// differ between the base pass and its own matched signature does not
/// yet animate a transition on that difference; every per-signature pass
/// in step 2 uses its own disposable timeline so it can't corrupt
/// `timeline`'s real cross-frame bookkeeping for everything else. A
/// documented gap, not a silent approximation.
pub fn compute_with_style(
    font: &mut florui_text::Font,
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: Viewport,
    timeline: &mut AnimationTimeline,
    available: Size<AvailableSpace>,
) -> Result<LayoutResult, LayoutError> {
    compute_with_style_in(
        None, font, arena, rules, state, viewport, timeline, available,
    )
}

/// [`compute_with_style`] that lays the tree out in `cache`, so what did not
/// change since the last call is not laid out again. The result is the same.
/// A stylesheet with `@container` rules lays out afresh, as the plain call does.
#[allow(clippy::too_many_arguments)]
pub fn compute_with_style_cached(
    cache: &mut LayoutCache,
    font: &mut florui_text::Font,
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: Viewport,
    timeline: &mut AnimationTimeline,
    available: Size<AvailableSpace>,
) -> Result<LayoutResult, LayoutError> {
    compute_with_style_in(
        Some(cache),
        font,
        arena,
        rules,
        state,
        viewport,
        timeline,
        available,
    )
}

#[allow(clippy::too_many_arguments)]
fn compute_with_style_in(
    mut cache: Option<&mut LayoutCache>,
    font: &mut florui_text::Font,
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: Viewport,
    timeline: &mut AnimationTimeline,
    available: Size<AvailableSpace>,
) -> Result<LayoutResult, LayoutError> {
    let has_container_queries = rules.iter().any(Rule::has_container_queries);
    let (base_styles, restyled) = {
        let _span = florui_profile::span(florui_profile::Phase::Cascade);
        match cache.as_deref_mut() {
            Some(cache) if !has_container_queries => {
                cache.styles(arena, rules, state, viewport, timeline)
            }
            _ => (
                florui_style::compute(arena, rules, state, viewport, timeline),
                true,
            ),
        }
    };
    if restyled {
        florui_profile::count(
            florui_profile::Counter::NodesStyled,
            base_styles.len() as u64,
        );
    }
    let (base_layouts, base_content_extents) = {
        let _span = florui_profile::span(florui_profile::Phase::Layout);
        match cache {
            Some(cache) if !has_container_queries => {
                cache.compute(font, arena, &base_styles, available)?
            }
            _ => compute_layout_with_content_extents(font, arena, &base_styles, available)?,
        }
    };
    florui_profile::count(
        florui_profile::Counter::NodesLaidOut,
        base_layouts.len() as u64,
    );

    if !has_container_queries {
        return Ok(LayoutResult {
            styles: base_styles,
            layouts: base_layouts,
            content_extents: base_content_extents,
        });
    }

    let sizes: HashMap<NodeId, ContentBoxSize> = base_layouts
        .iter()
        .map(|(&id, layout)| {
            (
                id,
                ContentBoxSize {
                    width: layout.width,
                    height: layout.height,
                },
            )
        })
        .collect();
    let cascade_span = florui_profile::span(florui_profile::Phase::Cascade);
    let signatures =
        florui_style::resolve_container_query_signatures(arena, rules, &base_styles, &sizes);

    let mut distinct_signatures: Vec<Vec<bool>> = Vec::new();
    for signature in signatures.values() {
        if !distinct_signatures.contains(signature) {
            distinct_signatures.push(signature.clone());
        }
    }

    let mut final_styles = StyleMap::with_capacity(base_styles.len());
    for signature in &distinct_signatures {
        let mut disposable_timeline = AnimationTimeline::default();
        let pass_styles = florui_style::compute_with_container_query_signature(
            arena,
            rules,
            state,
            viewport,
            &mut disposable_timeline,
            signature,
        );
        for (&id, node_signature) in &signatures {
            if node_signature == signature
                && let Some(style) = pass_styles.get(&id)
            {
                final_styles.insert(id, style.clone());
            }
        }
    }

    drop(cascade_span);
    florui_profile::count(
        florui_profile::Counter::NodesStyled,
        final_styles.len() as u64,
    );
    let _span = florui_profile::span(florui_profile::Phase::Layout);
    let (final_layouts, final_content_extents) =
        compute_layout_with_content_extents(font, arena, &final_styles, available)?;
    florui_profile::count(
        florui_profile::Counter::NodesLaidOut,
        final_layouts.len() as u64,
    );
    Ok(LayoutResult {
        styles: final_styles,
        layouts: final_layouts,
        content_extents: final_content_extents,
    })
}

/// A leaf with no text measures at `0x0` — `compute_leaf_layout` only
/// falls back to this for an axis that neither the node's own style nor
/// its parent's known dimensions already settled.
///
/// Wraps when a width is actually known: either the node's own explicit
/// style already resolved one (`known_dimensions.width`, e.g. a `width:
/// 200px` on this very leaf), or the space it's being measured within is
/// definite (`available_space.width`, e.g. a flex/block container with its
/// own resolved width). Neither being true means this measurement is for
/// an intrinsic/max-content size (nothing yet constrains this axis), where
/// real CSS's own behavior is also unwrapped — there is nothing to wrap
/// against. A genuine min-content query (the width of the single longest
/// unbreakable word) isn't distinguished from that case yet; see
/// `florui_text`'s own module docs for that tracked gap.
fn measure_leaf(
    font: &mut florui_text::Font,
    shaping_cache: &mut LeafScratch,
    context: Option<&mut LeafContext>,
    known_dimensions: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
    baseline_out: &mut Option<f32>,
) -> Size<f32> {
    let Some(context) = context else {
        return Size::ZERO;
    };

    let wrap_width = known_dimensions.width.or(match available_space.width {
        AvailableSpace::Definite(width) => Some(width),
        // Min-content is the longest unbreakable word: break at every chance.
        AvailableSpace::MinContent => Some(0.0),
        AvailableSpace::MaxContent => None,
    });

    match context {
        LeafContext::Text(text_context) => {
            // `measure_cached` reuses `shaping_cache`'s already-shaped
            // Parley layout across however many times Taffy calls this
            // closure for this same leaf during one layout pass — see its
            // own doc, and the cache's own doc for why re-breaking a
            // shaped layout at a new width is safe to do repeatedly.
            let metrics = shaping_cache.answer(wrap_width, |shaping| {
                font.measure_memoized(shaping, text_context.style, &text_context.text, wrap_width)
            });
            *baseline_out = Some(metrics.baseline);
            Size {
                width: metrics.width,
                height: metrics.height,
            }
        }
        LeafContext::Inline(context) => {
            // A real inline formatting context is its own build path
            // (`shape_inline`'s multi-item `RangedBuilder`), remembered whole
            // by its items and wrap width rather than by one text and style.
            let items = to_inline_items(context);
            let result = font.shape_inline_tree_memoized(&context.elements, &items, wrap_width);
            *baseline_out = Some(result.baseline);
            Size {
                width: result.width,
                height: result.height,
            }
        }
        LeafContext::Fixed(size) => Size {
            width: known_dimensions.width.unwrap_or(size.width),
            height: known_dimensions.height.unwrap_or(size.height),
        },
        LeafContext::Image(image_context) => {
            // A replaced element contributes no text baseline;
            // `baseline_out` stays whatever the caller already
            // initialized (`None`), matching Text/Inline's own behavior
            // when they have nothing to report.
            resolve_replaced_size(
                image_context.intrinsic_size,
                image_context.aspect_ratio,
                known_dimensions,
                available_space,
            )
        }
    }
}

/// Resolves a replaced element's (e.g. `<img>`) own content size against
/// `width`/`height`/`aspect-ratio` and its own intrinsic size — real CSS's
/// replaced-element sizing algorithm, verified directly against Chrome
/// rather than assumed from spec text alone (six real
/// `getBoundingClientRect` measurements against a served test page,
/// recorded in this function's own tests). The one easy-to-miss fact
/// that verification surfaced: a *plain* block box's `width: auto` always
/// fills its available width, but a replaced element with a real
/// intrinsic size uses that intrinsic size instead and does **not**
/// fill — it only falls back to filling when it has no intrinsic size at
/// all (still loading, or failed to load).
pub fn resolve_replaced_size(
    intrinsic: Option<Size<f32>>,
    aspect_ratio: florui_style::AspectRatio,
    known_dimensions: Size<Option<f32>>,
    available_space: Size<AvailableSpace>,
) -> Size<f32> {
    let ratio = resolve_aspect_ratio(aspect_ratio, intrinsic);
    let fill_width = || match available_space.width {
        AvailableSpace::Definite(width) => width,
        // A min/max-content query has nothing to fill against; fall back
        // to the intrinsic width if there is one, else contribute
        // nothing.
        AvailableSpace::MinContent | AvailableSpace::MaxContent => {
            intrinsic.map_or(0.0, |size| size.width)
        }
    };

    match (known_dimensions.width, known_dimensions.height) {
        // Both axes already pinned by CSS (explicit `width`/`height`, or
        // a container that already resolved both) — nothing left to
        // measure; `object-fit` governs how content maps into this box,
        // not this function.
        (Some(width), Some(height)) => Size { width, height },
        (Some(width), None) => Size {
            width,
            height: ratio
                .map(|r| width / r)
                .or(intrinsic.map(|size| size.height))
                .unwrap_or(0.0),
        },
        (None, Some(height)) => Size {
            width: ratio
                .map(|r| height * r)
                .or(intrinsic.map(|size| size.width))
                .unwrap_or(0.0),
            height,
        },
        (None, None) => match intrinsic {
            // A real intrinsic size always wins over available space —
            // measured directly (a 100x50 image with no CSS size at all,
            // inside a 400px container, stays 100x50; it does not fill).
            Some(size) => size,
            // No intrinsic size at all (loading, or failed): falls back
            // to an ordinary block box's own `width: auto` behavior
            // (fill), then derives height from the ratio if one exists
            // (measured: a 16:9 `aspect-ratio` in a 400px container
            // measures 400x225), or `0.0` otherwise. Chrome instead
            // shows an 18px broken-image glyph here — a UA
            // presentational detail this crate doesn't render at all,
            // so `0.0` is the honest value for "nothing to show yet"
            // rather than a copied magic number with no real meaning
            // here.
            None => {
                let width = fill_width();
                Size {
                    width,
                    height: ratio.map(|r| width / r).unwrap_or(0.0),
                }
            }
        },
    }
}

/// Resolves `aspect-ratio`'s `auto || <ratio>` grammar against whether
/// this element actually has an intrinsic ratio — see
/// [`florui_style::AspectRatio`]'s own doc for the three real behaviors
/// this distinguishes. Returns `width / height`, or `None` when nothing
/// (neither an intrinsic ratio nor an explicit one) applies.
fn resolve_aspect_ratio(
    aspect_ratio: florui_style::AspectRatio,
    intrinsic: Option<Size<f32>>,
) -> Option<f32> {
    if aspect_ratio.prefers_intrinsic {
        intrinsic
            .filter(|size| size.height > 0.0)
            .map(|size| size.width / size.height)
            .or_else(|| aspect_ratio.ratio.map(|(width, height)| width / height))
    } else {
        aspect_ratio.ratio.map(|(width, height)| width / height)
    }
}

/// Resolves `object-fit`/`object-position`'s content rect: where, and at
/// what size, a replaced element's own intrinsic content (e.g. a decoded
/// image) paints within its already-final content box. Pure geometry —
/// no painting, no decoding; `florui-paint` blits into the rect this
/// returns. `intrinsic_size` with a zero or negative axis, or a
/// zero-or-negative `box_size`, has nothing to paint, so the
/// zero-size/zero-position rect is returned rather than dividing by zero.
pub fn resolve_object_fit_content_rect(
    object_fit: florui_style::ObjectFit,
    object_position: (
        florui_style::LengthPercentage,
        florui_style::LengthPercentage,
    ),
    box_size: Size<f32>,
    intrinsic_size: Size<f32>,
) -> BoxLayout {
    if intrinsic_size.width <= 0.0
        || intrinsic_size.height <= 0.0
        || box_size.width <= 0.0
        || box_size.height <= 0.0
    {
        return BoxLayout {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }

    let intrinsic_ratio = intrinsic_size.width / intrinsic_size.height;
    let box_ratio = box_size.width / box_size.height;
    let contain_size = if box_ratio > intrinsic_ratio {
        Size {
            width: box_size.height * intrinsic_ratio,
            height: box_size.height,
        }
    } else {
        Size {
            width: box_size.width,
            height: box_size.width / intrinsic_ratio,
        }
    };

    let content_size = match object_fit {
        florui_style::ObjectFit::Fill => box_size,
        florui_style::ObjectFit::None => intrinsic_size,
        florui_style::ObjectFit::Contain => contain_size,
        florui_style::ObjectFit::Cover => {
            if box_ratio > intrinsic_ratio {
                Size {
                    width: box_size.width,
                    height: box_size.width / intrinsic_ratio,
                }
            } else {
                Size {
                    width: box_size.height * intrinsic_ratio,
                    height: box_size.height,
                }
            }
        }
        // Whichever of `None` or `Contain` is smaller — `Contain` is
        // only ever smaller than the intrinsic size when the box forced
        // it to shrink; when the box is large enough that `Contain`
        // would upscale instead, `None` (unscaled) is the smaller one.
        florui_style::ObjectFit::ScaleDown => {
            if intrinsic_size.width <= contain_size.width
                && intrinsic_size.height <= contain_size.height
            {
                intrinsic_size
            } else {
                contain_size
            }
        }
    };

    let extra_x = box_size.width - content_size.width;
    let extra_y = box_size.height - content_size.height;
    BoxLayout {
        x: object_position.0.resolve(extra_x),
        y: object_position.1.resolve(extra_y),
        width: content_size.width,
        height: content_size.height,
    }
}

/// One step of [`build_node`]'s walk: `Visit` a node (leaves resolve
/// immediately; a container defers itself behind its own children), or
/// `Build` a container once every child already has a [`taffy::NodeId`].
enum BuildStep {
    Visit(NodeId),
    Build(NodeId),
    /// A container with anonymous blocks among its children, which `Build`
    /// cannot find in the arena.
    BuildMixed(NodeId, Vec<Part>),
}

/// Builds `root`'s whole subtree into `tree`, returning its own
/// [`taffy::NodeId`]. An explicit stack, not one call frame per tree
/// level — `Build(node)` is pushed before its children's `Visit` steps,
/// so the stack's LIFO order still builds every child before its parent.
fn build_node(
    font: &mut florui_text::Font,
    arena: &Tree,
    styles: &StyleMap,
    root: NodeId,
    tree: &mut TaffyTree<LeafContext>,
    taffy_ids: &mut HashMap<NodeId, taffy::NodeId>,
    inline_leaves: &mut Vec<(NodeId, InlineContext)>,
) -> Result<taffy::NodeId, taffy::TaffyError> {
    let mut stack = vec![BuildStep::Visit(root)];

    while let Some(step) = stack.pop() {
        match step {
            BuildStep::Visit(node) => {
                let arena_children = arena.children(node);
                let kind = if arena_children.is_empty() {
                    LayoutKind::Blocks
                } else {
                    layout_kind(arena, styles, node)
                };
                let id = if arena_children.is_empty() {
                    let style = to_taffy_style(styles.get(&node));
                    // `icon` (a themable SVG icon) is the same kind of
                    // replaced element as `img` -- same intrinsic-size
                    // sizing rule, same missing channel into this crate
                    // (see `ImageContext`'s own doc), just a different
                    // registry on the platform side supplies it later.
                    if matches!(arena.tag(node), "img" | "icon") {
                        tree.new_leaf_with_context(
                            style,
                            LeafContext::Image(ImageContext {
                                // No intrinsic-size channel into this
                                // crate yet -- see `ImageContext`'s own
                                // doc.
                                intrinsic_size: None,
                                aspect_ratio: styles
                                    .get(&node)
                                    .map_or_else(Default::default, |s| s.aspect_ratio),
                            }),
                        )?
                    } else if arena.tag(node) == "textarea" {
                        let size = textarea_intrinsic_size(font, arena, node, styles.get(&node));
                        tree.new_leaf_with_context(style, LeafContext::Fixed(size))?
                    } else if leaf_text(arena, node).is_empty() {
                        tree.new_leaf(style)?
                    } else {
                        let text = leaf_text(arena, node);
                        tree.new_leaf_with_context(
                            style,
                            LeafContext::Text(TextContext {
                                text: text.to_string(),
                                style: styles
                                    .get(&node)
                                    .map_or_else(default_text_style, text_style_of),
                            }),
                        )?
                    }
                } else if kind == LayoutKind::Inline {
                    let style = to_taffy_style(styles.get(&node));
                    let context = build_inline_content_items(font, arena, styles, node, None);
                    let id =
                        tree.new_leaf_with_context(style, LeafContext::Inline(context.clone()))?;
                    inline_leaves.push((node, context));
                    id
                } else if kind == LayoutKind::Mixed {
                    let parts = mixed_parts_of(arena, styles, node);
                    let blocks: Vec<NodeId> = parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::Block(child) => Some(*child),
                            Part::Run(_) => None,
                        })
                        .collect();
                    stack.push(BuildStep::BuildMixed(node, parts));
                    for &child in blocks.iter().rev() {
                        stack.push(BuildStep::Visit(child));
                    }
                    continue;
                } else {
                    stack.push(BuildStep::Build(node));
                    for &child in arena_children.iter().rev() {
                        stack.push(BuildStep::Visit(child));
                    }
                    continue;
                };
                taffy_ids.insert(node, id);
            }
            BuildStep::BuildMixed(node, parts) => {
                let mut children = Vec::with_capacity(parts.len());
                let mut run_number = 0;
                for part in &parts {
                    match part {
                        Part::Block(child) => children.push(taffy_ids[child]),
                        Part::Run(range) => {
                            let context = build_run_content_items(
                                font,
                                arena,
                                styles,
                                node,
                                range.clone(),
                                None,
                            );
                            let leaf = tree.new_leaf_with_context(
                                taffy::Style {
                                    display: Display::Block,
                                    ..Default::default()
                                },
                                LeafContext::Inline(context.clone()),
                            )?;
                            let key = anonymous_id(node, run_number);
                            run_number += 1;
                            taffy_ids.insert(key, leaf);
                            inline_leaves.push((key, context));
                            children.push(leaf);
                        }
                    }
                }
                let id = tree.new_with_children(to_taffy_style(styles.get(&node)), &children)?;
                taffy_ids.insert(node, id);
            }
            BuildStep::Build(node) => {
                let style = to_taffy_style(styles.get(&node));
                let children: Vec<taffy::NodeId> = arena
                    .children(node)
                    .iter()
                    .map(|child| taffy_ids[child])
                    .collect();
                let id = tree.new_with_children(style, &children)?;
                taffy_ids.insert(node, id);
            }
        }
    }

    Ok(taffy_ids[&root])
}

fn to_taffy_box_sizing(value: StyleBoxSizing) -> BoxSizing {
    match value {
        StyleBoxSizing::ContentBox => BoxSizing::ContentBox,
        StyleBoxSizing::BorderBox => BoxSizing::BorderBox,
    }
}

fn to_taffy_style(style: Option<&ComputedStyle>) -> taffy::Style {
    let Some(style) = style else {
        return taffy::Style {
            display: Display::Block,
            ..Default::default()
        };
    };

    taffy::Style {
        display: to_display(style.display),
        // Real CSS's default is content-box: padding and border add to a
        // declared width/height rather than being carved out of it. Taffy's
        // own default is border-box, so this is always set explicitly.
        box_sizing: to_taffy_box_sizing(style.box_sizing),
        size: Size {
            width: to_size_dimension(style.width, style.width_percent),
            height: to_size_dimension(style.height, style.height_percent),
        },
        min_size: Size {
            width: to_min_size_dimension(style.min_width),
            height: to_min_size_dimension(style.min_height),
        },
        max_size: Size {
            width: to_length_percentage_auto(style.max_width),
            height: to_length_percentage_auto(style.max_height),
        },
        margin: Rect {
            left: to_length_percentage_auto(style.margin.left),
            right: to_length_percentage_auto(style.margin.right),
            top: to_length_percentage_auto(style.margin.top),
            bottom: to_length_percentage_auto(style.margin.bottom),
        },
        position: to_taffy_position(style.position),
        inset: Rect {
            left: to_inset_length_percentage_auto(style.inset.left),
            right: to_inset_length_percentage_auto(style.inset.right),
            top: to_inset_length_percentage_auto(style.inset.top),
            bottom: to_inset_length_percentage_auto(style.inset.bottom),
        },
        // These four only affect *this node's own children*, and only take
        // effect at all when `display` above is `Flex` — Taffy ignores them
        // for a block container, so setting them unconditionally is safe.
        flex_direction: to_flex_direction(style.flex_direction),
        flex_wrap: to_flex_wrap(style.flex_wrap),
        justify_content: to_content_alignment(style.justify_content),
        align_content: to_content_alignment(style.align_content),
        align_items: to_item_alignment(style.align_items),
        gap: Size {
            width: LengthPercentage::length(style.column_gap),
            height: LengthPercentage::length(style.row_gap),
        },
        // These three instead describe how *this node itself* behaves as a
        // flex item — meaningful only when this node's *parent* is a flex
        // container, regardless of this node's own `display`.
        align_self: to_item_alignment(style.align_self),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: to_dimension(style.flex_basis),
        padding: Rect {
            left: LengthPercentage::length(style.padding.left),
            right: LengthPercentage::length(style.padding.right),
            top: LengthPercentage::length(style.padding.top),
            bottom: LengthPercentage::length(style.padding.bottom),
        },
        // Border follows the same `box_sizing` as padding: under content-box a
        // `border-width` adds to a declared width/height, under border-box it
        // is carved out of it.
        border: Rect {
            left: LengthPercentage::length(style.border.left.width),
            right: LengthPercentage::length(style.border.right.width),
            top: LengthPercentage::length(style.border.top.width),
            bottom: LengthPercentage::length(style.border.bottom.width),
        },
        // Only meaningful when `display` above is `Grid`, the same way the
        // flex-* fields above are only meaningful for `Flex` — Taffy
        // ignores them otherwise, so setting them unconditionally is safe.
        grid_template_columns: to_grid_template_tracks(&style.grid_template_columns),
        grid_template_rows: to_grid_template_tracks(&style.grid_template_rows),
        // These two instead describe how *this node itself* is placed
        // within its *parent's* grid — meaningful only when this node's
        // parent has `display: grid`, regardless of this node's own
        // `display`, the same as `align_self` above.
        grid_column: to_grid_placement_line(style.grid_column),
        grid_row: to_grid_placement_line(style.grid_row),
        ..Default::default()
    }
}

fn to_grid_template_tracks(
    tracks: &[florui_style::GridTrackSize],
) -> Vec<GridTemplateComponent<String>> {
    tracks
        .iter()
        .map(|&track| GridTemplateComponent::Single(to_track_sizing_function(track)))
        .collect()
}

fn to_track_sizing_function(track: florui_style::GridTrackSize) -> TrackSizingFunction {
    match track {
        florui_style::GridTrackSize::Length(px) => length(px),
        florui_style::GridTrackSize::Fr(fraction) => fr(fraction),
        florui_style::GridTrackSize::Auto => auto(),
        florui_style::GridTrackSize::MinContent => min_content(),
        florui_style::GridTrackSize::MaxContent => max_content(),
    }
}

fn to_grid_placement_line(
    value: (florui_style::GridPlacement, florui_style::GridPlacement),
) -> Line<GridPlacement> {
    Line {
        start: to_grid_placement(value.0),
        end: to_grid_placement(value.1),
    }
}

fn to_grid_placement(value: florui_style::GridPlacement) -> GridPlacement {
    match value {
        florui_style::GridPlacement::Auto => GridPlacement::Auto,
        florui_style::GridPlacement::Line(index) => line(index),
        florui_style::GridPlacement::Span(count) => span(count),
    }
}

fn to_dimension(value: Option<f32>) -> Dimension {
    match value {
        Some(length) => Dimension::length(length),
        None => Dimension::auto(),
    }
}

fn to_size_dimension(length: Option<f32>, percent: Option<f32>) -> Dimension {
    match (length, percent) {
        (Some(length), _) => Dimension::length(length),
        (None, Some(percent)) => Dimension::percent(percent),
        (None, None) => Dimension::auto(),
    }
}

/// A `calc()` mixing a length and a percentage has no single Taffy
/// representation, so it is treated as `auto`.
fn to_min_size_dimension(value: Option<florui_style::LengthPercentage>) -> LengthPercentageAuto {
    match value {
        Some(lp) if lp.percentage == 0.0 => LengthPercentageAuto::length(lp.length),
        Some(lp) if lp.length == 0.0 => LengthPercentageAuto::percent(lp.percentage),
        _ => LengthPercentageAuto::auto(),
    }
}

fn to_length_percentage_auto(value: Option<f32>) -> LengthPercentageAuto {
    match value {
        Some(length) => LengthPercentageAuto::length(length),
        None => LengthPercentageAuto::auto(),
    }
}

/// Same as [`to_length_percentage_auto`], for `inset`'s own edges, which
/// (unlike margin's) keep a real percentage instead of collapsing it — see
/// [`florui_style::ComputedStyle::inset`]'s own doc. A length-only or
/// percentage-only edge maps to Taffy's own matching variant, which
/// resolves a percentage natively against the containing block Taffy
/// already computes during its own absolute-position layout pass. Real
/// CSS also allows mixing both in one `calc()`; Taffy's own inset type
/// cannot hold both at once, so a genuine mix keeps only the percentage
/// term and drops the length term — a documented simplification, not a
/// silently wrong resolution.
fn to_inset_length_percentage_auto(
    value: Option<florui_style::LengthPercentage>,
) -> LengthPercentageAuto {
    match value {
        Some(lp) if lp.percentage != 0.0 => LengthPercentageAuto::percent(lp.percentage),
        Some(lp) => LengthPercentageAuto::length(lp.length),
        None => LengthPercentageAuto::auto(),
    }
}

/// Taffy has no `Static` concept of its own — every node is already a
/// valid positioning context for an absolutely-positioned descendant
/// regardless (see [`florui_style::Position`]'s own doc for why this
/// crate accepts that simplification rather than modeling real CSS's
/// stricter "nearest *explicitly* positioned ancestor" containing-block
/// rule).
fn to_taffy_position(value: StylePosition) -> Position {
    match value {
        StylePosition::Static | StylePosition::Relative => Position::Relative,
        StylePosition::Absolute => Position::Absolute,
    }
}

fn to_display(value: StyleDisplay) -> Display {
    match value {
        StyleDisplay::Block => Display::Block,
        StyleDisplay::Flex => Display::Flex,
        StyleDisplay::Grid => Display::Grid,
        StyleDisplay::None => Display::None,
        // Reached only for a node that did *not* qualify for
        // `needs_inline_layout` (e.g. it sits at the tree root, where real
        // CSS also blockifies `display: inline` — see `florui_style`'s own
        // `to_display`/blockification doc — or a bound this slice doesn't
        // yet cover, like a mixed block+inline sibling). Falling back to
        // Taffy's block algorithm for its own children matches what real
        // CSS does for `inline-block`'s own children too; a plain `inline`
        // node reaching here is rarer (mainly the blockified-root case)
        // and gets the same fallback rather than a crash.
        StyleDisplay::Inline | StyleDisplay::InlineBlock => Display::Block,
    }
}

fn to_flex_direction(value: StyleFlexDirection) -> FlexDirection {
    match value {
        StyleFlexDirection::Row => FlexDirection::Row,
        StyleFlexDirection::RowReverse => FlexDirection::RowReverse,
        StyleFlexDirection::Column => FlexDirection::Column,
        StyleFlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
    }
}

fn to_flex_wrap(value: StyleFlexWrap) -> FlexWrap {
    match value {
        StyleFlexWrap::NoWrap => FlexWrap::NoWrap,
        StyleFlexWrap::Wrap => FlexWrap::Wrap,
        StyleFlexWrap::WrapReverse => FlexWrap::WrapReverse,
    }
}

/// Shared by `justify-content`/`align-content` — Taffy's own `JustifyContent`
/// is a type alias of `AlignContent`, so one conversion serves both fields.
fn to_content_alignment(value: Option<ContentAlignment>) -> Option<AlignContent> {
    value.map(|value| match value {
        ContentAlignment::Start => AlignContent::START,
        ContentAlignment::End => AlignContent::END,
        ContentAlignment::FlexStart => AlignContent::FLEX_START,
        ContentAlignment::FlexEnd => AlignContent::FLEX_END,
        ContentAlignment::Center => AlignContent::CENTER,
        ContentAlignment::Stretch => AlignContent::STRETCH,
        ContentAlignment::SpaceBetween => AlignContent::SPACE_BETWEEN,
        ContentAlignment::SpaceAround => AlignContent::SPACE_AROUND,
        ContentAlignment::SpaceEvenly => AlignContent::SPACE_EVENLY,
    })
}

/// Shared by `align-items`/`align-self` — Taffy's own `AlignSelf` is a type
/// alias of `AlignItems`, so one conversion serves both fields.
fn to_item_alignment(value: Option<ItemAlignment>) -> Option<AlignItems> {
    value.map(|value| match value {
        ItemAlignment::Stretch => AlignItems::STRETCH,
        ItemAlignment::FlexStart => AlignItems::FLEX_START,
        ItemAlignment::FlexEnd => AlignItems::FLEX_END,
        ItemAlignment::Start => AlignItems::START,
        ItemAlignment::End => AlignItems::END,
        ItemAlignment::Center => AlignItems::CENTER,
        ItemAlignment::Baseline => AlignItems::BASELINE,
    })
}

/// Accumulates `node`'s ancestors' [`BoxLayout`] offsets into a position
/// relative to the layout root, since Taffy itself only ever reports a
/// position relative to the immediate parent.
pub fn absolute_position(
    arena: &Tree,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
) -> (f32, f32) {
    let mut x = 0.0;
    let mut y = 0.0;
    let mut current = Some(node);
    while let Some(id) = current {
        if let Some(layout) = layouts.get(&id) {
            x += layout.x;
            y += layout.y;
        }
        current = layout_parent(arena, id);
    }
    (x, y)
}

/// The node `id` is laid out inside: its parent, or for an anonymous block
/// the container whose content it holds.
pub fn layout_parent(arena: &Tree, id: NodeId) -> Option<NodeId> {
    if is_anonymous(id) {
        Some(anonymous_container(id))
    } else if id & (PIECE | BASELINE) != 0 {
        None
    } else {
        arena.parent(id)
    }
}

/// Shifts every node's own box by its *immediate* parent's scroll offset,
/// if `scroll_offsets` has one — sufficient because [`absolute_position`]'s
/// own ancestor-chain summation already propagates that one-level shift to
/// every descendant beneath it once this returns. A scrolling node's own
/// box (its clip boundary in its own parent's coordinates) is never
/// shifted by its own offset, only by an ancestor's — scrolling moves a
/// node's *content*, not the node itself.
pub fn apply_scroll_offsets(
    arena: &Tree,
    layouts: &HashMap<NodeId, BoxLayout>,
    scroll_offsets: &HashMap<NodeId, (f32, f32)>,
) -> HashMap<NodeId, BoxLayout> {
    layouts
        .iter()
        .map(|(&id, &layout)| {
            let Some((offset_x, offset_y)) =
                layout_parent(arena, id).and_then(|parent| scroll_offsets.get(&parent).copied())
            else {
                return (id, layout);
            };
            (
                id,
                BoxLayout {
                    x: layout.x - offset_x,
                    y: layout.y - offset_y,
                    ..layout
                },
            )
        })
        .collect()
}

/// The topmost node whose box contains `(x, y)` (both relative to the
/// layout root, the same space [`absolute_position`] reports) — "topmost"
/// meaning whichever one `florui_paint` would have painted last, since an
/// overlap always resolves to the box on top. Real block layout has no
/// overlapping siblings yet, so in practice this only matters for nested
/// containment, but the rule generalizes to whatever layout produces.
///
/// A box only counts where its `border-radius` outline does (not the cut
/// corners), and only where every `overflow`-clipping ancestor's own
/// rounded padding box does too — the same shapes `florui_paint` draws and
/// clips to, so a click can't land on something that isn't visible there.
/// A node with `pointer-events: none` is never the target, though its
/// descendants still can be. A node with no entry in `styles` is a plain
/// rectangle.
///
/// A node's `transform` (and every ancestor's) moves where it is hit as
/// well as where it is drawn: the pointer is mapped back through each
/// transform from the outermost inward, and a transform that flattens the
/// plane (a zero scale) leaves its whole subtree unhittable.
pub fn hit_test(
    arena: &Arena,
    layouts: &HashMap<NodeId, BoxLayout>,
    styles: &StyleMap,
    x: f32,
    y: f32,
) -> Option<NodeId> {
    /// A node still to visit, with what its parent worked out for it.
    struct Visit {
        node: NodeId,
        /// The pointer in the parent's frame.
        parent_point: (f32, f32),
        /// Whether every clipping ancestor admits the pointer here.
        admitted: bool,
        /// The parent's absolute origin, carried down so no node walks its
        /// ancestors to find its own.
        parent_origin: (f32, f32),
    }

    let mut hit = None;
    let mut stack: Vec<Visit> = arena
        .roots()
        .iter()
        .rev()
        .map(|&root| Visit {
            node: root,
            parent_point: (x, y),
            admitted: true,
            parent_origin: (0.0, 0.0),
        })
        .collect();
    while let Some(Visit {
        node,
        parent_point,
        admitted,
        parent_origin,
    }) = stack.pop()
    {
        let mut point = parent_point;
        let mut admits_children = admitted;
        let mut origin = parent_origin;
        if let Some(&layout) = layouts.get(&node) {
            origin = (parent_origin.0 + layout.x, parent_origin.1 + layout.y);
            let (ax, ay) = origin;
            let style = styles.get(&node);
            if let Some(style) = style {
                let transform = resolve_transform(style, &layout, ax, ay, 1.0);
                if !transform.is_identity() {
                    let Some(inverse) = transform.invert() else {
                        continue;
                    };
                    point = inverse.map_point(point);
                }
            }
            let outline = border_box_outline(styles, node, ax, ay, layout);
            let pointer_events_none = style.is_some_and(|s| s.pointer_events_none);
            // An inline element that wraps over lines is hit only inside its
            // piece of each (see [`piece_id`]).
            let in_a_piece = !layouts.contains_key(&piece_id(node, 0))
                || (0..)
                    .map_while(|number| layouts.get(&piece_id(node, number)))
                    .any(|piece| {
                        let (px, py) = (point.0 - ax - piece.x, point.1 - ay - piece.y);
                        px >= 0.0 && py >= 0.0 && px < piece.width && py < piece.height
                    });
            if admitted && !pointer_events_none && in_a_piece && outline.contains(point.0, point.1)
            {
                hit = Some(node);
            }
            if let Some(style) = style
                && style.overflow_clips
            {
                let border = style.border;
                let padding_box = outline.inset(
                    border.top.width,
                    border.right.width,
                    border.bottom.width,
                    border.left.width,
                );
                admits_children = admitted && padding_box.contains(point.0, point.1);
            }
        }
        // Nothing under a node a clipping ancestor rejects can be hit, so its
        // subtree is not visited.
        if !admits_children {
            continue;
        }
        for &child in arena.children(node).iter().rev() {
            stack.push(Visit {
                node: child,
                parent_point: point,
                admitted: admits_children,
                parent_origin: origin,
            });
        }
    }
    hit
}

/// `node`'s border box with its `border-radius` applied. Layout runs in
/// logical pixels, so lengths resolve at scale `1.0`.
fn border_box_outline(
    styles: &StyleMap,
    node: NodeId,
    x: f32,
    y: f32,
    layout: BoxLayout,
) -> RoundedRect {
    match styles.get(&node) {
        Some(style) => {
            RoundedRect::new(x, y, layout.width, layout.height, &style.border_radius, 1.0)
        }
        None => RoundedRect::square(x, y, layout.width, layout.height),
    }
}

/// An explanation for why a flex or grid item's final size doesn't match
/// what plain flex-grow/flex-shrink math (or, for grid, the track's own
/// sizing) alone would produce — the inspector's
/// causal-diagnostics report. Percentage/containing-block causes and
/// which stylesheet rule supplied a value are not covered yet: the
/// former needs percentage-vs-auto to survive cascade resolution (today
/// both collapse to `None` in [`ComputedStyle`]), the latter needs
/// source-location/rule-identity tracking through Stylo — both are
/// materially larger, separate changes, the same gap already flagged for
/// attachments' own source-location tracking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SizeCause {
    /// A `Row`/`RowReverse` flex item's final width equals its own
    /// natural (unwrapped) content width — the row was too narrow for
    /// every child's natural width combined, and this item could not
    /// shrink past its own content.
    MinContentClampedWidth { intrinsic_width: f32 },
    /// The same, on the other axis: a `Column`/`ColumnReverse` flex
    /// item's final height equals its own natural content height.
    MinContentClampedHeight { intrinsic_height: f32 },
    /// A grid item's committed width ended up narrower than its own
    /// natural content width — the track it landed in didn't grow to fit
    /// it, so the content will overflow or clip.
    GridTrackNarrowerThanContent { intrinsic_width: f32 },
}

/// A diagnostic-only pass over an already-computed `layouts` (from
/// [`compute_layout`]): explains which flex/grid items were held to
/// their own content's size. Never called from the render path itself —
/// a caller (the inspector) opts into the extra measurement cost
/// explicitly, so not calling this never changes [`compute_layout`]'s
/// own output.
pub fn compute_size_causes(
    arena: &Arena,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
) -> HashMap<NodeId, SizeCause> {
    let mut font = florui_text::Font::load_embedded();
    let mut causes = HashMap::new();

    for (&parent, parent_style) in styles {
        let Some(parent_layout) = layouts.get(&parent) else {
            continue;
        };
        let children = arena.children(parent);
        if children.is_empty() {
            continue;
        }

        match parent_style.display {
            StyleDisplay::Flex if is_row(parent_style.flex_direction) => {
                flex_main_axis_causes(
                    &mut font,
                    arena,
                    styles,
                    layouts,
                    children,
                    parent_style,
                    parent_layout,
                    &mut causes,
                    true,
                );
            }
            StyleDisplay::Flex => {
                flex_main_axis_causes(
                    &mut font,
                    arena,
                    styles,
                    layouts,
                    children,
                    parent_style,
                    parent_layout,
                    &mut causes,
                    false,
                );
            }
            StyleDisplay::Grid => {
                for &child in children {
                    let (Some(child_style), Some(child_layout)) =
                        (styles.get(&child), layouts.get(&child))
                    else {
                        continue;
                    };
                    let intrinsic = natural_width(&mut font, arena, child, child_style);
                    if intrinsic > 0.0 && child_layout.width < intrinsic - 0.5 {
                        causes.insert(
                            child,
                            SizeCause::GridTrackNarrowerThanContent {
                                intrinsic_width: intrinsic,
                            },
                        );
                    }
                }
            }
            StyleDisplay::Block
            | StyleDisplay::Inline
            | StyleDisplay::InlineBlock
            | StyleDisplay::None => {}
        }
    }

    causes
}

fn is_row(direction: StyleFlexDirection) -> bool {
    matches!(
        direction,
        StyleFlexDirection::Row | StyleFlexDirection::RowReverse
    )
}

/// Shared by the `Row`/`RowReverse` (width) and `Column`/`ColumnReverse`
/// (height) cases: an item pinned to its own natural main-axis size
/// because the container, combined with every sibling's own natural
/// size, is over-constrained on that axis.
#[allow(clippy::too_many_arguments)]
fn flex_main_axis_causes(
    font: &mut florui_text::Font,
    arena: &Arena,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    children: &[NodeId],
    parent_style: &ComputedStyle,
    parent_layout: &BoxLayout,
    causes: &mut HashMap<NodeId, SizeCause>,
    is_row: bool,
) {
    let natural_sizes: Vec<f32> = children
        .iter()
        .map(|&child| match styles.get(&child) {
            Some(style) if is_row => natural_width(font, arena, child, style),
            Some(style) => {
                let cross_axis_width = layouts.get(&child).map_or(0.0, |l| l.width);
                natural_height(font, arena, child, style, cross_axis_width)
            }
            None => 0.0,
        })
        .collect();
    let gap = if is_row {
        parent_style.column_gap
    } else {
        parent_style.row_gap
    };
    let gap_total = gap * (children.len() - 1) as f32;
    let total_natural: f32 = natural_sizes.iter().sum::<f32>() + gap_total;
    let parent_main = if is_row {
        parent_layout.width
    } else {
        parent_layout.height
    };
    if total_natural <= parent_main + 0.5 {
        return;
    }

    for (&child, &intrinsic) in children.iter().zip(&natural_sizes) {
        let (Some(child_style), Some(child_layout)) = (styles.get(&child), layouts.get(&child))
        else {
            continue;
        };
        let child_main = if is_row {
            child_layout.width
        } else {
            child_layout.height
        };
        if child_style.flex_shrink > 0.0 && intrinsic > 0.0 && (child_main - intrinsic).abs() < 0.5
        {
            causes.insert(
                child,
                if is_row {
                    SizeCause::MinContentClampedWidth {
                        intrinsic_width: intrinsic,
                    }
                } else {
                    SizeCause::MinContentClampedHeight {
                        intrinsic_height: intrinsic,
                    }
                },
            );
        }
    }
}

/// This node's own natural (unwrapped) width: its explicit `flex-basis`
/// or `width` if it has one, or a text leaf's measured width. `0.0` (not
/// computed) for anything else, e.g. an element with its own element
/// children — a bounded first cut, not a full intrinsic-size algorithm.
fn natural_width(
    font: &mut florui_text::Font,
    arena: &Arena,
    node: NodeId,
    style: &ComputedStyle,
) -> f32 {
    if let Some(basis) = style.flex_basis {
        return basis;
    }
    if let Some(width) = style.width {
        return width;
    }
    if arena.children(node).is_empty() {
        let text = leaf_text(arena, node);
        if !text.is_empty() {
            let family = to_text_font_family(style.font_family);
            return font
                .measure(
                    florui_text::TextStyle::new(family, style.font_size, style.font_weight),
                    text,
                )
                .width;
        }
    }
    0.0
}

/// This node's own natural (unwrapped) height, the same reasoning as
/// [`natural_width`] but for a `Column`/`ColumnReverse` flex container,
/// where `flex-basis` sets the main-axis (height) size instead. Text's
/// own minimum height depends on where it wraps, which depends on the
/// width it's actually been given — unlike an unwrapped width, there is
/// no single width-independent "natural" height, so `cross_axis_width`
/// (the item's own already-committed width, the cross axis in a column
/// container) is required to wrap it the same way real layout did.
fn natural_height(
    font: &mut florui_text::Font,
    arena: &Arena,
    node: NodeId,
    style: &ComputedStyle,
    cross_axis_width: f32,
) -> f32 {
    if let Some(basis) = style.flex_basis {
        return basis;
    }
    if let Some(height) = style.height {
        return height;
    }
    if arena.children(node).is_empty() {
        let text = leaf_text(arena, node);
        if !text.is_empty() {
            let family = to_text_font_family(style.font_family);
            return font
                .measure_wrapped(
                    florui_text::TextStyle::new(family, style.font_size, style.font_weight),
                    text,
                    cross_axis_width,
                )
                .height;
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_style::InteractionState;

    use super::*;

    /// [`super::hit_test`] over unstyled (plain rectangle) nodes.
    fn hit_test(
        arena: &Arena,
        layouts: &HashMap<NodeId, BoxLayout>,
        x: f32,
        y: f32,
    ) -> Option<NodeId> {
        super::hit_test(arena, layouts, &StyleMap::new(), x, y)
    }

    /// The walk [`super::hit_test`] replaced: every node found its own
    /// origin by summing its ancestors, and clipped-out subtrees were still
    /// visited. Kept as the oracle the faster walk must agree with.
    fn reference_hit_test(
        arena: &Arena,
        layouts: &HashMap<NodeId, BoxLayout>,
        styles: &StyleMap,
        x: f32,
        y: f32,
    ) -> Option<NodeId> {
        let mut hit = None;
        let mut stack: Vec<(NodeId, (f32, f32), bool)> = arena
            .roots()
            .iter()
            .rev()
            .map(|&root| (root, (x, y), true))
            .collect();
        while let Some((node, parent_point, admitted)) = stack.pop() {
            let mut point = parent_point;
            let mut admits_children = admitted;
            if let Some(&layout) = layouts.get(&node) {
                let (ax, ay) = absolute_position(arena, layouts, node);
                let style = styles.get(&node);
                if let Some(style) = style {
                    let transform = resolve_transform(style, &layout, ax, ay, 1.0);
                    if !transform.is_identity() {
                        let Some(inverse) = transform.invert() else {
                            continue;
                        };
                        point = inverse.map_point(point);
                    }
                }
                let outline = border_box_outline(styles, node, ax, ay, layout);
                let pointer_events_none = style.is_some_and(|s| s.pointer_events_none);
                if admitted && !pointer_events_none && outline.contains(point.0, point.1) {
                    hit = Some(node);
                }
                if let Some(style) = style
                    && style.overflow_clips
                {
                    let border = style.border;
                    let padding_box = outline.inset(
                        border.top.width,
                        border.right.width,
                        border.bottom.width,
                        border.left.width,
                    );
                    admits_children = admitted && padding_box.contains(point.0, point.1);
                }
            }
            for &child in arena.children(node).iter().rev() {
                stack.push((child, point, admits_children));
            }
        }
        hit
    }

    /// A deterministic tree of nested boxes covering what `hit_test` looks at:
    /// clipping, rounded corners, transforms, `pointer-events: none`, and
    /// absolutely placed children.
    fn random_tree(seed: u32) -> Element {
        fn next(state: &mut u32) -> u32 {
            *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *state >> 8
        }
        fn node(state: &mut u32, depth: u32) -> Element {
            let class = format!("c{}", next(state) % 7);
            let children = if depth >= 5 {
                Vec::new()
            } else {
                (0..next(state) % 4)
                    .map(|_| node(state, depth + 1))
                    .collect()
            };
            Element::node("div", vec![("class".into(), class)], children)
        }
        let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(7);
        let children = (0..3).map(|_| node(&mut state, 1)).collect();
        Element::node("div", vec![("class".into(), "root".into())], children)
    }

    const RANDOM_TREE_CSS: &str = ".root { display: flex; flex-wrap: wrap; width: 300px; \
         position: relative; } \
         .c0 { width: 60px; height: 40px; } \
         .c1 { width: 80px; height: 50px; overflow: hidden; } \
         .c2 { width: 50px; height: 50px; border-radius: 12px; overflow: hidden; } \
         .c3 { width: 40px; height: 40px; transform: rotate(15deg); } \
         .c4 { width: 70px; height: 30px; pointer-events: none; } \
         .c5 { position: absolute; left: 30px; top: 20px; width: 50px; height: 50px; } \
         .c6 { width: 30px; height: 30px; margin-left: 7.5px; \
               transform: translate(5px, 3px) scale(1.2); }";

    #[test]
    fn the_faster_hit_test_agrees_with_the_walk_it_replaced_on_random_trees() {
        let mut compared = 0;
        let mut hits = 0;
        for seed in 0..40u32 {
            let (arena, styles, layouts) = layout_with_styles(&random_tree(seed), RANDOM_TREE_CSS);
            let mut state = seed.wrapping_mul(97).wrapping_add(13);
            for _ in 0..300 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let x = (state >> 8) % 3400;
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let y = (state >> 8) % 3400;
                let (x, y) = (x as f32 / 10.0 - 10.0, y as f32 / 10.0 - 10.0);
                let fast = super::hit_test(&arena, &layouts, &styles, x, y);
                let slow = reference_hit_test(&arena, &layouts, &styles, x, y);
                assert_eq!(fast, slow, "seed {seed} at ({x}, {y})");
                compared += 1;
                hits += usize::from(fast.is_some());
            }
        }
        assert_eq!(compared, 12_000);
        assert!(
            hits > 1_000,
            "the comparison must include real hits: {hits}"
        );
    }

    #[test]
    fn hit_testing_a_clipped_out_subtree_finds_nothing_inside_it() {
        let tree: Element = view! {
            <div class="clip">
                <div class="inner"></div>
            </div>
        };
        let css = ".clip { width: 20px; height: 20px; overflow: hidden; } \
                   .inner { width: 100px; height: 100px; }";
        let (arena, styles, layouts) = layout_with_styles(&tree, css);
        let clip = arena.roots()[0];
        let inner = arena.children(clip)[0];

        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 10.0, 10.0),
            Some(inner)
        );
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 50.0, 50.0),
            None,
            "inside the child's box but outside the clip"
        );
    }

    #[test]
    fn a_layout_with_the_text_memo_warm_is_identical_to_a_cold_one() {
        let tree: Element = view! {
            <div class="row">
                <span class="label">{"A label that wraps onto several lines when it is narrow"}</span>
                <button class="action">{"Open"}</button>
                <p class="note">{"Second paragraph, different text and size"}</p>
            </div>
        };
        let css = ".row { display: flex; flex-wrap: wrap; width: 160px; } \
                   .label { width: 90px; } .action { padding: 2px 8px; } \
                   .note { font-size: 13px; font-weight: 700; width: 120px; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let layout = |font: &mut florui_text::Font| {
            compute_layout(font, &arena, &styles, Size::MAX_CONTENT).unwrap()
        };

        let mut font = florui_text::Font::load_embedded();
        let cold = layout(&mut font);
        let warm = layout(&mut font);
        let fresh = layout(&mut florui_text::Font::load_embedded());

        assert_eq!(warm, cold, "the second pass answers from the memo");
        assert_eq!(fresh, cold, "and matches a font that never memoized");
    }

    fn layout_for(tree: &Element, css: &str) -> (Arena, HashMap<NodeId, BoxLayout>) {
        let (arena, _styles, layouts) = layout_with_styles(tree, css);
        (arena, layouts)
    }

    fn layout_with_styles(
        tree: &Element,
        css: &str,
    ) -> (Arena, StyleMap, HashMap<NodeId, BoxLayout>) {
        let arena = Arena::build(tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = florui_text::Font::load_embedded();
        let layouts = compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        (arena, styles, layouts)
    }

    fn layout_with_overlays_for(
        document: &Element,
        overlays: &Element,
        css: &str,
        available: Size<AvailableSpace>,
    ) -> (Arena, HashMap<NodeId, BoxLayout>) {
        let arena = Arena::build_with_overlays(document, overlays);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = florui_text::Font::load_embedded();
        let layouts = compute_layout(&mut font, &arena, &styles, available).unwrap();
        (arena, layouts)
    }

    /// [`resolve_replaced_size`] and [`resolve_object_fit_content_rect`],
    /// each measurement recorded against real Chrome first (a served test
    /// page, real `getBoundingClientRect`) rather than derived from CSS
    /// spec text alone — see [`resolve_replaced_size`]'s own doc.
    mod replaced_element_sizing {
        use super::*;

        fn no_ratio() -> florui_style::AspectRatio {
            florui_style::AspectRatio {
                prefers_intrinsic: true,
                ratio: None,
            }
        }

        fn explicit_ratio(width: f32, height: f32) -> florui_style::AspectRatio {
            florui_style::AspectRatio {
                prefers_intrinsic: false,
                ratio: Some((width, height)),
            }
        }

        fn both_auto() -> Size<Option<f32>> {
            Size {
                width: None,
                height: None,
            }
        }

        fn definite(width: f32, height: f32) -> Size<AvailableSpace> {
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            }
        }

        /// Measured: a broken/never-loaded image with no CSS size and no
        /// `aspect-ratio`, inside a 400px container, is 400 wide (an
        /// ordinary block box's own `width: auto` fill) and a small,
        /// UA-glyph-sized height Chrome shows its own broken-image icon
        /// at (18px there) — this crate renders no such glyph, so `0.0`
        /// is the honest value instead of a copied magic number.
        #[test]
        fn no_intrinsic_size_and_no_ratio_fills_width_and_has_zero_height() {
            let size = resolve_replaced_size(None, no_ratio(), both_auto(), definite(400.0, 900.0));
            assert_eq!(
                size,
                Size {
                    width: 400.0,
                    height: 0.0
                }
            );
        }

        /// Measured: 400x225 (400 * 9/16) for a 16:9 `aspect-ratio` with
        /// both axes `auto`, no intrinsic size, in a 400px container.
        #[test]
        fn no_intrinsic_size_with_a_ratio_derives_height_from_the_filled_width() {
            let size = resolve_replaced_size(
                None,
                explicit_ratio(16.0, 9.0),
                both_auto(),
                definite(400.0, 900.0),
            );
            assert_eq!(size.width, 400.0);
            assert!((size.height - 225.0).abs() < 0.01, "{size:?}");
        }

        /// Measured: 200x112.5 for a 16:9 ratio with an explicit
        /// `width: 200px` (`height: auto`).
        #[test]
        fn an_explicit_width_with_a_ratio_derives_the_height() {
            let size = resolve_replaced_size(
                None,
                explicit_ratio(16.0, 9.0),
                Size {
                    width: Some(200.0),
                    height: None,
                },
                definite(400.0, 900.0),
            );
            assert_eq!(size.width, 200.0);
            assert!((size.height - 112.5).abs() < 0.01, "{size:?}");
        }

        /// Measured: a real 100x50 intrinsic image with `width: 200px`
        /// and no `aspect-ratio` property resolves height from its own
        /// intrinsic ratio (2:1) — 200x100.
        #[test]
        fn an_explicit_width_with_no_ratio_property_derives_height_from_the_intrinsic_ratio() {
            let size = resolve_replaced_size(
                Some(Size {
                    width: 100.0,
                    height: 50.0,
                }),
                no_ratio(),
                Size {
                    width: Some(200.0),
                    height: None,
                },
                definite(400.0, 900.0),
            );
            assert_eq!(
                size,
                Size {
                    width: 200.0,
                    height: 100.0
                }
            );
        }

        /// Measured: the same 100x50 intrinsic image with an explicit
        /// `aspect-ratio: 1/1` (no `auto`) and `width: 200px` uses 1:1,
        /// not its own 2:1 intrinsic ratio — 200x200. A bare `<ratio>`
        /// always overrides the intrinsic ratio.
        #[test]
        fn an_explicit_bare_ratio_overrides_the_intrinsic_ratio() {
            let size = resolve_replaced_size(
                Some(Size {
                    width: 100.0,
                    height: 50.0,
                }),
                explicit_ratio(1.0, 1.0),
                Size {
                    width: Some(200.0),
                    height: None,
                },
                definite(400.0, 900.0),
            );
            assert_eq!(
                size,
                Size {
                    width: 200.0,
                    height: 200.0
                }
            );
        }

        /// Measured: a real 100x50 intrinsic image with no CSS size at
        /// all, inside a 400px container, stays 100x50 — it does **not**
        /// fill, unlike an ordinary block box or a sizeless/broken image.
        #[test]
        fn a_real_intrinsic_size_wins_over_available_space_when_both_axes_are_auto() {
            let size = resolve_replaced_size(
                Some(Size {
                    width: 100.0,
                    height: 50.0,
                }),
                no_ratio(),
                both_auto(),
                definite(400.0, 900.0),
            );
            assert_eq!(
                size,
                Size {
                    width: 100.0,
                    height: 50.0
                }
            );
        }

        #[test]
        fn both_axes_already_pinned_by_css_are_used_as_is() {
            let size = resolve_replaced_size(
                Some(Size {
                    width: 100.0,
                    height: 50.0,
                }),
                no_ratio(),
                Size {
                    width: Some(30.0),
                    height: Some(30.0),
                },
                definite(400.0, 900.0),
            );
            assert_eq!(
                size,
                Size {
                    width: 30.0,
                    height: 30.0
                }
            );
        }

        fn box_of(width: f32, height: f32) -> Size<f32> {
            Size { width, height }
        }

        fn centered() -> (
            florui_style::LengthPercentage,
            florui_style::LengthPercentage,
        ) {
            (
                florui_style::LengthPercentage {
                    length: 0.0,
                    percentage: 0.5,
                },
                florui_style::LengthPercentage {
                    length: 0.0,
                    percentage: 0.5,
                },
            )
        }

        #[test]
        fn object_fit_fill_stretches_to_the_whole_box_ignoring_aspect_ratio() {
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::Fill,
                centered(),
                box_of(200.0, 100.0),
                box_of(100.0, 50.0),
            );
            assert_eq!(rect.width, 200.0);
            assert_eq!(rect.height, 100.0);
        }

        /// A 2:1 image in a taller-than-2:1 (100x100) box: `Contain` fits
        /// entirely within by scaling to the narrower axis (100x50),
        /// centered — 25px empty on each side of the shorter axis.
        #[test]
        fn object_fit_contain_letterboxes_and_centers() {
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::Contain,
                centered(),
                box_of(100.0, 100.0),
                box_of(100.0, 50.0),
            );
            assert_eq!((rect.width, rect.height), (100.0, 50.0));
            assert_eq!(rect.y, 25.0, "centered vertically in the leftover space");
        }

        /// The same 2:1 image and box under `Cover` instead crops to fill
        /// every axis — 100x100, cropping the wider axis (200 scaled
        /// down to fit height, then only 100 of its 200 width shown).
        #[test]
        fn object_fit_cover_fills_and_crops() {
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::Cover,
                centered(),
                box_of(100.0, 100.0),
                box_of(100.0, 50.0),
            );
            assert_eq!((rect.width, rect.height), (200.0, 100.0));
            assert_eq!(
                rect.x, -50.0,
                "centered -- half the overflow is cropped left"
            );
        }

        #[test]
        fn object_fit_none_uses_intrinsic_size_unscaled() {
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::None,
                centered(),
                box_of(300.0, 300.0),
                box_of(100.0, 50.0),
            );
            assert_eq!((rect.width, rect.height), (100.0, 50.0));
        }

        /// A box larger than the intrinsic size: `ScaleDown` behaves like
        /// `None` (never upscales).
        #[test]
        fn object_fit_scale_down_never_upscales() {
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::ScaleDown,
                centered(),
                box_of(300.0, 300.0),
                box_of(100.0, 50.0),
            );
            assert_eq!((rect.width, rect.height), (100.0, 50.0));
        }

        /// A box smaller than the intrinsic size: `ScaleDown` behaves
        /// like `Contain` (does shrink).
        #[test]
        fn object_fit_scale_down_shrinks_like_contain_when_the_box_is_smaller() {
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::ScaleDown,
                centered(),
                box_of(50.0, 50.0),
                box_of(100.0, 50.0),
            );
            assert_eq!((rect.width, rect.height), (50.0, 25.0));
        }

        #[test]
        fn object_position_offsets_away_from_center() {
            let top_left = (
                florui_style::LengthPercentage {
                    length: 0.0,
                    percentage: 0.0,
                },
                florui_style::LengthPercentage {
                    length: 0.0,
                    percentage: 0.0,
                },
            );
            let rect = resolve_object_fit_content_rect(
                florui_style::ObjectFit::Contain,
                top_left,
                box_of(100.0, 100.0),
                box_of(100.0, 50.0),
            );
            assert_eq!((rect.x, rect.y), (0.0, 0.0));
        }

        /// End-to-end through real CSS parsing, Stylo cascade, and Taffy
        /// layout (not just the pure function above): an `<img>` with
        /// `aspect-ratio: 16 / 9` in a 400px-wide flow gets a real
        /// 400x225 box — the same case measured against Chrome.
        #[test]
        fn an_img_with_an_aspect_ratio_gets_a_real_measured_box_end_to_end() {
            let tree: Element = view! {
                <div class="container">
                    <img class="pic" />
                </div>
            };
            let (arena, layouts) = layout_for(
                &tree,
                ".container { width: 400px; } \
                 .pic { display: block; aspect-ratio: 16 / 9; }",
            );
            let container = arena.roots()[0];
            let img = arena.children(container)[0];
            assert_eq!(layouts[&img].width, 400.0);
            assert!((layouts[&img].height - 225.0).abs() < 0.01);
        }

        /// End-to-end: an `<img>` with an explicit `width`/`height` uses
        /// exactly those, the same as any other box-sized element.
        #[test]
        fn an_img_with_explicit_width_and_height_uses_them_end_to_end() {
            let tree: Element = view! { <img class="pic" /> };
            let (arena, layouts) =
                layout_for(&tree, ".pic { display: block; width: 64px; height: 48px; }");
            let img = arena.roots()[0];
            assert_eq!(layouts[&img].width, 64.0);
            assert_eq!(layouts[&img].height, 48.0);
        }
    }

    #[test]
    fn an_overlay_root_lays_out_at_the_viewport_origin_regardless_of_document_content() {
        let document: Element = view! { <div class="doc" /> };
        let overlay: Element = view! { <div class="overlay" /> };
        let available = Size {
            width: AvailableSpace::Definite(300.0),
            height: AvailableSpace::Definite(200.0),
        };
        let (arena, layouts) = layout_with_overlays_for(
            &document,
            &overlay,
            ".doc { width: 300px; height: 900px; } .overlay { width: 50px; height: 30px; }",
            available,
        );
        let overlay_root = arena.overlay_roots()[0];
        assert_eq!(
            layouts[&overlay_root].x, 0.0,
            "an overlay root must start at the viewport's own origin, not shifted by document \
             content -- before this fix it would land below 900px of document height instead"
        );
        assert_eq!(layouts[&overlay_root].y, 0.0);
        assert_eq!(layouts[&overlay_root].width, 50.0);
        assert_eq!(layouts[&overlay_root].height, 30.0);
    }

    /// The load-bearing claim `florui-platform`'s `Popover` relies on:
    /// combining an overlay root's own viewport-origin placement (proven
    /// above) with a plain `position: absolute` child's own inset
    /// resolution against its parent (proven by
    /// `position_absolute_lands_at_its_own_explicit_inset_offset`) must
    /// add up to true viewport pixel coordinates -- neither existing test
    /// proves that composition directly.
    #[test]
    fn an_absolutely_positioned_child_of_an_overlay_root_lands_at_true_viewport_coordinates() {
        let document: Element = view! { <div class="doc" /> };
        let overlay: Element = view! {
            <div class="overlay-root">
                <div class="anchored" />
            </div>
        };
        let available = Size {
            width: AvailableSpace::Definite(300.0),
            height: AvailableSpace::Definite(200.0),
        };
        let (arena, layouts) = layout_with_overlays_for(
            &document,
            &overlay,
            ".doc { width: 300px; height: 900px; } \
             .anchored { position: absolute; top: 40px; left: 25px; width: 10px; height: 10px; }",
            available,
        );
        let overlay_root = arena.overlay_roots()[0];
        let anchored = arena.children(overlay_root)[0];
        assert_eq!(
            absolute_position(&arena, &layouts, anchored),
            (25.0, 40.0),
            "an absolutely-positioned child of an overlay root must land at true viewport pixel \
             coordinates -- the same space a document-tree trigger's own absolute_position \
             reports -- regardless of document content or the overlay root's own lack of \
             explicit sizing"
        );
    }

    #[test]
    fn an_arena_built_without_overlays_never_runs_the_second_layout_pass() {
        let tree: Element = view! { <div /> };
        let (arena, layouts) = layout_for(&tree, "");
        assert!(arena.overlay_roots().is_empty());
        assert_eq!(layouts.len(), 1, "only the one document root is laid out");
    }

    #[test]
    fn an_explicitly_sized_node_gets_that_size() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, layouts) = layout_for(&tree, ".card { width: 200px; height: 100px; }");
        let node = arena.roots()[0];
        assert_eq!(layouts[&node].width, 200.0);
        assert_eq!(layouts[&node].height, 100.0);
    }

    #[test]
    fn position_relative_alone_is_a_no_op() {
        let tree: Element = view! {
            <div class="parent">
                <div class="rel" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".parent { width: 200px; height: 100px; } \
             .rel { position: relative; width: 50px; height: 20px; }",
        );
        let rel = arena.children(arena.roots()[0])[0];
        assert_eq!(
            (layouts[&rel].x, layouts[&rel].y),
            (0.0, 0.0),
            "position: relative with no inset must not move the box at all"
        );
    }

    #[test]
    fn position_absolute_lands_at_its_own_explicit_inset_offset() {
        let tree: Element = view! {
            <div class="parent">
                <div class="abs" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".parent { position: relative; width: 200px; height: 100px; } \
             .abs { position: absolute; top: 10px; left: 20px; width: 30px; height: 15px; }",
        );
        let abs = arena.children(arena.roots()[0])[0];
        assert_eq!(layouts[&abs].x, 20.0);
        assert_eq!(layouts[&abs].y, 10.0);
        assert_eq!(layouts[&abs].width, 30.0);
        assert_eq!(layouts[&abs].height, 15.0);
    }

    #[test]
    fn position_absolute_resolves_a_percentage_inset_against_its_containing_block() {
        let tree: Element = view! {
            <div class="parent">
                <div class="abs" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".parent { position: relative; width: 200px; height: 100px; } \
             .abs { position: absolute; top: 50%; left: 25%; width: 30px; height: 15px; }",
        );
        let abs = arena.children(arena.roots()[0])[0];
        assert_eq!(layouts[&abs].x, 50.0, "25% of the 200px containing block");
        assert_eq!(layouts[&abs].y, 50.0, "50% of the 100px containing block");
    }

    #[test]
    fn position_absolute_with_no_width_stretches_between_opposing_percentage_insets() {
        let tree: Element = view! {
            <div class="parent">
                <div class="abs" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".parent { position: relative; width: 200px; height: 100px; } \
             .abs { position: absolute; top: 0px; left: 0%; right: 25%; height: 10px; }",
        );
        let abs = arena.children(arena.roots()[0])[0];
        assert_eq!(
            layouts[&abs].width, 150.0,
            "200px containing block minus 0% left and 25% (50px) right, no explicit width"
        );
    }

    #[test]
    fn min_size_stops_a_shrinking_flex_item_and_wins_over_max_size() {
        let tree: Element = view! {
            <div class="col">
                <div class="row">
                    <div class="a" />
                    <div class="b" />
                </div>
                <div class="c" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".col { width: 200px; } \
             .row { display: flex; width: 200px; height: 40px; } \
             .a { width: 150px; min-width: 120px; } \
             .b { width: 150px; } \
             .c { max-width: 5px; min-width: 8px; min-height: 30px; }",
        );
        let col = arena.roots()[0];
        let [row, c] = arena.children(col)[..] else {
            panic!("a row and a box");
        };
        let [a, b] = arena.children(row)[..] else {
            panic!("two flex items");
        };
        assert_eq!(
            layouts[&a].width, 120.0,
            "shrinks to its min-width, not 100"
        );
        assert_eq!(layouts[&b].width, 80.0);
        assert_eq!(layouts[&c].width, 8.0, "min-width wins over max-width");
        assert_eq!(layouts[&c].height, 30.0);
    }

    #[test]
    fn a_percentage_min_size_resolves_against_a_definite_containing_block_only() {
        let tree: Element = view! {
            <div class="col">
                <div class="fixed"><div class="m" /></div>
                <div class="auto"><div class="i" /></div>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".fixed { width: 200px; height: 100px; } \
             .m { width: 10px; height: 10px; min-width: 50%; min-height: 50%; } \
             .auto { width: 200px; } \
             .i { height: 10px; min-height: 50%; }",
        );
        let [fixed, auto] = arena.children(arena.roots()[0])[..] else {
            panic!("two containers");
        };
        let m = arena.children(fixed)[0];
        let i = arena.children(auto)[0];
        assert_eq!(layouts[&m].width, 100.0);
        assert_eq!(layouts[&m].height, 50.0);
        assert_eq!(layouts[&i].height, 10.0, "auto-height parent: ignored");
    }

    #[test]
    fn position_absolute_is_removed_from_normal_flow() {
        let tree: Element = view! {
            <div class="parent">
                <div class="abs" />
                <div class="sibling" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".parent { position: relative; width: 200px; } \
             .abs { position: absolute; top: 0px; left: 0px; width: 30px; height: 15px; } \
             .sibling { width: 40px; height: 10px; }",
        );
        let parent = arena.roots()[0];
        let sibling = arena.children(parent)[1];
        assert_eq!(
            layouts[&sibling].y, 0.0,
            "an absolutely positioned sibling must not push document-flow \
             content down, as if it were never there at all"
        );
    }

    #[test]
    fn position_absolute_resolves_against_its_nearest_ancestor_not_the_root() {
        let tree: Element = view! {
            <div class="root">
                <div class="middle">
                    <div class="abs" />
                </div>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".root { width: 300px; height: 300px; padding-top: 50px; padding-left: 50px; } \
             .middle { position: relative; width: 100px; height: 100px; \
                       padding-top: 20px; padding-left: 20px; } \
             .abs { position: absolute; top: 5px; left: 5px; width: 10px; height: 10px; }",
        );
        let root = arena.roots()[0];
        let middle = arena.children(root)[0];
        let abs = arena.children(middle)[0];
        assert_eq!(
            (layouts[&abs].x, layouts[&abs].y),
            (5.0, 5.0),
            "must resolve relative to its nearest positioned ancestor's own \
             origin, not accumulate the root's own padding too"
        );
    }

    /// Real CSS's actual default is content-box: padding adds to a
    /// declared width/height rather than being carved out of it. Taffy's
    /// own default is border-box, where this same stylesheet would render
    /// at exactly 200x100 with zero content-box room left over.
    #[test]
    fn padding_adds_to_an_explicit_size_instead_of_shrinking_its_content_box() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, layouts) = layout_for(
            &tree,
            ".card { width: 200px; height: 100px; padding-top: 20px; padding-left: 10px; }",
        );
        let node = arena.roots()[0];
        assert_eq!(
            layouts[&node].width, 210.0,
            "200 declared + 10 padding-left"
        );
        assert_eq!(
            layouts[&node].height, 120.0,
            "100 declared + 20 padding-top"
        );
    }

    /// `box-sizing: border-box` puts padding and border inside a declared
    /// size; the same stylesheet without it adds them (the test above).
    #[test]
    fn border_box_keeps_padding_and_border_inside_the_declared_size() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, layouts) = layout_for(
            &tree,
            ".card { box-sizing: border-box; width: 100px; height: 60px; padding: 20px; \
                     border-width: 4px; border-style: solid; border-color: #000000; }",
        );
        let node = arena.roots()[0];
        assert_eq!(layouts[&node].width, 100.0);
        assert_eq!(layouts[&node].height, 60.0);
    }

    #[test]
    fn border_box_cannot_shrink_a_box_below_its_own_padding_and_border() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, layouts) = layout_for(
            &tree,
            ".card { box-sizing: border-box; width: 10px; height: 10px; padding: 12px; }",
        );
        let node = arena.roots()[0];
        assert_eq!(
            layouts[&node].width, 24.0,
            "12 + 12 of padding is the floor"
        );
        assert_eq!(layouts[&node].height, 24.0);
    }

    #[test]
    fn box_sizing_is_per_node_and_does_not_reach_the_children() {
        let tree: Element = view! {
            <div class="outer">
                <div class="inner" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".outer { box-sizing: border-box; width: 100px; padding: 10px; } \
             .inner { width: 50px; height: 20px; padding: 5px; }",
        );
        let outer = arena.roots()[0];
        let inner = arena.children(outer)[0];
        assert_eq!(layouts[&outer].width, 100.0);
        assert_eq!(
            layouts[&inner].width, 60.0,
            "the child is still content-box"
        );
    }

    #[test]
    fn an_inline_block_in_border_box_cannot_shrink_below_its_padding_and_border() {
        let tree = Element::node(
            "p",
            vec![],
            vec![Element::node(
                "button",
                vec![("class".to_string(), "tiny".to_string())],
                vec![Element::text("here")],
            )],
        );
        let (arena, layouts) = layout_for(
            &tree,
            ".tiny { box-sizing: border-box; width: 10px; height: 10px; padding: 12px; \n                      border-width: 0px; }",
        );
        let button = arena.children(arena.roots()[0])[0];
        assert_close(layouts[&button].width, 24.0);
        assert_close(layouts[&button].height, 24.0);
    }

    #[test]
    fn an_inline_block_follows_border_box_too() {
        let tree = Element::node(
            "p",
            vec![],
            vec![Element::node(
                "button",
                vec![("class".to_string(), "fixed".to_string())],
                vec![Element::text("here")],
            )],
        );
        let (arena, layouts) = layout_for(
            &tree,
            ".fixed { box-sizing: border-box; width: 80px; height: 30px; padding: 6px; \
                      border-width: 2px; border-style: solid; border-color: #000000; }",
        );
        let button = arena.children(arena.roots()[0])[0];
        assert_close(layouts[&button].width, 80.0);
        assert_close(layouts[&button].height, 30.0);
    }

    #[test]
    fn a_node_with_no_explicit_size_lays_out_at_zero_by_zero() {
        let tree: Element = view! { <div /> };
        let (arena, layouts) = layout_for(&tree, "");
        let node = arena.roots()[0];
        assert_eq!(layouts[&node].width, 0.0);
        assert_eq!(layouts[&node].height, 0.0);
    }

    #[test]
    fn padding_and_margin_position_a_child_inside_its_parent() {
        let tree: Element = view! {
            <div class="card">
                <div class="child" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .card { width: 200px; height: 100px; padding-top: 8px; padding-left: 8px; }
            .child { width: 50px; height: 30px; margin-top: 10px; margin-left: 5px; }
            ",
        );
        let card = arena.roots()[0];
        let child = arena.children(card)[0];

        assert_eq!(layouts[&child].x, 13.0, "padding-left 8 + margin-left 5");
        assert_eq!(layouts[&child].y, 18.0, "padding-top 8 + margin-top 10");
        assert_eq!(layouts[&child].width, 50.0);
    }

    #[test]
    fn border_adds_to_an_explicit_size_the_same_way_padding_does() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, layouts) = layout_for(
            &tree,
            ".card { width: 200px; height: 100px; border-top-width: 3px; \
             border-top-style: solid; border-left-width: 4px; border-left-style: solid; }",
        );
        let node = arena.roots()[0];
        assert_eq!(
            layouts[&node].width, 204.0,
            "200 declared content width + 4px border-left"
        );
        assert_eq!(
            layouts[&node].height, 103.0,
            "100 declared content height + 3px border-top"
        );
    }

    #[test]
    fn border_and_padding_both_offset_a_childs_position_the_same_way() {
        let tree: Element = view! {
            <div class="card">
                <div class="child" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .card { width: 200px; height: 100px; padding-top: 8px; padding-left: 8px; \
             border-top-width: 3px; border-top-style: solid; \
             border-left-width: 4px; border-left-style: solid; }
            .child { width: 50px; height: 30px; }
            ",
        );
        let card = arena.roots()[0];
        let child = arena.children(card)[0];

        assert_eq!(layouts[&child].x, 12.0, "border-left 4 + padding-left 8");
        assert_eq!(layouts[&child].y, 11.0, "border-top 3 + padding-top 8");
    }

    #[test]
    fn block_children_stack_vertically_in_source_order() {
        let tree: Element = view! {
            <div class="card">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .card { width: 200px; }
            .a { width: 200px; height: 40px; }
            .b { width: 200px; height: 20px; }
            ",
        );
        let card = arena.roots()[0];
        let a = arena.children(card)[0];
        let b = arena.children(card)[1];

        assert_eq!(layouts[&a].y, 0.0);
        assert_eq!(
            layouts[&b].y, 40.0,
            "block layout stacks the second child below the first one's height"
        );
    }

    #[test]
    fn absolute_position_accumulates_ancestor_offsets() {
        let tree: Element = view! {
            <div class="card">
                <div class="inner">
                    <div class="leaf" />
                </div>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .card { width: 200px; height: 200px; padding-top: 10px; padding-left: 10px; }
            .inner { width: 100px; height: 100px; padding-top: 5px; padding-left: 5px; }
            .leaf { width: 10px; height: 10px; }
            ",
        );
        let card = arena.roots()[0];
        let inner = arena.children(card)[0];
        let leaf = arena.children(inner)[0];

        assert_eq!(absolute_position(&arena, &layouts, leaf), (15.0, 15.0));
    }

    #[test]
    fn apply_scroll_offsets_shifts_only_the_scrolled_node_s_direct_children() {
        let tree: Element = view! {
            <div class="card">
                <div class="inner">
                    <div class="leaf" />
                </div>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .card { width: 200px; height: 200px; padding-top: 10px; padding-left: 10px; }
            .inner { width: 100px; height: 100px; padding-top: 5px; padding-left: 5px; }
            .leaf { width: 10px; height: 10px; }
            ",
        );
        let card = arena.roots()[0];
        let inner = arena.children(card)[0];
        let leaf = arena.children(inner)[0];

        let mut scroll_offsets = HashMap::new();
        scroll_offsets.insert(card, (3.0, 4.0));
        let scrolled = apply_scroll_offsets(&arena, &layouts, &scroll_offsets);

        // `card` is not itself anyone's scrolled child -- its own box is
        // untouched.
        assert_eq!(scrolled[&card].x, layouts[&card].x);
        assert_eq!(scrolled[&card].y, layouts[&card].y);
        // `inner` is `card`'s direct child -- shifted by `card`'s offset.
        assert_eq!(scrolled[&inner].x, layouts[&inner].x - 3.0);
        assert_eq!(scrolled[&inner].y, layouts[&inner].y - 4.0);
        // `leaf` is a grandchild, not a direct child of the scrolled node --
        // its own entry is untouched; `absolute_position` against `scrolled`
        // still picks up the shift by summing through the already-shifted
        // `inner`.
        assert_eq!(scrolled[&leaf].x, layouts[&leaf].x);
        assert_eq!(
            absolute_position(&arena, &scrolled, leaf),
            (15.0 - 3.0, 15.0 - 4.0)
        );
    }

    /// A value measured by `florui_text` is compared with a sub-pixel
    /// tolerance rather than for exact equality: layout is not rounded (see
    /// `compute_layout_with_content_extents`), but both sides are `f32`
    /// results of different arithmetic.
    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1.0,
            "expected {actual} to be within 1px of {expected}"
        );
    }

    #[test]
    fn a_text_bearing_leaf_with_no_explicit_size_gets_its_measured_intrinsic_size() {
        let tree: Element = view! { <h2>{"Hi"}</h2> };
        let (arena, layouts) = layout_for(&tree, "");
        let node = arena.roots()[0];

        // h2's real font-size is 24px (1.5em) by default — the framework's
        // own default element stylesheet, not the bare 16px initial value
        // an unstyled element with no matching default rule would get.
        // h2 is bold by default too (the framework's own default stylesheet).
        let expected = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 24.0, 700.0),
            "Hi",
        );
        assert_close(layouts[&node].width, expected.width);
        assert_close(layouts[&node].height, expected.height);
        assert!(expected.width > 0.0, "the font actually measured something");
    }

    #[test]
    fn an_input_with_no_explicit_size_sizes_to_its_value_texts_measured_width() {
        // <input> can never have Element::Text children (Content::Void
        // forbids it) -- its own value attribute, not text_content, must
        // drive intrinsic sizing, or it would always measure to zero.
        // Author CSS strips the framework's own default border/padding
        // (see default_stylesheet.rs) so this test stays focused on text
        // measurement alone, not box-model addition -- covered separately
        // by default_stylesheet.rs's own test.
        let tree: Element = view! { <input type="text" value="Hello" /> };
        let (arena, layouts) = layout_for(&tree, "input { border: none; padding: 0px; }");
        let node = arena.roots()[0];

        let expected = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hello",
        );
        assert_close(layouts[&node].width, expected.width);
        assert!(expected.width > 0.0, "the font actually measured something");
    }

    #[test]
    fn an_input_with_an_empty_value_still_keeps_a_real_nonzero_height() {
        // A real regression: an ordinary empty element legitimately
        // collapses to zero height, but a real `<input>` must not --
        // clearing its own value must never make the box disappear.
        let tree: Element = view! { <input type="text" value="" /> };
        let (arena, layouts) = layout_for(&tree, "");
        let node = arena.roots()[0];
        assert!(
            layouts[&node].height > 0.0,
            "an empty <input> must still keep its own line-height, not collapse to zero"
        );
    }

    #[test]
    fn an_explicit_size_overrides_measured_text_size() {
        let tree: Element = view! { <h2 class="title">{"Hi"}</h2> };
        let (arena, layouts) = layout_for(&tree, ".title { width: 300px; height: 50px; }");
        let node = arena.roots()[0];
        assert_eq!(layouts[&node].width, 300.0);
        assert_eq!(layouts[&node].height, 50.0);
    }

    #[test]
    fn font_size_changes_the_measured_text_size() {
        let tree: Element = view! { <h2 class="big">{"Hi"}</h2> };
        let (arena, layouts) = layout_for(&tree, ".big { font-size: 40px; }");
        let node = arena.roots()[0];

        let expected = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 40.0, 700.0),
            "Hi",
        );
        assert_close(layouts[&node].width, expected.width);
        assert_close(layouts[&node].height, expected.height);
    }

    const PANGRAM: &str = "The quick brown fox jumps over the lazy dog";

    #[test]
    fn line_height_sets_the_height_of_a_text_box() {
        let tree: Element = view! { <div class="a">{PANGRAM}</div> };
        let mut font = florui_text::Font::load_embedded();
        let sans = florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0);

        for (css, line_height) in [
            (".a { width: 200px; }", florui_text::LineHeight::Normal),
            (
                ".a { width: 200px; line-height: 30px; }",
                florui_text::LineHeight::Length(30.0),
            ),
            (
                ".a { width: 200px; line-height: 2; }",
                florui_text::LineHeight::Number(2.0),
            ),
        ] {
            let (arena, layouts) = layout_for(&tree, css);
            let expected = font.measure_wrapped(sans.with_line_height(line_height), PANGRAM, 200.0);
            assert_close(layouts[&arena.roots()[0]].height, expected.height);
        }
        let (arena, layouts) = layout_for(&tree, ".a { width: 200px; line-height: 30px; }");
        assert_close(layouts[&arena.roots()[0]].height, 60.0);
    }

    #[test]
    fn a_number_line_height_follows_the_font_size_of_each_run() {
        let tree = Element::node(
            "p",
            vec![],
            vec![
                Element::text("Hg "),
                Element::node(
                    "span",
                    vec![("class".to_string(), "big".to_string())],
                    vec![Element::text("Hg")],
                ),
            ],
        );
        let (arena, layouts) =
            layout_for(&tree, "p { line-height: 1.5; } .big { font-size: 40px; }");
        let p = arena.roots()[0];
        // The 40px run's own line is 1.5 x 40, so the line cannot be shorter.
        assert!(
            layouts[&p].height >= 60.0 - 0.01,
            "line height {} is shorter than the large run's 60px",
            layouts[&p].height
        );
        let (_, plain) = layout_for(&tree, ".big { font-size: 40px; }");
        assert!(layouts[&p].height > plain[&p].height - 0.01);
    }

    #[test]
    fn inline_lines_are_as_tall_as_css_makes_them_for_spans_and_the_strut() {
        // Every number is what Chrome gives for the same markup.
        let span = |class: &str, text: &str| {
            Element::node(
                "span",
                vec![("class".to_string(), class.to_string())],
                vec![Element::text(text)],
            )
        };
        let height_of = |tag: &'static str, children: Vec<Element>, css: &str| {
            let tree = Element::node(tag, vec![("class".to_string(), "p".to_string())], children);
            let (arena, layouts) = layout_for(&tree, css);
            layouts[&arena.roots()[0]].height
        };

        // A span that differs from its paragraph only in line-height: 50 + 20 + 20.
        let wrapped = height_of(
            "div",
            vec![
                Element::text("small text "),
                span("tall", "tall span"),
                Element::text(" and more words to wrap onto lines"),
            ],
            ".p { width: 200px; line-height: 20px; } .tall { line-height: 50px; }",
        );
        assert_close(wrapped, 90.0);

        // A larger span alone in a container: the container's strut still counts.
        let alone = height_of(
            "div",
            vec![span("big", "Fixed")],
            ".p { width: 200px; line-height: 30px; } .big { font-size: 32px; }",
        );
        assert_close(alone, 36.0);

        // The container's text and a larger span under one length.
        let mixed = height_of(
            "div",
            vec![
                Element::text("Hg "),
                span("big", "Hg"),
                Element::text(" tail"),
            ],
            ".p { width: 280px; line-height: 30px; } .big { font-size: 40px; }",
        );
        assert_close(mixed, 39.0);
    }

    #[test]
    fn letter_and_word_spacing_widen_a_text_box_by_what_the_text_api_measures() {
        let tree: Element = view! {
            <div class="row"><div class="s">{"Spaced out text"}</div></div>
        };
        let mut font = florui_text::Font::load_embedded();
        let sans = florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0);
        for (css, style) in [
            (".row { display: flex; }", sans),
            (
                ".row { display: flex; } .s { letter-spacing: 3px; }",
                sans.with_spacing(3.0, 0.0),
            ),
            (
                ".row { display: flex; } .s { word-spacing: 14px; }",
                sans.with_spacing(0.0, 14.0),
            ),
        ] {
            let (arena, layouts) = layout_for(&tree, css);
            let item = arena.children(arena.roots()[0])[0];
            let expected = font.measure(style, "Spaced out text");
            assert_close(layouts[&item].width, expected.width);
        }
    }

    fn span(class: &str, children: Vec<Element>) -> Element {
        Element::node(
            "span",
            vec![("class".to_string(), class.to_string())],
            children,
        )
    }

    fn paragraph(children: Vec<Element>) -> Element {
        Element::node(
            "div",
            vec![("class".to_string(), "p".to_string())],
            children,
        )
    }

    #[test]
    fn a_span_with_padding_border_and_margin_has_the_box_chrome_gives_it() {
        // Chrome: "hello" then a span with `padding: 0 10px; border: 2px solid;
        // margin: 0 8px` around "world" in a 400px div: 22 tall, and the span's
        // border box at x 44.5, y -2, 66.391 by 26.
        let tree = paragraph(vec![
            Element::text("hello"),
            span("s", vec![Element::text("world")]),
        ]);
        let (arena, layouts) = layout_for(
            &tree,
            ".p { width: 400px; } .s { padding: 0 10px; border: 2px solid #c00; margin: 0 8px; }",
        );
        let container = arena.roots()[0];
        let element = arena.children(container)[0];
        assert_close(layouts[&container].height, 22.0);
        let rect = layouts[&element];
        assert_close(rect.x, 44.5);
        assert_close(rect.y, -2.0);
        assert_close(rect.width, 66.391);
        assert_close(rect.height, 26.0);
    }

    #[test]
    fn vertical_padding_and_border_do_not_make_the_line_taller() {
        // Chrome: padding 10px and a 3px border on the span leave the line 22 and
        // the span's box at y -13, 48 tall, 55.375 wide.
        let tree = paragraph(vec![
            Element::text("aaa "),
            span("s", vec![Element::text("bbb")]),
            Element::text(" ccc"),
        ]);
        let (arena, layouts) = layout_for(
            &tree,
            ".p { width: 300px; } .s { padding: 10px; border: 3px solid #c00; }",
        );
        let container = arena.roots()[0];
        let element = arena.children(container)[0];
        assert_close(layouts[&container].height, 22.0);
        let rect = layouts[&element];
        assert_close(rect.y, -13.0);
        assert_close(rect.height, 48.0);
        assert_close(rect.width, 55.375);
    }

    #[test]
    fn nested_spans_have_boxes_inside_each_other() {
        // Chrome: an outer 24px span holding an inner 12px one: the line is 33,
        // the outer box 33 tall at the top and the inner one 17 tall, 13 down.
        let tree = paragraph(vec![
            Element::text("a "),
            span(
                "o",
                vec![
                    Element::text("b "),
                    span("i", vec![Element::text("c")]),
                    Element::text(" d"),
                ],
            ),
            Element::text(" e"),
        ]);
        let (arena, layouts) = layout_for(
            &tree,
            ".p { width: 300px; } .o { font-size: 24px; } .i { font-size: 12px; }",
        );
        let container = arena.roots()[0];
        let outer = arena.children(container)[0];
        let inner = arena.children(outer)[0];
        assert_close(layouts[&container].height, 33.0);
        assert_close(layouts[&outer].height, 33.0);
        assert_close(layouts[&inner].height, 17.0);
        let (ox, oy) = absolute_position(&arena, &layouts, outer);
        let (ix, iy) = absolute_position(&arena, &layouts, inner);
        assert_close(oy, 0.0);
        assert_close(iy, 13.0);
        assert!(ix > ox && ix + layouts[&inner].width < ox + layouts[&outer].width);
    }

    #[test]
    fn a_click_in_the_empty_corner_of_a_wrapped_span_misses_it() {
        // "aaa " then a span that wraps: its box spans both lines, but on the
        // first it begins after "aaa ", so the line's start is not the span.
        let tree = paragraph(vec![
            Element::text("aaa "),
            span("s", vec![Element::text("bbb ccc ddd")]),
        ]);
        let (arena, styles, layouts) =
            layout_with_styles(&tree, ".p { width: 120px; line-height: 40px; }");
        let container = arena.roots()[0];
        let element = arena.children(container)[0];
        assert!(layouts.contains_key(&piece_id(element, 1)));
        assert!(!layouts.contains_key(&piece_id(element, 2)));
        let hit = |x, y| super::hit_test(&arena, &layouts, &styles, x, y);
        // Inside the span's bounding box but on the first line, before it.
        assert_eq!(hit(2.0, 10.0), Some(container));
        // Inside the piece on the first line, and on the second line.
        assert_eq!(
            hit(layouts[&element].x + layouts[&element].width - 2.0, 10.0),
            Some(element)
        );
        assert_eq!(hit(5.0, 50.0), Some(element));
        // Without the pieces the bounding box answers for the corner.
        let mut whole = layouts.clone();
        whole.retain(|&node, _| node & PIECE == 0);
        assert_eq!(
            super::hit_test(&arena, &whole, &styles, 2.0, 10.0),
            Some(element)
        );
    }

    fn block(class: &str, children: Vec<Element>) -> Element {
        Element::node(
            "div",
            vec![("class".to_string(), class.to_string())],
            children,
        )
    }

    #[test]
    fn an_inline_block_with_a_declared_width_wraps_its_text_at_it() {
        // Chrome: width 40px around "one two three": 40 by 66 (three lines),
        // and the line holding it is 66 tall.
        let tree = paragraph(vec![
            Element::text("x "),
            span("b", vec![Element::text("one two three")]),
            Element::text(" y"),
        ]);
        let (arena, layouts) = layout_for(
            &tree,
            ".p { width: 300px; } .b { display: inline-block; width: 40px; }",
        );
        let container = arena.roots()[0];
        let element = arena.children(container)[0];
        assert_close(layouts[&container].height, 66.0);
        assert_close(layouts[&element].width, 40.0);
        assert_close(layouts[&element].height, 66.0);
    }

    #[test]
    fn an_inline_block_holding_a_larger_span_is_as_tall_as_the_span() {
        // Chrome: an inline-block with a 24px span and " small": 77.344 by 33,
        // the span at its top left (33.781 by 33), the line 33 tall.
        let tree = paragraph(vec![
            Element::text("x "),
            span(
                "b",
                vec![
                    span("c", vec![Element::text("big")]),
                    Element::text(" small"),
                ],
            ),
            Element::text(" y"),
        ]);
        let (arena, layouts) = layout_for(
            &tree,
            ".p { width: 300px; } .b { display: inline-block; } .c { font-size: 24px; }",
        );
        let container = arena.roots()[0];
        let outer = arena.children(container)[0];
        let inner = arena.children(outer)[0];
        assert_close(layouts[&container].height, 33.0);
        assert_close(layouts[&outer].width, 77.344);
        assert_close(layouts[&outer].height, 33.0);
        assert_close(layouts[&outer].y, 0.0);
        assert_close(layouts[&inner].x, 0.0);
        assert_close(layouts[&inner].y, 0.0);
        assert_close(layouts[&inner].width, 33.781);
        assert_close(layouts[&inner].height, 33.0);
    }

    #[test]
    fn an_inline_block_of_blocks_sits_on_its_last_line() {
        // Chrome: two stacked divs in an inline-block: 55.266 by 44, each
        // 22 tall, and the text around it on the second line, so the line is
        // 44 tall with the container's text 22 down.
        let tree = paragraph(vec![
            Element::text("x "),
            span(
                "b",
                vec![
                    block("", vec![Element::text("top")]),
                    block("", vec![Element::text("bottom")]),
                ],
            ),
            Element::text(" y"),
        ]);
        let (arena, layouts) =
            layout_for(&tree, ".p { width: 300px; } .b { display: inline-block; }");
        let container = arena.roots()[0];
        let outer = arena.children(container)[0];
        let (top, bottom) = (arena.children(outer)[0], arena.children(outer)[1]);
        assert_close(layouts[&container].height, 44.0);
        assert_close(layouts[&outer].width, 55.266);
        assert_close(layouts[&outer].height, 44.0);
        assert_close(layouts[&top].y, 0.0);
        assert_close(layouts[&top].height, 22.0);
        assert_close(layouts[&bottom].y, 22.0);
        assert_close(layouts[&bottom].width, 55.266);
    }

    #[test]
    fn text_before_and_after_a_block_is_laid_out_around_it() {
        // Chrome, in 300px: "text before", a 20px block and "text after" make
        // lines 22, 20 and 22 tall, so the container is 64 and the block 22 down.
        let tree = paragraph(vec![
            Element::text("text before"),
            block("blk", vec![]),
            Element::text("text after"),
        ]);
        let (arena, layouts) = layout_for(&tree, ".p { width: 300px; } .blk { height: 20px; }");
        let container = arena.roots()[0];
        let middle = arena.children(container)[0];
        assert_close(layouts[&container].height, 64.0);
        assert_close(layouts[&middle].y, 22.0);
        assert_close(layouts[&middle].height, 20.0);
        assert_close(layouts[&middle].width, 300.0);
        // The two runs are anonymous blocks of their own.
        let first = anonymous_id(container, 0);
        let second = anonymous_id(container, 1);
        assert_close(layouts[&first].y, 0.0);
        assert_close(layouts[&first].height, 22.0);
        assert_close(layouts[&second].y, 42.0);
        assert_close(layouts[&second].height, 22.0);
    }

    #[test]
    fn spans_around_a_block_sit_in_the_anonymous_blocks_beside_it() {
        // Chrome: a span, a 20px block and a span: the spans are 22 tall at the
        // top and 42 down, 54.125 and 35.5 wide.
        let tree = paragraph(vec![
            span("s", vec![Element::text("in span ")]),
            block("blk", vec![]),
            span("t", vec![Element::text("after")]),
        ]);
        let (arena, layouts) = layout_for(&tree, ".p { width: 300px; } .blk { height: 20px; }");
        let container = arena.roots()[0];
        let (first, middle, last) = (
            arena.children(container)[0],
            arena.children(container)[1],
            arena.children(container)[2],
        );
        assert_close(layouts[&container].height, 64.0);
        assert_close(layouts[&first].y, 0.0);
        assert_close(layouts[&first].width, 54.125);
        assert_close(layouts[&middle].y, 22.0);
        assert_close(layouts[&last].y, 42.0);
        assert_close(layouts[&last].width, 35.5);
        assert_close(absolute_position(&arena, &layouts, last).1, 42.0);
    }

    #[test]
    fn text_before_a_block_wraps_in_its_anonymous_block() {
        // Chrome, in 100px: seven words wrap to three lines, then a 10px block,
        // then six words on four more lines: 164 in all, the block 66 down.
        let tree = paragraph(vec![
            Element::text("one two three four five six seven"),
            block("blk", vec![]),
            Element::text("eight nine ten eleven twelve thirteen"),
        ]);
        let (arena, layouts) = layout_for(&tree, ".p { width: 100px; } .blk { height: 10px; }");
        let container = arena.roots()[0];
        let middle = arena.children(container)[0];
        assert_close(layouts[&container].height, 164.0);
        assert_close(layouts[&middle].y, 66.0);
    }

    #[test]
    fn whitespace_between_blocks_makes_no_line() {
        let tree = paragraph(vec![
            block("blk", vec![]),
            Element::text("  "),
            block("blk", vec![]),
        ]);
        let (arena, layouts) = layout_for(&tree, ".p { width: 100px; } .blk { height: 10px; }");
        let container = arena.roots()[0];
        assert_close(layouts[&container].height, 20.0);
        assert!(!layouts.contains_key(&anonymous_id(container, 0)));
    }

    /// The height of a 90px paragraph of `text` under `css`.
    fn wrapped_height(text: &str, css: &str) -> f32 {
        let tree = paragraph(vec![Element::text(text)]);
        let (arena, layouts) = layout_for(&tree, &format!(".p {{ width: 90px; {css} }}"));
        layouts[&arena.roots()[0]].height
    }

    #[test]
    fn white_space_wraps_and_collapses_as_chromium_does() {
        // Chromium, 90px wide, lines of 22px.
        let words = "one two three four five six";
        for (value, lines) in [
            ("normal", 3.0),
            ("nowrap", 1.0),
            ("pre", 1.0),
            ("pre-wrap", 3.0),
            ("pre-line", 3.0),
            ("break-spaces", 3.0),
        ] {
            let css = format!("white-space: {value};");
            assert_close(wrapped_height(words, &css), 22.0 * lines);
        }
        // Spaces and a line break: collapsed into one line, or kept.
        let spaced = "a   b\n  c  d  ";
        for (value, lines) in [
            ("normal", 1.0),
            ("nowrap", 1.0),
            ("pre", 2.0),
            ("pre-wrap", 2.0),
            ("pre-line", 2.0),
            ("break-spaces", 2.0),
        ] {
            let css = format!("white-space: {value};");
            assert_close(wrapped_height(spaced, &css), 22.0 * lines);
        }
    }

    #[test]
    fn a_space_collapses_with_the_one_before_even_across_an_element() {
        // Chromium: in "a <span> b </span> c" the span is 13.953 wide, in
        // "a <span>  b</span>  c" it is 9.797 wide, and the line is one.
        let measure = |inside: &str, after: &str| {
            let tree = paragraph(vec![
                Element::text("a "),
                span("s", vec![Element::text(inside)]),
                Element::text(after),
            ]);
            let (arena, layouts) = layout_for(&tree, ".p { width: 300px; }");
            let container = arena.roots()[0];
            let element = arena.children(container)[0];
            assert_close(layouts[&container].height, 22.0);
            layouts[&element].width
        };
        assert_close(measure(" b ", " c"), 13.953);
        assert_close(measure("  b", "  c"), 9.797);
    }

    #[test]
    fn word_break_and_overflow_wrap_break_inside_a_long_word_as_chromium_does() {
        // Chromium, 60px wide: a 135px word and a 105px one.
        let text = "Supercalifragilistic expialidocious";
        let at_60 = |css: &str| {
            let tree = paragraph(vec![Element::text(text)]);
            let (arena, layouts) = layout_for(&tree, &format!(".p {{ width: 60px; {css} }}"));
            layouts[&arena.roots()[0]].height
        };
        assert_close(at_60(""), 44.0);
        assert_close(at_60("word-break: break-all;"), 110.0);
        assert_close(at_60("word-break: keep-all;"), 44.0);
        assert_close(at_60("overflow-wrap: break-word;"), 110.0);
        assert_close(at_60("overflow-wrap: anywhere;"), 110.0);
        assert_close(at_60("overflow-wrap: normal;"), 44.0);
    }

    #[test]
    fn pre_wrap_with_break_word_breaks_a_long_word_as_a_textarea_does() {
        // What a textarea's user-agent style says: the 135px word breaks into
        // three 22px lines in 60px, as in Chromium's five lines for two words.
        let tree: Element = Element::node(
            "div",
            vec![("class".into(), "p".into())],
            vec![Element::node(
                "span",
                vec![("class".into(), "t".into())],
                vec![Element::text("Supercalifragilistic")],
            )],
        );
        let (arena, layouts) = layout_for(
            &tree,
            ".p { width: 60px; } .t { display: block; white-space: pre-wrap; overflow-wrap: break-word; line-height: 22px; }",
        );
        let element = arena.children(arena.roots()[0])[0];
        assert_close(layouts[&element].height, 66.0);
    }

    #[test]
    fn a_glyph_from_a_fallback_font_does_not_change_the_line_height() {
        // Chrome: "Hello 😀 world" in 400px is 22 tall under a normal
        // line height, 30 under 30px and 24 under 1.5, the same as without it.
        for (css, height) in [
            ("", 22.0),
            ("line-height: 30px;", 30.0),
            ("line-height: 1.5;", 24.0),
        ] {
            let tree = paragraph(vec![Element::text("Hello \u{1F600} world")]);
            let (arena, layouts) = layout_for(&tree, &format!(".p {{ width: 400px; {css} }}"));
            assert_close(layouts[&arena.roots()[0]].height, height);
        }
    }

    #[test]
    fn a_box_inside_a_span_lands_where_it_would_in_the_container() {
        let button = || {
            Element::node(
                "button",
                vec![("class".to_string(), "b".to_string())],
                vec![Element::text("ok")],
            )
        };
        let css = ".p { width: 300px; } .b { padding: 2px 6px; }";
        let loose = paragraph(vec![Element::text("x "), button()]);
        let (arena, layouts) = layout_for(&loose, css);
        let loose_at = absolute_position(&arena, &layouts, arena.children(arena.roots()[0])[0]);
        let wrapped = paragraph(vec![Element::text("x "), span("w", vec![button()])]);
        let (arena, layouts) = layout_for(&wrapped, css);
        let element = arena.children(arena.roots()[0])[0];
        let inside = arena.children(element)[0];
        let wrapped_at = absolute_position(&arena, &layouts, inside);
        assert_close(wrapped_at.0, loose_at.0);
        assert_close(wrapped_at.1, loose_at.1);
    }

    fn hit_styled(
        tree: &Element,
        css: &str,
        points: &[(f32, f32)],
    ) -> (Arena, Vec<Option<NodeId>>) {
        let (arena, styles, layouts) = layout_with_styles(tree, css);
        let hits = points
            .iter()
            .map(|&(x, y)| super::hit_test(&arena, &layouts, &styles, x, y))
            .collect();
        (arena, hits)
    }

    #[test]
    fn screen_bounds_follow_a_nodes_and_its_ancestors_transforms() {
        let tree: Element = view! {
            <div class="parent">
                <div class="child" />
                <div class="turned" />
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            ".parent { width: 100px; height: 100px; transform: translate(200px, 0px); } \
             .child { width: 10px; height: 10px; } \
             .turned { width: 40px; height: 10px; transform: rotate(90deg); }",
        );
        let bounds = screen_bounds(&arena, &layouts, &styles, 1.0);
        let parent = arena.roots()[0];
        let [child, turned] = [arena.children(parent)[0], arena.children(parent)[1]];
        assert_eq!(bounds[&parent], (200.0, 0.0, 100.0, 100.0));
        assert_eq!(
            bounds[&child],
            (200.0, 0.0, 10.0, 10.0),
            "moved with its parent"
        );
        let (x, y, w, h) = bounds[&turned];
        // 40x10 at (200, 10) turned about its centre (220, 15): 10x40 at (215, -5).
        for (got, want) in [(x, 215.0), (y, -5.0), (w, 10.0), (h, 40.0)] {
            assert!(
                (got - want).abs() < 1e-3,
                "{:?} vs {:?}",
                bounds[&turned],
                (215.0, -5.0, 10.0, 40.0)
            );
        }
    }

    #[test]
    fn screen_bounds_of_an_untransformed_tree_are_the_layout_boxes() {
        let tree: Element = view! { <div class="box" /> };
        let (arena, styles, layouts) =
            layout_with_styles(&tree, ".box { width: 30px; height: 20px; }");
        let node = arena.roots()[0];
        assert_eq!(
            screen_bounds(&arena, &layouts, &styles, 1.0)[&node],
            (0.0, 0.0, 30.0, 20.0)
        );
    }
    #[test]
    fn a_translated_box_is_hit_where_it_is_drawn() {
        let tree: Element = view! { <div class="box" /> };
        let (arena, hits) = hit_styled(
            &tree,
            ".box { width: 20px; height: 20px; transform: translate(50px, 30px); }",
            &[(10.0, 10.0), (60.0, 40.0)],
        );
        let node = arena.roots()[0];
        assert_eq!(
            hits,
            [None, Some(node)],
            "its old spot is empty, the new one hits"
        );
    }

    #[test]
    fn a_rotated_box_is_hit_inside_its_rotated_outline_only() {
        // 100x20 about its centre (50, 10), turned 90deg clockwise: it now
        // stands 20 wide and 100 tall around x 40..60, y -40..60.
        let tree: Element = view! { <div class="bar" /> };
        let (arena, hits) = hit_styled(
            &tree,
            ".bar { width: 100px; height: 20px; transform: rotate(90deg); }",
            &[(50.0, 50.0), (5.0, 10.0), (50.0, 5.0)],
        );
        let node = arena.roots()[0];
        assert_eq!(
            hits,
            [Some(node), None, Some(node)],
            "the turned bar covers (50, 50) but no longer its old left end"
        );
    }

    #[test]
    fn a_scaled_box_grows_about_its_origin() {
        let tree: Element = view! { <div class="box" /> };
        let (arena, hits) = hit_styled(
            &tree,
            ".box { width: 20px; height: 20px; transform: scale(3); }",
            &[(-15.0, -15.0), (39.0, 39.0), (45.0, 45.0)],
        );
        let node = arena.roots()[0];
        assert_eq!(
            hits,
            [Some(node), Some(node), None],
            "centre-origin scale 3 spans -20..40 on both axes"
        );
    }

    #[test]
    fn a_childs_hit_follows_its_ancestors_transform() {
        let tree: Element = view! {
            <div class="parent">
                <div class="child" />
            </div>
        };
        let (arena, hits) = hit_styled(
            &tree,
            ".parent { width: 50px; height: 50px; transform: translate(100px, 0px); } \
             .child { width: 10px; height: 10px; }",
            &[(105.0, 5.0), (5.0, 5.0), (140.0, 40.0)],
        );
        let parent = arena.roots()[0];
        let child = arena.children(parent)[0];
        assert_eq!(hits, [Some(child), None, Some(parent)]);
    }

    #[test]
    fn a_clipping_ancestor_clips_where_it_is_drawn_not_where_it_was_laid_out() {
        let tree: Element = view! {
            <div class="clip">
                <div class="wide" />
            </div>
        };
        let (arena, hits) = hit_styled(
            &tree,
            ".clip { width: 20px; height: 20px; overflow: hidden; transform: translate(100px, 0px); } \
             .wide { width: 200px; height: 20px; }",
            &[(110.0, 10.0), (150.0, 10.0), (10.0, 10.0)],
        );
        let clip = arena.roots()[0];
        let wide = arena.children(clip)[0];
        assert_eq!(
            hits,
            [Some(wide), None, None],
            "the overflowing part is clipped to the moved box; the old spot is empty"
        );
    }

    #[test]
    fn a_transform_that_flattens_the_plane_hides_its_whole_subtree() {
        let tree: Element = view! {
            <div class="flat">
                <div class="child" />
            </div>
        };
        let (_, hits) = hit_styled(
            &tree,
            ".flat { width: 50px; height: 50px; transform: scale(0); } \
             .child { width: 10px; height: 10px; }",
            &[(5.0, 5.0), (25.0, 25.0)],
        );
        assert_eq!(hits, [None, None]);
    }

    #[test]
    fn a_percentage_translate_and_a_custom_origin_are_honoured() {
        let tree: Element = view! { <div class="box" /> };
        let (arena, hits) = hit_styled(
            &tree,
            ".box { width: 40px; height: 40px; transform-origin: 0px 0px; \
                   transform: translate(100%, 0px) scale(2); }",
            &[(45.0, 5.0), (115.0, 70.0), (10.0, 10.0)],
        );
        let node = arena.roots()[0];
        assert_eq!(
            hits,
            [Some(node), Some(node), None],
            "scale 2 about the corner, then moved one box-width right: x 40..120, y 0..80"
        );
    }
    #[test]
    fn hit_test_finds_the_deepest_node_containing_the_point() {
        let tree: Element = view! {
            <div class="card">
                <div class="button" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .card { width: 100px; height: 60px; padding-top: 10px; padding-left: 10px; }
            .button { width: 40px; height: 20px; }
            ",
        );
        let card = arena.roots()[0];
        let button = arena.children(card)[0];

        assert_eq!(
            hit_test(&arena, &layouts, 20.0, 20.0),
            Some(button),
            "(20, 20) is inside the button, nested inside the card"
        );
        assert_eq!(
            hit_test(&arena, &layouts, 5.0, 5.0),
            Some(card),
            "(5, 5) is inside the card's padding, outside the button"
        );
        assert_eq!(
            hit_test(&arena, &layouts, 200.0, 200.0),
            None,
            "outside every box"
        );
    }

    /// Regression guard for a label ("Wi-Fi") that painted on two lines
    /// beside a fixed-size box in a flex row. With Taffy's default rounding
    /// layout gave the label 36px while its text measures 36.26px; paint
    /// reshapes the text at the laid-out width, so it wrapped at its only
    /// break opportunity, the hyphen. Confirmed by turning rounding off,
    /// which is now how layout runs: the label gets its exact 36.26px and
    /// does not wrap at any scale.
    #[test]
    fn a_hyphenated_label_beside_a_fixed_box_does_not_wrap_at_any_hidpi_scale() {
        let tree: Element = view! {
            <div class="row">
                <div class="track" />
                <p class="label">{"Wi-Fi"}</p>
            </div>
        };
        let (arena, _styles, layouts) = layout_with_styles(
            &tree,
            ".row { display: flex; flex-direction: row; align-items: center; } \
             .track { width: 44px; height: 24px; } \
             .label { margin-left: 12px; margin-top: 0px; margin-bottom: 0px; font-size: 16px; }",
        );
        let label = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "label"))
            .unwrap();
        let laid_out = layouts[&label].width;

        let mut font = florui_text::Font::load_embedded();
        let family = florui_text::FontFamily::SansSerif;
        let natural = font.shape_wrapped(
            florui_text::TextStyle::new(family, 16.0, 400.0),
            "Wi-Fi",
            f32::MAX,
        );
        eprintln!(
            "laid out width {laid_out}, natural width {}, one line {}",
            natural.width, natural.height
        );
        assert!(
            laid_out >= natural.width,
            "layout gave the label {laid_out}px, narrower than its {}px of text",
            natural.width
        );

        for scale in [1.0_f32, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5, 3.0] {
            let wrap_width = (laid_out * scale) / scale;
            let shaped = font.shape_wrapped(
                florui_text::TextStyle::new(family, 16.0, 400.0),
                "Wi-Fi",
                wrap_width,
            );
            eprintln!(
                "scale {scale}: wrap width {wrap_width}, height {} (one line {})",
                shaped.height, natural.height
            );
            assert_eq!(
                shaped.height, natural.height,
                "wrapped onto two lines at scale {scale} (wrap width {wrap_width})"
            );
        }
    }

    #[test]
    fn hit_test_skips_a_pointer_events_none_node_and_falls_through_to_what_is_beneath() {
        let tree: Element = view! {
            <div class="card">
                <div class="overlay" />
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            ".card { width: 40px; height: 40px; } \
             .overlay { width: 40px; height: 40px; pointer-events: none; }",
        );
        let card = arena.roots()[0];
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 10.0, 10.0),
            Some(card),
            "the click passes through the overlay to the card"
        );
    }

    #[test]
    fn hit_test_lets_a_descendant_re_enable_pointer_events() {
        let tree: Element = view! {
            <div class="card">
                <div class="overlay">
                    <div class="button" />
                </div>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            ".card { width: 40px; height: 40px; } \
             .overlay { width: 40px; height: 40px; pointer-events: none; } \
             .button { width: 10px; height: 10px; pointer-events: auto; }",
        );
        let button = arena.find(|a, id| a.classes(id).iter().any(|c| c == "button"));
        assert_eq!(super::hit_test(&arena, &layouts, &styles, 5.0, 5.0), button);
    }

    #[test]
    fn hit_test_misses_a_rounded_corner_but_hits_the_edge_midpoint() {
        let tree: Element = view! { <div class="disc" /> };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            ".disc { width: 20px; height: 20px; border-radius: 50%; }",
        );
        let disc = arena.roots()[0];
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 10.0, 10.0),
            Some(disc)
        );
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 10.0, 0.5),
            Some(disc)
        );
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 1.0, 1.0),
            None,
            "the box's corner lies outside its own rounded outline"
        );
    }

    #[test]
    fn hit_test_clips_a_child_to_a_rounded_overflow_hidden_ancestor() {
        let tree: Element = view! {
            <div class="frame">
                <div class="content" />
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            ".frame { width: 20px; height: 20px; overflow: hidden; border-radius: 10px; } \
             .content { width: 40px; height: 40px; }",
        );
        let frame = arena.roots()[0];
        let content = arena.children(frame)[0];
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 10.0, 10.0),
            Some(content)
        );
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 1.0, 1.0),
            None,
            "the child is clipped out at the frame's cut corner"
        );
        assert_eq!(
            super::hit_test(&arena, &layouts, &styles, 30.0, 30.0),
            None,
            "and past the frame's padding box altogether"
        );
    }

    #[test]
    fn hit_test_prefers_an_overlay_root_over_overlapping_document_content() {
        let document: Element = view! { <div class="doc" /> };
        let overlay: Element = view! { <div class="overlay" /> };
        let available = Size {
            width: AvailableSpace::Definite(200.0),
            height: AvailableSpace::Definite(200.0),
        };
        let (arena, layouts) = layout_with_overlays_for(
            &document,
            &overlay,
            ".doc { width: 200px; height: 200px; } .overlay { width: 200px; height: 200px; }",
            available,
        );
        let overlay_root = arena.overlay_roots()[0];

        assert_eq!(
            hit_test(&arena, &layouts, 50.0, 50.0),
            Some(overlay_root),
            "an overlay root fully covering the document must win hit-testing"
        );
    }

    #[test]
    fn nested_inline_text_sizes_its_containers_real_inline_formatting_context() {
        // `span` is a real `display: inline` element (the framework's own
        // default stylesheet, not a hand-rolled special case) — a `<div>`
        // whose only content is one inline element establishes a real
        // inline formatting context for it, the same as a real browser,
        // rather than giving `span` its own standalone block box the way
        // this crate did before real inline layout existed. A plain
        // `Inline` child doesn't get its own `BoxLayout` in this slice
        // (see this module's own top-level doc for the bound) — its text
        // instead sizes its container's one inline-formatting-context box,
        // which is what this test now checks instead of `span`'s own.
        let tree: Element = view! {
            <div>
                <span>{"Hi"}</span>
            </div>
        };
        let (arena, layouts) = layout_for(&tree, "");
        let card = arena.roots()[0];

        let expected = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hi",
        );
        assert_close(layouts[&card].width, expected.width);
        assert_close(layouts[&card].height, expected.height);
    }

    #[test]
    fn text_wraps_and_grows_taller_inside_a_narrow_explicitly_sized_container() {
        let tree: Element = view! {
            <div class="card">
                <h2>{"one two three four five six seven eight nine ten"}</h2>
            </div>
        };
        // No explicit width on the h2 itself — it must still wrap, inheriting
        // its available width from the block container's own resolved
        // content width, the same as a real browser's default block
        // formatting context.
        let (arena, layouts) = layout_for(&tree, ".card { width: 100px; }");
        let card = arena.roots()[0];
        let h2 = arena.children(card)[0];

        let mut font = florui_text::Font::load_embedded();
        let unwrapped = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 700.0),
            "one two three four five six seven eight nine ten",
        );

        assert!(
            layouts[&h2].height > unwrapped.height,
            "wrapping across a 100px container must take more than one line's height"
        );
        assert!(
            layouts[&h2].width <= 100.0 + 1.0,
            "a wrapped leaf must not exceed its container's own width"
        );
    }

    #[test]
    fn an_explicit_width_on_the_text_node_itself_also_wraps_it() {
        let tree: Element = view! { <h2 class="narrow">{"one two three four five"}</h2> };
        let (arena, layouts) = layout_for(&tree, ".narrow { width: 60px; }");
        let node = arena.roots()[0];

        let mut font = florui_text::Font::load_embedded();
        let unwrapped = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 700.0),
            "one two three four five",
        );

        assert_eq!(layouts[&node].width, 60.0, "the explicit width still wins");
        assert!(
            layouts[&node].height > unwrapped.height,
            "an explicit width on the leaf itself must also trigger wrapping"
        );
    }

    #[test]
    fn a_wide_enough_container_does_not_wrap_short_text() {
        let tree: Element = view! {
            <div class="card">
                <h2>{"Hi"}</h2>
            </div>
        };
        let (arena, layouts) = layout_for(&tree, ".card { width: 400px; }");
        let card = arena.roots()[0];
        let h2 = arena.children(card)[0];

        // h2's default font-size is 24px (1.5em) — see the identical note
        // on `a_text_bearing_leaf_with_no_explicit_size_gets_its_measured_intrinsic_size`.
        let expected = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 24.0, 700.0),
            "Hi",
        );
        assert_close(layouts[&h2].height, expected.height);
    }

    #[test]
    fn font_family_monospace_measures_with_the_monospace_embedded_font() {
        let tree: Element = view! { <span class="code">{"AAAAA"}</span> };
        let (arena, layouts) = layout_for(&tree, ".code { font-family: monospace; }");
        let node = arena.roots()[0];

        let expected = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::Monospace, 16.0, 400.0),
            "AAAAA",
        );
        assert_close(layouts[&node].width, expected.width);
    }

    #[test]
    fn no_font_family_declared_measures_with_the_sans_serif_default() {
        let tree: Element = view! { <span>{"AAAAA"}</span> };
        let (arena, layouts) = layout_for(&tree, "");
        let node = arena.roots()[0];

        let sans_serif = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "AAAAA",
        );
        let monospace = florui_text::Font::load_embedded().measure(
            florui_text::TextStyle::new(florui_text::FontFamily::Monospace, 16.0, 400.0),
            "AAAAA",
        );
        assert_close(layouts[&node].width, sans_serif.width);
        assert!(
            (layouts[&node].width - monospace.width).abs() > 1.0,
            "must actually be measuring with the proportional sans-serif default, \
             not coincidentally matching the monospace width"
        );
    }

    #[test]
    fn a_flex_row_lays_out_children_left_to_right() {
        let tree: Element = view! {
            <div class="row">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; width: 300px; height: 50px; }
            .a { width: 100px; height: 50px; }
            .b { width: 80px; height: 50px; }
            ",
        );
        let row = arena.roots()[0];
        let a = arena.children(row)[0];
        let b = arena.children(row)[1];

        assert_eq!(layouts[&a].x, 0.0);
        assert_eq!(layouts[&b].x, 100.0, "b starts right where a's 100px ends");
        assert_eq!(
            layouts[&a].y, 0.0,
            "a flex row keeps children on the same cross-axis line by default"
        );
    }

    #[test]
    fn a_flex_column_lays_out_children_top_to_bottom() {
        let tree: Element = view! {
            <div class="col">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .col { display: flex; flex-direction: column; width: 100px; height: 300px; }
            .a { width: 100px; height: 40px; }
            .b { width: 100px; height: 20px; }
            ",
        );
        let col = arena.roots()[0];
        let a = arena.children(col)[0];
        let b = arena.children(col)[1];

        assert_eq!(layouts[&a].y, 0.0);
        assert_eq!(layouts[&b].y, 40.0, "b starts right where a's 40px ends");
    }

    #[test]
    fn justify_content_space_between_pushes_children_to_the_edges() {
        let tree: Element = view! {
            <div class="row">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; justify-content: space-between; width: 300px; height: 50px; }
            .a { width: 50px; height: 50px; }
            .b { width: 50px; height: 50px; }
            ",
        );
        let row = arena.roots()[0];
        let a = arena.children(row)[0];
        let b = arena.children(row)[1];

        assert_eq!(
            layouts[&a].x, 0.0,
            "the first child stays flush with the start"
        );
        assert_eq!(
            layouts[&b].x, 250.0,
            "the last child is flush with the end: 300 container - 50 own width"
        );
    }

    #[test]
    fn align_items_center_centers_a_shorter_child_on_the_cross_axis() {
        let tree: Element = view! {
            <div class="row">
                <div class="short" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; align-items: center; width: 100px; height: 100px; }
            .short { width: 20px; height: 20px; }
            ",
        );
        let row = arena.roots()[0];
        let short = arena.children(row)[0];

        assert_eq!(
            layouts[&short].y, 40.0,
            "centered in a 100px-tall row: (100 - 20) / 2"
        );
    }

    #[test]
    fn flex_grow_distributes_remaining_space_proportionally() {
        let tree: Element = view! {
            <div class="row">
                <div class="one" />
                <div class="two" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; width: 300px; height: 50px; }
            .one { flex-grow: 1; height: 50px; }
            .two { flex-grow: 2; height: 50px; }
            ",
        );
        let row = arena.roots()[0];
        let one = arena.children(row)[0];
        let two = arena.children(row)[1];

        assert_eq!(
            layouts[&one].width, 100.0,
            "1 share of 300px free space (both start at 0 width)"
        );
        assert_eq!(layouts[&two].width, 200.0, "2 shares of the same 300px");
    }

    #[test]
    fn flex_shrink_zero_keeps_a_child_at_its_basis_even_when_siblings_overflow() {
        let tree: Element = view! {
            <div class="row">
                <div class="fixed" />
                <div class="flexible" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; width: 150px; height: 50px; }
            .fixed { width: 100px; height: 50px; flex-shrink: 0; }
            .flexible { width: 100px; height: 50px; }
            ",
        );
        let row = arena.roots()[0];
        let fixed = arena.children(row)[0];

        assert_eq!(
            layouts[&fixed].width, 100.0,
            "flex-shrink: 0 must not shrink even though the row is 50px too narrow \
             for both children's own widths"
        );
    }

    #[test]
    /// A `<select multiple>`'s own box carries no inset of its own (see
    /// `florui_style::default_stylesheet`'s `select[multiple="true"]`
    /// override) — each option must stretch symmetrically to the full
    /// content width either side, not just on its own declared padding.
    fn a_multiple_selects_option_stretches_symmetrically_with_no_container_inset() {
        let tree: Element = view! {
            <select id="fruits" class="listbox" multiple="true">
                <option value="banana">{"Banana"}</option>
            </select>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .listbox { display: flex; flex-direction: column; width: 200px; \
                       border-width: 2px; border-style: solid; }
            option { padding-top: 6px; padding-right: 10px; padding-bottom: 6px; \
                     padding-left: 10px; }
            ",
        );
        let select = arena.roots()[0];
        let option = arena.children(select)[0];

        assert_eq!(
            layouts[&option].x, 2.0,
            "flush against the select's own left border"
        );
        assert_eq!(
            layouts[&option].x + layouts[&option].width,
            layouts[&select].x + layouts[&select].width - 2.0,
            "flush against the select's own right border, symmetrically"
        );
    }

    #[test]
    fn gap_adds_space_between_flex_children_without_affecting_the_first_ones_position() {
        let tree: Element = view! {
            <div class="row">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; column-gap: 10px; width: 300px; height: 50px; }
            .a { width: 50px; height: 50px; }
            .b { width: 50px; height: 50px; }
            ",
        );
        let row = arena.roots()[0];
        let a = arena.children(row)[0];
        let b = arena.children(row)[1];

        assert_eq!(layouts[&a].x, 0.0);
        assert_eq!(layouts[&b].x, 60.0, "a's 50px width + 10px column-gap");
    }

    /// A node's own `display` never affects how its parent places *it* —
    /// only how it places its own children.
    #[test]
    fn a_block_child_inside_a_flex_row_is_still_laid_out_by_the_flex_algorithm() {
        let tree: Element = view! {
            <div class="row">
                <div class="block-child">
                    <div class="grandchild" />
                </div>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; width: 200px; height: 50px; }
            .block-child { width: 80px; height: 50px; }
            .grandchild { width: 20px; height: 20px; }
            ",
        );
        let row = arena.roots()[0];
        let block_child = arena.children(row)[0];

        assert_eq!(
            layouts[&block_child].x, 0.0,
            "the flex row still positions its block-display child as a flex item"
        );
    }

    #[test]
    fn align_items_baseline_lines_up_differently_sized_text_by_their_shared_baseline() {
        let tree: Element = view! {
            <div class="row">
                <span class="small">{"Hg"}</span>
                <span class="big">{"Hg"}</span>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .row { display: flex; align-items: baseline; height: 100px; }
            .small { font-size: 16px; }
            .big { font-size: 40px; }
            ",
        );
        let row = arena.roots()[0];
        let small = arena.children(row)[0];
        let big = arena.children(row)[1];

        let mut font = florui_text::Font::load_embedded();
        let small_metrics = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hg",
        );
        let big_metrics = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 40.0, 400.0),
            "Hg",
        );

        // Real baseline alignment: each item's own (y + its baseline offset)
        // must land on the same line — not the same `y`, and not simply
        // top- or center-aligned, both of which would put these at
        // different combined baselines since the two font sizes have
        // different ascents.
        assert_close(
            layouts[&small].y + small_metrics.baseline,
            layouts[&big].y + big_metrics.baseline,
        );
        assert!(
            layouts[&small].y > layouts[&big].y,
            "the smaller text's shorter ascent means it starts lower, not at the same y \
             (small starts at {}, big at {})",
            layouts[&small].y,
            layouts[&big].y
        );
    }

    #[test]
    fn inline_text_flows_beside_a_short_inline_element_when_it_fits_on_one_line() {
        // Real mixed inline content — `Element::node`/`Element::text`
        // directly, the same construction `florui_style::tree`'s own
        // interleaving tests use, since `view!` has no ergonomic syntax
        // for bare text directly adjacent to a child element.
        let tree = Element::node(
            "p",
            vec![],
            vec![
                Element::text("Hello "),
                Element::node("span", vec![], vec![Element::text("world")]),
                Element::text("!"),
            ],
        );
        let mut font = florui_text::Font::load_embedded();
        let one_line = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hello world!",
        );

        let (arena, layouts) = layout_for(&tree, "");
        let p = arena.roots()[0];

        assert_close(layouts[&p].height, one_line.height);
        assert_close(layouts[&p].width, one_line.width);
    }

    #[test]
    fn text_wraps_to_a_new_line_around_an_inline_element_when_it_does_not_fit() {
        let tree = Element::node(
            "div",
            vec![("class".to_string(), "narrow".to_string())],
            vec![Element::node(
                "p",
                vec![],
                vec![
                    Element::text("Hello "),
                    Element::node("span", vec![], vec![Element::text("world")]),
                    Element::text("!"),
                ],
            )],
        );
        let mut font = florui_text::Font::load_embedded();
        let first_word = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hello",
        );
        let one_line = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hello world!",
        );

        // Wide enough for "Hello" but not for "Hello world!" — the inline
        // span's own "world!" (no space before "!") must wrap to a second
        // line, the same as real CSS wrapping around/beside an inline
        // element.
        let css = format!(".narrow {{ width: {}px; }}", first_word.width + 1.0);
        let (arena, layouts) = layout_for(&tree, &css);
        let div = arena.roots()[0];
        let p = arena.children(div)[0];

        assert!(
            layouts[&p].height > one_line.height,
            "the inline span's \"world!\" must have wrapped to a second line: \
             p height {}, one unwrapped line's height {}",
            layouts[&p].height,
            one_line.height
        );
    }

    #[test]
    fn an_inline_elements_own_long_text_wraps_across_multiple_lines_within_the_flow() {
        let tree = Element::node(
            "div",
            vec![("class".to_string(), "narrow".to_string())],
            vec![Element::node(
                "p",
                vec![],
                vec![Element::node(
                    "span",
                    vec![],
                    vec![Element::text("one two three four five six seven")],
                )],
            )],
        );
        let mut font = florui_text::Font::load_embedded();
        let one_word = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "one",
        );
        let unwrapped = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "one two three four five six seven",
        );

        let css = format!(".narrow {{ width: {}px; }}", one_word.width + 5.0);
        let (arena, layouts) = layout_for(&tree, &css);
        let div = arena.roots()[0];
        let p = arena.children(div)[0];

        assert!(
            layouts[&p].height > unwrapped.height,
            "a narrow enough container must wrap the inline span's own long text \
             across multiple lines: p height {}, one unwrapped line's height {}",
            layouts[&p].height,
            unwrapped.height
        );
    }

    #[test]
    fn inline_block_sizes_to_its_own_content_while_flowing_inline() {
        let tree = Element::node(
            "p",
            vec![],
            vec![
                Element::text("Click "),
                Element::node("button", vec![], vec![Element::text("here")]),
            ],
        );
        let mut font = florui_text::Font::load_embedded();
        let preceding = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Click ",
        );
        let button_text = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "here",
        );

        let (arena, layouts) = layout_for(&tree, "");
        let p = arena.roots()[0];
        let button = arena.children(p)[0];

        // Sized to its own content, not stretched to the container's width
        // the way a block child would be -- plus the 1px default border
        // every `<button>` gets (`default_stylesheet.rs`'s own rule),
        // which now correctly contributes to its own box.
        assert_close(layouts[&button].width, button_text.width + 2.0);
        assert_close(layouts[&button].height, button_text.height + 2.0);

        // Flowing inline: positioned after the preceding text on the same
        // line, not stacked below it as its own block box.
        assert!(
            layouts[&button].x > preceding.width - 1.0,
            "the button must sit after \"Click \" (x = {}), not at the line's start",
            layouts[&button].x
        );
        assert!(
            layouts[&button].y < button_text.height,
            "the button must share the first line, not be pushed onto a line of its \
             own (y = {})",
            layouts[&button].y
        );
    }

    #[test]
    fn an_inline_blocks_own_padding_and_border_add_to_its_content_size() {
        let tree = Element::node(
            "p",
            vec![],
            vec![Element::node(
                "button",
                vec![("class".to_string(), "padded".to_string())],
                vec![Element::text("here")],
            )],
        );
        let mut font = florui_text::Font::load_embedded();
        let text = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "here",
        );

        let (arena, layouts) = layout_for(
            &tree,
            ".padded { padding-top: 8px; padding-right: 14px; padding-bottom: 8px; \
                       padding-left: 14px; border-width: 2px; border-style: solid; \
                       border-color: #000000; }",
        );
        let p = arena.roots()[0];
        let button = arena.children(p)[0];

        assert_close(layouts[&button].width, text.width + 14.0 + 14.0 + 2.0 + 2.0);
        assert_close(layouts[&button].height, text.height + 8.0 + 8.0 + 2.0 + 2.0);
    }

    #[test]
    fn line_height_grows_with_the_tallest_mixed_font_size_run() {
        let tree = Element::node(
            "p",
            vec![],
            vec![
                Element::text("Hg "),
                Element::node(
                    "span",
                    vec![("class".to_string(), "big".to_string())],
                    vec![Element::text("Hg")],
                ),
            ],
        );
        let mut font = florui_text::Font::load_embedded();
        let small_only = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
            "Hg Hg",
        );
        let big_alone = font.measure(
            florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 48.0, 400.0),
            "Hg",
        );

        let (arena, layouts) = layout_for(&tree, ".big { font-size: 48px; }");
        let p = arena.roots()[0];

        // Same sub-pixel tolerance as `assert_close`, rather than a strict
        // `>=` between two independently computed `f32`s.
        assert!(
            layouts[&p].height >= big_alone.height - 1.0,
            "a line containing a 48px run must be at least as tall as that run's own \
             single-line height (p height {}, 48px line height {})",
            layouts[&p].height,
            big_alone.height
        );
        assert!(
            layouts[&p].height > small_only.height,
            "must be taller than an all-16px line: p height {}, all-16px height {}",
            layouts[&p].height,
            small_only.height
        );
    }

    #[test]
    fn grid_template_columns_sizes_tracks_from_lengths_and_fr_units() {
        let tree: Element = view! {
            <div class="grid">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .grid { display: grid; width: 300px; height: 50px; \
             grid-template-columns: 100px 1fr; }
            .a { height: 50px; }
            .b { height: 50px; }
            ",
        );
        let grid = arena.roots()[0];
        let a = arena.children(grid)[0];
        let b = arena.children(grid)[1];

        assert_eq!(layouts[&a].width, 100.0, "the fixed 100px column");
        assert_eq!(
            layouts[&b].width, 200.0,
            "the 1fr column takes all remaining space: 300 - 100"
        );
        assert_eq!(layouts[&b].x, 100.0, "b starts right where a's column ends");
    }

    #[test]
    fn grid_template_rows_sizes_tracks_the_same_way() {
        let tree: Element = view! {
            <div class="grid">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .grid { display: grid; width: 50px; height: 300px; \
             grid-template-rows: 100px 1fr; grid-template-columns: 50px; }
            .a { width: 50px; }
            .b { width: 50px; }
            ",
        );
        let grid = arena.roots()[0];
        let a = arena.children(grid)[0];
        let b = arena.children(grid)[1];

        assert_eq!(layouts[&a].height, 100.0);
        assert_eq!(layouts[&b].height, 200.0, "300 - 100");
        assert_eq!(layouts[&b].y, 100.0);
    }

    #[test]
    fn grid_column_places_an_item_at_an_explicit_line() {
        let tree: Element = view! {
            <div class="grid">
                <div class="placed" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .grid { display: grid; width: 300px; height: 50px; \
             grid-template-columns: 100px 100px 100px; }
            .placed { grid-column: 3 / 4; height: 50px; }
            ",
        );
        let grid = arena.roots()[0];
        let placed = arena.children(grid)[0];

        assert_eq!(
            layouts[&placed].x, 200.0,
            "the 3rd column starts after the first two 100px columns"
        );
        assert_eq!(layouts[&placed].width, 100.0);
    }

    #[test]
    fn grid_row_span_places_an_item_across_multiple_rows() {
        let tree: Element = view! {
            <div class="grid">
                <div class="spans" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .grid { display: grid; width: 50px; height: 300px; \
             grid-template-rows: 100px 100px 100px; grid-template-columns: 50px; }
            .spans { grid-row: 1 / span 2; width: 50px; }
            ",
        );
        let grid = arena.roots()[0];
        let spans = arena.children(grid)[0];

        assert_eq!(
            layouts[&spans].height, 200.0,
            "spanning 2 of the 100px rows"
        );
    }

    #[test]
    fn gap_adds_space_between_grid_tracks() {
        let tree: Element = view! {
            <div class="grid">
                <div class="a" />
                <div class="b" />
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            "
            .grid { display: grid; width: 210px; height: 50px; \
             grid-template-columns: 100px 100px; column-gap: 10px; }
            .a { height: 50px; }
            .b { height: 50px; }
            ",
        );
        let grid = arena.roots()[0];
        let a = arena.children(grid)[0];
        let b = arena.children(grid)[1];

        assert_eq!(layouts[&a].x, 0.0);
        assert_eq!(layouts[&b].x, 110.0, "a's 100px column + 10px column-gap");
    }

    #[test]
    fn a_flex_item_too_narrow_for_its_own_text_reports_a_min_content_cause() {
        let tree: Element = view! {
            <div class="row">
                <span class="text">{"anunbreakablerunoftextwithoutspaces"}</span>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            "
            .row { display: flex; width: 20px; height: 20px; }
            .text { }
            ",
        );
        let row = arena.roots()[0];
        let text = arena.children(row)[0];
        let causes = compute_size_causes(&arena, &styles, &layouts);

        match causes.get(&text) {
            Some(SizeCause::MinContentClampedWidth { intrinsic_width }) => {
                assert_close(*intrinsic_width, layouts[&text].width);
                assert!(
                    *intrinsic_width > 20.0,
                    "the text's own natural width must be wider than the 20px row for this \
                     to be a real clamp, not a coincidence"
                );
            }
            other => panic!("expected a MinContentClampedWidth cause, got {other:?}"),
        }
    }

    #[test]
    fn a_percentage_width_and_height_resolve_against_the_containing_block() {
        let tree: Element = view! {
            <div class="bar"><div class="fill"></div></div>
        };
        let (arena, _styles, layouts) = layout_with_styles(
            &tree,
            ".bar { width: 200px; height: 8px; } .fill { width: 68%; height: 100%; }",
        );
        let bar = arena.roots()[0];
        let fill = arena.children(bar)[0];

        assert_close(layouts[&fill].width, 136.0);
        assert_close(layouts[&fill].height, 8.0);
    }

    #[test]
    fn a_flex_item_shrinks_to_its_longest_word_and_wraps_the_rest() {
        let tree: Element = view! {
            <div class="row">
                <div class="a">{"Recent"}</div>
                <div class="b">
                    <span>{"Bruno shared Brand assets with Design"}</span>
                </div>
            </div>
        };
        let (arena, _styles, layouts) = layout_with_styles(
            &tree,
            ".row { display: flex; width: 300px; gap: 10px; } \
             .a { flex-grow: 2; flex-basis: 0; } \
             .b { display: flex; flex-direction: column; flex-grow: 1; flex-basis: 0; }",
        );
        let row = arena.roots()[0];
        let a = arena.children(row)[0];
        let b = arena.children(row)[1];

        assert_close(layouts[&a].width, 290.0 * 2.0 / 3.0);
        assert_close(layouts[&b].width, 290.0 / 3.0);
        let text = arena.children(b)[0];
        assert!(
            layouts[&text].height > 40.0,
            "the text wraps to its narrow column instead of overflowing it: {}",
            layouts[&text].height
        );
    }

    #[test]
    fn flex_items_with_a_zero_basis_share_the_row_equally_whatever_their_text() {
        let tree: Element = view! {
            <div class="row">
                <div class="card"><span>{"+128 this week"}</span></div>
                <div class="card"><span>{"4 new"}</span></div>
                <div class="card"><span>{"auto-clears in 9d"}</span></div>
            </div>
        };
        let (arena, _styles, layouts) = layout_with_styles(
            &tree,
            ".row { display: flex; width: 300px; gap: 10px; } \
             .card { display: flex; flex-direction: column; flex-grow: 1; flex-basis: 0; }",
        );
        let row = arena.roots()[0];
        for card in arena.children(row) {
            assert_close(layouts[card].width, 280.0 / 3.0);
        }
    }

    #[test]
    fn a_flex_item_with_room_to_spare_reports_no_size_cause() {
        let tree: Element = view! {
            <div class="row">
                <span class="text">{"short"}</span>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            "
            .row { display: flex; width: 500px; height: 20px; }
            .text { }
            ",
        );
        let row = arena.roots()[0];
        let text = arena.children(row)[0];
        let causes = compute_size_causes(&arena, &styles, &layouts);

        assert!(
            !causes.contains_key(&text),
            "a row with plenty of room never needed to shrink anything"
        );
    }

    #[test]
    fn a_column_flex_item_with_room_to_spare_reports_no_size_cause() {
        let tree: Element = view! {
            <div class="col">
                <span class="text">{"short"}</span>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            "
            .col { display: flex; flex-direction: column; width: 100px; height: 500px; }
            .text { }
            ",
        );
        let col = arena.roots()[0];
        let text = arena.children(col)[0];
        let causes = compute_size_causes(&arena, &styles, &layouts);

        assert!(
            !causes.contains_key(&text),
            "a column with plenty of room never needed to shrink anything"
        );
    }

    #[test]
    fn a_column_flex_item_too_tall_for_its_own_wrapped_text_reports_a_min_content_cause() {
        let tree: Element = view! {
            <div class="col">
                <span class="text">{"one two three four five six seven eight nine ten"}</span>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            "
            .col { display: flex; flex-direction: column; width: 100px; height: 5px; }
            .text { }
            ",
        );
        let col = arena.roots()[0];
        let text = arena.children(col)[0];
        let causes = compute_size_causes(&arena, &styles, &layouts);

        match causes.get(&text) {
            Some(SizeCause::MinContentClampedHeight { intrinsic_height }) => {
                assert_close(*intrinsic_height, layouts[&text].height);
                assert!(
                    *intrinsic_height > 5.0,
                    "the text's own height at its committed 100px width must be taller than \
                     the 5px column for this to be a real clamp, not a coincidence"
                );
            }
            other => panic!("expected a MinContentClampedHeight cause, got {other:?}"),
        }
    }

    #[test]
    fn a_grid_item_too_wide_for_its_own_text_reports_a_track_cause() {
        let tree: Element = view! {
            <div class="grid">
                <span class="text">{"anunbreakablerunoftextwithoutspaces"}</span>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            "
            .grid { display: grid; grid-template-columns: 20px; height: 20px; }
            .text { }
            ",
        );
        let grid = arena.roots()[0];
        let text = arena.children(grid)[0];
        let causes = compute_size_causes(&arena, &styles, &layouts);

        match causes.get(&text) {
            Some(SizeCause::GridTrackNarrowerThanContent { intrinsic_width }) => {
                assert!(
                    *intrinsic_width > 20.0,
                    "the text's own natural width must be wider than the 20px track for this \
                     to be a real narrowing, not a coincidence"
                );
                assert!(
                    layouts[&text].width < *intrinsic_width,
                    "the committed width must actually be narrower than the natural content \
                     width for this to be a real narrowing"
                );
            }
            other => panic!("expected a GridTrackNarrowerThanContent cause, got {other:?}"),
        }
    }

    #[test]
    fn a_grid_item_with_room_to_spare_reports_no_size_cause() {
        let tree: Element = view! {
            <div class="grid">
                <span class="text">{"short"}</span>
            </div>
        };
        let (arena, styles, layouts) = layout_with_styles(
            &tree,
            "
            .grid { display: grid; grid-template-columns: 500px; height: 20px; }
            .text { }
            ",
        );
        let grid = arena.roots()[0];
        let text = arena.children(grid)[0];
        let causes = compute_size_causes(&arena, &styles, &layouts);

        assert!(
            !causes.contains_key(&text),
            "a track with plenty of room never narrowed its item below its own content"
        );
    }

    /// `build_node`/`hit_test` used to recurse once per tree level; both
    /// are iterative now. Taffy's own `compute_layout_with_measure` still
    /// recurses per depth internally (third-party code, not rewritten
    /// here) and would overflow the stack somewhere between 300 and 500 —
    /// `compute_layout`'s own `stacker::maybe_grow` wrapper covers that.
    /// 1,200 (past the original 1,000-deep crash report) rather than
    /// something far higher: Stylo's own cascade is quadratic-ish in
    /// depth for a single-chain tree (a separate, real, undiagnosed cost
    /// — see this crate's own top-level scope doc), so this test's own
    /// runtime, not a stack limit, is what bounds the depth chosen here.
    #[test]
    fn compute_layout_and_hit_test_survive_a_tree_far_deeper_than_the_old_recursion_limit() {
        let depth = 1_200;
        let mut tree = Element::node("div", vec![("class".into(), "leaf".into())], vec![]);
        for _ in 0..depth {
            tree = Element::node("div", vec![], vec![tree]);
        }
        let (arena, layouts) = layout_for(&tree, ".leaf { width: 10px; height: 10px; }");

        let leaf = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "leaf"))
            .unwrap();
        assert_close(layouts[&leaf].width, 10.0);

        let hit = hit_test(&arena, &layouts, 5.0, 5.0);
        assert_eq!(
            hit,
            Some(leaf),
            "the leaf sits at the origin at every depth"
        );
    }

    mod container_queries {
        use super::*;

        fn compute_with_style_for(
            tree: &Element,
            css: &str,
            available: Size<AvailableSpace>,
        ) -> (Arena, StyleMap, HashMap<NodeId, BoxLayout>) {
            let arena = Arena::build(tree);
            let rules = florui_style::parse_stylesheet(css).unwrap();
            let mut font = florui_text::Font::load_embedded();
            let mut timeline = florui_style::AnimationTimeline::default();
            let result = compute_with_style(
                &mut font,
                &arena,
                &rules,
                &InteractionState::new(),
                florui_style::Viewport::default(),
                &mut timeline,
                available,
            )
            .unwrap();
            (arena, result.styles, result.layouts)
        }

        /// The real, end-to-end path: an outer explicitly-narrow container
        /// and a nested wider one, both size containers — a descendant's
        /// `@container (min-width: ...)` must resolve against its own
        /// nearest real container's real laid-out width, not the outer
        /// one, and the resulting declaration must actually reach the
        /// element's painted geometry (a background color, verified via
        /// `ComputedStyle`, is enough to prove the declaration applied;
        /// layout itself doesn't carry color).
        #[test]
        fn nested_containers_resolve_against_real_laid_out_geometry() {
            let tree: Element = view! {
                <div class="outer">
                    <div class="inner">
                        <div class="card" />
                    </div>
                </div>
            };
            let css = "
                .outer { container-type: inline-size; width: 200px; }
                .inner { container-type: inline-size; width: 500px; }
                @container (min-width: 400px) { .card { background-color: #ff0000; } }
            ";
            let (arena, styles, _layouts) = compute_with_style_for(&tree, css, Size::MAX_CONTENT);
            let outer = arena.roots()[0];
            let inner = arena.children(outer)[0];
            let card = arena.children(inner)[0];

            assert_eq!(
                styles[&card].background_color,
                florui_style::Rgba::opaque(0xff, 0, 0),
                "the nearer 500px `.inner` container matches, even though the \
                 farther 200px `.outer` one alone would not"
            );
        }

        /// The same stylesheet, laid out at two different available widths
        /// for the single container involved — a real resize must flip
        /// which branch applies, driven only by `compute_with_style`'s own
        /// base layout pass, the same way every other layout-dependent
        /// value already invalidates on a fresh call.
        #[test]
        fn resizing_the_container_changes_which_declarations_apply() {
            let tree: Element = view! {
                <div class="box">
                    <div class="card" />
                </div>
            };
            let css = "
                .box { container-type: inline-size; width: 100%; }
                @container (min-width: 400px) { .card { background-color: #ff0000; } }
            ";

            let narrow = Size {
                width: AvailableSpace::Definite(300.0),
                height: AvailableSpace::Definite(100.0),
            };
            let (arena, styles, _) = compute_with_style_for(&tree, css, narrow);
            let card = arena.children(arena.roots()[0])[0];
            assert_eq!(
                styles[&card].background_color,
                florui_style::Rgba::TRANSPARENT
            );

            let wide = Size {
                width: AvailableSpace::Definite(600.0),
                height: AvailableSpace::Definite(100.0),
            };
            let (arena, styles, _) = compute_with_style_for(&tree, css, wide);
            let card = arena.children(arena.roots()[0])[0];
            assert_eq!(
                styles[&card].background_color,
                florui_style::Rgba::opaque(0xff, 0, 0)
            );
        }

        /// A gated `width` declaration only takes effect once the query
        /// matches — proving the final layout pass, not just the final
        /// *styles*, reflects the matched declarations, using an explicit
        /// pixel width to isolate that from a separate, pre-existing gap:
        /// `ComputedStyle::width` collapses any percentage to the same
        /// `None` as `auto` (`to_optional_length`'s own doc, in
        /// `crates/florui-style/src/stylo.rs`) — a real `width: 50%` and no
        /// `width` at all currently produce the identical (auto-fallback,
        /// full-stretch) layout either way, container query or not. Not
        /// this change's own scope to fix; verified only that container
        /// queries don't regress or change that existing behavior below.
        #[test]
        fn a_gated_width_only_applies_once_the_container_query_matches() {
            let tree: Element = view! {
                <div class="box">
                    <div class="card" />
                </div>
            };
            let css = "
                .box { container-type: inline-size; width: 500px; }
                @container (min-width: 400px) { .card { width: 250px; height: 10px; } }
            ";
            let root = Size {
                width: AvailableSpace::Definite(500.0),
                height: AvailableSpace::Definite(100.0),
            };
            let (arena, _styles, layouts) = compute_with_style_for(&tree, css, root);
            let card = arena.children(arena.roots()[0])[0];

            assert_eq!(
                layouts[&card].width, 250.0,
                "the gated 250px width only applies once `.box`'s real size matches"
            );
        }

        /// A percentage `width` declared inside a matched `@container` block
        /// resolves against the parent, the same as it would unconditionally.
        #[test]
        fn a_percentage_inside_a_matched_container_resolves_against_the_parent() {
            let tree: Element = view! {
                <div class="box">
                    <div class="card" />
                </div>
            };
            let css = "
                .box { container-type: inline-size; width: 500px; }
                @container (min-width: 400px) { .card { width: 50%; height: 10px; } }
            ";
            let root = Size {
                width: AvailableSpace::Definite(500.0),
                height: AvailableSpace::Definite(100.0),
            };
            let (arena, _styles, layouts) = compute_with_style_for(&tree, css, root);
            let card = arena.children(arena.roots()[0])[0];

            assert_eq!(layouts[&card].width, 250.0);
        }

        /// A stylesheet with zero `@container` blocks must take the cheap
        /// early-out path (`Rule::has_container_queries`) — same result a
        /// plain `compute` + `compute_layout` call already produced, at no
        /// extra cost.
        #[test]
        fn a_stylesheet_with_no_container_queries_behaves_exactly_like_before() {
            let tree: Element = view! { <div class="card" /> };
            let css = ".card { width: 123px; height: 45px; }";
            let (arena, _styles, layouts) = compute_with_style_for(&tree, css, Size::MAX_CONTENT);
            let node = arena.roots()[0];
            assert_eq!(layouts[&node].width, 123.0);
            assert_eq!(layouts[&node].height, 45.0);
        }
    }

    /// Measured in Edge: a default `<textarea>` is 168x36 (162x30 content),
    /// and `rows`/`cols` scale the content box by line height and advance.
    #[test]
    fn a_textarea_defaults_to_twenty_columns_by_two_rows_plus_a_scrollbar() {
        let tree: Element = view! {
            <div>
                <textarea id="a"></textarea>
                <textarea id="b" rows="4" cols="30"></textarea>
                <textarea id="c" style="width: 100px; height: 50px;"></textarea>
            </div>
        };
        let (arena, layouts) = layout_for(&tree, "");
        let size = |id: &str| {
            let node = arena.find(|a, n| a.id_attr(n) == Some(id)).unwrap();
            (layouts[&node].width, layouts[&node].height)
        };
        let (aw, ah) = size("a");
        let (bw, bh) = size("b");
        assert!(bw > aw && bh > ah, "more cols and rows make it bigger");
        // 1px border + 2px padding on each side is 6px of chrome.
        let (content_a, content_b) = (ah - 6.0, bh - 6.0);
        assert!(
            (content_b - 2.0 * content_a).abs() < 0.5,
            "four rows are twice the height of two"
        );
        let (cols_a, cols_b) = (aw - 6.0 - 15.0, bw - 6.0 - 15.0);
        assert!(
            (cols_b / cols_a - 30.0 / 20.0).abs() < 0.01,
            "columns scale the width left after the scrollbar"
        );
        assert_eq!(
            size("c"),
            (106.0, 56.0),
            "explicit CSS sizes the content box, as in a browser"
        );
    }

    #[test]
    fn display_none_takes_no_space_and_leaves_no_flex_gap() {
        let tree: Element = view! {
            <div class="row">
                <div class="box" id="a"></div>
                <div class="box gone" id="b"></div>
                <div class="box" id="c"></div>
            </div>
        };
        let (arena, layouts) = layout_for(
            &tree,
            ".row { display: flex; gap: 10px; } .box { width: 20px; height: 20px; } \
             .gone { display: none; }",
        );
        let node = |id: &str| arena.find(|a, n| a.id_attr(n) == Some(id)).unwrap();
        assert_eq!(layouts[&node("c")].x, 30.0, "one gap, not two");
        assert_eq!(layouts[&node("b")].width, 0.0);
        assert_eq!(layouts[&node("b")].height, 0.0);
    }

    #[test]
    fn a_leaf_is_measured_once_per_wrap_width_and_answers_repeats_from_memory() {
        let mut font = florui_text::Font::load_embedded();
        let family = florui_text::FontFamily::SansSerif;
        let text = "A label long enough to wrap at a narrow width";
        let mut scratch = LeafScratch::default();
        let mut measured = 0;
        let mut ask = |scratch: &mut LeafScratch, width: Option<f32>| {
            scratch.answer(width, |shaping| {
                measured += 1;
                font.measure_memoized(
                    shaping,
                    florui_text::TextStyle::new(family, 14.0, 400.0),
                    text,
                    width,
                )
            })
        };
        let unwrapped = ask(&mut scratch, None);
        let narrow = ask(&mut scratch, Some(60.0));
        // The eleven questions Taffy asks collapse onto these two widths.
        for _ in 0..9 {
            assert_eq!(ask(&mut scratch, None), unwrapped);
            assert_eq!(ask(&mut scratch, Some(60.0)), narrow);
        }
        assert_eq!(measured, 2);
        assert!(narrow.height > unwrapped.height, "the narrow width wraps");
    }

    #[test]
    fn a_leaf_asked_at_more_widths_than_it_keeps_still_measures_each_correctly() {
        let mut font = florui_text::Font::load_embedded();
        let family = florui_text::FontFamily::SansSerif;
        let text = "Several widths, more than a leaf keeps answers for";
        let widths = [
            None,
            Some(40.0),
            Some(80.0),
            Some(120.0),
            Some(160.0),
            Some(200.0),
        ];
        let mut scratch = LeafScratch::default();
        for round in 0..2 {
            for width in widths {
                let got = scratch.answer(width, |shaping| {
                    font.measure_memoized(
                        shaping,
                        florui_text::TextStyle::new(family, 14.0, 400.0),
                        text,
                        width,
                    )
                });
                let fresh = font.measure(florui_text::TextStyle::new(family, 14.0, 400.0), text);
                let expected = match width {
                    None => fresh,
                    Some(w) => font.measure_wrapped(
                        florui_text::TextStyle::new(family, 14.0, 400.0),
                        text,
                        w,
                    ),
                };
                assert_eq!(
                    (got.width, got.height, got.baseline),
                    (expected.width, expected.height, expected.baseline),
                    "round {round}, width {width:?}"
                );
            }
        }
    }
}
