//! The used-value types every other crate consumes — [`ComputedStyle`],
//! [`Edges`] — and [`compute`], the entry point that resolves them for
//! every node in a tree. The actual selector matching, cascade, and
//! inheritance behind `compute` is Stylo's, in [`crate::stylo`]; this
//! module owns the public shape, not the resolution logic.

use std::collections::HashMap;

use crate::color::Rgba;
use crate::interaction::InteractionState;
use crate::stylesheet_parse::Rule;
use crate::stylo;
use crate::tree::{Arena, NodeId};

/// One edge's value on each of the four sides of the box, in that order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

/// One value per corner of the box, clockwise from the top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corners<T> {
    pub top_left: T,
    pub top_right: T,
    pub bottom_right: T,
    pub bottom_left: T,
}

/// Which of this crate's two embedded font families to shape/measure text
/// with — real CSS's own `font-family` is a whole comma-separated
/// preference list of specific names and generics; this crate only
/// distinguishes the one pair it actually has fonts for. A specific named
/// family (`"Helvetica"`, `"Georgia"`) it can't back with an embedded font
/// resolves to [`Self::SansSerif`], the same as an unspecified
/// `font-family` — real CSS's own initial value is itself UA-dependent,
/// and a browser's is typically a sans-serif system font (often Arial on
/// Windows); this crate's stand-in for that default is its own embedded
/// Open Sans, not a redistribution of Arial itself (proprietary, so this
/// crate cannot embed it) and not metrically matched to it either — see
/// `florui_text`'s own `fonts/NOTICE.md` for that tradeoff.
/// How much work the glass material may do; see [`GlassMaterial`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GlassQuality {
    #[default]
    Full,
    Reduced,
    Off,
}

/// The parameters of the opt-in glass material, read from the
/// `--florui-glass*` custom properties. Lengths are CSS pixels. These are the
/// values as written; the renderer applies the limits of the material
/// contract and reports when it does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassMaterial {
    /// Peak displacement of the backdrop at the edge.
    pub refraction: f32,
    /// Width of the lensing band, measured inward from the edge.
    pub edge: f32,
    /// Direction the light comes from, in degrees clockwise from the top.
    pub light_angle: f32,
    /// Strength of the rim light, 0 to 1.
    pub light_strength: f32,
    pub quality: GlassQuality,
}

/// How a node's `backdrop-filter` blur is computed, read from
/// `--florui-backdrop-blur`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackdropBlur {
    /// Every blur at full resolution; the pixels match the browser's. Also
    /// what an unrecognized value means.
    #[default]
    Exact,
    /// A large blur runs on a smaller copy of the backdrop and is enlarged
    /// again, which costs far less and differs a little from the browser's.
    Fast,
}

/// What `--florui-glass` asks of a node.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum GlassSpec {
    /// The property is absent, or not `refract`.
    #[default]
    None,
    Material(GlassMaterial),
    /// `refract` was asked for with a value that cannot be honored, so there
    /// is no material; the reason is for the effective-settings report.
    Invalid(&'static str),
}

/// `text-align` for left-to-right text: `justify` is `Start`, `left` and
/// `right` are `Start` and `End`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontFamily {
    #[default]
    SansSerif,
    Monospace,
}

/// How this node participates in its parent's formatting context
/// (`Block`/`Inline`/`InlineBlock`, real CSS's `display-outside` plus
/// `inline-block`'s special case), *and*, for `Block`/`Flex`/`Grid`, which
/// algorithm lays out its own children (real CSS's `display-inside`) —
/// this crate conflates the two into one field rather than splitting them
/// the way real CSS's two-value `display` syntax does, since nothing here
/// yet needs an `outside`/`inside` combination beyond the five this enum
/// already names. `Inline`'s own children (if it somehow has element
/// children, not just text) and `InlineBlock`'s own children both use the
/// same `Block` algorithm real CSS itself uses for both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    #[default]
    Block,
    Flex,
    /// `display: inline` — participates in a surrounding inline formatting
    /// context (mixed with text and other inline-level siblings, wrapping
    /// at the container's available width) rather than stacking as its own
    /// block. See `florui-layout`'s own module docs for this slice's
    /// documented bounds (one level of mixed inline content, no bidi, no
    /// `vertical-align` beyond baseline).
    Inline,
    /// `display: inline-block` — participates inline like [`Self::Inline`],
    /// but as a single opaque box sized from its own content (like a block
    /// box would be), not fragmented across lines.
    InlineBlock,
    Grid,
    /// `display: none` — generates no box: no layout space, no painting, no
    /// hit-testing, for the node and its whole subtree.
    None,
}

/// `position` — `Fixed`/`Sticky` aren't supported yet, see
/// [`ComputedStyle::position`]'s own doc for why they collapse to
/// [`Self::Static`] rather than [`Self::Absolute`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    #[default]
    Static,
    Relative,
    Absolute,
}

/// One track's sizing function, from `grid-template-columns`/`-rows` —
/// bounded to what a single (non-`repeat()`) track can be: `repeat()`,
/// named lines, `grid-template-areas`, and `fit-content()`/`minmax()`
/// beyond their max side aren't resolved here yet (real CSS still cascades
/// and parses them through Stylo; this crate just doesn't read them back).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridTrackSize {
    Length(f32),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
}

/// One line of a `grid-column`/`grid-row` placement — `grid-*-start`/`-end`
/// each resolve to one of these. Named lines aren't resolved (a `<custom-
/// ident>` falls back to [`Self::Auto`], the same as an unrecognized name
/// would in real CSS once no line actually carries that name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridPlacement {
    #[default]
    Auto,
    Line(i16),
    Span(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDirection {
    #[default]
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

/// Shared by `justify-content`/`align-content` — real CSS resolves both
/// through the same `content-distribution` value space. `None` means
/// `normal`: packed at the start with no extra distribution, and (unlike
/// every other `Option<...>` field here) not the same as an explicit
/// `flex-start`, since `normal` is genuinely a distinct initial value with
/// no equivalent keyword of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAlignment {
    Start,
    End,
    FlexStart,
    FlexEnd,
    Center,
    Stretch,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// Shared by `align-items`/`align-self` — same reasoning as
/// [`ContentAlignment`] for why this is `Option`, not a `#[default]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAlignment {
    Stretch,
    FlexStart,
    FlexEnd,
    Start,
    End,
    Center,
    Baseline,
}

/// One side's resolved border — solid-only, the minimum needed for a
/// visible default control outline (the scope set for this
/// property). Real CSS's other border styles (`dashed`, `dotted`, `double`,
/// …) still parse and cascade correctly through Stylo; this crate paints
/// every non-`none`/`hidden` style as a plain solid line, the same
/// "supported syntax, simplified rendering" tradeoff `stylesheet_parse`'s
/// own doc already documents for other unrendered CSS. `width` is always
/// `0.0` for `border-style: none`/`hidden` (real CSS's own initial style,
/// which makes a border invisible regardless of its width/color) — a
/// zero-width side needs no separate "is it visible" flag downstream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BorderSide {
    pub width: f32,
    pub color: Rgba,
}

/// One layer of `box-shadow` — real CSS's `<length>{2,4}` offsets/blur/
/// spread plus `inset`, all already resolved to concrete pixels (no
/// percentages in this property's own grammar, unlike `margin`/`padding`,
/// so unlike [`ComputedStyle::width`] this never needs an `Option`).
/// `blur_radius` is a real Gaussian blur (`florui-paint`'s own `blur`
/// module, since this crate's rasterizer, tiny-skia, has no blur
/// primitive of its own to reach for instead) — see `florui-paint`'s own
/// module doc for the CSS-spec correspondence between this field and the
/// blur's actual standard deviation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    /// A shadow's own shape grows or shrinks by this amount on every
    /// side before the offset is applied, real CSS's own `spread-radius`
    /// semantics.
    pub spread_radius: f32,
    pub color: Rgba,
    /// `inset` — an outer (drop) shadow paints outside the border box; an
    /// inset shadow paints inside the padding box instead. See
    /// `florui-paint`'s own doc for the painted shape and painting order
    /// each one gets relative to background/border.
    pub inset: bool,
}

/// One layer of `text-shadow`: the glyphs again, in `color`, moved by the
/// offsets and blurred, behind the text. Lengths are CSS pixels. Unlike
/// `box-shadow` there is no spread and no `inset`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub color: Rgba,
}

/// A `<length-percentage>` still carrying its own percentage component
/// unresolved — real CSS's own computed-value shape for this type. Every
/// other length field in this crate ([`ComputedStyle::width`], `padding`,
/// ...) already collapses a percentage to `None`/`0.0` at this layer
/// because nothing downstream can resolve it without a containing-block
/// size that isn't known until layout runs — but `transform`'s
/// `translate()` and `transform-origin` resolve against *this node's
/// own* already-final box, which paint always has in hand by the time it
/// reads these fields, so deferring resolution instead of discarding the
/// percentage costs nothing and matches real CSS instead of
/// approximating it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LengthPercentage {
    pub length: f32,
    pub percentage: f32,
}

impl LengthPercentage {
    /// Real CSS's own `<length-percentage>` resolution: the length
    /// component plus the percentage component scaled by `basis`.
    pub fn resolve(&self, basis: f32) -> f32 {
        self.length + self.percentage * basis
    }
}

/// One `transform` function, already reduced to this crate's documented
/// initial (2D-only) subset. `skew()`/`skewX()`/`skewY()`, every 3D
/// function (`translateZ`, `rotate3d`, `scale3d`, `matrix3d`,
/// `perspective`), and the animation-only `interpolatematrix`/
/// `accumulatematrix` intermediates all parse and cascade correctly
/// through Stylo but drop out of this list entirely — silently treated
/// as absent, the least-wrong approximation available without a partial
/// 2D projection of a genuinely 3D effect. See `florui-paint`'s own doc
/// for how the surviving functions fold into one 2D affine matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformFunction {
    /// `translate()`/`translateX()`/`translateY()`, unified: a bare
    /// `translateX(x)` is `Translate(x, 0)`, `translateY(y)` is
    /// `Translate(0, y)`.
    Translate(LengthPercentage, LengthPercentage),
    /// `scale()`/`scaleX()`/`scaleY()`, unified the same way.
    Scale(f32, f32),
    /// `rotate()`, in degrees — real CSS's own computed-value unit for
    /// `<angle>` regardless of the authored unit (`rad`, `turn`, `deg`,
    /// ...).
    Rotate(f32),
    /// `matrix(a, b, c, d, e, f)` — real CSS's own 2D matrix argument
    /// order and meaning (`x' = a*x + c*y + e`, `y' = b*x + d*y + f`);
    /// `e`/`f` are always plain lengths (real CSS's own `matrix()` has no
    /// percentage form), unlike [`Self::Translate`].
    Matrix {
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        e: f32,
        f: f32,
    },
}

/// `container-type` — whether, and on which axes, this node establishes a
/// size query container for its descendants' `@container` conditions. Real
/// CSS's own three-keyword grammar (`normal`/`size`/`inline-size`), read
/// back verbatim from Stylo (no reimplementation): only the `@container`
/// at-rule itself needs a hand-built adapter (Stylo's CSS parser never
/// recognizes it outside a `gecko` build), not this property — see
/// [`crate::stylo`]'s own module doc for why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerType {
    #[default]
    Normal,
    /// Establishes containment on the inline axis only (the axis
    /// `writing-mode` currently makes horizontal — this crate has no
    /// `writing-mode` support, so always physical width).
    InlineSize,
    /// Establishes containment on both axes.
    Size,
}

/// One `filter` function, already reduced to this crate's documented
/// initial subset: `blur()`, `brightness()`, `contrast()`, and
/// `saturate()`. `grayscale()`, `hue-rotate()`, `invert()`, the filter
/// list's own `opacity()` function (distinct from the `opacity`
/// property), `sepia()`, `drop-shadow()`, and `url()` all parse and
/// cascade correctly through Stylo but drop out of this list entirely —
/// the same treatment [`TransformFunction`] gives its own unsupported
/// functions. See `florui-paint`'s own doc for how the surviving
/// functions apply to a node's own rendered content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterFunction {
    /// `blur(<length>)` — a Gaussian blur radius in pixels; real CSS's
    /// own grammar already forbids a negative one.
    Blur(f32),
    /// `brightness(<factor>)` — `1.0` (`100%`) is a no-op, `0.0` is
    /// black, and above `1.0` brightens; real CSS's own grammar already
    /// forbids a negative factor.
    Brightness(f32),
    /// `contrast(<factor>)` — `1.0` (`100%`) is a no-op, `0.0` is flat
    /// mid-gray; real CSS's own grammar already forbids a negative
    /// factor.
    Contrast(f32),
    /// `saturate(<factor>)` — `1.0` (`100%`) is a no-op, `0.0` is
    /// grayscale; real CSS's own grammar already forbids a negative
    /// factor.
    Saturate(f32),
}

/// `box-sizing`: what a declared `width` or `height` measures. Not
/// inherited; the initial value, and real CSS's default, is `content-box`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxSizing {
    /// The declared size is the content box; padding and border add to it.
    #[default]
    ContentBox,
    /// The declared size includes padding and border; the content box is
    /// what is left of it.
    BorderBox,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub background_color: Rgba,
    /// `background-image` and the properties that go with it, one entry per
    /// layer, the first drawn on top.
    pub background_layers: Vec<crate::background::BackgroundLayer>,
    pub color: Rgba,
    /// `None` means `auto` — or, since real CSS now parses here, any
    /// value this crate can't yet resolve to a concrete pixel length
    /// (a percentage, a `calc()`); see [`crate::stylo`]'s conversion
    /// notes.
    pub width: Option<f32>,
    /// `None` means `auto`; see [`Self::width`].
    pub height: Option<f32>,
    /// The fraction (`68%` is `0.68`) when `width` is a plain percentage
    /// of the containing block, which only layout can resolve; `width` is
    /// then `None`.
    pub width_percent: Option<f32>,
    /// See [`Self::width_percent`].
    pub height_percent: Option<f32>,
    /// `None` means `none`, or a value that is not a plain length; see
    /// [`Self::width`]. Only lengths clamp.
    pub max_width: Option<f32>,
    /// See [`Self::max_width`].
    pub max_height: Option<f32>,
    /// `None` means `auto`; a percentage is kept unresolved, like
    /// [`Self::inset`], because its basis is the containing block that only
    /// layout knows. For a flex item `auto` is the content-based minimum,
    /// which the layout engine applies itself.
    pub min_width: Option<LengthPercentage>,
    /// See [`Self::min_width`].
    pub min_height: Option<LengthPercentage>,
    /// See [`BoxSizing`]. Does not inherit.
    pub box_sizing: BoxSizing,
    /// Each edge is `None` for an explicit `auto` (enabling the usual
    /// auto-margin centering behavior), `Some(0.0)` when nothing set it.
    pub margin: Edges<Option<f32>>,
    /// Always a concrete value; real CSS padding has no `auto`.
    pub padding: Edges<f32>,
    /// Inherits; initial `16.0`.
    pub font_size: f32,
    /// Inherits; initial [`FontFamily::SansSerif`]. See [`FontFamily`]'s
    /// own doc for what "resolves to" means here.
    pub font_family: FontFamily,
    /// Inherits; initial `400.0` (`normal`), CSS's numeric 1–1000 scale.
    pub font_weight: f32,
    /// This node's own layout algorithm, applied to *its children* — a
    /// leaf's `display` never affects how its own box is placed by its
    /// parent (that's [`Self::flex_grow`]/[`Self::flex_shrink`]/
    /// [`Self::flex_basis`]/[`Self::align_self`] instead).
    pub display: Display,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    /// `justify-content`: main-axis distribution of this flex container's
    /// own children. Meaningless when [`Self::display`] isn't
    /// [`Display::Flex`].
    pub justify_content: Option<ContentAlignment>,
    /// `align-content`: cross-axis distribution across wrapped flex lines.
    /// Meaningless when [`Self::display`] isn't [`Display::Flex`].
    pub align_content: Option<ContentAlignment>,
    /// `align-items`: this flex container's default cross-axis alignment
    /// for its children, unless a child overrides it with
    /// [`Self::align_self`]. Meaningless when [`Self::display`] isn't
    /// [`Display::Flex`].
    pub align_items: Option<ItemAlignment>,
    /// `align-self`: this node's *own* cross-axis alignment within
    /// whichever flex container it's a child of, overriding that
    /// container's [`Self::align_items`]. Meaningless when this node's
    /// parent isn't a flex container.
    pub align_self: Option<ItemAlignment>,
    /// How much of a flex container's remaining free space this node
    /// claims, relative to its flex siblings. `0.0` (the CSS initial
    /// value) means it does not grow.
    pub flex_grow: f32,
    /// How much this node shrinks when a flex container's children
    /// collectively overflow it, relative to its flex siblings. `1.0` is
    /// the CSS initial value — flex items shrink by default.
    pub flex_shrink: f32,
    /// The size a flex item starts from before growing/shrinking
    /// distributes remaining space. `None` means `auto` (fall back to
    /// [`Self::width`]/[`Self::height`], on whichever axis is the main
    /// one).
    pub flex_basis: Option<f32>,
    /// `column-gap`, between adjacent children on the main axis for a row
    /// flex container (or the cross axis for a column one).
    pub column_gap: f32,
    /// `row-gap`, the same on the other axis.
    pub row_gap: f32,
    /// `border-*-width`/`-style`/`-color` per side — see [`BorderSide`]'s
    /// own doc for the solid-only scope and the `none`/`hidden` collapse.
    pub border: Edges<BorderSide>,
    /// `border-*-radius` per corner as a (horizontal, vertical) pair, still
    /// carrying any percentage unresolved: it resolves against this node's
    /// own border box (width for horizontal, height for vertical), which
    /// only paint knows. Not clamped here either -- the spec's
    /// scale-down-when-radii-overlap rule needs that same final box.
    pub border_radius: Corners<(LengthPercentage, LengthPercentage)>,
    /// `grid-template-columns`. Meaningless when [`Self::display`] isn't
    /// [`Display::Grid`]. See [`GridTrackSize`]'s own doc for the bound.
    pub grid_template_columns: Vec<GridTrackSize>,
    /// `grid-template-rows`, the same on the other axis.
    pub grid_template_rows: Vec<GridTrackSize>,
    /// `grid-column-start`/`grid-column-end`.
    pub grid_column: (GridPlacement, GridPlacement),
    /// `grid-row-start`/`grid-row-end`.
    pub grid_row: (GridPlacement, GridPlacement),
    /// `position`. `Fixed`/`Sticky` aren't supported yet — collapsed to
    /// [`Position::Static`] rather than silently behaving like
    /// [`Position::Absolute`], a real behavior mismatch an author asking
    /// for viewport-relative or scroll-anchored positioning would hit
    /// otherwise. `Absolute`'s real containing-block rule (the *nearest
    /// explicitly* `relative`/`absolute` ancestor) is also simplified:
    /// this crate's own [`Position::Static`] still counts as a valid
    /// positioning context for an absolutely-positioned descendant, the
    /// same deviation the official Stylo→Taffy bridge crate itself
    /// already accepts (Taffy has no `Static` concept of its own).
    pub position: Position,
    /// `top`/`right`/`bottom`/`left` — meaningless when [`Self::position`]
    /// is [`Position::Static`]. Each edge is `None` for an explicit
    /// `auto`. Kept as a [`LengthPercentage`], not collapsed to a resolved
    /// pixel like [`Self::width`]/[`Self::margin`] are, for the same
    /// reason `transform`'s own `translate()`/`transform-origin` are: the
    /// containing block it resolves against (the nearest positioned
    /// ancestor's own padding box) is only known once layout runs; Taffy's
    /// own inset type already resolves a percentage there natively, so
    /// `florui-layout` only has to hand it through unresolved.
    pub inset: Edges<Option<LengthPercentage>>,
    /// `z-index`. `None` means `auto` (the initial value) — real CSS only
    /// gives `z-index` an effect on a positioned (non-[`Position::Static`])
    /// element, a flex item, or a grid item (see `florui_paint`'s own doc
    /// on stacking order). Meaningless anywhere else, matching real CSS.
    pub z_index: Option<i32>,
    /// `opacity`, clamped to `0.0..=1.0` (real CSS's own computed-value
    /// clamp). `1.0` (fully opaque) is the initial value and paints
    /// exactly as before this property existed. Below `1.0`, painting
    /// this node's own box *and every descendant* as one composited group
    /// is what makes it "group" opacity rather than a per-primitive
    /// multiply — see `florui_paint`'s own doc on why that distinction is
    /// visible wherever a node's own children overlap each other.
    pub opacity: f32,
    /// `box-shadow` — zero or more comma-separated layers, in the order
    /// authored. Real CSS paints the *first*-listed layer on top of the
    /// rest; see [`BoxShadow`]'s own doc for what's painted vs. carried
    /// through unrendered, and `florui-paint`'s own doc for the ordering
    /// this crate paints them in.
    pub box_shadow: Vec<BoxShadow>,
    /// `pointer-events: none` — the node is never the target of a pointer
    /// hit (its descendants still can be, if they don't inherit `none`).
    /// `auto` and every other value read as `false`.
    pub pointer_events_none: bool,
    /// `text-decoration`/`text-decoration-line` — true only when the
    /// value includes `underline`. `overline`/`line-through`/`blink`
    /// aren't tracked; this is `<a>`'s own default-stylesheet need, not a
    /// claim of full property coverage.
    pub text_decoration_underline: bool,
    /// Inherited; see [`TextAlign`].
    pub text_align: TextAlign,
    /// Inherited `text-shadow`, the first layer on top.
    pub text_shadow: Vec<TextShadow>,
    /// The opt-in glass material, read from `--florui-glass` and its
    /// parameters. Like any custom property it inherits.
    pub glass: GlassSpec,
    /// `--florui-backdrop-blur`; inherits like any custom property.
    pub backdrop_blur: BackdropBlur,
    /// `cursor: pointer` — true only for that one keyword; every other
    /// value (including `auto`) reads `false`, the same narrow scope as
    /// [`Self::text_decoration_underline`].
    pub cursor_pointer: bool,
    /// Whether this node clips its own content (including descendants) to
    /// its padding box — real CSS's `overflow-x`/`overflow-y`, collapsed
    /// to one bool. `false` only when *both* axes are the initial
    /// `visible`; every other combination clips on both axes regardless
    /// of which single axis declared it, which is real CSS's own rule too
    /// (a `visible` axis paired with a non-`visible` one computes to
    /// `auto`, not `visible`) — so this isn't a simplification of the
    /// clipping behavior itself, only of which of `hidden`/`scroll`/
    /// `auto`/`clip` caused it. See [`Self::overflow_scrolls_x`]/
    /// [`Self::overflow_scrolls_y`] for which of those clipping causes also
    /// makes the axis reachable via real scrolling.
    pub overflow_clips: bool,
    /// Whether `overflow-x` is real CSS's `scroll`/`auto` — the two
    /// keywords that make this axis's clipped-away content reachable again
    /// by scrolling, rather than clipped away for good (`hidden`/`clip`).
    /// Independent of [`Self::overflow_clips`], which only says an axis
    /// clips, not whether it scrolls.
    pub overflow_scrolls_x: bool,
    /// Same as [`Self::overflow_scrolls_x`], for `overflow-y`.
    pub overflow_scrolls_y: bool,
    /// `transform`'s own function list, in authored order — see
    /// [`TransformFunction`]'s own doc for the supported subset. An empty
    /// list is real CSS's own `none`, the initial value. Composing these
    /// into one matrix and resolving [`Self::transform_origin`] against
    /// this node's own box happens in `florui-paint`, the first place a
    /// node's final box size is known.
    pub transform: Vec<TransformFunction>,
    /// `transform-origin`'s `x`/`y` components — its own `z` component is
    /// dropped, matching [`TransformFunction`]'s 2D-only scope. `(50%,
    /// 50%)` (the box's own center) is real CSS's initial value.
    pub transform_origin: (LengthPercentage, LengthPercentage),
    /// `filter`'s own function list, in authored order — see
    /// [`FilterFunction`]'s own doc for the supported subset. An empty
    /// list is real CSS's own `none`, the initial value. Real CSS applies
    /// each listed function to the *previous* one's own output in order
    /// (the first-listed function reads the node's own unfiltered
    /// content); `florui-paint`'s own doc covers how that chain applies
    /// to a node's rendered content.
    pub filter: Vec<FilterFunction>,
    /// Same grammar/subset as [`Self::filter`], applied to whatever is
    /// already painted behind this node instead of its own content.
    pub backdrop_filter: Vec<FilterFunction>,
    /// `container-type` — see [`ContainerType`]'s own doc.
    pub container_type: ContainerType,
    /// `container-name` — zero or more `<custom-ident>`s a descendant's
    /// `@container <name> (...)` can filter by; empty is real CSS's own
    /// initial `none`. Meaningless when [`Self::container_type`] is
    /// [`ContainerType::Normal`], matching real CSS (a name with no
    /// established containment names nothing).
    pub container_name: Vec<String>,
    /// `object-fit` — meaningless on anything but a replaced element
    /// (e.g. `<img>`); [`ObjectFit::Fill`] is the initial value.
    pub object_fit: ObjectFit,
    /// `object-position` — same `(horizontal, vertical)` shape as
    /// [`Self::transform_origin`], for the same reason: it resolves
    /// against a replaced element's own already-final content box, which
    /// only paint has in hand. `(50%, 50%)` (centered) is the initial
    /// value. Meaningless on anything but a replaced element.
    pub object_position: (LengthPercentage, LengthPercentage),
    /// `aspect-ratio`. Meaningful on any box (not just a replaced
    /// element) — see [`AspectRatio`]'s own doc.
    pub aspect_ratio: AspectRatio,
    /// Real CSS's `appearance` — narrowed to just the two values this
    /// crate can actually act on (see [`Appearance`]'s own doc for why
    /// it isn't the real property).
    pub appearance: Appearance,
    /// Which way a `<textarea>` can be resized by dragging its corner,
    /// read from `--florui-resize` (real `resize` is not in the servo
    /// build of the style engine, the same reason as `appearance`).
    pub resize: Resize,
    /// The color of a text field's placeholder, read from
    /// `--florui-placeholder-color` (real `::placeholder` only parses in a
    /// user-agent sheet in the servo build of the style engine); `None`
    /// keeps the browser default.
    pub placeholder_color: Option<Rgba>,
    /// `scroll-behavior: smooth`. Real `scroll-behavior` is gecko-only in
    /// the style engine, so an adapter spells it `--florui-scroll-behavior`
    /// (see `scroll_behavior_adapter`); unlike a custom property it does
    /// not inherit. It animates programmatic scrolls of this element.
    pub scroll_behavior_smooth: bool,
}

impl ComputedStyle {
    /// Copies from `other` the fields that change only how a node is painted:
    /// its colors (background, text, each border side's color, placeholder),
    /// box shadows, opacity, text underline and the pointer cursor. Nothing
    /// that can change a size, a position, which node is hit, stacking, or what
    /// a registry reads from a style (overflow, font, appearance, transform)
    /// is copied.
    ///
    /// The destructuring below names every field, so adding one to
    /// [`ComputedStyle`] does not compile until it is put on one side or the
    /// other here. A field left off the paint side only makes a restyle take
    /// the slower path; a field wrongly put on it would leave a layout stale.
    pub fn copy_paint_from(&mut self, other: &ComputedStyle) {
        let ComputedStyle {
            // Paint only.
            background_color,
            background_layers,
            color,
            border,
            box_shadow,
            opacity,
            text_decoration_underline,
            text_align,
            text_shadow,
            glass,
            backdrop_blur,
            cursor_pointer,
            placeholder_color,
            // Everything else may change layout, hit testing, stacking or what a
            // registry reads, so it is not copied.
            width: _,
            height: _,
            width_percent: _,
            height_percent: _,
            max_width: _,
            max_height: _,
            min_width: _,
            min_height: _,
            box_sizing: _,
            margin: _,
            padding: _,
            font_size: _,
            font_family: _,
            font_weight: _,
            display: _,
            flex_direction: _,
            flex_wrap: _,
            justify_content: _,
            align_content: _,
            align_items: _,
            align_self: _,
            flex_grow: _,
            flex_shrink: _,
            flex_basis: _,
            column_gap: _,
            row_gap: _,
            border_radius: _,
            grid_template_columns: _,
            grid_template_rows: _,
            grid_column: _,
            grid_row: _,
            position: _,
            inset: _,
            z_index: _,
            pointer_events_none: _,
            overflow_clips: _,
            overflow_scrolls_x: _,
            overflow_scrolls_y: _,
            transform: _,
            transform_origin: _,
            filter: _,
            backdrop_filter: _,
            container_type: _,
            container_name: _,
            object_fit: _,
            object_position: _,
            aspect_ratio: _,
            appearance: _,
            resize: _,
            scroll_behavior_smooth: _,
        } = other;
        self.background_color = *background_color;
        self.background_layers.clone_from(background_layers);
        self.color = *color;
        self.border.top.color = border.top.color;
        self.border.right.color = border.right.color;
        self.border.bottom.color = border.bottom.color;
        self.border.left.color = border.left.color;
        self.box_shadow.clone_from(box_shadow);
        self.opacity = *opacity;
        self.text_decoration_underline = *text_decoration_underline;
        self.text_align = *text_align;
        self.text_shadow.clone_from(text_shadow);
        self.glass = *glass;
        self.backdrop_blur = *backdrop_blur;
        self.cursor_pointer = *cursor_pointer;
        self.placeholder_color = *placeholder_color;
    }

    /// Whether `other` differs from `self` only in what
    /// [`Self::copy_paint_from`] copies: a node whose style changed this way
    /// needs repainting, not laying out again.
    pub fn differs_only_in_paint(&self, other: &ComputedStyle) -> bool {
        if self == other {
            return true;
        }
        let mut repainted = self.clone();
        repainted.copy_paint_from(other);
        repainted == *other
    }
}

/// The values of `resize`, spelled through `--florui-resize`. Only a
/// `<textarea>` acts on it; anything unset or unrecognized is `Both`, which
/// is what a textarea has by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resize {
    #[default]
    Both,
    Vertical,
    Horizontal,
    None,
}

/// A narrow stand-in for real CSS's `appearance` property: whether a
/// built-in control decoration with no real DOM node of its own (a
/// `<select>`'s own chevron, a checked checkbox's own check mark) should
/// paint at all. The real `appearance` property would be the standard
/// fit, but the `style` crate this project's cascade runs on compiles it
/// out entirely under Servo mode (`engines="gecko"` in its own source) —
/// pulling in Gecko/Firefox FFI bindings just for this one toggle isn't
/// viable. Read instead from a plain custom property, `--florui-appearance`
/// (the same naming spirit as a vendor prefix like `-webkit-appearance`,
/// since this genuinely stands in for the real thing): real CSS syntax,
/// cascaded/inherited by Stylo's own existing (and already exercised)
/// custom-property machinery, no engine changes, just a value this crate
/// defines the meaning of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Appearance {
    /// The built-in decoration paints normally. Real CSS's initial value,
    /// and this crate's fallback whenever `--florui-appearance` is unset
    /// or holds anything other than `none`.
    #[default]
    Auto,
    /// The built-in decoration doesn't paint at all -- `--florui-appearance: none;`.
    None,
}

/// `object-fit`'s exact CSS-spec variant set — a replaced element's
/// intrinsic content (e.g. a decoded image) may need to be scaled/cropped
/// to fill its own content box differently from the box-model default
/// (stretch to fill).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectFit {
    /// Stretch to fill the content box exactly, ignoring the content's
    /// own intrinsic aspect ratio. Real CSS's initial value.
    #[default]
    Fill,
    /// Uniform scale to fit entirely within the content box, preserving
    /// aspect ratio — the shorter axis leaves empty space (letterboxing).
    Contain,
    /// Uniform scale to fully cover the content box, preserving aspect
    /// ratio — the longer axis is cropped.
    Cover,
    /// Render at intrinsic size, uncropped and unscaled (may overflow or
    /// underfill the content box).
    None,
    /// Whichever of [`Self::None`] or [`Self::Contain`] produces the
    /// smaller concrete size.
    ScaleDown,
}

/// `aspect-ratio`. Real CSS's grammar is `auto || <ratio>` — either
/// component alone, or both together — which is why this isn't collapsed
/// to a single `Option<(f32, f32)>`: `auto` (alone) and `auto <ratio>`
/// both prefer a replaced element's own intrinsic ratio when it has one,
/// but only the latter also gives a fallback for when it doesn't (and a
/// non-replaced box, which never has an intrinsic ratio, always falls
/// back when `ratio` is present); a bare `<ratio>` (no `auto`) instead
/// always wins over any intrinsic ratio, even a replaced element's own.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AspectRatio {
    /// Whether a replaced element's own intrinsic ratio, if it has one,
    /// takes precedence over [`Self::ratio`]. Real CSS's `auto` keyword.
    pub prefers_intrinsic: bool,
    /// The explicit `<width> / <height>` ratio, if the author gave one.
    /// `None` together with `prefers_intrinsic: true` is real CSS's bare
    /// `auto` (no explicit ratio at all, real CSS's initial value).
    pub ratio: Option<(f32, f32)>,
}

impl Default for AspectRatio {
    fn default() -> Self {
        Self {
            prefers_intrinsic: true,
            ratio: None,
        }
    }
}

/// The viewport `@media` queries evaluate against — real CSS's own
/// initial containing block size, in CSS pixels (not physical/DPR-scaled
/// ones: `min-width`/`max-width` are always defined in terms of the
/// viewport's own CSS pixel size). [`Default`] is this crate's own
/// placeholder (`1024x768`) for callers — mostly tests — that don't have
/// a real window and don't care what a size-based query resolves to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1024.0,
            height: 768.0,
        }
    }
}

/// Resolves every node in `arena` against `rules` and `state` — real
/// selector matching, cascade, and inheritance, via Stylo. `viewport` is
/// what `@media`'s own size features (`min-width`, ...) resolve against.
/// `timeline` carries `transition`/`@keyframes` state across calls — see
/// [`crate::AnimationTimeline`]'s own doc.
///
/// Every `@container` condition resolves as non-matching under this
/// entry point (an empty signature) — a stylesheet with `@container`
/// blocks needs [`compute_with_container_query_signature`] instead, driven
/// by `florui_layout::compute_with_style`'s own multi-pass orchestration
/// (a container query's match depends on real, already-laid-out geometry
/// this crate alone can't produce — see
/// [`crate::container_query_adapter`]'s own module doc). Every existing
/// caller with no container queries of its own is unaffected either way.
pub fn compute(
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: Viewport,
    timeline: &mut crate::animation::AnimationTimeline,
) -> HashMap<NodeId, ComputedStyle> {
    stylo::compute(arena, rules, state, viewport, timeline, &[])
}

/// Same as [`compute`], but with an explicit, real per-node
/// `@container` signature instead of treating every condition as
/// non-matching. `container_query_signature` must be exactly the flattened,
/// in-order length every `Rule` in `rules` reports via its own (crate-
/// private) `container_query_blocks()` — see
/// [`crate::container_query_adapter::resolve_container_query_signatures`]
/// for how a caller derives one for a specific node's own signature, and
/// `florui_layout::compute_with_style` for the orchestration that groups
/// nodes by signature and calls this once per distinct group.
pub fn compute_with_container_query_signature(
    arena: &Arena,
    rules: &[Rule],
    state: &InteractionState,
    viewport: Viewport,
    timeline: &mut crate::animation::AnimationTimeline,
    container_query_signature: &[bool],
) -> HashMap<NodeId, ComputedStyle> {
    stylo::compute(
        arena,
        rules,
        state,
        viewport,
        timeline,
        container_query_signature,
    )
}

#[cfg(test)]
mod tests {
    mod paint_only {
        use super::*;

        /// The style of a bare element under no stylesheet.
        fn base() -> ComputedStyle {
            let tree: Element = view! { <div /> };
            let (arena, computed) = styles(&tree, "", &InteractionState::new());
            computed[&arena.roots()[0]].clone()
        }

        fn rgba(r: u8) -> Rgba {
            Rgba::opaque(r, 0, 0)
        }

        #[test]
        fn a_style_equal_to_itself_differs_in_nothing() {
            let style = base();
            assert!(style.differs_only_in_paint(&style.clone()));
        }

        #[test]
        fn colors_shadows_opacity_underline_and_cursor_are_paint_only() {
            let base = base();
            let mut changed = base.clone();
            changed.background_color = rgba(10);
            changed.color = rgba(20);
            changed.border.left.color = rgba(30);
            changed.opacity = 0.5;
            changed.text_decoration_underline = true;
            changed.cursor_pointer = true;
            changed.placeholder_color = Some(rgba(40));
            changed.box_shadow = vec![BoxShadow {
                offset_x: 1.0,
                offset_y: 2.0,
                blur_radius: 3.0,
                spread_radius: 0.0,
                color: rgba(50),
                inset: false,
            }];
            assert!(base.differs_only_in_paint(&changed));

            let mut copied = base.clone();
            copied.copy_paint_from(&changed);
            assert_eq!(
                copied, changed,
                "copying the paint fields reaches the new style"
            );
        }

        #[test]
        fn a_size_a_margin_or_a_border_width_is_not_paint_only() {
            let base = base();
            for change in [
                (|s: &mut ComputedStyle| s.width = Some(10.0)) as fn(&mut ComputedStyle),
                |s| s.margin.top = Some(4.0),
                |s| s.border.top.width = 2.0,
                |s| s.font_size = 20.0,
                |s| s.display = Display::None,
                |s| s.z_index = Some(3),
                |s| s.overflow_clips = true,
                |s| s.transform = vec![TransformFunction::Rotate(1.0)],
                |s| s.filter = vec![FilterFunction::Blur(2.0)],
            ] {
                let mut changed = base.clone();
                change(&mut changed);
                assert!(
                    !base.differs_only_in_paint(&changed),
                    "a change to a layout field was taken for paint-only: {changed:?}"
                );
            }
        }

        #[test]
        fn a_color_together_with_a_width_is_not_paint_only() {
            let base = base();
            let mut changed = base.clone();
            changed.background_color = rgba(10);
            changed.width = Some(50.0);
            assert!(!base.differs_only_in_paint(&changed));
        }

        #[test]
        fn copying_paint_fields_leaves_layout_fields_alone() {
            let mut target = base();
            target.width = Some(77.0);
            let mut source = base();
            source.background_color = rgba(90);
            source.width = Some(5.0);
            target.copy_paint_from(&source);
            assert_eq!(target.background_color, rgba(90));
            assert_eq!(target.width, Some(77.0), "the width stays what it was");
        }
    }

    mod shared_styles {
        use super::*;

        fn row(i: usize) -> Element {
            let mut attrs: Vec<(String, String)> = vec![(
                "class".into(),
                if i.is_multiple_of(3) {
                    "row odd"
                } else {
                    "row"
                }
                .into(),
            )];
            if i % 4 == 1 {
                attrs.push(("data-k".into(), "x".into()));
            }
            if i == 5 {
                attrs.push(("id".into(), "unique".into()));
            }
            if i % 11 == 3 {
                attrs.push(("style".into(), "color: #ff0000".into()));
            }
            Element::node(
                "div",
                attrs,
                vec![
                    Element::node("span", vec![("class".into(), "a".into())], vec![]),
                    Element::node("span", vec![("class".into(), "b".into())], vec![]),
                    Element::node("button", vec![("class".into(), "go".into())], vec![]),
                ],
            )
        }

        const CSS: &str = ".row { height: 20px; }             .row:nth-child(odd) { margin-left: 3px; }             .row:nth-child(3n+1) { margin-right: 4px; }             .row:first-child { padding-top: 9px; } .row:last-child { padding-bottom: 8px; }             .row + .row { margin-top: 2px; }             .row:not(.odd) { opacity: 0.5; }             .row[data-k] { padding-left: 5px; }             #unique { width: 77px; }             .a + .b { color: #00ff00; }             .row:hover { background-color: #0000ff; } .row:hover + .row { padding-right: 6px; }             .row:hover .a { color: #ff00ff; }";

        const ROWS: usize = 40;

        #[test]
        fn alike_rows_that_differ_by_position_attribute_or_state_do_not_share_a_style() {
            let list: Element = Element::node(
                "div",
                vec![("class".into(), "root".into())],
                (0..ROWS).map(row).collect(),
            );
            let hovered_row = 7;
            let arena = Arena::build(&list);
            let rows = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"));
            assert_eq!(rows.len(), ROWS);

            let mut state = InteractionState::new().with_hovered(rows[hovered_row]);
            let mut up = arena.parent(rows[hovered_row]);
            while let Some(parent) = up {
                state = state.with_hovered(parent);
                up = arena.parent(parent);
            }
            let (arena, computed) = styles(&list, CSS, &state);
            let rows = arena.find_all(|a, id| a.classes(id).iter().any(|c| c == "row"));

            for (i, &row) in rows.iter().enumerate() {
                let style = &computed[&row];
                let position = i + 1; // :nth-child counts from one
                // Each expectation below is worked out from the row's position and
                // attributes, not by asking the cascade, so a row that took a
                // look-alike sibling's style shows up as a difference.
                assert_eq!(
                    style.margin.left,
                    Some(if position % 2 == 1 { 3.0 } else { 0.0 }),
                    "row {i}: :nth-child(odd) margin"
                );
                assert_eq!(
                    style.margin.right,
                    Some(if position % 3 == 1 { 4.0 } else { 0.0 }),
                    "row {i}: :nth-child(3n+1) margin"
                );
                assert_eq!(
                    style.padding.top,
                    if i == 0 { 9.0 } else { 0.0 },
                    "row {i}: :first-child padding"
                );
                assert_eq!(
                    style.padding.bottom,
                    if i == ROWS - 1 { 8.0 } else { 0.0 },
                    "row {i}: :last-child padding"
                );
                assert_eq!(
                    style.margin.top,
                    Some(if i == 0 { 0.0 } else { 2.0 }),
                    "row {i}: sibling combinator margin"
                );
                assert_eq!(
                    style.opacity,
                    if i.is_multiple_of(3) { 1.0 } else { 0.5 },
                    "row {i}: :not(.odd) opacity"
                );
                assert_eq!(
                    style.padding.left,
                    if i % 4 == 1 { 5.0 } else { 0.0 },
                    "row {i}: attribute selector padding"
                );
                assert_eq!(
                    style.width,
                    if i == 5 { Some(77.0) } else { None },
                    "row {i}: #id width"
                );
                assert_eq!(
                    style.color,
                    if i % 11 == 3 {
                        Rgba::opaque(255, 0, 0)
                    } else {
                        computed[&rows[0]].color
                    },
                    "row {i}: inline style color"
                );
                assert_eq!(
                    style.background_color == Rgba::opaque(0, 0, 255),
                    i == hovered_row,
                    "row {i}: only the hovered row is :hover"
                );
                assert_eq!(
                    style.padding.right,
                    if i == hovered_row + 1 { 6.0 } else { 0.0 },
                    "row {i}: only the row after the hovered one gets `:hover +`"
                );
            }

            // The first span of each row is `.a`; only the hovered row's recolors it.
            for (i, &row) in rows.iter().enumerate() {
                let first_span = arena.children(row)[0];
                let recolored = computed[&first_span].color == Rgba::opaque(255, 0, 255);
                assert_eq!(recolored, i == hovered_row, "row {i}: `.row:hover .a`");
                let second_span = arena.children(row)[1];
                assert_eq!(
                    computed[&second_span].color,
                    Rgba::opaque(0, 255, 0),
                    "row {i}: `.a + .b` applies in every row"
                );
            }
        }
    }

    use std::collections::HashSet;

    use florui::prelude::*;

    use super::*;
    use crate::stylesheet_parse::parse_stylesheet;

    fn styles(
        tree: &Element,
        css: &str,
        state: &InteractionState,
    ) -> (Arena, HashMap<NodeId, ComputedStyle>) {
        let arena = Arena::build(tree);
        let rules = parse_stylesheet(css).unwrap();
        let computed = compute(
            &arena,
            &rules,
            state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        (arena, computed)
    }

    #[test]
    fn a_matching_class_rule_sets_background_color() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { background-color: #1e1e22; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].background_color,
            Rgba::opaque(0x1e, 0x1e, 0x22)
        );
    }

    #[test]
    fn background_color_does_not_inherit() {
        let tree: Element = view! {
            <div class="card">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card { background-color: #1e1e22; }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].background_color, Rgba::TRANSPARENT);
    }

    #[test]
    fn color_inherits_by_default() {
        let tree: Element = view! {
            <div class="card">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) =
            styles(&tree, ".card { color: #ffffff; }", &InteractionState::new());
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].color, Rgba::opaque(0xff, 0xff, 0xff));
    }

    #[test]
    fn explicit_inherit_pulls_a_non_inheriting_property_from_the_parent() {
        let tree: Element = view! {
            <div class="card">
                <span class="mirror">{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card { background-color: #1e1e22; } .mirror { background-color: inherit; }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(
            computed[&span].background_color,
            Rgba::opaque(0x1e, 0x1e, 0x22)
        );
    }

    #[test]
    fn higher_specificity_wins_regardless_of_source_order() {
        let tree: Element = view! { <div id="main" class="card" /> };
        let (arena, computed) = styles(
            &tree,
            "#main { background-color: #ff0000; } .card { background-color: #00ff00; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].background_color, Rgba::opaque(0xff, 0, 0));
    }

    #[test]
    fn later_source_order_wins_a_specificity_tie() {
        let tree: Element = view! { <div class="a b" /> };
        let (arena, computed) = styles(
            &tree,
            ".a { background-color: #ff0000; } .b { background-color: #00ff00; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].background_color, Rgba::opaque(0, 0xff, 0));
    }

    #[test]
    fn hover_state_overrides_the_base_rule_via_higher_specificity() {
        let tree: Element = view! { <button class="primary">{"Go"}</button> };
        let css =
            ".primary { background-color: #42734f; } .primary:hover { background-color: #345c3e; }";

        let (arena, base) = styles(&tree, css, &InteractionState::new());
        let button = arena.roots()[0];
        assert_eq!(
            base[&button].background_color,
            Rgba::opaque(0x42, 0x73, 0x4f)
        );

        let hovered_state = InteractionState::new().with_hovered(button);
        let hovered = compute(
            &arena,
            &crate::stylesheet_parse::parse_stylesheet(css).unwrap(),
            &hovered_state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            hovered[&button].background_color,
            Rgba::opaque(0x34, 0x5c, 0x3e)
        );
    }

    #[test]
    fn focus_visible_matches_only_when_the_caller_marks_it_so() {
        let tree: Element = view! { <button class="primary">{"Go"}</button> };
        let css = ".primary { background-color: #42734f; } .primary:focus-visible { background-color: #345c3e; }";
        let rules = crate::stylesheet_parse::parse_stylesheet(css).unwrap();

        let (arena, base) = styles(&tree, css, &InteractionState::new());
        let button = arena.roots()[0];
        assert_eq!(
            base[&button].background_color,
            Rgba::opaque(0x42, 0x73, 0x4f)
        );

        // Focused via a mouse click: :focus matches, :focus-visible must not.
        let mouse_focused_state = InteractionState::new().with_focused(button);
        let mouse_focused = compute(
            &arena,
            &rules,
            &mouse_focused_state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            mouse_focused[&button].background_color,
            Rgba::opaque(0x42, 0x73, 0x4f),
            ":focus-visible must not match a pointer-origin focus"
        );

        // Focused via keyboard traversal: both :focus and :focus-visible match.
        let keyboard_focused_state = InteractionState::new()
            .with_focused(button)
            .with_focus_visible(button);
        let keyboard_focused = compute(
            &arena,
            &rules,
            &keyboard_focused_state,
            Viewport::default(),
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(
            keyboard_focused[&button].background_color,
            Rgba::opaque(0x34, 0x5c, 0x3e)
        );
    }

    #[test]
    fn disabled_and_enabled_match_a_buttons_markup_state_directly() {
        let tree: Element = view! {
            <div>
                <button disabled="true" class="a">{"A"}</button>
                <button class="b">{"B"}</button>
            </div>
        };
        let css = ".a, .b { background-color: #111111; } \
                   button:disabled { background-color: #ff0000; } \
                   button:enabled { background-color: #00ff00; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let disabled_button = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let enabled_button = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();

        assert_eq!(
            computed[&disabled_button].background_color,
            Rgba::opaque(0xff, 0, 0),
            ":disabled must match a button with disabled=\"true\", with no InteractionState involved"
        );
        assert_eq!(
            computed[&enabled_button].background_color,
            Rgba::opaque(0, 0xff, 0),
            ":enabled must match a button that isn't disabled"
        );
    }

    fn radius_of(css: &str) -> Corners<(LengthPercentage, LengthPercentage)> {
        let tree: Element = view! { <div class="a" /> };
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let id = arena.roots()[0];
        computed[&id].border_radius
    }

    fn lp(length: f32, percentage: f32) -> LengthPercentage {
        LengthPercentage { length, percentage }
    }

    #[test]
    fn border_radius_defaults_to_zero() {
        let zero = (lp(0.0, 0.0), lp(0.0, 0.0));
        let radius = radius_of(".a { width: 10px; }");
        assert_eq!(radius.top_left, zero);
        assert_eq!(radius.bottom_right, zero);
    }

    #[test]
    fn border_radius_shorthand_expands_one_to_four_values_clockwise() {
        let r = |px| (lp(px, 0.0), lp(px, 0.0));
        let one = radius_of(".a { border-radius: 8px; }");
        assert_eq!(
            [
                one.top_left,
                one.top_right,
                one.bottom_right,
                one.bottom_left
            ],
            [r(8.0); 4]
        );
        let four = radius_of(".a { border-radius: 1px 2px 3px 4px; }");
        assert_eq!(four.top_left, r(1.0));
        assert_eq!(four.top_right, r(2.0));
        assert_eq!(four.bottom_right, r(3.0));
        assert_eq!(four.bottom_left, r(4.0));
    }

    #[test]
    fn border_radius_slash_form_is_elliptical_and_percentages_stay_unresolved() {
        let radius = radius_of(".a { border-radius: 10px 50% / 20px 25%; }");
        assert_eq!(radius.top_left, (lp(10.0, 0.0), lp(20.0, 0.0)));
        assert_eq!(radius.top_right, (lp(0.0, 0.5), lp(0.0, 0.25)));
    }

    #[test]
    fn border_radius_longhand_sets_only_its_own_corner() {
        let radius = radius_of(".a { border-bottom-left-radius: 6px 3px; }");
        assert_eq!(radius.bottom_left, (lp(6.0, 0.0), lp(3.0, 0.0)));
        assert_eq!(radius.top_left, (lp(0.0, 0.0), lp(0.0, 0.0)));
    }

    #[test]
    fn border_radius_calc_splits_into_length_and_percentage() {
        let radius = radius_of(".a { border-top-left-radius: calc(4px + 10%); }");
        let (horizontal, _) = radius.top_left;
        assert_eq!(horizontal.length, 4.0);
        assert!((horizontal.percentage - 0.1).abs() < 1e-4);
    }

    #[test]
    fn a_two_hop_adjacent_sibling_chain_matches() {
        let tree: Element = view! {
            <div>
                <input type="checkbox" disabled="true" class="a" />
                <span class="b" />
                <span class="c" />
            </div>
        };
        let css = ".c { background-color: #111111; } \
                   input:disabled + .b + .c { background-color: #00ff00; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let c = arena
            .find(|a, id| a.classes(id).iter().any(|cl| cl == "c"))
            .unwrap();
        assert_eq!(computed[&c].background_color, Rgba::opaque(0, 0xff, 0));
    }

    #[test]
    fn disabled_matches_a_range_input_directly() {
        let tree: Element = view! {
            <div>
                <input type="range" disabled="true" class="a" />
                <input type="range" class="b" />
            </div>
        };
        let css = ".a, .b { background-color: #111111; } \
                   input:disabled { background-color: #ff0000; } \
                   input:enabled { background-color: #00ff00; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let disabled = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let enabled = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();
        assert_eq!(
            computed[&disabled].background_color,
            Rgba::opaque(0xff, 0, 0)
        );
        assert_eq!(
            computed[&enabled].background_color,
            Rgba::opaque(0, 0xff, 0)
        );
    }

    #[test]
    fn checked_and_disabled_match_a_checkboxs_markup_state_directly() {
        let tree: Element = view! {
            <div>
                <input type="checkbox" checked="true" class="a" />
                <input type="checkbox" disabled="true" class="b" />
            </div>
        };
        let css = ".a, .b { background-color: #111111; } \
                   input:checked { background-color: #00ff00; } \
                   input:disabled { background-color: #ff0000; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let checked_input = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let disabled_input = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();

        assert_eq!(
            computed[&checked_input].background_color,
            Rgba::opaque(0, 0xff, 0),
            ":checked must match a checkbox with checked=\"true\""
        );
        assert_eq!(
            computed[&disabled_input].background_color,
            Rgba::opaque(0xff, 0, 0),
            ":disabled must also match a disabled checkbox, unlike a disabled non-button/checkbox \
             element"
        );
    }

    #[test]
    fn indeterminate_matches_a_checkbox_but_never_a_radio() {
        let tree: Element = view! {
            <div>
                <input type="checkbox" indeterminate="true" class="a" />
                <input type="radio" indeterminate="true" class="b" />
            </div>
        };
        let css = "input { background-color: #111111; } \
                   input:indeterminate { background-color: #00ff00; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let checkbox = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let radio = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();
        assert_eq!(
            computed[&checkbox].background_color,
            Rgba::opaque(0, 0xff, 0)
        );
        assert_eq!(
            computed[&radio].background_color,
            Rgba::opaque(0x11, 0x11, 0x11),
            "indeterminate is a checkbox-only IDL property in real HTML, never a radio's"
        );
    }

    #[test]
    fn checked_matches_a_radio() {
        let tree: Element = view! {
            <div>
                <input type="radio" checked="true" class="a" />
                <input type="radio" class="b" />
            </div>
        };
        let css =
            "input { background-color: #111111; } input:checked { background-color: #00ff00; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let on = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let off = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();
        assert_eq!(computed[&on].background_color, Rgba::opaque(0, 0xff, 0));
        assert_eq!(
            computed[&off].background_color,
            Rgba::opaque(0x11, 0x11, 0x11)
        );
    }

    #[test]
    fn disabled_has_no_effect_on_a_non_button_element() {
        let tree: Element = view! { <div disabled="true" class="a" /> };
        let css = ".a { background-color: #111111; } div:disabled { background-color: #ff0000; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].background_color,
            Rgba::opaque(0x11, 0x11, 0x11),
            "v1 scope: disabled is only wired into ElementState on <button>"
        );
    }

    #[test]
    fn text_decoration_underline_reads_the_declared_value() {
        let tree: Element = view! {
            <div>
                <span class="a" />
                <span class="b" />
            </div>
        };
        let css = ".a { text-decoration: underline; } .b { text-decoration: line-through; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let underlined = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let not_underlined = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();
        assert!(computed[&underlined].text_decoration_underline);
        assert!(
            !computed[&not_underlined].text_decoration_underline,
            "line-through alone must not read as underline"
        );
    }

    #[test]
    fn cursor_pointer_reads_the_declared_value() {
        let tree: Element = view! {
            <div>
                <span class="a" />
                <span class="b" />
            </div>
        };
        let css = ".a { cursor: pointer; } .b { cursor: text; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let pointer = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "a"))
            .unwrap();
        let not_pointer = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == "b"))
            .unwrap();
        assert!(computed[&pointer].cursor_pointer);
        assert!(!computed[&not_pointer].cursor_pointer);
    }

    #[test]
    fn an_unvisited_link_matches_link_not_visited() {
        let tree: Element = view! { <a href="https://example.com" class="a" /> };
        let css = "a:link { background-color: #00ff00; } \
                   a:visited { background-color: #ff0000; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let a = arena.roots()[0];
        assert_eq!(computed[&a].background_color, Rgba::opaque(0, 0xff, 0));
    }

    #[test]
    fn a_visited_link_matches_visited_not_link() {
        let tree: Element = view! { <a href="https://example.com" class="a" /> };
        let css = "a:link { background-color: #00ff00; } \
                   a:visited { background-color: #ff0000; }";
        let visited = HashSet::from(["https://example.com".to_string()]);
        let state = InteractionState::new().with_visited(&visited);
        let (arena, computed) = styles(&tree, css, &state);
        let a = arena.roots()[0];
        assert_eq!(computed[&a].background_color, Rgba::opaque(0xff, 0, 0));
    }

    #[test]
    fn an_a_with_no_href_matches_neither_link_nor_visited() {
        let tree: Element = view! { <a class="a" /> };
        let css = "a { background-color: #111111; } \
                   a:link { background-color: #00ff00; } \
                   a:visited { background-color: #ff0000; }";
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        let a = arena.roots()[0];
        assert_eq!(
            computed[&a].background_color,
            Rgba::opaque(0x11, 0x11, 0x11),
            "an <a> with no href is not a hyperlink at all"
        );
    }

    #[test]
    fn unmatched_node_gets_initial_values() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(
            &tree,
            ".unused { background-color: #ff0000; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].background_color, Rgba::TRANSPARENT);
        assert_eq!(computed[&node].color, Rgba::opaque(0, 0, 0));
    }

    #[test]
    fn width_and_height_default_to_auto() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].width, None);
        assert_eq!(computed[&node].height, None);
    }

    #[test]
    fn overflow_visible_is_the_default_and_does_not_clip() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(!computed[&node].overflow_clips);
    }

    #[test]
    fn overflow_hidden_on_either_axis_alone_clips() {
        let tree: Element = view! { <div class="x" /> };
        let (arena, computed) = styles(
            &tree,
            ".x { overflow-x: hidden; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].overflow_clips);

        let tree: Element = view! { <div class="y" /> };
        let (arena, computed) = styles(
            &tree,
            ".y { overflow-y: hidden; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].overflow_clips);
    }

    #[test]
    fn overflow_hidden_clips_but_does_not_scroll() {
        let tree: Element = view! { <div class="x" /> };
        let (arena, computed) = styles(&tree, ".x { overflow: hidden; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(computed[&node].overflow_clips);
        assert!(!computed[&node].overflow_scrolls_x);
        assert!(!computed[&node].overflow_scrolls_y);
    }

    #[test]
    fn overflow_scroll_and_auto_clip_and_scroll_independently_per_axis() {
        let tree: Element = view! { <div class="x" /> };
        let (arena, computed) = styles(
            &tree,
            ".x { overflow-x: scroll; overflow-y: auto; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].overflow_clips);
        assert!(computed[&node].overflow_scrolls_x);
        assert!(computed[&node].overflow_scrolls_y);

        // `hidden` (unlike the unset default `visible`) isn't promoted to
        // `auto` by the other axis's own non-`visible` value — see
        // `overflow_clips`'s own doc on that promotion rule — so this pair
        // stays a real per-axis split: `x` scrolls, `y` only clips.
        let tree: Element = view! { <div class="y" /> };
        let (arena, computed) = styles(
            &tree,
            ".y { overflow-x: scroll; overflow-y: hidden; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].overflow_scrolls_x);
        assert!(
            !computed[&node].overflow_scrolls_y,
            "overflow-y: hidden must not report as scrollable just because overflow-x does"
        );
    }

    #[test]
    fn opacity_defaults_to_fully_opaque() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].opacity, 1.0);
    }

    #[test]
    fn an_explicit_opacity_resolves_to_its_own_value() {
        let tree: Element = view! { <div class="ghost" /> };
        let (arena, computed) = styles(&tree, ".ghost { opacity: 0.4; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].opacity, 0.4);
    }

    #[test]
    fn an_out_of_range_opacity_clamps_to_0_1() {
        let tree: Element = view! { <div class="over" /> };
        let (arena, computed) = styles(&tree, ".over { opacity: 3; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].opacity, 1.0);
    }

    #[test]
    fn container_type_defaults_to_normal() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].container_type, ContainerType::Normal);
    }

    #[test]
    fn container_type_resolves_inline_size_and_size_from_real_css() {
        let tree: Element = view! { <div class="a" /> };
        let (arena, computed) = styles(
            &tree,
            ".a { container-type: inline-size; }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].container_type,
            ContainerType::InlineSize
        );

        let tree: Element = view! { <div class="b" /> };
        let (arena, computed) = styles(
            &tree,
            ".b { container-type: size; }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].container_type,
            ContainerType::Size
        );
    }

    #[test]
    fn container_name_defaults_to_empty_and_resolves_from_real_css() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        assert!(computed[&arena.roots()[0]].container_name.is_empty());

        let tree: Element = view! { <div class="sidebar" /> };
        let (arena, computed) = styles(
            &tree,
            ".sidebar { container-name: sidebar; }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].container_name,
            vec!["sidebar".to_string()]
        );
    }

    #[test]
    fn z_index_defaults_to_auto() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].z_index, None);
    }

    #[test]
    fn an_explicit_z_index_resolves_to_its_own_integer_including_negative() {
        let tree: Element = view! {
            <div class="back" />
        };
        let (arena, computed) = styles(&tree, ".back { z-index: -2; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].z_index, Some(-2));
    }

    #[test]
    fn a_real_inline_style_attribute_resolves_and_wins_over_a_class_rule() {
        let tree: Element = view! { <div class="card" style="height: 1900px;" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { width: 200px; height: 100px; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].width,
            Some(200.0),
            "an unrelated class rule still applies"
        );
        assert_eq!(
            computed[&node].height,
            Some(1900.0),
            "a real style=\"...\" attribute is the highest-specificity declaration, \
             it must win over a class rule for the same property"
        );
    }

    #[test]
    fn explicit_size_and_box_model_resolve_correctly() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { width: 200px; height: 100px; padding-top: 8px; margin-left: 4px; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let style = &computed[&node];
        assert_eq!(style.width, Some(200.0));
        assert_eq!(style.height, Some(100.0));
        assert_eq!(style.padding.top, 8.0);
        assert_eq!(style.padding.left, 0.0, "unset padding edges default to 0");
        assert_eq!(style.margin.left, Some(4.0));
        assert_eq!(
            style.margin.top,
            Some(0.0),
            "unset margin edges default to 0, not auto"
        );
    }

    #[test]
    fn max_size_resolves_lengths_and_ignores_none() {
        let tree: Element = view! { <div class="a" /> };
        let (arena, computed) = styles(
            &tree,
            ".a { max-width: 317px; max-height: 40px; }",
            &InteractionState::new(),
        );
        let style = &computed[&arena.roots()[0]];
        assert_eq!(style.max_width, Some(317.0));
        assert_eq!(style.max_height, Some(40.0));

        let (arena, computed) = styles(&tree, ".a { max-width: none; }", &InteractionState::new());
        assert_eq!(computed[&arena.roots()[0]].max_width, None);
    }

    #[test]
    fn box_sizing_is_content_box_unless_a_style_says_border_box_and_does_not_inherit() {
        let tree: Element = view! {
            <div class="outer">
                <div class="inner" />
            </div>
        };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        assert_eq!(
            computed[&arena.roots()[0]].box_sizing,
            BoxSizing::ContentBox
        );

        let (arena, computed) = styles(
            &tree,
            ".outer { box-sizing: border-box; }",
            &InteractionState::new(),
        );
        let outer = arena.roots()[0];
        assert_eq!(computed[&outer].box_sizing, BoxSizing::BorderBox);
        assert_eq!(
            computed[&arena.children(outer)[0]].box_sizing,
            BoxSizing::ContentBox,
            "box-sizing does not inherit"
        );

        let (arena, computed) = styles(
            &tree,
            ".outer { box-sizing: border-box; } .outer { box-sizing: content-box; }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].box_sizing,
            BoxSizing::ContentBox
        );
    }

    #[test]
    fn min_size_keeps_lengths_and_percentages_and_treats_auto_as_none() {
        let tree: Element = view! { <div class="a" /> };
        let (arena, computed) = styles(
            &tree,
            ".a { min-width: 120px; min-height: 25%; }",
            &InteractionState::new(),
        );
        let style = &computed[&arena.roots()[0]];
        assert_eq!(
            style.min_width,
            Some(LengthPercentage {
                length: 120.0,
                percentage: 0.0
            })
        );
        assert_eq!(
            style.min_height,
            Some(LengthPercentage {
                length: 0.0,
                percentage: 0.25
            })
        );

        let (arena, computed) = styles(
            &tree,
            ".a { min-width: calc(50% + 10px); }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].min_width,
            Some(LengthPercentage {
                length: 10.0,
                percentage: 0.5
            })
        );

        let (arena, computed) = styles(&tree, ".a { min-width: auto; }", &InteractionState::new());
        assert_eq!(computed[&arena.roots()[0]].min_width, None);
    }

    #[test]
    fn explicit_auto_margin_is_distinguishable_from_unset() {
        let tree: Element = view! { <div class="centered" /> };
        let (arena, computed) = styles(
            &tree,
            ".centered { margin-left: auto; margin-right: auto; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].margin.left, None);
        assert_eq!(computed[&node].margin.right, None);
    }

    #[test]
    fn position_defaults_to_static() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].position, Position::Static);
    }

    #[test]
    fn position_relative_and_absolute_are_read() {
        let tree: Element = view! {
            <div class="rel">
                <div class="abs" />
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".rel { position: relative; } .abs { position: absolute; }",
            &InteractionState::new(),
        );
        let rel = arena.roots()[0];
        let abs = arena.children(rel)[0];
        assert_eq!(computed[&rel].position, Position::Relative);
        assert_eq!(computed[&abs].position, Position::Absolute);
    }

    #[test]
    fn position_fixed_and_sticky_collapse_to_static() {
        let tree: Element = view! {
            <div>
                <div class="fixed" />
                <div class="sticky" />
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".fixed { position: fixed; } .sticky { position: sticky; }",
            &InteractionState::new(),
        );
        let root = arena.roots()[0];
        let (fixed, sticky) = (arena.children(root)[0], arena.children(root)[1]);
        assert_eq!(
            computed[&fixed].position,
            Position::Static,
            "fixed isn't supported yet -- inert, not silently absolute"
        );
        assert_eq!(computed[&sticky].position, Position::Static);
    }

    #[test]
    fn inset_edges_are_read_when_positioned() {
        let tree: Element = view! { <div class="abs" /> };
        let (arena, computed) = styles(
            &tree,
            ".abs { position: absolute; top: 4px; right: 8px; bottom: 12px; left: 16px; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let inset = &computed[&node].inset;
        let px = |length| LengthPercentage {
            length,
            percentage: 0.0,
        };
        assert_eq!(inset.top, Some(px(4.0)));
        assert_eq!(inset.right, Some(px(8.0)));
        assert_eq!(inset.bottom, Some(px(12.0)));
        assert_eq!(inset.left, Some(px(16.0)));
    }

    #[test]
    fn unset_inset_edges_default_to_auto() {
        let tree: Element = view! { <div class="abs" /> };
        let (arena, computed) = styles(
            &tree,
            ".abs { position: absolute; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].inset.top, None);
    }

    #[test]
    fn a_percentage_inset_edge_keeps_its_percentage_unresolved() {
        let tree: Element = view! { <div class="abs" /> };
        let (arena, computed) = styles(
            &tree,
            ".abs { position: absolute; left: 25%; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].inset.left,
            Some(LengthPercentage {
                length: 0.0,
                percentage: 0.25,
            })
        );
    }

    #[test]
    fn size_and_box_properties_do_not_inherit() {
        let tree: Element = view! {
            <div class="card">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card { width: 200px; padding-top: 8px; }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].width, None);
        assert_eq!(computed[&span].padding.top, 0.0);
    }

    #[test]
    fn font_size_inherits_but_an_explicit_value_overrides_it() {
        let tree: Element = view! {
            <div class="card">
                <span>{"inherited"}</span>
                <span class="big">{"overridden"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card { font-size: 24px; } .big { font-size: 32px; }",
            &InteractionState::new(),
        );
        let card = arena.roots()[0];
        assert_eq!(computed[&card].font_size, 24.0);

        let mut spans = arena.children(card).iter().copied();
        let plain = spans.next().unwrap();
        let big = spans.next().unwrap();
        assert_eq!(computed[&plain].font_size, 24.0);
        assert_eq!(computed[&big].font_size, 32.0);
    }

    #[test]
    fn font_weight_defaults_to_400_and_resolves_bold_from_real_css() {
        let tree: Element = view! { <div class="bold" /> };
        let (arena, computed) = styles(
            &tree,
            ".bold { font-weight: bold; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].font_weight, 700.0);

        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        assert_eq!(computed[&arena.roots()[0]].font_weight, 400.0);
    }

    #[test]
    fn font_size_defaults_to_sixteen_pixels() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].font_size, 16.0);
    }

    #[test]
    fn descendant_combinator_matches_any_depth_not_just_direct_children() {
        let tree: Element = view! {
            <div class="card">
                <div>
                    <button>{"Go"}</button>
                </div>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card button { background-color: #42734f; }",
            &InteractionState::new(),
        );
        let button = arena.find(|a, id| a.tag(id) == "button").unwrap();
        assert_eq!(
            computed[&button].background_color,
            Rgba::opaque(0x42, 0x73, 0x4f)
        );
    }

    #[test]
    fn descendant_combinator_requires_the_ancestor_to_exist() {
        let tree: Element = view! {
            <div>
                <button>{"Go"}</button>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card button { background-color: #42734f; }",
            &InteractionState::new(),
        );
        let button = arena.find(|a, id| a.tag(id) == "button").unwrap();
        assert_eq!(computed[&button].background_color, Rgba::TRANSPARENT);
    }

    #[test]
    fn display_defaults_to_block() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].display, Display::Block);
    }

    #[test]
    fn display_flex_is_read_back_from_real_css() {
        let tree: Element = view! { <div class="row" /> };
        let (arena, computed) = styles(&tree, ".row { display: flex; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].display, Display::Flex);
    }

    #[test]
    fn display_does_not_inherit() {
        let tree: Element = view! {
            <div class="row">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(&tree, ".row { display: flex; }", &InteractionState::new());
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(
            computed[&span].display,
            Display::Block,
            "a flex container's own display must not leak onto its children"
        );
    }

    #[test]
    fn flex_direction_and_wrap_resolve_from_real_css() {
        let tree: Element = view! { <div class="row" /> };
        let (arena, computed) = styles(
            &tree,
            ".row { flex-direction: column-reverse; flex-wrap: wrap; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].flex_direction, FlexDirection::ColumnReverse);
        assert_eq!(computed[&node].flex_wrap, FlexWrap::Wrap);
    }

    #[test]
    fn flex_direction_and_wrap_default_to_row_and_nowrap() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].flex_direction, FlexDirection::Row);
        assert_eq!(computed[&node].flex_wrap, FlexWrap::NoWrap);
    }

    #[test]
    fn justify_content_and_align_items_resolve_from_real_css() {
        let tree: Element = view! { <div class="row" /> };
        let (arena, computed) = styles(
            &tree,
            ".row { justify-content: space-between; align-items: center; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].justify_content,
            Some(ContentAlignment::SpaceBetween)
        );
        assert_eq!(computed[&node].align_items, Some(ItemAlignment::Center));
    }

    #[test]
    fn justify_content_and_align_items_default_to_none() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].justify_content, None,
            "CSS's own initial value is `normal`, not an explicit keyword"
        );
        assert_eq!(computed[&node].align_items, None);
    }

    #[test]
    fn align_self_resolves_independently_of_the_parents_align_items() {
        let tree: Element = view! {
            <div class="row">
                <span class="odd-one-out">{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".row { align-items: flex-start; } .odd-one-out { align-self: flex-end; }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].align_self, Some(ItemAlignment::FlexEnd));
    }

    #[test]
    fn flex_grow_shrink_and_basis_resolve_from_real_css() {
        let tree: Element = view! { <div class="item" /> };
        let (arena, computed) = styles(
            &tree,
            ".item { flex-grow: 2; flex-shrink: 0; flex-basis: 50px; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].flex_grow, 2.0);
        assert_eq!(computed[&node].flex_shrink, 0.0);
        assert_eq!(computed[&node].flex_basis, Some(50.0));
    }

    #[test]
    fn flex_grow_shrink_and_basis_default_to_css_initial_values() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].flex_grow, 0.0, "CSS's own initial value");
        assert_eq!(
            computed[&node].flex_shrink, 1.0,
            "flex items shrink by default in real CSS"
        );
        assert_eq!(computed[&node].flex_basis, None, "auto by default");
    }

    #[test]
    fn a_percentage_size_is_kept_as_a_fraction_and_a_length_is_not() {
        let tree: Element = view! {
            <div>
                <div class="pct" />
                <div class="len" />
                <div class="mixed" />
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".pct { width: 68%; height: 100%; } .len { width: 10px; height: 20px; }              .mixed { width: calc(50% + 4px); }",
            &InteractionState::new(),
        );
        let [pct, len, mixed] = arena.children(arena.roots()[0]) else {
            panic!("three children");
        };
        let (pct, len, mixed) = (&computed[pct], &computed[len], &computed[mixed]);
        assert_eq!((pct.width, pct.width_percent), (None, Some(0.68)));
        assert_eq!((pct.height, pct.height_percent), (None, Some(1.0)));
        assert_eq!((len.width, len.width_percent), (Some(10.0), None));
        assert_eq!((len.height, len.height_percent), (Some(20.0), None));
        assert_eq!((mixed.width, mixed.width_percent), (None, None));
    }

    #[test]
    fn text_align_inherits_and_a_button_is_centered_unless_it_says_otherwise() {
        let tree: Element = view! {
            <div>
                <div class="right"><span>{"a"}</span></div>
                <button>{"b"}</button>
                <button class="left">{"c"}</button>
                <p>{"d"}</p>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".right { text-align: right; } .left { text-align: left; }",
            &InteractionState::new(),
        );
        let [right, button, left, plain] = arena.children(arena.roots()[0]) else {
            panic!("four children");
        };
        let span = arena.children(*right)[0];
        assert_eq!(computed[right].text_align, TextAlign::End);
        assert_eq!(computed[&span].text_align, TextAlign::End, "inherited");
        assert_eq!(computed[button].text_align, TextAlign::Center);
        assert_eq!(computed[left].text_align, TextAlign::Start);
        assert_eq!(computed[plain].text_align, TextAlign::Start);
    }

    #[test]
    fn the_glass_material_is_opt_in_with_defaults_and_reports_why_it_is_refused() {
        let tree: Element = view! {
            <div>
                <div class="none" />
                <div class="bare" />
                <div class="full" />
                <div class="bad-length" />
                <div class="bad-quality" />
                <div class="bad-strength" />
                <div class="other" />
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".bare { --florui-glass: refract; }              .full { --florui-glass: refract; --florui-glass-refraction: 8px;                --florui-glass-edge: 14px; --florui-glass-light-angle: 90deg;                --florui-glass-light-strength: 0.5; --florui-glass-quality: reduced; }              .bad-length { --florui-glass: refract; --florui-glass-refraction: wide; }              .bad-quality { --florui-glass: refract; --florui-glass-quality: best; }              .bad-strength { --florui-glass: refract; --florui-glass-light-strength: 2; }              .other { --florui-glass: shiny; }",
            &InteractionState::new(),
        );
        let kids = arena.children(arena.roots()[0]);
        let glass = |i: usize| computed[&kids[i]].glass;

        assert_eq!(glass(0), GlassSpec::None, "absent is no material");
        assert_eq!(
            glass(1),
            GlassSpec::Material(GlassMaterial {
                refraction: 12.0,
                edge: 20.0,
                light_angle: 315.0,
                light_strength: 0.0,
                quality: GlassQuality::Full,
            })
        );
        assert_eq!(
            glass(2),
            GlassSpec::Material(GlassMaterial {
                refraction: 8.0,
                edge: 14.0,
                light_angle: 90.0,
                light_strength: 0.5,
                quality: GlassQuality::Reduced,
            })
        );
        for (index, name) in [(3, "refraction"), (4, "quality"), (5, "light-strength")] {
            match glass(index) {
                GlassSpec::Invalid(reason) => assert!(reason.contains(name), "{reason}"),
                other => panic!("expected an invalid request, got {other:?}"),
            }
        }
        assert_eq!(glass(6), GlassSpec::None, "only refract opts in");
    }

    #[test]
    fn the_glass_material_inherits_like_any_custom_property_and_can_be_reset() {
        let tree: Element = view! {
            <div class="panel"><div class="child" /><div class="reset" /></div>
        };
        let (arena, computed) = styles(
            &tree,
            ".panel { --florui-glass: refract; } .reset { --florui-glass: none; }",
            &InteractionState::new(),
        );
        let kids = arena.children(arena.roots()[0]);
        assert!(matches!(computed[&kids[0]].glass, GlassSpec::Material(_)));
        assert_eq!(computed[&kids[1]].glass, GlassSpec::None);
    }

    fn layers(css: &str) -> Vec<crate::BackgroundLayer> {
        let tree: Element = view! { <div class="a" /> };
        let (arena, computed) = styles(&tree, css, &InteractionState::new());
        computed[&arena.roots()[0]].background_layers.clone()
    }

    #[test]
    fn a_linear_gradient_keeps_its_direction_stops_and_hints() {
        use crate::{BackgroundImage, GradientItem, LinearDirection};
        let red = Rgba::opaque(255, 0, 0);
        let blue = Rgba::opaque(0, 0, 255);
        let only = |css: &str| match layers(css).remove(0).image {
            BackgroundImage::Linear(gradient) => gradient,
            other => panic!("not a linear gradient: {other:?}"),
        };

        let plain = only(".a { background-image: linear-gradient(red, blue); }");
        assert_eq!(
            plain.direction,
            LinearDirection::Angle(180.0),
            "the default is to bottom"
        );
        assert!(!plain.repeating);
        assert_eq!(
            plain.items,
            vec![
                GradientItem::Stop {
                    color: red,
                    position: None
                },
                GradientItem::Stop {
                    color: blue,
                    position: None
                },
            ]
        );

        assert_eq!(
            only(".a { background-image: linear-gradient(45deg, red, blue); }").direction,
            LinearDirection::Angle(45.0)
        );
        for (keyword, degrees) in [
            ("top", 0.0),
            ("right", 90.0),
            ("bottom", 180.0),
            ("left", 270.0),
        ] {
            assert_eq!(
                only(&format!(
                    ".a {{ background-image: linear-gradient(to {keyword}, red, blue); }}"
                ))
                .direction,
                LinearDirection::Angle(degrees),
                "to {keyword}"
            );
        }
        assert_eq!(
            only(".a { background-image: linear-gradient(to bottom right, red, blue); }").direction,
            LinearDirection::Corner {
                right: true,
                bottom: true
            }
        );
        assert_eq!(
            only(".a { background-image: linear-gradient(to top left, red, blue); }").direction,
            LinearDirection::Corner {
                right: false,
                bottom: false
            }
        );

        let positioned = only(
            ".a { background-image: repeating-linear-gradient(red 10px, 30%, blue calc(50% + 4px)); }",
        );
        assert!(positioned.repeating);
        assert_eq!(
            positioned.items,
            vec![
                GradientItem::Stop {
                    color: red,
                    position: Some(lp(10.0, 0.0))
                },
                GradientItem::Hint(lp(0.0, 0.3)),
                GradientItem::Stop {
                    color: blue,
                    position: Some(lp(4.0, 0.5))
                },
            ]
        );
    }

    #[test]
    fn a_gradient_color_can_be_currentcolor_or_translucent() {
        use crate::{BackgroundImage, GradientItem};
        let layer = layers(
            ".a { color: #102030; background-image: linear-gradient(currentcolor, rgba(255, 0, 0, 0.5)); }",
        )
        .remove(0);
        let BackgroundImage::Linear(gradient) = layer.image else {
            panic!("linear")
        };
        assert_eq!(
            gradient.items,
            vec![
                GradientItem::Stop {
                    color: Rgba::opaque(0x10, 0x20, 0x30),
                    position: None
                },
                GradientItem::Stop {
                    color: Rgba {
                        r: 255,
                        g: 0,
                        b: 0,
                        a: 128
                    },
                    position: None
                },
            ]
        );
    }

    #[test]
    fn a_radial_gradient_keeps_its_shape_size_and_center() {
        use crate::{BackgroundImage, RadialExtent, RadialSize};
        let only = |css: &str| match layers(css).remove(0).image {
            BackgroundImage::Radial(gradient) => gradient,
            other => panic!("not a radial gradient: {other:?}"),
        };
        let default = only(".a { background-image: radial-gradient(red, blue); }");
        assert_eq!(
            default.size,
            RadialSize::Extent {
                circle: false,
                extent: RadialExtent::FarthestCorner
            }
        );
        assert_eq!(default.center, (lp(0.0, 0.5), lp(0.0, 0.5)));

        let circle =
            only(".a { background-image: radial-gradient(circle 40px at 10px 25%, red, blue); }");
        assert_eq!(circle.size, RadialSize::Circle(40.0));
        assert_eq!(circle.center, (lp(10.0, 0.0), lp(0.0, 0.25)));

        let ellipse = only(".a { background-image: radial-gradient(30px 50%, red, blue); }");
        assert_eq!(
            ellipse.size,
            RadialSize::Ellipse(lp(30.0, 0.0), lp(0.0, 0.5))
        );

        let extent = only(
            ".a { background-image: repeating-radial-gradient(circle closest-side at right bottom, red, blue); }",
        );
        assert_eq!(
            extent.size,
            RadialSize::Extent {
                circle: true,
                extent: RadialExtent::ClosestSide
            }
        );
        assert_eq!(extent.center, (lp(0.0, 1.0), lp(0.0, 1.0)));
        assert!(extent.repeating);
    }

    #[test]
    fn a_conic_gradient_keeps_its_start_center_and_stops_in_turns() {
        use crate::{BackgroundImage, ConicItem};
        let BackgroundImage::Conic(gradient) = layers(
            ".a { background-image: conic-gradient(from 90deg at 20% 30%, red 0deg, 25%, blue 180deg, green); }",
        )
        .remove(0)
        .image
        else {
            panic!("conic")
        };
        assert_eq!(gradient.from, 90.0);
        assert_eq!(gradient.center, (lp(0.0, 0.2), lp(0.0, 0.3)));
        assert_eq!(
            gradient.items,
            vec![
                ConicItem::Stop {
                    color: Rgba::opaque(255, 0, 0),
                    position: Some(0.0)
                },
                ConicItem::Hint(0.25),
                ConicItem::Stop {
                    color: Rgba::opaque(0, 0, 255),
                    position: Some(0.5)
                },
                ConicItem::Stop {
                    color: Rgba::opaque(0, 128, 0),
                    position: None
                },
            ]
        );
    }

    #[test]
    fn background_layers_take_their_properties_in_turn() {
        use crate::{BackgroundBox, BackgroundRepeat, BackgroundSize};
        let all = layers(
            ".a { background-image: linear-gradient(red, blue), none, radial-gradient(red, blue), linear-gradient(red, blue);              background-size: 20px 30px, cover;              background-position: 10px 5px, right bottom;              background-repeat: no-repeat, repeat-x, space round;              background-origin: content-box;              background-clip: padding-box, border-box; }",
        );
        // `none` makes no layer but keeps its place in the cycle.
        assert_eq!(all.len(), 3);
        assert_eq!(
            all[0].size,
            BackgroundSize::Explicit(Some(lp(20.0, 0.0)), Some(lp(30.0, 0.0)))
        );
        assert_eq!(all[0].position, (lp(10.0, 0.0), lp(5.0, 0.0)));
        assert_eq!(
            all[0].repeat,
            (BackgroundRepeat::NoRepeat, BackgroundRepeat::NoRepeat)
        );
        assert_eq!(all[0].clip, BackgroundBox::Padding);
        // The third image is index 2: its size is the first again, its repeat
        // is the third (`space round`), its clip the first.
        assert_eq!(
            all[1].size,
            BackgroundSize::Explicit(Some(lp(20.0, 0.0)), Some(lp(30.0, 0.0)))
        );
        assert_eq!(
            all[1].repeat,
            (BackgroundRepeat::Space, BackgroundRepeat::Round)
        );
        assert_eq!(all[1].origin, BackgroundBox::Content);
        // The fourth is index 3: second size and position, first repeat, second clip.
        assert_eq!(all[2].size, BackgroundSize::Cover);
        assert_eq!(all[2].position, (lp(0.0, 1.0), lp(0.0, 1.0)));
        assert_eq!(all[2].clip, BackgroundBox::Border);
    }

    #[test]
    fn a_url_layer_keeps_its_path_or_its_data_and_its_place_among_the_others() {
        use crate::{BackgroundImage, BackgroundRepeat};
        let all = layers(
            ".a { background-image: url(textures/paper.png), linear-gradient(red, blue), url(\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg'/>\");              background-repeat: repeat-x, no-repeat; }",
        );
        assert_eq!(all.len(), 3);
        assert_eq!(
            all[0].image,
            BackgroundImage::Url("textures/paper.png".to_string())
        );
        assert!(matches!(all[1].image, BackgroundImage::Linear(_)));
        let BackgroundImage::Url(data) = &all[2].image else {
            panic!("a url")
        };
        assert!(data.starts_with("data:image/svg+xml;utf8,<svg"), "{data}");
        // The properties cycle against the images, url ones included.
        assert_eq!(
            all[0].repeat,
            (BackgroundRepeat::Repeat, BackgroundRepeat::NoRepeat)
        );
        assert_eq!(
            all[1].repeat,
            (BackgroundRepeat::NoRepeat, BackgroundRepeat::NoRepeat)
        );
        assert_eq!(
            all[2].repeat,
            (BackgroundRepeat::Repeat, BackgroundRepeat::NoRepeat)
        );
    }

    #[test]
    fn text_shadow_keeps_its_layers_offsets_blur_and_color_and_inherits() {
        use crate::TextShadow;
        let tree: Element = view! {
            <div class="none"></div>
            <div class="outer"><span class="inner"></span></div>
            <div class="own"></div>
        };
        let (arena, computed) = styles(
            &tree,
            ".outer { color: #102030; text-shadow: 2px 3px 4px rgba(255, 0, 0, 0.5), -1px 0 currentcolor; }              .own { text-shadow: 1px 1px #00ff00; } .reset { text-shadow: none; }",
            &InteractionState::new(),
        );
        let roots = arena.roots();
        assert!(
            computed[&roots[0]].text_shadow.is_empty(),
            "none by default"
        );
        let outer = &computed[&roots[1]].text_shadow;
        assert_eq!(
            outer[..],
            [
                TextShadow {
                    offset_x: 2.0,
                    offset_y: 3.0,
                    blur_radius: 4.0,
                    color: Rgba {
                        r: 255,
                        g: 0,
                        b: 0,
                        a: 128
                    }
                },
                TextShadow {
                    offset_x: -1.0,
                    offset_y: 0.0,
                    blur_radius: 0.0,
                    color: Rgba::opaque(0x10, 0x20, 0x30)
                },
            ]
        );
        let inner = arena.children(roots[1])[0];
        assert_eq!(&computed[&inner].text_shadow, outer, "inherited");
        assert_eq!(
            computed[&roots[2]].text_shadow[0].color,
            Rgba::opaque(0, 255, 0),
            "a shadow with no blur"
        );
    }

    #[test]
    fn a_node_without_a_background_image_has_no_layers() {
        assert!(layers(".a { background-color: red; }").is_empty());
        assert!(layers(".a { background-image: none; }").is_empty());
    }

    #[test]
    fn the_fast_backdrop_blur_is_opt_in_inherits_and_can_be_reset() {
        let tree: Element = view! {
            <div class="none"></div>
            <div class="panel"><div class="child" /><div class="reset" /></div>
            <div class="loud"></div>
            <div class="typo"></div>
        };
        let (arena, computed) = styles(
            &tree,
            ".panel { --florui-backdrop-blur: fast; } .reset { --florui-backdrop-blur: exact; }              .loud { --florui-backdrop-blur: FAST; } .typo { --florui-backdrop-blur: faster; }",
            &InteractionState::new(),
        );
        let roots = arena.roots();
        let blur = |node| computed[&node].backdrop_blur;
        assert_eq!(blur(roots[0]), BackdropBlur::Exact, "absent is exact");
        assert_eq!(blur(roots[1]), BackdropBlur::Fast);
        let kids = arena.children(roots[1]);
        assert_eq!(blur(kids[0]), BackdropBlur::Fast, "inherited");
        assert_eq!(blur(kids[1]), BackdropBlur::Exact, "reset by a child");
        assert_eq!(blur(roots[2]), BackdropBlur::Fast, "keywords ignore case");
        assert_eq!(
            blur(roots[3]),
            BackdropBlur::Exact,
            "an unknown value keeps the exact blur"
        );
    }

    #[test]
    fn gap_resolves_from_real_css() {
        let tree: Element = view! { <div class="row" /> };
        let (arena, computed) = styles(
            &tree,
            ".row { column-gap: 12px; row-gap: 4px; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].column_gap, 12.0);
        assert_eq!(computed[&node].row_gap, 4.0);
    }

    #[test]
    fn gap_defaults_to_zero() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].column_gap, 0.0);
        assert_eq!(computed[&node].row_gap, 0.0);
    }

    #[test]
    fn font_family_defaults_to_sans_serif() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].font_family, FontFamily::SansSerif);
    }

    #[test]
    fn font_family_monospace_is_read_back_from_real_css() {
        let tree: Element = view! { <div class="code" /> };
        let (arena, computed) = styles(
            &tree,
            ".code { font-family: monospace; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].font_family, FontFamily::Monospace);
    }

    #[test]
    fn font_family_named_falls_back_to_sans_serif() {
        // This crate has no embedded font backing an arbitrary requested
        // name — it must fall back to its own default rather than erroring
        // or silently picking something else unpredictable.
        let tree: Element = view! { <div class="fancy" /> };
        let (arena, computed) = styles(
            &tree,
            ".fancy { font-family: Helvetica; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].font_family, FontFamily::SansSerif);
    }

    #[test]
    fn font_family_inherits_but_an_explicit_value_overrides_it() {
        let tree: Element = view! {
            <div class="code">
                <span>{"inherited"}</span>
                <span class="prose">{"overridden"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".code { font-family: monospace; } .prose { font-family: sans-serif; }",
            &InteractionState::new(),
        );
        let code = arena.roots()[0];
        assert_eq!(computed[&code].font_family, FontFamily::Monospace);

        let mut spans = arena.children(code).iter().copied();
        let inherited = spans.next().unwrap();
        let overridden = spans.next().unwrap();
        assert_eq!(computed[&inherited].font_family, FontFamily::Monospace);
        assert_eq!(computed[&overridden].font_family, FontFamily::SansSerif);
    }

    #[test]
    fn border_resolves_width_and_color_per_side_from_real_css() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { border-top-width: 2px; border-top-style: solid; border-top-color: #ff0000; \
             border-left-width: 3px; border-left-style: solid; border-left-color: #00ff00; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let border = computed[&node].border;
        assert_eq!(border.top.width, 2.0);
        assert_eq!(border.top.color, Rgba::opaque(0xff, 0x00, 0x00));
        assert_eq!(border.left.width, 3.0);
        assert_eq!(border.left.color, Rgba::opaque(0x00, 0xff, 0x00));
    }

    /// Real CSS's own initial `border-style` is `none`, which makes a
    /// border invisible regardless of any `border-width`/`border-color`
    /// also set — an explicit width with no style set must still resolve
    /// to a `0.0`-width side, not a visible one.
    #[test]
    fn a_border_with_no_style_declared_is_invisible_despite_an_explicit_width() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { border-top-width: 5px; border-top-color: #ff0000; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].border.top.width, 0.0,
            "no border-style means border-style: none, which is always invisible"
        );
    }

    /// `border-style: none` explicitly set must behave the same as never
    /// setting a style at all.
    #[test]
    fn a_border_explicitly_set_to_none_is_invisible() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { border-top-width: 5px; border-top-style: none; border-top-color: #ff0000; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].border.top.width, 0.0);
    }

    #[test]
    fn border_color_of_currentcolor_resolves_against_this_elements_own_color() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { color: #123456; border-top-width: 1px; border-top-style: solid; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].border.top.color,
            Rgba::opaque(0x12, 0x34, 0x56),
            "no border-color declared means currentcolor, the real CSS initial value"
        );
    }

    #[test]
    fn border_defaults_to_invisible_on_every_side_with_zero_author_css() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        let border = computed[&node].border;
        assert_eq!(border.top.width, 0.0);
        assert_eq!(border.right.width, 0.0);
        assert_eq!(border.bottom.width, 0.0);
        assert_eq!(border.left.width, 0.0);
    }

    #[test]
    fn display_grid_resolves_from_real_css() {
        let tree: Element = view! { <div class="g" /> };
        let (arena, computed) = styles(&tree, ".g { display: grid; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].display, Display::Grid);
    }

    #[test]
    fn grid_template_columns_resolves_lengths_and_fr_units() {
        let tree: Element = view! { <div class="g" /> };
        let (arena, computed) = styles(
            &tree,
            ".g { display: grid; grid-template-columns: 100px 1fr 2fr; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].grid_template_columns,
            vec![
                GridTrackSize::Length(100.0),
                GridTrackSize::Fr(1.0),
                GridTrackSize::Fr(2.0),
            ]
        );
    }

    #[test]
    fn grid_template_rows_resolves_auto_and_keyword_tracks() {
        let tree: Element = view! { <div class="g" /> };
        let (arena, computed) = styles(
            &tree,
            ".g { display: grid; grid-template-rows: auto min-content max-content; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].grid_template_rows,
            vec![
                GridTrackSize::Auto,
                GridTrackSize::MinContent,
                GridTrackSize::MaxContent,
            ]
        );
    }

    #[test]
    fn grid_template_tracks_default_to_empty_with_zero_author_css() {
        let tree: Element = view! { <div class="g" /> };
        let (arena, computed) = styles(&tree, ".g { display: grid; }", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(computed[&node].grid_template_columns.is_empty());
        assert!(computed[&node].grid_template_rows.is_empty());
    }

    #[test]
    fn grid_column_and_row_resolve_line_and_span_placement() {
        let tree: Element = view! { <div class="item" /> };
        let (arena, computed) = styles(
            &tree,
            ".item { grid-column: 2 / 4; grid-row: 1 / span 2; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].grid_column,
            (GridPlacement::Line(2), GridPlacement::Line(4))
        );
        assert_eq!(
            computed[&node].grid_row,
            (GridPlacement::Line(1), GridPlacement::Span(2))
        );
    }

    #[test]
    fn grid_column_and_row_default_to_auto() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].grid_column,
            (GridPlacement::Auto, GridPlacement::Auto)
        );
        assert_eq!(
            computed[&node].grid_row,
            (GridPlacement::Auto, GridPlacement::Auto)
        );
    }

    #[test]
    fn box_shadow_resolves_offsets_blur_spread_and_color_from_real_css() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { box-shadow: 2px 4px 6px 1px #ff0000; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let shadows = &computed[&node].box_shadow;
        assert_eq!(shadows.len(), 1);
        assert_eq!(shadows[0].offset_x, 2.0);
        assert_eq!(shadows[0].offset_y, 4.0);
        assert_eq!(shadows[0].blur_radius, 6.0);
        assert_eq!(shadows[0].spread_radius, 1.0);
        assert_eq!(shadows[0].color, Rgba::opaque(0xff, 0x00, 0x00));
        assert!(!shadows[0].inset);
    }

    #[test]
    fn box_shadow_defaults_to_an_empty_list_with_zero_author_css() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(computed[&node].box_shadow.is_empty());
    }

    #[test]
    fn box_shadow_inset_keyword_resolves_to_true() {
        let tree: Element = view! { <div class="well" /> };
        let (arena, computed) = styles(
            &tree,
            ".well { box-shadow: inset 0px 2px 0px 0px #000000; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].box_shadow[0].inset);
    }

    #[test]
    fn box_shadow_resolves_multiple_comma_separated_layers_in_source_order() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { box-shadow: 1px 1px 0px 0px #ff0000, 2px 2px 0px 0px #00ff00; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let shadows = &computed[&node].box_shadow;
        assert_eq!(shadows.len(), 2);
        assert_eq!(shadows[0].color, Rgba::opaque(0xff, 0x00, 0x00));
        assert_eq!(shadows[1].color, Rgba::opaque(0x00, 0xff, 0x00));
    }

    #[test]
    fn box_shadow_color_of_currentcolor_resolves_against_this_elements_own_color() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { color: #123456; box-shadow: 0px 0px 0px 0px; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].box_shadow[0].color,
            Rgba::opaque(0x12, 0x34, 0x56),
            "no explicit shadow color declared means currentcolor, the real CSS initial value"
        );
    }

    #[test]
    fn box_shadow_does_not_inherit() {
        let tree: Element = view! {
            <div class="card">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card { box-shadow: 2px 2px 2px 0px #ff0000; }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert!(computed[&span].box_shadow.is_empty());
    }

    #[test]
    fn transform_defaults_to_none() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(computed[&node].transform.is_empty());
    }

    #[test]
    fn transform_origin_defaults_to_the_boxs_own_center() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        let (x, y) = computed[&node].transform_origin;
        assert_eq!(
            x,
            LengthPercentage {
                length: 0.0,
                percentage: 0.5
            }
        );
        assert_eq!(
            y,
            LengthPercentage {
                length: 0.0,
                percentage: 0.5
            }
        );
    }

    #[test]
    fn object_fit_defaults_to_fill() {
        let tree: Element = view! { <img /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(computed[&node].object_fit, ObjectFit::Fill);
    }

    #[test]
    fn object_fit_reads_each_real_css_keyword() {
        for (keyword, expected) in [
            ("contain", ObjectFit::Contain),
            ("cover", ObjectFit::Cover),
            ("none", ObjectFit::None),
            ("scale-down", ObjectFit::ScaleDown),
        ] {
            let tree: Element = view! { <img class="pic" /> };
            let (arena, computed) = styles(
                &tree,
                &format!(".pic {{ object-fit: {keyword}; }}"),
                &InteractionState::new(),
            );
            let node = arena.roots()[0];
            assert_eq!(
                computed[&node].object_fit, expected,
                "object-fit: {keyword}"
            );
        }
    }

    #[test]
    fn object_position_defaults_to_centered() {
        let tree: Element = view! { <img /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        let (x, y) = computed[&node].object_position;
        assert_eq!(
            x,
            LengthPercentage {
                length: 0.0,
                percentage: 0.5
            }
        );
        assert_eq!(
            y,
            LengthPercentage {
                length: 0.0,
                percentage: 0.5
            }
        );
    }

    #[test]
    fn object_position_reads_an_explicit_length_and_percentage() {
        let tree: Element = view! { <img class="pic" /> };
        let (arena, computed) = styles(
            &tree,
            ".pic { object-position: 10px 20%; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let (x, y) = computed[&node].object_position;
        assert_eq!(
            x,
            LengthPercentage {
                length: 10.0,
                percentage: 0.0
            }
        );
        assert_eq!(
            y,
            LengthPercentage {
                length: 0.0,
                percentage: 0.2
            }
        );
    }

    #[test]
    fn aspect_ratio_defaults_to_bare_auto() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].aspect_ratio,
            AspectRatio {
                prefers_intrinsic: true,
                ratio: None,
            }
        );
    }

    #[test]
    fn aspect_ratio_of_a_bare_ratio_never_prefers_intrinsic() {
        let tree: Element = view! { <img class="pic" /> };
        let (arena, computed) = styles(
            &tree,
            ".pic { aspect-ratio: 16 / 9; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].aspect_ratio,
            AspectRatio {
                prefers_intrinsic: false,
                ratio: Some((16.0, 9.0)),
            }
        );
    }

    #[test]
    fn aspect_ratio_of_auto_with_a_ratio_prefers_intrinsic_but_keeps_the_fallback() {
        let tree: Element = view! { <img class="pic" /> };
        let (arena, computed) = styles(
            &tree,
            ".pic { aspect-ratio: auto 4 / 3; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].aspect_ratio,
            AspectRatio {
                prefers_intrinsic: true,
                ratio: Some((4.0, 3.0)),
            }
        );
    }

    #[test]
    fn translate_resolves_to_its_own_length_and_percentage() {
        let tree: Element = view! { <div class="moved" /> };
        let (arena, computed) = styles(
            &tree,
            ".moved { transform: translate(10px, 25%); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].transform,
            vec![TransformFunction::Translate(
                LengthPercentage {
                    length: 10.0,
                    percentage: 0.0
                },
                LengthPercentage {
                    length: 0.0,
                    percentage: 0.25
                },
            )]
        );
    }

    #[test]
    fn translate_x_and_translate_y_each_leave_the_other_axis_at_zero() {
        let tree: Element = view! { <div class="x" /> };
        let (arena, computed) = styles(
            &tree,
            ".x { transform: translateX(5px); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].transform,
            vec![TransformFunction::Translate(
                LengthPercentage {
                    length: 5.0,
                    percentage: 0.0
                },
                LengthPercentage::default(),
            )]
        );
    }

    #[test]
    fn scale_x_and_scale_y_default_the_other_axis_to_1() {
        let tree: Element = view! { <div class="x" /> };
        let (arena, computed) = styles(
            &tree,
            ".x { transform: scaleX(2); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].transform,
            vec![TransformFunction::Scale(2.0, 1.0)]
        );
    }

    #[test]
    fn rotate_resolves_to_degrees_regardless_of_the_authored_angle_unit() {
        let tree: Element = view! { <div class="turned" /> };
        let (arena, computed) = styles(
            &tree,
            ".turned { transform: rotate(0.5turn); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].transform,
            vec![TransformFunction::Rotate(180.0)]
        );
    }

    #[test]
    fn matrix_resolves_to_its_own_six_components_in_css_order() {
        let tree: Element = view! { <div class="m" /> };
        let (arena, computed) = styles(
            &tree,
            ".m { transform: matrix(1, 2, 3, 4, 5, 6); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].transform,
            vec![TransformFunction::Matrix {
                a: 1.0,
                b: 2.0,
                c: 3.0,
                d: 4.0,
                e: 5.0,
                f: 6.0,
            }]
        );
    }

    #[test]
    fn multiple_transform_functions_resolve_in_authored_order() {
        let tree: Element = view! { <div class="both" /> };
        let (arena, computed) = styles(
            &tree,
            ".both { transform: translateX(5px) scale(2); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].transform,
            vec![
                TransformFunction::Translate(
                    LengthPercentage {
                        length: 5.0,
                        percentage: 0.0
                    },
                    LengthPercentage::default(),
                ),
                TransformFunction::Scale(2.0, 2.0),
            ]
        );
    }

    #[test]
    fn an_explicit_transform_origin_resolves_to_its_own_percentage() {
        let tree: Element = view! { <div class="pivot" /> };
        let (arena, computed) = styles(
            &tree,
            ".pivot { transform-origin: 0% 100%; }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        let (x, y) = computed[&node].transform_origin;
        assert_eq!(
            x,
            LengthPercentage {
                length: 0.0,
                percentage: 0.0
            }
        );
        assert_eq!(
            y,
            LengthPercentage {
                length: 0.0,
                percentage: 1.0
            }
        );
    }

    #[test]
    fn a_skew_function_is_dropped_as_an_unsupported_2d_only_gap() {
        let tree: Element = view! { <div class="skewed" /> };
        let (arena, computed) = styles(
            &tree,
            ".skewed { transform: skewX(20deg); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].transform.is_empty());
    }

    #[test]
    fn transform_does_not_inherit() {
        let tree: Element = view! {
            <div class="moved">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".moved { transform: translate(10px, 10px); }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert!(computed[&span].transform.is_empty());
    }

    #[test]
    fn filter_defaults_to_none() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(computed[&node].filter.is_empty());
    }

    #[test]
    fn blur_resolves_to_its_own_pixel_radius() {
        let tree: Element = view! { <div class="soft" /> };
        let (arena, computed) = styles(
            &tree,
            ".soft { filter: blur(4px); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(computed[&node].filter, vec![FilterFunction::Blur(4.0)]);
    }

    #[test]
    fn brightness_contrast_and_saturate_resolve_to_their_own_factors() {
        let tree: Element = view! { <div class="adjusted" /> };
        let (arena, computed) = styles(
            &tree,
            ".adjusted { filter: brightness(1.5) contrast(0.8) saturate(2); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].filter,
            vec![
                FilterFunction::Brightness(1.5),
                FilterFunction::Contrast(0.8),
                FilterFunction::Saturate(2.0),
            ]
        );
    }

    #[test]
    fn a_grayscale_function_is_dropped_as_an_unsupported_gap() {
        let tree: Element = view! { <div class="gray" /> };
        let (arena, computed) = styles(
            &tree,
            ".gray { filter: grayscale(1); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert!(computed[&node].filter.is_empty());
    }

    #[test]
    fn filter_does_not_inherit() {
        let tree: Element = view! {
            <div class="soft">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".soft { filter: blur(4px); }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert!(computed[&span].filter.is_empty());
    }

    #[test]
    fn backdrop_filter_defaults_to_none() {
        let tree: Element = view! { <div /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        let node = arena.roots()[0];
        assert!(computed[&node].backdrop_filter.is_empty());
    }

    #[test]
    fn backdrop_filter_resolves_the_same_subset_as_filter() {
        let tree: Element = view! { <div class="glass" /> };
        let (arena, computed) = styles(
            &tree,
            ".glass { backdrop-filter: blur(10px) brightness(1.2); }",
            &InteractionState::new(),
        );
        let node = arena.roots()[0];
        assert_eq!(
            computed[&node].backdrop_filter,
            vec![FilterFunction::Blur(10.0), FilterFunction::Brightness(1.2)]
        );
        assert!(computed[&node].filter.is_empty());
    }

    #[test]
    fn backdrop_filter_does_not_inherit() {
        let tree: Element = view! {
            <div class="glass">
                <span>{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".glass { backdrop-filter: blur(10px); }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert!(computed[&span].backdrop_filter.is_empty());
    }

    // CSS custom properties (`--foo`) and `var()` need no conversion code
    // of this crate's own: Stylo's real cascade already substitutes them
    // before any longhand (here `background-color`) resolves its own
    // value, so these tests exist to prove and pin that behavior, not to
    // exercise anything florui-style itself implements.
    #[test]
    fn a_custom_property_resolves_via_var_on_the_same_element() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { --brand: #ff0000; background-color: var(--brand); }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].background_color,
            Rgba::opaque(0xff, 0, 0)
        );
    }

    #[test]
    fn a_custom_property_inherits_to_a_child_that_reads_it_via_var() {
        let tree: Element = view! {
            <div class="card">
                <span class="mirror">{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".card { --brand: #ff0000; } .mirror { background-color: var(--brand); }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].background_color, Rgba::opaque(0xff, 0, 0));
    }

    #[test]
    fn var_falls_back_to_its_own_default_when_the_custom_property_is_undeclared() {
        let tree: Element = view! { <div class="card" /> };
        let (arena, computed) = styles(
            &tree,
            ".card { background-color: var(--missing, #00ff00); }",
            &InteractionState::new(),
        );
        assert_eq!(
            computed[&arena.roots()[0]].background_color,
            Rgba::opaque(0, 0xff, 0)
        );
    }

    #[test]
    fn a_custom_property_declared_on_root_reaches_every_descendant() {
        let tree: Element = view! {
            <div class="card">
                <span class="mirror">{"x"}</span>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ":root { --brand: #ff0000; } .mirror { background-color: var(--brand); }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].background_color, Rgba::opaque(0xff, 0, 0));
    }

    #[test]
    fn a_descendant_redeclaring_a_custom_property_overrides_it_for_its_own_subtree() {
        let tree: Element = view! {
            <div class="outer">
                <div class="inner">
                    <span class="mirror">{"x"}</span>
                </div>
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".outer { --brand: #ff0000; } .inner { --brand: #00ff00; } .mirror { background-color: var(--brand); }",
            &InteractionState::new(),
        );
        let span = arena.find(|a, id| a.tag(id) == "span").unwrap();
        assert_eq!(computed[&span].background_color, Rgba::opaque(0, 0xff, 0));
    }

    #[test]
    fn appearance_defaults_to_auto_with_zero_author_css() {
        let tree: Element = view! { <select /> };
        let (arena, computed) = styles(&tree, "", &InteractionState::new());
        assert_eq!(computed[&arena.roots()[0]].appearance, Appearance::Auto);
    }

    #[test]
    fn florui_appearance_none_reads_as_appearance_none() {
        let tree: Element = view! { <select class="plain" /> };
        let (arena, computed) = styles(
            &tree,
            ".plain { --florui-appearance: none; }",
            &InteractionState::new(),
        );
        assert_eq!(computed[&arena.roots()[0]].appearance, Appearance::None);
    }

    #[test]
    fn florui_appearance_inherits_to_a_descendant_like_any_other_custom_property() {
        let tree: Element = view! {
            <div class="outer">
                <select />
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".outer { --florui-appearance: none; }",
            &InteractionState::new(),
        );
        let select = arena.find(|a, id| a.tag(id) == "select").unwrap();
        assert_eq!(computed[&select].appearance, Appearance::None);
    }

    #[test]
    fn florui_appearance_with_any_other_value_reads_as_auto() {
        let tree: Element = view! { <select class="odd" /> };
        let (arena, computed) = styles(
            &tree,
            ".odd { --florui-appearance: auto; }",
            &InteractionState::new(),
        );
        assert_eq!(computed[&arena.roots()[0]].appearance, Appearance::Auto);
    }

    fn smooth_of(css: &str, tree: &Element, class: &str) -> bool {
        let (arena, computed) = styles(tree, css, &InteractionState::new());
        let node = arena
            .find(|a, id| a.classes(id).iter().any(|c| c == class))
            .unwrap();
        computed[&node].scroll_behavior_smooth
    }

    #[test]
    fn the_florui_properties_read_alongside_unrelated_custom_properties() {
        let tree: Element = view! {
            <div class="theme">
                <textarea class="all" />
                <textarea class="none" />
            </div>
        };
        let (arena, computed) = styles(
            &tree,
            ".theme { --brand: #ff0000; } \
             .all { --florui-appearance: none; --florui-resize: vertical; \
                    --florui-placeholder-color: #112233; --florui-scroll-behavior: smooth; }",
            &InteractionState::new(),
        );
        let find = |class: &str| {
            arena
                .find(|a, id| a.classes(id).iter().any(|c| c == class))
                .unwrap()
        };
        let all = &computed[&find("all")];
        assert_eq!(all.appearance, Appearance::None);
        assert_eq!(all.resize, Resize::Vertical);
        assert_eq!(all.placeholder_color, Some(Rgba::opaque(0x11, 0x22, 0x33)));
        assert!(all.scroll_behavior_smooth);

        // A node whose custom properties are all someone else's keeps the defaults.
        let none = &computed[&find("none")];
        assert_eq!(none.appearance, Appearance::Auto);
        assert_eq!(none.resize, Resize::Both);
        assert_eq!(none.placeholder_color, None);
        assert!(!none.scroll_behavior_smooth);
    }

    #[test]
    fn scroll_behavior_is_auto_until_an_author_asks_for_smooth() {
        let tree: Element = view! { <div class="a" /> };
        assert!(!smooth_of("", &tree, "a"));
        assert!(smooth_of(".a { scroll-behavior: smooth; }", &tree, "a"));
        assert!(!smooth_of(".a { scroll-behavior: auto; }", &tree, "a"));
    }

    #[test]
    fn scroll_behavior_does_not_inherit_like_the_real_property() {
        let tree: Element = view! {
            <div class="outer">
                <div class="inner" />
            </div>
        };
        let css = ".outer { scroll-behavior: smooth; }";
        assert!(smooth_of(css, &tree, "outer"));
        assert!(
            !smooth_of(css, &tree, "inner"),
            "a nested scroller stays auto"
        );
    }

    #[test]
    fn scroll_behavior_inherit_takes_the_parents_value() {
        let tree: Element = view! {
            <div class="outer">
                <div class="inner" />
            </div>
        };
        let css = ".outer { scroll-behavior: smooth; } .inner { scroll-behavior: inherit; }";
        assert!(smooth_of(css, &tree, "inner"));
    }

    #[test]
    fn scroll_behavior_unset_and_initial_are_auto_even_under_a_smooth_parent() {
        let tree: Element = view! {
            <div class="outer">
                <div class="inner" />
            </div>
        };
        for keyword in ["unset", "initial"] {
            let css = format!(
                ".outer {{ scroll-behavior: smooth; }} .inner {{ scroll-behavior: {keyword}; }}"
            );
            assert!(!smooth_of(&css, &tree, "inner"), "{keyword}");
        }
        let css = ".a { scroll-behavior: smooth; } .a { scroll-behavior: unset; }";
        let single: Element = view! { <div class="a" /> };
        assert!(
            !smooth_of(css, &single, "a"),
            "unset after smooth on the same element"
        );
    }

    #[test]
    fn an_invalid_scroll_behavior_is_dropped_and_the_earlier_value_survives() {
        let tree: Element = view! { <div class="a" /> };
        let css = ".a { scroll-behavior: smooth; } .a { scroll-behavior: instant; }";
        assert!(smooth_of(css, &tree, "a"));
    }

    #[test]
    fn scroll_behavior_follows_the_normal_cascade_and_important() {
        let tree: Element = view! { <div class="a b" /> };
        let css = ".a { scroll-behavior: smooth !important; } .a.b { scroll-behavior: auto; }";
        assert!(
            smooth_of(css, &tree, "a"),
            "!important beats higher specificity"
        );
        let css = ".a { scroll-behavior: smooth; } .a.b { scroll-behavior: auto; }";
        assert!(!smooth_of(css, &tree, "a"), "the more specific rule wins");
    }

    #[test]
    fn a_style_attribute_can_set_scroll_behavior() {
        let tree: Element = view! { <div class="a" style="scroll-behavior: smooth" /> };
        assert!(smooth_of("", &tree, "a"));
    }

    #[test]
    fn an_inline_scroll_behavior_does_not_leak_to_a_nested_scroller_either() {
        let tree: Element = view! {
            <div class="outer" style="scroll-behavior: smooth">
                <div class="inner" />
            </div>
        };
        assert!(smooth_of("", &tree, "outer"));
        assert!(!smooth_of("", &tree, "inner"));
    }

    #[test]
    fn scroll_behavior_works_inside_a_media_query() {
        let tree: Element = view! { <div class="a" /> };
        let css = "@media (min-width: 1px) { .a { scroll-behavior: smooth; } }";
        assert!(smooth_of(css, &tree, "a"));
    }

    #[test]
    fn a_min_width_media_query_applies_only_once_the_viewport_is_wide_enough() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules =
            parse_stylesheet("@media (min-width: 500px) { .card { background-color: #ff0000; } }")
                .unwrap();
        let node = arena.roots()[0];

        let narrow = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 400.0,
                height: 300.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(narrow[&node].background_color, Rgba::TRANSPARENT);

        let wide = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 600.0,
                height: 300.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(wide[&node].background_color, Rgba::opaque(0xff, 0, 0));
    }

    #[test]
    fn a_max_width_media_query_stops_applying_once_the_viewport_is_too_wide() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules =
            parse_stylesheet("@media (max-width: 500px) { .card { background-color: #ff0000; } }")
                .unwrap();
        let node = arena.roots()[0];

        let narrow = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 400.0,
                height: 300.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(narrow[&node].background_color, Rgba::opaque(0xff, 0, 0));

        let wide = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 600.0,
                height: 300.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(wide[&node].background_color, Rgba::TRANSPARENT);
    }

    #[test]
    fn a_min_height_media_query_matches_a_tall_enough_viewport() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules =
            parse_stylesheet("@media (min-height: 500px) { .card { background-color: #ff0000; } }")
                .unwrap();
        let node = arena.roots()[0];

        let short = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 900.0,
                height: 400.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(short[&node].background_color, Rgba::TRANSPARENT);

        let tall = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 900.0,
                height: 600.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(tall[&node].background_color, Rgba::opaque(0xff, 0, 0));
    }

    #[test]
    fn a_max_height_media_query_stops_applying_once_the_viewport_is_too_tall() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules =
            parse_stylesheet("@media (max-height: 500px) { .card { background-color: #ff0000; } }")
                .unwrap();
        let node = arena.roots()[0];

        let short = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 900.0,
                height: 400.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(short[&node].background_color, Rgba::opaque(0xff, 0, 0));

        let tall = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport {
                width: 900.0,
                height: 600.0,
            },
            &mut crate::AnimationTimeline::default(),
        );
        assert_eq!(tall[&node].background_color, Rgba::TRANSPARENT);
    }

    #[test]
    fn a_prefers_reduced_motion_media_query_resolves_against_the_real_timeline_value() {
        let tree: Element = view! { <div class="card" /> };
        let rules = parse_stylesheet(
            "@media (prefers-reduced-motion: reduce) { .card { background-color: #ff0000; } }",
        )
        .unwrap();

        for prefers_reduced in [true, false, true] {
            let arena = Arena::build(&tree);
            let node = arena.roots()[0];
            let mut timeline = crate::AnimationTimeline::new();
            timeline.set_os_prefers_reduced_motion(prefers_reduced);
            let computed = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
            );
            let expected = if prefers_reduced {
                Rgba::opaque(0xff, 0, 0)
            } else {
                Rgba::TRANSPARENT
            };
            assert_eq!(computed[&node].background_color, expected);
        }
    }

    #[test]
    fn a_prefers_color_scheme_media_query_resolves_against_the_real_timeline_value() {
        let tree: Element = view! { <div class="card" /> };
        let rules = parse_stylesheet(
            "@media (prefers-color-scheme: dark) { .card { background-color: #ff0000; } }",
        )
        .unwrap();

        for prefers_dark in [true, false, true] {
            let arena = Arena::build(&tree);
            let node = arena.roots()[0];
            let mut timeline = crate::AnimationTimeline::new();
            timeline.set_prefers_dark_color_scheme(prefers_dark);
            let computed = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport::default(),
                &mut timeline,
            );
            let expected = if prefers_dark {
                Rgba::opaque(0xff, 0, 0)
            } else {
                Rgba::TRANSPARENT
            };
            assert_eq!(computed[&node].background_color, expected);
        }
    }

    #[test]
    fn a_theme_change_updates_style_without_resetting_component_state() {
        // Mirrors set_rules_changes_style_without_resetting_component_state
        // (in florui-platform's runtime.rs) at the florui-style layer: the
        // literal "theme changes must invalidate dependent values correctly
        // without remounting component state" requirement.
        let css = "
            .card { background-color: #ffffff; }
            @media (prefers-color-scheme: dark) {
                .card { background-color: #000000; }
            }
        ";
        let rules = parse_stylesheet(css).unwrap();
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let node = arena.roots()[0];
        let mut timeline = crate::AnimationTimeline::new();

        let light = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert_eq!(
            light[&node].background_color,
            Rgba::opaque(0xff, 0xff, 0xff)
        );

        // Same Arena, same AnimationTimeline (the only place component
        // state would live in a real runtime) — only the color-scheme
        // signal changes between these two compute() calls.
        timeline.set_prefers_dark_color_scheme(true);
        let dark = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert_eq!(
            dark[&node].background_color,
            Rgba::opaque(0, 0, 0),
            "a theme change must actually take effect"
        );
    }

    #[test]
    fn a_height_sensitive_stylesheet_is_reused_correctly_across_computes_at_different_heights() {
        let tree: Element = view! { <div class="card" /> };
        let arena = Arena::build(&tree);
        let rules =
            parse_stylesheet("@media (min-height: 500px) { .card { background-color: #ff0000; } }")
                .unwrap();
        let node = arena.roots()[0];

        for height in [400.0, 600.0, 400.0, 600.0] {
            let computed = compute(
                &arena,
                &rules,
                &InteractionState::new(),
                Viewport {
                    width: 900.0,
                    height,
                },
                &mut crate::AnimationTimeline::default(),
            );
            let expected = if height >= 500.0 {
                Rgba::opaque(0xff, 0, 0)
            } else {
                Rgba::TRANSPARENT
            };
            assert_eq!(computed[&node].background_color, expected);
        }
    }

    #[test]
    fn a_background_color_transition_starts_on_a_class_change_and_samples_in_between() {
        let css = "
            .box {
                background-color: #ff0000;
                transition-property: background-color;
                transition-duration: 1s;
                transition-timing-function: linear;
            }
            .box.on { background-color: #0000ff; }
        ";
        let rules = parse_stylesheet(css).unwrap();
        let mut timeline = crate::AnimationTimeline::new();

        let off: Element = view! { <div class="box" /> };
        let off_arena = Arena::build(&off);
        let off_node = off_arena.roots()[0];
        let baseline = compute(
            &off_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert_eq!(
            baseline[&off_node].background_color,
            Rgba::opaque(0xff, 0, 0)
        );

        // Same structural identity (a lone root `<div>`), new class — the
        // transition should just be starting, still at its from-value.
        let on: Element = view! { <div class="box on" /> };
        let on_arena = Arena::build(&on);
        let on_node = on_arena.roots()[0];
        let started = compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert_eq!(started[&on_node].background_color, Rgba::opaque(0xff, 0, 0));

        // Halfway through a 1s linear transition: red and blue only ever
        // differ in the red/blue channels, so this should land near the
        // midpoint of each rather than either endpoint.
        timeline.advance_to(0.5);
        let midway = compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        let sampled = midway[&on_node].background_color;
        assert!(
            (sampled.r as i32 - 128).abs() <= 5 && (sampled.b as i32 - 128).abs() <= 5,
            "expected a color near the midpoint of red and blue, got {sampled:?}"
        );

        // Past the full duration: the transition has finished, so this is
        // just the declared end value again.
        timeline.advance_to(2.0);
        let finished = compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert_eq!(
            finished[&on_node].background_color,
            Rgba::opaque(0, 0, 0xff)
        );
    }

    #[test]
    fn suppressed_motion_snaps_a_transition_straight_to_its_target_with_no_interpolation() {
        let css = "
            .box {
                background-color: #ff0000;
                transition-property: background-color;
                transition-duration: 1s;
                transition-timing-function: linear;
            }
            .box.on { background-color: #0000ff; }
        ";
        let rules = parse_stylesheet(css).unwrap();
        let mut timeline = crate::AnimationTimeline::new();
        timeline.set_os_prefers_reduced_motion(true);

        let off: Element = view! { <div class="box" /> };
        let off_arena = Arena::build(&off);
        compute(
            &off_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );

        let on: Element = view! { <div class="box on" /> };
        let on_arena = Arena::build(&on);
        let on_node = on_arena.roots()[0];
        compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );

        // Halfway through what would be a 1s linear transition — an
        // unsuppressed run (see the test above) samples a color near the
        // red/blue midpoint here. Suppressed, it must already be the plain
        // target value, with no in-between state ever visible.
        timeline.advance_to(0.5);
        let midway = compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert_eq!(
            midway[&on_node].background_color,
            Rgba::opaque(0, 0, 0xff),
            "a suppressed transition must never show an interpolated in-between value"
        );
    }

    #[test]
    fn opting_out_of_auto_suppress_lets_a_transition_animate_even_when_the_os_prefers_reduced_motion()
     {
        let css = "
            .box {
                background-color: #ff0000;
                transition-property: background-color;
                transition-duration: 1s;
                transition-timing-function: linear;
            }
            .box.on { background-color: #0000ff; }
        ";
        let rules = parse_stylesheet(css).unwrap();
        let mut timeline = crate::AnimationTimeline::new();
        timeline.set_os_prefers_reduced_motion(true);
        timeline.set_auto_suppress_motion(false);

        let off: Element = view! { <div class="box" /> };
        let off_arena = Arena::build(&off);
        compute(
            &off_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );

        let on: Element = view! { <div class="box on" /> };
        let on_arena = Arena::build(&on);
        let on_node = on_arena.roots()[0];
        compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );

        timeline.advance_to(0.5);
        let midway = compute(
            &on_arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        let sampled = midway[&on_node].background_color;
        assert!(
            (sampled.r as i32 - 128).abs() <= 5 && (sampled.b as i32 - 128).abs() <= 5,
            "opting out of auto-suppress must restore real interpolation even when the OS \
             prefers reduced motion, got {sampled:?}"
        );
    }

    #[test]
    fn a_keyframes_animation_samples_opacity_partway_through() {
        let css = "
            .box {
                opacity: 0;
                animation-name: fade;
                animation-duration: 10s;
                animation-timing-function: linear;
                animation-fill-mode: forwards;
            }
            @keyframes fade {
                from { opacity: 0; }
                to { opacity: 1; }
            }
        ";
        let rules = parse_stylesheet(css).unwrap();
        let mut timeline = crate::AnimationTimeline::new();
        let tree: Element = view! { <div class="box" /> };
        let arena = Arena::build(&tree);
        let node = arena.roots()[0];

        let start = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert!((start[&node].opacity - 0.0).abs() < 0.01);

        timeline.advance_to(5.0);
        let midway = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert!(
            (midway[&node].opacity - 0.5).abs() < 0.05,
            "expected opacity near 0.5 at the animation's midpoint, got {}",
            midway[&node].opacity
        );

        timeline.advance_to(20.0);
        let finished = compute(
            &arena,
            &rules,
            &InteractionState::new(),
            Viewport::default(),
            &mut timeline,
        );
        assert!((finished[&node].opacity - 1.0).abs() < 0.01);
    }
}
