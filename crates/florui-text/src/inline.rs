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

use crate::{run_metrics_of, synthesis_of};
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
    /// The style the element's text has where it is on the first line of the container,
    /// when that is not `style`.
    pub first_line: Option<TextStyle>,
    /// Whether the element's own text runs right to left, which makes the right side of
    /// its box the one its start is on.
    pub rtl: bool,
}

impl InlineElement {
    /// An element with the same style on every line.
    pub fn new(parent: usize, style: TextStyle) -> Self {
        Self {
            parent,
            style,
            first_line: None,
            rtl: false,
        }
    }
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
    /// A float, which is no part of the line but is placed where the line reaches it:
    /// the caller says where it goes (see [`LineSource`]). Out of a context
    /// shaped without one it takes no room.
    Float {
        id: u64,
    },
}

/// The room of one line: where it starts and how wide it may be, in the
/// coordinates of the inline context. A line beside a float starts after it, or
/// ends before it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineSlot {
    pub x: f32,
    pub width: f32,
    /// How far the line is pushed down, below where the one before it ends: it
    /// had to wait for a float to end to have room for anything.
    pub drop: f32,
}

/// Where the floats of a context go, as the lines reach them. All `y` are from
/// the top of the context.
pub trait LineEnv {
    /// The room for a line box that starts at `y` and is `height` tall (what the
    /// line is expected to be: a taller one is asked again).
    fn slot(&mut self, y: f32, height: f32) -> LineSlot;
    /// Float `id` is reached on the line that starts at `y`, which holds `used`
    /// of its `slot`. Returns the new slot of the line if the float is put on
    /// it, or `None` if it does not fit beside what the line holds and goes on
    /// the line below ([`Self::float_below`]).
    fn float_here(&mut self, id: u64, y: f32, used: f32, slot: LineSlot) -> Option<LineSlot>;
    /// Float `id`, which did not fit on its line, goes at the top of the line
    /// that starts at `y`.
    fn float_below(&mut self, id: u64, y: f32);
    /// Where a line that starts at `y` and is `height` tall can go when what it
    /// holds does not fit in its slot: the bottom of the first float beside it
    /// to end, or `None` when no float is.
    fn clear_of_floats(&mut self, y: f32, height: f32) -> Option<f32>;
    /// How many floats have been placed, to be forgotten with [`Self::rewind`].
    fn mark(&mut self) -> usize;
    /// Forgets the floats placed after `mark`.
    fn rewind(&mut self, mark: usize);
}

/// Slack for lengths that should be equal.
const EPSILON: f32 = 0.01;

/// How many times lines are moved below the floats beside them in one layout.
const MOVES: usize = 64;

/// Where the room of each line comes from.
pub enum LineSource<'a> {
    /// The floats are placed as the lines reach them. `hints` are the heights
    /// the lines had in an earlier pass (a line is as tall as it turns out to
    /// be, which is only known once it is built).
    Env {
        env: &'a mut dyn LineEnv,
        hints: &'a [f32],
    },
    /// The slots a pass found, one per line (the last for any line after it),
    /// to lay the same lines out again where the floats are no longer known.
    Replay(&'a [LineSlot]),
}

/// What a layout in slots took: the slot of every line, and how tall each came
/// out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LineTrace {
    pub slots: Vec<LineSlot>,
    pub heights: Vec<f32>,
}

/// What goes before and after the text of the paragraph a container's style gives, given the
/// prepared text of its items.
fn paragraph_frame_of(
    container: &TextStyle,
    runs: &[crate::Prepared<'_>],
) -> (&'static str, &'static str) {
    let has_rtl = container.direction == crate::TextDirection::Ltr
        && runs
            .iter()
            .any(|run| run.text.chars().any(crate::is_rtl_char));
    crate::bidi::paragraph_frame(container.direction, container.bidi, has_rtl)
}

/// Which lines of a context a layout is, and where they start.
#[derive(Debug, Clone, Copy, Default)]
struct Part {
    /// How many lines the context has before them.
    line_base: usize,
    /// Where the first of them starts, from the top of the context.
    cursor: f32,
    /// The same for the floats, which are asked about the lines as tall as they were in an
    /// earlier pass.
    y: f32,
    /// How many of the lines to place; all of them when `None`.
    take: Option<usize>,
    /// They come after a first line that was laid out apart, so the indent of the first
    /// line is not theirs.
    after_first: bool,
    /// The element that is the container, whose style gives every line its strut.
    root: usize,
}

/// Where the first line of a layout ends.
struct FirstLineEnd {
    /// The end of its text.
    text_end: usize,
    /// Where the second line starts, for the floats.
    y: f32,
    /// How many floats were placed once the first line was done.
    floats: usize,
}

/// A layout in lines.
struct Laid {
    layout: InlineLayout,
    trace: LineTrace,
    /// How many lines the text has, of which the layout holds those it was asked for.
    lines: usize,
    first: Option<FirstLineEnd>,
}

/// What breaking the lines of a layout in slots gave.
struct Broken {
    slots: Vec<LineSlot>,
    /// Where the second line starts and how many floats were placed once the first was
    /// done.
    after_first: Option<(f32, usize)>,
}

/// Breaks the lines of `layout` one at a time, each in the slot `source` gives
/// it, and returns the slots. The layout is the lines from `base` on, which start
/// at `y0` from the top of the context.
fn break_in_slots(
    layout: &mut parley::Layout<[u8; 4]>,
    width: f32,
    strut_height: f32,
    source: &mut LineSource<'_>,
    shifts: &[usize],
    base: usize,
    y0: f32,
) -> Broken {
    let mut slots: Vec<LineSlot> = Vec::new();
    let mut after_first = None;
    let mut breaker = layout.break_lines();
    breaker.state_mut().set_layout_max_advance(width);
    // Where the line being built starts, and where the one before it ended.
    let mut y = y0;
    let mut previous_end = y0;
    let mut deferred: Vec<u64> = Vec::new();
    // The room of the line that starts at `y`, once it is moved down as many
    // times as `shifts` says, each to the end of the next float beside it.
    let start_line =
        |source: &mut LineSource<'_>, line: usize, y: &mut f32, previous_end: f32| match source {
            LineSource::Env { env, hints } => {
                let height = hints.get(base + line).copied().unwrap_or(strut_height);
                for _ in 0..shifts.get(line).copied().unwrap_or(0) {
                    match env.clear_of_floats(*y, height) {
                        Some(next) if next > *y + EPSILON => *y = next,
                        _ => break,
                    }
                }
                let mut slot = env.slot(*y, height);
                slot.drop = *y - previous_end;
                slot
            }
            LineSource::Replay(known) => known
                .get(base + line)
                .copied()
                .or_else(|| known.last().map(|last| LineSlot { drop: 0.0, ..*last }))
                .unwrap_or(LineSlot {
                    x: 0.0,
                    width,
                    drop: 0.0,
                }),
        };
    let apply = |breaker: &mut parley::BreakLines<'_, [u8; 4]>, slot: LineSlot| {
        let state = breaker.state_mut();
        state.set_line_x(slot.x);
        state.set_line_max_advance(slot.width.min(width));
    };
    let mut slot = start_line(source, 0, &mut y, previous_end);
    apply(&mut breaker, slot);
    loop {
        match breaker.break_next() {
            None => break,
            Some(parley::YieldData::LineBreak(data)) => {
                slots.push(slot);
                y += match source {
                    LineSource::Env { hints, .. } => hints
                        .get(base + slots.len() - 1)
                        .copied()
                        .unwrap_or(data.line_height),
                    LineSource::Replay(_) => data.line_height,
                };
                previous_end = y;
                if let LineSource::Env { env, .. } = source {
                    for id in deferred.drain(..) {
                        env.float_below(id, y);
                    }
                }
                if slots.len() == 1 {
                    let floats = match source {
                        LineSource::Env { env, .. } => env.mark(),
                        LineSource::Replay(_) => 0,
                    };
                    after_first = Some((y, floats));
                }
                slot = start_line(source, slots.len(), &mut y, previous_end);
                apply(&mut breaker, slot);
            }
            Some(parley::YieldData::InlineBoxBreak(reached)) => {
                if let LineSource::Env { env, .. } = source {
                    match env.float_here(reached.inline_box_id, y, reached.advance, slot) {
                        Some(narrower) => {
                            slot = LineSlot {
                                drop: slot.drop,
                                ..narrower
                            };
                            apply(&mut breaker, slot);
                        }
                        None => deferred.push(reached.inline_box_id),
                    }
                }
                breaker
                    .state_mut()
                    .append_inline_box_to_line(reached.advance, 0.0);
            }
            Some(parley::YieldData::MaxHeightExceeded(_)) => {}
        }
    }
    breaker.finish();
    Broken { slots, after_first }
}

/// The first line that holds more than the room a float left it, which a line
/// with room for nothing starts below the float instead.
fn overflowing_line(
    layout: &parley::Layout<[u8; 4]>,
    slots: &[LineSlot],
    width: f32,
) -> Option<usize> {
    layout.lines().zip(slots).position(|(line, slot)| {
        let metrics = line.metrics();
        slot.width + EPSILON < width
            && metrics.advance - metrics.trailing_whitespace > slot.width + EPSILON
    })
}

/// [`break_in_slots`], moving the lines that have no room for what they hold
/// down below the floats that narrow them. `source` is left as the last
/// pass found it.
fn break_lines_in_slots(
    layout: &mut parley::Layout<[u8; 4]>,
    width: f32,
    strut_height: f32,
    source: &mut LineSource<'_>,
    base: usize,
    y0: f32,
) -> Broken {
    let start = match source {
        LineSource::Env { env, .. } => env.mark(),
        LineSource::Replay(_) => {
            return break_in_slots(layout, width, strut_height, source, &[], base, y0);
        }
    };
    let mut shifts: Vec<usize> = Vec::new();
    let mut broken = break_in_slots(layout, width, strut_height, source, &shifts, base, y0);
    // Each pass moves one line, so there are as many as there are lines.
    for _ in 0..MOVES {
        let Some(line) = overflowing_line(layout, &broken.slots, width) else {
            break;
        };
        shifts.resize(shifts.len().max(line + 1), 0);
        shifts[line] += 1;
        if let LineSource::Env { env, .. } = source {
            env.rewind(start);
        }
        let again = break_in_slots(layout, width, strut_height, source, &shifts, base, y0);
        if again.slots == broken.slots {
            // No float is left to wait for: it overflows where it is.
            broken.after_first = again.after_first;
            break;
        }
        broken = again;
    }
    broken
}

/// A font's vertical metrics at one size, which line boxes are built from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StyleMetrics {
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
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
        self.shape_inline_core(elements, items, max_width, None).0
    }

    /// [`Self::shape_inline_tree`] with the room of each line given by `source`
    /// instead of one width: lines beside floats are shorter and start further
    /// in. `width` is the width of the whole context. Returns the layout, and
    /// what it took to lay it out: the slot of each line and the real height of
    /// each line, to give the next pass as hints.
    pub fn shape_inline_tree_in_slots(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        width: f32,
        source: &mut LineSource<'_>,
    ) -> (InlineLayout, LineTrace) {
        self.shape_inline_core(elements, items, Some(width), Some(source))
    }

    fn shape_inline_core(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        max_width: Option<f32>,
        source: Option<&mut LineSource<'_>>,
    ) -> (InlineLayout, LineTrace) {
        if elements.iter().any(|element| element.first_line.is_some()) {
            return self.shape_with_first_line(elements, items, max_width, source);
        }
        let runs = self.prepare_runs(elements, items, (true, false)).0;
        let laid = self.lay_out(elements, items, &runs, max_width, source, Part::default());
        (laid.layout, laid.trace)
    }

    /// The text of every item as it is laid out: each under its own element's white
    /// space, and a space collapses with the one before even across elements. `state`
    /// says whether the text before ends in a space and in a word; the state the text
    /// ends in is returned with it.
    fn prepare_runs<'i>(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'i>],
        state: (bool, bool),
    ) -> (Vec<crate::Prepared<'i>>, (bool, bool)) {
        let (mut previous_space, mut in_word) = state;
        let empty = || crate::Prepared {
            text: std::borrow::Cow::Borrowed(""),
            small: Vec::new(),
        };
        // Whether each element's font has small capitals of its own.
        let native_caps: Vec<bool> = elements
            .iter()
            .map(|element| element.style.caps.is_synthesized() && self.caps_in_font(&element.style))
            .collect();
        let runs = items
            .iter()
            .map(|item| match *item {
                InlineItem::Text { text, element } => elements[element].style.prepared(
                    text,
                    &mut previous_space,
                    &mut in_word,
                    native_caps[element],
                ),
                InlineItem::Box { .. } => {
                    previous_space = false;
                    in_word = false;
                    empty()
                }
                InlineItem::Spacer { .. } | InlineItem::Float { .. } => empty(),
            })
            .collect();
        (runs, (previous_space, in_word))
    }

    /// An inline formatting context whose container has a first line of a style of its
    /// own, which is laid out apart: the first line is the first line of the whole text
    /// in the style of the first line, and the lines after it are what is left of the text,
    /// in the style it has elsewhere, as Chrome lays them out. Every element is there
    /// twice, `n + element` being the element as it is on the first line.
    fn shape_with_first_line(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        max_width: Option<f32>,
        mut source: Option<&mut LineSource<'_>>,
    ) -> (InlineLayout, LineTrace) {
        let n = elements.len();
        let mut both = elements.to_vec();
        both.extend(elements.iter().map(|element| InlineElement {
            rtl: element.rtl,
            ..InlineElement::new(
                n + element.parent,
                element.first_line.unwrap_or(element.style),
            )
        }));
        let on_first_line: Vec<InlineItem<'_>> = items
            .iter()
            .map(|item| match *item {
                InlineItem::Text { text, element } => InlineItem::Text {
                    text,
                    element: n + element,
                },
                InlineItem::Spacer {
                    element,
                    inside,
                    margin,
                    end,
                } => InlineItem::Spacer {
                    element: n + element,
                    inside,
                    margin,
                    end,
                },
                InlineItem::Box {
                    id,
                    width,
                    height,
                    baseline,
                    element,
                    vertical_align,
                } => InlineItem::Box {
                    id,
                    width,
                    height,
                    baseline,
                    element: n + element,
                    vertical_align,
                },
                InlineItem::Float { id } => InlineItem::Float { id },
            })
            .collect();
        let (runs, _) = self.prepare_runs(&both, &on_first_line, (true, false));
        let mut first = self.lay_out(
            &both,
            &on_first_line,
            &runs,
            max_width,
            source.as_deref_mut(),
            Part {
                take: Some(1),
                root: n,
                ..Part::default()
            },
        );
        first.layout.first_line = Some(n);
        let Some((text_end, rest_y, floats)) = first
            .first
            .as_ref()
            .filter(|_| first.lines > 1)
            .map(|end| (end.text_end, end.y, end.floats))
        else {
            return (first.layout, first.trace);
        };
        // The lines after the first are laid out again, so what they placed beside floats goes.
        if let Some(LineSource::Env { env, .. }) = source.as_deref_mut() {
            env.rewind(floats);
        }

        // Where the first line ends in the items: the first item that is wholly after it,
        // or the one it ends in, of which `cut` bytes are on the first line.
        let on_line: Vec<u64> = first.layout.boxes.iter().map(|b| b.id).collect();
        let mut offset = paragraph_frame_of(&both[n].style, &runs).0.len();
        let mut split = None;
        for (index, (item, run)) in items.iter().zip(&runs).enumerate() {
            let after = offset >= text_end;
            match *item {
                InlineItem::Text { text, element } => {
                    let length = run.text.len();
                    if length > 0 && after {
                        split = Some((index, None));
                        break;
                    }
                    if offset + length > text_end {
                        let (_, state) =
                            self.prepare_runs(&both, &on_first_line[..index], (true, false));
                        let cut = self.source_len(
                            &both[n + element].style,
                            text,
                            text_end - offset,
                            state,
                        );
                        split = Some((index, Some(cut)));
                        break;
                    }
                    offset += length;
                }
                InlineItem::Spacer { inside, margin, .. } => {
                    if inside + margin > 0.0 {
                        if after {
                            split = Some((index, None));
                            break;
                        }
                        offset += SPACER.len_utf8();
                    }
                }
                InlineItem::Box { id, .. } | InlineItem::Float { id } => {
                    if after && !on_line.contains(&id) {
                        split = Some((index, None));
                        break;
                    }
                }
            }
        }
        let Some((split, cut)) = split else {
            return (first.layout, first.trace);
        };
        let mut rest: Vec<InlineItem<'_>> = Vec::with_capacity(items.len() - split);
        let (_, mut state) = self.prepare_runs(&both, &on_first_line[..split], (true, false));
        match (cut, items[split]) {
            (Some(cut), InlineItem::Text { text, element }) => {
                let style = both[n + element].style;
                let native = style.caps.is_synthesized() && self.caps_in_font(&style);
                let _ = style.prepared(&text[..cut], &mut state.0, &mut state.1, native);
                rest.push(InlineItem::Text {
                    text: &text[cut..],
                    element,
                });
                rest.extend_from_slice(&items[split + 1..]);
            }
            _ => rest.extend_from_slice(&items[split..]),
        }
        let (runs, _) = self.prepare_runs(&both, &rest, state);
        let second = self.lay_out(
            &both,
            &rest,
            &runs,
            max_width,
            source,
            Part {
                line_base: 1,
                cursor: first.layout.height,
                y: rest_y,
                take: None,
                after_first: true,
                root: 0,
            },
        );

        // An element cut by the end of the first line is no more at its end there than it
        // was at its start where the next lines go on with it.
        let reached = |items: &[InlineItem<'_>]| {
            let mut reached = vec![false; n];
            for item in items {
                let element = match *item {
                    InlineItem::Text { element, .. }
                    | InlineItem::Spacer { element, .. }
                    | InlineItem::Box { element, .. } => element,
                    InlineItem::Float { .. } => continue,
                };
                let mut at = element;
                loop {
                    reached[at] = true;
                    if at == 0 {
                        break;
                    }
                    at = elements[at].parent;
                }
            }
            reached
        };
        let in_rest = reached(&rest);
        let in_first = reached(&items[..split + usize::from(cut.is_some())]);
        let mut layout = first.layout;
        for fragment in &mut layout.fragments {
            if fragment.element >= n && in_rest[fragment.element - n] {
                fragment.ends = false;
            }
        }
        let after = second.layout;
        layout.runs.extend(after.runs);
        layout.boxes.extend(after.boxes);
        layout.lines.extend(after.lines);
        layout
            .fragments
            .extend(after.fragments.into_iter().map(|mut fragment| {
                if in_first[fragment.element] {
                    fragment.starts = false;
                }
                fragment
            }));
        layout.width = layout.width.max(after.width);
        if second.lines > 0 {
            layout.height = after.height;
            layout.last_baseline = after.last_baseline;
        }
        let mut trace = first.trace;
        trace.slots.truncate(1);
        trace.slots.extend(second.trace.slots);
        trace.heights.extend(second.trace.heights);
        (layout, trace)
    }

    /// How many bytes of `text`, written in `style` after text that left the state `state`
    /// (whether it ends in a space and in a word), make `length` bytes of prepared text.
    fn source_len(
        &mut self,
        style: &TextStyle,
        text: &str,
        length: usize,
        state: (bool, bool),
    ) -> usize {
        let native = style.caps.is_synthesized() && self.caps_in_font(style);
        let boundaries: Vec<usize> = text
            .char_indices()
            .map(|(at, _)| at)
            .chain(std::iter::once(text.len()))
            .collect();
        // The prepared text of a prefix is a prefix of the prepared text, so the first
        // prefix that is long enough is the one.
        let at = boundaries.partition_point(|&end| {
            let (mut space, mut word) = state;
            style
                .prepared(&text[..end], &mut space, &mut word, native)
                .text
                .len()
                < length
        });
        boundaries.get(at).copied().unwrap_or(text.len())
    }

    /// Lays out `items`, which belong to `elements`, in lines: the whole of a context, or
    /// as much of it as `part` says.
    fn lay_out(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        runs: &[crate::Prepared<'_>],
        max_width: Option<f32>,
        source: Option<&mut LineSource<'_>>,
        part: Part,
    ) -> Laid {
        let mut trace = LineTrace::default();
        // The paragraph takes the direction of its container, not of its first strong
        // character.
        let (before, after) = paragraph_frame_of(&elements[part.root].style, runs);
        let mut text = String::from(before);
        let mut spans: Vec<Span> = Vec::new();
        // The small capitals of each run, where they are in `text`.
        let mut small_caps: Vec<(Range<usize>, usize)> = Vec::new();
        for (item, run) in items.iter().zip(runs) {
            let run_text = run.text.as_ref();
            match *item {
                InlineItem::Text { element, .. } if !run_text.is_empty() => {
                    spans.push(Span {
                        range: text.len()..text.len() + run_text.len(),
                        element,
                    });
                    for range in &run.small {
                        small_caps
                            .push((text.len() + range.start..text.len() + range.end, element));
                    }
                    text.push_str(run_text);
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
        text.push_str(after);
        let has_box = items
            .iter()
            .any(|item| matches!(item, InlineItem::Box { .. }));
        if text.len() == before.len() + after.len() && !has_box {
            return Laid {
                layout: InlineLayout {
                    width: 0.0,
                    height: part.cursor,
                    baseline: 0.0,
                    last_baseline: 0.0,
                    runs: Vec::new(),
                    boxes: Vec::new(),
                    lines: Vec::new(),
                    fragments: Vec::new(),
                    first_line: None,
                },
                trace,
                lines: 0,
                first: None,
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
        let root_style = elements[part.root].style;
        // The room each upright character of a vertical line takes, by item.
        let cells: Vec<Vec<(Range<usize>, f32)>> = items
            .iter()
            .zip(runs)
            .map(|(item, run)| match *item {
                InlineItem::Text { element, .. }
                    if elements[element].style.orientation.may_stand() =>
                {
                    self.upright_cells(elements[element].style, run.text.as_ref())
                }
                _ => Vec::new(),
            })
            .collect();

        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, &text, 1.0, true);
        // Parley needs a base style to resolve every range against; every
        // range below sets its own size and family.
        builder.push_default(StyleProperty::FontSize(16.0));

        let mut offset = before.len();
        for (index, (item, run)) in items.iter().zip(runs).enumerate() {
            let run = run.text.as_ref();
            match *item {
                InlineItem::Text { element, .. } => {
                    if run.is_empty() {
                        continue;
                    }
                    let end = offset + run.len();
                    let style = elements[element].style;
                    let family = family_names[style.family.base()].as_str();
                    builder.push(StyleProperty::FontSize(style.font_size), offset..end);
                    builder.push(
                        StyleProperty::FontWeight(FontWeight::new(style.font_weight)),
                        offset..end,
                    );
                    builder.push(
                        StyleProperty::FontFamily(style.parley_family(family)),
                        offset..end,
                    );
                    for extra in style.parley_extras(false).into_iter().flatten() {
                        builder.push(extra, offset..end);
                    }
                    builder.push(
                        StyleProperty::Brush(brush_of(element, SpanKind::Text)),
                        offset..end,
                    );
                    let features = if cells[index].is_empty() {
                        String::new()
                    } else {
                        style.upright_features()
                    };
                    for (range, spacing) in &cells[index] {
                        let range = offset + range.start..offset + range.end;
                        builder.push(
                            StyleProperty::FontFeatures(parley::FontFeatures::Source(
                                features.clone().into(),
                            )),
                            range.clone(),
                        );
                        builder.push(StyleProperty::LetterSpacing(*spacing), range.clone());
                        let mut brush = brush_of(element, SpanKind::Text);
                        brush[3] = 1;
                        builder.push(StyleProperty::Brush(brush), range);
                    }
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
                    let family = family_names[style.family.base()].as_str();
                    let width = (inside + margin) / spacer_em[style.family.base()];
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
                InlineItem::Float { id } => {
                    builder.push_inline_box(InlineBox {
                        id,
                        kind: if source.is_some() {
                            InlineBoxKind::CustomOutOfFlow
                        } else {
                            InlineBoxKind::OutOfFlow
                        },
                        index: offset,
                        width: 0.0,
                        height: 0.0,
                    });
                }
            }
        }

        // Last, so they win over the size their run was given.
        for (range, element) in &small_caps {
            let style = elements[*element].style;
            builder.push(
                StyleProperty::FontSize(style.font_size * style.caps.scale()),
                range.clone(),
            );
        }

        let mut layout: parley::Layout<[u8; 4]> = builder.build(&text);
        let (indent, options) = root_style.paragraph_indent(max_width);
        // The lines after the first have no first line's indent.
        let indent = if part.after_first && !options.each_line {
            0.0
        } else {
            indent
        };
        layout.set_text_indent(indent, options);
        let mut after_first = None;
        match source {
            Some(source) => {
                let strut = self.element_box(root_style);
                let broken = break_lines_in_slots(
                    &mut layout,
                    max_width.unwrap_or(f32::MAX),
                    strut.above + strut.below,
                    source,
                    part.line_base,
                    part.y,
                );
                trace.slots = broken.slots;
                after_first = broken.after_first;
            }
            None => layout.break_all_lines(max_width),
        }
        layout.align(root_style.parley_alignment(), AlignmentOptions::default());

        let element_boxes: Vec<ElementBox> = elements
            .iter()
            .map(|element| self.element_box(element.style))
            .collect();

        let mut runs = Vec::new();
        let mut boxes = Vec::new();
        let mut lines = Vec::new();
        let mut fragments = Vec::new();
        let mut cursor = part.cursor;
        let mut first_baseline = None;
        let mut last_baseline = 0.0;
        let mut widest = 0.0f32;
        // A line break at the very end ends the line it is in, and no line comes after it.
        let ends_in_break = text.ends_with('\n')
            && layout
                .lines()
                .last()
                .is_some_and(|line| line.text_range().is_empty());
        let line_count =
            layout.lines().count() - usize::from(ends_in_break && layout.lines().count() > 1);
        let taken = part.take.map_or(line_count, |lines| lines.min(line_count));
        for (line_number, line) in layout.lines().enumerate().take(taken) {
            cursor += trace.slots.get(line_number).map_or(0.0, |slot| slot.drop);
            let placed = place_line(
                &line,
                &text,
                &spans,
                elements,
                &element_boxes,
                items,
                (cursor, part.root),
            );
            let number = (part.line_base + line_number) as u32;
            for (glyph_run, run_element, shift_y) in placed.runs {
                let mut shaped = glyph_run;
                for glyph in &mut shaped.glyphs {
                    glyph.y += shift_y;
                }
                shaped.element = run_element;
                shaped.line = number;
                runs.push(shaped);
            }
            boxes.extend(placed.boxes);
            fragments.extend(placed.fragments.into_iter().map(|mut fragment| {
                fragment.line = number;
                fragment
            }));
            first_baseline.get_or_insert(placed.baseline);
            last_baseline = placed.baseline;
            let metrics = line.metrics();
            widest = widest.max(metrics.advance - metrics.trailing_whitespace);
            trace.heights.push(placed.height);
            lines.push(crate::LineBox {
                top: cursor,
                height: placed.height,
                hanging: metrics.trailing_whitespace,
            });
            cursor += placed.height;
        }
        let first = layout.lines().next().map(|line| {
            let (y, floats) = after_first.unwrap_or((0.0, 0));
            FirstLineEnd {
                text_end: line.text_range().end,
                y,
                floats,
            }
        });

        Laid {
            layout: InlineLayout {
                width: if part.take.is_some() {
                    widest
                } else {
                    layout.width()
                },
                height: cursor,
                baseline: first_baseline.unwrap_or(0.0),
                last_baseline,
                runs,
                boxes,
                lines,
                fragments,
                first_line: None,
            },
            trace,
            lines: line_count,
            first,
        }
    }

    /// Ascent, descent, line gap and x-height of a font at a size, from the
    /// metrics Parley reports for one glyph; remembered per family, size and
    /// weight.
    pub(crate) fn style_metrics(&mut self, style: TextStyle) -> StyleMetrics {
        let key = (
            style.family.bits(),
            style.font_size.to_bits(),
            style.font_weight.to_bits(),
            style.font_style.bits().0,
        );
        if let Some(metrics) = self.style_metrics.get(&key) {
            return *metrics;
        }
        let plain = TextStyle::new(style.family, style.font_size, style.font_weight)
            .with_font_style(style.font_style);
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
        if let Some(em) = self.spacer_em[family.base()] {
            return em;
        }
        let mut width_of = |text: &str| {
            let mut layout = self.build_unbroken(TextStyle::new(family, 1000.0, 400.0), text);
            layout.break_all_lines(None);
            layout.width()
        };
        let with = format!("x{SPACER}x");
        let em = ((width_of(&with) - width_of("xx")) / 1000.0).max(0.01);
        self.spacer_em[family.base()] = Some(em);
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
    let mut elements = vec![InlineElement::new(0, strut)];
    let mut tree = Vec::with_capacity(items.len());
    for item in items {
        match *item {
            crate::InlineContent::Text { text, style } => {
                elements.push(InlineElement::new(0, style));
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
    (cursor, root): (f32, usize),
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
    for span in spans {
        if span.range.start < visible_end && span.range.end > range.start {
            present[span.element] = true;
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
                let no_synthesis = elements[element].style.no_synthesis;
                let run = glyph_run.run();
                let glyphs = glyph_run
                    .positioned_glyphs()
                    .map(|glyph| ShapedGlyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                        advance: glyph.advance,
                    })
                    .collect();
                let metrics = run.metrics();
                let synthesis = synthesis_of(run, no_synthesis);
                line_runs.push(LineRun {
                    shaped: ShapedRun {
                        upright: glyph_run.style().brush[3] == 1,
                        font: run.font().clone(),
                        font_size: run.font_size(),
                        normalized_coords: run.normalized_coords().to_vec(),
                        skew: synthesis.0,
                        embolden: synthesis.1,
                        metrics: run_metrics_of(run),
                        glyphs,
                        element,
                        x: glyph_run.offset(),
                        width: glyph_run.advance(),
                        line: 0,
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
    // The white space that ends a line hangs outside it, and in a right to left paragraph
    // that is the left side. A line of one run that the shaper left it at the right end of
    // is moved over by its width, which puts the text where the paragraph's edge is.
    let hanging = line.metrics().trailing_whitespace;
    if hanging > 0.01 && elements[root].style.direction == crate::TextDirection::Rtl {
        let mut text_runs = line_runs
            .iter_mut()
            .filter(|run| run.kind == SpanKind::Text && run.advance > 0.0);
        if let (Some(only), None) = (text_runs.next(), text_runs.next())
            && let Some(last) = only.shaped.glyphs.last()
            && (only.x + only.advance - last.x - hanging).abs() < 0.1
        {
            only.x += hanging;
            only.shaped.x += hanging;
            for glyph in &mut only.shaped.glyphs {
                glyph.x += hanging;
            }
        }
    }
    // The strut is on every line, a line of atomic boxes alone included: the
    // boxes sit on its baseline and its depth shows below them.
    present[root] = true;
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
            vertical_align: if element == root {
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
        while participants[element].is_none() && element != root {
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
            if element == root {
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
            if element == root {
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
            line: 0,
            x: low[element],
            y: line_top + baseline_of[element] - base.content_ascent,
            width: high[element] - low[element],
            ascent: base.content_ascent,
            descent: base.content_descent,
            starts: valid && range.start <= first[element] && first[element] < range.end,
            ends: valid && range.start < last[element] && last[element] <= range.end,
            rtl: elements[element].rtl,
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
    use crate::{BidiMode, TextAlign, TextDirection, TextIndent};

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
            InlineElement::new(0, sans(16.0).with_line_height(line_height)),
            InlineElement::new(
                0,
                sans(size)
                    .with_line_height(line_height)
                    .with_vertical_align(align),
            ),
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
        let elements = [InlineElement::new(0, sans(16.0))];
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
            InlineElement::new(0, sans(16.0)),
            InlineElement::new(0, sans(16.0)),
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
            InlineElement::new(0, sans(16.0)),
            InlineElement::new(0, sans(16.0)),
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
            InlineElement::new(0, sans(16.0)),
            InlineElement::new(0, sans(16.0)),
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
            InlineElement::new(0, sans(16.0)),
            InlineElement::new(0, sans(24.0)),
            InlineElement::new(1, sans(12.0)),
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
            InlineElement::new(0, sans(16.0)),
            InlineElement::new(0, sans(20.0)),
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

    /// A float of the given margin box on one side of a 200 wide context.
    struct Beside {
        left: bool,
        width: f32,
        height: f32,
        placed: Vec<(u64, f32)>,
    }

    impl Beside {
        fn edges(&self, y: f32, height: f32) -> (f32, f32) {
            if y < self.height && y + height > 0.0 {
                if self.left {
                    (self.width, 200.0)
                } else {
                    (0.0, 200.0 - self.width)
                }
            } else {
                (0.0, 200.0)
            }
        }
    }

    impl LineEnv for Beside {
        fn slot(&mut self, y: f32, height: f32) -> LineSlot {
            let (from, to) = self.edges(y, height);
            LineSlot {
                x: from,
                width: to - from,
                drop: 0.0,
            }
        }

        fn float_here(&mut self, id: u64, y: f32, used: f32, slot: LineSlot) -> Option<LineSlot> {
            if used + self.width > slot.width {
                return None;
            }
            self.placed.push((id, y));
            let (from, to) = self.edges(y, 1.0);
            Some(LineSlot {
                x: from,
                width: to - from,
                drop: 0.0,
            })
        }

        fn float_below(&mut self, id: u64, y: f32) {
            self.placed.push((id, y));
        }

        fn clear_of_floats(&mut self, y: f32, height: f32) -> Option<f32> {
            (y < self.height && y + height > 0.0).then_some(self.height)
        }

        fn mark(&mut self) -> usize {
            self.placed.len()
        }

        fn rewind(&mut self, mark: usize) {
            self.placed.truncate(mark);
        }
    }

    /// How far each line of a layout reaches: from where its first glyph run
    /// starts to where its last ends.
    fn line_extents(layout: &InlineLayout) -> Vec<(f32, f32)> {
        let lines = layout
            .runs
            .iter()
            .map(|run| run.line)
            .max()
            .map_or(0, |last| last + 1);
        (0..lines)
            .map(|line| {
                let runs = layout.runs.iter().filter(|run| run.line == line);
                let from = runs.clone().map(|run| run.x).fold(f32::MAX, f32::min);
                let to = runs.map(|run| run.x + run.width).fold(0.0, f32::max);
                (from, to - from)
            })
            .collect()
    }

    const FOX: &str =
        "The quick brown fox jumps over the lazy dog and keeps running along the road home again";

    fn shaped_beside(env: &mut Beside, style: TextStyle) -> (InlineLayout, LineTrace) {
        shaped_text_beside(env, style, FOX)
    }

    fn shaped_text_beside(
        env: &mut Beside,
        style: TextStyle,
        text: &str,
    ) -> (InlineLayout, LineTrace) {
        let style = style.with_line_height(LineHeight::Length(20.0));
        let elements = [InlineElement::new(0, style)];
        let items = [InlineItem::Text { text, element: 0 }];
        let mut hints: Vec<f32> = Vec::new();
        let mut result = None;
        for _ in 0..3 {
            env.placed.clear();
            let mut source = LineSource::Env {
                env: &mut *env,
                hints: &hints,
            };
            let (layout, trace) = Font::load_embedded().shape_inline_tree_in_slots(
                &elements,
                &items,
                200.0,
                &mut source,
            );
            let settled = trace.heights == hints;
            hints = trace.heights.clone();
            result = Some((layout, trace));
            if settled {
                break;
            }
        }
        result.expect("at least one pass")
    }

    #[test]
    fn lines_beside_a_left_float_start_after_it_and_those_below_start_at_the_edge() {
        // Chrome 153: a 50x50 float at the left of 200px of 16px text with a 20px
        // line height: three lines start at x = 50, the rest at 0.
        let mut env = Beside {
            left: true,
            width: 50.0,
            height: 50.0,
            placed: Vec::new(),
        };
        let (layout, trace) = shaped_beside(&mut env, sans(16.0));
        let lines = line_extents(&layout);
        let expected = [
            (50.0, 123.8),
            (50.0, 139.58),
            (50.0, 141.3),
            (0.0, 171.39),
            (0.0, 87.72),
        ];
        assert_eq!(lines.len(), expected.len(), "{lines:?}");
        for (line, (from, width)) in lines.iter().zip(expected) {
            // A trailing space belongs to the line's end.
            assert!((line.0 - from).abs() < 0.6, "{lines:?}");
            assert!((line.1 - width).abs() < 5.0, "{lines:?}");
        }
        assert_eq!(trace.slots.len(), 5);
        close(trace.slots[0].x, 50.0);
        close(trace.slots[3].x, 0.0);
        close(trace.slots[3].width, 200.0);
    }

    #[test]
    fn a_line_with_no_room_for_a_word_beside_a_float_starts_below_the_float() {
        // Chrome 153: a 150x30 float at the left of 200px of 16px text with a 20px
        // line height: "Short" fits beside it, "stackedwords" does not, so the next
        // line starts where the float ends (y = 30, not 20) and the three lines
        // are 70 tall.
        let mut env = Beside {
            left: true,
            width: 150.0,
            height: 30.0,
            placed: Vec::new(),
        };
        let text = "Short stackedwords floats of different heights.";
        let (layout, trace) = shaped_text_beside(&mut env, sans(16.0), text);
        assert_eq!(trace.slots.len(), 3, "{trace:?}");
        close(trace.slots[0].x, 150.0);
        close(trace.slots[1].x, 0.0);
        close(trace.slots[1].drop, 10.0);
        close(trace.slots[2].drop, 0.0);
        close(layout.height, 70.0);
    }

    #[test]
    fn a_line_beside_a_right_float_is_shorter_and_stays_at_the_left_edge() {
        let mut env = Beside {
            left: false,
            width: 70.0,
            height: 30.0,
            placed: Vec::new(),
        };
        let (layout, trace) = shaped_beside(&mut env, sans(16.0));
        let lines = line_extents(&layout);
        // Chrome 153: the first two lines end before the float (130 wide room).
        assert!(
            lines[0].1 <= 130.0 + 0.5 && lines[1].1 <= 130.0 + 0.5,
            "{lines:?}"
        );
        close(lines[0].0, 0.0);
        close(trace.slots[0].width, 130.0);
        close(trace.slots[2].width, 200.0);
    }

    #[test]
    fn a_float_item_is_put_at_the_top_of_the_line_it_is_reached_on_when_it_fits() {
        let elements = [InlineElement::new(
            0,
            sans(16.0).with_line_height(LineHeight::Length(20.0)),
        )];
        let items = [
            InlineItem::Text {
                text: "ab ",
                element: 0,
            },
            InlineItem::Float { id: 7 },
            InlineItem::Text {
                text: "cd ef gh ij kl mn op qr st uv wx yz 01 23 45 67 89",
                element: 0,
            },
        ];
        let mut env = Beside {
            left: false,
            width: 60.0,
            height: 30.0,
            placed: Vec::new(),
        };
        let mut source = LineSource::Env {
            env: &mut env,
            hints: &[],
        };
        let (_, trace) =
            Font::load_embedded().shape_inline_tree_in_slots(&elements, &items, 200.0, &mut source);
        assert_eq!(env.placed, vec![(7, 0.0)]);
        close(trace.slots[0].width, 140.0);
    }

    #[test]
    fn replaying_the_slots_of_a_pass_gives_the_same_lines_without_the_floats() {
        let mut env = Beside {
            left: true,
            width: 50.0,
            height: 50.0,
            placed: Vec::new(),
        };
        let (first, trace) = shaped_beside(&mut env, sans(16.0));
        let style = sans(16.0).with_line_height(LineHeight::Length(20.0));
        let elements = [InlineElement::new(0, style)];
        let items = [InlineItem::Text {
            text: FOX,
            element: 0,
        }];
        let mut source = LineSource::Replay(&trace.slots);
        let (again, _) =
            Font::load_embedded().shape_inline_tree_in_slots(&elements, &items, 200.0, &mut source);
        assert_eq!(line_extents(&first), line_extents(&again));
    }

    /// A paragraph of plain words, wide enough for a few lines at 14px.
    const WORDS: &str = "The quick brown fox jumps over the lazy dog and keeps running far away";

    fn first_line_elements(base: TextStyle, line: TextStyle) -> [InlineElement; 1] {
        [InlineElement {
            first_line: Some(line),
            ..InlineElement::new(0, base)
        }]
    }

    fn sizes_by_line(layout: &InlineLayout) -> Vec<Vec<f32>> {
        let lines = layout
            .runs
            .iter()
            .map(|r| r.line)
            .max()
            .map_or(0, |l| l + 1);
        (0..lines)
            .map(|line| {
                layout
                    .runs
                    .iter()
                    .filter(|r| r.line == line)
                    .map(|r| r.font_size)
                    .collect()
            })
            .collect()
    }

    fn glyph_count(layout: &InlineLayout, line: Option<u32>) -> usize {
        layout
            .runs
            .iter()
            .filter(|r| line.is_none_or(|line| r.line == line))
            .map(|r| r.glyphs.len())
            .sum()
    }

    #[test]
    fn the_first_line_is_set_in_its_own_style_and_the_rest_in_the_style_of_the_text() {
        let base = sans(14.0);
        let elements = first_line_elements(base, sans(22.0));
        let items = [text(WORDS, 0)];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(200.0));
        let sizes = sizes_by_line(&layout);
        assert!(sizes.len() >= 3, "{sizes:?}");
        assert!(sizes[0].iter().all(|&s| s == 22.0), "{sizes:?}");
        assert!(sizes[1..].iter().flatten().all(|&s| s == 14.0), "{sizes:?}");
        assert_eq!(layout.first_line, Some(1));
        // The lines break where the style of each puts them: fewer words fit in the
        // first line than in a line of the plain style.
        let plain = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, base)],
            &items,
            Some(200.0),
        );
        assert!(glyph_count(&layout, Some(0)) < glyph_count(&plain, Some(0)));
        // Every glyph of the text is there once.
        assert_eq!(glyph_count(&layout, None), glyph_count(&plain, None));
    }

    #[test]
    fn a_first_line_style_equal_to_the_style_lays_out_the_same_lines() {
        let base = sans(14.0);
        let items = [text(WORDS, 0)];
        let plain = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, base)],
            &items,
            Some(300.0),
        );
        let same = Font::load_embedded().shape_inline_tree(
            &first_line_elements(base, base),
            &items,
            Some(300.0),
        );
        assert_eq!(plain.runs.len(), same.runs.len());
        close(plain.height, same.height);
        close(plain.width, same.width);
        for (a, b) in plain.runs.iter().zip(&same.runs) {
            assert_eq!((a.line, a.glyphs.len()), (b.line, b.glyphs.len()));
            close(a.x, b.x);
            close(a.width, b.width);
        }
    }

    #[test]
    fn the_first_line_has_the_line_height_of_its_style() {
        let base = sans(14.0).with_line_height(LineHeight::Length(18.0));
        let line = base.with_line_height(LineHeight::Length(40.0));
        let items = [text(WORDS, 0)];
        let layout = Font::load_embedded().shape_inline_tree(
            &first_line_elements(base, line),
            &items,
            Some(300.0),
        );
        let plain = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, base)],
            &items,
            Some(300.0),
        );
        close(layout.height, plain.height + 22.0);
    }

    #[test]
    fn a_centered_first_line_is_centered_in_the_width_it_has() {
        let base = sans(14.0).with_align(TextAlign::Center);
        let elements = first_line_elements(base, sans(14.0).with_align(TextAlign::Center));
        let items = [text(WORDS, 0)];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(300.0));
        let line: Vec<&ShapedRun> = layout.runs.iter().filter(|r| r.line == 0).collect();
        let start = line.iter().map(|r| r.x).fold(f32::MAX, f32::min);
        let end = line.iter().map(|r| r.x + r.width).fold(0.0, f32::max);
        // White space at the end of the line hangs: the text is centered.
        assert!(
            start > 1.0 && (start - (300.0 - end)).abs() < 8.0,
            "{start} {end}"
        );
    }

    #[test]
    fn a_justified_first_line_is_stretched_to_the_width() {
        let base = sans(14.0).with_align(TextAlign::Justify);
        let elements = first_line_elements(base, base);
        let items = [text(WORDS, 0)];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(300.0));
        let plain = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, base)],
            &items,
            Some(300.0),
        );
        let end = |layout: &InlineLayout| {
            layout
                .runs
                .iter()
                .filter(|r| r.line == 0)
                .map(|r| r.x + r.width)
                .fold(0.0, f32::max)
        };
        // The first line is stretched to the width like a line of the plain style.
        assert!(end(&plain) > 295.0, "{}", end(&plain));
        close(end(&layout), end(&plain));
    }

    #[test]
    fn an_element_the_first_line_ends_in_is_cut_there_and_goes_on_below() {
        let base = sans(14.0);
        let elements = [
            InlineElement {
                first_line: Some(sans(20.0)),
                ..InlineElement::new(0, base)
            },
            InlineElement {
                first_line: Some(sans(20.0)),
                ..InlineElement::new(0, base)
            },
        ];
        let items = [
            text("Words before ", 0),
            InlineItem::Spacer {
                element: 1,
                inside: 4.0,
                margin: 0.0,
                end: false,
            },
            text("and then an element with a good many more words in it", 1),
            InlineItem::Spacer {
                element: 1,
                inside: 4.0,
                margin: 0.0,
                end: true,
            },
            text(" and after", 0),
        ];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(260.0));
        let on_first: Vec<&InlineFragment> = layout
            .fragments
            .iter()
            .filter(|f| f.line == 0 && f.element == 2 + 1)
            .collect();
        let below: Vec<&InlineFragment> = layout
            .fragments
            .iter()
            .filter(|f| f.line == 1 && f.element == 1)
            .collect();
        assert_eq!(on_first.len(), 1, "{:?}", layout.fragments);
        assert_eq!(below.len(), 1, "{:?}", layout.fragments);
        assert!(on_first[0].starts && !on_first[0].ends);
        assert!(!below[0].starts, "it started on the first line");
        let last = layout
            .fragments
            .iter()
            .rfind(|f| f.element == 1)
            .expect("the element has fragments");
        assert!(last.ends, "and ends where its text does");
    }

    #[test]
    fn text_that_fits_the_first_line_alone_is_one_line_in_its_style() {
        let elements = first_line_elements(sans(14.0), sans(20.0));
        let items = [text("short", 0)];
        let layout = Font::load_embedded().shape_inline_tree(&elements, &items, Some(300.0));
        assert_eq!(sizes_by_line(&layout), vec![vec![20.0]]);
        assert_eq!(layout.first_line, Some(1));
    }

    #[test]
    fn the_text_after_the_first_line_is_prepared_in_its_own_style() {
        let base = sans(14.0);
        let mut line = sans(14.0);
        line.transform.case = crate::TextCase::Uppercase;
        let items = [text(
            "alpha beta gamma delta epsilon zeta eta theta iota kappa",
            0,
        )];
        let layout = Font::load_embedded().shape_inline_tree(
            &first_line_elements(base, line),
            &items,
            Some(160.0),
        );
        let plain_upper = {
            let mut upper = base;
            upper.transform.case = crate::TextCase::Uppercase;
            Font::load_embedded().shape_inline_tree(
                &[InlineElement::new(0, upper)],
                &items,
                Some(160.0),
            )
        };
        let rest = [text("delta epsilon zeta eta theta iota kappa", 0)];
        let plain = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, base)],
            &rest,
            Some(160.0),
        );
        let glyph_ids = |layout: &InlineLayout, line: u32| -> Vec<u32> {
            layout
                .runs
                .iter()
                .filter(|r| r.line == line)
                .flat_map(|r| r.glyphs.iter().map(|g| g.id))
                .collect()
        };
        assert_eq!(glyph_ids(&layout, 0), glyph_ids(&plain_upper, 0));
        // What the first line leaves is laid out as text of its own, in its own style.
        assert_eq!(glyph_ids(&layout, 1), glyph_ids(&plain, 0));
        assert_eq!(glyph_ids(&layout, 2), glyph_ids(&plain, 1));
    }

    #[test]
    fn a_first_line_beside_a_float_has_the_room_the_float_leaves_it() {
        let mut env = Beside {
            left: true,
            width: 50.0,
            height: 30.0,
            placed: Vec::new(),
        };
        let base = sans(14.0).with_line_height(LineHeight::Length(20.0));
        let line = sans(20.0).with_line_height(LineHeight::Length(20.0));
        let elements = first_line_elements(base, line);
        let items = [text(WORDS, 0)];
        let mut source = LineSource::Env {
            env: &mut env,
            hints: &[],
        };
        let (layout, trace) =
            Font::load_embedded().shape_inline_tree_in_slots(&elements, &items, 200.0, &mut source);
        let extents = line_extents(&layout);
        assert!(extents.len() >= 3, "{extents:?}");
        close(extents[0].0, 50.0);
        close(extents[1].0, 50.0);
        close(extents[2].0, 0.0);
        assert_eq!(trace.slots.len(), extents.len());
        assert_eq!(trace.heights.len(), extents.len());
        close(trace.slots[0].x, 50.0);
        close(trace.slots[2].x, 0.0);
    }

    #[test]
    fn a_float_after_the_first_line_is_placed_once_on_the_line_it_is_reached_on() {
        let mut env = Beside {
            left: false,
            width: 40.0,
            height: 200.0,
            placed: Vec::new(),
        };
        let base = sans(14.0).with_line_height(LineHeight::Length(20.0));
        let line = sans(18.0).with_line_height(LineHeight::Length(20.0));
        let elements = first_line_elements(base, line);
        let items = [
            text("The quick brown fox jumps over the lazy dog ", 0),
            InlineItem::Float { id: 7 },
            text("and keeps running far away from here", 0),
        ];
        let mut source = LineSource::Env {
            env: &mut env,
            hints: &[],
        };
        let (layout, _) =
            Font::load_embedded().shape_inline_tree_in_slots(&elements, &items, 200.0, &mut source);
        assert!(layout.runs.iter().any(|run| run.line >= 1));
        assert_eq!(env.placed.len(), 1, "{:?}", env.placed);
        assert_eq!(env.placed[0].0, 7);
        // It is reached below the first line, which is 20px tall.
        assert!(env.placed[0].1 >= 19.9, "{:?}", env.placed);
    }

    fn right_to_left(style: TextStyle) -> TextStyle {
        style.with_direction(TextDirection::Rtl, BidiMode::Normal)
    }

    fn visual_ids(layout: &InlineLayout) -> Vec<u32> {
        layout
            .runs
            .iter()
            .filter(|run| run.width > 0.0)
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.id))
            .collect()
    }

    #[test]
    fn a_right_to_left_paragraph_of_latin_text_starts_at_the_right_edge() {
        let items = [text("abc def", 0)];
        let ltr = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, sans(14.0))],
            &items,
            Some(300.0),
        );
        let rtl = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, right_to_left(sans(14.0)))],
            &items,
            Some(300.0),
        );
        let reach = |layout: &InlineLayout| {
            let run = layout.runs.iter().find(|run| run.width > 0.0).unwrap();
            (run.x, run.x + run.width)
        };
        close(reach(&ltr).0, 0.0);
        // The trailing space hangs, so the text ends at the edge.
        assert!((300.0 - reach(&rtl).1).abs() < 5.0, "{:?}", reach(&rtl));
        assert!(reach(&rtl).0 > 200.0, "{:?}", reach(&rtl));
        assert_eq!(
            visual_ids(&ltr).len(),
            visual_ids(&rtl).len(),
            "the mark draws nothing"
        );
    }

    #[test]
    fn the_runs_of_mixed_text_are_ordered_by_the_direction_of_the_paragraph() {
        let items = [text("ab \u{5e9}\u{5dc}", 0)];
        let order = |style: TextStyle| {
            let layout = Font::load_embedded().shape_inline_tree(
                &[InlineElement::new(0, style)],
                &items,
                Some(300.0),
            );
            let mut runs: Vec<(f32, usize)> = layout
                .runs
                .iter()
                .filter(|run| run.width > 0.0)
                .map(|run| (run.x, run.glyphs.len()))
                .collect();
            runs.sort_by(|a, b| a.0.total_cmp(&b.0));
            runs.first().map(|run| run.1)
        };
        // Two Hebrew letters and three characters of "ab ": the left end is
        // Latin in a left to right paragraph and Hebrew in a right to left one.
        let (ltr_first, rtl_first) = (order(sans(14.0)), order(right_to_left(sans(14.0))));
        assert_eq!((ltr_first, rtl_first), (Some(3), Some(2)));
    }

    #[test]
    fn a_bidi_override_puts_the_text_in_the_order_of_the_direction() {
        let items = [text("abc def", 0)];
        let plain = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, sans(14.0))],
            &items,
            Some(300.0),
        );
        let overridden = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(
                0,
                sans(14.0).with_direction(TextDirection::Rtl, BidiMode::Override),
            )],
            &items,
            Some(300.0),
        );
        let mut reversed = visual_ids(&plain);
        reversed.reverse();
        assert_eq!(visual_ids(&overridden), reversed);
    }

    #[test]
    fn an_empty_right_to_left_container_has_no_text_to_lay_out() {
        let layout = Font::load_embedded().shape_inline_tree(
            &[InlineElement::new(0, right_to_left(sans(14.0)))],
            &[text("", 0)],
            Some(300.0),
        );
        assert!(layout.runs.is_empty());
        assert_eq!(layout.height, 0.0);
    }
}
