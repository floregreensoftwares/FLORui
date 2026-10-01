//! Text measurement — the "text" stage the engine's layout consults for
//! intrinsic sizing, between elements/style and boxes/layout in the
//! pipeline (components → elements → style → boxes → layout ↔ **text** →
//! painting).
//!
//! Checked against [Parley](https://github.com/linebender/parley) 0.11's
//! actual current API (a standalone experiment, plus reading its vendored
//! source) rather than assumed from memory, after getting bitten by
//! exactly that mistake earlier in this project's history.
//!
//! # Scope
//!
//! Two embedded font families — [`FontFamily::SansSerif`] (Open Sans) and
//! [`FontFamily::Monospace`] (Fira Mono); see `fonts/NOTICE.md` for
//! provenance and the deliberate choice not to metrically match Arial —
//! selected per call, one size, one plain unstyled run of text.
//! [`Font::measure`]/[`Font::shape`] answer their questions (respectively:
//! how wide/tall, and which glyphs at which pen positions) unwrapped, as a
//! single line; [`Font::measure_wrapped`]/[`Font::shape_wrapped`] answer
//! the same two questions wrapped at a given available width, real
//! multi-line layout. `ShapedGlyph`'s `x`/`y` are already absolute within
//! the whole shaped block regardless of which line a glyph landed on, so
//! [`ShapedText`] needs no separate per-line type to support either case.
//!
//! [`Font::register`] adds a font beyond the two embedded ones — e.g. one
//! covering a script or symbol set neither does — into the same shared
//! collection every measure/shape call already searches for a codepoint
//! the requested family doesn't cover. That search is Parley/fontique's
//! own fallback behavior, not something this crate implements itself;
//! registering a font that covers a previously-uncovered codepoint takes
//! effect on the very next call, since nothing here caches a shaped or
//! measured result across calls to go stale.
//!
//! Not addressed here: min-content sizing (the width of the single
//! longest unbreakable word) — [`Font::measure_wrapped`] wraps at an
//! explicit width or not at all, so a caller with only a min-content
//! constraint (no definite width anywhere) still gets unwrapped
//! measurement, which overstates how narrow the text could actually go.

use std::collections::HashMap;
use std::sync::Arc;

use parley::{
    FontContext, FontData, FontFamily as ParleyFontFamily, FontWeight, InlineBox, InlineBoxKind,
    LayoutContext, PositionedLayoutItem, StyleProperty,
};
use peniko::Blob;

pub mod editing;

/// Open Sans (SIL Open Font License 1.1) — see `fonts/NOTICE.md` for
/// provenance and why it's this crate's sans-serif default despite not
/// being metrically matched to Arial.
pub const EMBEDDED_SANS_SERIF_FONT: &[u8] = include_bytes!("../fonts/OpenSans-Variable.ttf");
/// Fira Mono (SIL Open Font License 1.1) — see `fonts/NOTICE.md`.
pub const EMBEDDED_MONOSPACE_FONT: &[u8] = include_bytes!("../fonts/FiraMono-Medium.ttf");

/// Which of this crate's two embedded families to shape/measure with —
/// mirrors `florui_style::FontFamily`, kept as this crate's own type since
/// `florui-text` has no dependency on `florui-style` (`florui-layout` is
/// what translates between them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontFamily {
    #[default]
    SansSerif,
    Monospace,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMetrics {
    /// The widest line's width — the whole (unwrapped) text's width for
    /// [`Font::measure`], or the widest of however many lines
    /// [`Font::measure_wrapped`] wrapped it into.
    pub width: f32,
    /// The full height across every line.
    pub height: f32,
    /// Offset from the very top of the text block down to the *first*
    /// line's baseline — constant for a given family/size regardless of
    /// content or how many lines wrapping produced, since it comes from
    /// the font's own ascent metric, not from what the text says. This is
    /// what CSS's `align-items: baseline` aligns flex items by.
    pub baseline: f32,
}

/// One glyph, positioned relative to the text's own top-left origin
/// (y-down, baseline already accounted for) — everything a rasterizer
/// needs to place it, but no outline data: that lives in the font itself,
/// keyed by [`ShapedGlyph::id`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    pub id: u32,
    pub x: f32,
    pub y: f32,
}

/// A sequence of glyphs sharing one font and size — Parley may itemize a
/// single call to [`Font::shape`] into more than one run (script/bidi
/// boundaries, or a fallback font substituted for a codepoint the
/// requested family doesn't cover), so each run carries its own font
/// rather than assuming the caller's requested one.
#[derive(Debug, Clone)]
pub struct ShapedRun {
    pub font: FontData,
    pub font_size: f32,
    /// This run's variable-font instance coordinates (e.g. Open Sans's
    /// `wght` axis for a bold run) — a rasterizer must pass these back
    /// when drawing outlines, or every run draws at the font's default
    /// instance regardless of what was shaped.
    pub normalized_coords: Vec<i16>,
    pub glyphs: Vec<ShapedGlyph>,
}

#[derive(Debug, Clone)]
pub struct ShapedText {
    pub runs: Vec<ShapedRun>,
    pub width: f32,
    pub height: f32,
}

/// One piece of a real inline formatting context, in source order — either
/// a run of text with its own font (a bare inline run, or a `display:
/// inline` element's own text), or an opaque box with a pre-measured size
/// that flows with the surrounding text as one atomic unit (`display:
/// inline-block`'s real CSS behavior). `id` is the caller's own identifier
/// (e.g. an arena `NodeId` cast to `u64`), echoed back on the matching
/// [`PositionedBox`] so the caller can tell which box is which — a
/// [`InlineContent::Text`] item needs no such echo since this crate does
/// not yet report a per-run box, only the whole block's own metrics (see
/// [`Font::shape_inline`]'s own doc for why).
#[derive(Debug, Clone, Copy)]
pub enum InlineContent<'a> {
    Text {
        text: &'a str,
        font_size: f32,
        font_weight: f32,
        family: FontFamily,
    },
    Box {
        id: u64,
        width: f32,
        height: f32,
    },
}

/// Where [`Font::shape_inline`] placed one [`InlineContent::Box`] item,
/// keyed back to it by `id` — `x`/`y` are relative to the whole inline
/// block's own top-left origin, the same space [`InlineLayout::width`]/
/// [`InlineLayout::height`] describe.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionedBox {
    pub id: u64,
    pub x: f32,
    pub y: f32,
}

/// The result of laying out a real inline formatting context — real
/// multi-style, multi-line text wrapping plus in-flow boxes, via
/// [Parley's own rich-text and inline-box support](https://docs.rs/parley),
/// not a hand-rolled line breaker.
#[derive(Debug, Clone)]
pub struct InlineLayout {
    /// The widest line's width.
    pub width: f32,
    /// The full height across every line.
    pub height: f32,
    /// Offset from the top of the block to the first line's baseline —
    /// same meaning as [`TextMetrics::baseline`].
    pub baseline: f32,
    pub runs: Vec<ShapedRun>,
    pub boxes: Vec<PositionedBox>,
}

#[derive(Debug)]
pub struct TextError {
    message: &'static str,
}

impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TextError {}

/// A Parley layout that has already been itemized and shaped, kept around
/// so [`Font::measure_cached`] can re-line-break it (cheap) instead of
/// rebuilding it (the expensive step — itemization, script/font
/// fallback, shaping every cluster) on every call. Safe to re-break
/// repeatedly at different widths: Parley's own `break_lines`/
/// `break_all_lines` clear and recompute a layout's line data from
/// scratch every time they're called, never accumulating stale lines
/// from a prior break. Opaque — nothing outside this crate constructs or
/// inspects one; a caller only ever holds it in an `Option` it passes
/// back into [`Font::measure_cached`].
pub struct CachedLayout(parley::Layout<[u8; 4]>);

/// Family, font size bits and font weight bits.
type MemoStyle = (u8, u32, u32);

/// A wrap width (bits, `None` for unwrapped) and what it measured.
type MemoWidth = (Option<u32>, TextMetrics);

/// One generation of [`MetricsMemo`]. Flattened into two levels so a lookup
/// borrows the text instead of allocating a key for it.
#[derive(Default)]
struct Generation {
    by_style: HashMap<MemoStyle, HashMap<String, Vec<MemoWidth>>>,
    entries: usize,
}

impl Generation {
    fn get(&self, style: MemoStyle, text: &str, width: Option<u32>) -> Option<TextMetrics> {
        self.by_style
            .get(&style)?
            .get(text)?
            .iter()
            .find(|(wrap, _)| *wrap == width)
            .map(|&(_, metrics)| metrics)
    }

    fn insert(&mut self, style: MemoStyle, text: &str, width: Option<u32>, metrics: TextMetrics) {
        self.by_style
            .entry(style)
            .or_default()
            .entry(text.to_owned())
            .or_default()
            .push((width, metrics));
        self.entries += 1;
    }
}

/// Metrics of text already measured, by exactly what was asked: style, text
/// and wrap width. The same tree asks the same questions every frame, so a
/// hit skips shaping and line breaking entirely.
///
/// Kept in two generations. New answers go into the current one; when it
/// fills, it becomes the previous one and the old previous is dropped. A hit
/// in the previous generation moves back into the current one, so whatever a
/// frame keeps asking for survives indefinitely and whatever stopped being
/// asked for is gone within two generations, without tracking the order of
/// every use. The two together hold at most `capacity` entries.
struct MetricsMemo {
    current: Generation,
    previous: Generation,
    generation_capacity: usize,
    #[cfg(test)]
    misses: usize,
}

/// Entries kept in total, across both generations.
const METRICS_MEMO_CAPACITY: usize = 50_000;

impl MetricsMemo {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            current: Generation::default(),
            previous: Generation::default(),
            generation_capacity: (capacity / 2).max(1),
            #[cfg(test)]
            misses: 0,
        }
    }

    #[cfg(test)]
    fn entries(&self) -> usize {
        self.current.entries + self.previous.entries
    }

    fn get(&mut self, style: MemoStyle, text: &str, width: Option<u32>) -> Option<TextMetrics> {
        if let Some(metrics) = self.current.get(style, text, width) {
            return Some(metrics);
        }
        let metrics = self.previous.get(style, text, width)?;
        self.insert(style, text, width, metrics);
        Some(metrics)
    }

    fn insert(&mut self, style: MemoStyle, text: &str, width: Option<u32>, metrics: TextMetrics) {
        if self.current.entries >= self.generation_capacity {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(style, text, width, metrics);
    }

    fn clear(&mut self) {
        self.current = Generation::default();
        self.previous = Generation::default();
    }
}

/// Holds the (deliberately reused, per Parley's own guidance) scratch
/// state needed to measure text, plus every font this crate currently
/// knows about.
pub struct Font {
    /// `pub(crate)`, not private: [`editing`]'s `impl Font` block (a real
    /// text-editing buffer needs the exact same `FontContext`/
    /// `LayoutContext` every other shape/measure call already reuses, not
    /// a second, separately-loaded pair) reaches these directly.
    pub(crate) font_cx: FontContext,
    pub(crate) layout_cx: LayoutContext,
    sans_serif_family_name: String,
    monospace_family_name: String,
    metrics_memo: MetricsMemo,
}

impl Font {
    /// Loads both embedded fonts: [`EMBEDDED_SANS_SERIF_FONT`] as
    /// [`FontFamily::SansSerif`], [`EMBEDDED_MONOSPACE_FONT`] as
    /// [`FontFamily::Monospace`].
    pub fn load_embedded() -> Self {
        let mut font_cx = FontContext::new();
        let sans_serif_family_name = Self::register_into(&mut font_cx, EMBEDDED_SANS_SERIF_FONT)
            .expect("the embedded sans-serif font is always valid");
        let monospace_family_name = Self::register_into(&mut font_cx, EMBEDDED_MONOSPACE_FONT)
            .expect("the embedded monospace font is always valid");
        Self {
            font_cx,
            layout_cx: LayoutContext::new(),
            sans_serif_family_name,
            monospace_family_name,
            metrics_memo: MetricsMemo::with_capacity(METRICS_MEMO_CAPACITY),
        }
    }

    /// Registers `font_bytes` into this instance's shared font collection,
    /// returning its resolved family name. Neither embedded default
    /// changes — this is additive, for a codepoint neither covers (a
    /// script, a symbol set): Parley's own fallback search already
    /// considers every font in the collection, this one included, the
    /// moment it's registered.
    pub fn register(&mut self, font_bytes: &[u8]) -> Result<String, TextError> {
        // A newly registered font can change what text measures.
        self.metrics_memo.clear();
        Self::register_into(&mut self.font_cx, font_bytes)
    }

    fn register_into(font_cx: &mut FontContext, font_bytes: &[u8]) -> Result<String, TextError> {
        let registered = font_cx
            .collection
            .register_fonts(Blob::new(Arc::new(font_bytes.to_vec())), None);
        let (family_id, _) = registered.first().ok_or(TextError {
            message: "not a valid font file",
        })?;
        font_cx
            .collection
            .family_name(*family_id)
            .ok_or(TextError {
                message: "registered font has no family name",
            })
            .map(str::to_owned)
    }

    pub(crate) fn family_name(&self, family: FontFamily) -> &str {
        match family {
            FontFamily::SansSerif => &self.sans_serif_family_name,
            FontFamily::Monospace => &self.monospace_family_name,
        }
    }

    /// Measures `text` in `family` at `font_size`/`font_weight`, as if
    /// nothing constrained its width: no wrapping, a single line.
    pub fn measure(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
    ) -> TextMetrics {
        Self::metrics_of(self.layout_unwrapped(family, text, font_size, font_weight))
    }

    /// Measures `text` in `family` at `font_size`/`font_weight`, wrapping
    /// at `max_width` — real multi-line layout. A single word wider than
    /// `max_width` still gets its own (overflowing) line rather than
    /// being broken mid-word or bleeding onto an adjacent line — Parley's
    /// own wrapping behavior, matching real CSS's default
    /// `overflow-wrap: normal`.
    pub fn measure_wrapped(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
        max_width: f32,
    ) -> TextMetrics {
        Self::metrics_of(self.layout_wrapped(family, text, font_size, font_weight, max_width))
    }

    /// Same measurement as [`Self::measure`] (`max_width: None`) or
    /// [`Self::measure_wrapped`] (`max_width: Some(width)`), but reusing
    /// `*cache` across repeated calls instead of rebuilding — itemizing
    /// and shaping every cluster — from scratch each time. A caller that
    /// queries the same `text`/`family`/`font_size`/`font_weight` more
    /// than once (Taffy's own layout algorithms do this per leaf: an
    /// intrinsic min/max-content pass, then a final definite pass, often
    /// landing on more than one distinct width and sometimes the same one
    /// twice) only pays the expensive shaping step on the first call;
    /// every later call just re-breaks the already-shaped layout at
    /// whatever `max_width` it asks for, which is comparatively cheap —
    /// see [`CachedLayout`]'s own doc for why that's safe to do repeatedly.
    ///
    /// `*cache` must have come from a prior call with this exact
    /// text/family/size/weight — typically one cache slot per text leaf,
    /// scoped to one layout pass and reset for the next. Passing a cache
    /// built for different text silently measures the *cached* text, not
    /// `text`, the same kind of caller contract this crate's other
    /// measure/shape methods already trust their arguments to uphold.
    pub fn measure_cached(
        &mut self,
        cache: &mut Option<CachedLayout>,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
        max_width: Option<f32>,
    ) -> TextMetrics {
        if text.is_empty() {
            *cache = None;
            return TextMetrics {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
            };
        }
        if cache.is_none() {
            *cache = Some(CachedLayout(self.build_unbroken(
                family,
                text,
                font_size,
                font_weight,
            )));
        }
        let layout = &mut cache
            .as_mut()
            .expect("just populated above if it was empty")
            .0;
        layout.break_all_lines(max_width);
        Self::metrics_of_ref(layout)
    }

    /// [`Self::measure_cached`], remembered across calls: the same style,
    /// text and wrap width answer from the memo without shaping or breaking
    /// lines, which is what a frame that changed nothing asks for again.
    pub fn measure_memoized(
        &mut self,
        cache: &mut Option<CachedLayout>,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
        max_width: Option<f32>,
    ) -> TextMetrics {
        let style = (family as u8, font_size.to_bits(), font_weight.to_bits());
        let width = max_width.map(f32::to_bits);
        if let Some(metrics) = self.metrics_memo.get(style, text, width) {
            florui_profile::count(florui_profile::Counter::TextMemoHits, 1);
            return metrics;
        }
        florui_profile::count(florui_profile::Counter::TextMemoMisses, 1);
        let metrics = self.measure_cached(cache, family, text, font_size, font_weight, max_width);
        #[cfg(test)]
        {
            self.metrics_memo.misses += 1;
        }
        // Empty text measures to zero without shaping; not worth an entry.
        if !text.is_empty() {
            self.metrics_memo.insert(style, text, width, metrics);
        }
        metrics
    }

    fn metrics_of(layout: Option<parley::Layout<[u8; 4]>>) -> TextMetrics {
        match layout {
            // Parley measures an empty string as zero-width but still
            // reports a nonzero line height; a truly empty run should not
            // claim to occupy a line it never lays out.
            None => TextMetrics {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
            },
            Some(layout) => Self::metrics_of_ref(&layout),
        }
    }

    fn metrics_of_ref(layout: &parley::Layout<[u8; 4]>) -> TextMetrics {
        TextMetrics {
            width: layout.width(),
            height: layout.height(),
            // The first line's own baseline offset is already relative to
            // the top of the whole block, since nothing precedes it.
            baseline: layout
                .lines()
                .next()
                .map(|line| line.metrics().baseline)
                .unwrap_or(0.0),
        }
    }

    /// Shapes `text` in `family` at `font_size`/`font_weight` into
    /// paintable glyphs — same unwrapped, single-line layout as
    /// [`Self::measure`], but exposing glyph ids and pen positions instead
    /// of just overall width/height.
    pub fn shape(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
    ) -> ShapedText {
        Self::shaped_text_of(self.layout_unwrapped(family, text, font_size, font_weight))
    }

    /// Shapes `text` in `family` at `font_size`/`font_weight` into
    /// paintable glyphs, wrapped at `max_width` — same wrapping behavior
    /// as [`Self::measure_wrapped`]. Every glyph's `x`/`y` stays absolute
    /// within the whole shaped block (not line-relative), so a caller
    /// paints this exactly like an unwrapped [`Self::shape`] result —
    /// later lines simply carry larger `y` values, with no separate
    /// per-line structure to unpack.
    pub fn shape_wrapped(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
        max_width: f32,
    ) -> ShapedText {
        Self::shaped_text_of(self.layout_wrapped(family, text, font_size, font_weight, max_width))
    }

    fn shaped_text_of(layout: Option<parley::Layout<[u8; 4]>>) -> ShapedText {
        let Some(layout) = layout else {
            return ShapedText {
                runs: Vec::new(),
                width: 0.0,
                height: 0.0,
            };
        };

        let mut runs = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                    continue;
                };
                let run = glyph_run.run();
                let glyphs = glyph_run
                    .positioned_glyphs()
                    .map(|glyph| ShapedGlyph {
                        id: glyph.id,
                        x: glyph.x,
                        y: glyph.y,
                    })
                    .collect();
                runs.push(ShapedRun {
                    font: run.font().clone(),
                    font_size: run.font_size(),
                    normalized_coords: run.normalized_coords().to_vec(),
                    glyphs,
                });
            }
        }

        ShapedText {
            runs,
            width: layout.width(),
            height: layout.height(),
        }
    }

    /// Lays out a real inline formatting context: `items` in source order,
    /// each a text run with its own font or an in-flow box with a
    /// pre-measured size — real multi-style text wrapping mixed with
    /// atomic boxes, via Parley's own `RangedBuilder::push`/
    /// `push_inline_box` (checked against Parley 0.11's actual doc example
    /// for this exact feature, not assumed). `max_width` wraps the same way
    /// [`Self::measure_wrapped`] does; `None` is a single unconstrained
    /// line unless a box or line break forces more.
    ///
    /// Bounded, not a full inline formatting context: this only reports a
    /// bounding box back for a [`InlineContent::Box`] item, not for a
    /// [`InlineContent::Text`] item — a plain `display: inline` element's
    /// own box (for painting its own background, or hit-testing it
    /// directly) isn't available from this call, only its text's ink,
    /// mixed into the block's own `runs`. See `florui_layout`'s own module
    /// docs for why this bound was chosen and what it costs.
    pub fn shape_inline(
        &mut self,
        items: &[InlineContent<'_>],
        max_width: Option<f32>,
    ) -> InlineLayout {
        let mut text = String::new();
        for item in items {
            if let InlineContent::Text { text: run_text, .. } = item {
                text.push_str(run_text);
            }
        }
        let has_box = items
            .iter()
            .any(|item| matches!(item, InlineContent::Box { .. }));
        if text.is_empty() && !has_box {
            return InlineLayout {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
                runs: Vec::new(),
                boxes: Vec::new(),
            };
        }

        // Resolved before the builder borrows `self` mutably below — only
        // two possible families exist, so both names are cheap to clone
        // up front rather than re-borrowing `self` per item.
        let sans_serif_family_name = self.sans_serif_family_name.clone();
        let monospace_family_name = self.monospace_family_name.clone();
        let family_name = |family: FontFamily| match family {
            FontFamily::SansSerif => &sans_serif_family_name,
            FontFamily::Monospace => &monospace_family_name,
        };

        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, &text, 1.0, true);
        // Parley's ranged builder needs a base style to resolve every
        // range against, even though every byte range with real text below
        // sets its own font-size/family explicitly — nothing ever actually
        // renders at this default.
        builder.push_default(StyleProperty::FontSize(16.0));

        let mut offset = 0usize;
        for item in items {
            match item {
                InlineContent::Text {
                    text: run_text,
                    font_size,
                    font_weight,
                    family,
                } => {
                    let end = offset + run_text.len();
                    if !run_text.is_empty() {
                        let family_name = family_name(*family);
                        builder.push(StyleProperty::FontSize(*font_size), offset..end);
                        builder.push(
                            StyleProperty::FontWeight(FontWeight::new(*font_weight)),
                            offset..end,
                        );
                        builder.push(
                            StyleProperty::FontFamily(ParleyFontFamily::named(
                                family_name.as_str(),
                            )),
                            offset..end,
                        );
                    }
                    offset = end;
                }
                InlineContent::Box { id, width, height } => {
                    builder.push_inline_box(InlineBox {
                        id: *id,
                        kind: InlineBoxKind::InFlow,
                        index: offset,
                        width: *width,
                        height: *height,
                    });
                }
            }
        }

        let mut layout: parley::Layout<[u8; 4]> = builder.build(&text);
        layout.break_all_lines(max_width);

        let mut runs = Vec::new();
        let mut boxes = Vec::new();
        for line in layout.lines() {
            for item in line.items() {
                match item {
                    PositionedLayoutItem::GlyphRun(glyph_run) => {
                        let run = glyph_run.run();
                        let glyphs = glyph_run
                            .positioned_glyphs()
                            .map(|glyph| ShapedGlyph {
                                id: glyph.id,
                                x: glyph.x,
                                y: glyph.y,
                            })
                            .collect();
                        runs.push(ShapedRun {
                            font: run.font().clone(),
                            font_size: run.font_size(),
                            normalized_coords: run.normalized_coords().to_vec(),
                            glyphs,
                        });
                    }
                    PositionedLayoutItem::InlineBox(positioned) => {
                        boxes.push(PositionedBox {
                            id: positioned.id,
                            x: positioned.x,
                            y: positioned.y,
                        });
                    }
                }
            }
        }

        InlineLayout {
            width: layout.width(),
            height: layout.height(),
            baseline: layout
                .lines()
                .next()
                .map(|line| line.metrics().baseline)
                .unwrap_or(0.0),
            runs,
            boxes,
        }
    }

    /// [`Self::layout_wrapped`] with no width constraint — a single
    /// unwrapped line.
    fn layout_unwrapped(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
    ) -> Option<parley::Layout<[u8; 4]>> {
        self.build_layout(family, text, font_size, font_weight, None)
    }

    /// Builds and line-breaks a Parley layout for `text` in `family` at
    /// `font_size`, wrapped at `max_width` — the scratch-state setup every
    /// measure/shape method needs before reading anything back out of it.
    /// `None` for empty `text`, which Parley itself would otherwise still
    /// lay out as one zero-width line with a nonzero line height.
    fn layout_wrapped(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
        max_width: f32,
    ) -> Option<parley::Layout<[u8; 4]>> {
        self.build_layout(family, text, font_size, font_weight, Some(max_width))
    }

    fn build_layout(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
        max_width: Option<f32>,
    ) -> Option<parley::Layout<[u8; 4]>> {
        if text.is_empty() {
            return None;
        }
        let mut layout = self.build_unbroken(family, text, font_size, font_weight);
        layout.break_all_lines(max_width);
        Some(layout)
    }

    /// Itemizes and shapes `text` in `family` at `font_size`/`font_weight`
    /// — the expensive step every measure/shape method needs, shared by
    /// [`Self::build_layout`] (which breaks it once and discards it) and
    /// [`Self::measure_cached`] (which keeps the result around to re-break
    /// instead of redoing this). Callers with non-empty `text` only —
    /// unlike [`Self::build_layout`], this has nothing sensible to return
    /// for an empty string, so callers must check first.
    fn build_unbroken(
        &mut self,
        family: FontFamily,
        text: &str,
        font_size: f32,
        font_weight: f32,
    ) -> parley::Layout<[u8; 4]> {
        let family_name = self.family_name(family).to_owned();
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontSize(font_size));
        builder.push_default(StyleProperty::FontFamily(ParleyFontFamily::named(
            family_name.as_str(),
        )));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(font_weight)));
        builder.build(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_both_embedded_families_under_their_real_names() {
        let font = Font::load_embedded();
        assert_eq!(font.family_name(FontFamily::SansSerif), "Open Sans");
        assert_eq!(font.family_name(FontFamily::Monospace), "Fira Mono");
    }

    #[test]
    fn rejects_data_that_is_not_a_font() {
        let mut font = Font::load_embedded();
        assert!(font.register(b"not a font").is_err());
    }

    const MEMO_CASES: [(FontFamily, &str, f32, f32, Option<f32>); 5] = [
        (FontFamily::SansSerif, "Hello world", 16.0, 400.0, None),
        (
            FontFamily::SansSerif,
            "Hello world",
            16.0,
            400.0,
            Some(40.0),
        ),
        (
            FontFamily::SansSerif,
            "Hello world",
            16.0,
            700.0,
            Some(40.0),
        ),
        (
            FontFamily::Monospace,
            "fn main() {}",
            14.0,
            400.0,
            Some(60.0),
        ),
        (
            FontFamily::SansSerif,
            "A longer line of text that has to wrap",
            18.0,
            400.0,
            Some(120.0),
        ),
    ];

    #[test]
    fn a_memoized_measure_equals_a_fresh_one_and_repeats_without_remeasuring() {
        let mut font = Font::load_embedded();
        let mut reference = Font::load_embedded();
        for round in 0..3 {
            for (family, text, size, weight, width) in MEMO_CASES {
                let memoized = font.measure_memoized(&mut None, family, text, size, weight, width);
                let fresh = reference.measure_cached(&mut None, family, text, size, weight, width);
                assert_eq!(memoized, fresh, "{text:?} at {width:?}, round {round}");
            }
            assert_eq!(
                font.metrics_memo.misses,
                MEMO_CASES.len(),
                "every case misses once, then only hits (round {round})"
            );
        }
    }

    #[test]
    fn empty_text_measures_to_zero_and_is_not_kept() {
        let mut font = Font::load_embedded();
        let metrics =
            font.measure_memoized(&mut None, FontFamily::SansSerif, "", 16.0, 400.0, None);
        assert_eq!(
            (metrics.width, metrics.height, metrics.baseline),
            (0.0, 0.0, 0.0)
        );
        assert_eq!(font.metrics_memo.entries(), 0);
    }

    #[test]
    fn registering_a_font_empties_the_memo() {
        let mut font = Font::load_embedded();
        font.measure_memoized(&mut None, FontFamily::SansSerif, "kept", 16.0, 400.0, None);
        assert_eq!(font.metrics_memo.entries(), 1);

        font.register(EMBEDDED_MONOSPACE_FONT).unwrap();

        assert_eq!(font.metrics_memo.entries(), 0);
    }

    fn metrics(width: f32) -> TextMetrics {
        TextMetrics {
            width,
            height: 2.0,
            baseline: 3.0,
        }
    }

    #[test]
    fn what_keeps_being_asked_for_survives_while_the_unused_is_dropped() {
        // Two generations of two entries each.
        let mut memo = MetricsMemo::with_capacity(4);
        memo.insert((0, 0, 0), "hot", None, metrics(1.0));
        memo.insert((0, 0, 0), "cold", None, metrics(2.0));
        memo.insert((0, 0, 0), "c", None, metrics(3.0));
        // "hot" and "cold" are now the previous generation; asking for "hot"
        // brings it back into the current one.
        assert_eq!(memo.get((0, 0, 0), "hot", None), Some(metrics(1.0)));

        memo.insert((0, 0, 0), "d", None, metrics(4.0));
        memo.insert((0, 0, 0), "e", None, metrics(5.0));

        assert_eq!(memo.get((0, 0, 0), "hot", None), Some(metrics(1.0)));
        assert!(
            memo.get((0, 0, 0), "cold", None).is_none(),
            "never asked for again"
        );
    }

    #[test]
    fn the_memo_never_holds_more_than_its_capacity() {
        let mut memo = MetricsMemo::with_capacity(10);
        for i in 0..200 {
            memo.insert((0, 0, 0), &format!("text {i}"), None, metrics(i as f32));
            assert!(
                memo.entries() <= 10,
                "after {i} inserts: {}",
                memo.entries()
            );
        }
    }

    #[test]
    fn a_promoted_entry_keeps_its_exact_answer_and_width() {
        let mut memo = MetricsMemo::with_capacity(6);
        memo.insert((0, 0, 0), "wide", Some(40), metrics(7.0));
        memo.insert((0, 0, 0), "wide", None, metrics(9.0));
        memo.insert((0, 0, 0), "other", None, metrics(1.0));

        assert_eq!(memo.get((0, 0, 0), "wide", Some(40)), Some(metrics(7.0)));
        assert_eq!(memo.get((0, 0, 0), "wide", None), Some(metrics(9.0)));
        assert!(memo.get((0, 0, 0), "wide", Some(41)).is_none());
    }

    #[test]
    fn registering_an_additional_font_does_not_change_either_default() {
        let mut font = Font::load_embedded();
        // Registering Fira Mono again under a different role: the two
        // defaults set at construction must stay exactly what they were.
        font.register(EMBEDDED_MONOSPACE_FONT).unwrap();
        assert_eq!(font.family_name(FontFamily::SansSerif), "Open Sans");
        assert_eq!(font.family_name(FontFamily::Monospace), "Fira Mono");
    }

    #[test]
    fn measures_a_monospace_run_proportionally_to_its_length() {
        let mut font = Font::load_embedded();
        let one = font.measure(FontFamily::Monospace, "A", 16.0, 400.0);
        let five = font.measure(FontFamily::Monospace, "AAAAA", 16.0, 400.0);
        // Every glyph in a monospace font has the same advance, so five
        // characters must measure to exactly five times one character's
        // width — this would not hold for a proportional font.
        assert_eq!(five.width, one.width * 5.0);
        assert_eq!(
            five.height, one.height,
            "line height does not depend on content length"
        );
    }

    #[test]
    fn sans_serif_is_proportional_not_monospace() {
        // The opposite assertion from the monospace test above: a
        // proportional font's characters have different advances, so this
        // would be a coincidence rather than a guarantee — "i" and "M" are
        // about as different as two Latin letters get.
        let mut font = Font::load_embedded();
        let narrow = font.measure(FontFamily::SansSerif, "iiiii", 16.0, 400.0);
        let wide = font.measure(FontFamily::SansSerif, "MMMMM", 16.0, 400.0);
        assert!(
            narrow.width < wide.width,
            "Open Sans must not measure \"iiiii\" as wide as \"MMMMM\""
        );
    }

    #[test]
    fn larger_font_size_measures_wider_and_taller() {
        let mut font = Font::load_embedded();
        let small = font.measure(FontFamily::SansSerif, "Hello", 16.0, 400.0);
        let large = font.measure(FontFamily::SansSerif, "Hello", 32.0, 400.0);
        assert!(large.width > small.width);
        assert!(large.height > small.height);
    }

    #[test]
    fn bold_measures_wider_than_regular_at_the_same_size() {
        // Real variable-font weight instancing, not a no-op: bold glyphs
        // are wider, so this must actually differ, not just accept the
        // parameter and ignore it.
        let mut font = Font::load_embedded();
        let regular = font.measure(FontFamily::SansSerif, "Hello", 16.0, 400.0);
        let bold = font.measure(FontFamily::SansSerif, "Hello", 16.0, 700.0);
        assert!(bold.width > regular.width);
    }

    #[test]
    fn empty_text_measures_to_zero() {
        let mut font = Font::load_embedded();
        let metrics = font.measure(FontFamily::SansSerif, "", 16.0, 400.0);
        assert_eq!(
            metrics,
            TextMetrics {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
            }
        );
    }

    #[test]
    fn different_text_of_equal_length_can_still_differ_by_glyph() {
        // Guards against a measurement that secretly only counts
        // characters rather than actually shaping them.
        let mut font = Font::load_embedded();
        let dots = font.measure(FontFamily::Monospace, "iiiii", 16.0, 400.0);
        let wide = font.measure(FontFamily::Monospace, "MMMMM", 16.0, 400.0);
        // Fira Mono is monospace, so these happen to be equal — this test
        // exists to be revisited if the embedded monospace font ever
        // changes to a proportional one, where it would need to assert
        // inequality instead.
        assert_eq!(dots.width, wide.width);
    }

    #[test]
    fn shape_produces_one_glyph_per_character_in_source_order() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(FontFamily::Monospace, "AB", 16.0, 400.0);
        assert_eq!(shaped.runs.len(), 1, "one plain run, one font, one style");
        assert_eq!(shaped.runs[0].glyphs.len(), 2);
    }

    #[test]
    fn shape_advances_each_glyph_by_the_monospace_width() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(FontFamily::Monospace, "AA", 16.0, 400.0);
        let glyphs = &shaped.runs[0].glyphs;
        let advance = glyphs[1].x - glyphs[0].x;
        assert_eq!(
            advance,
            font.measure(FontFamily::Monospace, "A", 16.0, 400.0).width,
            "a monospace font's per-glyph advance equals a single character's measured width"
        );
        assert_eq!(
            glyphs[0].y, glyphs[1].y,
            "glyphs on the same unwrapped line share a baseline"
        );
    }

    #[test]
    fn shape_reports_the_run_actually_used_not_just_the_requested_size() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(FontFamily::Monospace, "A", 24.0, 400.0);
        assert_eq!(shaped.runs[0].font_size, 24.0);
    }

    #[test]
    fn empty_text_shapes_to_no_runs() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(FontFamily::SansSerif, "", 16.0, 400.0);
        assert!(shaped.runs.is_empty());
        assert_eq!(shaped.width, 0.0);
        assert_eq!(shaped.height, 0.0);
    }

    #[test]
    fn wrapping_at_a_width_that_fits_everything_matches_unwrapped_measurement() {
        let mut font = Font::load_embedded();
        let unwrapped = font.measure(FontFamily::Monospace, "one two three", 16.0, 400.0);
        let wrapped = font.measure_wrapped(
            FontFamily::Monospace,
            "one two three",
            16.0,
            400.0,
            unwrapped.width + 1.0,
        );
        assert_eq!(wrapped.width, unwrapped.width);
        assert_eq!(
            wrapped.height, unwrapped.height,
            "room for the whole line must not wrap it at all"
        );
    }

    #[test]
    fn wrapping_at_a_narrower_width_grows_the_height_and_shrinks_the_width() {
        let mut font = Font::load_embedded();
        let one_word = font.measure(FontFamily::Monospace, "aaaaa", 16.0, 400.0);
        let unwrapped = font.measure(FontFamily::Monospace, "aaaaa bbbbb ccccc", 16.0, 400.0);

        // Just wide enough for the widest single word, not the whole line —
        // must wrap onto three lines, one per word.
        let wrapped = font.measure_wrapped(
            FontFamily::Monospace,
            "aaaaa bbbbb ccccc",
            16.0,
            400.0,
            one_word.width + 1.0,
        );

        assert!(
            wrapped.width <= one_word.width + 1.0,
            "no wrapped line should exceed the width it wrapped at"
        );
        assert_eq!(
            wrapped.height,
            one_word.height * 3.0,
            "three words, one per line, at three times a single line's height"
        );
        assert!(wrapped.height > unwrapped.height);
    }

    #[test]
    fn wrapping_never_breaks_a_single_word_even_when_narrower_than_the_max_width() {
        // Real CSS's own default (`overflow-wrap: normal`): an unbreakable
        // word wider than the container overflows it rather than being
        // split mid-word.
        let mut font = Font::load_embedded();
        let word = font.measure(
            FontFamily::Monospace,
            "supercalifragilisticexpialidocious",
            16.0,
            400.0,
        );
        let wrapped = font.measure_wrapped(
            FontFamily::Monospace,
            "supercalifragilisticexpialidocious",
            16.0,
            400.0,
            word.width / 2.0,
        );
        assert_eq!(
            wrapped.width, word.width,
            "the single unbreakable word must overflow the requested width, not be cut"
        );
        assert_eq!(wrapped.height, word.height, "still exactly one line");
    }

    #[test]
    fn empty_text_wraps_to_zero() {
        let mut font = Font::load_embedded();
        let metrics = font.measure_wrapped(FontFamily::SansSerif, "", 16.0, 400.0, 100.0);
        assert_eq!(
            metrics,
            TextMetrics {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
            }
        );
    }

    #[test]
    fn shape_wrapped_keeps_every_glyph_and_spreads_them_across_lines() {
        let mut font = Font::load_embedded();
        let one_word = font.measure(FontFamily::Monospace, "aaaaa", 16.0, 400.0);
        let shaped = font.shape_wrapped(
            FontFamily::Monospace,
            "aaaaa bbbbb",
            16.0,
            400.0,
            one_word.width + 1.0,
        );

        let total_glyphs: usize = shaped.runs.iter().map(|run| run.glyphs.len()).sum();
        assert_eq!(
            total_glyphs, 11,
            "wrapping must not drop or duplicate any of the 10 letters or the \
             space between the two words"
        );

        let first_y = shaped.runs[0].glyphs[0].y;
        let last_run = shaped.runs.last().unwrap();
        let last_y = last_run.glyphs.last().unwrap().y;
        assert!(
            last_y > first_y,
            "a glyph on the second wrapped line must sit lower than one on the first"
        );
    }

    #[test]
    fn shape_wrapped_glyphs_on_the_same_line_share_a_y() {
        let mut font = Font::load_embedded();
        let shaped = font.shape_wrapped(FontFamily::Monospace, "aaaaa", 16.0, 400.0, 1000.0);
        let glyphs = &shaped.runs[0].glyphs;
        assert_eq!(glyphs[0].y, glyphs[4].y, "one line, one shared baseline");
    }

    /// Real fallback proof, not just an API that exists: neither embedded
    /// font has a color-emoji glyph, so this only shapes to a real glyph
    /// (not a hollow "missing character" box, and not silently empty) if
    /// Parley/fontique's own fallback search reaches past this crate's two
    /// registered fonts into a system-installed one — this environment's
    /// pinned CI is `windows-latest`, which ships Segoe UI Emoji, so this
    /// is expected to be stable there, not just on this machine.
    #[test]
    fn an_emoji_neither_embedded_font_covers_falls_back_to_a_real_glyph() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(FontFamily::Monospace, "\u{1F600}", 16.0, 400.0);

        assert_eq!(shaped.runs.len(), 1);
        let run = &shaped.runs[0];
        assert_eq!(run.glyphs.len(), 1);
        assert_ne!(
            run.glyphs[0].id, 0,
            "glyph 0 is the conventional .notdef placeholder — a real fallback \
             font must resolve the emoji to an actual glyph, not miss it"
        );
        assert!(shaped.width > 0.0);

        let font_data = run.font.data.data();
        assert_ne!(
            font_data, EMBEDDED_SANS_SERIF_FONT,
            "the emoji glyph must come from neither embedded font"
        );
        assert_ne!(font_data, EMBEDDED_MONOSPACE_FONT);
    }

    #[test]
    fn baseline_sits_strictly_between_the_top_and_the_bottom_of_the_line() {
        let mut font = Font::load_embedded();
        let metrics = font.measure(FontFamily::SansSerif, "Hg", 16.0, 400.0);
        assert!(
            metrics.baseline > 0.0 && metrics.baseline < metrics.height,
            "the baseline ({}) must fall strictly within the line's own height (0..{})",
            metrics.baseline,
            metrics.height
        );
    }

    #[test]
    fn baseline_does_not_depend_on_the_text_content() {
        // Purely a font-metric property (the ascent above the baseline) —
        // two different strings at the same family/size must report the
        // same baseline offset even though their widths differ.
        let mut font = Font::load_embedded();
        let short = font.measure(FontFamily::SansSerif, "x", 16.0, 400.0);
        let long = font.measure(FontFamily::SansSerif, "Testing Baseline", 16.0, 400.0);
        assert_eq!(short.baseline, long.baseline);
        assert_ne!(short.width, long.width, "sanity check: these really differ");
    }

    #[test]
    fn baseline_scales_with_font_size() {
        let mut font = Font::load_embedded();
        let small = font.measure(FontFamily::SansSerif, "Hg", 16.0, 400.0);
        let large = font.measure(FontFamily::SansSerif, "Hg", 32.0, 400.0);
        assert!(large.baseline > small.baseline);
    }

    #[test]
    fn baseline_is_the_same_whether_or_not_the_text_wraps() {
        // The ascent above the baseline comes from the font itself, not
        // from how many lines the content wrapped into — the first line's
        // baseline offset from the very top of the block must be identical
        // either way.
        let mut font = Font::load_embedded();
        let unwrapped = font.measure(FontFamily::SansSerif, "one two three", 16.0, 400.0);
        let one_word = font.measure(FontFamily::SansSerif, "one", 16.0, 400.0);
        let wrapped = font.measure_wrapped(
            FontFamily::SansSerif,
            "one two three",
            16.0,
            400.0,
            one_word.width,
        );
        assert!(
            wrapped.height > unwrapped.height,
            "sanity check: this must actually have wrapped onto more than one line"
        );
        assert_eq!(unwrapped.baseline, wrapped.baseline);
    }

    #[test]
    fn shape_inline_of_pure_text_matches_plain_shape() {
        // A single text item with no boxes must behave exactly like the
        // plain (non-rich) shaping path — proves the rich-text builder
        // isn't secretly changing ordinary single-style shaping.
        let mut font = Font::load_embedded();
        let plain = font.shape(FontFamily::SansSerif, "Hi", 16.0, 400.0);
        let inline = font.shape_inline(
            &[InlineContent::Text {
                text: "Hi",
                font_size: 16.0,
                font_weight: 400.0,
                family: FontFamily::SansSerif,
            }],
            None,
        );
        assert_eq!(inline.runs.len(), plain.runs.len());
        assert_eq!(inline.runs[0].glyphs.len(), plain.runs[0].glyphs.len());
        assert_eq!(inline.width, plain.width);
        assert_eq!(inline.height, plain.height);
    }

    #[test]
    fn shape_inline_wraps_a_text_run_around_a_box_that_does_not_fit_on_the_same_line() {
        let mut font = Font::load_embedded();
        let word = font.measure(FontFamily::SansSerif, "Hello", 16.0, 400.0);

        // A box wide enough that "Hello" plus the box can't share a line at
        // this width — must push the box (and nothing else, one item) onto
        // its own line, growing the total height past one line.
        let unconstrained = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hello ",
                    font_size: 16.0,
                    font_weight: 400.0,
                    family: FontFamily::SansSerif,
                },
                InlineContent::Box {
                    id: 1,
                    width: word.width,
                    height: 30.0,
                },
            ],
            None,
        );
        let wrapped = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hello ",
                    font_size: 16.0,
                    font_weight: 400.0,
                    family: FontFamily::SansSerif,
                },
                InlineContent::Box {
                    id: 1,
                    width: word.width,
                    height: 30.0,
                },
            ],
            Some(word.width + 1.0),
        );

        assert!(
            wrapped.height > unconstrained.height,
            "the box must have wrapped onto its own line: unconstrained height {}, \
             wrapped height {}",
            unconstrained.height,
            wrapped.height
        );
        assert_eq!(wrapped.boxes.len(), 1);
    }

    #[test]
    fn shape_inline_reports_a_box_position_relative_to_the_block_origin() {
        let mut font = Font::load_embedded();
        let inline = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hi ",
                    font_size: 16.0,
                    font_weight: 400.0,
                    family: FontFamily::SansSerif,
                },
                InlineContent::Box {
                    id: 42,
                    width: 30.0,
                    height: 20.0,
                },
            ],
            None,
        );

        assert_eq!(inline.boxes.len(), 1);
        let positioned = &inline.boxes[0];
        assert_eq!(positioned.id, 42, "the caller's own id is echoed back");
        assert!(
            positioned.x > 0.0,
            "the box must sit after \"Hi \", not at the origin"
        );
    }

    #[test]
    fn shape_inline_multiple_boxes_keep_their_own_distinct_ids() {
        let mut font = Font::load_embedded();
        let inline = font.shape_inline(
            &[
                InlineContent::Box {
                    id: 1,
                    width: 10.0,
                    height: 10.0,
                },
                InlineContent::Box {
                    id: 2,
                    width: 10.0,
                    height: 10.0,
                },
            ],
            None,
        );

        assert_eq!(inline.boxes.len(), 2);
        let ids: Vec<u64> = inline.boxes.iter().map(|b| b.id).collect();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert_ne!(
            inline.boxes[0].x, inline.boxes[1].x,
            "two adjacent boxes must not land at the same x"
        );
    }

    #[test]
    fn shape_inline_line_height_is_driven_by_the_tallest_run_on_the_line() {
        let mut font = Font::load_embedded();
        let small_only = font.shape_inline(
            &[InlineContent::Text {
                text: "Hg",
                font_size: 16.0,
                font_weight: 400.0,
                family: FontFamily::SansSerif,
            }],
            None,
        );
        let mixed = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hg ",
                    font_size: 16.0,
                    font_weight: 400.0,
                    family: FontFamily::SansSerif,
                },
                InlineContent::Text {
                    text: "Hg",
                    font_size: 48.0,
                    font_weight: 400.0,
                    family: FontFamily::SansSerif,
                },
            ],
            None,
        );
        assert!(
            mixed.height > small_only.height,
            "a line with a much larger run mixed in must be at least as tall as that \
             run, taller than the small-only line"
        );
    }

    #[test]
    fn shape_inline_with_no_items_measures_to_zero() {
        let mut font = Font::load_embedded();
        let inline = font.shape_inline(&[], None);
        assert_eq!(inline.width, 0.0);
        assert_eq!(inline.height, 0.0);
        assert_eq!(inline.baseline, 0.0);
        assert!(inline.boxes.is_empty());
        assert!(inline.runs.is_empty());
    }

    #[test]
    fn shape_inline_with_only_a_box_and_no_text_still_positions_it() {
        let mut font = Font::load_embedded();
        let inline = font.shape_inline(
            &[InlineContent::Box {
                id: 7,
                width: 40.0,
                height: 25.0,
            }],
            None,
        );
        assert_eq!(inline.boxes.len(), 1);
        assert_eq!(inline.boxes[0].id, 7);
        assert!(inline.width >= 40.0);
    }

    #[test]
    fn measure_cached_first_call_matches_measure_wrapped() {
        let mut font = Font::load_embedded();
        let text = "one two three four five six seven eight nine ten";
        let expected = font.measure_wrapped(FontFamily::SansSerif, text, 16.0, 400.0, 120.0);

        let mut cache = None;
        let cached = font.measure_cached(
            &mut cache,
            FontFamily::SansSerif,
            text,
            16.0,
            400.0,
            Some(120.0),
        );
        assert_eq!(cached, expected);
        assert!(
            cache.is_some(),
            "a non-empty measurement must populate the cache for reuse"
        );
    }

    #[test]
    fn measure_cached_reuses_the_same_cache_across_different_widths() {
        // The real point of `measure_cached`: re-breaking the same built
        // layout at a new width must produce the identical result a fresh
        // `measure_wrapped` at that width would — proving the cached path
        // doesn't silently keep stale line-break state from the first call.
        let mut font = Font::load_embedded();
        let text = "one two three four five six seven eight nine ten";
        let mut cache = None;
        let first = font.measure_cached(
            &mut cache,
            FontFamily::SansSerif,
            text,
            16.0,
            400.0,
            Some(400.0),
        );
        let second = font.measure_cached(
            &mut cache,
            FontFamily::SansSerif,
            text,
            16.0,
            400.0,
            Some(80.0),
        );
        assert!(
            second.height > first.height,
            "wrapping the same text at a much narrower width must take more lines"
        );

        let expected_at_80 = font.measure_wrapped(FontFamily::SansSerif, text, 16.0, 400.0, 80.0);
        assert_eq!(
            second, expected_at_80,
            "re-breaking a cached layout at 80px must match a fresh measurement at 80px"
        );

        let back_to_400 = font.measure_cached(
            &mut cache,
            FontFamily::SansSerif,
            text,
            16.0,
            400.0,
            Some(400.0),
        );
        assert_eq!(
            back_to_400, first,
            "re-breaking back to the original width must reproduce the original result, \
             not something drifted by the intermediate re-break"
        );
    }

    #[test]
    fn measure_cached_with_no_max_width_matches_unwrapped_measure() {
        let mut font = Font::load_embedded();
        let text = "Hello, Florui";
        let expected = font.measure(FontFamily::SansSerif, text, 16.0, 400.0);

        let mut cache = None;
        let cached =
            font.measure_cached(&mut cache, FontFamily::SansSerif, text, 16.0, 400.0, None);
        assert_eq!(cached, expected);
    }

    #[test]
    fn measure_cached_with_empty_text_clears_the_cache_and_measures_zero() {
        let mut font = Font::load_embedded();
        let mut cache = None;
        let metrics = font.measure_cached(
            &mut cache,
            FontFamily::SansSerif,
            "",
            16.0,
            400.0,
            Some(100.0),
        );
        assert_eq!(metrics.width, 0.0);
        assert_eq!(metrics.height, 0.0);
        assert_eq!(metrics.baseline, 0.0);
        assert!(cache.is_none());
    }
}
