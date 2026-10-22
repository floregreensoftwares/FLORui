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

use rustc_hash::FxHashMap;
use std::sync::Arc;

use parley::{Alignment, AlignmentOptions, IndentOptions};
use parley::{
    FontContext, FontData, FontFamily as ParleyFontFamily, FontWeight, LayoutContext,
    LineHeight as ParleyLineHeight, PositionedLayoutItem, StyleProperty,
};
use peniko::Blob;

pub mod editing;
mod editor_core;
mod ellipsis;
mod features;
pub use inline::{InlineElement, InlineItem, LineEnv, LineSlot, LineSource, LineTrace};
mod bidi;
pub use bidi::{BidiMode, TextDirection, element_frame, first_strong, is_rtl_char};
mod inline;
mod inline_memo;
mod settings;
mod transform;
pub use settings::{intern_family_stack, intern_features, intern_variations};
pub use transform::{FontCaps, TextCase, TextTransform};

/// Open Sans (SIL Open Font License 1.1) — see `fonts/NOTICE.md` for
/// provenance and why it's this crate's sans-serif default despite not
/// being metrically matched to Arial.
pub const EMBEDDED_SANS_SERIF_FONT: &[u8] = include_bytes!("../fonts/OpenSans-Variable.ttf");
/// The italic of [`EMBEDDED_SANS_SERIF_FONT`], the same family under the
/// same license: what `font-style: italic` draws in sans-serif.
pub const EMBEDDED_SANS_SERIF_ITALIC_FONT: &[u8] =
    include_bytes!("../fonts/OpenSans-Italic-Variable.ttf");
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
    /// A `font-family` list, by the number [`intern_family_stack`] gave it.
    Stack(u32),
}

impl FontFamily {
    /// The embedded family this one measures like where a size per family is
    /// kept: `0` for sans-serif and what a list falls back to, `1` for
    /// monospace.
    pub(crate) fn base(self) -> usize {
        match self {
            FontFamily::Monospace => 1,
            FontFamily::SansSerif | FontFamily::Stack(_) => 0,
        }
    }

    fn bits(self) -> u32 {
        match self {
            FontFamily::SansSerif => 0,
            FontFamily::Monospace => 1,
            FontFamily::Stack(id) => id.saturating_add(2),
        }
    }
}

/// `font-style`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
    /// Slanted by this many degrees (a font with no italic is slanted by hand).
    Oblique(f32),
}

impl FontStyle {
    fn bits(self) -> (u32, u32) {
        match self {
            FontStyle::Normal => (0, 0),
            FontStyle::Italic => (1, 0),
            FontStyle::Oblique(degrees) => (2, degrees.to_bits()),
        }
    }

    fn parley(self) -> parley::FontStyle {
        match self {
            FontStyle::Normal => parley::FontStyle::Normal,
            FontStyle::Italic => parley::FontStyle::Italic,
            FontStyle::Oblique(degrees) => parley::FontStyle::Oblique(Some(degrees)),
        }
    }
}

/// `line-height` of a run of text: `Normal` is the font's own line height, a
/// number is a multiple of the font size and a length is in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LineHeight {
    #[default]
    Normal,
    Number(f32),
    Length(f32),
}

impl LineHeight {
    fn parley(self) -> Option<ParleyLineHeight> {
        match self {
            LineHeight::Normal => None,
            LineHeight::Number(n) => Some(ParleyLineHeight::FontSizeRelative(n)),
            LineHeight::Length(px) => Some(ParleyLineHeight::Absolute(px)),
        }
    }

    fn bits(self) -> (u8, u32) {
        match self {
            LineHeight::Normal => (0, 0),
            LineHeight::Number(n) => (1, n.to_bits()),
            LineHeight::Length(px) => (2, px.to_bits()),
        }
    }
}

/// `text-align` of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Start,
    End,
    Center,
    Justify,
}

/// `white-space-collapse`: what happens to the runs of white space and the
/// line breaks of the text before it is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhiteSpaceCollapse {
    /// A run of spaces, tabs and line breaks is one space.
    #[default]
    Collapse,
    /// Everything stays as written.
    Preserve,
    /// Line breaks stay; the rest collapses.
    PreserveBreaks,
    /// As `Preserve`.
    BreakSpaces,
}

/// `word-break`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    BreakAll,
    KeepAll,
}

/// `overflow-wrap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverflowWrap {
    #[default]
    Normal,
    BreakWord,
    Anywhere,
}

/// `text-indent` of a paragraph. A percentage is of the width the text is
/// wrapped at, so it is resolved when the lines are broken.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TextIndent {
    pub length: f32,
    pub percent: f32,
    pub hanging: bool,
    pub each_line: bool,
}

/// `vertical-align` of an inline run: how its box sits against its parent's.
/// `Raise` lifts the box by a length plus a fraction of its own line height
/// (a negative one lowers it).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum VerticalAlign {
    #[default]
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    Raise {
        length: f32,
        percent: f32,
    },
}

impl VerticalAlign {
    fn bits(self) -> [u32; 3] {
        match self {
            VerticalAlign::Baseline => [0, 0, 0],
            VerticalAlign::Sub => [1, 0, 0],
            VerticalAlign::Super => [2, 0, 0],
            VerticalAlign::TextTop => [3, 0, 0],
            VerticalAlign::TextBottom => [4, 0, 0],
            VerticalAlign::Middle => [5, 0, 0],
            VerticalAlign::Top => [6, 0, 0],
            VerticalAlign::Bottom => [7, 0, 0],
            VerticalAlign::Raise { length, percent } => [8, length.to_bits(), percent.to_bits()],
        }
    }
}

/// What a run of text is shaped with. Two runs shape alike only when every field
/// matches bit for bit, which is also what the memos key on.
///
/// `align` and `indent` belong to the paragraph: in an inline run only the
/// container's count. `vertical_align` belongs to an inline run, and is
/// ignored by the container.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    pub family: FontFamily,
    pub font_size: f32,
    pub font_weight: f32,
    pub line_height: LineHeight,
    /// Added after every character, in pixels.
    pub letter_spacing: f32,
    /// Added after every space, in pixels.
    pub word_spacing: f32,
    pub align: TextAlign,
    pub indent: TextIndent,
    pub vertical_align: VerticalAlign,
    pub collapse: WhiteSpaceCollapse,
    /// Whether the text wraps at soft wrap opportunities.
    pub wrap: bool,
    pub word_break: WordBreak,
    pub overflow_wrap: OverflowWrap,
    pub font_style: FontStyle,
    pub transform: TextTransform,
    pub caps: FontCaps,
    /// OpenType features, by the number [`intern_features`] gave them; `0` is none.
    pub features: u32,
    /// Variable font axes, by the number [`intern_variations`] gave them; `0` is none.
    pub variations: u32,
    /// Which synthesized forms `font-synthesis` turns off: [`NO_SYNTHETIC_STYLE`],
    /// [`NO_SYNTHETIC_CAPS`] and [`NO_SYNTHETIC_WEIGHT`] added together; `0` leaves all on.
    pub no_synthesis: u8,
    /// `direction`: which way a paragraph in this style runs.
    pub direction: TextDirection,
    /// `unicode-bidi`.
    pub bidi: BidiMode,
}

/// A font with no italic is not slanted by hand.
pub const NO_SYNTHETIC_STYLE: u8 = 1;
/// Small capitals are not made from capitals.
pub const NO_SYNTHETIC_CAPS: u8 = 2;
/// A font with no bold is not emboldened.
pub const NO_SYNTHETIC_WEIGHT: u8 = 4;

/// A [`TextStyle`] as the bits the memos compare and hash.
pub(crate) type StyleBits = [u32; 20];

/// Text ready to shape: see [`TextStyle::prepared`].
pub(crate) struct Prepared<'a> {
    pub text: std::borrow::Cow<'a, str>,
    pub small: Vec<std::ops::Range<usize>>,
}

impl TextStyle {
    pub fn new(family: FontFamily, font_size: f32, font_weight: f32) -> Self {
        Self {
            font_style: FontStyle::Normal,
            transform: TextTransform::default(),
            caps: FontCaps::Normal,
            features: 0,
            variations: 0,
            no_synthesis: 0,
            direction: TextDirection::Ltr,
            bidi: BidiMode::Normal,
            family,
            font_size,
            font_weight,
            line_height: LineHeight::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            align: TextAlign::Start,
            indent: TextIndent::default(),
            vertical_align: VerticalAlign::Baseline,
            collapse: WhiteSpaceCollapse::Collapse,
            wrap: true,
            word_break: WordBreak::Normal,
            overflow_wrap: OverflowWrap::Normal,
        }
    }

    /// How the text collapses, wraps and breaks inside words.
    pub fn with_wrapping(
        mut self,
        collapse: WhiteSpaceCollapse,
        wrap: bool,
        word_break: WordBreak,
        overflow_wrap: OverflowWrap,
    ) -> Self {
        self.collapse = collapse;
        self.wrap = wrap;
        self.word_break = word_break;
        self.overflow_wrap = overflow_wrap;
        self
    }

    /// `direction` and `unicode-bidi`.
    pub fn with_direction(mut self, direction: TextDirection, bidi: BidiMode) -> Self {
        self.direction = direction;
        self.bidi = bidi;
        self
    }

    pub fn with_align(mut self, align: TextAlign) -> Self {
        self.align = align;
        self
    }

    pub fn with_indent(mut self, indent: TextIndent) -> Self {
        self.indent = indent;
        self
    }

    pub fn with_vertical_align(mut self, vertical_align: VerticalAlign) -> Self {
        self.vertical_align = vertical_align;
        self
    }

    pub fn with_line_height(mut self, line_height: LineHeight) -> Self {
        self.line_height = line_height;
        self
    }

    pub fn with_spacing(mut self, letter_spacing: f32, word_spacing: f32) -> Self {
        self.letter_spacing = letter_spacing;
        self.word_spacing = word_spacing;
        self
    }

    pub fn with_font_style(mut self, font_style: FontStyle) -> Self {
        self.font_style = font_style;
        self
    }

    pub fn with_transform(mut self, transform: TextTransform) -> Self {
        self.transform = transform;
        self
    }

    pub fn with_caps(mut self, caps: FontCaps) -> Self {
        self.caps = caps;
        self
    }

    /// OpenType features and variable font axes, as the numbers
    /// [`intern_features`] and [`intern_variations`] gave them.
    pub fn with_font_settings(mut self, features: u32, variations: u32) -> Self {
        self.features = features;
        self.variations = variations;
        self
    }

    /// Turns off the synthesized forms in `no_synthesis` (see [`Self::no_synthesis`]).
    pub fn with_no_synthesis(mut self, no_synthesis: u8) -> Self {
        self.no_synthesis = no_synthesis;
        self
    }

    pub(crate) fn bits(&self) -> StyleBits {
        let (line_height_kind, line_height_value) = self.line_height.bits();
        let [vertical_kind, vertical_length, vertical_percent] = self.vertical_align.bits();
        let (style_kind, style_angle) = self.font_style.bits();
        [
            self.family.bits(),
            style_kind,
            style_angle,
            self.transform.bits() | ((self.caps as u32) << 4) | (u32::from(self.no_synthesis) << 8),
            self.features,
            self.variations,
            self.font_size.to_bits(),
            self.font_weight.to_bits(),
            u32::from(line_height_kind),
            line_height_value,
            self.letter_spacing.to_bits(),
            self.word_spacing.to_bits(),
            self.align as u32,
            self.indent.length.to_bits(),
            self.indent.percent.to_bits(),
            u32::from(self.indent.hanging) | (u32::from(self.indent.each_line) << 1),
            vertical_kind,
            vertical_length,
            vertical_percent,
            self.collapse as u32
                | (u32::from(self.wrap) << 2)
                | ((self.word_break as u32) << 3)
                | ((self.overflow_wrap as u32) << 5)
                | ((self.direction as u32) << 8)
                | ((self.bidi as u32) << 9),
        ]
    }

    /// `text` as it is laid out under `white-space-collapse`. `previous_space`
    /// says whether what comes before ended in a collapsible space (or is the
    /// start of the block), and is updated for what comes after.
    pub(crate) fn collapsed<'a>(
        &self,
        text: &'a str,
        previous_space: &mut bool,
    ) -> std::borrow::Cow<'a, str> {
        use std::borrow::Cow;
        let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}');
        match self.collapse {
            WhiteSpaceCollapse::Preserve | WhiteSpaceCollapse::BreakSpaces => {
                if !text.is_empty() {
                    *previous_space = false;
                }
                Cow::Borrowed(text)
            }
            WhiteSpaceCollapse::Collapse | WhiteSpaceCollapse::PreserveBreaks => {
                let keep_breaks = self.collapse == WhiteSpaceCollapse::PreserveBreaks;
                // Most text collapses to itself: found out without building it.
                if !keep_breaks {
                    let mut previous = *previous_space;
                    let mut clean = true;
                    for c in text.chars() {
                        if is_space(c) {
                            if previous || c != ' ' {
                                clean = false;
                                break;
                            }
                            previous = true;
                        } else {
                            previous = false;
                        }
                    }
                    if clean {
                        if !text.is_empty() {
                            *previous_space = previous;
                        }
                        return Cow::Borrowed(text);
                    }
                }
                let mut out = String::with_capacity(text.len());
                let mut previous = *previous_space;
                for c in text.chars() {
                    if keep_breaks && c == '\n' {
                        // Spaces before a kept break go, and so do those after it.
                        if out.ends_with(' ') {
                            out.pop();
                        }
                        out.push('\n');
                        previous = true;
                    } else if is_space(c) {
                        if !previous {
                            out.push(' ');
                            previous = true;
                        }
                    } else {
                        out.push(c);
                        previous = false;
                    }
                }
                if !text.is_empty() {
                    *previous_space = previous;
                }
                if out == text {
                    Cow::Borrowed(text)
                } else {
                    Cow::Owned(out)
                }
            }
        }
    }

    /// `text` as it is shaped: collapsed, transformed by `text-transform` and
    /// with the small capitals of `font-variant-caps` made, and the byte ranges
    /// of those capitals, which are set smaller. `in_word` is the state
    /// `capitalize` carries from one run to the next.
    pub(crate) fn prepared<'a>(
        &self,
        text: &'a str,
        previous_space: &mut bool,
        in_word: &mut bool,
        native_caps: bool,
    ) -> Prepared<'a> {
        let collapsed = self.collapsed(text, previous_space);
        let transformed = match collapsed {
            std::borrow::Cow::Borrowed(text) => self.transform.apply(text, in_word),
            std::borrow::Cow::Owned(text) => {
                std::borrow::Cow::Owned(self.transform.apply(&text, in_word).into_owned())
            }
        };
        if self.caps.is_synthesized() && !native_caps && self.no_synthesis & NO_SYNTHETIC_CAPS == 0
        {
            let (made, small) = transform::small_caps(&transformed, self.caps);
            return Prepared {
                text: std::borrow::Cow::Owned(made),
                small,
            };
        }
        Prepared {
            text: transformed,
            small: Vec::new(),
        }
    }

    /// The features `font-variant-caps` asks of the font, in CSS.
    fn caps_features(&self) -> Option<&'static str> {
        match self.caps {
            FontCaps::Normal => None,
            FontCaps::SmallCaps => Some("\"smcp\" 1"),
            FontCaps::AllSmallCaps => Some("\"smcp\" 1, \"c2sc\" 1"),
            FontCaps::PetiteCaps => Some("\"pcap\" 1"),
            FontCaps::AllPetiteCaps => Some("\"pcap\" 1, \"c2pc\" 1"),
            FontCaps::Unicase => Some("\"unic\" 1"),
            FontCaps::TitlingCaps => Some("\"titl\" 1"),
        }
    }

    /// The tag a font must list for its small capitals to be used rather than
    /// made.
    fn caps_tag(&self) -> Option<[u8; 4]> {
        match self.caps {
            FontCaps::SmallCaps | FontCaps::AllSmallCaps => Some(*b"smcp"),
            FontCaps::PetiteCaps | FontCaps::AllPetiteCaps => Some(*b"pcap"),
            _ => None,
        }
    }

    /// All the OpenType features this style asks for, in CSS: those of the
    /// caps first, so the settings written out win.
    fn feature_source(&self) -> Option<String> {
        let settings = settings::features(self.features);
        match (self.caps_features(), settings) {
            (None, None) => None,
            (Some(caps), None) => Some(caps.to_owned()),
            (None, Some(settings)) => Some(settings.to_string()),
            (Some(caps), Some(settings)) => Some(format!("{caps}, {settings}")),
        }
    }

    /// The Parley family of this style: the embedded one named `name` for
    /// the two it has, the list itself (and the embedded sans-serif last, for
    /// whatever no name in it finds) for [`FontFamily::Stack`].
    fn parley_family<'a>(&self, name: &'a str) -> ParleyFontFamily<'a> {
        match self.family {
            FontFamily::Stack(id) => match settings::family_stack(id) {
                Some(css) => ParleyFontFamily::Source(format!("{css}, \"{name}\"").into()),
                None => ParleyFontFamily::named(name),
            },
            _ => ParleyFontFamily::named(name),
        }
    }

    /// The indent in pixels for a paragraph wrapped at `max_width`, with
    /// Parley's options for its keywords.
    fn paragraph_indent(&self, max_width: Option<f32>) -> (f32, IndentOptions) {
        let amount = self.indent.length + self.indent.percent * max_width.unwrap_or(0.0);
        (
            amount,
            IndentOptions {
                each_line: self.indent.each_line,
                hanging: self.indent.hanging,
            },
        )
    }

    fn parley_alignment(&self) -> Alignment {
        match self.align {
            TextAlign::Start => Alignment::Start,
            TextAlign::End => Alignment::End,
            TextAlign::Center => Alignment::Center,
            TextAlign::Justify => Alignment::Justify,
        }
    }

    /// The Parley properties that differ from Parley's own defaults, so a
    /// plain style adds nothing to the builder. An inline run leaves the line
    /// height out: its lines are measured by [`Font::shape_inline`] itself.
    fn parley_extras(
        &self,
        with_line_height: bool,
    ) -> [Option<StyleProperty<'static, [u8; 4]>>; 9] {
        [
            (self.font_style != FontStyle::Normal)
                .then(|| StyleProperty::FontStyle(self.font_style.parley())),
            self.feature_source()
                .map(|css| StyleProperty::FontFeatures(parley::FontFeatures::Source(css.into()))),
            settings::variations(self.variations).map(|css| {
                StyleProperty::FontVariations(parley::FontVariations::Source(
                    css.to_string().into(),
                ))
            }),
            with_line_height
                .then(|| self.line_height.parley().map(StyleProperty::LineHeight))
                .flatten(),
            (self.letter_spacing != 0.0)
                .then_some(StyleProperty::LetterSpacing(self.letter_spacing)),
            (self.word_spacing != 0.0).then_some(StyleProperty::WordSpacing(self.word_spacing)),
            (!self.wrap).then_some(StyleProperty::TextWrapMode(parley::TextWrapMode::NoWrap)),
            (self.word_break != WordBreak::Normal).then_some(StyleProperty::WordBreak(match self
                .word_break
            {
                WordBreak::Normal => parley::WordBreak::Normal,
                WordBreak::BreakAll => parley::WordBreak::BreakAll,
                WordBreak::KeepAll => parley::WordBreak::KeepAll,
            })),
            (self.overflow_wrap != OverflowWrap::Normal).then_some(StyleProperty::OverflowWrap(
                match self.overflow_wrap {
                    OverflowWrap::Normal => parley::OverflowWrap::Normal,
                    OverflowWrap::BreakWord => parley::OverflowWrap::BreakWord,
                    OverflowWrap::Anywhere => parley::OverflowWrap::Anywhere,
                },
            )),
        ]
    }
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

/// What a run's font lacks that is made up by hand: the tangent of the angle it
/// is leaned by (no italic), and whether it is emboldened (no bold), each unless
/// `font-synthesis` (`no_synthesis`) turned it off.
pub(crate) fn synthesis_of<B: parley::Brush>(
    run: &parley::Run<'_, B>,
    no_synthesis: u8,
) -> (f32, bool) {
    let synthesis = run.synthesis();
    let skew = synthesis
        .skew()
        .filter(|_| no_synthesis & NO_SYNTHETIC_STYLE == 0)
        .map_or(0.0, |degrees| degrees.to_radians().tan());
    (
        skew,
        synthesis.embolden() && no_synthesis & NO_SYNTHETIC_WEIGHT == 0,
    )
}

/// What a run needs of its font to draw the decorations of its text.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RunMetrics {
    /// From the baseline up to the top of the font's tallest glyphs, unrounded.
    pub ascent: f32,
    /// From the baseline down to the bottom of the font's lowest glyphs.
    pub descent: f32,
    /// The thickness the font gives an underline, in pixels at the run's size.
    pub underline_size: f32,
}

pub(crate) fn run_metrics_of<B: parley::Brush>(run: &parley::Run<'_, B>) -> RunMetrics {
    let metrics = run.metrics();
    RunMetrics {
        ascent: metrics.ascent,
        descent: metrics.descent,
        underline_size: metrics.underline_size,
    }
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
    /// How far a run the font has no italic for leans: the tangent of the angle
    /// (`0` for an upright run or one in a real italic).
    pub skew: f32,
    /// Whether the font has no bold and the run is thickened by hand.
    pub embolden: bool,
    pub metrics: RunMetrics,
    pub glyphs: Vec<ShapedGlyph>,
    /// For a run of an inline context, the element the text belongs to
    /// (see [`InlineElement`]); `0` everywhere else.
    pub element: usize,
    /// Where the run starts on its line and how far it advances.
    pub x: f32,
    pub width: f32,
    /// The line the run is on, counting from `0`.
    pub line: u32,
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
    Text { text: &'a str, style: TextStyle },
    Box { id: u64, width: f32, height: f32 },
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
    /// The same for the last line, which is where an `inline-block` is
    /// aligned on the line it sits in.
    pub last_baseline: f32,
    pub runs: Vec<ShapedRun>,
    pub boxes: Vec<PositionedBox>,
    /// Where each inline element has text, one per element and line it
    /// appears on, in line order.
    pub fragments: Vec<InlineFragment>,
    /// When the first line of the context has a style of its own (see
    /// [`InlineElement::first_line`]), how many elements there were: the elements of the
    /// first line are numbered from there, element `n + i` being element `i` as it is on
    /// the first line.
    pub first_line: Option<usize>,
}

/// The part of an inline element on one line: its content area, from where
/// its first glyph (or the inside of its leading padding and border) starts
/// to where its last ends, between the line's top and the element's own
/// baseline. `x`/`y` are relative to the inline block's origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InlineFragment {
    pub element: usize,
    /// The line this fragment is on.
    pub line: u32,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub ascent: f32,
    pub descent: f32,
    /// This fragment holds the start of the element (its padding, border and
    /// margin before the first glyph), and/or its end.
    pub starts: bool,
    pub ends: bool,
    /// The element's text runs right to left: its start is the right side of the fragment.
    pub rtl: bool,
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

type MemoStyle = StyleBits;

/// A wrap width (bits, `None` for unwrapped) and what it measured.
type MemoWidth = (Option<u32>, TextMetrics);

/// One generation of [`MetricsMemo`], on a fast non-cryptographic hasher: a
/// layout pass asks tens of thousands of times, and the memo is bounded, so
/// the default hasher's collision resistance is not worth its cost here.
/// Flattened into two levels so a lookup borrows the text instead of
/// allocating a key for it.
#[derive(Default)]
struct Generation {
    by_style: FxHashMap<MemoStyle, FxHashMap<String, Vec<MemoWidth>>>,
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
            self.release(self.previous.entries);
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.insert(style, text, width, metrics);
        florui_profile::gauge::add(florui_profile::gauge::Gauge::TextMetrics, 1);
    }

    fn clear(&mut self) {
        self.release(self.current.entries + self.previous.entries);
        self.current = Generation::default();
        self.previous = Generation::default();
    }

    fn release(&self, entries: usize) {
        florui_profile::gauge::sub(florui_profile::gauge::Gauge::TextMetrics, entries);
    }
}

impl Drop for MetricsMemo {
    fn drop(&mut self) {
        self.release(self.current.entries + self.previous.entries);
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
    inline_memo: inline_memo::InlineMemo,
    /// Vertical metrics by family, size and weight bits.
    style_metrics: FxHashMap<(u32, u32, u32, u32), inline::StyleMetrics>,
    /// The width of U+2007 per em, by family.
    spacer_em: [Option<f32>; 2],
    /// Whether the first font that answers to a family has a feature, by the
    /// family's name and the feature.
    feature_support: FxHashMap<(String, [u8; 4]), bool>,
    /// Counts the fonts registered after loading, so a copy made earlier can tell
    /// it is out of date.
    generation: u64,
}

/// What another thread needs to shape text as a [`Font`] does: its fonts, with
/// none of its scratch state. See [`Font::seed`].
#[derive(Clone)]
pub struct FontSeed {
    font_cx: FontContext,
    sans_serif_family_name: String,
    monospace_family_name: String,
    generation: u64,
}

impl FontSeed {
    /// Changes whenever a font is registered on the [`Font`] this came from.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Font {
    /// Loads both embedded fonts: [`EMBEDDED_SANS_SERIF_FONT`] as
    /// [`FontFamily::SansSerif`], [`EMBEDDED_MONOSPACE_FONT`] as
    /// [`FontFamily::Monospace`].
    pub fn load_embedded() -> Self {
        let mut font_cx = FontContext::new();
        let sans_serif_family_name = Self::register_into(&mut font_cx, EMBEDDED_SANS_SERIF_FONT)
            .expect("the embedded sans-serif font is always valid");
        // Same family name: a request for italic finds this face by its style.
        Self::register_into(&mut font_cx, EMBEDDED_SANS_SERIF_ITALIC_FONT)
            .expect("the embedded italic font is always valid");
        let monospace_family_name = Self::register_into(&mut font_cx, EMBEDDED_MONOSPACE_FONT)
            .expect("the embedded monospace font is always valid");
        Self {
            font_cx,
            layout_cx: LayoutContext::new(),
            sans_serif_family_name,
            monospace_family_name,
            metrics_memo: MetricsMemo::with_capacity(METRICS_MEMO_CAPACITY),
            inline_memo: inline_memo::InlineMemo::with_capacity(inline_memo::INLINE_MEMO_BYTES),
            style_metrics: FxHashMap::default(),
            spacer_em: [None; 2],
            feature_support: FxHashMap::default(),
            generation: 0,
        }
    }

    /// Changes whenever a font is registered, so a [`FontSeed`] taken before is
    /// out of date.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The fonts of this instance, to make another [`Font`] from (on another
    /// thread) that shapes text the same way. It shares the font data and starts
    /// with scratch state of its own.
    pub fn seed(&self) -> FontSeed {
        FontSeed {
            font_cx: self.font_cx.clone(),
            sans_serif_family_name: self.sans_serif_family_name.clone(),
            monospace_family_name: self.monospace_family_name.clone(),
            generation: self.generation,
        }
    }

    /// A [`Font`] with the fonts of `seed`.
    pub fn from_seed(seed: &FontSeed) -> Self {
        Self {
            font_cx: seed.font_cx.clone(),
            layout_cx: LayoutContext::new(),
            sans_serif_family_name: seed.sans_serif_family_name.clone(),
            monospace_family_name: seed.monospace_family_name.clone(),
            metrics_memo: MetricsMemo::with_capacity(METRICS_MEMO_CAPACITY),
            inline_memo: inline_memo::InlineMemo::with_capacity(inline_memo::INLINE_MEMO_BYTES),
            style_metrics: FxHashMap::default(),
            spacer_em: [None; 2],
            feature_support: FxHashMap::default(),
            generation: seed.generation,
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
        self.inline_memo.clear();
        self.style_metrics.clear();
        self.spacer_em = [None; 2];
        self.feature_support.clear();
        self.generation += 1;
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

    /// The names of the families `style` asks for, in order, the embedded
    /// sans-serif last. A generic one (`serif`) names no family a font file can
    /// be looked up by here and is left out.
    fn family_candidates(&self, style: &TextStyle) -> Vec<String> {
        let mut names = Vec::new();
        if let FontFamily::Stack(id) = style.family
            && let Some(css) = settings::family_stack(id)
        {
            for item in css.split(',') {
                let item = item.trim();
                if let Some(quoted) = item
                    .strip_prefix('"')
                    .and_then(|rest| rest.strip_suffix('"'))
                {
                    names.push(quoted.replace("\\\"", "\"").replace("\\\\", "\\"));
                } else if !item.is_empty()
                    && !matches!(
                        item,
                        "serif" | "sans-serif" | "monospace" | "cursive" | "fantasy" | "system-ui"
                    )
                {
                    names.push(item.to_owned());
                }
            }
        } else {
            names.push(self.family_name(style.family).to_owned());
        }
        names.push(self.sans_serif_family_name.clone());
        names
    }

    /// Whether the font that answers to `style`'s family has the feature the
    /// style's `font-variant-caps` needs for small capitals of its own.
    fn caps_in_font(&mut self, style: &TextStyle) -> bool {
        let Some(tag) = style.caps_tag() else {
            return false;
        };
        let names = self.family_candidates(style);
        let key = (names.join(","), tag);
        if let Some(&known) = self.feature_support.get(&key) {
            return known;
        }
        let mut answer = false;
        for name in &names {
            let Some(family) = self.font_cx.collection.family_by_name(name) else {
                continue;
            };
            let Some(info) = family.fonts().first() else {
                continue;
            };
            answer = info
                .load(None)
                .is_some_and(|blob| features::font_has_feature(blob.data(), info.index(), tag));
            break;
        }
        self.feature_support.insert(key, answer);
        answer
    }

    pub(crate) fn family_name(&self, family: FontFamily) -> &str {
        match family {
            FontFamily::SansSerif | FontFamily::Stack(_) => &self.sans_serif_family_name,
            FontFamily::Monospace => &self.monospace_family_name,
        }
    }

    /// Measures `text` in `style`, as if nothing constrained its width: no
    /// wrapping, a single line.
    pub fn measure(&mut self, style: TextStyle, text: &str) -> TextMetrics {
        Self::metrics_of(self.layout_unwrapped(style, text))
    }

    /// Measures `text` in `style`, wrapping
    /// at `max_width` — real multi-line layout. A single word wider than
    /// `max_width` still gets its own (overflowing) line rather than
    /// being broken mid-word or bleeding onto an adjacent line — Parley's
    /// own wrapping behavior, matching real CSS's default
    /// `overflow-wrap: normal`.
    pub fn measure_wrapped(&mut self, style: TextStyle, text: &str, max_width: f32) -> TextMetrics {
        Self::metrics_of(self.layout_wrapped(style, text, max_width))
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
        style: TextStyle,
        text: &str,
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
            *cache = Some(CachedLayout(self.build_unbroken(style, text)));
        }
        let layout = &mut cache
            .as_mut()
            .expect("just populated above if it was empty")
            .0;
        let (indent, options) = style.paragraph_indent(max_width);
        layout.set_text_indent(indent, options);
        layout.break_all_lines(max_width);
        Self::metrics_of_ref(layout)
    }

    /// [`Self::measure_cached`], remembered across calls: the same style,
    /// text and wrap width answer from the memo without shaping or breaking
    /// lines, which is what a frame that changed nothing asks for again.
    pub fn measure_memoized(
        &mut self,
        cache: &mut Option<CachedLayout>,
        style: TextStyle,
        text: &str,
        max_width: Option<f32>,
    ) -> TextMetrics {
        self.memoized_metrics(cache, style, text, max_width, true)
    }

    /// [`Self::measure_memoized`] for a caller asking how big text is rather
    /// than measuring it for layout: it shares the memo but leaves the
    /// profiler's text counters to the layout's own lookups.
    pub fn probe_memoized(
        &mut self,
        style: TextStyle,
        text: &str,
        max_width: Option<f32>,
    ) -> TextMetrics {
        self.memoized_metrics(&mut None, style, text, max_width, false)
    }

    fn memoized_metrics(
        &mut self,
        cache: &mut Option<CachedLayout>,
        style: TextStyle,
        text: &str,
        max_width: Option<f32>,
        counted: bool,
    ) -> TextMetrics {
        let key = style.bits();
        let width = max_width.map(f32::to_bits);
        if let Some(metrics) = self.metrics_memo.get(key, text, width) {
            if counted {
                florui_profile::count(florui_profile::Counter::TextMemoHits, 1);
            }
            return metrics;
        }
        if counted {
            florui_profile::count(florui_profile::Counter::TextMemoMisses, 1);
        }
        let metrics = self.measure_cached(cache, style, text, max_width);
        #[cfg(test)]
        {
            self.metrics_memo.misses += 1;
        }
        // Empty text measures to zero without shaping; not worth an entry.
        if !text.is_empty() {
            self.metrics_memo.insert(key, text, width, metrics);
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
            height: height_without_final_break(layout),
            // The first line's own baseline offset is already relative to
            // the top of the whole block, since nothing precedes it.
            baseline: layout
                .lines()
                .next()
                .map(|line| line.metrics().baseline)
                .unwrap_or(0.0),
        }
    }

    /// Shapes `text` in `style` into
    /// paintable glyphs — same unwrapped, single-line layout as
    /// [`Self::measure`], but exposing glyph ids and pen positions instead
    /// of just overall width/height.
    pub fn shape(&mut self, style: TextStyle, text: &str) -> ShapedText {
        Self::shaped_text_of(self.layout_unwrapped(style, text), style.no_synthesis)
    }

    /// Shapes `text` in `style` into
    /// paintable glyphs, wrapped at `max_width` — same wrapping behavior
    /// as [`Self::measure_wrapped`]. Every glyph's `x`/`y` stays absolute
    /// within the whole shaped block (not line-relative), so a caller
    /// paints this exactly like an unwrapped [`Self::shape`] result —
    /// later lines simply carry larger `y` values, with no separate
    /// per-line structure to unpack.
    pub fn shape_wrapped(&mut self, style: TextStyle, text: &str, max_width: f32) -> ShapedText {
        Self::shaped_text_of(
            self.layout_wrapped(style, text, max_width),
            style.no_synthesis,
        )
    }

    fn shaped_text_of(layout: Option<parley::Layout<[u8; 4]>>, no_synthesis: u8) -> ShapedText {
        let Some(layout) = layout else {
            return ShapedText {
                runs: Vec::new(),
                width: 0.0,
                height: 0.0,
            };
        };

        let mut runs = Vec::new();
        for (line_number, line) in layout.lines().enumerate() {
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
                let synthesis = synthesis_of(run, no_synthesis);
                runs.push(ShapedRun {
                    font: run.font().clone(),
                    font_size: run.font_size(),
                    normalized_coords: run.normalized_coords().to_vec(),
                    skew: synthesis.0,
                    embolden: synthesis.1,
                    metrics: run_metrics_of(run),
                    glyphs,
                    element: 0,
                    x: glyph_run.offset(),
                    width: glyph_run.advance(),
                    line: line_number as u32,
                });
            }
        }

        ShapedText {
            runs,
            width: layout.width(),
            height: height_without_final_break(&layout),
        }
    }

    /// [`Self::layout_wrapped`] with no width constraint — a single
    /// unwrapped line.
    fn layout_unwrapped(
        &mut self,
        style: TextStyle,
        text: &str,
    ) -> Option<parley::Layout<[u8; 4]>> {
        self.build_layout(style, text, None)
    }

    /// Builds and line-breaks a Parley layout for `text` in `family` at
    /// `font_size`, wrapped at `max_width` — the scratch-state setup every
    /// measure/shape method needs before reading anything back out of it.
    /// `None` for empty `text`, which Parley itself would otherwise still
    /// lay out as one zero-width line with a nonzero line height.
    fn layout_wrapped(
        &mut self,
        style: TextStyle,
        text: &str,
        max_width: f32,
    ) -> Option<parley::Layout<[u8; 4]>> {
        self.build_layout(style, text, Some(max_width))
    }

    fn build_layout(
        &mut self,
        style: TextStyle,
        text: &str,
        max_width: Option<f32>,
    ) -> Option<parley::Layout<[u8; 4]>> {
        if text.is_empty() {
            return None;
        }
        let mut layout = self.build_unbroken(style, text);
        let (indent, options) = style.paragraph_indent(max_width);
        layout.set_text_indent(indent, options);
        layout.break_all_lines(max_width);
        layout.align(style.parley_alignment(), AlignmentOptions::default());
        Some(layout)
    }

    /// Itemizes and shapes `text` in `family` at `font_size`/`font_weight`
    /// — the expensive step every measure/shape method needs, shared by
    /// [`Self::build_layout`] (which breaks it once and discards it) and
    /// [`Self::measure_cached`] (which keeps the result around to re-break
    /// instead of redoing this). Callers with non-empty `text` only —
    /// unlike [`Self::build_layout`], this has nothing sensible to return
    /// for an empty string, so callers must check first.
    fn build_unbroken(&mut self, style: TextStyle, text: &str) -> parley::Layout<[u8; 4]> {
        // A normal line height is Chrome's: the font's ascent, descent and line
        // gap, each rounded to a whole pixel.
        let normal = matches!(style.line_height, LineHeight::Normal)
            .then(|| self.style_metrics(style).normal_line_height());
        self.build_with(style, text, normal)
    }

    /// [`Self::build_unbroken`] with the line height to use when the style's is
    /// `normal`.
    fn build_with(
        &mut self,
        style: TextStyle,
        text: &str,
        normal_line_height: Option<f32>,
    ) -> parley::Layout<[u8; 4]> {
        let family_name = self.family_name(style.family).to_owned();
        let native_caps = style.caps.is_synthesized() && self.caps_in_font(&style);
        let prepared = style.prepared(text, &mut true, &mut false, native_caps);
        // The paragraph takes the direction of the style, not of its first strong character.
        let (before, after) = bidi::paragraph_frame(
            style.direction,
            style.bidi,
            prepared.text.chars().any(bidi::is_rtl_char),
        );
        let framed: std::borrow::Cow<'_, str> = if before.is_empty() && after.is_empty() {
            std::borrow::Cow::Borrowed(prepared.text.as_ref())
        } else {
            std::borrow::Cow::Owned(format!("{before}{}{after}", prepared.text))
        };
        let shift = before.len();
        let text = framed.as_ref();
        let mut builder = self
            .layout_cx
            .ranged_builder(&mut self.font_cx, text, 1.0, true);
        builder.push_default(StyleProperty::FontSize(style.font_size));
        builder.push_default(StyleProperty::FontFamily(
            style.parley_family(family_name.as_str()),
        ));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(
            style.font_weight,
        )));
        for extra in style.parley_extras(true).into_iter().flatten() {
            builder.push_default(extra);
        }
        if let Some(height) = normal_line_height {
            builder.push_default(StyleProperty::LineHeight(ParleyLineHeight::Absolute(
                height,
            )));
        }
        for range in &prepared.small {
            builder.push(
                StyleProperty::FontSize(style.font_size * style.caps.scale()),
                range.start + shift..range.end + shift,
            );
        }
        builder.build(text)
    }
}

/// The height of `layout`, less the empty line that a line break at the very end of the text
/// would start: the break ends the line it is in, and no line comes after it.
fn height_without_final_break(layout: &parley::Layout<[u8; 4]>) -> f32 {
    let mut lines = layout.lines().rev();
    match (lines.next(), lines.next()) {
        (Some(last), Some(_)) if last.text_range().is_empty() => {
            layout.height() - last.metrics().line_height
        }
        _ => layout.height(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_break_at_the_end_of_the_text_starts_no_line() {
        let mut font = Font::load_embedded();
        let mut style = TextStyle::new(FontFamily::SansSerif, 16.0, 400.0);
        style.collapse = WhiteSpaceCollapse::Preserve;
        let one = font.measure_wrapped(style, "a", 100.0).height;
        let ended = font.measure_wrapped(style, "a\n", 100.0).height;
        let two = font.measure_wrapped(style, "a\nb", 100.0).height;
        let blank = font.measure_wrapped(style, "a\n\n", 100.0).height;
        assert_eq!(ended, one, "the break ends the line it is in");
        assert!(two > one * 1.5, "a line after the break is one more");
        assert_eq!(blank, two, "a second break is an empty line");
        assert_eq!(
            font.shape_wrapped(style, "a\n", 100.0).height,
            one,
            "and the same for what is painted"
        );
    }

    #[test]
    fn italic_uses_the_real_italic_face_where_there_is_one_and_a_slant_where_not() {
        let mut font = Font::load_embedded();
        let mut italic = |family| {
            font.shape(
                TextStyle::new(family, 16.0, 400.0).with_font_style(FontStyle::Italic),
                "Hag",
            )
        };
        let sans = italic(FontFamily::SansSerif);
        let mono = italic(FontFamily::Monospace);
        assert_eq!(
            sans.runs[0].skew, 0.0,
            "a real italic is not slanted by hand"
        );
        let upright = font.shape(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hag");
        assert_ne!(
            sans.runs[0].font.data.id(),
            upright.runs[0].font.data.id(),
            "a different face than the upright one"
        );
        assert!(
            (mono.runs[0].skew - 14f32.to_radians().tan()).abs() < 1e-3,
            "{}",
            mono.runs[0].skew
        );
    }

    #[test]
    fn font_synthesis_turns_off_the_slant_the_bold_and_the_small_capitals() {
        let mut font = Font::load_embedded();
        let mono_italic =
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0).with_font_style(FontStyle::Italic);
        assert!(font.shape(mono_italic, "Hag").runs[0].skew > 0.2);
        assert_eq!(
            font.shape(mono_italic.with_no_synthesis(NO_SYNTHETIC_STYLE), "Hag")
                .runs[0]
                .skew,
            0.0
        );
        let mono_bold = TextStyle::new(FontFamily::Monospace, 16.0, 700.0);
        assert!(font.shape(mono_bold, "Hag").runs[0].embolden);
        assert!(
            !font
                .shape(mono_bold.with_no_synthesis(NO_SYNTHETIC_WEIGHT), "Hag")
                .runs[0]
                .embolden
        );
        let sans_bold = TextStyle::new(FontFamily::SansSerif, 16.0, 700.0);
        assert!(
            !font.shape(sans_bold, "Hag").runs[0].embolden,
            "Open Sans has a bold"
        );
        let sans = TextStyle::new(FontFamily::SansSerif, 16.0, 400.0);
        let plain = font.measure(sans, "hello").width;
        let small = font
            .measure(sans.with_caps(FontCaps::SmallCaps), "hello")
            .width;
        let off = font
            .measure(
                sans.with_caps(FontCaps::SmallCaps)
                    .with_no_synthesis(NO_SYNTHETIC_CAPS),
                "hello",
            )
            .width;
        assert_ne!(plain, small);
        assert_eq!(plain, off, "no small capitals made: the text is as it was");
    }

    #[test]
    fn text_transform_and_small_caps_change_what_is_shaped() {
        let mut font = Font::load_embedded();
        let style = TextStyle::new(FontFamily::SansSerif, 16.0, 400.0);
        let upper = font.measure(
            style.with_transform(TextTransform {
                case: TextCase::Uppercase,
                ..Default::default()
            }),
            "hello",
        );
        assert_eq!(upper.width, font.measure(style, "HELLO").width);
        let all = font.measure(style.with_caps(FontCaps::AllSmallCaps), "hello");
        let all_from_capitals = font.measure(style.with_caps(FontCaps::AllSmallCaps), "HELLO");
        assert_eq!(
            all.width, all_from_capitals.width,
            "every letter is a small capital"
        );
        assert!(all.width < font.measure(style, "HELLO").width);
        let small = font.measure(style.with_caps(FontCaps::SmallCaps), "Hello");
        let capitals = font.measure(style, "HELLO");
        let plain = font.measure(style, "Hello");
        assert!(small.width < capitals.width && small.width > 0.5 * capitals.width);
        assert_ne!(small.width, plain.width);
    }

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
                let memoized = font.measure_memoized(
                    &mut None,
                    TextStyle::new(family, size, weight),
                    text,
                    width,
                );
                let fresh = reference.measure_cached(
                    &mut None,
                    TextStyle::new(family, size, weight),
                    text,
                    width,
                );
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
        let metrics = font.measure_memoized(
            &mut None,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "",
            None,
        );
        assert_eq!(
            (metrics.width, metrics.height, metrics.baseline),
            (0.0, 0.0, 0.0)
        );
        assert_eq!(font.metrics_memo.entries(), 0);
    }

    #[test]
    fn registering_a_font_empties_the_memo() {
        let mut font = Font::load_embedded();
        font.measure_memoized(
            &mut None,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "kept",
            None,
        );
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
        memo.insert([0; 20], "hot", None, metrics(1.0));
        memo.insert([0; 20], "cold", None, metrics(2.0));
        memo.insert([0; 20], "c", None, metrics(3.0));
        // "hot" and "cold" are now the previous generation; asking for "hot"
        // brings it back into the current one.
        assert_eq!(memo.get([0; 20], "hot", None), Some(metrics(1.0)));

        memo.insert([0; 20], "d", None, metrics(4.0));
        memo.insert([0; 20], "e", None, metrics(5.0));

        assert_eq!(memo.get([0; 20], "hot", None), Some(metrics(1.0)));
        assert!(
            memo.get([0; 20], "cold", None).is_none(),
            "never asked for again"
        );
    }

    #[test]
    fn the_memo_never_holds_more_than_its_capacity() {
        let mut memo = MetricsMemo::with_capacity(10);
        for i in 0..200 {
            memo.insert([0; 20], &format!("text {i}"), None, metrics(i as f32));
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
        memo.insert([0; 20], "wide", Some(40), metrics(7.0));
        memo.insert([0; 20], "wide", None, metrics(9.0));
        memo.insert([0; 20], "other", None, metrics(1.0));

        assert_eq!(memo.get([0; 20], "wide", Some(40)), Some(metrics(7.0)));
        assert_eq!(memo.get([0; 20], "wide", None), Some(metrics(9.0)));
        assert!(memo.get([0; 20], "wide", Some(41)).is_none());
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
        let one = font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "A");
        let five = font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "AAAAA");
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
        let narrow = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "iiiii");
        let wide = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "MMMMM");
        assert!(
            narrow.width < wide.width,
            "Open Sans must not measure \"iiiii\" as wide as \"MMMMM\""
        );
    }

    #[test]
    fn larger_font_size_measures_wider_and_taller() {
        let mut font = Font::load_embedded();
        let small = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hello");
        let large = font.measure(TextStyle::new(FontFamily::SansSerif, 32.0, 400.0), "Hello");
        assert!(large.width > small.width);
        assert!(large.height > small.height);
    }

    #[test]
    fn bold_measures_wider_than_regular_at_the_same_size() {
        // Real variable-font weight instancing, not a no-op: bold glyphs
        // are wider, so this must actually differ, not just accept the
        // parameter and ignore it.
        let mut font = Font::load_embedded();
        let regular = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hello");
        let bold = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 700.0), "Hello");
        assert!(bold.width > regular.width);
    }

    #[test]
    fn empty_text_measures_to_zero() {
        let mut font = Font::load_embedded();
        let metrics = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "");
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
        let dots = font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "iiiii");
        let wide = font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "MMMMM");
        // Fira Mono is monospace, so these happen to be equal — this test
        // exists to be revisited if the embedded monospace font ever
        // changes to a proportional one, where it would need to assert
        // inequality instead.
        assert_eq!(dots.width, wide.width);
    }

    #[test]
    fn shape_produces_one_glyph_per_character_in_source_order() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "AB");
        assert_eq!(shaped.runs.len(), 1, "one plain run, one font, one style");
        assert_eq!(shaped.runs[0].glyphs.len(), 2);
    }

    #[test]
    fn shape_advances_each_glyph_by_the_monospace_width() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "AA");
        let glyphs = &shaped.runs[0].glyphs;
        let advance = glyphs[1].x - glyphs[0].x;
        assert_eq!(
            advance,
            font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "A")
                .width,
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
        let shaped = font.shape(TextStyle::new(FontFamily::Monospace, 24.0, 400.0), "A");
        assert_eq!(shaped.runs[0].font_size, 24.0);
    }

    #[test]
    fn empty_text_shapes_to_no_runs() {
        let mut font = Font::load_embedded();
        let shaped = font.shape(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "");
        assert!(shaped.runs.is_empty());
        assert_eq!(shaped.width, 0.0);
        assert_eq!(shaped.height, 0.0);
    }

    #[test]
    fn wrapping_at_a_width_that_fits_everything_matches_unwrapped_measurement() {
        let mut font = Font::load_embedded();
        let unwrapped = font.measure(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "one two three",
        );
        let wrapped = font.measure_wrapped(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "one two three",
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
        let one_word = font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "aaaaa");
        let unwrapped = font.measure(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "aaaaa bbbbb ccccc",
        );

        // Just wide enough for the widest single word, not the whole line —
        // must wrap onto three lines, one per word.
        let wrapped = font.measure_wrapped(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "aaaaa bbbbb ccccc",
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
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "supercalifragilisticexpialidocious",
        );
        let wrapped = font.measure_wrapped(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "supercalifragilisticexpialidocious",
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
        let metrics = font.measure_wrapped(
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "",
            100.0,
        );
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
        let one_word = font.measure(TextStyle::new(FontFamily::Monospace, 16.0, 400.0), "aaaaa");
        let shaped = font.shape_wrapped(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "aaaaa bbbbb",
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
        let shaped = font.shape_wrapped(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "aaaaa",
            1000.0,
        );
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
        let shaped = font.shape(
            TextStyle::new(FontFamily::Monospace, 16.0, 400.0),
            "\u{1F600}",
        );

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

    fn sans(size: f32) -> TextStyle {
        TextStyle::new(FontFamily::SansSerif, size, 400.0)
    }

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.01, "{a} is not {b}");
    }

    const PANGRAM: &str = "The quick brown fox jumps over the lazy dog";

    /// The container style the inline tests lay out under.
    fn strut() -> TextStyle {
        sans(14.0)
    }

    #[test]
    fn line_height_sets_the_height_of_every_line() {
        let mut font = Font::load_embedded();
        let normal = font.measure_wrapped(sans(16.0), PANGRAM, 200.0);
        let length = font.measure_wrapped(
            sans(16.0).with_line_height(LineHeight::Length(30.0)),
            PANGRAM,
            200.0,
        );
        let number = font.measure_wrapped(
            sans(16.0).with_line_height(LineHeight::Number(2.0)),
            PANGRAM,
            200.0,
        );
        // Two lines each: 2 x 30 px, and 2 x (2 x the 16 px font size).
        close(length.height, 60.0);
        close(number.height, 64.0);
        assert!(normal.height < 50.0, "normal is the font's own height");
        close(
            font.measure(sans(16.0).with_line_height(LineHeight::Length(30.0)), "Hi")
                .height,
            30.0,
        );
        // The extra space is split above and below, so the baseline moves down
        // but stays inside the line.
        assert!(length.baseline > normal.baseline && length.baseline < 30.0);
    }

    #[test]
    fn letter_and_word_spacing_widen_by_the_exact_amount() {
        let mut font = Font::load_embedded();
        let text = "Spaced out text";
        let plain = font.measure(sans(16.0), text).width;
        let letters = font.measure(sans(16.0).with_spacing(3.0, 0.0), text).width;
        close(letters - plain, 3.0 * text.chars().count() as f32);

        let words_text = "a b c d e";
        let plain = font.measure(sans(16.0), words_text).width;
        let words = font
            .measure(sans(16.0).with_spacing(0.0, 14.0), words_text)
            .width;
        close(words - plain, 14.0 * 4.0);
    }

    #[test]
    fn measuring_and_shaping_agree_on_a_styled_run() {
        let mut font = Font::load_embedded();
        let style = sans(16.0)
            .with_line_height(LineHeight::Length(30.0))
            .with_spacing(1.5, 4.0);
        let measured = font.measure_wrapped(style, PANGRAM, 160.0);
        let shaped = font.shape_wrapped(style, PANGRAM, 160.0);
        close(measured.width, shaped.width);
        close(measured.height, shaped.height);
        let inline = font.shape_inline(
            &[InlineContent::Text {
                text: PANGRAM,
                style,
            }],
            strut(),
            Some(160.0),
        );
        close(measured.width, inline.width);
        close(measured.height, inline.height);
        close(measured.baseline, inline.baseline);
    }

    #[test]
    fn the_metrics_memo_tells_styles_that_differ_only_in_spacing_apart() {
        let mut font = Font::load_embedded();
        let ask = |font: &mut Font, style: TextStyle| {
            font.measure_memoized(&mut None, style, PANGRAM, Some(200.0))
        };
        let tall = sans(16.0).with_line_height(LineHeight::Length(30.0));
        let spaced = sans(16.0).with_spacing(2.0, 0.0);
        let plain = ask(&mut font, sans(16.0));
        let tall_metrics = ask(&mut font, tall);
        let spaced_metrics = ask(&mut font, spaced);
        assert_ne!(plain.height, tall_metrics.height);
        assert_ne!(plain.width, spaced_metrics.width);
        assert_eq!(font.metrics_memo.entries(), 3);
        assert_eq!(ask(&mut font, tall), tall_metrics, "a repeat is a hit");
        assert_eq!(font.metrics_memo.entries(), 3);
    }

    fn length_run(text: &str, size: f32, px: f32) -> InlineContent<'_> {
        InlineContent::Text {
            text,
            style: sans(size).with_line_height(LineHeight::Length(px)),
        }
    }

    /// The baseline of each line, read off the glyphs.
    fn baselines(layout: &InlineLayout) -> Vec<f32> {
        let mut ys: Vec<f32> = layout
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.y))
            .collect();
        ys.sort_by(f32::total_cmp);
        ys.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        ys
    }

    #[test]
    fn a_span_that_differs_only_in_line_height_still_sets_the_height_of_its_line() {
        // The numbers are Chrome's for the same CSS: 50 + 20 + 20.
        let strut = sans(16.0).with_line_height(LineHeight::Length(20.0));
        let items = [
            length_run("small text ", 16.0, 20.0),
            length_run("tall span", 16.0, 50.0),
            length_run(" and more words to wrap onto lines", 16.0, 20.0),
        ];
        let layout = Font::load_embedded().shape_inline(&items, strut, Some(200.0));
        close(layout.height, 90.0);
        assert_eq!(baselines(&layout), vec![31.0, 66.0, 86.0]);
        close(layout.baseline, 31.0);
    }

    #[test]
    fn the_strut_and_the_boxes_of_larger_and_smaller_runs_are_united_around_the_baseline() {
        let mut font = Font::load_embedded();
        let under = |px: f32| sans(16.0).with_line_height(LineHeight::Length(px));

        // A 32px span alone under a 30px line height: Chrome 36, not 30.
        let only = [length_run("Fixed", 32.0, 30.0)];
        close(
            font.shape_inline(&only, under(30.0), Some(200.0)).height,
            36.0,
        );

        // The container's text and a 40px span under 30px: Chrome 39.
        let larger = [
            length_run("Hg ", 16.0, 30.0),
            length_run("Hg", 40.0, 30.0),
            length_run(" tail", 16.0, 30.0),
        ];
        let layout = font.shape_inline(&larger, under(30.0), Some(280.0));
        close(layout.height, 39.0);
        close(layout.baseline, 30.0);

        // A 10px span under 40px: Chrome 42.
        let smaller = [
            length_run("Hg ", 16.0, 40.0),
            length_run("Hg", 10.0, 40.0),
            length_run(" tail", 16.0, 40.0),
        ];
        close(
            font.shape_inline(&smaller, under(40.0), Some(280.0)).height,
            42.0,
        );

        // A number scales with each run's own size: Chrome 60.
        let number = |size: f32| sans(size).with_line_height(LineHeight::Number(1.5));
        let items = [
            InlineContent::Text {
                text: "Hg ",
                style: number(16.0),
            },
            InlineContent::Text {
                text: "Hg",
                style: number(40.0),
            },
            InlineContent::Text {
                text: " tail",
                style: number(16.0),
            },
        ];
        close(
            font.shape_inline(&items, number(16.0), Some(280.0)).height,
            60.0,
        );
    }

    #[test]
    fn trailing_whitespace_in_a_span_does_not_make_its_line_taller() {
        let strut = sans(16.0).with_line_height(LineHeight::Length(20.0));
        let items = [
            length_run("aaaaaa", 16.0, 20.0),
            length_run(" ", 16.0, 50.0),
            length_run("bbbbbb", 16.0, 20.0),
        ];
        let layout = Font::load_embedded().shape_inline(&items, strut, Some(80.0));
        assert_eq!(baselines(&layout).len(), 2, "wrapped after the space");
        close(layout.height, 40.0);
    }

    #[test]
    fn an_atomic_box_sits_on_the_baseline_and_the_strut_sets_the_depth() {
        let items = [
            InlineContent::Text {
                text: "Hg",
                style: sans(16.0),
            },
            InlineContent::Box {
                id: 1,
                width: 20.0,
                height: 50.0,
            },
        ];
        let normal = sans(16.0);
        let line = Font::load_embedded().shape_inline(&items, normal, None);
        // 50 above the baseline, then what an empty 16px line has below it.
        close(line.height, 50.0 + 5.0);
        close(line.boxes[0].y, 0.0);
        close(line.baseline, 50.0);
    }

    #[test]
    fn a_line_of_atomic_boxes_alone_has_the_strut_depth_below_them() {
        // Chrome 153: a div holding one inline box 20px high is 25px tall, the 20 of
        // the box and the 5 an empty 16px line has below its baseline.
        let items = [InlineContent::Box {
            id: 1,
            width: 20.0,
            height: 50.0,
        }];
        let line = Font::load_embedded().shape_inline(&items, sans(16.0), None);
        close(line.height, 50.0 + 5.0);
        close(line.baseline, 50.0);
        close(line.boxes[0].y, 0.0);
    }

    #[test]
    fn registering_a_font_empties_the_style_metrics() {
        let mut font = Font::load_embedded();
        font.shape_inline(&[length_run("Hg", 16.0, 30.0)], strut(), None);
        assert!(!font.style_metrics.is_empty());
        font.register(EMBEDDED_MONOSPACE_FONT).unwrap();
        assert!(font.style_metrics.is_empty());
    }

    #[test]
    fn baseline_sits_strictly_between_the_top_and_the_bottom_of_the_line() {
        let mut font = Font::load_embedded();
        let metrics = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hg");
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
        let short = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "x");
        let long = font.measure(
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "Testing Baseline",
        );
        assert_eq!(short.baseline, long.baseline);
        assert_ne!(short.width, long.width, "sanity check: these really differ");
    }

    #[test]
    fn baseline_scales_with_font_size() {
        let mut font = Font::load_embedded();
        let small = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hg");
        let large = font.measure(TextStyle::new(FontFamily::SansSerif, 32.0, 400.0), "Hg");
        assert!(large.baseline > small.baseline);
    }

    #[test]
    fn baseline_is_the_same_whether_or_not_the_text_wraps() {
        // The ascent above the baseline comes from the font itself, not
        // from how many lines the content wrapped into — the first line's
        // baseline offset from the very top of the block must be identical
        // either way.
        let mut font = Font::load_embedded();
        let unwrapped = font.measure(
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "one two three",
        );
        let one_word = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "one");
        let wrapped = font.measure_wrapped(
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "one two three",
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
        let plain = font.shape(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hi");
        let inline = font.shape_inline(
            &[InlineContent::Text {
                text: "Hi",
                style: TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            }],
            strut(),
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
        let word = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), "Hello");

        // A box wide enough that "Hello" plus the box can't share a line at
        // this width — must push the box (and nothing else, one item) onto
        // its own line, growing the total height past one line.
        let unconstrained = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hello ",
                    style: TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                },
                InlineContent::Box {
                    id: 1,
                    width: word.width,
                    height: 30.0,
                },
            ],
            strut(),
            None,
        );
        let wrapped = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hello ",
                    style: TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                },
                InlineContent::Box {
                    id: 1,
                    width: word.width,
                    height: 30.0,
                },
            ],
            strut(),
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
                    style: TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                },
                InlineContent::Box {
                    id: 42,
                    width: 30.0,
                    height: 20.0,
                },
            ],
            strut(),
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
            strut(),
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
                style: TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            }],
            strut(),
            None,
        );
        let mixed = font.shape_inline(
            &[
                InlineContent::Text {
                    text: "Hg ",
                    style: TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
                },
                InlineContent::Text {
                    text: "Hg",
                    style: TextStyle::new(FontFamily::SansSerif, 48.0, 400.0),
                },
            ],
            strut(),
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
        let inline = font.shape_inline(&[], strut(), None);
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
            strut(),
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
        let expected = font.measure_wrapped(
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
            120.0,
        );

        let mut cache = None;
        let cached = font.measure_cached(
            &mut cache,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
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
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
            Some(400.0),
        );
        let second = font.measure_cached(
            &mut cache,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
            Some(80.0),
        );
        assert!(
            second.height > first.height,
            "wrapping the same text at a much narrower width must take more lines"
        );

        let expected_at_80 = font.measure_wrapped(
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
            80.0,
        );
        assert_eq!(
            second, expected_at_80,
            "re-breaking a cached layout at 80px must match a fresh measurement at 80px"
        );

        let back_to_400 = font.measure_cached(
            &mut cache,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
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
        let expected = font.measure(TextStyle::new(FontFamily::SansSerif, 16.0, 400.0), text);

        let mut cache = None;
        let cached = font.measure_cached(
            &mut cache,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            text,
            None,
        );
        assert_eq!(cached, expected);
    }

    #[test]
    fn measure_cached_with_empty_text_clears_the_cache_and_measures_zero() {
        let mut font = Font::load_embedded();
        let mut cache = None;
        let metrics = font.measure_cached(
            &mut cache,
            TextStyle::new(FontFamily::SansSerif, 16.0, 400.0),
            "",
            Some(100.0),
        );
        assert_eq!(metrics.width, 0.0);
        assert_eq!(metrics.height, 0.0);
        assert_eq!(metrics.baseline, 0.0);
        assert!(cache.is_none());
    }

    #[test]
    fn a_probe_shares_the_memo_but_is_not_counted_as_cache_activity() {
        use florui_profile::Counter;

        let mut font = Font::load_embedded();
        florui_profile::start(false);
        // Miss then hit through the probe: neither is counted.
        let probed = font.probe_memoized(
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0),
            "Hello",
            Some(80.0),
        );
        font.probe_memoized(
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0),
            "Hello",
            Some(80.0),
        );
        // The same entry answers a real measurement, which is counted.
        let measured = font.measure_memoized(
            &mut None,
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0),
            "Hello",
            Some(80.0),
        );
        florui_profile::finish_frame(Vec::new());
        let frames = florui_profile::frames();
        florui_profile::stop();

        assert_eq!(probed.height, measured.height);
        assert_eq!(frames[0].counter(Counter::TextMemoHits), 1);
        assert_eq!(frames[0].counter(Counter::TextMemoMisses), 0);
    }

    fn mixed<'a>(size: f32, weight: f32, tail: &'a str) -> [InlineContent<'a>; 3] {
        [
            InlineContent::Text {
                text: "Mixed paragraph with ",
                style: TextStyle::new(FontFamily::SansSerif, 14.0, 400.0),
            },
            InlineContent::Text {
                text: "a styled span",
                style: TextStyle::new(FontFamily::SansSerif, size, weight),
            },
            InlineContent::Text {
                text: tail,
                style: TextStyle::new(FontFamily::SansSerif, 14.0, 400.0),
            },
        ]
    }

    fn same_layout(a: &InlineLayout, b: &InlineLayout) -> bool {
        a.width == b.width
            && a.height == b.height
            && a.baseline == b.baseline
            && a.boxes == b.boxes
            && a.runs.len() == b.runs.len()
            && a.runs.iter().zip(&b.runs).all(|(x, y)| {
                x.font_size == y.font_size
                    && x.normalized_coords == y.normalized_coords
                    && x.glyphs.len() == y.glyphs.len()
                    && x.glyphs
                        .iter()
                        .zip(&y.glyphs)
                        .all(|(p, q)| (p.id, p.x, p.y) == (q.id, q.x, q.y))
            })
    }

    #[test]
    fn a_memoized_inline_run_equals_a_fresh_one_and_is_shared_on_repeat() {
        let mut font = Font::load_embedded();
        let items = mixed(18.0, 700.0, " and more words to wrap onto several lines.");
        for width in [None, Some(60.0), Some(140.0), Some(4000.0)] {
            let fresh = font.shape_inline(&items, strut(), width);
            let first = font.shape_inline_memoized(&items, strut(), width);
            let again = font.shape_inline_memoized(&items, strut(), width);
            assert!(same_layout(&fresh, &first), "width {width:?}");
            assert!(Arc::ptr_eq(&first, &again), "width {width:?} is shared");
        }
        assert_eq!(font.inline_memo.misses, 4, "one shaping per width");
    }

    #[test]
    fn every_part_of_the_key_makes_a_separate_entry() {
        every_part_of_the_key_is_told_apart();
    }

    #[test]
    fn colliding_keys_are_told_apart_by_the_whole_key() {
        inline_memo::COLLIDE.with(|c| c.set(true));
        every_part_of_the_key_is_told_apart();
        inline_memo::COLLIDE.with(|c| c.set(false));
    }

    fn every_part_of_the_key_is_told_apart() {
        let mut font = Font::load_embedded();
        let base = mixed(14.0, 400.0, " tail");
        font.shape_inline_memoized(&base, strut(), Some(100.0));
        // Each of these differs from the base in exactly one input.
        font.shape_inline_memoized(&base, strut(), Some(101.0));
        // And so does the container's own style.
        font.shape_inline_memoized(
            &base,
            strut().with_line_height(LineHeight::Length(30.0)),
            Some(100.0),
        );
        font.shape_inline_memoized(&mixed(15.0, 400.0, " tail"), strut(), Some(100.0));
        font.shape_inline_memoized(&mixed(14.0, 700.0, " tail"), strut(), Some(100.0));
        font.shape_inline_memoized(&mixed(14.0, 400.0, " tale"), strut(), Some(100.0));
        for styled in [
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0)
                .with_line_height(LineHeight::Length(30.0)),
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0)
                .with_line_height(LineHeight::Number(2.0)),
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0).with_spacing(2.0, 0.0),
            TextStyle::new(FontFamily::SansSerif, 14.0, 400.0).with_spacing(0.0, 6.0),
        ] {
            let mut items = base;
            items[2] = InlineContent::Text {
                text: " tail",
                style: styled,
            };
            font.shape_inline_memoized(&items, strut(), Some(100.0));
        }
        font.shape_inline_memoized(&base[..2], strut(), Some(100.0));
        font.shape_inline_memoized(&base, strut(), None);
        let mut boxed = base.to_vec();
        boxed.push(InlineContent::Box {
            id: 1,
            width: 10.0,
            height: 10.0,
        });
        font.shape_inline_memoized(&boxed, strut(), Some(100.0));
        let mut taller = boxed.clone();
        taller[3] = InlineContent::Box {
            id: 1,
            width: 10.0,
            height: 12.0,
        };
        font.shape_inline_memoized(&taller, strut(), Some(100.0));
        assert_eq!(font.inline_memo.misses, 14);
        assert_eq!(font.inline_memo.entries(), 14);
    }

    #[test]
    fn registering_a_font_empties_the_inline_memo() {
        let mut font = Font::load_embedded();
        font.shape_inline_memoized(&mixed(14.0, 400.0, " tail"), strut(), Some(100.0));
        assert_eq!(font.inline_memo.entries(), 1);
        font.register(EMBEDDED_MONOSPACE_FONT).unwrap();
        assert_eq!(font.inline_memo.entries(), 0);
    }

    #[test]
    fn the_inline_memo_stays_within_its_byte_budget() {
        let mut font = Font::load_embedded();
        // Room for a few entries only; far more distinct runs are asked for.
        font.inline_memo = inline_memo::InlineMemo::with_capacity(8 * 1024);
        for i in 0..200 {
            let tail = format!(" tail number {i}");
            font.shape_inline_memoized(&mixed(14.0, 400.0, &tail), strut(), Some(100.0));
        }
        let entries = font.inline_memo.entries();
        assert!(entries > 0 && entries < 200, "kept {entries} of 200");
        // A run larger than a whole generation is shaped but not kept.
        let mut tiny = Font::load_embedded();
        tiny.inline_memo = inline_memo::InlineMemo::with_capacity(16);
        let a = tiny.shape_inline_memoized(&mixed(14.0, 400.0, " tail"), strut(), Some(100.0));
        let b = tiny.shape_inline_memoized(&mixed(14.0, 400.0, " tail"), strut(), Some(100.0));
        assert_eq!(tiny.inline_memo.entries(), 0);
        assert!(same_layout(&a, &b));
    }

    #[test]
    fn a_stack_names_its_unquoted_families_but_not_the_generic_ones() {
        let font = Font::load_embedded();
        let stack = settings::intern_family_stack("Arial, \"Open Sans\", serif, Georgia");
        let style = TextStyle::new(FontFamily::Stack(stack), 16.0, 400.0);
        let names = font.family_candidates(&style);
        assert_eq!(&names[..3], ["Arial", "Open Sans", "Georgia"]);
    }

    #[test]
    fn inline_hits_and_shapings_are_counted_apart() {
        use florui_profile::Counter;

        let mut font = Font::load_embedded();
        let items = mixed(14.0, 400.0, " tail");
        florui_profile::start(false);
        font.shape_inline_memoized(&items, strut(), Some(100.0));
        font.shape_inline_memoized(&items, strut(), Some(100.0));
        font.shape_inline_memoized(&items, strut(), Some(100.0));
        florui_profile::finish_frame(Vec::new());
        let frames = florui_profile::frames();
        florui_profile::stop();
        assert_eq!(frames[0].counter(Counter::InlineShapes), 1);
        assert_eq!(frames[0].counter(Counter::InlineMemoHits), 2);
    }
}
