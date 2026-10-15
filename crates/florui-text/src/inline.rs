//! Real inline formatting contexts: text mixed with nested inline elements,
//! atomic boxes and the horizontal padding, border and margin of an element.
//!
//! Parley lays the text out and breaks the lines. Everything about a line's
//! height and vertical position is done here, with CSS's rules, because
//! Parley's own line is as tall as its tallest run and takes a run's line
//! height from the first span merged into it.
//!
//! An element's horizontal padding, border and margin are carried by one
//! U+2007 FIGURE SPACE per side, given a font size that makes it exactly as
//! wide as needed: it never breaks a line (an inline box of that width would,
//! Parley treats it as a break opportunity), is not trailing whitespace and
//! is not stretched by `justify`. Which element a glyph run belongs to travels
//! in Parley's brush.

use std::ops::Range;
use std::sync::Arc;

use parley::{
    AlignmentOptions, FontFamily as ParleyFontFamily, FontWeight, InlineBox, InlineBoxKind,
    PositionedLayoutItem, StyleProperty,
};

use crate::{
    Font, FontFamily, InlineFragment, InlineLayout, LineHeight, PositionedBox, ShapedGlyph,
    ShapedRun, TextStyle, VerticalAlign,
};

/// The character standing in for horizontal padding, border and margin.
const SPACER: char = '\u{2007}';

/// An inline element of a context, or the container as element `0`. Each
/// element's text takes its `style`; `parent` is an earlier element (the
/// container's own is `0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineElement {
    pub parent: usize,
    pub style: TextStyle,
}

/// One piece of an inline context, in source order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InlineItem<'a> {
    Text {
        text: &'a str,
        element: usize,
    },
    /// The padding and border `inside` an element's box and the `margin`
    /// outside it, on its start side or, with `end`, its end side.
    Spacer {
        element: usize,
        inside: f32,
        margin: f32,
        end: bool,
    },
    /// An atomic box (`display: inline-block`). `baseline` is how far down its
    /// top its baseline is: its height for a box with no text of its own.
    Box {
        id: u64,
        width: f32,
        height: f32,
        baseline: f32,
        element: usize,
        vertical_align: VerticalAlign,
    },
}

/// A font's vertical metrics at one size, which line boxes are built from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StyleMetrics {
    ascent: f32,
    descent: f32,
    leading: f32,
    x_height: f32,
}

/// What an element contributes to a line: its box (how far it reaches above
/// and below its baseline, summing to its line height), the font's content
/// area, and what its children need to place themselves against it.
#[derive(Debug, Clone, Copy)]
struct ElementBox {
    above: f32,
    below: f32,
    content_ascent: f32,
    content_descent: f32,
    x_height: f32,
    font_size: f32,
    /// What a percentage `vertical-align` is a fraction of.
    line_height: f32,
    normal: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum SpanKind {
    Text,
    SpacerStart,
    SpacerEnd,
}

struct Span {
    range: Range<usize>,
    element: usize,
}

fn brush_of(element: usize, kind: SpanKind) -> [u8; 4] {
    let element = u16::try_from(element).unwrap_or(u16::MAX);
    let [low, high] = element.to_le_bytes();
    [low, high, kind as u8, 0]
}

fn element_of(brush: [u8; 4]) -> (usize, SpanKind) {
    let kind = match brush[2] {
        1 => SpanKind::SpacerStart,
        2 => SpanKind::SpacerEnd,
        _ => SpanKind::Text,
    };
    (usize::from(u16::from_le_bytes([brush[0], brush[1]])), kind)
}

/// Whitespace a line drops at its end. Not U+2007, which is a spacer.
fn is_collapsible(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Chrome keeps layout lengths in 1/64 pixel.
fn to_layout_unit(value: f32) -> f32 {
    (value * 64.0).floor() / 64.0
}

impl StyleMetrics {
    /// A `normal` line height as Chrome has it: ascent, descent and line gap
    /// rounded to whole pixels, then added.
    pub(crate) fn normal_line_height(&self) -> f32 {
        self.ascent.round() + self.descent.round() + self.leading.round()
    }
}

impl Font {
    /// [`Self::shape_inline_tree`] for a context with no nesting: every text
    /// item is its own element, a child of the container, whose style is
    /// `strut`.
    pub fn shape_inline(
        &mut self,
        items: &[crate::InlineContent<'_>],
        strut: TextStyle,
        max_width: Option<f32>,
    ) -> InlineLayout {
        let (elements, tree) = flatten(items, strut);
        self.shape_inline_tree(&elements, &tree, max_width)
    }

    /// [`Self::shape_inline`], remembered across calls.
    pub fn shape_inline_memoized(
        &mut self,
        items: &[crate::InlineContent<'_>],
        strut: TextStyle,
        max_width: Option<f32>,
    ) -> Arc<InlineLayout> {
        let (elements, tree) = flatten(items, strut);
        self.shape_inline_tree_memoized(&elements, &tree, max_width)
    }

    /// [`Self::shape_inline_tree`], remembered across calls: the same elements
    /// and items at the same wrap width answer from the memo without shaping or
    /// breaking lines, which is what layout asks for at several widths in one
    /// pass, again on every frame, and what paint asks for after it. The result
    /// is shared, not copied.
    pub fn shape_inline_tree_memoized(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        max_width: Option<f32>,
    ) -> Arc<InlineLayout> {
        if let Some(layout) = self.inline_memo.get(elements, items, max_width) {
            florui_profile::count(florui_profile::Counter::InlineMemoHits, 1);
            return layout;
        }
        florui_profile::count(florui_profile::Counter::InlineShapes, 1);
        #[cfg(test)]
        {
            self.inline_memo.misses += 1;
        }
        let layout = Arc::new(self.shape_inline_tree(elements, items, max_width));
        self.inline_memo
            .insert(elements, items, max_width, Arc::clone(&layout));
        layout
    }

    /// Lays out an inline formatting context: `items` in source order, each a
    /// run of text, a spacer or an atomic box, belonging to an element of the
    /// tree `elements` (element `0` is the container, whose style gives the
    /// paragraph its alignment and indent and every line its strut: as in CSS,
    /// each line is at least as tall as an empty line of the container).
    ///
    /// Each line is the union, around the shared baseline, of the box of every
    /// element that has something on it, moved by that element's
    /// `vertical-align`; `top` and `bottom` align the element, with what it
    /// contains, to the line's edge once the line's height is known.
    pub fn shape_inline_tree(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        max_width: Option<f32>,
    ) -> InlineLayout {
        let mut text = String::new();
        let mut spans: Vec<Span> = Vec::new();
        for item in items {
            match *item {
                InlineItem::Text { text: run, element } if !run.is_empty() => {
                    spans.push(Span {
                        range: text.len()..text.len() + run.len(),
                        element,
                    });
                    text.push_str(run);
                }
                InlineItem::Spacer {
                    element,
                    inside,
                    margin,
                    ..
                } if inside + margin > 0.0 => {
                    spans.push(Span {
                        range: text.len()..text.len() + SPACER.len_utf8(),
                        element,
                    });
                    text.push(SPACER);
                }
                _ => {}
            }
        }
        let has_box = items
            .iter()
            .any(|item| matches!(item, InlineItem::Box { .. }));
        if text.is_empty() && !has_box {
            return InlineLayout {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
                runs: Vec::new(),
                boxes: Vec::new(),
                fragments: Vec::new(),
            };
        }

        let family_names = [
            self.family_name(FontFamily::SansSerif).to_owned(),
            self.family_name(FontFamily::Monospace).to_owned(),
        ];
        let spacer_em = [
            self.spacer_em(FontFamily::SansSerif),
            self.spacer_em(FontFamily::Monospace),
        ];
        let root_style = elements[0].style;

        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, &text, 1.0, true);
        // Parley needs a base style to resolve every range against; every
        // range below sets its own size and family.
        builder.push_default(StyleProperty::FontSize(16.0));

        let mut offset = 0usize;
        for item in items {
            match *item {
                InlineItem::Text { text: run, element } => {
                    if run.is_empty() {
                        continue;
                    }
                    let end = offset + run.len();
                    let style = elements[element].style;
                    let family = family_names[style.family as usize].as_str();
                    builder.push(StyleProperty::FontSize(style.font_size), offset..end);
                    builder.push(
                        StyleProperty::FontWeight(FontWeight::new(style.font_weight)),
                        offset..end,
                    );
                    builder.push(
                        StyleProperty::FontFamily(ParleyFontFamily::named(family)),
                        offset..end,
                    );
                    for extra in style.parley_extras(false).into_iter().flatten() {
                        builder.push(extra, offset..end);
                    }
                    builder.push(
                        StyleProperty::Brush(brush_of(element, SpanKind::Text)),
                        offset..end,
                    );
                    offset = end;
                }
                InlineItem::Spacer {
                    element,
                    inside,
                    margin,
                    end,
                } => {
                    if inside + margin <= 0.0 {
                        continue;
                    }
                    let range = offset..offset + SPACER.len_utf8();
                    let spacer_end = range.end;
                    let style = elements[element].style;
                    let family = family_names[style.family as usize].as_str();
                    let width = (inside + margin) / spacer_em[style.family as usize];
                    builder.push(StyleProperty::FontSize(width), range.clone());
                    builder.push(
                        StyleProperty::FontWeight(FontWeight::new(400.0)),
                        range.clone(),
                    );
                    builder.push(
                        StyleProperty::FontFamily(ParleyFontFamily::named(family)),
                        range.clone(),
                    );
                    builder.push(StyleProperty::LetterSpacing(0.0), range.clone());
                    builder.push(StyleProperty::WordSpacing(0.0), range.clone());
                    builder.push(
                        StyleProperty::Brush(brush_of(
                            element,
                            if end {
                                SpanKind::SpacerEnd
                            } else {
                                SpanKind::SpacerStart
                            },
                        )),
                        range,
                    );
                    offset = spacer_end;
                }
                InlineItem::Box {
                    id, width, height, ..
                } => {
                    builder.push_inline_box(InlineBox {
                        id,
                        kind: InlineBoxKind::InFlow,
                        index: offset,
                        width,
                        height,
                    });
                }
            }
        }

        let mut layout: parley::Layout<[u8; 4]> = builder.build(&text);
        let (indent, options) = root_style.paragraph_indent(max_width);
        layout.set_text_indent(indent, options);
        layout.break_all_lines(max_width);
        layout.align(root_style.parley_alignment(), AlignmentOptions::default());

        let element_boxes: Vec<ElementBox> = elements
            .iter()
            .map(|element| self.element_box(element.style))
            .collect();

        let mut runs = Vec::new();
        let mut boxes = Vec::new();
        let mut fragments = Vec::new();
        let mut cursor = 0.0f32;
        let mut first_baseline = None;
        for line in layout.lines() {
            let placed = place_line(
                &line,
                &text,
                &spans,
                elements,
                &element_boxes,
                items,
                cursor,
            );
            for (glyph_run, run_element, shift_y) in placed.runs {
                let mut shaped = glyph_run;
                for glyph in &mut shaped.glyphs {
                    glyph.y += shift_y;
                }
                shaped.element = run_element;
                runs.push(shaped);
            }
            boxes.extend(placed.boxes);
            fragments.extend(placed.fragments);
            first_baseline.get_or_insert(placed.baseline);
            cursor += placed.height;
        }

        InlineLayout {
            width: layout.width(),
            height: cursor,
            baseline: first_baseline.unwrap_or(0.0),
            runs,
            boxes,
            fragments,
        }
    }

    /// Ascent, descent, line gap and x-height of a font at a size, from the
    /// metrics Parley reports for one glyph; remembered per family, size and
    /// weight.
    pub(crate) fn style_metrics(&mut self, style: TextStyle) -> StyleMetrics {
        let key = (
            style.family as u8,
            style.font_size.to_bits(),
            style.font_weight.to_bits(),
        );
        if let Some(metrics) = self.style_metrics.get(&key) {
            return *metrics;
        }
        let plain = TextStyle::new(style.family, style.font_size, style.font_weight);
        let mut layout = self.build_with(plain, "x", None);
        layout.break_all_lines(None);
        let mut metrics = StyleMetrics {
            ascent: 0.0,
            descent: 0.0,
            leading: 0.0,
            x_height: style.font_size * 0.5,
        };
        if let Some(line) = layout.lines().next() {
            let line_metrics = line.metrics();
            metrics.ascent = line_metrics.ascent;
            metrics.descent = line_metrics.descent;
            metrics.leading = line_metrics.leading;
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(glyph_run) = item {
                    if let Some(x_height) = glyph_run.run().metrics().x_height {
                        metrics.x_height = x_height;
                    }
                    break;
                }
            }
        }
        self.style_metrics.insert(key, metrics);
        metrics
    }

    /// How wide U+2007 is per em of font size in a family, at regular weight.
    fn spacer_em(&mut self, family: FontFamily) -> f32 {
        if let Some(em) = self.spacer_em[family as usize] {
            return em;
        }
        let mut width_of = |text: &str| {
            let mut layout = self.build_unbroken(TextStyle::new(family, 1000.0, 400.0), text);
            layout.break_all_lines(None);
            layout.width()
        };
        let with = format!("x{SPACER}x");
        let em = ((width_of(&with) - width_of("xx")) / 1000.0).max(0.01);
        self.spacer_em[family as usize] = Some(em);
        em
    }

    /// An element's box and the pieces its children place themselves against.
    /// The split is Parley's, which copies Chrome's: ascent and descent rounded
    /// to whole pixels, and the leading left over (negative for a short line
    /// height) divided with the smaller half above. A line of one style
    /// therefore comes out exactly as Parley's own.
    fn element_box(&mut self, style: TextStyle) -> ElementBox {
        let metrics = self.style_metrics(style);
        let line_height = match style.line_height {
            LineHeight::Normal => metrics.normal_line_height(),
            LineHeight::Number(n) => n * style.font_size,
            LineHeight::Length(px) => px,
        };
        let (ascent, descent) = (metrics.ascent.round(), metrics.descent.round());
        let above = ascent + ((line_height - (ascent + descent)) * 0.5).floor();
        ElementBox {
            above,
            below: line_height - above,
            content_ascent: ascent,
            content_descent: descent,
            x_height: metrics.x_height,
            font_size: style.font_size,
            line_height,
            normal: matches!(style.line_height, LineHeight::Normal),
        }
    }
}

/// The items of a context with no nesting as a tree: each text item an
/// element of its own under the container.
fn flatten<'a>(
    items: &[crate::InlineContent<'a>],
    strut: TextStyle,
) -> (Vec<InlineElement>, Vec<InlineItem<'a>>) {
    let mut elements = vec![InlineElement {
        parent: 0,
        style: strut,
    }];
    let mut tree = Vec::with_capacity(items.len());
    for item in items {
        match *item {
            crate::InlineContent::Text { text, style } => {
                elements.push(InlineElement { parent: 0, style });
                tree.push(InlineItem::Text {
                    text,
                    element: elements.len() - 1,
                });
            }
            crate::InlineContent::Box { id, width, height } => tree.push(InlineItem::Box {
                id,
                width,
                height,
                baseline: height,
                element: 0,
                vertical_align: VerticalAlign::Baseline,
            }),
        }
    }
    (elements, tree)
}

/// An element box widened to take in a font its text used: that font's own
/// box at its normal line height, split as [`Font::element_box`] splits it.
fn take_in_font(
    (above, below): (f32, f32),
    (ascent, descent, leading): (f32, f32, f32),
) -> (f32, f32) {
    let (ascent, descent) = (ascent.round(), descent.round());
    let natural = ascent + descent + leading.round();
    let used_above = ascent + ((natural - (ascent + descent)) * 0.5).floor();
    (above.max(used_above), below.max(natural - used_above))
}

struct PlacedLine {
    runs: Vec<(ShapedRun, usize, f32)>,
    boxes: Vec<PositionedBox>,
    fragments: Vec<InlineFragment>,
    baseline: f32,
    height: f32,
}

/// One participant of a line's vertical layout: an element, or an atomic box.
struct Participant {
    parent: usize,
    vertical_align: VerticalAlign,
    above: f32,
    below: f32,
    /// A box's own `line-height` for a percentage; an element's from its style.
    line_height: f32,
}

/// Builds one line: its height, the vertical shift of each glyph run and box,
/// and the fragment of each element on it. `cursor` is where the line starts.
fn place_line(
    line: &parley::Line<'_, [u8; 4]>,
    text: &str,
    spans: &[Span],
    elements: &[InlineElement],
    element_boxes: &[ElementBox],
    items: &[InlineItem<'_>],
    cursor: f32,
) -> PlacedLine {
    let count = elements.len();

    // What the line shows: trailing collapsible whitespace hangs.
    let range = line.text_range();
    let valid = range.start < range.end && range.end <= text.len();
    let visible_end = if valid {
        range.start + text[range.clone()].trim_end_matches(is_collapsible).len()
    } else {
        0
    };

    let mut present = vec![false; count];
    let mut has_text = false;
    for span in spans {
        if span.range.start < visible_end && span.range.end > range.start {
            present[span.element] = true;
            has_text = true;
        }
    }

    struct LineRun {
        shaped: ShapedRun,
        element: usize,
        kind: SpanKind,
        x: f32,
        advance: f32,
        ascent: f32,
        descent: f32,
        leading: f32,
    }
    let mut line_runs: Vec<LineRun> = Vec::new();
    let mut line_boxes: Vec<(PositionedBox, usize, f32, f32)> = Vec::new();
    for item in line.items() {
        match item {
            PositionedLayoutItem::GlyphRun(glyph_run) => {
                let (element, kind) = element_of(glyph_run.style().brush);
                let run = glyph_run.run();
                let glyphs = glyph_run
                    .positioned_glyphs()
                    .map(|glyph| ShapedGlyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    })
                    .collect();
                let metrics = run.metrics();
                line_runs.push(LineRun {
                    shaped: ShapedRun {
                        font: run.font().clone(),
                        font_size: run.font_size(),
                        normalized_coords: run.normalized_coords().to_vec(),
                        glyphs,
                        element,
                        x: glyph_run.offset(),
                        width: glyph_run.advance(),
                    },
                    element,
                    kind,
                    x: glyph_run.offset(),
                    advance: glyph_run.advance(),
                    ascent: metrics.ascent,
                    descent: metrics.descent,
                    leading: metrics.leading,
                });
            }
            PositionedLayoutItem::InlineBox(positioned) => {
                let found = items.iter().find_map(|item| match *item {
                    InlineItem::Box {
                        id,
                        element,
                        baseline,
                        vertical_align,
                        height,
                        ..
                    } if id == positioned.id => Some((element, baseline, vertical_align, height)),
                    _ => None,
                });
                if let Some((element, baseline, _, _)) = found {
                    present[element] = true;
                    line_boxes.push((
                        PositionedBox {
                            id: positioned.id,
                            x: positioned.x,
                            y: positioned.y,
                        },
                        element,
                        baseline,
                        positioned.height,
                    ));
                }
            }
        }
    }
    // A line of atomic boxes alone has no strut here: a box's baseline is its
    // own text's, which is not known, and the strut's depth would add a gap.
    present[0] = has_text || line_boxes.is_empty();
    for element in (1..count).rev() {
        if present[element] {
            present[elements[element].parent] = true;
        }
    }

    // Participants: the elements on the line, then its boxes.
    let mut participants: Vec<Option<Participant>> = (0..count).map(|_| None).collect();
    for element in 0..count {
        if !present[element] {
            continue;
        }
        let style = elements[element].style;
        let base = element_boxes[element];
        let (mut above, mut below) = (base.above, base.below);
        if base.normal {
            // With a normal line height the line also takes in every font the
            // element's text on it really used.
            for run in line_runs
                .iter()
                .filter(|r| r.element == element && r.kind == SpanKind::Text)
            {
                (above, below) =
                    take_in_font((above, below), (run.ascent, run.descent, run.leading));
            }
        }
        participants[element] = Some(Participant {
            parent: elements[element].parent,
            vertical_align: if element == 0 {
                VerticalAlign::Baseline
            } else {
                style.vertical_align
            },
            above,
            below,
            line_height: base.line_height,
        });
    }
    let box_start = count;
    for (index, (_, element, baseline, height)) in line_boxes.iter().enumerate() {
        let vertical_align = items
            .iter()
            .find_map(|item| match *item {
                InlineItem::Box {
                    id, vertical_align, ..
                } if id == line_boxes[index].0.id => Some(vertical_align),
                _ => None,
            })
            .unwrap_or_default();
        participants.push(Some(Participant {
            parent: *element,
            vertical_align,
            above: *baseline,
            below: height - baseline,
            line_height: *height,
        }));
    }

    // Each participant's raise over its parent's baseline, and the participant
    // that aligns it to the line's edge (itself, or an ancestor).
    let total = participants.len();
    let mut raise = vec![0.0f32; total];
    let mut anchor: Vec<Option<usize>> = vec![None; total];
    let order = (1..count).chain(box_start..total);
    for index in order {
        let Some(me) = &participants[index] else {
            continue;
        };
        let parent = me.parent;
        let parent_box = element_boxes[parent];
        let relative = match me.vertical_align {
            VerticalAlign::Top | VerticalAlign::Bottom => None,
            VerticalAlign::Baseline => Some(0.0),
            VerticalAlign::Sub => Some(-to_layout_unit(parent_box.font_size / 5.0 + 1.0)),
            VerticalAlign::Super => Some(to_layout_unit(parent_box.font_size / 3.0 + 1.0)),
            VerticalAlign::TextTop => Some(parent_box.content_ascent - me.above),
            VerticalAlign::TextBottom => Some(me.below - parent_box.content_descent),
            VerticalAlign::Middle => Some(parent_box.x_height / 2.0 - (me.above - me.below) / 2.0),
            VerticalAlign::Raise { length, percent } => Some(length + percent * me.line_height),
        };
        match relative {
            None => anchor[index] = Some(index),
            Some(value) => {
                raise[index] = raise[parent] + value;
                anchor[index] = anchor[parent];
            }
        }
    }

    // Extents: the baseline group's, and each aligned subtree's own.
    let (mut above, mut below) = (0.0f32, 0.0f32);
    let mut subtree: Vec<(f32, f32)> = vec![(0.0, 0.0); total];
    for index in 0..total {
        let Some(me) = &participants[index] else {
            continue;
        };
        let up = me.above + raise[index];
        let down = me.below - raise[index];
        match anchor[index] {
            None => {
                above = above.max(up);
                below = below.max(down);
            }
            Some(group) => {
                subtree[group].0 = subtree[group].0.max(up);
                subtree[group].1 = subtree[group].1.max(down);
            }
        }
    }
    let mut top_height = 0.0f32;
    let mut bottom_height = 0.0f32;
    for index in 0..total {
        if anchor[index] == Some(index) {
            let height = subtree[index].0 + subtree[index].1;
            if matches!(
                participants[index].as_ref().map(|p| p.vertical_align),
                Some(VerticalAlign::Top)
            ) {
                top_height = top_height.max(height);
            } else {
                bottom_height = bottom_height.max(height);
            }
        }
    }
    above = above.max(bottom_height - below);
    below = below.max(top_height - above);
    let height = above + below;
    let line_top = cursor.round();

    // Each participant's baseline, from the line's top.
    let mut baseline_of = vec![0.0f32; total];
    for index in 0..total {
        if participants[index].is_none() {
            continue;
        }
        baseline_of[index] = match anchor[index] {
            None => above - raise[index],
            Some(group) => {
                let group_baseline = if matches!(
                    participants[group].as_ref().map(|p| p.vertical_align),
                    Some(VerticalAlign::Top)
                ) {
                    subtree[group].0
                } else {
                    height - subtree[group].1
                };
                group_baseline - raise[index]
            }
        };
    }

    let baseline_for = |mut element: usize| {
        while participants[element].is_none() && element != 0 {
            element = elements[element].parent;
        }
        baseline_of[element]
    };

    // The runs, shifted from where Parley put them to their element's baseline.
    let parley_baseline = line.metrics().baseline;
    let mut runs = Vec::new();
    for run in line_runs.iter() {
        if run.kind != SpanKind::Text {
            continue;
        }
        let shift = line_top + baseline_for(run.element) - parley_baseline;
        runs.push((run.shaped.clone(), run.element, shift));
    }
    let mut boxes = Vec::new();
    for (index, (positioned, _, baseline, _)) in line_boxes.iter().enumerate() {
        let slot = box_start + index;
        boxes.push(PositionedBox {
            id: positioned.id,
            x: positioned.x,
            y: line_top + baseline_of[slot] - baseline,
        });
    }

    // Fragments: where each element's content, with the inside of its padding
    // and border, lies on this line.
    let mut low = vec![f32::INFINITY; count];
    let mut high = vec![f32::NEG_INFINITY; count];
    let mut grow = |mut element: usize, from: f32, to: f32| {
        loop {
            low[element] = low[element].min(from);
            high[element] = high[element].max(to);
            if element == 0 {
                break;
            }
            element = elements[element].parent;
        }
    };
    let trailing = line.metrics().trailing_whitespace;
    let line_end = line_runs
        .iter()
        .map(|run| run.x + run.advance)
        .fold(0.0f32, f32::max);
    for run in &line_runs {
        match run.kind {
            SpanKind::Text => {
                let end = run.x + run.advance;
                // The line's last run hangs its trailing whitespace outside.
                let end = if (end - line_end).abs() < 0.01 {
                    end - trailing
                } else {
                    end
                };
                grow(run.element, run.x, end);
            }
            SpanKind::SpacerStart | SpanKind::SpacerEnd => {
                let end = run.kind == SpanKind::SpacerEnd;
                let (inside, margin) = items
                    .iter()
                    .find_map(|item| match *item {
                        InlineItem::Spacer {
                            element,
                            inside,
                            margin,
                            end: item_end,
                        } if element == run.element && item_end == end => Some((inside, margin)),
                        _ => None,
                    })
                    .unwrap_or((run.advance, 0.0));
                if end {
                    grow(run.element, run.x, run.x + inside);
                } else {
                    grow(run.element, run.x + margin, run.x + run.advance);
                }
            }
        }
    }
    for (positioned, element, _, _) in &line_boxes {
        let width = items
            .iter()
            .find_map(|item| match *item {
                InlineItem::Box { id, width, .. } if id == positioned.id => Some(width),
                _ => None,
            })
            .unwrap_or(0.0);
        grow(*element, positioned.x, positioned.x + width);
    }

    // Where each element's text begins and ends, to tell which line holds its
    // start and which its end.
    let mut first = vec![usize::MAX; count];
    let mut last = vec![0usize; count];
    for span in spans {
        let mut element = span.element;
        loop {
            first[element] = first[element].min(span.range.start);
            last[element] = last[element].max(span.range.end);
            if element == 0 {
                break;
            }
            element = elements[element].parent;
        }
    }
    let mut fragments = Vec::new();
    for element in 1..count {
        if !present[element] || !low[element].is_finite() {
            continue;
        }
        let base = element_boxes[element];
        fragments.push(InlineFragment {
            element,
            x: low[element],
            y: line_top + baseline_of[element] - base.content_ascent,
            width: high[element] - low[element],
            ascent: base.content_ascent,
            descent: base.content_descent,
            starts: valid && range.start <= first[element] && first[element] < range.end,
            ends: valid && range.start < last[element] && last[element] <= range.end,
        });
    }

    PlacedLine {
        runs,
        boxes,
        fragments,
        baseline: line_top + above,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TextAlign, TextIndent};

    fn sans(size: f32) -> TextStyle {
        TextStyle::new(FontFamily::SansSerif, size, 400.0)
    }

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.05, "{a} is not {b}");
    }

    fn text(text: &str, element: usize) -> InlineItem<'_> {
        InlineItem::Text { text, element }
    }

    /// The height of the line and the content area's top of a span under
    /// `vertical-align`, in a 16px container, as Chrome measured them.
    fn vertical(line_height: LineHeight, size: f32, align: VerticalAlign) -> (f32, f32) {
        let elements = [
            InlineElement {
                parent: 0,
                style: sans(16.0).with_line_height(line_height),
            },
            InlineElement {
                parent: 0,
                style: sans(size)
                    .with_line_height(line_height)
                    .with_vertical_align(align),
            },
        ];
        let items = [text("ab ", 0), text("xy", 1), text(" cd", 0)];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(400.0));
        let fragment = layout
            .fragments
            .iter()
            .find(|f| f.element == 1)
            .expect("the span has a fragment");
        (layout.height, fragment.y)
    }

    #[test]
    fn vertical_align_places_a_span_where_chrome_does() {
        use VerticalAlign::*;
        let raise = |length: f32, percent: f32| Raise { length, percent };
        let thirty = LineHeight::Length(30.0);
        // (align, line height and span top at 12px, the same at 32px), all from
        // Chrome 155 for the same CSS.
        let table = [
            (Baseline, (32.0, 8.0), (36.0, -7.0)),
            (Sub, (36.188, 12.188), (31.813, -7.0)),
            (Super, (34.328, 6.0), (42.328, -7.0)),
            (Middle, (31.719, 7.719), (31.719, -5.281)),
            (TextTop, (34.0, 10.0), (34.0, -3.0)),
            (TextBottom, (34.0, 6.0), (34.0, -7.0)),
            (Top, (30.0, 6.0), (30.0, -7.0)),
            (Bottom, (30.0, 6.0), (30.0, -7.0)),
            (raise(5.0, 0.0), (33.0, 6.0), (41.0, -7.0)),
            (raise(-5.0, 0.0), (37.0, 13.0), (31.0, -7.0)),
            (raise(0.0, 0.5), (43.0, 6.0), (51.0, -7.0)),
            (raise(10.0, 0.0), (38.0, 6.0), (46.0, -7.0)),
        ];
        for (align, small, large) in table {
            let (height, top) = vertical(thirty, 12.0, align);
            assert!(
                (height - small.0).abs() < 0.05,
                "{align:?} 12px height {height}"
            );
            assert!((top - small.1).abs() < 0.05, "{align:?} 12px top {top}");
            let (height, top) = vertical(thirty, 32.0, align);
            assert!(
                (height - large.0).abs() < 0.05,
                "{align:?} 32px height {height}"
            );
            assert!((top - large.1).abs() < 0.05, "{align:?} 32px top {top}");
        }
    }

    #[test]
    fn vertical_align_with_a_normal_line_height_matches_chrome_too() {
        use VerticalAlign::*;
        let normal = LineHeight::Normal;
        for (align, small, large) in [
            (Sub, (25.188, 8.188), 43.0),
            (Super, (24.328, 0.0), 45.328),
            (Middle, (22.0, 4.219), 43.0),
            (TextTop, (22.0, 0.0), 43.0),
            (TextBottom, (22.0, 5.0), 43.0),
            (Top, (22.0, 0.0), 43.0),
            (Bottom, (22.0, 5.0), 43.0),
        ] {
            let (height, top) = vertical(normal, 12.0, align);
            assert!((height - small.0).abs() < 0.05, "{align:?} 12px: {height}");
            assert!((top - small.1).abs() < 0.05, "{align:?} 12px top {top}");
            let (height, _) = vertical(normal, 32.0, align);
            assert!((height - large).abs() < 0.05, "{align:?} 32px: {height}");
        }
    }

    #[test]
    fn a_box_aligned_to_the_middle_sits_on_the_baseline_plus_half_the_x_height() {
        // Chrome: an empty 30px inline-block with vertical-align: middle makes a
        // 30px line and the text 2.281px below the top.
        let elements = [InlineElement {
            parent: 0,
            style: sans(16.0),
        }];
        let items = [
            text("x ", 0),
            InlineItem::Box {
                id: 7,
                width: 30.0,
                height: 30.0,
                baseline: 30.0,
                element: 0,
                vertical_align: VerticalAlign::Middle,
            },
            text(" y", 0),
        ];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(300.0));
        close(layout.height, 30.0);
        let baseline = layout.runs[0].glyphs[0].y;
        close(baseline - 17.0, 2.281);
    }

    #[test]
    fn padding_border_and_margin_make_the_box_a_fragment_that_never_breaks_off_its_text() {
        // Chrome: "hello" then a span with `padding: 0 10px; border: 2px solid;
        // margin: 0 8px` around "world": the span's border box starts at 44.5
        // and is 66.391 wide, and nothing breaks even in 100px.
        let elements = [
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
        ];
        let spacer = |end| InlineItem::Spacer {
            element: 1,
            inside: 12.0,
            margin: 8.0,
            end,
        };
        let items = [
            text("hello", 0),
            spacer(false),
            text("world", 1),
            spacer(true),
        ];
        let mut font = Font::load_embedded();
        for width in [400.0, 100.0] {
            let layout = font.shape_inline_tree(&elements, &items, Some(width));
            assert_eq!(layout.fragments.len(), 1, "one line, width {width}");
            let fragment = layout.fragments[0];
            close(fragment.x, 44.5);
            close(fragment.width, 66.391);
            assert!(fragment.starts && fragment.ends);
            assert_eq!(layout.runs.len(), 2, "the spacers draw nothing");
            // One line, with no empty one after a spacer that ends the text.
            close(layout.height, 22.0);
        }
    }

    #[test]
    fn justifying_a_line_does_not_stretch_a_spacer() {
        let elements = [
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
        ];
        let spacer = |end| InlineItem::Spacer {
            element: 1,
            inside: 12.0,
            margin: 8.0,
            end,
        };
        let items = [
            text("aaa bbb ", 0),
            spacer(false),
            text("ccc", 1),
            spacer(true),
            text(" ddd eee fff ggg hhh iii", 0),
        ];
        let width_of_element = |align: TextAlign| {
            let mut elements = elements;
            elements[0].style = sans(16.0).with_align(align);
            let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(260.0));
            layout.fragments[0].width
        };
        close(
            width_of_element(TextAlign::Justify),
            width_of_element(TextAlign::Start),
        );
    }

    #[test]
    fn an_element_across_two_lines_has_a_fragment_on_each_and_flags_its_ends() {
        // Chrome: "aaa " then the span "bbb ccc ddd" with the same padding,
        // border and margin, then " eee", in 120px: 38.828/68.516 on the first
        // line and 0/41.375 on the second.
        let elements = [
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
        ];
        let spacer = |end| InlineItem::Spacer {
            element: 1,
            inside: 12.0,
            margin: 8.0,
            end,
        };
        let items = [
            text("aaa ", 0),
            spacer(false),
            text("bbb ccc ddd", 1),
            spacer(true),
            text(" eee", 0),
        ];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(120.0));
        assert_eq!(layout.fragments.len(), 2);
        let (first, second) = (layout.fragments[0], layout.fragments[1]);
        close(first.x, 38.828);
        close(first.width, 68.516);
        assert!(first.starts && !first.ends);
        close(second.x, 0.0);
        close(second.width, 41.375);
        assert!(!second.starts && second.ends);
        assert!(second.y > first.y);
    }

    #[test]
    fn a_nested_element_lies_on_its_parents_baseline() {
        // Chrome: an outer 24px span (content area 33) holding "b ", an inner
        // 12px span (17 tall, top 13) and " d", in a 16px container: line 33.
        let elements = [
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
            InlineElement {
                parent: 0,
                style: sans(24.0),
            },
            InlineElement {
                parent: 1,
                style: sans(12.0),
            },
        ];
        let items = [
            text("a ", 0),
            text("b ", 1),
            text("c", 2),
            text(" d", 1),
            text(" e", 0),
        ];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(300.0));
        close(layout.height, 33.0);
        let outer = layout.fragments.iter().find(|f| f.element == 1).unwrap();
        let inner = layout.fragments.iter().find(|f| f.element == 2).unwrap();
        close(outer.y, 0.0);
        close(outer.ascent + outer.descent, 33.0);
        close(inner.y, 13.0);
        close(inner.ascent + inner.descent, 17.0);
        assert!(inner.x > outer.x && inner.x + inner.width < outer.x + outer.width);
    }

    #[test]
    fn each_run_knows_its_element() {
        let elements = [
            InlineElement {
                parent: 0,
                style: sans(16.0),
            },
            InlineElement {
                parent: 0,
                style: sans(20.0),
            },
        ];
        let items = [text("one ", 0), text("two", 1), text(" three", 0)];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, None);
        let ids: Vec<usize> = layout.runs.iter().map(|r| r.element).collect();
        assert_eq!(ids, vec![0, 1, 0]);
    }

    #[test]
    fn alignment_and_indent_land_where_the_plain_path_puts_them() {
        const TEXT: &str = "The quick brown fox jumps over the lazy dog and keeps running";
        let mut font = Font::load_embedded();
        let indents = [
            TextIndent::default(),
            TextIndent {
                length: 30.0,
                ..TextIndent::default()
            },
            TextIndent {
                length: 20.0,
                percent: 0.1,
                hanging: true,
                each_line: false,
            },
        ];
        for align in [
            TextAlign::Start,
            TextAlign::End,
            TextAlign::Center,
            TextAlign::Justify,
        ] {
            for indent in indents {
                let style = sans(16.0).with_align(align).with_indent(indent);
                let plain = font.shape_wrapped(style, TEXT, 200.0);
                let inline = font.shape_inline(
                    &[crate::InlineContent::Text { text: TEXT, style }],
                    style,
                    Some(200.0),
                );
                let positions = |runs: &[ShapedRun]| -> Vec<(f32, f32)> {
                    runs.iter()
                        .flat_map(|run| run.glyphs.iter().map(|g| (g.x, g.y)))
                        .collect()
                };
                let (a, b) = (positions(&plain.runs), positions(&inline.runs));
                assert_eq!(a.len(), b.len());
                for (p, q) in a.iter().zip(&b) {
                    assert!(
                        (p.0 - q.0).abs() < 0.01 && (p.1 - q.1).abs() < 0.01,
                        "{align:?} {indent:?}: {p:?} against {q:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn center_and_end_move_a_line_and_indent_moves_the_first_line() {
        const TEXT: &str = "short line";
        let mut font = Font::load_embedded();
        let mut first_x = |style: TextStyle| {
            let layout = font.shape_wrapped(style, TEXT, 200.0);
            layout.runs[0].glyphs[0].x
        };
        let start = first_x(sans(16.0));
        assert!(first_x(sans(16.0).with_align(TextAlign::Center)) > start + 40.0);
        assert!(first_x(sans(16.0).with_align(TextAlign::End)) > start + 80.0);
        let indented = first_x(sans(16.0).with_indent(TextIndent {
            length: 30.0,
            ..TextIndent::default()
        }));
        close(indented, start + 30.0);
    }

    #[test]
    fn a_percentage_indent_is_of_the_width_the_text_is_wrapped_at() {
        const TEXT: &str = "short line";
        let mut font = Font::load_embedded();
        let style = sans(16.0).with_indent(TextIndent {
            percent: 0.25,
            ..TextIndent::default()
        });
        let mut at = |width: f32| font.shape_wrapped(style, TEXT, width).runs[0].glyphs[0].x;
        close(at(200.0), 50.0);
        close(at(400.0), 100.0);
    }

    #[test]
    fn a_line_widened_by_a_used_font_takes_that_fonts_box() {
        // A font whose box is taller than the element's own widens the line to it.
        let own = (17.0, 5.0);
        assert_eq!(take_in_font(own, (20.0, 6.0, 0.0)), (20.0, 6.0));
        // One that fits inside leaves it as it was.
        assert_eq!(take_in_font(own, (10.0, 3.0, 0.0)), own);
        // A fractional metric is rounded as the element's own font is.
        assert_eq!(take_in_font(own, (17.6, 4.7, 0.0)), (18.0, 5.0));
    }
}
