//! Paints a styled, laid-out `florui_style::Arena` into pixels — the
//! "painting" stage at the end of the pipeline (components → elements →
//! style → boxes/layout → text → **painting**).
//!
//! # Scope
//!
//! Flat background-color rectangles, solid borders, and hard-edged
//! box-shadows, painted in real paint order (see [`paint_order`]): a
//! node before its children, and within one node's own children,
//! document order — except a flex or grid child with an explicit
//! `z-index` reorders among its own siblings the same way real CSS
//! does, the only case florui can express `z-index` for yet (there is
//! no `position` property, so nothing here establishes a positioned
//! element's own stacking context). An overlap always resolves to
//! whichever box paints later. A leaf's own text (see
//! [`florui_style::Arena::text_content`]) paints inside its padding
//! box: glyph outlines come from [`florui_text::Font::shape`] and are
//! rasterized with [skrifa](https://github.com/googlefonts/fontations)'s
//! outline extractor feeding a
//! [tiny-skia](https://github.com/RazrFalcon/tiny-skia) path, the same
//! rasterizer backgrounds/borders/shadows use — one painting backend,
//! not several. A node with `opacity` below `1.0`, a `transform`, a
//! `filter`, or any combination paints itself and its whole subtree into
//! an offscreen buffer first, composited back as one group (see
//! [`paint_group`]) — real CSS's own group-opacity semantics (not a
//! per-primitive alpha multiply) and its own "transform moves the whole
//! rendered result, clip included" rule for transforms, both falling out
//! of the same offscreen-buffer mechanism for free: the buffer already
//! holds the group's content at its normal, untransformed position, so
//! compositing it back with a real matrix instead of the identity one
//! moves everything — background, border, already-clipped descendants —
//! as a single unit, pivoting around [`ComputedStyle::transform_origin`].
//! See [`resolve_transform`] for the supported `transform` subset and its
//! own matrix composition, and [`apply_filters`] for the documented
//! `filter` subset (`blur`/`brightness`/`contrast`/`saturate`), applied to
//! that same buffer before it's composited — real CSS's own
//! filter-then-composite order. `backdrop-filter` (same subset) works the
//! other way: [`apply_backdrop_filter`] samples what's already painted
//! behind a node, filters it, and writes it back before that node's own
//! background/border/content paint on top.
//!
//! `box-shadow`'s `blur-radius` is painted too, via a real Gaussian blur
//! — see [`blur`]'s own module doc: tiny-skia 0.11 (this crate's whole
//! rasterizer) has no blur or mask-filter primitive of its own, so this
//! crate rasterizes the shadow's own shape into a scratch alpha buffer,
//! blurs *that* by hand, and composites the result back onto the canvas
//! pixel by pixel — the one place in this crate that blends manually
//! instead of going through a `tiny_skia::Paint` fill (`filter: blur()`
//! reuses the same hand-rolled Gaussian, run over the group's own already-
//! rasterized pixels instead of a shape rasterized just for it). A node
//! whose `overflow` clips content (see [`clip_for_children`]) restricts
//! its own descendants — never its own border/background — to its
//! padding box; nested clips intersect. `border-radius` rounds a box's
//! background and border (see [`rounded::RoundedRect`]).

mod blur;
mod rounded;

use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use florui_layout::{BoxLayout, absolute_position};
use florui_style::{
    Arena, ComputedStyle, Display, FilterFunction, LengthPercentage, NodeId, ObjectFit, Position,
    Rgba, RoundedRect, TransformFunction,
};
use florui_text::Font;
use rounded::RoundedRectPath;
use skrifa::instance::{LocationRef, NormalizedCoord, Size as GlyphSize};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia::{
    FillRule, IntRect, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, PremultipliedColorU8, Rect,
    Transform,
};

pub type Canvas = Pixmap;

/// The fixed logical (CSS-pixel) size a built-in control decoration (a
/// `<select>`'s own chevron, a checked checkbox's own check mark) is
/// rasterized and painted at — see [`is_control_icon_node`]'s own doc for
/// which nodes this applies to. A pragmatic simplification (no
/// `object-fit`-aware or font-size-relative sizing for UA chrome that has
/// no CSS box of its own to size against), the same category of
/// documented trade-off `RasterFit::Contain` already is for an
/// `<img>`/`<icon>`'s own raster target.
pub const CONTROL_ICON_SIZE: f32 = 12.0;
/// A `<select>`'s own chevron sits this many logical px in from its
/// content box's right edge. Meaningless for a checkbox's check mark,
/// which centers instead — see [`control_icon_rect`].
const CONTROL_ICON_INSET: f32 = 4.0;

/// Whether `node` is a built-in-control-decoration node — a `<select>`
/// (always, regardless of open/closed) or a checked `<input
/// type="checkbox">`. Shared by [`crate::desktop`]'s own registration
/// query (`crate::platform`, not this crate) and this crate's own paint
/// branch, so both agree on exactly which nodes this applies to.
fn is_control_icon_node(arena: &Arena, node: NodeId) -> bool {
    arena.tag(node) == "select"
        || (arena.tag(node) == "input"
            && arena.input_type(node) == Some("checkbox")
            && arena.is_checked(node))
}

/// Where a control decoration paints within its own content box, in
/// logical units relative to the content-box origin (the same convention
/// [`florui_layout::resolve_object_fit_content_rect`] returns for an
/// `<img>`/`<icon>`, consumed identically by [`paint_image`]) — a
/// `<select>`'s chevron sits right-inset and vertically centered; a
/// checkbox's check mark centers on both axes. Real CSS has no
/// `object-fit`-like concept for either (this is UA chrome, not a
/// replaced element's own content), so this is a fixed formula, not a
/// resolved CSS property.
fn control_icon_rect(
    arena: &Arena,
    node: NodeId,
    content_width: f32,
    content_height: f32,
) -> BoxLayout {
    let size = CONTROL_ICON_SIZE;
    let y = ((content_height - size) / 2.0).max(0.0);
    let x = if arena.tag(node) == "select" {
        (content_width - CONTROL_ICON_INSET - size).max(0.0)
    } else {
        ((content_width - size) / 2.0).max(0.0)
    };
    BoxLayout {
        x,
        y,
        width: size,
        height: size,
    }
}

/// One paint target: an owned pixel buffer, plus the absolute canvas
/// pixel its own local `(0, 0)` corresponds to. The root canvas is a
/// `Surface` with `origin: (0, 0)`; [`paint_group`] allocates a smaller
/// one sized to its own subtree's real bounds instead of the whole
/// canvas (see [`group_extent`]) — every painting primitive still
/// computes in absolute coordinates exactly as before, converting to
/// this surface's own local ones only at the last moment, right before
/// the actual pixel write, so a bounded surface paints bit-identical
/// pixels to today's canvas-sized one (see the differential test in this
/// module's own tests).
struct Surface {
    pixmap: Pixmap,
    origin: (i32, i32),
}

impl Surface {
    fn root(pixmap: Pixmap) -> Self {
        Self {
            pixmap,
            origin: (0, 0),
        }
    }

    fn new(width: u32, height: u32, origin: (i32, i32)) -> Option<Self> {
        Pixmap::new(width, height).map(|pixmap| Self { pixmap, origin })
    }

    fn width(&self) -> u32 {
        self.pixmap.width()
    }

    fn height(&self) -> u32 {
        self.pixmap.height()
    }

    /// An absolute canvas coordinate, converted to this surface's own
    /// local pixel space.
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.origin.0 as f32, y - self.origin.1 as f32)
    }
}

/// An axis-aligned clip rectangle in absolute canvas coordinates — every
/// clip this crate ever produces is an intersection of axis-aligned
/// padding boxes in untransformed space (see [`clip_for_children`]), so
/// this reproduces that exactly without needing a rasterized [`Mask`]
/// until a fill actually needs one. Plain `f32` bounds rather than
/// [`tiny_skia::Rect`] specifically so an empty intersection (`x1 <=
/// x0`) stays representable instead of failing to construct — real CSS
/// nests clips that vanish entirely (a clipped-away descendant) as
/// routinely as ones that don't.
///
/// `rounded` holds the rounded padding-box outlines of any
/// `border-radius` clipping ancestors; the rectangle bounds stay
/// axis-aligned and conservative for extent math, and only
/// [`Self::to_mask`] applies the outlines.
#[derive(Clone, Debug, PartialEq)]
struct ClipRect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    rounded: Option<Rc<[RoundedRect]>>,
}

impl ClipRect {
    fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + height,
            rounded: None,
        }
    }

    /// This clip also restricted to `outline`, a rounded box inside it.
    fn with_rounded(mut self, outline: RoundedRect) -> Self {
        let mut outlines: Vec<RoundedRect> = self
            .rounded
            .iter()
            .flat_map(|o| o.iter().copied())
            .collect();
        outlines.push(outline);
        self.rounded = Some(outlines.into());
        self
    }

    fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// This rect intersected with `other` — never fails; an empty result
    /// (`is_empty()`) means nothing under both clips is ever visible,
    /// same as real CSS's own nested `overflow: hidden`.
    fn intersect(&self, other: &ClipRect) -> ClipRect {
        let rounded = match (&self.rounded, &other.rounded) {
            (None, None) => None,
            (Some(only), None) | (None, Some(only)) => Some(Rc::clone(only)),
            (Some(a), Some(b)) => Some(a.iter().chain(b.iter()).copied().collect()),
        };
        ClipRect {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
            rounded,
        }
    }

    /// Materializes a real [`Mask`] the size of `surface`, opaque exactly
    /// where this clip (converted to `surface`'s own local coordinates)
    /// allows painting — `None` for an empty clip, matching every other
    /// "nothing to paint" case in this module (the caller skips the fill
    /// entirely rather than passing a mask that would filter out
    /// everything).
    fn to_mask(&self, surface: &Surface) -> Option<Mask> {
        if self.is_empty() {
            return None;
        }
        let (lx0, ly0) = surface.local(self.x0, self.y0);
        let (lx1, ly1) = surface.local(self.x1, self.y1);
        let rect = Rect::from_ltrb(
            lx0.max(0.0),
            ly0.max(0.0),
            lx1.min(surface.width() as f32),
            ly1.min(surface.height() as f32),
        )?;
        let mut mask = Mask::new(surface.width(), surface.height())?;
        let mut path_builder = PathBuilder::new();
        path_builder.push_rect(rect);
        let path = path_builder.finish()?;
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        let to_local = surface_transform(surface);
        for outline in self.rounded.iter().flat_map(|o| o.iter()) {
            match outline.path() {
                Some(path) => mask.intersect_path(&path, FillRule::Winding, true, to_local),
                None => return None,
            }
        }
        Some(mask)
    }
}

#[derive(Debug)]
pub struct PaintError {
    path: std::path::PathBuf,
    source: png::EncodingError,
}

impl std::fmt::Display for PaintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "could not write {}: {}",
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for PaintError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Paints `width` x `height` physical pixels — `canvas` everywhere, then
/// every node's background and text, in document order — and writes the
/// result as a PNG.
///
/// # Panics
///
/// Panics if `width` or `height` is zero: there is no meaningful canvas to
/// paint into.
#[allow(clippy::too_many_arguments)]
pub fn paint_to_png(
    font: &mut Font,
    path: &Path,
    width: u32,
    height: u32,
    canvas: Rgba,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
) -> Result<(), PaintError> {
    paint_to_buffer(
        font,
        width,
        height,
        canvas,
        arena,
        styles,
        layouts,
        scale_factor,
    )
    .save_png(path)
    .map_err(|source| PaintError {
        path: path.to_owned(),
        source,
    })
}

/// What an editable `<input>` node needs painted for it — the real
/// caret/selection this crate has no other way to know about, since
/// neither lives in `styles`/`layouts` (see `crates/florui-platform/src/text_input.rs`,
/// the only real source of one of these). `runs` are this input's own
/// shaped glyphs — real ones for `type="text"`, a masked substitute (same
/// positions, a "•" glyph swapped in) for `type="password"` — painted
/// exactly like any other text run, no `<input>`-specific rasterization
/// path needed.
pub struct TextInputPaint {
    pub runs: Vec<florui_text::ShapedRun>,
    pub caret_rect: Option<(f32, f32, f32, f32)>,
    pub selection_rects: Vec<(f32, f32, f32, f32)>,
    /// The current IME preedit composition's own area, `None` unless
    /// actually composing — `runs` already includes the real composing
    /// glyphs (Parley splices preedit text straight into the shaped
    /// buffer), so this is only ever used to draw an underline beneath
    /// them, distinguishing an in-progress composition from committed
    /// text.
    pub compose_rect: Option<(f32, f32, f32, f32)>,
    /// `false` while the caret should be hidden this frame (a future
    /// blink timer, or a real Parley IME hidden-cursor request) — distinct
    /// from `caret_rect` being `None` (there's a real geometry answer, it
    /// just shouldn't paint right now).
    pub show_caret: bool,
}

/// A decoded `<img>`'s own real pixels — `object-fit`/`object-position`
/// (already in `styles`, no need to duplicate them here) decide where and
/// at what size they blit into this node's own content box. `Arc`,
/// matching `florui_assets::AssetCache`'s own retention contract: the
/// same handle a `crate::UiRuntime` host holds onto for as long as the
/// image stays loaded, not a fresh copy per paint.
pub type ImagePaint = std::sync::Arc<florui_assets::RasterImage>;

/// Same painting as [`paint_to_png`], returning the pixel buffer directly
/// instead of writing it to disk. `font` is the caller's own long-lived
/// instance — see [`florui_layout::compute_layout`]'s own doc for why, and
/// pass it the exact same instance that computed `layouts`, since this
/// paints the identical glyphs that font already shaped.
///
/// `layouts` and `width`/`height` are in the *painted* canvas's own units —
/// a caller painting a HiDPI window passes boxes already scaled up to
/// physical pixels (its own `scale_layouts`, mirrored in
/// `florui_conformance::engine`), not the logical pixels layout itself ran
/// against. `scale_factor` is how much that scaling multiplied every box
/// by (`1.0` for an unscaled/logical canvas). Text painting splits the
/// difference: it shapes at the *logical* font size and wrap width — the
/// same ones layout itself measured with, recovered by dividing the
/// already-scaled box width back down — so wrapping matches the committed
/// layout exactly, then scales the resulting glyph outlines and positions
/// back up by `scale_factor` only at rasterization time, so the ink
/// itself is painted at the canvas's own (possibly physical) size. Get
/// either half wrong and it shows up differently: skip the wrap-width
/// divide and text re-wraps wider than the box was sized for; skip the
/// rasterization scale-up and text paints correctly wrapped but roughly
/// `scale_factor` times too small.
///
/// # Panics
///
/// Panics if `width` or `height` is zero.
#[allow(clippy::too_many_arguments)]
pub fn paint_to_buffer(
    font: &mut Font,
    width: u32,
    height: u32,
    canvas: Rgba,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
) -> Canvas {
    paint_to_buffer_with_desktop_extras(
        font,
        width,
        height,
        canvas,
        arena,
        styles,
        layouts,
        scale_factor,
        None,
        None,
    )
}

/// Same as [`paint_to_buffer`], plus `text_inputs` (real caret/selection
/// painting for every editable `<input>` node it has an entry for) and
/// `images` (a decoded `<img>`'s own real pixels, blitted per its
/// `object-fit`/`object-position`) — kept as a separate function, rather
/// than changing [`paint_to_buffer`]'s own signature, since both are only
/// needed by a real desktop host and every other caller (conformance,
/// devtools, benches) has no use for either.
#[allow(clippy::too_many_arguments)]
pub fn paint_to_buffer_with_desktop_extras(
    font: &mut Font,
    width: u32,
    height: u32,
    canvas: Rgba,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> Canvas {
    let pixmap = Pixmap::new(width, height)
        .expect("paint_to_buffer_with_desktop_extras requires a nonzero-sized canvas");
    let mut surface = Surface::root(pixmap);
    surface.pixmap.fill(to_tiny_skia_color(canvas));
    paint_nodes(
        &mut surface,
        arena,
        styles,
        layouts,
        font,
        None,
        arena.roots(),
        scale_factor,
        None,
        text_inputs,
        images,
    );
    surface.pixmap
}

/// Paints `nodes` (a set of siblings — document roots when
/// `parent_display` is `None`, one node's own children otherwise) and
/// their descendants onto `buffer`, in real paint order (see
/// [`paint_order`]).
///
/// Iterative for the common case — every node fully opaque and
/// untransformed, the same walk `paint_to_buffer` always did — but
/// recurses once per ancestor whose own [`ComputedStyle::opacity`] is
/// below `1.0`, whose own [`ComputedStyle::transform`] isn't empty, or
/// both, to render that ancestor's whole subtree into its own offscreen
/// buffer before compositing it back as a single group (see this
/// module's own doc on why that's not the same as multiplying each
/// descendant's own paint individually, or transforming each one on its
/// own). Recursion depth tracks the *nesting depth of these groups
/// specifically*, not overall tree depth — a subtree with neither still
/// walks iteratively, the same guarantee this module's own deep-tree test
/// already covers. A pathologically deep chain of nested groups could
/// still overflow the stack; unguarded for now, a smaller and far less
/// likely bound than the plain-tree-depth case that motivated this
/// function's own iterative design in the first place.
///
/// `clip`, if given, restricts every fill in this call (and everything it
/// recurses into) to the pixels it marks visible — inherited from an
/// ancestor whose own `overflow` clips its content; see
/// [`clip_for_children`] for how a node's own `overflow` narrows it
/// further for its own children. Shared via [`Rc`] rather than cloned:
/// most of a tree has no `overflow: hidden` ancestor at all (`clip` stays
/// `None` all the way down, the same zero-cost path as before this
/// existed), and even under one, only a node that *itself* clips ever
/// allocates a new mask — every sibling and descendant that doesn't
/// clip shares the same one.
#[allow(clippy::too_many_arguments)]
fn paint_nodes(
    buffer: &mut Surface,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    parent_display: Option<Display>,
    nodes: &[NodeId],
    scale_factor: f32,
    clip: Option<ClipRect>,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    let mut stack: Vec<(NodeId, Option<ClipRect>)> = paint_order(styles, parent_display, nodes)
        .into_iter()
        .rev()
        .map(|id| (id, clip.clone()))
        .collect();
    while let Some((node, node_clip)) = stack.pop() {
        let opacity = styles.get(&node).map_or(1.0, |s| s.opacity);
        if opacity <= 0.0 {
            // Real CSS: a fully transparent subtree still occupies its
            // own layout box and stays hit-testable, but paints nothing
            // at all — not "paint it and let zero alpha erase it," which
            // would still cost the same work for no visible result.
            continue;
        }
        if node_clip.as_ref().is_some_and(|c| c.is_empty()) {
            // An ancestor's `overflow: hidden` clip has already vanished
            // entirely (fully outside its own visible region) — nothing
            // under it can ever be visible either. Distinct from `None`
            // (no clip at all, paint everywhere): `ClipRect::to_mask`
            // returns `None` for both an absent clip and an empty one, so
            // this has to be checked before converting, not after.
            continue;
        }
        let node_abs = absolute_position(arena, layouts, node);
        let transform = match (styles.get(&node), layouts.get(&node)) {
            (Some(style), Some(&layout)) => {
                resolve_transform(style, &layout, node_abs.0, node_abs.1, scale_factor)
            }
            _ => Transform::identity(),
        };
        let has_filter = styles.get(&node).is_some_and(|s| !s.filter.is_empty());
        if opacity < 1.0 || !transform.is_identity() || has_filter {
            paint_group(
                buffer,
                arena,
                styles,
                layouts,
                font,
                node,
                node_abs,
                scale_factor,
                node_clip,
                opacity,
                transform,
                text_inputs,
                images,
            );
            continue;
        }
        let node_mask = node_clip.as_ref().and_then(|c| c.to_mask(buffer));
        paint_node(
            buffer,
            arena,
            styles,
            layouts,
            font,
            node,
            scale_factor,
            node_mask.as_ref(),
            text_inputs,
            images,
        );
        let child_clip = clip_for_children(arena, styles, layouts, node, node_clip, scale_factor);
        let child_display = styles.get(&node).map(|s| s.display);
        stack.extend(
            paint_order(styles, child_display, arena.children(node))
                .into_iter()
                .rev()
                .map(|id| (id, child_clip.clone())),
        );
    }
}

/// The clip a node's own children paint under: `incoming` unchanged
/// unless `node` itself has [`ComputedStyle::overflow_clips`] set, in
/// which case its own padding box is intersected into it (or becomes the
/// whole clip, if there was no `incoming` one yet). Doesn't affect `node`
/// itself — real CSS's own `overflow` clips a box's *content*, not the
/// box's own border/background, which is why this only ever changes what
/// gets passed down, never what [`paint_node`] used for `node` itself.
///
/// A node's own text is a documented exception: it's painted by
/// [`paint_node`] using the *incoming* clip, not this function's result,
/// so a leaf whose own unbreakable text overflows its own
/// `overflow: hidden` box isn't self-clipped yet — only a *descendant*
/// extending past this box is. Narrower than real CSS, but the common
/// and far more visible case (clipping other elements, e.g. an image
/// container) works correctly today.
fn clip_for_children(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
    incoming: Option<ClipRect>,
    scale_factor: f32,
) -> Option<ClipRect> {
    let clips = styles.get(&node).is_some_and(|s| s.overflow_clips);
    if !clips {
        return incoming;
    }
    let Some(&layout) = layouts.get(&node) else {
        return incoming;
    };
    let (x, y) = absolute_position(arena, layouts, node);
    let border = styles.get(&node).map_or(NO_BORDER, |s| s.border);
    let mut padding_box = ClipRect::from_xywh(
        x + border.left.width,
        y + border.top.width,
        (layout.width - border.left.width - border.right.width).max(0.0),
        (layout.height - border.top.width - border.bottom.width).max(0.0),
    );
    if let Some(style) = styles.get(&node) {
        let outline = RoundedRect::new(
            x,
            y,
            layout.width,
            layout.height,
            &style.border_radius,
            scale_factor,
        )
        .inset(
            border.top.width,
            border.right.width,
            border.bottom.width,
            border.left.width,
        );
        if !outline.is_square() {
            padding_box = padding_box.with_rounded(outline);
        }
    }
    Some(match incoming {
        Some(parent) => parent.intersect(&padding_box),
        None => padding_box,
    })
}

const NO_BORDER: florui_style::Edges<florui_style::BorderSide> = florui_style::Edges {
    top: NO_BORDER_SIDE,
    right: NO_BORDER_SIDE,
    bottom: NO_BORDER_SIDE,
    left: NO_BORDER_SIDE,
};
const NO_BORDER_SIDE: florui_style::BorderSide = florui_style::BorderSide {
    width: 0.0,
    color: Rgba::TRANSPARENT,
};

/// A fixed selection-highlight color for an editable `<input>` — real
/// `caret-color`/`::selection` CSS is a documented, deliberate gap this
/// slice leaves for later; every input highlights identically for now.
const TEXT_INPUT_SELECTION_COLOR: Rgba = Rgba {
    r: 0x3a,
    g: 0x84,
    b: 0xf7,
    a: 0x66,
};
/// Logical pixels — matches a common OS caret width; not spec-mandated to
/// this exact number, the same as `desktop.rs`'s own `WHEEL_LINE_HEIGHT`.
const TEXT_INPUT_CARET_WIDTH: f32 = 1.0;

/// Total padding `filters`' own combined blur reach needs on every side
/// — zero for `brightness`/`contrast`/`saturate`, which are pointwise
/// and never read a neighboring pixel. Matched exhaustively (no wildcard
/// arm) so a future [`FilterFunction`] variant fails to compile here
/// until someone classifies its own extent, rather than silently
/// contributing zero padding for a filter that actually needs some.
fn filter_inflation(filters: &[FilterFunction]) -> f32 {
    filters
        .iter()
        .map(|function| match function {
            FilterFunction::Blur(radius) => blur::kernel_radius(*radius) as f32,
            FilterFunction::Brightness(_)
            | FilterFunction::Contrast(_)
            | FilterFunction::Saturate(_) => 0.0,
        })
        .sum()
}

/// Every pixel `node`'s own subtree could possibly paint, in absolute,
/// pre-`node`'s-own-transform coordinates — the union of `node`'s own
/// border box, each of its own outset `box-shadow` layers (inflated by
/// their own blur), its own text ink, and each child's own contribution.
/// A child that itself starts a nested group is mapped through its own
/// transform (and inflated by its own filter first) before folding into
/// this union — its pixels land at their own transformed position
/// *inside* this still-untransformed surface. A child with
/// [`ComputedStyle::overflow_clips`] narrows what its own children
/// contribute to its own padding box first, mirroring
/// [`clip_for_children`]; a child at `opacity <= 0` contributes nothing,
/// mirroring [`paint_nodes`]'s own skip.
///
/// Reshapes text fresh (the same call [`paint_node`] itself makes, since
/// neither this crate nor `florui_text` caches shaped runs across
/// calls) — a real, measured cost for a text-bearing animated group,
/// accepted here rather than falling back to the canvas-sized surface
/// for every label; see this crate's own paint benchmark for the actual
/// number.
///
/// `None` only when `node` has no entry in `layouts` at all — the same
/// case every other painting function in this module already treats as
/// nothing to paint.
#[allow(clippy::too_many_arguments)]
fn subtree_extent(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    abs_x: f32,
    abs_y: f32,
    scale_factor: f32,
) -> Option<(f32, f32, f32, f32)> {
    let layout = *layouts.get(&node)?;
    let style = styles.get(&node);

    let mut min_x = abs_x;
    let mut min_y = abs_y;
    let mut max_x = abs_x + layout.width;
    let mut max_y = abs_y + layout.height;

    if let Some(style) = style {
        for shadow in style
            .box_shadow
            .iter()
            .filter(|s| !s.inset && s.color.a != 0)
        {
            let (sx, sy, sw, sh) =
                outset_shadow_extent(shadow, abs_x, abs_y, layout.width, layout.height);
            let pad = if shadow.blur_radius > 0.0 {
                blur::kernel_radius(shadow.blur_radius / 2.0) as f32
            } else {
                0.0
            };
            min_x = min_x.min(sx - pad);
            min_y = min_y.min(sy - pad);
            max_x = max_x.max(sx + sw + pad);
            max_y = max_y.max(sy + sh + pad);
        }
    }

    let border = style.map_or(NO_BORDER, |s| s.border);
    let no_padding = florui_style::Edges {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };
    let padding = style.map_or(no_padding, |s| s.padding);
    let content_x = abs_x + border.left.width + padding.left;
    let content_y = abs_y + border.top.width + padding.top;
    let content_width =
        (layout.width - border.left.width - border.right.width - padding.left - padding.right)
            .max(0.0);
    let wrap_width = content_width / scale_factor;
    let font_size = style.map_or(16.0, |s| s.font_size);
    // Glyph ink can overhang its own advance/line box (an italic swash,
    // a tall diacritic) — one `font_size` of slack on every side, same
    // reasoning as `rasterize_and_blur`'s own padding: cheap, and the
    // 1c visibility clamp already tightens this back down wherever the
    // extra room isn't actually visible.
    let ink_pad = font_size * scale_factor;
    let mut shaped_extent = None;
    if florui_layout::is_inline_formatting_context(arena, styles, node) {
        if let Some(shaped) =
            florui_layout::shape_inline_formatting_context(font, arena, styles, node, wrap_width)
        {
            shaped_extent = Some((shaped.width, shaped.height));
        }
    } else {
        let text = arena.text_content(node);
        if !text.is_empty() {
            let font_weight = style.map_or(400.0, |s| s.font_weight);
            let font_family = style.map_or(florui_text::FontFamily::SansSerif, |s| {
                to_text_font_family(s.font_family)
            });
            let shaped = font.shape_wrapped(font_family, text, font_size, font_weight, wrap_width);
            shaped_extent = Some((shaped.width, shaped.height));
        }
    }
    if let Some((shaped_width, shaped_height)) = shaped_extent {
        min_x = min_x.min(content_x - ink_pad);
        min_y = min_y.min(content_y - ink_pad);
        max_x = max_x.max(content_x + shaped_width * scale_factor + ink_pad);
        max_y = max_y.max(content_y + shaped_height * scale_factor + ink_pad);
    }

    let overflow_clips = style.is_some_and(|s| s.overflow_clips);
    let clip_to_padding = overflow_clips.then(|| {
        ClipRect::from_xywh(
            abs_x + border.left.width,
            abs_y + border.top.width,
            (layout.width - border.left.width - border.right.width).max(0.0),
            (layout.height - border.top.width - border.bottom.width).max(0.0),
        )
    });

    for &child in arena.children(node) {
        let child_opacity = styles.get(&child).map_or(1.0, |s| s.opacity);
        if child_opacity <= 0.0 {
            continue;
        }
        let Some(&child_layout) = layouts.get(&child) else {
            continue;
        };
        let child_abs_x = abs_x + child_layout.x;
        let child_abs_y = abs_y + child_layout.y;
        let Some(child_extent) = subtree_extent(
            arena,
            styles,
            layouts,
            font,
            child,
            child_abs_x,
            child_abs_y,
            scale_factor,
        ) else {
            continue;
        };

        let child_style = styles.get(&child);
        let child_transform = child_style
            .map(|s| resolve_transform(s, &child_layout, child_abs_x, child_abs_y, scale_factor))
            .unwrap_or(Transform::identity());
        let child_has_filter = child_style.is_some_and(|s| !s.filter.is_empty());
        let child_is_group =
            child_opacity < 1.0 || !child_transform.is_identity() || child_has_filter;

        let (cx0, cy0, cx1, cy1) = if child_is_group {
            let filter_pad = child_style.map_or(0.0, |s| filter_inflation(&s.filter));
            let (fx0, fy0, fx1, fy1) = (
                child_extent.0 - filter_pad,
                child_extent.1 - filter_pad,
                child_extent.2 + filter_pad,
                child_extent.3 + filter_pad,
            );
            map_rect_aabb(fx0, fy0, fx1, fy1, child_transform)
        } else {
            child_extent
        };

        let (cx0, cy0, cx1, cy1) = match &clip_to_padding {
            Some(clip) => {
                let clipped = clip.intersect(&ClipRect {
                    x0: cx0,
                    y0: cy0,
                    x1: cx1,
                    y1: cy1,
                    rounded: None,
                });
                if clipped.is_empty() {
                    continue;
                }
                (clipped.x0, clipped.y0, clipped.x1, clipped.y1)
            }
            None => (cx0, cy0, cx1, cy1),
        };

        min_x = min_x.min(cx0);
        min_y = min_y.min(cy0);
        max_x = max_x.max(cx1);
        max_y = max_y.max(cy1);
    }

    Some((min_x, min_y, max_x, max_y))
}

/// What [`group_extent`] decided: either a real, computed bound smaller
/// than the target it'll composite onto, or a reason it fell back to
/// the target's own full size instead — see [`group_extent`]'s own doc
/// for when each reason applies. `#[cfg(test)]`-only: production code
/// only needs to know bounded-vs-not, but a test asserting a fixture
/// took the bounded path (catching a silent fallback regression) needs
/// to know *which* fallback it silently took instead.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq)]
enum FallbackReason {
    /// [`subtree_extent`] found nothing to paint at all.
    NothingToPaint,
    /// A computed bound was non-finite (a non-invertible or otherwise
    /// degenerate `transform`).
    NonFinite,
    /// The computed bound's own area already reaches or exceeds the
    /// target's — nothing gained from a "bounded" surface that isn't
    /// actually bounded.
    AreaReachesTarget,
}

#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
enum GroupSizing {
    Bounded {
        width: u32,
        height: u32,
        origin: (i32, i32),
    },
    /// Nothing in `node`'s own subtree is visible at all — `paint_group`
    /// returns without painting or compositing anything, the correct
    /// (not merely convenient) outcome for, e.g., a `0×0` box with a
    /// `transform` declared, which used to panic on the old canvas-sized
    /// `Pixmap::new(0, 0).expect(..)`.
    Empty,
    FullTarget(FallbackReason),
}

#[cfg(test)]
thread_local! {
    static FORCE_FULL_TARGET_GROUPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Every [`GroupSizing`] decision [`group_extent`] has made since the
    /// last [`tests::reset_group_sizing_log`] — a test's own way to
    /// assert a fixture's groups actually took the bounded path, not a
    /// silent fallback nobody noticed regressed.
    static GROUP_SIZING_LOG: std::cell::RefCell<Vec<GroupSizing>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Records `sizing` into [`GROUP_SIZING_LOG`] (test builds only) and
/// returns it unchanged — every `group_extent` return path goes through
/// this instead of a bare `return`, so the log always reflects exactly
/// what callers actually got back.
fn record_sizing(sizing: GroupSizing) -> GroupSizing {
    #[cfg(test)]
    GROUP_SIZING_LOG.with(|log| log.borrow_mut().push(sizing));
    sizing
}

/// The region [`group_extent`]'s own 1c visibility clamp treats as
/// "could end up on screen" — a lightweight stand-in for a real
/// [`Surface`] (no pixel buffer, just its own bounds) so computing the
/// clamp never needs to allocate a pixmap the size of whatever region
/// it's bounding.
#[derive(Clone, Copy)]
struct TargetRegion {
    origin: (i32, i32),
    width: u32,
    height: u32,
}

/// Decides how large a surface [`paint_group`] should allocate for
/// `node`'s own subtree, and where its own local `(0, 0)` sits in
/// absolute canvas coordinates — bounded to the subtree's own real
/// extent (see [`subtree_extent`]) rather than always `target`'s own
/// full size, clamped further to whatever of that extent could actually
/// end up visible once `transform` and `target`'s own bounds are
/// accounted for. See this module's own tests for the differential
/// proof that a bounded surface paints bit-identical pixels to a
/// `target`-sized one regardless of which this returns.
#[allow(clippy::too_many_arguments)]
fn group_extent(
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
    target: TargetRegion,
    transform: Transform,
) -> GroupSizing {
    #[cfg(test)]
    if FORCE_FULL_TARGET_GROUPS.with(|flag| flag.get()) {
        return record_sizing(GroupSizing::FullTarget(FallbackReason::AreaReachesTarget));
    }

    let Some((x0, y0, x1, y1)) = subtree_extent(
        arena,
        styles,
        layouts,
        font,
        node,
        node_abs.0,
        node_abs.1,
        scale_factor,
    ) else {
        return record_sizing(GroupSizing::FullTarget(FallbackReason::NothingToPaint));
    };

    let own_filter = styles.get(&node).map_or(&[][..], |s| &s.filter[..]);
    let pad = filter_inflation(own_filter);
    let (x0, y0, x1, y1) = (x0 - pad, y0 - pad, x1 + pad, y1 + pad);

    // 1c: clamp to what could possibly be seen — `target`'s own visible
    // rect, mapped back through `transform` into this group's own
    // pre-transform space (an inverted, non-invertible `transform`
    // — `scale(0)`, degenerate — skips the clamp rather than folding to
    // nothing; the unclamped extent is still a correct, if looser,
    // bound). Inflated by the same filter reach as above: a pixel just
    // inside the clamped edge still needs blur samples from just
    // outside it.
    let (x0, y0, x1, y1) = match transform.invert() {
        Some(inverse) => {
            let target_x0 = target.origin.0 as f32;
            let target_y0 = target.origin.1 as f32;
            let target_x1 = target_x0 + target.width as f32;
            let target_y1 = target_y0 + target.height as f32;
            let (vx0, vy0, vx1, vy1) =
                map_rect_aabb(target_x0, target_y0, target_x1, target_y1, inverse);
            (
                x0.max(vx0 - pad),
                y0.max(vy0 - pad),
                x1.min(vx1 + pad),
                y1.min(vy1 + pad),
            )
        }
        None => (x0, y0, x1, y1),
    };

    if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
        return record_sizing(GroupSizing::FullTarget(FallbackReason::NonFinite));
    }

    // 1d: snap outward to whole pixels, plus a 1px transparent margin —
    // `draw_pixmap`'s own edge-pad sampling under a rotated/scaled
    // composite needs the surface's own edge to actually be transparent,
    // not real content right up against it (see the differential test
    // for the case this covers).
    const MARGIN: f32 = 1.0;
    let ix0 = (x0 - MARGIN).floor();
    let iy0 = (y0 - MARGIN).floor();
    let ix1 = (x1 + MARGIN).ceil();
    let iy1 = (y1 + MARGIN).ceil();

    if ix1 <= ix0 || iy1 <= iy0 {
        return record_sizing(GroupSizing::Empty);
    }

    let width = (ix1 - ix0) as u32;
    let height = (iy1 - iy0) as u32;
    if u64::from(width) * u64::from(height) >= u64::from(target.width) * u64::from(target.height) {
        return record_sizing(GroupSizing::FullTarget(FallbackReason::AreaReachesTarget));
    }

    record_sizing(GroupSizing::Bounded {
        width,
        height,
        origin: (ix0 as i32, iy0 as i32),
    })
}

/// Renders `node`'s own box and its whole subtree into a fresh,
/// transparent buffer — sized and positioned by [`group_extent`] to
/// `node`'s own subtree's real bounds, not always `target`'s own full
/// size (see that function's own doc) — applies `node`'s own
/// [`ComputedStyle::filter`] chain to that buffer's pixels (see
/// [`apply_filters`]), then composites the result onto `buffer` at
/// `opacity` through `transform` — real CSS's own "group opacity" (every
/// overlap *inside* the group still resolves at full strength against its
/// own siblings, later paints over earlier exactly as usual, and only the
/// group's own combined result is faded as one flat image — painting each
/// descendant at the reduced opacity individually instead would show
/// every overlap inside the group as a visibly different, doubled-up
/// alpha), real CSS's own `transform` semantics (the buffer already holds
/// `node` at its normal, transform-free position, so compositing it with a
/// real matrix instead of the identity one moves the *entire rendered
/// group* — background, border, and every already-clipped descendant
/// together — as a single rigid image, exactly what "transform
/// establishes its own coordinate system" means), and real CSS's own
/// filter-then-composite order (a filter transforms what the element
/// itself looks like *before* opacity fades it or transform repositions
/// it, not after). Called whenever any of the three conditions holds; a
/// node with more than one applies them together in this one function,
/// matching real CSS's own single stacking-context-establishing group.
///
/// `clip` is an ancestor's clip, exactly as [`paint_node`] takes it —
/// applied both to what's painted *inside* the group (so a descendant
/// still respects an ancestor's `overflow: hidden` even though it's
/// rendered into a separate buffer first) and to the final composite
/// (redundant with the first once the inner paint already respected it,
/// but cheap and keeps this function correct even if that ever changes).
/// Left in the ancestor's own untransformed coordinate space in both
/// places — real CSS clips a transformed box's *rendered result* against
/// an ancestor's clip, not the other way around.
#[allow(clippy::too_many_arguments)]
fn paint_group(
    buffer: &mut Surface,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
    clip: Option<ClipRect>,
    opacity: f32,
    transform: Transform,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    let target_rect = ClipRect::from_xywh(
        buffer.origin.0 as f32,
        buffer.origin.1 as f32,
        buffer.width() as f32,
        buffer.height() as f32,
    );
    // A clip already narrower than the whole target tightens what
    // `group_extent`'s own 1c visibility clamp needs to consider —
    // harmless to skip (the clamp against the full target alone still
    // gives a correct, just looser, bound), so an absent clip just
    // reuses `target_rect` unchanged.
    let visible_rect = match &clip {
        Some(clip) => target_rect.intersect(clip),
        None => target_rect,
    };
    let sizing = if visible_rect.is_empty() {
        GroupSizing::Empty
    } else {
        let visible_target = TargetRegion {
            origin: (visible_rect.x0 as i32, visible_rect.y0 as i32),
            width: (visible_rect.x1 - visible_rect.x0).max(1.0).ceil() as u32,
            height: (visible_rect.y1 - visible_rect.y0).max(1.0).ceil() as u32,
        };
        group_extent(
            arena,
            styles,
            layouts,
            font,
            node,
            node_abs,
            scale_factor,
            visible_target,
            transform,
        )
    };
    let (width, height, origin) = match sizing {
        GroupSizing::Empty => return,
        GroupSizing::Bounded {
            width,
            height,
            origin,
        } => (width, height, origin),
        GroupSizing::FullTarget(_) => (buffer.width(), buffer.height(), buffer.origin),
    };
    let Some(mut group) = Surface::new(width, height, origin) else {
        return;
    };
    let inner_mask = clip.as_ref().and_then(|c| c.to_mask(&group));
    paint_node(
        &mut group,
        arena,
        styles,
        layouts,
        font,
        node,
        scale_factor,
        inner_mask.as_ref(),
        text_inputs,
        images,
    );
    let content_clip = clip_for_children(arena, styles, layouts, node, clip.clone(), scale_factor);
    let child_display = styles.get(&node).map(|s| s.display);
    paint_nodes(
        &mut group,
        arena,
        styles,
        layouts,
        font,
        child_display,
        arena.children(node),
        scale_factor,
        content_clip,
        text_inputs,
        images,
    );

    let filter = styles.get(&node).map_or(&[][..], |s| &s.filter[..]);
    apply_filters(&mut group.pixmap, filter);

    let paint = PixmapPaint {
        opacity,
        ..Default::default()
    };
    // `draw_pixmap`'s own `(x, y)` places the group's local `(0, 0)` at
    // that *absolute* position before `transform` (its own `transform`
    // argument) ever runs — see this crate's own differential test for
    // why the target's own origin can't just be folded into `(x, y)`
    // alongside the group's: for a non-identity `transform` (rotate,
    // scale) that would double-shift, correct only by coincidence for a
    // translate-only one. Composing it into the transform itself instead
    // (`translate(-target_origin)` run *after* `transform`, via
    // `pre_concat` — the same "last-listed runs first" idiom
    // `resolve_transform` uses) reduces exactly to today's `(0, 0,
    // transform)` call whenever the target is the root canvas
    // (`buffer.origin == (0, 0)`), and stays correct for a group nested
    // inside another bounded one too.
    let to_target = Transform::from_translate(-(buffer.origin.0 as f32), -(buffer.origin.1 as f32))
        .pre_concat(transform);
    let composite_mask = clip.and_then(|c| c.to_mask(buffer));
    buffer.pixmap.draw_pixmap(
        group.origin.0,
        group.origin.1,
        group.pixmap.as_ref(),
        &paint,
        to_target,
        composite_mask.as_ref(),
    );
}

/// The real 2D affine matrix `style`'s own `transform` describes, pivoted
/// around its own `transform_origin` — [`Transform::identity()`] (a cheap
/// no-op composite, taken by [`paint_nodes`]'s own fast path) when
/// `transform` is empty, real CSS's own `none`.
///
/// `x`/`y` are `style`'s node's own absolute border-box position in the
/// canvas's own (possibly HiDPI-scaled) physical pixels — the same
/// position [`paint_node`] paints that box at. `scale_factor` converts a
/// `transform` value's own CSS-authored lengths (`translate(10px)`,
/// `matrix()`'s `e`/`f`) from the logical pixels they're specified in up
/// to the canvas's physical ones, exactly the split [`paint_shaped_runs`]
/// already needs for text and for the same reason: `layout` (and this
/// whole crate's coordinate space) already accounts for HiDPI, but a raw
/// CSS length read off [`ComputedStyle`] hasn't yet. A `<percentage>`
/// component needs no such correction — it's already relative to
/// `layout`'s own (already-scaled) box, so the same ratio holds
/// regardless of DPR.
///
/// Each function in [`ComputedStyle::transform`]'s own list folds into
/// one running matrix via [`Transform::pre_concat`], in authored order —
/// real CSS's own composition rule: `transform: A B` maps a point as
/// `A(B(point))`, so the *last*-listed function acts on the original
/// point first and the *first*-listed one acts last, in the coordinate
/// system every earlier function already established. `pre_concat`
/// builds exactly that: starting from the identity, concatenating `A`
/// then `B` leaves `A * B`, which maps a point as `A(B(point))` — the
/// same left-to-right accumulation, not the other order.
fn resolve_transform(
    style: &ComputedStyle,
    layout: &BoxLayout,
    x: f32,
    y: f32,
    scale_factor: f32,
) -> Transform {
    if style.transform.is_empty() {
        return Transform::identity();
    }
    let mut list = Transform::identity();
    for function in &style.transform {
        let next = match *function {
            TransformFunction::Translate(tx, ty) => Transform::from_translate(
                tx.length * scale_factor + tx.percentage * layout.width,
                ty.length * scale_factor + ty.percentage * layout.height,
            ),
            TransformFunction::Scale(sx, sy) => Transform::from_scale(sx, sy),
            TransformFunction::Rotate(degrees) => Transform::from_rotate(degrees),
            TransformFunction::Matrix { a, b, c, d, e, f } => {
                Transform::from_row(a, b, c, d, e * scale_factor, f * scale_factor)
            }
        };
        list = list.pre_concat(next);
    }

    let (origin_x_lp, origin_y_lp) = style.transform_origin;
    let origin_x = x + origin_x_lp.length * scale_factor + origin_x_lp.percentage * layout.width;
    let origin_y = y + origin_y_lp.length * scale_factor + origin_y_lp.percentage * layout.height;
    Transform::from_translate(origin_x, origin_y)
        .pre_concat(list)
        .pre_concat(Transform::from_translate(-origin_x, -origin_y))
}

/// `node`'s own border box at `(x, y, width, height)`, after applying
/// [`resolve_transform`] to its four corners and taking the axis-aligned
/// bounding box of the result — real CSS's own `getBoundingClientRect()`
/// semantics for a transformed element (CSSOM View's spec: the
/// *transformed* border box, not the pre-transform layout one). Painting
/// itself never needs this — [`resolve_transform`]'s own [`Transform`]
/// composites the whole rendered group directly instead of remeasuring a
/// box — but anything comparing this crate's own geometry against a real
/// browser's (`florui-conformance`'s own harness) needs to compare on the
/// same terms, or every transformed fixture would show a geometry
/// mismatch that isn't a real bug.
pub fn transformed_bounding_box(
    style: &ComputedStyle,
    layout: &BoxLayout,
    x: f32,
    y: f32,
    scale_factor: f32,
) -> (f32, f32, f32, f32) {
    let transform = resolve_transform(style, layout, x, y, scale_factor);
    let (min_x, min_y, max_x, max_y) =
        map_rect_aabb(x, y, x + layout.width, y + layout.height, transform);
    (min_x, min_y, max_x - min_x, max_y - min_y)
}

/// Maps an axis-aligned rectangle's own four corners through `transform`
/// and returns the axis-aligned bounding box of the result, as `(min_x,
/// min_y, max_x, max_y)` — the same corner-mapping
/// [`transformed_bounding_box`] always did, pulled out so
/// [`group_extent`] can reuse it for a nested group's own contribution to
/// its enclosing group's bounds.
fn map_rect_aabb(x0: f32, y0: f32, x1: f32, y1: f32, transform: Transform) -> (f32, f32, f32, f32) {
    let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for &(px, py) in &corners {
        let mut point = tiny_skia::Point { x: px, y: py };
        transform.map_point(&mut point);
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    (min_x, min_y, max_x, max_y)
}

/// Samples `buffer` within `node`'s own border box, filters that sample
/// (see [`apply_filters`]), then writes it back with `BlendMode::Source`
/// (replace, not blend) — the filtered sample *is* the new backdrop.
/// Region is clamped to `buffer`'s own bounds first, since
/// [`Pixmap::clone_rect`] refuses a rect that isn't fully contained.
/// Sampling ignores `clip` (an ancestor's clip stops painting, it doesn't
/// erase what's already there); only the write-back respects it. A rounded
/// `outline` narrows that write-back to its own shape, so the filtered
/// backdrop doesn't show at the cut corners.
fn apply_backdrop_filter(
    buffer: &mut Surface,
    outline: &RoundedRect,
    functions: &[FilterFunction],
    clip: Option<&Mask>,
) {
    if functions.is_empty() {
        return;
    }
    let (x, y, width, height) = (outline.x, outline.y, outline.width, outline.height);
    let rounded_clip = (!outline.is_square())
        .then(|| {
            let mut mask = Mask::new(buffer.width(), buffer.height())?;
            mask.fill_path(
                &outline.path()?,
                FillRule::Winding,
                true,
                surface_transform(buffer),
            );
            if let Some(clip) = clip {
                for (own, ancestor) in mask.data_mut().iter_mut().zip(clip.data()) {
                    *own = mul_div_255(*own, *ancestor);
                }
            }
            Some(mask)
        })
        .flatten();
    let clip = rounded_clip.as_ref().or(clip);
    let (lx, ly) = buffer.local(x, y);
    let x0 = lx.max(0.0).floor() as i32;
    let y0 = ly.max(0.0).floor() as i32;
    let x1 = (lx + width).min(buffer.width() as f32).ceil() as i32;
    let y1 = (ly + height).min(buffer.height() as f32).ceil() as i32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let Some(region) = IntRect::from_xywh(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32) else {
        return;
    };
    let Some(mut backdrop) = buffer.pixmap.clone_rect(region) else {
        return;
    };
    apply_filters(&mut backdrop, functions);
    // Replace, weighted by the mask: `BlendMode::Source` with a mask would
    // scale the source itself, clearing the pixels outside it instead of
    // leaving them alone.
    let surface_width = buffer.width();
    let (region_width, region_height) = (backdrop.width(), backdrop.height());
    for row in 0..region_height {
        for col in 0..region_width {
            let index = ((y0 as u32 + row) * surface_width + x0 as u32 + col) as usize;
            let coverage = clip.map_or(255, |mask| mask.data()[index]);
            if coverage == 0 {
                continue;
            }
            let source = backdrop.pixels()[(row * region_width + col) as usize];
            let pixels = buffer.pixmap.pixels_mut();
            pixels[index] = if coverage == 255 {
                source
            } else {
                lerp_premultiplied(pixels[index], source, coverage)
            };
        }
    }
}

/// `from` blended toward `to` by `weight / 255`, channel by channel on
/// premultiplied pixels (which interpolate linearly).
fn lerp_premultiplied(
    from: PremultipliedColorU8,
    to: PremultipliedColorU8,
    weight: u8,
) -> PremultipliedColorU8 {
    let mix = |a: u8, b: u8| mul_div_255(b, weight).saturating_add(mul_div_255(a, 255 - weight));
    let alpha = mix(from.alpha(), to.alpha());
    PremultipliedColorU8::from_rgba(
        mix(from.red(), to.red()).min(alpha),
        mix(from.green(), to.green()).min(alpha),
        mix(from.blue(), to.blue()).min(alpha),
        alpha,
    )
    .unwrap_or(to)
}

/// Applies `functions`' own chain to `pixmap`'s premultiplied pixels in
/// place, each function processing the *previous* one's own output in
/// authored order — real CSS's own filter-chain semantics (the
/// first-listed function reads the node's own unfiltered content).
fn apply_filters(pixmap: &mut Pixmap, functions: &[FilterFunction]) {
    let (width, height) = (pixmap.width(), pixmap.height());
    for function in functions {
        match *function {
            FilterFunction::Blur(radius) => blur_pixmap_in_place(pixmap, width, height, radius),
            FilterFunction::Brightness(factor) => {
                for pixel in pixmap.pixels_mut() {
                    *pixel = scale_premultiplied(*pixel, factor);
                }
            }
            FilterFunction::Contrast(factor) => {
                for pixel in pixmap.pixels_mut() {
                    // Real CSS's own formula operates in normalized
                    // [0, 1] space as `(color - 0.5) * amount + 0.5`;
                    // `127.5` is that same midpoint scaled to this
                    // buffer's own 0..255 range, not the nearby `128`.
                    *pixel = map_unpremultiplied(*pixel, |c| (c - 127.5) * factor + 127.5);
                }
            }
            FilterFunction::Saturate(factor) => {
                for pixel in pixmap.pixels_mut() {
                    *pixel = saturate_premultiplied(*pixel, factor);
                }
            }
        }
    }
}

/// `blur()`'s own Gaussian, run independently over each of `pixmap`'s own
/// four premultiplied channels (including alpha) rather than un-
/// premultiplying to straight color first: blurring straight color would
/// let a fully transparent pixel's own arbitrary RGB bleed into a blurred
/// edge — the dark/bright fringe this crate's own module doc on
/// premultiplied handling warns against — while blurring every
/// premultiplied channel identically keeps `rgb <= a` everywhere the
/// source did, the same invariant [`blur::gaussian_blur_in_place`]'s own
/// per-channel treatment preserves for any linear operation.
///
/// `radius_px` is real CSS's own `<length>` argument to `blur()`, already
/// the Gaussian's own standard deviation per the Filter Effects spec — a
/// *different* correspondence than `box-shadow`'s own `blur-radius`
/// (`radius / 2`, see [`rasterize_and_blur`]'s own doc), since the two
/// properties define the relationship differently, not a shared radius
/// convention this crate could factor into one constant.
fn blur_pixmap_in_place(pixmap: &mut Pixmap, width: u32, height: u32, radius_px: f32) {
    if radius_px <= 0.0 {
        return;
    }
    let pixel_count = (width as usize) * (height as usize);
    let mut channels = [
        vec![0u8; pixel_count],
        vec![0u8; pixel_count],
        vec![0u8; pixel_count],
        vec![0u8; pixel_count],
    ];
    for (i, pixel) in pixmap.pixels().iter().enumerate() {
        channels[0][i] = pixel.red();
        channels[1][i] = pixel.green();
        channels[2][i] = pixel.blue();
        channels[3][i] = pixel.alpha();
    }
    for channel in &mut channels {
        blur::gaussian_blur_in_place(channel, width, height, radius_px);
    }
    for (i, pixel) in pixmap.pixels_mut().iter_mut().enumerate() {
        let alpha = channels[3][i];
        *pixel = PremultipliedColorU8::from_rgba(
            channels[0][i].min(alpha),
            channels[1][i].min(alpha),
            channels[2][i].min(alpha),
            alpha,
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
}

/// Scales `pixel`'s own color by `factor` (real CSS's own `brightness()`),
/// clamping each channel to `pixel`'s own alpha rather than `255` —
/// scaling a premultiplied channel by a non-negative factor and clamping
/// to alpha is exactly equivalent to un-premultiplying, scaling and
/// clamping to `255`, then re-premultiplying, without the intermediate
/// division. `factor` is never negative — real CSS's own grammar already
/// forbids it (see [`crate::cascade::FilterFunction::Brightness`]'s own
/// doc via [`crate::cascade`]).
fn scale_premultiplied(pixel: PremultipliedColorU8, factor: f32) -> PremultipliedColorU8 {
    let alpha = pixel.alpha();
    let scale = |channel: u8| ((channel as f32) * factor).round().clamp(0.0, alpha as f32) as u8;
    PremultipliedColorU8::from_rgba(
        scale(pixel.red()),
        scale(pixel.green()),
        scale(pixel.blue()),
        alpha,
    )
    .unwrap_or(PremultipliedColorU8::TRANSPARENT)
}

/// Un-premultiplies `pixel`, applies `f` to each of its own R/G/B channels
/// independently (`contrast()`'s own affine remap doesn't commute with
/// premultiplication the way [`scale_premultiplied`]'s plain scale does,
/// so this un-premultiplies first), then re-premultiplies — a fully
/// transparent pixel passes through unchanged rather than dividing by a
/// zero alpha.
fn map_unpremultiplied(
    pixel: PremultipliedColorU8,
    mut f: impl FnMut(f32) -> f32,
) -> PremultipliedColorU8 {
    let alpha = pixel.alpha();
    if alpha == 0 {
        return pixel;
    }
    let unpremultiply = |channel: u8| (channel as f32) * 255.0 / (alpha as f32);
    let repremultiply = |channel: f32| {
        ((channel.clamp(0.0, 255.0) * (alpha as f32) / 255.0).round() as u8).min(alpha)
    };
    PremultipliedColorU8::from_rgba(
        repremultiply(f(unpremultiply(pixel.red()))),
        repremultiply(f(unpremultiply(pixel.green()))),
        repremultiply(f(unpremultiply(pixel.blue()))),
        alpha,
    )
    .unwrap_or(PremultipliedColorU8::TRANSPARENT)
}

/// `saturate()`'s own luminance-preserving mix: real CSS's own saturate
/// matrix (CSS Filter Effects' own reference to SVG's `feColorMatrix
/// type="saturate"`) is algebraically identical to mixing each channel
/// toward its own Rec.-601-ish luma at `1 - factor` and keeping the rest
/// at `factor` — `factor` `1.0` is a no-op, `0.0` is grayscale, and above
/// `1.0` oversaturates.
fn saturate_premultiplied(pixel: PremultipliedColorU8, factor: f32) -> PremultipliedColorU8 {
    let alpha = pixel.alpha();
    if alpha == 0 {
        return pixel;
    }
    let unpremultiply = |channel: u8| (channel as f32) * 255.0 / (alpha as f32);
    let r = unpremultiply(pixel.red());
    let g = unpremultiply(pixel.green());
    let b = unpremultiply(pixel.blue());
    let luma = 0.213 * r + 0.715 * g + 0.072 * b;
    let mix = |channel: f32| luma + (channel - luma) * factor;
    let repremultiply = |channel: f32| {
        ((channel.clamp(0.0, 255.0) * (alpha as f32) / 255.0).round() as u8).min(alpha)
    };
    PremultipliedColorU8::from_rgba(
        repremultiply(mix(r)),
        repremultiply(mix(g)),
        repremultiply(mix(b)),
        alpha,
    )
    .unwrap_or(PremultipliedColorU8::TRANSPARENT)
}

/// `nodes` (a set of siblings — document roots when `parent_display` is
/// `None`, one node's own children otherwise) sorted into real paint
/// order — back to front, so a caller walking this list in order and
/// painting each one gets later entries drawn on top of earlier ones, the
/// same "later wins" rule [`paint_node`]'s own doc already establishes
/// for document order.
///
/// Real CSS only gives `z-index` an effect on a positioned (non-
/// [`Position::Static`]) element, a flex item, or a grid item — a plain
/// static block/inline child's `z-index` has no real effect regardless
/// of what any sibling declares. `parent_display` being
/// [`Display::Flex`]/[`Display::Grid`] makes *every* child in `nodes`
/// eligible (real CSS: any flex/grid item, positioned or not); a child
/// with its own `position` other than [`Position::Static`] is eligible
/// on its own regardless of `parent_display`. An ineligible sibling
/// always sorts as `0` (its own `z-index`, if it declared one, is inert)
/// — an eligible one sorts by its real [`ComputedStyle::z_index`]
/// ascending, `auto` (`None`) also as `0` for comparison purposes only,
/// its own semantic meaning otherwise unaffected. Ties (including every
/// sibling at the default `auto`, the common case) keep their original
/// document order: [`Vec::sort_by_key`] is a stable sort, so nothing
/// eligible anywhere paints in plain document order, unchanged from
/// before this function existed. Real per-element stacking *contexts*
/// (where a positioned descendant's `z-index` can reach past its own
/// siblings to stack against unrelated, non-sibling content elsewhere in
/// the tree) are not implemented — this only ever reorders siblings
/// within the one shared parent `nodes` already came from.
pub fn paint_order(
    styles: &HashMap<NodeId, ComputedStyle>,
    parent_display: Option<Display>,
    nodes: &[NodeId],
) -> Vec<NodeId> {
    let parent_is_flex_or_grid =
        matches!(parent_display, Some(Display::Flex) | Some(Display::Grid));
    let is_eligible = |id: &NodeId| {
        parent_is_flex_or_grid
            || styles
                .get(id)
                .is_some_and(|s| s.position != Position::Static)
    };
    if !nodes.iter().any(is_eligible) {
        return nodes.to_vec();
    }
    let mut ordered = nodes.to_vec();
    ordered.sort_by_key(|id| {
        if is_eligible(id) {
            styles.get(id).and_then(|s| s.z_index).unwrap_or(0)
        } else {
            0
        }
    });
    ordered
}

/// Paints `node`'s own background and text — document-order painting
/// (an overlapping later node always wins) comes from the caller's own
/// pre-order walk over the whole tree, not from this function recursing.
/// A `backdrop-filter` runs first (see [`apply_backdrop_filter`]), so
/// `node`'s own background/border/content paint on top of the already-
/// filtered backdrop, not the other way around.
///
/// `clip`, if given, is an *ancestor's* clip (see [`clip_for_children`]):
/// it restricts everything painted here, but `node`'s own `overflow`
/// never does — real CSS never clips a box's own border/background
/// against its own content-clip, only a descendant's.
#[allow(clippy::too_many_arguments)]
fn paint_node(
    buffer: &mut Surface,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    scale_factor: f32,
    clip: Option<&Mask>,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    if let Some(&layout) = layouts.get(&node) {
        let style = styles.get(&node);
        let (x, y) = absolute_position(arena, layouts, node);

        let outline = style.map_or(
            RoundedRect::square(x, y, layout.width, layout.height),
            |s| {
                RoundedRect::new(
                    x,
                    y,
                    layout.width,
                    layout.height,
                    &s.border_radius,
                    scale_factor,
                )
            },
        );
        let rounded = !outline.is_square();

        let backdrop_filter: &[FilterFunction] = style.map_or(&[][..], |s| &s.backdrop_filter[..]);
        apply_backdrop_filter(buffer, &outline, backdrop_filter, clip);

        let border = style.map_or(NO_BORDER, |s| s.border);
        let box_shadow: &[florui_style::BoxShadow] = style.map_or(&[][..], |s| &s.box_shadow[..]);

        // Real CSS's own painting order for a normal-flow box with no
        // stacking context: outer (non-inset) shadows sit behind
        // everything else — background, then border, then content — while
        // inset shadows sit on top of the background but still behind the
        // border and content. See [`paint_box_shadows`]'s own doc for the
        // painted shape.
        paint_box_shadows(buffer, &outline, border, box_shadow, false);

        let background = style.map_or(Rgba::TRANSPARENT, |s| s.background_color);
        if background.a != 0 {
            if rounded {
                fill_rounded_rect(buffer, &outline, background, clip);
            } else {
                fill_rect(buffer, x, y, layout.width, layout.height, background, clip);
            }
        }

        paint_box_shadows(buffer, &outline, border, box_shadow, true);

        if rounded {
            paint_rounded_border(buffer, &outline, border, clip);
        } else {
            paint_border(buffer, x, y, layout.width, layout.height, border, clip);
        }

        let color = style.map_or(Rgba::opaque(0, 0, 0), |s| s.color);
        let no_padding = florui_style::Edges {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        };
        let padding = style.map_or(no_padding, |s| s.padding);
        // Content starts inside both the border and the padding — the same
        // content-box math `florui_layout`'s own `to_taffy_style` doc
        // describes for sizing applies to painting's own offset too.
        let content_x = x + border.left.width + padding.left;
        let content_y = y + border.top.width + padding.top;
        // The box's own final content width, whatever layout resolved it
        // to (wrapped or not) — shaping at exactly this width always
        // reproduces what layout already measured: an intrinsically
        // unwrapped box is already exactly as wide as its one line, so
        // wrapping "at" that width changes nothing, and a box layout
        // wrapped to fit stays wrapped identically here.
        let content_width =
            (layout.width - border.left.width - border.right.width - padding.left - padding.right)
                .max(0.0);
        // Same content-box math as `content_width`, for `<img>`'s own
        // `object-fit` content rect below — nothing else needs a content
        // *height* (text wraps against width alone).
        let content_height =
            (layout.height - border.top.width - border.bottom.width - padding.top - padding.bottom)
                .max(0.0);
        // `content_width` is in the painted canvas's own (possibly scaled)
        // units, but shaping below runs at the *logical* `font_size`
        // `styles` itself carries — text must reshape at the same logical
        // width layout itself wrapped against, or a HiDPI canvas
        // (`scale_factor` > 1) wraps text wider than the committed layout,
        // overflowing past where the box was sized to fit it. See
        // `paint_to_buffer`'s own doc. `paint_shaped_runs`/`paint_text`
        // scale the resulting logical-sized glyphs back up to
        // `scale_factor` at rasterization time, the other half of the same
        // split: shape at the size layout actually measured, paint at the
        // size the canvas actually needs.
        let wrap_width = content_width / scale_factor;

        if is_control_icon_node(arena, node)
            && style.is_some_and(|s| s.appearance != florui_style::Appearance::None)
            && let Some(image) = images.and_then(|images| images.get(&node))
        {
            // A built-in control decoration's own already-themed pixels
            // (same `currentColor` theming, same registry, as a real
            // `<icon>` — see `crate::icon::IconRegistry`'s own doc) —
            // positioned by a fixed formula instead of `object-fit`,
            // since this is UA chrome with no CSS box of its own to
            // resolve that property against. Checked *before* the plain
            // `tag == "input"` branch below: a checked checkbox is still
            // an `<input>`, and would otherwise always match that branch
            // first (a real, found regression — `text_inputs` simply has
            // no entry for a checkbox, so skipping this branch entirely
            // for one was silently a no-op, not a compile error).
            // `content_width`/`content_height` are already this canvas's
            // own physical pixels here; `control_icon_rect` returns
            // logical units, the same split the `img`/`icon` branch below
            // uses for the same reason (`paint_image` scales back up at
            // blit time).
            let rect = control_icon_rect(
                arena,
                node,
                content_width / scale_factor,
                content_height / scale_factor,
            );
            paint_image(
                buffer,
                image,
                content_x,
                content_y,
                rect,
                scale_factor,
                clip,
            );
            // A select's label is its own text; a checkbox has none.
            if arena.tag(node) == "select" {
                paint_node_text(
                    buffer,
                    font,
                    arena,
                    node,
                    style,
                    color,
                    (content_x, content_y),
                    wrap_width,
                    scale_factor,
                    clip,
                );
            }
        } else if arena.tag(node) == "input" {
            if let Some(paint) = text_inputs.and_then(|inputs| inputs.get(&node)) {
                // `caret_rect`/`selection_rects` come from the same
                // logical-space editor layout `paint.runs`' own glyph
                // positions do -- scaled up here exactly like
                // `paint_shaped_runs` already scales each glyph, so a
                // selection highlight lands under the glyphs it visually
                // covers instead of drifting on a HiDPI canvas.
                let scaled = |(x0, y0, x1, y1): (f32, f32, f32, f32)| {
                    (
                        x0 * scale_factor,
                        y0 * scale_factor,
                        x1 * scale_factor,
                        y1 * scale_factor,
                    )
                };
                // Behind the text -- real CSS/browser selection highlight
                // paints under the glyphs it covers, not over them.
                for &rect in &paint.selection_rects {
                    let (sx0, sy0, sx1, sy1) = scaled(rect);
                    fill_rect(
                        buffer,
                        content_x + sx0,
                        content_y + sy0,
                        sx1 - sx0,
                        sy1 - sy0,
                        TEXT_INPUT_SELECTION_COLOR,
                        clip,
                    );
                }
                paint_shaped_runs(
                    buffer,
                    &paint.runs,
                    content_x,
                    content_y,
                    color,
                    scale_factor,
                    clip,
                );
                // A live IME preedit composition already painted above
                // (its glyphs are real, spliced straight into `paint.runs`
                // by Parley) -- this underline is the only visual cue
                // distinguishing it from already-committed text.
                if let Some(rect) = paint.compose_rect {
                    let (rx0, _, rx1, ry1) = scaled(rect);
                    fill_rect(
                        buffer,
                        content_x + rx0,
                        content_y + ry1 - TEXT_INPUT_CARET_WIDTH * scale_factor,
                        rx1 - rx0,
                        TEXT_INPUT_CARET_WIDTH * scale_factor,
                        color,
                        clip,
                    );
                }
                if paint.show_caret
                    && let Some(rect) = paint.caret_rect
                {
                    let (cx0, cy0, cx1, cy1) = scaled(rect);
                    fill_rect(
                        buffer,
                        content_x + cx0,
                        content_y + cy0,
                        (cx1 - cx0).max(TEXT_INPUT_CARET_WIDTH * scale_factor),
                        cy1 - cy0,
                        color,
                        clip,
                    );
                }
            }
        } else if matches!(arena.tag(node), "img" | "icon") {
            // An `<icon>`'s own decoded pixels are already fully themed
            // (real `currentColor` substitution happened before
            // rasterization, on the platform side that owns style/color
            // resolution) -- this crate just blits either tag's own
            // pixels identically, no icon-specific painting path needed.
            if let Some(image) = images.and_then(|images| images.get(&node)) {
                let object_fit = style.map_or(ObjectFit::Fill, |s| s.object_fit);
                let object_position = style.map_or(
                    (
                        LengthPercentage {
                            length: 0.0,
                            percentage: 0.5,
                        },
                        LengthPercentage {
                            length: 0.0,
                            percentage: 0.5,
                        },
                    ),
                    |s| s.object_position,
                );
                // Resolved in *logical* units, the same split
                // `wrap_width` above uses for text: `content_width`/
                // `content_height` are already in this canvas's own
                // (possibly HiDPI-scaled) physical pixels, but
                // `image.width`/`image.height` are the decoded source's
                // own raw pixel counts -- real CSS's own object-fit
                // treats those as this image's intrinsic *CSS*-pixel
                // size (1 image pixel = 1 logical px, no 2x/3x
                // image-density concept yet), so mixing a physical box
                // size against a logical intrinsic one here would double
                // (or halve) the result on any non-1x display.
                // `paint_image` scales the resolved rect back up to this
                // canvas's own physical pixels at blit time, mirroring
                // how `paint_shaped_runs` scales logical-sized glyphs up
                // when it rasterizes them.
                let rect = florui_layout::resolve_object_fit_content_rect(
                    object_fit,
                    object_position,
                    taffy::geometry::Size {
                        width: content_width / scale_factor,
                        height: content_height / scale_factor,
                    },
                    taffy::geometry::Size {
                        width: image.width as f32,
                        height: image.height as f32,
                    },
                );
                paint_image(
                    buffer,
                    image,
                    content_x,
                    content_y,
                    rect,
                    scale_factor,
                    clip,
                );
            }
        } else if florui_layout::is_inline_formatting_context(arena, styles, node) {
            // A real mixed text/inline-element node: rebuilt and
            // reshaped fresh here, since this crate doesn't share layout's
            // own internal Taffy tree — deterministic at the same final
            // width, the same reasoning `paint_text`'s own doc gives for
            // the plain single-style case below. Every run paints in this
            // node's own inherited `color` uniformly — a per-span `color`
            // override isn't painted differently yet, a documented bound
            // matching `florui_layout`'s own module doc.
            if let Some(shaped) = florui_layout::shape_inline_formatting_context(
                font, arena, styles, node, wrap_width,
            ) {
                paint_shaped_runs(
                    buffer,
                    &shaped.runs,
                    content_x,
                    content_y,
                    color,
                    scale_factor,
                    clip,
                );
                if style.is_some_and(|s| s.text_decoration_underline) {
                    let font_size = style.map_or(16.0, |s| s.font_size);
                    paint_underline(
                        buffer,
                        content_x,
                        content_y,
                        shaped.width,
                        font_size,
                        shaped.baseline,
                        color,
                        scale_factor,
                        clip,
                    );
                }
            }
        } else {
            paint_node_text(
                buffer,
                font,
                arena,
                node,
                style,
                color,
                (content_x, content_y),
                wrap_width,
                scale_factor,
                clip,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_node_text(
    buffer: &mut Surface,
    font: &mut Font,
    arena: &Arena,
    node: NodeId,
    style: Option<&ComputedStyle>,
    color: Rgba,
    (x, y): (f32, f32),
    wrap_width: f32,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let text = arena.text_content(node);
    if text.is_empty() {
        return;
    }
    let font_size = style.map_or(16.0, |s| s.font_size);
    let font_weight = style.map_or(400.0, |s| s.font_weight);
    let font_family = style.map_or(florui_text::FontFamily::SansSerif, |s| {
        to_text_font_family(s.font_family)
    });
    paint_text(
        buffer,
        font,
        TextPaint {
            text,
            font_size,
            font_weight,
            font_family,
            color,
            x,
            y,
            wrap_width,
            scale_factor,
            clip,
        },
    );
}

/// Blits `image`'s own decoded pixels into `rect` (already resolved
/// against this node's content box by `resolve_object_fit_content_rect`;
/// `rect.x`/`rect.y` are relative to `content_x`/`content_y`, the same
/// origin `object-position`'s own offset resolves against). `tiny-skia`'s
/// own `draw_pixmap` does the actual scaling (`rect`'s size vs. `image`'s
/// own), the same real resampling every other scaled blit in this crate
/// (a transformed group, see [`paint_group`]) already goes through — no
/// separate resize path needed.
#[allow(clippy::too_many_arguments)]
fn paint_image(
    buffer: &mut Surface,
    image: &florui_assets::RasterImage,
    content_x: f32,
    content_y: f32,
    rect: florui_layout::BoxLayout,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    if rect.width <= 0.0 || rect.height <= 0.0 || image.width == 0 || image.height == 0 {
        return;
    }
    let Some(source) = premultiplied_pixmap(image) else {
        return;
    };
    let (lx, ly) = buffer.local(
        content_x + rect.x * scale_factor,
        content_y + rect.y * scale_factor,
    );
    // The destination position has to be folded into the transform
    // itself (`post_translate`, run *after* the scale), not passed as
    // `draw_pixmap`'s own `(x, y)` alongside a non-identity transform:
    // `draw_pixmap`'s `(x, y)` places the source's local `(0, 0)` at that
    // position *before* `transform` runs (see `paint_group`'s own doc on
    // this exact composition order), so a plain scale transform would
    // scale the destination position too, not just the image — verified
    // directly against a real render before landing this (a naive
    // `draw_pixmap(lx, ly, ..., Transform::from_scale(sx, sy), ...)`
    // pushed the image to roughly double its intended offset).
    let transform = Transform::from_scale(
        (rect.width * scale_factor) / image.width as f32,
        (rect.height * scale_factor) / image.height as f32,
    )
    .post_translate(lx, ly);
    buffer.pixmap.draw_pixmap(
        0,
        0,
        source.as_ref(),
        &PixmapPaint::default(),
        transform,
        clip,
    );
}

/// `florui_assets::RasterImage::rgba` is straight (non-premultiplied)
/// alpha (see that type's own doc); `tiny-skia`'s `Pixmap` requires
/// premultiplied pixels as its own invariant. The exact inverse of
/// `florui-assets`'s own `unpremultiply`: `premultiplied = straight *
/// alpha / 255`, per channel, in `u16` to avoid the intermediate
/// overflowing `u8` (max `255 * 255`).
fn premultiplied_pixmap(image: &florui_assets::RasterImage) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(image.width, image.height)?;
    for (i, pixel) in pixmap.pixels_mut().iter_mut().enumerate() {
        let bytes: [u8; 4] = image.rgba[i * 4..i * 4 + 4]
            .try_into()
            .expect("RasterImage guarantees rgba.len() == width * height * 4");
        let [r, g, b, a] = bytes;
        let premultiply = |channel: u8| ((channel as u16 * a as u16) / 255) as u8;
        *pixel = PremultipliedColorU8::from_rgba(premultiply(r), premultiply(g), premultiply(b), a)
            .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
    Some(pixmap)
}

fn fill_rect(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Rgba,
    clip: Option<&Mask>,
) {
    let (lx, ly) = buffer.local(x, y);
    let x0 = lx.max(0.0);
    let y0 = ly.max(0.0);
    let x1 = (lx + width).max(0.0).min(buffer.width() as f32);
    let y1 = (ly + height).max(0.0).min(buffer.height() as f32);
    let Some(rect) = Rect::from_ltrb(x0, y0, x1, y1) else {
        return;
    };

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = false;
    buffer
        .pixmap
        .fill_rect(rect, &paint, Transform::identity(), clip);
}

/// Anti-aliased fill of a rounded box; a square one should use
/// [`fill_rect`] instead, which stays the unchanged pixel-exact path.
fn fill_rounded_rect(buffer: &mut Surface, rect: &RoundedRect, color: Rgba, clip: Option<&Mask>) {
    let Some(path) = rect.path() else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    buffer.pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        surface_transform(buffer),
        clip,
    );
}

/// Maps absolute canvas coordinates into `buffer`'s own local pixel space.
fn surface_transform(buffer: &Surface) -> Transform {
    Transform::from_translate(-(buffer.origin.0 as f32), -(buffer.origin.1 as f32))
}

/// A rounded box's border: the ring between its outer outline and its
/// padding box's (radii shrunk by the side widths). One flat color paints
/// the ring in a single fill; differing side colors clip the same ring to
/// each side's mitered wedge, the diagonal from an outer corner to the
/// matching inner one, like real CSS.
fn paint_rounded_border(
    buffer: &mut Surface,
    outer: &RoundedRect,
    border: florui_style::Edges<florui_style::BorderSide>,
    clip: Option<&Mask>,
) {
    let sides = [border.top, border.right, border.bottom, border.left];
    if sides.iter().all(|side| side.width <= 0.0) {
        return;
    }
    let inner = outer.inset(
        border.top.width,
        border.right.width,
        border.bottom.width,
        border.left.width,
    );
    let (Some(outer_path), inner_path) = (outer.path(), inner.path()) else {
        return;
    };
    let mut ring = PathBuilder::new();
    ring.push_path(&outer_path);
    if let Some(inner_path) = &inner_path {
        ring.push_path(inner_path);
    }
    let Some(ring) = ring.finish() else {
        return;
    };
    let transform = surface_transform(buffer);

    let visible: Vec<Rgba> = sides
        .iter()
        .filter(|side| side.width > 0.0)
        .map(|side| side.color)
        .collect();
    if visible.windows(2).all(|pair| pair[0] == pair[1]) {
        let color = visible[0];
        let mut paint = Paint::default();
        paint.set_color_rgba8(color.r, color.g, color.b, color.a);
        paint.anti_alias = true;
        buffer
            .pixmap
            .fill_path(&ring, &paint, FillRule::EvenOdd, transform, clip);
        return;
    }

    let (x0, y0) = (outer.x, outer.y);
    let (x1, y1) = (outer.x + outer.width, outer.y + outer.height);
    let (ix0, iy0) = (inner.x, inner.y);
    let (ix1, iy1) = (inner.x + inner.width, inner.y + inner.height);
    let wedges = [
        [(x0, y0), (x1, y0), (ix1, iy0), (ix0, iy0)],
        [(x1, y0), (x1, y1), (ix1, iy1), (ix1, iy0)],
        [(x1, y1), (x0, y1), (ix0, iy1), (ix1, iy1)],
        [(x0, y1), (x0, y0), (ix0, iy0), (ix0, iy1)],
    ];
    for (side, corners) in sides.iter().zip(wedges) {
        if side.width <= 0.0 {
            continue;
        }
        let mut wedge = PathBuilder::new();
        wedge.move_to(corners[0].0, corners[0].1);
        for &(px, py) in &corners[1..] {
            wedge.line_to(px, py);
        }
        wedge.close();
        let Some(wedge) = wedge.finish() else {
            continue;
        };
        let mut mask = match clip {
            Some(existing) => existing.clone(),
            None => {
                let Some(mut full) = Mask::new(buffer.width(), buffer.height()) else {
                    continue;
                };
                full.data_mut().fill(255);
                full
            }
        };
        mask.intersect_path(&wedge, FillRule::Winding, true, transform);
        let mut paint = Paint::default();
        paint.set_color_rgba8(side.color.r, side.color.g, side.color.b, side.color.a);
        paint.anti_alias = true;
        buffer
            .pixmap
            .fill_path(&ring, &paint, FillRule::EvenOdd, transform, Some(&mask));
    }
}

/// Paints `border`'s four sides as flat rectangles at the box's own outer
/// edges — `(x, y, width, height)` is the whole border-box, matching
/// `fill_rect`'s own background call in [`paint_node`]. A zero-width side
/// (real CSS's own invisible default; see [`florui_style::BorderSide`]'s
/// doc) paints nothing. A box with a `border-radius` goes through
/// [`paint_rounded_border`] instead. Here adjacent sides simply overlap
/// by their own width at each corner, which a flat single color per side
/// (this crate's only supported case, real CSS's own `solid`) paints
/// identically to a mitered corner anyway.
#[allow(clippy::too_many_arguments)]
fn paint_border(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    border: florui_style::Edges<florui_style::BorderSide>,
    clip: Option<&Mask>,
) {
    if border.top.width > 0.0 {
        fill_rect(
            buffer,
            x,
            y,
            width,
            border.top.width,
            border.top.color,
            clip,
        );
    }
    if border.bottom.width > 0.0 {
        fill_rect(
            buffer,
            x,
            y + height - border.bottom.width,
            width,
            border.bottom.width,
            border.bottom.color,
            clip,
        );
    }
    if border.left.width > 0.0 {
        fill_rect(
            buffer,
            x,
            y,
            border.left.width,
            height,
            border.left.color,
            clip,
        );
    }
    if border.right.width > 0.0 {
        fill_rect(
            buffer,
            x + width - border.right.width,
            y,
            border.right.width,
            height,
            border.right.color,
            clip,
        );
    }
}

/// Paints every layer of `shadows` whose own `inset` matches `inset` — the
/// caller makes two passes, one per value, so it can interleave outer
/// shadows behind the background and inset shadows in front of it (see
/// [`paint_node`]'s own doc for why). Layers paint back-to-front in list
/// order: the *last*-listed layer paints first, so the first-listed one
/// ends up on top — real CSS's own layering rule for `box-shadow`'s
/// comma-separated list.
fn paint_box_shadows(
    buffer: &mut Surface,
    outline: &RoundedRect,
    border: florui_style::Edges<florui_style::BorderSide>,
    shadows: &[florui_style::BoxShadow],
    inset: bool,
) {
    let (x, y, width, height) = (outline.x, outline.y, outline.width, outline.height);
    for shadow in shadows.iter().filter(|s| s.inset == inset).rev() {
        if shadow.color.a == 0 {
            continue;
        }
        if !outline.is_square() {
            paint_rounded_shadow(buffer, outline, border, shadow);
            continue;
        }
        match (inset, shadow.blur_radius > 0.0) {
            (false, false) => paint_outset_shadow(buffer, x, y, width, height, shadow),
            (false, true) => paint_outset_shadow_blurred(buffer, x, y, width, height, shadow),
            (true, false) => paint_inset_shadow(buffer, x, y, width, height, border, shadow),
            (true, true) => paint_inset_shadow_blurred(buffer, x, y, width, height, border, shadow),
        }
    }
}

/// One `box-shadow` layer on a box with rounded corners. Same geometry as
/// the rectangular paths (an outer shadow is the border box grown by the
/// spread and offset, hidden behind the box itself; an inset one fills the
/// padding box except for a spread-shrunk, offset hole), with every shape
/// following the rounded outline. The hidden/kept region is a
/// hard-edged cut in the blurred case, like the rectangular one.
fn paint_rounded_shadow(
    buffer: &mut Surface,
    outline: &RoundedRect,
    border: florui_style::Edges<florui_style::BorderSide>,
    shadow: &florui_style::BoxShadow,
) {
    let padding = outline.inset(
        border.top.width,
        border.right.width,
        border.bottom.width,
        border.left.width,
    );
    let (region, shape) = if shadow.inset {
        let hole = padding
            .inset(
                shadow.spread_radius,
                shadow.spread_radius,
                shadow.spread_radius,
                shadow.spread_radius,
            )
            .translated(shadow.offset_x, shadow.offset_y);
        (padding, hole)
    } else {
        let grown = outline
            .grown(shadow.spread_radius)
            .translated(shadow.offset_x, shadow.offset_y);
        (grown, grown)
    };

    if shadow.blur_radius <= 0.0 {
        // Outer: the grown shape minus the box itself. Inset: the padding
        // box minus the hole.
        let (fill, exclude) = if shadow.inset {
            (&region, &shape)
        } else {
            (&shape, outline)
        };
        let Some(path) = fill.path() else {
            return;
        };
        let transform = surface_transform(buffer);
        let mask = match exclude.path() {
            Some(exclude_path) => {
                let Some(mut mask) = Mask::new(buffer.width(), buffer.height()) else {
                    return;
                };
                mask.fill_path(&exclude_path, FillRule::Winding, true, transform);
                mask.invert();
                mask
            }
            None => {
                let Some(mut mask) = Mask::new(buffer.width(), buffer.height()) else {
                    return;
                };
                mask.data_mut().fill(255);
                mask
            }
        };
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            shadow.color.r,
            shadow.color.g,
            shadow.color.b,
            shadow.color.a,
        );
        paint.anti_alias = true;
        buffer
            .pixmap
            .fill_path(&path, &paint, FillRule::Winding, transform, Some(&mask));
        return;
    }

    let Some(blurred) = rasterize_and_blur(
        region.x,
        region.y,
        region.width,
        region.height,
        &shape,
        shadow.blur_radius,
    ) else {
        return;
    };
    composite_blurred_shadow(
        buffer,
        &blurred,
        shadow.color,
        shadow.inset,
        |canvas_x, canvas_y| {
            let inside = outline_contains_pixel(&padding, canvas_x, canvas_y);
            if shadow.inset {
                inside
            } else {
                !outline_contains_pixel(outline, canvas_x, canvas_y)
            }
        },
    );
}

/// Whether the pixel whose top-left is `(x, y)` has its center inside
/// `outline`.
fn outline_contains_pixel(outline: &RoundedRect, x: f32, y: f32) -> bool {
    outline.contains(x + 0.5, y + 0.5)
}

/// An outer (drop) shadow: a copy of the border box, grown by
/// `spread_radius` on every side and offset by `(offset_x, offset_y)`,
/// filled with `shadow.color` — except wherever it's covered by the
/// box's own border box, which the box's own subsequent background/border
/// painting would cover anyway, but real CSS clips it there regardless
/// (visible through a *transparent* background otherwise, which a real
/// browser never shows).
fn paint_outset_shadow(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    shadow: &florui_style::BoxShadow,
) {
    let (outer_x, outer_y, outer_width, outer_height) =
        outset_shadow_extent(shadow, x, y, width, height);
    fill_rect_minus_hole(
        buffer,
        outer_x,
        outer_y,
        outer_width,
        outer_height,
        x,
        y,
        width,
        height,
        shadow.color,
    );
}

/// An inset shadow: real CSS clips it to the box's own padding box (its
/// interior, inside the border), which is filled with `shadow.color`
/// except for a "hole" — the padding box shrunk by `spread_radius` on
/// every side, then offset by `(offset_x, offset_y)`. Growing
/// `spread_radius` *shrinks* the hole here — the opposite sign from
/// [`paint_outset_shadow`]'s own shape, matching real CSS's own "spread
/// makes an outer shadow bigger, an inner one's hole smaller" rule.
fn paint_inset_shadow(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    border: florui_style::Edges<florui_style::BorderSide>,
    shadow: &florui_style::BoxShadow,
) {
    let padding_x = x + border.left.width;
    let padding_y = y + border.top.width;
    let padding_width = (width - border.left.width - border.right.width).max(0.0);
    let padding_height = (height - border.top.width - border.bottom.width).max(0.0);

    let hole_x = padding_x + shadow.offset_x + shadow.spread_radius;
    let hole_y = padding_y + shadow.offset_y + shadow.spread_radius;
    let hole_width = padding_width - 2.0 * shadow.spread_radius;
    let hole_height = padding_height - 2.0 * shadow.spread_radius;

    fill_rect_minus_hole(
        buffer,
        padding_x,
        padding_y,
        padding_width,
        padding_height,
        hole_x,
        hole_y,
        hole_width,
        hole_height,
        shadow.color,
    );
}

/// Fills the `outer` rectangle with `color`, minus whatever area it
/// shares with `hole` — the shape both an outer shadow (`hole` = the
/// box's own border box) and an inset shadow (`hole` = the offset/spread
/// "shadow-free" interior) need. Decomposed into up to four
/// non-overlapping rectangle strips — the top and bottom spanning
/// `outer`'s full width (covering its corners), left and right filling
/// only the vertical band between them — rather than an even-odd path
/// fill: even-odd computes a symmetric difference (an XOR) of the two
/// shapes, not a true subtraction, so it paints the wrong thing whenever
/// `hole` only partially overlaps `outer` or fully contains it (a
/// shadow entirely hidden behind its own box would wrongly show a ring).
/// A degenerate (non-positive width/height) strip is left to
/// [`fill_rect`]'s own no-op handling.
#[allow(clippy::too_many_arguments)]
fn fill_rect_minus_hole(
    buffer: &mut Surface,
    outer_x: f32,
    outer_y: f32,
    outer_width: f32,
    outer_height: f32,
    hole_x: f32,
    hole_y: f32,
    hole_width: f32,
    hole_height: f32,
    color: Rgba,
) {
    if outer_width <= 0.0 || outer_height <= 0.0 {
        return;
    }
    let outer_x1 = outer_x + outer_width;
    let outer_y1 = outer_y + outer_height;

    // `hole` clipped to `outer`'s own bounds: the only part of it that can
    // actually remove anything.
    let clip_x0 = hole_x.max(outer_x);
    let clip_y0 = hole_y.max(outer_y);
    let clip_x1 = (hole_x + hole_width).min(outer_x1);
    let clip_y1 = (hole_y + hole_height).min(outer_y1);

    if clip_x0 >= clip_x1 || clip_y0 >= clip_y1 {
        // No overlap (including a hole with a non-positive width/height,
        // e.g. an inset shadow whose spread erased it entirely): nothing
        // to subtract.
        fill_rect(
            buffer,
            outer_x,
            outer_y,
            outer_width,
            outer_height,
            color,
            None,
        );
        return;
    }

    fill_rect(
        buffer,
        outer_x,
        outer_y,
        outer_width,
        clip_y0 - outer_y,
        color,
        None,
    );
    fill_rect(
        buffer,
        outer_x,
        clip_y1,
        outer_width,
        outer_y1 - clip_y1,
        color,
        None,
    );
    fill_rect(
        buffer,
        outer_x,
        clip_y0,
        clip_x0 - outer_x,
        clip_y1 - clip_y0,
        color,
        None,
    );
    fill_rect(
        buffer,
        clip_x1,
        clip_y0,
        outer_x1 - clip_x1,
        clip_y1 - clip_y0,
        color,
        None,
    );
}

/// A blurred coverage buffer, plus the canvas pixel its own `(0, 0)`
/// lands on — [`rasterize_and_blur`]'s own output, read back by
/// [`composite_blurred_shadow`].
struct BlurredShape {
    origin_x: i32,
    origin_y: i32,
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

/// Stamps a `shape_width x shape_height` rect (at `shape_x`/`shape_y`,
/// canvas coordinates) into a fresh buffer covering `region` padded by
/// [`blur::kernel_radius`] on every side, then blurs the whole buffer with
/// a real Gaussian of standard deviation `blur_radius / 2.0` (see
/// `blur`'s own module doc for that CSS-spec correspondence). `region` is
/// the caller's own region of interest — the only part of the result it
/// actually reads back — which may differ from the shape itself: an
/// outset shadow's shape *is* its region of interest, but an inset
/// shadow's region of interest is the padding box, not the (differently
/// sized/positioned) hole shape blurred within it. Padding the buffer by
/// the kernel's own reach beyond `region`, not just beyond the shape,
/// keeps every value the caller will actually read genuinely unaffected
/// by this buffer's own edges — see [`blur::gaussian_blur_in_place`]'s
/// own doc on why that padding has to be real, not merely clamped.
///
/// Returns `None` for a degenerate (non-positive) region — nothing to
/// rasterize or read back.
#[allow(clippy::too_many_arguments)]
fn rasterize_and_blur(
    region_x: f32,
    region_y: f32,
    region_width: f32,
    region_height: f32,
    shape: &RoundedRect,
    blur_radius: f32,
) -> Option<BlurredShape> {
    if region_width <= 0.0 || region_height <= 0.0 {
        return None;
    }
    let sigma = blur_radius / 2.0;
    let pad = blur::kernel_radius(sigma) as f32;

    // Snapped to a whole pixel so every later coordinate in this buffer's
    // own local space is an exact integer offset from a real canvas pixel
    // — composite_blurred_shadow then never needs to re-round a float.
    let origin_x = (region_x - pad).round() as i32;
    let origin_y = (region_y - pad).round() as i32;
    let width = (region_width + 2.0 * pad).ceil().max(1.0) as u32;
    let height = (region_height + 2.0 * pad).ceil().max(1.0) as u32;

    let mut coverage = vec![0u8; (width as usize) * (height as usize)];
    let local = shape.translated(-(origin_x as f32), -(origin_y as f32));
    if local.is_square() {
        stamp_rect(
            &mut coverage,
            width,
            height,
            local.x,
            local.y,
            local.width,
            local.height,
            255,
        );
    } else {
        stamp_rounded(&mut coverage, width, height, &local);
    }

    blur::gaussian_blur_in_place(&mut coverage, width, height, sigma);

    Some(BlurredShape {
        origin_x,
        origin_y,
        width,
        height,
        coverage,
    })
}

/// [`stamp_rect`]'s rounded counterpart: the outline's anti-aliased
/// coverage, written into the buffer at full strength.
fn stamp_rounded(buf: &mut [u8], buf_width: u32, buf_height: u32, shape: &RoundedRect) {
    let (Some(path), Some(mut pixmap)) = (shape.path(), Pixmap::new(buf_width, buf_height)) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 255);
    paint.anti_alias = true;
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    for (out, pixel) in buf.iter_mut().zip(pixmap.pixels()) {
        *out = pixel.alpha();
    }
}

/// Fills the (clamped-to-buffer) rectangle at `local_x`/`local_y` with
/// `value` — [`rasterize_and_blur`]'s own way of stamping a shape's
/// unblurred, hard-edged silhouette before blurring it. A rect that
/// doesn't overlap the buffer at all (including a non-positive width or
/// height — an inset shadow's hole erased entirely by its own spread, the
/// same case [`fill_rect_minus_hole`] already handles for the unblurred
/// path) simply stamps nothing.
#[allow(clippy::too_many_arguments)]
fn stamp_rect(
    buf: &mut [u8],
    buf_width: u32,
    buf_height: u32,
    local_x: f32,
    local_y: f32,
    width: f32,
    height: f32,
    value: u8,
) {
    let x0 = local_x.max(0.0).round() as i64;
    let y0 = local_y.max(0.0).round() as i64;
    let x1 = (local_x + width).min(buf_width as f32).round() as i64;
    let y1 = (local_y + height).min(buf_height as f32).round() as i64;
    for row in y0.max(0)..y1.max(0) {
        for col in x0.max(0)..x1.max(0) {
            buf[(row as u32 * buf_width + col as u32) as usize] = value;
        }
    }
}

/// An outer (drop) shadow with a real declared blur: the same outer
/// shape [`paint_outset_shadow`] paints, but blurred (real CSS blurs the
/// shadow shape itself, then clips the *already-blurred* result away
/// from the box's own border box — the clip itself stays a hard edge,
/// only the shape's own outer boundary softens).
/// The rect an outset `box-shadow` layer's own shape occupies (its region
/// of interest for a blur too — the shape *is* the region for an outset
/// shadow, unlike an inset one's differently-shaped hole), as `(x, y,
/// width, height)`. Shared by [`paint_outset_shadow_blurred`] (which
/// blurs exactly this) and [`group_extent`] (which needs to know how far
/// a shadow can paint without running the blur itself), so the two can't
/// drift apart.
fn outset_shadow_extent(
    shadow: &florui_style::BoxShadow,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> (f32, f32, f32, f32) {
    (
        x + shadow.offset_x - shadow.spread_radius,
        y + shadow.offset_y - shadow.spread_radius,
        width + 2.0 * shadow.spread_radius,
        height + 2.0 * shadow.spread_radius,
    )
}

fn paint_outset_shadow_blurred(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    shadow: &florui_style::BoxShadow,
) {
    let (outer_x, outer_y, outer_width, outer_height) =
        outset_shadow_extent(shadow, x, y, width, height);

    let Some(blurred) = rasterize_and_blur(
        outer_x,
        outer_y,
        outer_width,
        outer_height,
        &RoundedRect::square(outer_x, outer_y, outer_width, outer_height),
        shadow.blur_radius,
    ) else {
        return;
    };

    composite_blurred_shadow(
        buffer,
        &blurred,
        shadow.color,
        false,
        |canvas_x, canvas_y| {
            // Excluded from the box's own border box — the hard clip real
            // CSS applies to an outer shadow regardless of its own blur.
            canvas_x < x || canvas_x >= x + width || canvas_y < y || canvas_y >= y + height
        },
    );
}

/// An inset shadow with a real declared blur. Blurring the *hole* shape
/// directly (rather than "padding box minus hole", inverted before
/// blurring) and inverting the coverage only at composite time relies on
/// the Gaussian blur's own linearity — `blur(255 - hole) == 255 -
/// blur(hole)` wherever the buffer's padding is real (see
/// [`rasterize_and_blur`]'s own doc) — so the same rasterize-then-blur
/// routine [`paint_outset_shadow_blurred`] uses works here too, with the
/// hole as the shape and the padding box as the region of interest.
fn paint_inset_shadow_blurred(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    border: florui_style::Edges<florui_style::BorderSide>,
    shadow: &florui_style::BoxShadow,
) {
    let padding_x = x + border.left.width;
    let padding_y = y + border.top.width;
    let padding_width = (width - border.left.width - border.right.width).max(0.0);
    let padding_height = (height - border.top.width - border.bottom.width).max(0.0);

    let hole_x = padding_x + shadow.offset_x + shadow.spread_radius;
    let hole_y = padding_y + shadow.offset_y + shadow.spread_radius;
    let hole_width = padding_width - 2.0 * shadow.spread_radius;
    let hole_height = padding_height - 2.0 * shadow.spread_radius;

    let Some(blurred) = rasterize_and_blur(
        padding_x,
        padding_y,
        padding_width,
        padding_height,
        &RoundedRect::square(hole_x, hole_y, hole_width, hole_height),
        shadow.blur_radius,
    ) else {
        return;
    };

    composite_blurred_shadow(
        buffer,
        &blurred,
        shadow.color,
        true,
        |canvas_x, canvas_y| {
            // The hard clip real CSS applies to an inset shadow: never past
            // its own padding box, regardless of its own blur.
            canvas_x >= padding_x
                && canvas_x < padding_x + padding_width
                && canvas_y >= padding_y
                && canvas_y < padding_y + padding_height
        },
    );
}

/// Blends `blurred`'s own coverage (or, with `invert`, `255 -` it — see
/// [`paint_inset_shadow_blurred`]'s own doc for why that's correct) onto
/// `buffer` in `color`, skipping any pixel `allowed` rejects or that
/// falls outside `buffer`'s own bounds.
fn composite_blurred_shadow(
    buffer: &mut Surface,
    blurred: &BlurredShape,
    color: Rgba,
    invert: bool,
    allowed: impl Fn(f32, f32) -> bool,
) {
    // `blurred.origin_x/y` are absolute canvas coordinates (computed from
    // the shadow's own absolute position, independent of which surface
    // this ends up composited onto) — `allowed` keeps comparing in that
    // same absolute space (matching the caller's own absolute
    // `x`/`width`-based bounds check), only the final pixel index below
    // converts to `buffer`'s own local one.
    let surface_width = buffer.width() as i64;
    let surface_height = buffer.height() as i64;

    for local_y in 0..blurred.height {
        let canvas_y = blurred.origin_y as i64 + local_y as i64;
        for local_x in 0..blurred.width {
            let canvas_x = blurred.origin_x as i64 + local_x as i64;
            if !allowed(canvas_x as f32, canvas_y as f32) {
                continue;
            }

            let (buffer_x, buffer_y) = buffer.local(canvas_x as f32, canvas_y as f32);
            let (buffer_x, buffer_y) = (buffer_x as i64, buffer_y as i64);
            if buffer_x < 0
                || buffer_x >= surface_width
                || buffer_y < 0
                || buffer_y >= surface_height
            {
                continue;
            }

            let raw = blurred.coverage[(local_y * blurred.width + local_x) as usize];
            let coverage = if invert { 255 - raw } else { raw };
            if coverage == 0 {
                continue;
            }

            let index = (buffer_y as u32 * buffer.width() + buffer_x as u32) as usize;
            let dst = buffer.pixmap.pixels()[index];
            buffer.pixmap.pixels_mut()[index] = blend_source_over(dst, color, coverage);
        }
    }
}

/// `u16` fixed-point `a * b / 255`, rounded to nearest — the one
/// multiply-divide every channel below needs, pulled out once so the
/// rounding stays consistent across all of them.
fn mul_div_255(a: u8, b: u8) -> u8 {
    ((a as u16 * b as u16 + 127) / 255) as u8
}

/// Standard Porter-Duff "`A` over `B`", both operands already
/// premultiplied — the real per-pixel compositing math
/// [`composite_blurred_shadow`] needs, that [`fill_rect`]'s own calls
/// into tiny-skia get for free from the library instead: this path
/// writes into an already-painted `Canvas` pixel by pixel, not through a
/// fresh `tiny_skia::Paint` fill, so there's no library call already
/// doing this blend to reuse. `src_color`/`src_coverage` are straight
/// (non-premultiplied) — the same shape [`florui_style::Rgba`] and this
/// crate's own alpha masks already use — converted to a premultiplied
/// source here before blending.
fn blend_source_over(
    dst: PremultipliedColorU8,
    src_color: Rgba,
    src_coverage: u8,
) -> PremultipliedColorU8 {
    let src_a = mul_div_255(src_color.a, src_coverage);
    let src_r = mul_div_255(src_color.r, src_a);
    let src_g = mul_div_255(src_color.g, src_a);
    let src_b = mul_div_255(src_color.b, src_a);

    let inv = 255 - src_a;
    let out_a = (src_a as u16 + mul_div_255(dst.alpha(), inv) as u16).min(255) as u8;
    let clamp_channel = |src: u8, dst: u8| -> u8 {
        let value = src as u16 + mul_div_255(dst, inv) as u16;
        value.min(out_a as u16) as u8
    };
    let out_r = clamp_channel(src_r, dst.red());
    let out_g = clamp_channel(src_g, dst.green());
    let out_b = clamp_channel(src_b, dst.blue());

    PremultipliedColorU8::from_rgba(out_r, out_g, out_b, out_a).unwrap_or(dst)
}

/// Shapes `text` (wrapped at `wrap_width`, matching whatever content width
/// layout already resolved this box to) and fills each glyph's outline at
/// `(x, y)` — the top-left of the whole shaped block, in the same
/// coordinate space as [`fill_rect`] — with `color`. Skips a run whose font
/// data doesn't parse, or an individual glyph with no outline (e.g.
/// genuinely missing from the font); a partial render beats aborting the
/// whole paint over one bad glyph.
struct TextPaint<'a> {
    text: &'a str,
    font_size: f32,
    font_weight: f32,
    font_family: florui_text::FontFamily,
    color: Rgba,
    x: f32,
    y: f32,
    wrap_width: f32,
    scale_factor: f32,
    clip: Option<&'a Mask>,
}

fn to_text_font_family(value: florui_style::FontFamily) -> florui_text::FontFamily {
    match value {
        florui_style::FontFamily::SansSerif => florui_text::FontFamily::SansSerif,
        florui_style::FontFamily::Monospace => florui_text::FontFamily::Monospace,
    }
}

fn paint_text(buffer: &mut Surface, font: &mut Font, params: TextPaint<'_>) {
    let TextPaint {
        text,
        font_size,
        font_weight,
        font_family,
        color,
        x,
        y,
        wrap_width,
        scale_factor,
        clip,
    } = params;
    let shaped = font.shape_wrapped(font_family, text, font_size, font_weight, wrap_width);
    paint_shaped_runs(buffer, &shaped.runs, x, y, color, scale_factor, clip);
}

/// `text-decoration: underline`'s own mark, spanning `width` (logical
/// units, same as `font_size`/`baseline`) at `baseline`'s own offset from
/// the block's top. Single-line only: the caller has no per-line width/
/// baseline to draw a second mark from (see `florui-style`'s own
/// `ComputedStyle::text_decoration_underline` doc for why that's a
/// deliberate bound, not an oversight).
#[allow(clippy::too_many_arguments)]
fn paint_underline(
    buffer: &mut Surface,
    x: f32,
    y: f32,
    width: f32,
    font_size: f32,
    baseline: f32,
    color: Rgba,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    // Approximated from `font_size` (typographic convention, roughly what
    // real fonts' own underline metrics land on for a UA default) rather
    // than read from real font metrics, which this crate has no accessor
    // for yet -- unlike the *positioning* logic around it (which reuses
    // the real shaped baseline, not a guess), these two constants are a
    // deliberate approximation.
    let gap = font_size * 0.08;
    let thickness = (font_size * 0.05).max(1.0);
    fill_rect(
        buffer,
        x,
        y + (baseline + gap) * scale_factor,
        width * scale_factor,
        thickness * scale_factor,
        color,
        clip,
    );
}

/// Fills every glyph across `runs` at `(x, y)` — the top-left of the whole
/// shaped block, in the same coordinate space as [`fill_rect`] — with
/// `color`. Shared by [`paint_text`] (single-style text) and a real inline
/// formatting context's own mixed-style runs, since both end up
/// with the same `&[florui_text::ShapedRun]` shape to paint, just built
/// via a different `florui_text::Font` call. Skips a run whose font data
/// doesn't parse, or an individual glyph with no outline (e.g. genuinely
/// missing from the font); a partial render beats aborting the whole paint
/// over one bad glyph.
///
/// `runs` was shaped at the *logical* font size (matching what layout
/// itself measured — see [`paint_node`]'s own doc on `wrap_width`), so
/// every glyph's own outline scale and relative position is still in
/// logical units here; `scale_factor` blows both back up to the painted
/// canvas's own (possibly physical) units, the other half of that split.
#[allow(clippy::too_many_arguments)]
fn paint_shaped_runs(
    buffer: &mut Surface,
    runs: &[florui_text::ShapedRun],
    x: f32,
    y: f32,
    color: Rgba,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    // Converted to `buffer`'s own local coordinates once, up front —
    // every glyph's own pen position below builds directly on it, so the
    // whole path ends up in local space without touching each glyph
    // individually.
    let (x, y) = buffer.local(x, y);
    let mut builder = PathBuilder::new();

    for run in runs {
        let Ok(font_ref) = FontRef::from_index(run.font.data.data(), run.font.index) else {
            continue;
        };
        let outlines = font_ref.outline_glyphs();
        let size = GlyphSize::new(run.font_size * scale_factor);
        // Without this, every glyph draws at the font's default variable
        // instance regardless of what was actually shaped — a bold run
        // would measure wider (Parley resolves the wght axis correctly
        // for layout) but paint no bolder at all.
        let coords: Vec<NormalizedCoord> = run
            .normalized_coords
            .iter()
            .map(|&bits| NormalizedCoord::from_bits(bits))
            .collect();
        let location = LocationRef::new(&coords);

        for glyph in &run.glyphs {
            let Some(outline) = outlines.get(GlyphId::new(glyph.id)) else {
                continue;
            };
            let mut pen = GlyphPen {
                builder: &mut builder,
                origin_x: x + glyph.x * scale_factor,
                origin_y: y + glyph.y * scale_factor,
            };
            let settings = DrawSettings::unhinted(size, location);
            let _ = outline.draw(settings, &mut pen);
        }
    }

    let Some(path) = builder.finish() else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    buffer.pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        clip,
    );
}

/// Feeds a glyph's outline (font units, Y-up from its own baseline origin)
/// into a [`PathBuilder`] (screen space, Y-down), offsetting by the
/// glyph's pen position and flipping the Y axis for every emitted point.
struct GlyphPen<'a> {
    builder: &'a mut PathBuilder,
    origin_x: f32,
    origin_y: f32,
}

impl OutlinePen for GlyphPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(self.origin_x + x, self.origin_y - y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(self.origin_x + x, self.origin_y - y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder.quad_to(
            self.origin_x + cx0,
            self.origin_y - cy0,
            self.origin_x + x,
            self.origin_y - y,
        );
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_to(
            self.origin_x + cx0,
            self.origin_y - cy0,
            self.origin_x + cx1,
            self.origin_y - cy1,
            self.origin_x + x,
            self.origin_y - y,
        );
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

fn to_tiny_skia_color(color: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(color.r, color.g, color.b, color.a)
}

#[cfg(test)]
mod tests {
    use florui::prelude::*;
    use florui_style::InteractionState;
    use taffy::prelude::*;

    use super::*;

    fn pixel_rgb(buffer: &Canvas, x: u32, y: u32) -> [u8; 3] {
        let pixel = buffer.pixel(x, y).expect("pixel is within the canvas");
        [pixel.red(), pixel.green(), pixel.blue()]
    }

    /// Runs `paint` (a real `paint_to_buffer` call, or anything that ends
    /// up calling it) once with [`group_extent`] deciding real bounded
    /// surfaces, once with every group forced to `target`-sized — the
    /// keystone correctness check this whole change stands on: since
    /// every group surface's origin is an integer, every fill still
    /// lands on the identical sub-pixel coordinate either way (see
    /// `Surface::local`'s own doc), so the two runs must produce
    /// pixel-identical output, not merely visually close. Panics with
    /// the first differing pixel's own coordinates and colors, which is
    /// far more useful for tracking down a real regression than a bare
    /// `assert_eq!` on two whole buffers.
    fn assert_bounded_and_full_target_paint_identically(mut paint: impl FnMut() -> Canvas) {
        FORCE_FULL_TARGET_GROUPS.with(|flag| flag.set(false));
        let bounded = paint();
        FORCE_FULL_TARGET_GROUPS.with(|flag| flag.set(true));
        let full_target = paint();
        FORCE_FULL_TARGET_GROUPS.with(|flag| flag.set(false));

        assert_eq!(bounded.width(), full_target.width());
        assert_eq!(bounded.height(), full_target.height());
        for y in 0..bounded.height() {
            for x in 0..bounded.width() {
                let a = bounded.pixel(x, y).unwrap();
                let b = full_target.pixel(x, y).unwrap();
                assert_eq!(
                    (a.red(), a.green(), a.blue(), a.alpha()),
                    (b.red(), b.green(), b.blue(), b.alpha()),
                    "pixel ({x}, {y}) differs between a bounded group surface and a \
                     target-sized one — bounding changed real output, not just its cost"
                );
            }
        }
    }

    /// Clears [`GROUP_SIZING_LOG`] and returns its previous contents —
    /// call right after a real paint, so the returned entries are
    /// exactly what that one paint's own groups decided.
    fn take_group_sizing_log() -> Vec<GroupSizing> {
        GROUP_SIZING_LOG.with(|log| std::mem::take(&mut *log.borrow_mut()))
    }

    /// Asserts every group `paint` triggers actually took the bounded
    /// path — the regression this whole log exists to catch: a bug that
    /// makes `group_extent` silently fall back to `FullTarget` would
    /// still pass every pixel-correctness test (a target-sized surface
    /// is still *correct*, just not what this change is for), so
    /// correctness tests alone can't catch it.
    fn assert_every_group_was_bounded(paint: impl FnOnce()) {
        take_group_sizing_log();
        paint();
        let log = take_group_sizing_log();
        assert!(
            !log.is_empty(),
            "expected at least one group to be painted, saw none"
        );
        for sizing in &log {
            assert!(
                matches!(sizing, GroupSizing::Bounded { .. }),
                "expected every group to be bounded, one instead took {sizing:?}"
            );
        }
    }

    #[test]
    fn paints_a_parent_and_a_smaller_positioned_child() {
        let tree: Element = view! {
            <div class="card">
                <div class="button" />
            </div>
        };
        let css = "
            .card { width: 100px; height: 60px; background-color: #1e1e22; padding-top: 10px; padding-left: 10px; }
            .button { width: 40px; height: 20px; background-color: #42734f; }
        ";

        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            100,
            60,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // Inside the card, outside the button: the card's own color.
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0x1e, 0x1e, 0x22]);
        // Inside the button (offset by the card's padding).
        assert_eq!(pixel_rgb(&buffer, 15, 15), [0x42, 0x73, 0x4f]);
    }

    fn solid_image(width: u32, height: u32, rgba: [u8; 4]) -> ImagePaint {
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            pixels.extend_from_slice(&rgba);
        }
        std::sync::Arc::new(florui_assets::RasterImage {
            rgba: pixels,
            width,
            height,
        })
    }

    /// A real `<img>`, painted through the exact same `paint_nodes`/
    /// `paint_node` path as any other content — not a special-cased blit
    /// function tested in isolation.
    fn paint_img(css: &str, width: u32, height: u32, image: ImagePaint) -> (Canvas, Arena) {
        let tree: Element = view! { <img class="pic" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let node = arena.roots()[0];
        let mut images = HashMap::new();
        images.insert(node, image);
        let buffer = paint_to_buffer_with_desktop_extras(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
            None,
            Some(&images),
        );
        (buffer, arena)
    }

    #[test]
    fn object_fit_fill_stretches_the_image_across_the_whole_content_box() {
        // A 2x2 solid-red source, stretched (real CSS's own initial
        // `object-fit: fill`) to fill a 40x20 box -- every corner and the
        // center should read pure red, proving the blit really scales
        // rather than just centering/cropping the source.
        let (buffer, _) = paint_img(
            ".pic { display: block; width: 40px; height: 20px; }",
            40,
            20,
            solid_image(2, 2, [255, 0, 0, 255]),
        );
        for (x, y) in [(0, 0), (39, 0), (0, 19), (39, 19), (20, 10)] {
            assert_eq!(pixel_rgb(&buffer, x, y), [255, 0, 0], "at ({x}, {y})");
        }
    }

    #[test]
    fn object_fit_contain_letterboxes_and_the_boxs_own_background_shows_through() {
        // A 2:1 red source in a 100x100 square box under `object-fit:
        // contain` letterboxes to 100x50, centered -- the box's own
        // green `background-color` should show through the empty top
        // and bottom strips, exactly as it would behind any other
        // narrower content.
        let (buffer, _) = paint_img(
            ".pic { display: block; width: 100px; height: 100px; \
             background-color: #00ff00; object-fit: contain; }",
            100,
            100,
            solid_image(100, 50, [255, 0, 0, 255]),
        );
        assert_eq!(pixel_rgb(&buffer, 50, 10), [0, 255, 0], "top letterbox");
        assert_eq!(pixel_rgb(&buffer, 50, 50), [255, 0, 0], "image content");
        assert_eq!(pixel_rgb(&buffer, 50, 90), [0, 255, 0], "bottom letterbox");
    }

    #[test]
    fn opacity_on_an_img_blends_its_pixels_with_the_canvas_like_any_other_content() {
        // Same offscreen-group compositing path opacity already uses for
        // every other node (see this module's own doc) -- a solid-white
        // image at 50% opacity over a black canvas should land close to
        // mid-gray, not full white (proving it actually composited
        // through the group, not painted directly at full strength).
        let (buffer, _) = paint_img(
            ".pic { display: block; width: 20px; height: 20px; opacity: 0.5; }",
            20,
            20,
            solid_image(1, 1, [255, 255, 255, 255]),
        );
        let [r, g, b] = pixel_rgb(&buffer, 10, 10);
        assert!(
            (100..=160).contains(&r) && (100..=160).contains(&g) && (100..=160).contains(&b),
            "expected roughly 50%-blended gray, got ({r}, {g}, {b})"
        );
    }

    fn paint_control_icon(tree: Element, css: &str, width: u32, height: u32) -> Canvas {
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let node = arena.roots()[0];
        let mut images = HashMap::new();
        images.insert(
            node,
            solid_image(
                CONTROL_ICON_SIZE as u32,
                CONTROL_ICON_SIZE as u32,
                [0, 255, 0, 255],
            ),
        );
        paint_to_buffer_with_desktop_extras(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
            None,
            Some(&images),
        )
    }

    #[test]
    fn a_selects_chevron_paints_right_inset_and_vertically_centered_not_over_the_label() {
        let tree: Element = view! { <select class="sel" /> };
        let buffer = paint_control_icon(
            tree,
            ".sel { width: 100px; height: 20px; border: none; padding: 0px; \
             background-color: transparent; }",
            100,
            20,
        );
        // Inside the chevron's own 12x12 box, right-inset 4px: spans
        // x in [84, 96), y in [4, 16).
        assert_eq!(
            pixel_rgb(&buffer, 90, 10),
            [0, 255, 0],
            "inside the chevron"
        );
        // Far left, where a select's own label text sits -- must stay
        // the plain canvas background, not painted over by the chevron.
        assert_eq!(pixel_rgb(&buffer, 10, 10), [0, 0, 0], "left of the chevron");
    }

    #[test]
    fn a_selects_label_still_paints_beside_its_chevron() {
        let tree: Element = view! { <select class="sel">{"Medium"}</select> };
        let buffer = paint_control_icon(
            tree,
            ".sel { width: 100px; height: 20px; border: none; padding: 0px; \
             background-color: transparent; color: #ffffff; }",
            100,
            20,
        );
        assert_eq!(pixel_rgb(&buffer, 90, 10), [0, 255, 0], "the chevron");
        let label_ink = (0..80)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel_rgb(&buffer, x, y) != [0, 0, 0])
            .count();
        assert!(
            label_ink > 0,
            "the label text must paint left of the chevron"
        );
    }

    #[test]
    fn a_checked_checkboxs_check_paints_centered() {
        let tree: Element = view! { <input type="checkbox" checked={true} class="cb" /> };
        let buffer = paint_control_icon(
            tree,
            ".cb { width: 20px; height: 20px; border: none; padding: 0px; \
             background-color: transparent; }",
            20,
            20,
        );
        // Centered 12x12 in a 20x20 box: spans x/y in [4, 16) on both axes.
        assert_eq!(pixel_rgb(&buffer, 10, 10), [0, 255, 0], "center");
        assert_eq!(
            pixel_rgb(&buffer, 1, 1),
            [0, 0, 0],
            "corner, outside the check"
        );
    }

    #[test]
    fn florui_appearance_none_suppresses_the_built_in_control_icon() {
        // Even with a real decoded image already sitting in the `images`
        // map for this node (as `desktop.rs`'s own registry lookup would
        // produce for an `appearance: none` control it *shouldn't* have
        // requested in the first place -- see `IconRegistry::sync_controls`'s
        // own doc), the paint branch itself must independently refuse to
        // draw it. Defense in depth, not redundant: this is the only test
        // that would catch a regression in the paint-side check alone.
        let tree: Element = view! { <select class="sel" /> };
        let buffer = paint_control_icon(
            tree,
            ".sel { width: 100px; height: 20px; border: none; padding: 0px; \
             background-color: transparent; --florui-appearance: none; }",
            100,
            20,
        );
        assert_eq!(
            pixel_rgb(&buffer, 90, 10),
            [0, 0, 0],
            "chevron must not paint"
        );
    }

    /// The bug this guards against: `content_width`/`content_height` (in
    /// `paint_node`) are already this canvas's own *physical* pixels once
    /// a real HiDPI host scales `layouts` before painting, but
    /// `image.width`/`image.height` are the decoded source's own raw
    /// (logical-equivalent) pixel counts — feeding the physical box size
    /// into `resolve_object_fit_content_rect` directly, then scaling its
    /// result by `scale_factor` *again* in `paint_image`, double-scales
    /// everything `object-fit`/`object-position` depend on. `object-fit:
    /// none` (content size independent of the box, only its *centering
    /// offset* depends on it) exposes this clearly: the double-scaled
    /// offset pushes the image out to the canvas's bottom-right corner
    /// instead of centering it.
    #[test]
    fn object_fit_none_centers_correctly_on_a_hidpi_canvas_not_just_a_1x_one() {
        let tree: Element = view! { <img class="pic" /> };
        let css = ".pic { display: block; width: 20px; height: 20px; object-fit: none; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let node = arena.roots()[0];

        let scale_factor = 2.0_f32;
        let physical_layouts = layouts
            .iter()
            .map(|(&id, l)| {
                (
                    id,
                    BoxLayout {
                        x: l.x * scale_factor,
                        y: l.y * scale_factor,
                        width: l.width * scale_factor,
                        height: l.height * scale_factor,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let mut images = HashMap::new();
        images.insert(node, solid_image(10, 10, [255, 0, 0, 255]));

        let buffer = paint_to_buffer_with_desktop_extras(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &physical_layouts,
            scale_factor,
            None,
            Some(&images),
        );

        // A 20x20 logical box, 10x10 logical (unscaled) content, centered
        // -> a 5px logical margin each side -> physical (10,10)-(30,30).
        assert_eq!(pixel_rgb(&buffer, 20, 20), [255, 0, 0], "centered content");
        assert_eq!(
            pixel_rgb(&buffer, 2, 2),
            [0, 0, 0],
            "outside the content, top-left"
        );
        assert_eq!(
            pixel_rgb(&buffer, 37, 37),
            [0, 0, 0],
            "outside the content, bottom-right -- where the double-scale bug \
             would have wrongly pushed the whole image to"
        );
    }

    #[test]
    fn overflow_hidden_on_an_ancestor_clips_the_image_like_any_other_content() {
        let tree: Element = view! {
            <div class="clip">
                <img class="pic" />
            </div>
        };
        let css = "
            .clip { display: block; width: 20px; height: 20px; overflow: hidden; }
            .pic { display: block; width: 40px; height: 40px; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let img_node = arena.children(arena.roots()[0])[0];
        let mut images = HashMap::new();
        images.insert(img_node, solid_image(2, 2, [255, 0, 0, 255]));

        let buffer = paint_to_buffer_with_desktop_extras(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
            None,
            Some(&images),
        );

        assert_eq!(pixel_rgb(&buffer, 10, 10), [255, 0, 0], "inside the clip");
        assert_eq!(
            pixel_rgb(&buffer, 30, 30),
            [0, 0, 0],
            "past the 20x20 clip -- the image's own overflow must not paint here"
        );
    }

    #[test]
    fn a_border_paints_its_own_color_at_the_boxs_outer_edge() {
        let tree: Element = view! { <div class="box" /> };
        let css = "
            .box {
                width: 20px; height: 20px; background-color: #1e1e22;
                border-top-width: 4px; border-top-style: solid; border-top-color: #ff0000;
                border-left-width: 4px; border-left-style: solid; border-left-color: #ff0000;
            }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let node = arena.roots()[0];
        let width = layouts[&node].width.ceil() as u32;
        let height = layouts[&node].height.ceil() as u32;
        let buffer = paint_to_buffer(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // Inside the 4px border strip, on both the top and left edges.
        assert_eq!(pixel_rgb(&buffer, 10, 1), [0xff, 0, 0], "top border");
        assert_eq!(pixel_rgb(&buffer, 1, 10), [0xff, 0, 0], "left border");
        // Past the border, into the content-box background — not still
        // border color, and not the canvas's own clear color either.
        assert_eq!(pixel_rgb(&buffer, 10, 10), [0x1e, 0x1e, 0x22]);
    }

    #[test]
    fn a_border_style_of_none_paints_no_border_despite_an_explicit_width() {
        let tree: Element = view! { <div class="box" /> };
        let css = "
            .box {
                width: 20px; height: 20px; background-color: #1e1e22;
                border-top-width: 4px; border-top-color: #ff0000;
            }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        // No border-style declared means border-style: none, real CSS's
        // own initial value — every pixel is the flat background color,
        // including the strip a rendered border would have occupied.
        for py in 0..20 {
            for px in 0..20 {
                assert_eq!(pixel_rgb(&buffer, px, py), [0x1e, 0x1e, 0x22]);
            }
        }
    }

    #[test]
    fn an_outset_box_shadow_paints_a_hard_edged_offset_copy_outside_the_box() {
        let tree: Element = view! {
            <div class="wrapper">
                <div class="box" />
            </div>
        };
        let css = "
            .wrapper {
                width: 40px; height: 40px; background-color: #000000;
                padding-top: 15px; padding-right: 15px; padding-bottom: 15px; padding-left: 15px;
            }
            .box { width: 10px; height: 10px; background-color: #1e1e22; box-shadow: 0px 8px 0px 0px #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // The box sits at (15,15)-(25,25); an 8px downward offset with no
        // blur/spread paints a hard-edged red copy at (15,25)-(25,33).
        assert_eq!(
            pixel_rgb(&buffer, 20, 28),
            [0xff, 0, 0],
            "the offset shadow band below the box"
        );
        assert_eq!(
            pixel_rgb(&buffer, 20, 20),
            [0x1e, 0x1e, 0x22],
            "inside the box's own footprint the shadow must not show through \
             (real CSS clips a shadow to outside its own border box)"
        );
        assert_eq!(
            pixel_rgb(&buffer, 20, 10),
            [0, 0, 0],
            "above the box, where the downward-only offset never reaches, \
             just the wrapper's own background"
        );
    }

    #[test]
    fn an_outset_box_shadow_with_only_spread_frames_the_box_without_a_ring_inside_it() {
        // Zero offset and a positive spread means the shadow's own
        // (grown) shape fully contains the box's own border box — the
        // "hole" this crate subtracts is then nested entirely inside the
        // shadow's outer rect, exactly the case an even-odd/XOR fill
        // would get wrong (it would paint a visible ring bleeding into
        // the box's own interior instead of leaving it alone).
        let tree: Element = view! {
            <div class="wrapper">
                <div class="box" />
            </div>
        };
        let css = "
            .wrapper {
                width: 40px; height: 40px; background-color: #000000;
                padding-top: 15px; padding-right: 15px; padding-bottom: 15px; padding-left: 15px;
            }
            .box { width: 10px; height: 10px; background-color: #1e1e22; box-shadow: 0px 0px 0px 4px #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // The 4px spread frames the box's (15,15)-(25,25) footprint with a
        // red ring from (11,11) to (29,29).
        assert_eq!(
            pixel_rgb(&buffer, 20, 13),
            [0xff, 0, 0],
            "inside the spread ring, just above the box"
        );
        assert_eq!(
            pixel_rgb(&buffer, 20, 20),
            [0x1e, 0x1e, 0x22],
            "the box's own interior must stay its own background, with no \
             shadow ring bleeding in from the nested hole"
        );
    }

    #[test]
    fn an_inset_box_shadow_paints_only_on_the_side_opposite_its_offset() {
        let tree: Element = view! {
            <div class="wrapper">
                <div class="box" />
            </div>
        };
        let css = "
            .wrapper {
                width: 40px; height: 40px; background-color: #000000;
                padding-top: 15px; padding-right: 15px; padding-bottom: 15px; padding-left: 15px;
            }
            .box { width: 10px; height: 10px; background-color: #1e1e22; box-shadow: inset 3px 3px 0px 0px #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // A positive (right/down) offset pushes the shadow-free hole
        // toward the bottom-right, so the visible inset shadow band shows
        // up along the box's own top and left interior edges instead.
        assert_eq!(
            pixel_rgb(&buffer, 20, 16),
            [0xff, 0, 0],
            "the inset shadow band along the box's own top interior edge"
        );
        assert_eq!(
            pixel_rgb(&buffer, 23, 23),
            [0x1e, 0x1e, 0x22],
            "the box's own bottom-right interior, inside the shifted hole, \
             must stay its own background"
        );
    }

    #[test]
    fn multiple_box_shadow_layers_paint_the_first_listed_one_on_top() {
        let tree: Element = view! { <div class="box" /> };
        let css = "
            .box {
                width: 10px; height: 10px; background-color: #1e1e22;
                box-shadow: 0px 4px 0px 0px #ff0000, 0px 4px 0px 0px #00ff00;
            }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            10,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // Both shadows land on the identical offset rect; the first
        // listed (red) must win, not whichever painted last by list
        // order alone.
        assert_eq!(
            pixel_rgb(&buffer, 5, 12),
            [0xff, 0, 0],
            "the first-listed shadow layer must paint on top of later ones"
        );
    }

    #[test]
    fn a_declared_blur_radius_softens_the_shadows_own_edge_into_a_gradient() {
        let tree: Element = view! {
            <div class="wrapper">
                <div class="box" />
            </div>
        };
        let css = "
            .wrapper {
                width: 40px; height: 40px; background-color: #000000;
                padding-top: 15px; padding-right: 15px; padding-bottom: 15px; padding-left: 15px;
            }
            .box { width: 10px; height: 10px; background-color: #1e1e22; box-shadow: 0px 8px 20px 0px #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // The box's own interior never shows the shadow at all, blurred
        // or not — real CSS clips the *already-blurred* shape away from
        // the border box, the clip itself staying a hard edge.
        assert_eq!(
            pixel_rgb(&buffer, 20, 20),
            [0x1e, 0x1e, 0x22],
            "blur must not bleed into the box's own footprint"
        );

        // Scanning straight down from the box's own bottom edge, at least
        // one pixel must land strictly between fully red and fully
        // background — proof of a real gradient, not a hard 0/255 step.
        let mut found_partial = false;
        for py in 25..39u32 {
            let [r, g, b] = pixel_rgb(&buffer, 20, py);
            if (1..0xff).contains(&r) && g == 0 && b == 0 {
                found_partial = true;
            }
        }
        assert!(
            found_partial,
            "expected a genuine red gradient below the box, found only hard 0/255 steps"
        );
    }

    #[test]
    fn an_inset_shadow_with_blur_softens_near_its_own_edges_but_stays_hard_at_the_padding_box() {
        let tree: Element = view! {
            <div class="wrapper">
                <div class="box" />
            </div>
        };
        let css = "
            .wrapper {
                width: 50px; height: 50px; background-color: #000000;
                padding-top: 15px; padding-right: 15px; padding-bottom: 15px; padding-left: 15px;
            }
            .box { width: 20px; height: 20px; background-color: #1e1e22; box-shadow: inset 0px 0px 6px 0px #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            50,
            50,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // Deep in the box's own center (well beyond the blur's own
        // reach from any edge): completely untouched by the shadow.
        assert_eq!(
            pixel_rgb(&buffer, 25, 25),
            [0x1e, 0x1e, 0x22],
            "a zero-offset inset shadow with a small blur must not reach the box's own center"
        );
        // One pixel in from the box's own left edge: inside the blur's
        // reach, so a genuine partial red tint, not the flat background.
        let [near_edge_r, _, near_edge_b] = pixel_rgb(&buffer, 16, 25);
        assert!(
            near_edge_r > 0x1e && near_edge_b < 0x22,
            "expected a visible red tint blended in near the box's own edge, got \
             rgb component readings r={near_edge_r:#x} b={near_edge_b:#x}"
        );
        // Outside the box (and so outside the padding box) entirely: the
        // hard clip at the padding box still holds despite the blur —
        // no soft fringe leaking past it.
        assert_eq!(
            pixel_rgb(&buffer, 10, 25),
            [0, 0, 0],
            "an inset shadow, blurred or not, must never paint past its own padding box"
        );
    }

    #[test]
    fn writes_a_real_png_file() {
        let tree: Element = view! { <div class="card" /> };
        let css = ".card { width: 20px; height: 20px; background-color: #ff0000; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let dir = std::env::temp_dir().join(format!("florui-paint-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("capture.png");

        paint_to_png(
            &mut font,
            &path,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        )
        .unwrap();

        let decoded = Pixmap::load_png(&path).unwrap();
        assert_eq!(pixel_rgb(&decoded, 10, 10), [0xff, 0, 0]);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Real block layout has no absolute positioning yet, so it can never
    /// actually produce overlapping siblings — but the painter's contract
    /// (later source order wins an overlap) should hold for whatever
    /// layout it's handed, including a synthetic one. This is also the
    /// case the old area-sorted approach got wrong: two equal-area,
    /// fully-overlapping siblings tie on area, so which one ends up on top
    /// depended on `HashMap` iteration order — unspecified, and observed
    /// to vary from run to run.
    #[test]
    fn overlapping_siblings_paint_in_document_order_not_by_area() {
        let tree: Element = view! {
            <div>
                <div class="back" />
                <div class="front" />
            </div>
        };
        let css = ".back { background-color: #ff0000; } .front { background-color: #00ff00; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let root = arena.roots()[0];
        let back = arena.children(root)[0];
        let front = arena.children(root)[1];
        let mut layouts = HashMap::new();
        layouts.insert(
            back,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        layouts.insert(
            front,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0x00, 0xff, 0x00],
            "front is later in source order, so it should paint on top of back"
        );
    }

    #[test]
    fn a_flex_items_positive_z_index_paints_it_above_a_later_sibling() {
        // "back" is later in source order (would win a plain document-order
        // tie), but "front"'s own explicit z-index outranks it — real CSS's
        // own rule for a flex item, which this is: z-index applies without
        // needing `position` at all.
        let tree: Element = view! {
            <div class="row">
                <div class="front" />
                <div class="back" />
            </div>
        };
        let css = "
            .row { display: flex; }
            .front { background-color: #00ff00; z-index: 1; }
            .back { background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let row = arena.roots()[0];
        let front = arena.children(row)[0];
        let back = arena.children(row)[1];
        let mut layouts = HashMap::new();
        layouts.insert(
            row,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        for child in [front, back] {
            layouts.insert(
                child,
                BoxLayout {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
            );
        }

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0x00, 0xff, 0x00],
            "front's positive z-index must win over back's later source position"
        );
    }

    #[test]
    fn z_index_has_no_effect_outside_a_flex_or_grid_container() {
        // Same shape as the flex test above (later source order beaten by
        // an earlier sibling's positive z-index), but the parent is a
        // plain block — real CSS gives z-index no effect there at all, so
        // document order (later wins) must still hold.
        let tree: Element = view! {
            <div class="column">
                <div class="front" />
                <div class="back" />
            </div>
        };
        let css = "
            .front { background-color: #00ff00; z-index: 1; }
            .back { background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let column = arena.roots()[0];
        let front = arena.children(column)[0];
        let back = arena.children(column)[1];
        let mut layouts = HashMap::new();
        layouts.insert(
            column,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        for child in [front, back] {
            layouts.insert(
                child,
                BoxLayout {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
            );
        }

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0xff, 0x00, 0x00],
            "z-index on a plain block child has no real CSS effect; back is later in \
             source order and must still win"
        );
    }

    #[test]
    fn z_index_has_an_effect_on_a_positioned_block_child_outside_flex_or_grid() {
        // Same shape as the two tests above, but `.front` is `position:
        // relative` -- real CSS now gives its z-index a real effect, even
        // though the parent is a plain block, not flex/grid.
        let tree: Element = view! {
            <div class="column">
                <div class="front" />
                <div class="back" />
            </div>
        };
        let css = "
            .front { background-color: #00ff00; position: relative; z-index: 1; }
            .back { background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let column = arena.roots()[0];
        let front = arena.children(column)[0];
        let back = arena.children(column)[1];
        let mut layouts = HashMap::new();
        layouts.insert(
            column,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        for child in [front, back] {
            layouts.insert(
                child,
                BoxLayout {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
            );
        }

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0x00, 0xff, 0x00],
            "front is position: relative with a real z-index now -- it must win over \
             back's later source position, the same as inside a flex/grid container"
        );
    }

    #[test]
    fn paint_order_sorts_flex_children_by_z_index_ascending_with_stable_ties() {
        let tree: Element = view! {
            <div class="row">
                <div class="a" />
                <div class="b" />
                <div class="c" />
                <div class="d" />
            </div>
        };
        // `a` and `c` tie at 2; `d` is left at the default `auto`.
        let css =
            ".row { display: flex; } .a { z-index: 2; } .b { z-index: -1; } .c { z-index: 2; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let row = arena.roots()[0];
        let children = arena.children(row).to_vec();
        let (a, b, c, d) = (children[0], children[1], children[2], children[3]);

        let ordered = paint_order(&styles, Some(florui_style::Display::Flex), &children);
        assert_eq!(
            ordered,
            vec![b, d, a, c],
            "ascending by z-index (-1, then auto treated as 0, then the 2/2 tie kept in its \
             own original a-before-c order)"
        );
    }

    #[test]
    fn a_fully_transparent_node_does_not_overwrite_what_is_beneath_it() {
        let tree: Element = view! {
            <div class="card">
                <div class="ghost" />
            </div>
        };
        let css = ".card { width: 20px; height: 20px; background-color: #1e1e22; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 10, 10), [0x1e, 0x1e, 0x22]);
    }

    #[test]
    fn an_opacity_below_1_fades_a_solid_box_toward_whatever_is_behind_it() {
        let tree: Element = view! { <div class="ghost" /> };
        let css = ".ghost { opacity: 0.5; background-color: #ff0000; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let node = arena.roots()[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            node,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        let pixel = pixel_rgb(&buffer, 10, 10);
        assert!(
            (100..160).contains(&pixel[0]) && pixel[1] == 0 && pixel[2] == 0,
            "an opaque red box at 50% group opacity over a black canvas should read as \
             roughly half-strength red, got {pixel:?}"
        );
    }

    #[test]
    fn an_opacity_of_zero_paints_nothing_at_all() {
        let tree: Element = view! {
            <div class="card">
                <div class="ghost" />
            </div>
        };
        let css = "
            .card { width: 20px; height: 20px; background-color: #1e1e22; }
            .ghost { opacity: 0; background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0x1e, 0x1e, 0x22],
            "an opacity: 0 child must paint nothing at all, leaving its parent's own \
             background untouched"
        );
    }

    #[test]
    fn group_opacity_does_not_double_blend_children_that_overlap_inside_the_group() {
        // The whole reason this is "group" opacity and not a per-node
        // alpha multiply: back and front are both fully opaque relative
        // to *each other* (front, painted later, fully occludes back
        // wherever they overlap), and only the group's own combined
        // result fades against whatever is behind it. Painting each
        // child at the group's reduced opacity individually instead
        // would show the overlap as a visibly different red/green blend
        // (back's red bleeding through front) rather than pure green
        // faded once.
        let tree: Element = view! {
            <div class="group">
                <div class="back" />
                <div class="front" />
            </div>
        };
        let css = "
            .group { opacity: 0.5; }
            .back { background-color: #ff0000; }
            .front { background-color: #00ff00; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let group = arena.roots()[0];
        let back = arena.children(group)[0];
        let front = arena.children(group)[1];
        let mut layouts = HashMap::new();
        layouts.insert(
            group,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );
        layouts.insert(
            back,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 30.0,
            },
        );
        layouts.insert(
            front,
            BoxLayout {
                x: 10.0,
                y: 10.0,
                width: 30.0,
                height: 30.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        let overlap = pixel_rgb(&buffer, 20, 20);
        assert!(
            overlap[0] < 20,
            "the overlap must read as green faded by the group's own opacity (red channel \
             near zero), not a red/green blend from fading each child individually — got \
             {overlap:?}"
        );
        assert!(
            (100..160).contains(&overlap[1]),
            "green should be roughly half-strength after the group's 50% opacity, got \
             {overlap:?}"
        );

        let red_only = pixel_rgb(&buffer, 5, 20);
        assert!(
            (100..160).contains(&red_only[0]) && red_only[1] == 0,
            "outside the overlap, back's own red must still fade correctly, got {red_only:?}"
        );
    }

    #[test]
    fn overflow_hidden_clips_a_child_that_extends_past_the_parents_padding_box() {
        let tree: Element = view! {
            <div class="frame">
                <div class="content" />
            </div>
        };
        let css = "
            .frame { width: 20px; height: 20px; overflow: hidden; }
            .content { background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let frame = arena.roots()[0];
        let content = arena.children(frame)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            frame,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        // Deliberately much larger than, and positioned to spill past,
        // the frame's own 20x20 box on every side.
        layouts.insert(
            content,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0xff, 0, 0],
            "inside the frame's own bounds, the child must still paint normally"
        );
        assert_eq!(
            pixel_rgb(&buffer, 30, 30),
            [0, 0, 0],
            "past the frame's own padding box, the child's overflow must be clipped away, \
             leaving the plain canvas background"
        );
    }

    fn rounded_frame_with_child(child_css: &str) -> Canvas {
        let tree: Element = view! {
            <div class="frame">
                <div class="content" />
            </div>
        };
        let css = format!(
            ".frame {{ width: 20px; height: 20px; overflow: hidden; border-radius: 10px; }} \
             .content {{ background-color: #ff0000; {child_css} }}"
        );
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(&css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let frame = arena.roots()[0];
        let content = arena.children(frame)[0];
        let mut layouts = HashMap::new();
        for (node, size) in [(frame, 20.0), (content, 40.0)] {
            layouts.insert(
                node,
                BoxLayout {
                    x: 0.0,
                    y: 0.0,
                    width: size,
                    height: size,
                },
            );
        }
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        )
    }

    #[test]
    fn overflow_hidden_clips_a_child_to_the_rounded_corner() {
        let buffer = rounded_frame_with_child("");
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0xff, 0, 0],
            "center is visible"
        );
        assert_eq!(
            pixel_rgb(&buffer, 1, 1),
            [0, 0, 0],
            "inside the frame's box but outside its rounded corner, the child is clipped"
        );
    }

    #[test]
    fn a_rounded_clip_survives_a_bounded_group_surface() {
        assert_bounded_and_full_target_paint_identically(|| {
            rounded_frame_with_child("opacity: 0.5;")
        });
        let buffer = rounded_frame_with_child("opacity: 0.5;");
        assert_eq!(pixel_rgb(&buffer, 1, 1), [0, 0, 0]);
        assert_ne!(pixel_rgb(&buffer, 10, 10), [0, 0, 0]);
    }

    #[test]
    fn overflow_visible_the_default_does_not_clip_an_overflowing_child() {
        // The exact same shape as the `overflow: hidden` test above, minus
        // that one declaration — proves the clip in that test comes from
        // `overflow: hidden` specifically, not from some other limit (a
        // canvas edge, a coincidentally-unpainted region) that would make
        // that test pass for the wrong reason.
        let tree: Element = view! {
            <div class="frame">
                <div class="content" />
            </div>
        };
        let css = "
            .frame { width: 20px; height: 20px; }
            .content { background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let frame = arena.roots()[0];
        let content = arena.children(frame)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            frame,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        layouts.insert(
            content,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        assert_eq!(
            pixel_rgb(&buffer, 30, 30),
            [0xff, 0, 0],
            "overflow: visible (the default) must not clip the child at all"
        );
    }

    #[test]
    fn nested_overflow_hidden_clips_to_the_intersection_of_both_ancestors() {
        // The outer frame alone would let the child show through at
        // (25, 5) (inside the outer's 30x30 box, outside the inner's own
        // narrower 15x15 one) -- only the *intersection* of both clips
        // correctly hides it there too.
        let tree: Element = view! {
            <div class="outer">
                <div class="inner">
                    <div class="content" />
                </div>
            </div>
        };
        let css = "
            .outer { width: 30px; height: 30px; overflow: hidden; }
            .inner { width: 15px; height: 15px; overflow: hidden; }
            .content { background-color: #ff0000; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );

        let outer = arena.roots()[0];
        let inner = arena.children(outer)[0];
        let content = arena.children(inner)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            outer,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 30.0,
            },
        );
        layouts.insert(
            inner,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 15.0,
                height: 15.0,
            },
        );
        layouts.insert(
            content,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [0xff, 0, 0],
            "inside both the inner and outer bounds, the content must still paint"
        );
        assert_eq!(
            pixel_rgb(&buffer, 25, 5),
            [0, 0, 0],
            "inside the outer's own 30x30 box but past the inner's own narrower 15x15 one, \
             the intersection of both clips must still hide it"
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 25),
            [0, 0, 0],
            "past the outer's own 30x30 box entirely, the intersection must hide it here too"
        );
    }

    #[test]
    fn overflow_hidden_does_not_clip_its_own_background_or_border() {
        // Real CSS: `overflow` clips a box's *content*, never the box's
        // own border/background against its own clip -- only a
        // descendant can be clipped by it.
        let tree: Element = view! { <div class="frame" /> };
        let css = "
            .frame {
                width: 20px;
                height: 20px;
                overflow: hidden;
                background-color: #ff0000;
                border-top-width: 4px;
                border-top-style: solid;
                border-top-color: #00ff00;
            }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let node = arena.roots()[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            node,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );

        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 2, 2),
            [0, 0xff, 0],
            "the frame's own border must still paint in full"
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0xff, 0, 0],
            "the frame's own background must still paint in full"
        );
    }

    #[test]
    fn text_paints_visible_ink_in_its_declared_color() {
        let tree: Element = view! { <h2>{"H"}</h2> };
        let css = "h2 { color: #ff0000; font-size: 40px; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let node = arena.roots()[0];
        let width = layouts[&node].width.ceil() as u32;
        let height = layouts[&node].height.ceil() as u32;
        let buffer = paint_to_buffer(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // A large "H" fills a good portion of its own tight box; scanning
        // for any red-ish pixel proves real glyph ink landed somewhere,
        // without pinning the test to one exact anti-aliased pixel.
        let mut found_ink = false;
        for py in 0..height {
            for px in 0..width {
                let pixel = pixel_rgb(&buffer, px, py);
                if pixel[0] > 0x80 && pixel[1] < 0x40 && pixel[2] < 0x40 {
                    found_ink = true;
                }
            }
        }
        assert!(found_ink, "expected at least one red glyph pixel");
    }

    /// Renders `<a href>"H"</a>` at a fixed size under `css` and returns
    /// `(buffer, width, height)` — shared by the underline tests below
    /// so they only differ in the one declaration that matters. `<a>`,
    /// not `<h2>`: no default margin to account for when sizing the
    /// buffer to `layouts[&node]` alone.
    fn render_a(css: &str) -> (Canvas, u32, u32) {
        let tree: Element = view! { <a href="https://example.com">{"H"}</a> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let node = arena.roots()[0];
        let width = layouts[&node].width.ceil() as u32;
        let height = layouts[&node].height.ceil() as u32;
        let buffer = paint_to_buffer(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        (buffer, width, height)
    }

    fn lowest_red_row(buffer: &Canvas, width: u32, height: u32) -> Option<u32> {
        (0..height).rev().find(|&py| {
            (0..width).any(|px| {
                let pixel = pixel_rgb(buffer, px, py);
                pixel[0] > 0x80 && pixel[1] < 0x40 && pixel[2] < 0x40
            })
        })
    }

    #[test]
    fn text_decoration_underline_paints_a_mark_below_the_glyphs_own_ink() {
        // Explicit `text-decoration: none`, not just an author rule with
        // no `text-decoration` at all: the default stylesheet's own
        // `a:link`/`a:visited` rule (see `default_stylesheet.rs`) already
        // underlines a bare `<a href>` by default, real Chromium's own
        // behavior too -- an author has to override it explicitly to get
        // a genuinely undecorated baseline to compare against.
        let css = "a { color: #ff0000; font-size: 40px; text-decoration: none; }";
        let (plain, width, height) = render_a(css);
        let plain_bottom = lowest_red_row(&plain, width, height)
            .expect("expected at least one red glyph pixel with no underline");

        let underlined_css = "a { color: #ff0000; font-size: 40px; text-decoration: underline; }";
        let (underlined, width, height) = render_a(underlined_css);
        let underlined_bottom = lowest_red_row(&underlined, width, height)
            .expect("expected at least one red pixel with underline declared");

        assert!(
            underlined_bottom > plain_bottom,
            "the underline must paint strictly below the glyph's own lowest ink \
             (plain bottom row {plain_bottom}, underlined bottom row {underlined_bottom})"
        );
    }

    #[test]
    fn line_through_alone_paints_no_underline() {
        // Same `text-decoration: none` reasoning as the test above.
        let css = "a { color: #ff0000; font-size: 40px; text-decoration: none; }";
        let (plain, width, height) = render_a(css);
        let plain_bottom = lowest_red_row(&plain, width, height).unwrap();

        let line_through_css =
            "a { color: #ff0000; font-size: 40px; text-decoration: line-through; }";
        let (line_through, width, height) = render_a(line_through_css);
        let line_through_bottom = lowest_red_row(&line_through, width, height).unwrap();

        assert_eq!(
            plain_bottom, line_through_bottom,
            "line-through alone must not paint an underline mark"
        );
    }

    #[test]
    fn a_text_input_paints_its_caret_in_its_own_declared_color() {
        let tree: Element = view! { <input type="text" value="Hi" style="color: #ff0000;" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet("").unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let node = arena.roots()[0];
        let width = layouts[&node].width.ceil() as u32 + 10;
        let height = layouts[&node].height.ceil() as u32;

        // A caret placed 4px past the text's own measured width -- past
        // every real glyph, so any red pixel there can only be the caret,
        // not anti-aliased glyph ink.
        let text_width = font
            .measure(florui_text::FontFamily::SansSerif, "Hi", 16.0, 400.0)
            .width;
        let mut text_inputs = HashMap::new();
        text_inputs.insert(
            node,
            TextInputPaint {
                runs: font
                    .shape(florui_text::FontFamily::SansSerif, "Hi", 16.0, 400.0)
                    .runs,
                caret_rect: Some((text_width + 4.0, 0.0, text_width + 4.0, height as f32)),
                selection_rects: Vec::new(),
                compose_rect: None,
                show_caret: true,
            },
        );

        let buffer = paint_to_buffer_with_desktop_extras(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
            Some(&text_inputs),
            None,
        );

        // Scanned only past the text's own measured width plus a small
        // margin -- real glyph ink never extends past its own advance by
        // more than a pixel or two, so any red pixel out here can only be
        // the caret, not anti-aliased text.
        let past_text = (text_width.ceil() as u32 + 2).min(width);
        let mut found_caret = false;
        for py in 0..height {
            for px in past_text..width {
                let pixel = pixel_rgb(&buffer, px, py);
                if pixel[0] > 0x80 && pixel[1] < 0x40 && pixel[2] < 0x40 {
                    found_caret = true;
                }
            }
        }
        assert!(
            found_caret,
            "expected a red caret pixel past x={past_text} (text width {text_width})"
        );
    }

    #[test]
    fn bold_paints_thicker_strokes_than_regular_at_the_same_size() {
        // Real variation-coordinate rendering, not just wider layout
        // metrics: bold's own glyph outlines must cover more ink pixels
        // than regular's at the identical size — this is exactly the bug
        // a size-only measurement test would miss (the width changed, but
        // outlines still drew at the font's default instance).
        fn ink_pixel_count(font_weight: &str) -> u32 {
            let tree: Element = view! { <h2>{"H"}</h2> };
            let css =
                format!("h2 {{ color: #ff0000; font-size: 60px; font-weight: {font_weight}; }}");
            let arena = Arena::build(&tree);
            let rules = florui_style::parse_stylesheet(&css).unwrap();
            let styles = florui_style::compute(
                &arena,
                &rules,
                &InteractionState::new(),
                florui_style::Viewport::default(),
                &mut florui_style::AnimationTimeline::default(),
            );
            let mut font = Font::load_embedded();
            let layouts =
                florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT)
                    .unwrap();

            let node = arena.roots()[0];
            let width = layouts[&node].width.ceil() as u32;
            let height = layouts[&node].height.ceil() as u32;
            let buffer = paint_to_buffer(
                &mut font,
                width,
                height,
                Rgba::opaque(0, 0, 0),
                &arena,
                &styles,
                &layouts,
                1.0,
            );

            let mut count = 0;
            for py in 0..height {
                for px in 0..width {
                    if pixel_rgb(&buffer, px, py)[0] > 0x20 {
                        count += 1;
                    }
                }
            }
            count
        }

        let regular = ink_pixel_count("400");
        let bold = ink_pixel_count("700");
        assert!(
            bold > regular,
            "bold ({bold} ink pixels) must cover more area than regular ({regular})"
        );
    }

    #[test]
    fn a_real_inline_formatting_context_paints_ink_from_both_the_text_and_the_inline_element() {
        // `Element::node`/`Element::text` directly — real mixed inline
        // content, which `view!` has no ergonomic syntax for. Before
        // real inline formatting context support, `florui-paint` painted
        // a node with element children using only `arena.text_content`,
        // which flattens through nested elements and would have silently
        // dropped "B" — this test is exactly what would have caught that:
        // ink must appear on *both* sides of the box, not just the left.
        let tree = Element::node(
            "p",
            vec![],
            vec![
                Element::text("A"),
                Element::node("span", vec![], vec![Element::text("B")]),
            ],
        );
        let css = "p { color: #ff0000; font-size: 40px; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let node = arena.roots()[0];
        let width = layouts[&node].width.ceil() as u32;
        let height = layouts[&node].height.ceil() as u32;
        let buffer = paint_to_buffer(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // A lower bar than "fully-opaque red" (the single-glyph "H" test's
        // own threshold): two adjacent, mostly-curved glyphs like "A"/"B"
        // at this size can anti-alias every pixel of their outline without
        // any one pixel reaching full coverage, unlike "H"'s thick straight
        // strokes — any non-trivial red channel value still distinguishes
        // real ink from the plain black background.
        let is_ink = |px: u32, py: u32| pixel_rgb(&buffer, px, py)[0] > 0x20;
        let half = width / 2;
        let mut ink_in_left_half = false;
        let mut ink_in_right_half = false;
        for py in 0..height {
            for px in 0..width {
                if is_ink(px, py) {
                    if px < half {
                        ink_in_left_half = true;
                    } else {
                        ink_in_right_half = true;
                    }
                }
            }
        }
        assert!(ink_in_left_half, "expected ink from \"A\" on the left");
        assert!(
            ink_in_right_half,
            "expected ink from the inline <span>'s own \"B\" on the right — \
             text_content-only painting would have dropped it entirely"
        );
    }

    #[test]
    fn empty_text_content_paints_no_ink() {
        let tree: Element = view! { <div class="card" /> };
        let css = ".card { width: 20px; height: 20px; background-color: #1e1e22; color: #ff0000; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        // No text content, so every pixel is exactly the flat background —
        // no stray glyph ink from a leaf with nothing to shape.
        for py in 0..20 {
            for px in 0..20 {
                assert_eq!(pixel_rgb(&buffer, px, py), [0x1e, 0x1e, 0x22]);
            }
        }
    }

    /// `paint_node` used to recurse once per tree level; iterative now.
    /// 1,200, matching florui-layout's own equivalent test: past the
    /// original 1,000-deep crash report, but not far past it.
    #[test]
    fn paint_to_buffer_survives_a_tree_far_deeper_than_the_old_recursion_limit() {
        let depth = 1_200;
        let mut tree = Element::node("div", vec![("class".into(), "leaf".into())], vec![]);
        for _ in 0..depth {
            tree = Element::node("div", vec![], vec![tree]);
        }
        let css = ".leaf { width: 5px; height: 5px; background-color: #ff0000; }";

        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();

        let buffer = paint_to_buffer(
            &mut font,
            5,
            5,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 0, 0), [0xff, 0x00, 0x00]);
    }

    #[test]
    fn text_still_wraps_at_the_logical_width_when_painted_onto_a_scaled_hidpi_canvas() {
        // A HiDPI caller (a real window, or `florui_conformance::engine`'s
        // own DPR fixtures) passes `layouts` already scaled up to physical
        // pixels — if painting reshaped text at that scaled box width
        // instead of dividing back to the logical width layout wrapped
        // against, it would wrap wider than the committed layout and
        // overflow the second line straight into empty canvas.
        let text = "Hello world this line is long enough to wrap";
        let css = "p { width: 100px; font-size: 16px; color: #ff0000; }";
        let tree = Element::node("p", vec![], vec![Element::text(text)]);

        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let layouts = florui_layout::compute_layout(
            &mut font,
            &arena,
            &styles,
            Size {
                width: AvailableSpace::Definite(200.0),
                height: AvailableSpace::MaxContent,
            },
        )
        .unwrap();

        let node = arena.roots()[0];
        let logical_width = layouts[&node].width;
        let logical_height = layouts[&node].height;

        // Proves the chosen text really does wrap to fewer lines at double
        // the width — otherwise the scenario below wouldn't exercise the
        // bug this test guards against at all.
        let unwrapped_at_double_width = font
            .measure_wrapped(
                florui_text::FontFamily::SansSerif,
                text,
                16.0,
                400.0,
                logical_width * 2.0,
            )
            .height;
        assert!(
            unwrapped_at_double_width < logical_height,
            "the chosen text must wrap to fewer lines at double the width, or this test proves \
             nothing"
        );

        let scale_factor = 2.0;
        let physical_layouts = layouts
            .iter()
            .map(|(&id, l)| {
                (
                    id,
                    BoxLayout {
                        x: l.x * scale_factor,
                        y: l.y * scale_factor,
                        width: l.width * scale_factor,
                        height: l.height * scale_factor,
                    },
                )
            })
            .collect();

        let width = (logical_width * scale_factor).ceil() as u32;
        let height = (logical_height * scale_factor).ceil() as u32;
        let buffer = paint_to_buffer(
            &mut font,
            width,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &physical_layouts,
            scale_factor,
        );

        // If painting had wrapped at the scaled (physical) width instead
        // of the logical one, the whole text would fit on its first line
        // and the canvas's bottom half — reserved by layout for the
        // second wrapped line — would be pure background, no ink.
        let is_ink = |px: u32, py: u32| pixel_rgb(&buffer, px, py)[0] > 0x20;
        let bottom_half_has_ink =
            (height / 2..height).any(|py| (0..width).any(|px| is_ink(px, py)));
        assert!(
            bottom_half_has_ink,
            "text painted onto a scaled canvas must still wrap at the logical width, filling \
             the second line layout reserved room for — not re-wrap wider and collapse onto \
             one line"
        );
    }

    #[test]
    fn a_glyph_painted_onto_a_scaled_hidpi_canvas_is_rasterized_at_the_scaled_size_not_just_repositioned()
     {
        // The bug this guards against: positioning glyphs at physical
        // coordinates (correct) while still rasterizing their outlines at
        // the *logical* font size paints text roughly `scale_factor` times
        // smaller than a real HiDPI render — box geometry still matches
        // (nothing here depends on glyph size), so a percent-different
        // pixel tolerance can pass even though the rendered ink itself is
        // visibly wrong, since thin glyph strokes cover little of a mostly
        // blank canvas.
        fn ink_height(scale_factor: f32) -> u32 {
            let tree: Element = view! { <h2>{"H"}</h2> };
            let css = "h2 { color: #ff0000; font-size: 40px; }";
            let arena = Arena::build(&tree);
            let rules = florui_style::parse_stylesheet(css).unwrap();
            let styles = florui_style::compute(
                &arena,
                &rules,
                &InteractionState::new(),
                florui_style::Viewport::default(),
                &mut florui_style::AnimationTimeline::default(),
            );
            let mut font = Font::load_embedded();
            let layouts =
                florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT)
                    .unwrap();

            let node = arena.roots()[0];
            // The whole node, margin offset included: a canvas of only the
            // node's own size would crop the glyph by a different amount at
            // each scale.
            let logical_width = layouts[&node].x + layouts[&node].width;
            let logical_height = layouts[&node].y + layouts[&node].height;
            let physical_layouts = layouts
                .iter()
                .map(|(&id, l)| {
                    (
                        id,
                        BoxLayout {
                            x: l.x * scale_factor,
                            y: l.y * scale_factor,
                            width: l.width * scale_factor,
                            height: l.height * scale_factor,
                        },
                    )
                })
                .collect();
            let width = (logical_width * scale_factor).ceil() as u32;
            let height = (logical_height * scale_factor).ceil() as u32;
            let buffer = paint_to_buffer(
                &mut font,
                width,
                height,
                Rgba::opaque(0, 0, 0),
                &arena,
                &styles,
                &physical_layouts,
                scale_factor,
            );

            let is_ink = |px: u32, py: u32| pixel_rgb(&buffer, px, py)[0] > 0x20;
            let mut min_y = None;
            let mut max_y = None;
            for py in 0..height {
                if (0..width).any(|px| is_ink(px, py)) {
                    min_y.get_or_insert(py);
                    max_y = Some(py);
                }
            }
            let min_y = min_y.expect("the glyph must paint some ink");
            let max_y = max_y.expect("the glyph must paint some ink");
            max_y - min_y + 1
        }

        let height_at_1x = ink_height(1.0);
        let height_at_2x = ink_height(2.0);
        let ratio = f64::from(height_at_2x) / f64::from(height_at_1x);
        assert!(
            (1.8..=2.2).contains(&ratio),
            "a glyph on a 2x-scaled canvas must paint roughly twice as tall as on an unscaled \
             one (ink height {height_at_1x}px -> {height_at_2x}px, ratio {ratio:.2}), not stay \
             at the logical size just repositioned"
        );
    }

    fn single_box_buffer(css: &str, canvas: u32, scale_factor: f32) -> Canvas {
        single_box_buffer_at(css, canvas, scale_factor, 0.0, 0.0)
    }

    fn single_box_buffer_at(css: &str, canvas: u32, scale_factor: f32, x: f32, y: f32) -> Canvas {
        let tree: Element = view! { <div class="box" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let node = arena.roots()[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            node,
            BoxLayout {
                x,
                y,
                width: 10.0 * scale_factor,
                height: 10.0 * scale_factor,
            },
        );
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            canvas,
            canvas,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            scale_factor,
        )
    }

    #[test]
    fn a_rounded_background_leaves_its_corners_unpainted() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; border-radius: 5px; }",
            10,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 0, 0), [0, 0, 0], "corner stays canvas");
        assert_eq!(pixel_rgb(&buffer, 9, 9), [0, 0, 0]);
        assert_eq!(pixel_rgb(&buffer, 5, 5), [255, 0, 0], "center is filled");
        assert_eq!(
            pixel_rgb(&buffer, 5, 0),
            [255, 0, 0],
            "edge midpoint is filled"
        );
    }

    #[test]
    fn a_zero_radius_box_is_unchanged_from_the_plain_rect_path() {
        let buffer = single_box_buffer(".box { background-color: #ff0000; }", 10, 1.0);
        assert_eq!(pixel_rgb(&buffer, 0, 0), [255, 0, 0]);
        assert_eq!(pixel_rgb(&buffer, 9, 9), [255, 0, 0]);
    }

    #[test]
    fn a_rounded_border_follows_the_outline_and_keeps_the_interior() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; border-radius: 5px; border-width: 2px; \
             border-style: solid; border-color: #0000ff; }",
            10,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 0, 0), [0, 0, 0], "corner stays canvas");
        assert_eq!(pixel_rgb(&buffer, 5, 0), [0, 0, 255], "top edge is border");
        assert_eq!(pixel_rgb(&buffer, 0, 5), [0, 0, 255], "left edge is border");
        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [255, 0, 0],
            "interior is background"
        );
    }

    #[test]
    fn a_rounded_border_with_differing_side_colors_miters_at_the_corners() {
        let buffer = single_box_buffer(
            ".box { border-radius: 3px; border-width: 3px; border-style: solid; \
             border-top-color: #00ff00; border-right-color: #0000ff; \
             border-bottom-color: #0000ff; border-left-color: #0000ff; }",
            10,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 5, 0), [0, 255, 0], "top side");
        assert_eq!(pixel_rgb(&buffer, 0, 5), [0, 0, 255], "left side");
        assert_eq!(pixel_rgb(&buffer, 5, 9), [0, 0, 255], "bottom side");
    }

    #[test]
    fn a_rounded_outer_shadow_follows_the_outline_and_hides_behind_the_box() {
        let buffer = single_box_buffer_at(
            ".box { border-radius: 5px; box-shadow: 0 0 0 4px #00ff00; }",
            30,
            1.0,
            10.0,
            10.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 15, 15),
            [0, 0, 0],
            "hidden behind the box"
        );
        assert_eq!(
            pixel_rgb(&buffer, 7, 15),
            [0, 255, 0],
            "spread ring beside the box"
        );
        assert_eq!(
            pixel_rgb(&buffer, 10, 10),
            [0, 255, 0],
            "the box's own cut corner shows the shadow through it"
        );
        assert_eq!(
            pixel_rgb(&buffer, 6, 6),
            [0, 0, 0],
            "past the grown, rounded corner"
        );
    }

    #[test]
    fn a_rounded_inset_shadow_stays_inside_the_rounded_padding_box() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; border-radius: 5px; \
             box-shadow: inset 0 0 0 2px #00ff00; }",
            10,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 1),
            [0, 255, 0],
            "inset ring at the top edge"
        );
        assert_eq!(pixel_rgb(&buffer, 5, 5), [255, 0, 0], "center is the hole");
        assert_eq!(
            pixel_rgb(&buffer, 0, 0),
            [0, 0, 0],
            "never past the rounded corner"
        );
    }

    #[test]
    fn a_blurred_rounded_outer_shadow_is_soft_outside_and_absent_inside() {
        let buffer = single_box_buffer_at(
            ".box { border-radius: 5px; box-shadow: 0 0 6px #00ff00; }",
            30,
            1.0,
            10.0,
            10.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 15, 15),
            [0, 0, 0],
            "hidden behind the box"
        );
        let beside = pixel_rgb(&buffer, 8, 15);
        assert!(
            beside[1] > 0 && beside[1] < 255,
            "soft edge outside the box: {beside:?}"
        );
    }

    #[test]
    fn a_rounded_box_paints_the_same_in_a_bounded_and_a_full_target_group() {
        assert_bounded_and_full_target_paint_identically(|| {
            single_box_buffer_at(
                ".box { background-color: #ff0000; border-radius: 4px; opacity: 0.5; }",
                30,
                1.0,
                7.0,
                9.0,
            )
        });
    }

    #[test]
    fn translate_moves_a_solid_box_to_its_own_new_position() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; transform: translate(15px, 5px); }",
            30,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [0, 0, 0],
            "the box's own untransformed spot must go back to the canvas's clear color"
        );
        assert_eq!(
            pixel_rgb(&buffer, 20, 10),
            [0xff, 0, 0],
            "the box must actually paint at translate()'s own new position"
        );
    }

    #[test]
    fn translate_percentage_resolves_against_the_nodes_own_box() {
        // A 10px-wide/tall box: translate(50%, 0) must move it by 5px on
        // the x axis alone, the percentage resolving against *this
        // node's own* box (real CSS's own reference box for `translate`),
        // not some other size.
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; transform: translate(50%, 0); }",
            20,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 2, 5), [0, 0, 0], "left of the moved box");
        assert_eq!(
            pixel_rgb(&buffer, 7, 5),
            [0xff, 0, 0],
            "inside the moved box"
        );
    }

    #[test]
    fn translates_own_length_scales_with_a_hidpi_canvas_but_its_percentage_does_not_need_to() {
        // Same logical move (`translate(10px, 0)` on a box already
        // scaled to physical pixels by the caller) at two different
        // scale factors: the *physical* distance the box moves must
        // scale right along with the canvas, the same correction real
        // text painting already needs for its own glyph offsets.
        let buffer_1x = single_box_buffer(
            ".box { background-color: #ff0000; transform: translate(10px, 0); }",
            30,
            1.0,
        );
        let buffer_2x = single_box_buffer(
            ".box { background-color: #ff0000; transform: translate(10px, 0); }",
            60,
            2.0,
        );
        assert_eq!(
            pixel_rgb(&buffer_1x, 15, 5),
            [0xff, 0, 0],
            "moved 10 logical px at 1x"
        );
        assert_eq!(
            pixel_rgb(&buffer_2x, 30, 10),
            [0xff, 0, 0],
            "the same 10 logical px must land 20 physical px over on a 2x canvas"
        );
    }

    #[test]
    fn rotate_pivots_around_the_boxs_own_default_center_origin() {
        // A box with a red top border only: rotating 180 degrees around
        // the default center origin must flip that border to the
        // opposite (bottom) edge — proof both that rotation actually
        // rotates and that the default `transform-origin` (the box's own
        // center, not its top-left corner) is right, since pivoting
        // around the corner would move the whole box off-canvas instead
        // of flipping it in place.
        let buffer = single_box_buffer(
            ".box {
                background-color: #1e1e22;
                border-top-width: 3px; border-top-style: solid; border-top-color: #ff0000;
                transform: rotate(180deg);
            }",
            10,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 1),
            [0x1e, 0x1e, 0x22],
            "the top edge no longer shows the border — it flipped away from here"
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 8),
            [0xff, 0, 0],
            "bottom edge now shows the red border"
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [0x1e, 0x1e, 0x22],
            "the middle stays the box's own background"
        );
    }

    #[test]
    fn scale_grows_the_box_around_its_own_default_center_origin() {
        // A 10x10 box at (10, 10) — centered at (15, 15) — scaled 2x
        // around its own default center must span 20x20 still centered
        // on (15, 15), i.e. (5, 5)-(25, 25): 5px past each of its
        // original edges on *every* side, not 10px past only the
        // bottom-right ones (which a top-left-pivoted scale would
        // produce instead).
        let buffer = single_box_buffer_at(
            ".box { background-color: #ff0000; transform: scale(2); }",
            30,
            1.0,
            10.0,
            10.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 3, 15),
            [0, 0, 0],
            "still outside the scaled box's new left edge"
        );
        assert_eq!(
            pixel_rgb(&buffer, 7, 15),
            [0xff, 0, 0],
            "just inside the scaled box's new left edge"
        );
        assert_eq!(
            pixel_rgb(&buffer, 23, 15),
            [0xff, 0, 0],
            "just inside the scaled box's new right edge"
        );
        assert_eq!(
            pixel_rgb(&buffer, 27, 15),
            [0, 0, 0],
            "still outside the scaled box's new right edge"
        );
    }

    #[test]
    fn matrix_produces_the_same_result_as_the_equivalent_translate() {
        let via_translate = single_box_buffer(
            ".box { background-color: #ff0000; transform: translate(15px, 5px); }",
            30,
            1.0,
        );
        let via_matrix = single_box_buffer(
            ".box { background-color: #ff0000; transform: matrix(1, 0, 0, 1, 15, 5); }",
            30,
            1.0,
        );
        for y in 0..30 {
            for x in 0..30 {
                assert_eq!(
                    pixel_rgb(&via_matrix, x, y),
                    pixel_rgb(&via_translate, x, y),
                    "matrix(1,0,0,1,15,5) must paint identically to translate(15px, 5px) at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn transform_functions_compose_left_to_right_not_commutatively() {
        // Real CSS composes a transform list so the *last*-listed
        // function acts on the original point first and the
        // *first*-listed one acts last, in the coordinate system every
        // earlier function already established — so `translate(10px, 0)
        // rotate(90deg)` (rotate first, in the box's own local frame,
        // then translate in the outer/world frame) must not paint the
        // same as `rotate(90deg) translate(10px, 0)` (translate first,
        // then that already-shifted result gets rotated around the
        // box's own original center). Any reversed composition order
        // would make these two indistinguishable or swapped.
        let translate_then_rotate = single_box_buffer(
            ".box { background-color: #ff0000; transform: translate(10px, 0) rotate(90deg); }",
            30,
            1.0,
        );
        let rotate_then_translate = single_box_buffer(
            ".box { background-color: #ff0000; transform: rotate(90deg) translate(10px, 0); }",
            30,
            1.0,
        );
        let mut any_pixel_differs = false;
        for y in 0..30 {
            for x in 0..30 {
                if pixel_rgb(&translate_then_rotate, x, y)
                    != pixel_rgb(&rotate_then_translate, x, y)
                {
                    any_pixel_differs = true;
                }
            }
        }
        assert!(
            any_pixel_differs,
            "translate(10px,0) rotate(90deg) and rotate(90deg) translate(10px,0) must not paint \
             identically — composition order matters in real CSS"
        );
    }

    #[test]
    fn opacity_and_transform_apply_together_in_the_same_group() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; opacity: 0.5; transform: translate(15px, 0); }",
            30,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [0, 0, 0],
            "the box's own untransformed spot must be untouched — opacity alone must not leave \
             it painted in place"
        );
        assert_eq!(
            pixel_rgb(&buffer, 20, 5),
            [0x80, 0, 0],
            "the moved box must still be faded to half opacity against the canvas's own black"
        );
    }

    #[test]
    fn an_unsupported_skew_function_is_treated_as_identity() {
        // `skew()` isn't in this crate's documented 2D subset (see
        // `TransformFunction`'s own doc) and drops out of the resolved
        // list entirely — so a box with only a skew must paint exactly
        // where it would with no `transform` at all, not distorted and
        // not silently discarded along with the rest of its own styles.
        let with_skew = single_box_buffer(
            ".box { background-color: #ff0000; transform: skewX(30deg); }",
            30,
            1.0,
        );
        let without_transform = single_box_buffer(".box { background-color: #ff0000; }", 30, 1.0);
        for y in 0..30 {
            for x in 0..30 {
                assert_eq!(
                    pixel_rgb(&with_skew, x, y),
                    pixel_rgb(&without_transform, x, y)
                );
            }
        }
    }

    #[test]
    fn blur_spreads_a_solid_boxs_own_color_past_its_own_edge() {
        let filtered = single_box_buffer_at(
            ".box { background-color: #ff0000; filter: blur(6px); }",
            30,
            1.0,
            10.0,
            10.0,
        );
        let unfiltered =
            single_box_buffer_at(".box { background-color: #ff0000; }", 30, 1.0, 10.0, 10.0);
        assert_eq!(
            pixel_rgb(&unfiltered, 8, 15),
            [0, 0, 0],
            "sanity check: 2px outside the unblurred box is pure background"
        );
        let blurred_pixel = pixel_rgb(&filtered, 8, 15);
        assert_ne!(
            blurred_pixel,
            [0, 0, 0],
            "a blurred box's own color must bleed past its own original edge"
        );
        assert_ne!(
            blurred_pixel,
            [0xff, 0, 0],
            "but not at full strength right at the blur's own reach"
        );
    }

    #[test]
    fn brightness_of_zero_paints_the_box_fully_black_without_touching_its_own_alpha() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff8040; filter: brightness(0); }",
            20,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0, 0, 0]);
    }

    #[test]
    fn brightness_above_one_brightens_and_clamps_at_full_strength() {
        let buffer = single_box_buffer(
            ".box { background-color: #804020; filter: brightness(4); }",
            20,
            1.0,
        );
        // 0x80*4 and 0x40*4 both saturate to 0xff; 0x20*4 = 0x80 exactly.
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0xff, 0xff, 0x80]);
    }

    #[test]
    fn contrast_pushes_a_light_color_further_from_mid_gray() {
        let buffer = single_box_buffer(
            ".box { background-color: #c0c0c0; filter: contrast(2); }",
            20,
            1.0,
        );
        // (0xc0 - 127.5) * 2 + 127.5 = 256.5, clamped to 255.
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0xff, 0xff, 0xff]);
    }

    #[test]
    fn contrast_of_zero_flattens_every_color_to_mid_gray() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; filter: contrast(0); }",
            20,
            1.0,
        );
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0x80, 0x80, 0x80]);
    }

    #[test]
    fn saturate_of_zero_desaturates_to_the_colors_own_luma() {
        let buffer = single_box_buffer(
            ".box { background-color: #ff0000; filter: saturate(0); }",
            20,
            1.0,
        );
        // Luma of pure red at this crate's own saturate coefficients:
        // 0.213 * 255 ~= 54.
        let [r, g, b] = pixel_rgb(&buffer, 5, 5);
        assert_eq!(r, g, "a fully desaturated pixel has equal channels");
        assert_eq!(g, b, "a fully desaturated pixel has equal channels");
        assert!(
            (50..=58).contains(&r),
            "expected red's own luma (~54), got {r}"
        );
    }

    #[test]
    fn filter_functions_apply_in_authored_order_not_commutatively() {
        // brightness(2) then contrast(0) must land on mid-gray (contrast
        // ignores whatever brightness already did); contrast(0) then
        // brightness(2) must double that same mid-gray instead. If this
        // crate applied the chain in the wrong order, both would produce
        // the identical result.
        let brighten_then_flatten = single_box_buffer(
            ".box { background-color: #300000; filter: brightness(2) contrast(0); }",
            20,
            1.0,
        );
        let flatten_then_brighten = single_box_buffer(
            ".box { background-color: #300000; filter: contrast(0) brightness(2); }",
            20,
            1.0,
        );
        assert_eq!(pixel_rgb(&brighten_then_flatten, 5, 5), [0x80, 0x80, 0x80]);
        assert_eq!(pixel_rgb(&flatten_then_brighten, 5, 5), [0xff, 0xff, 0xff]);
    }

    #[test]
    fn an_unsupported_grayscale_function_is_dropped_from_the_filter_chain() {
        let with_grayscale = single_box_buffer(
            ".box { background-color: #ff0000; filter: grayscale(1); }",
            20,
            1.0,
        );
        let without_filter = single_box_buffer(".box { background-color: #ff0000; }", 20, 1.0);
        assert_eq!(
            pixel_rgb(&with_grayscale, 5, 5),
            pixel_rgb(&without_filter, 5, 5)
        );
    }

    #[test]
    fn filter_opacity_and_transform_all_apply_together() {
        // filter must act on the box's own unfaded, untransformed
        // content: contrast(0) flattens it to mid-gray, *then* opacity
        // fades that gray, *then* translate moves the whole faded result.
        let buffer = single_box_buffer(
            ".box {
                background-color: #ff0000;
                filter: contrast(0);
                opacity: 0.5;
                transform: translate(15px, 0);
            }",
            30,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [0, 0, 0],
            "the box's own untransformed spot must be untouched"
        );
        assert_eq!(
            pixel_rgb(&buffer, 20, 5),
            [0x40, 0x40, 0x40],
            "mid-gray (0x80) faded to half opacity against black"
        );
    }

    fn backdrop_over_red_buffer(backdrop_css: &str) -> Canvas {
        let tree: Element = view! {
            <div class="back">
                <div class="glass"></div>
            </div>
        };
        let css = format!(".back {{ background-color: #ff0000; }} .glass {{ {backdrop_css} }}");
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(&css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let back = arena.roots()[0];
        let glass = arena.children(back)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            back,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );
        layouts.insert(
            glass,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            40,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        )
    }

    #[test]
    fn backdrop_filter_stops_at_the_rounded_corner() {
        let buffer =
            backdrop_over_red_buffer("backdrop-filter: brightness(0.5); border-radius: 10px;");
        assert_eq!(pixel_rgb(&buffer, 10, 10), [0x80, 0, 0], "inside is dimmed");
        assert_eq!(
            pixel_rgb(&buffer, 1, 1),
            [0xff, 0, 0],
            "the cut corner keeps the undimmed backdrop"
        );
    }

    #[test]
    fn backdrop_filter_darkens_what_is_already_painted_behind_it() {
        let buffer = backdrop_over_red_buffer("backdrop-filter: brightness(0.5);");
        assert_eq!(
            pixel_rgb(&buffer, 5, 5),
            [0x80, 0, 0],
            "inside the glass, the red behind it must be dimmed"
        );
        assert_eq!(
            pixel_rgb(&buffer, 30, 30),
            [0xff, 0, 0],
            "outside the glass, the background is untouched"
        );
    }

    #[test]
    fn an_empty_backdrop_filter_paints_nothing_extra() {
        let buffer = backdrop_over_red_buffer("");
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0xff, 0, 0]);
    }

    // --- Bounded group surfaces (`group_extent`/`Surface`) ---

    #[test]
    fn a_typical_small_box_with_opacity_actually_gets_a_bounded_group_surface() {
        // The whole point of this change: prove it's not just correct,
        // but that a normal small animated box actually takes the
        // bounded path in practice, not a silent fallback that would
        // leave the original canvas-sized-allocation cost unchanged
        // while still passing every pixel-correctness test below.
        assert_every_group_was_bounded(|| {
            let _ = single_box_buffer(
                ".box { background-color: #ff0000; opacity: 0.5; }",
                200,
                1.0,
            );
        });
    }

    #[test]
    fn group_extent_classifies_why_it_fell_back_to_the_full_target() {
        // AreaReachesTarget: a box that exactly fills its own canvas has
        // nothing to gain from a smaller surface.
        take_group_sizing_log();
        let _ = single_box_buffer(".box { background-color: #ff0000; opacity: 0.5; }", 10, 1.0);
        let log = take_group_sizing_log();
        assert!(
            matches!(
                log.as_slice(),
                [GroupSizing::FullTarget(FallbackReason::AreaReachesTarget)]
            ),
            "expected a single AreaReachesTarget fallback, got {log:?}"
        );

        // NonFinite: a `scale(0)` transform has no inverse.
        take_group_sizing_log();
        let _ = single_box_buffer_at(
            ".box { background-color: #ff0000; transform: scale(0); }",
            40,
            1.0,
            10.0,
            10.0,
        );
        let log = take_group_sizing_log();
        assert!(
            matches!(
                log.as_slice(),
                [GroupSizing::FullTarget(FallbackReason::NonFinite)]
            ),
            "expected a single NonFinite fallback, got {log:?}"
        );

        // NothingToPaint: `subtree_extent` bails out entirely when `node`
        // has no entry in `layouts` at all (the one case that isn't just
        // an empty box) — `opacity` alone is enough to route it into
        // `paint_group` even with no layout to size a border box from.
        let tree: Element = view! { <div class="box" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(".box { opacity: 0.5; }").unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let layouts = HashMap::new();
        let mut font = Font::load_embedded();
        take_group_sizing_log();
        let _ = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        let log = take_group_sizing_log();
        assert!(
            matches!(
                log.as_slice(),
                [GroupSizing::FullTarget(FallbackReason::NothingToPaint)]
            ),
            "expected a single NothingToPaint fallback, got {log:?}"
        );
    }

    #[test]
    fn a_rotated_box_at_a_non_zero_position_paints_identically_bounded_or_target_sized() {
        // The exact case a naive port breaks: `rotate(90deg)` about a
        // non-zero position composites through a real matrix, not just a
        // translate, so folding the target's own origin into
        // `draw_pixmap`'s `(x, y)` instead of into the transform itself
        // (the two "double-shift" traps this crate's own `paint_group`
        // doc warns about) would only show up here, never for a
        // translate-only transform.
        assert_bounded_and_full_target_paint_identically(|| {
            single_box_buffer_at(
                ".box { background-color: #ff0000; transform: rotate(90deg); }",
                60,
                1.0,
                15.0,
                20.0,
            )
        });
    }

    #[test]
    fn a_scaled_box_at_a_non_zero_position_paints_identically_bounded_or_target_sized() {
        assert_bounded_and_full_target_paint_identically(|| {
            single_box_buffer_at(
                ".box { background-color: #ff0000; transform: scale(2.5); transform-origin: 20% 80%; }",
                80,
                1.0,
                25.0,
                10.0,
            )
        });
    }

    #[test]
    fn a_blurred_filter_paints_identically_bounded_or_target_sized() {
        assert_bounded_and_full_target_paint_identically(|| {
            single_box_buffer_at(
                ".box { background-color: #ff0000; filter: blur(6px); }",
                80,
                1.0,
                30.0,
                30.0,
            )
        });
    }

    #[test]
    fn a_box_shadow_inside_a_group_paints_identically_bounded_or_target_sized() {
        assert_bounded_and_full_target_paint_identically(|| {
            single_box_buffer_at(
                ".box {
                    background-color: #ff0000;
                    opacity: 0.5;
                    box-shadow: 4px 4px 6px 2px #000000;
                }",
                80,
                1.0,
                25.0,
                25.0,
            )
        });
    }

    #[test]
    fn a_zero_sized_box_with_a_transform_does_not_panic() {
        // Used to panic: a 0x0 canvas-sized group buffer failed
        // `Pixmap::new(0, 0).expect(..)`. `group_extent` now sizes the
        // group to the (empty) content itself, and `paint_group` treats
        // `GroupSizing::Empty` as "paint nothing," not an error.
        let tree: Element = view! { <div class="box" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(".box { transform: rotate(45deg); }").unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let node = arena.roots()[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            node,
            BoxLayout {
                x: 5.0,
                y: 5.0,
                width: 0.0,
                height: 0.0,
            },
        );
        let mut font = Font::load_embedded();
        // Must not panic — the whole point of the test.
        let _ = paint_to_buffer(
            &mut font,
            20,
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
    }

    #[test]
    fn nested_groups_composite_at_the_right_position_and_combined_opacity() {
        // Outer: opacity 0.5, translated (10, 0). Inner: opacity 0.5,
        // translated a further (5, 5) within the outer's own box. A
        // wrong nested-target composite (the same double-shift risk as
        // the rotation test above, here across two bounded surfaces
        // instead of one) would land the inner box at the wrong
        // position, not just the wrong opacity.
        let tree: Element = view! {
            <div class="outer">
                <div class="inner" />
            </div>
        };
        let css = "
            .outer { width: 10px; height: 10px; opacity: 0.5; transform: translate(10px, 0); }
            .inner { width: 10px; height: 10px; background-color: #ff0000; opacity: 0.5; transform: translate(5px, 5px); }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let outer = arena.roots()[0];
        let inner = arena.children(outer)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            outer,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        );
        layouts.insert(
            inner,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
        );
        let mut font = Font::load_embedded();
        let mut paint = || {
            paint_to_buffer(
                &mut font,
                40,
                40,
                Rgba::opaque(0, 0, 0),
                &arena,
                &styles,
                &layouts,
                1.0,
            )
        };
        let buffer = paint();

        // Outer box's own untransformed spot (0,0)-(10,10): untouched.
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0, 0, 0]);
        // Inner box lands at outer's translate (10, 0) plus its own
        // (5, 5) = (15, 5)-(25, 15); combined opacity 0.5 * 0.5 = 0.25
        // against the canvas's own black.
        assert_eq!(pixel_rgb(&buffer, 20, 10), [0x40, 0, 0]);

        FORCE_FULL_TARGET_GROUPS.with(|flag| flag.set(false));
        assert_bounded_and_full_target_paint_identically(paint);
    }

    #[test]
    fn overflow_hidden_clips_an_opacity_child_even_through_a_bounded_group_surface() {
        // The single most important new test for this change: a naive
        // port that keeps passing canvas-sized clip masks into a smaller
        // bounded surface hits tiny-skia's own silent size-mismatch
        // no-op (`RasterPipelineBlitter::new` returns `None`, logged,
        // never panics) — every fill inside the frame would silently
        // paint nothing, and this test is what would actually notice.
        let tree: Element = view! {
            <div class="frame">
                <div class="content" />
            </div>
        };
        let css = "
            .frame { width: 20px; height: 20px; overflow: hidden; }
            .content { background-color: #ff0000; opacity: 0.5; }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let frame = arena.roots()[0];
        let content = arena.children(frame)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            frame,
            BoxLayout {
                x: 10.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
            },
        );
        // Spills past the frame on every side, same as the existing
        // non-group clip test — this time the child itself starts a
        // group (`opacity: 0.5`), so `paint_group`'s own bounded surface
        // is what needs to still respect the frame's clip.
        layouts.insert(
            content,
            BoxLayout {
                x: -10.0,
                y: -10.0,
                width: 40.0,
                height: 40.0,
            },
        );
        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            60,
            60,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // Inside the frame: the faded red content, not black.
        assert_eq!(pixel_rgb(&buffer, 15, 15), [0x80, 0, 0]);
        // Outside the frame, where the content would otherwise spill:
        // the canvas's own black, not red — proof the clip actually
        // held.
        assert_eq!(pixel_rgb(&buffer, 2, 2), [0, 0, 0]);
        assert_eq!(pixel_rgb(&buffer, 45, 45), [0, 0, 0]);
    }

    #[test]
    fn a_backdrop_filter_inside_a_translated_group_still_samples_the_right_pixels() {
        let tree: Element = view! {
            <div class="group">
                <div class="back">
                    <div class="glass" />
                </div>
            </div>
        };
        let css = "
            .group { opacity: 0.99; transform: translate(20px, 20px); }
            .back { width: 40px; height: 40px; background-color: #ff0000; }
            .glass { width: 20px; height: 20px; backdrop-filter: brightness(0.5); }
        ";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let group = arena.roots()[0];
        let back = arena.children(group)[0];
        let glass = arena.children(back)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            group,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );
        layouts.insert(
            back,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
            },
        );
        layouts.insert(
            glass,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
            },
        );
        let mut font = Font::load_embedded();
        let buffer = paint_to_buffer(
            &mut font,
            80,
            80,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );

        // Inside the glass (translated to (20,20)-(40,40)): dimmed red,
        // off by one LSB from a plain 0.5 brightness() (0x80) because
        // `opacity: 0.99` (needed to keep this a real group) also
        // shaves a fraction off every channel: 0x80 * 0.99 rounds down
        // to 0x7f.
        assert_eq!(pixel_rgb(&buffer, 30, 30), [0x7f, 0, 0]);
        // Inside the group but outside the glass: red, faded by the
        // same 0.99 group opacity (0xff * 0.99 rounds down to 0xfc).
        assert_eq!(pixel_rgb(&buffer, 55, 55), [0xfc, 0, 0]);
    }
}
