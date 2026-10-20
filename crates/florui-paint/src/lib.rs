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

mod background;
mod banded;
mod blend;
mod blur;
mod border_image;
mod border_style;
mod decoration;
mod glyph_cache;
mod gradient;
mod image_source;
mod incremental;
mod layer_cache;
mod mask;
mod material;
mod projection;
mod rounded;
mod scene3d;
mod shadow_cache;
mod svg_ref;
mod text_shadow;

pub use incremental::{PaintInputs, Repaint, Repainter};

/// The boxes that reach a band of a frame being painted in bands.
pub(crate) struct BandView {
    /// Boxes whose own ink reaches the band: only these are painted.
    pub(crate) own: HashSet<NodeId>,
    /// Boxes whose ink, or the ink of anything under them, reaches the band: a box
    /// that is not here, and everything under it, is passed by without a look.
    pub(crate) subtree: HashSet<NodeId>,
}

thread_local! {
    /// The view of the band this thread is painting, if it is painting one.
    static BAND_VIEW: RefCell<Option<Rc<BandView>>> = const { RefCell::new(None) };
}

use florui_style::StyleMap;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use florui_layout::{BoxLayout, absolute_position};
#[cfg(test)]
use florui_style::Arena;
use florui_style::{
    BlendMode, ComputedStyle, ContentAlignment, Display, FilterFunction, FlexDirection,
    GlassMaterial, GlassSpec, ItemAlignment, LengthPercentage, NodeId, ObjectFit, Position, Rgba,
    RoundedRect, Tree,
};
use florui_text::Font;
use projection::Projection;
use rounded::RoundedRectPath;
use skrifa::instance::{LocationRef, NormalizedCoord, Size as GlyphSize};
use skrifa::outline::DrawSettings;
#[cfg(test)]
use skrifa::outline::OutlinePen;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use tiny_skia::{
    FillRule, FilterQuality, IntRect, Mask, Paint, PathBuilder, Pixmap, PixmapPaint,
    PremultipliedColorU8, Rect, Transform,
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
fn is_control_icon_node(arena: &Tree, node: NodeId) -> bool {
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
    arena: &Tree,
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
        margin: florui_layout::BoxMargin::ZERO,
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
    /// The mask currently in use and the pixels (`x0, y0, x1, y1`, local) where
    /// it is fully opaque, set by [`paint_nodes`] for a rectangular clip. A
    /// fill that lies wholly inside those pixels needs no mask, and drawing
    /// without one is what keeps tiny-skia on its fast path for a solid color.
    solid_clip: Cell<Option<(usize, [i32; 4])>>,
}

impl Surface {
    fn root(pixmap: Pixmap) -> Self {
        Self {
            pixmap,
            origin: (0, 0),
            solid_clip: Cell::new(None),
        }
    }

    fn new(width: u32, height: u32, origin: (i32, i32)) -> Option<Self> {
        Pixmap::new(width, height).map(|pixmap| Self {
            pixmap,
            origin,
            solid_clip: Cell::new(None),
        })
    }

    /// `clip`, or `None` when `clip` is the mask [`Self::solid_clip`] describes and
    /// `bounds` (`x0, y0, x1, y1`, local pixels) lies wholly inside its opaque
    /// pixels, where the mask would change nothing. Any other mask is returned
    /// unchanged.
    fn mask_for<'a>(
        &self,
        clip: Option<&'a Mask>,
        bounds: (f32, f32, f32, f32),
    ) -> Option<&'a Mask> {
        let mask = clip?;
        #[cfg(test)]
        if SOLID_CLIP_OFF.with(Cell::get) {
            return Some(mask);
        }
        if let Some((identity, [x0, y0, x1, y1])) = self.solid_clip.get()
            && identity == std::ptr::from_ref(mask) as usize
            && bounds.0.floor() >= x0 as f32
            && bounds.1.floor() >= y0 as f32
            && bounds.2.ceil() <= x1 as f32
            && bounds.3.ceil() <= y1 as f32
        {
            #[cfg(test)]
            SOLID_CLIP_DROPS.with(|drops| drops.set(drops.get() + 1));
            return None;
        }
        Some(mask)
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

    /// The pixels of `surface` (`x0, y0, x1, y1`, local) that this clip's mask
    /// covers completely, or `None` when there are none or the clip has a
    /// rounded outline (whose corners the rectangle would overstate). Pixels
    /// only partly inside a fractional edge are left out.
    fn solid_interior(&self, surface: &Surface) -> Option<[i32; 4]> {
        if self.rounded.is_some() || self.is_empty() {
            return None;
        }
        let (lx0, ly0) = surface.local(self.x0, self.y0);
        let (lx1, ly1) = surface.local(self.x1, self.y1);
        let x0 = lx0.max(0.0).ceil() as i32;
        let y0 = ly0.max(0.0).ceil() as i32;
        let x1 = lx1.min(surface.width() as f32).floor() as i32;
        let y1 = ly1.min(surface.height() as f32).floor() as i32;
        (x0 < x1 && y0 < y1).then_some([x0, y0, x1, y1])
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
    arena: &Tree,
    styles: &StyleMap,
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
    /// `Some` while a number input shows its spinner (hovered or focused),
    /// with which arrow the pointer is over.
    pub spinner: Option<SpinnerHover>,
    /// How far the text is scrolled inside the field, in logical pixels.
    pub scroll: (f32, f32),
    /// The placeholder text shaped for painting, `Some` while the field is
    /// empty and has one.
    pub placeholder: Option<Vec<florui_text::ShapedRun>>,
    /// A textarea's vertical scrollbar, `Some` while its text overflows.
    pub scrollbar: Option<ScrollbarPaint>,
    /// Whether a textarea shows its resize corner.
    pub resizer: bool,
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
    arena: &Tree,
    styles: &StyleMap,
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
    arena: &Tree,
    styles: &StyleMap,
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

/// `canvas` stretched to `width x height`, sampled smoothly: a frame painted
/// small to be shown large. Bilinear: a row of the source is blended across once
/// and then down, two channels at a time, in whole numbers.
pub fn stretched(canvas: &Canvas, width: u32, height: u32) -> Option<Canvas> {
    /// `a` and `b` (four channels each) mixed, `weight` out of 256 of `b`.
    #[inline]
    fn blend(a: u32, b: u32, weight: u32) -> u32 {
        const LANES: u32 = 0x00FF_00FF;
        let mix = |a: u32, b: u32| ((a * (256 - weight) + b * weight) >> 8) & LANES;
        mix(a & LANES, b & LANES) | (mix((a >> 8) & LANES, (b >> 8) & LANES) << 8)
    }

    let mut stretched = Pixmap::new(width, height)?;
    let (source_width, source_height) = (canvas.width() as usize, canvas.height() as usize);
    let (width, height) = (width as usize, height as usize);
    // For each column of the result, the two source columns it blends and the
    // weight of the second.
    let columns: Vec<(usize, usize, u32)> = (0..width)
        .map(|x| {
            let at = ((x as f32 + 0.5) * source_width as f32 / width as f32 - 0.5).max(0.0);
            let first = (at.floor() as usize).min(source_width - 1);
            let second = (first + 1).min(source_width - 1);
            (first, second, ((at - first as f32) * 256.0) as u32)
        })
        .collect();
    let packed: Vec<u32> = canvas
        .data()
        .chunks_exact(4)
        .map(|pixel| u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]))
        .collect();
    // A source row blended across to the result's width, kept for the rows
    // below that need it again.
    let across = |row: usize, out: &mut Vec<u32>| {
        out.clear();
        let line = &packed[row * source_width..(row + 1) * source_width];
        out.extend(
            columns
                .iter()
                .map(|&(first, second, weight)| blend(line[first], line[second], weight)),
        );
    };
    let mut upper = Vec::with_capacity(width);
    let mut lower = Vec::with_capacity(width);
    let (mut upper_row, mut lower_row) = (usize::MAX, usize::MAX);
    let out = stretched.data_mut();
    for y in 0..height {
        let at = ((y as f32 + 0.5) * source_height as f32 / height as f32 - 0.5).max(0.0);
        let first = (at.floor() as usize).min(source_height - 1);
        let second = (first + 1).min(source_height - 1);
        let weight = ((at - first as f32) * 256.0) as u32;
        if upper_row != first {
            if lower_row == first {
                std::mem::swap(&mut upper, &mut lower);
                lower_row = upper_row;
            } else {
                across(first, &mut upper);
            }
            upper_row = first;
        }
        if lower_row != second {
            across(second, &mut lower);
            lower_row = second;
        }
        let line = &mut out[y * width * 4..(y + 1) * width * 4];
        for ((pixel, &a), &b) in line.chunks_exact_mut(4).zip(&upper).zip(&lower) {
            pixel.copy_from_slice(&blend(a, b, weight).to_le_bytes());
        }
    }
    Some(stretched)
}

/// [`paint_to_buffer_with_desktop_extras`] with a big frame painted in bands, at
/// once, on the thread pool. A band is painted on a surface of its own with room
/// round it that is thrown away, so the picture is the whole one's to within a
/// few levels of a color near the edges of the bands; a small frame, or a
/// machine with one thread, is painted in one piece as before.
#[allow(clippy::too_many_arguments)]
pub fn paint_to_buffer_banded(
    font: &mut Font,
    width: u32,
    height: u32,
    canvas: Rgba,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> Canvas {
    if banded::worth_it(width, height) {
        banded::paint(
            font,
            width,
            height,
            canvas,
            arena,
            styles,
            layouts,
            scale_factor,
            text_inputs,
            images,
        )
    } else {
        paint_to_buffer_with_desktop_extras(
            font,
            width,
            height,
            canvas,
            arena,
            styles,
            layouts,
            scale_factor,
            text_inputs,
            images,
        )
    }
}

/// A border in each side's own style, on a square or rounded box.
fn paint_box_border(
    buffer: &mut Surface,
    outline: &RoundedRect,
    border: florui_style::Edges<florui_style::BorderSide>,
    clip: Option<&Mask>,
) {
    if !border_style::is_plain(&border) {
        border_style::paint(buffer, outline, border, clip);
    } else if outline.is_square() {
        paint_border(
            buffer,
            outline.x,
            outline.y,
            outline.width,
            outline.height,
            border,
            clip,
        );
    } else {
        paint_rounded_border(buffer, outline, border, clip);
    }
}

/// How far what a style draws outside its border box reaches: its outline, and
/// a border image's outset.
fn ink_reach(style: &ComputedStyle, scale: f32) -> f32 {
    let image = style.border_image.as_deref().map_or(0.0, |image| {
        border_image::reach(image, &scaled_border(style.border, scale), scale)
    });
    border_style::outline_reach(style, scale).max(image)
}

/// Draws `node`'s outline, over everything inside it.
fn paint_outline_of(
    buffer: &mut Surface,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let (Some(style), Some(&layout)) = (styles.get(&node), layouts.get(&node)) else {
        return;
    };
    if !border_style::has_outline(style) {
        return;
    }
    let (x, y) = absolute_position(arena, layouts, node);
    let border_box = RoundedRect::new(
        x,
        y,
        layout.width,
        layout.height,
        &style.border_radius,
        scale_factor,
    );
    border_style::paint_outline(buffer, &style.outline, &border_box, scale_factor, clip);
}

/// Why a node is on the paint stack: its first visit, its turn among the boxes
/// fixed to the viewport, or the outline it draws over its subtree.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Turn {
    First,
    Fixed,
    Outline,
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
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    parent_display: Option<Display>,
    nodes: &[NodeId],
    scale_factor: f32,
    clip: Option<ClipRect>,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    // The flag is set for a box fixed to the viewport once its turn has come.
    let mut stack: Vec<(NodeId, Option<ClipRect>, Turn)> =
        paint_order(styles, parent_display, nodes)
            .into_iter()
            .rev()
            .map(|id| (id, clip.clone(), Turn::First))
            .collect();
    // Boxes fixed to the viewport go over everything else in their scope, in
    // `z-index` order, and nothing above them clips them.
    let mut fixed: Vec<NodeId> = Vec::new();
    let mut mask_cache: Option<(ClipRect, Option<Mask>)> = None;
    // Nothing outside the surface can appear, whatever clips the tree has.
    let surface_rect = ClipRect::from_xywh(
        buffer.origin.0 as f32,
        buffer.origin.1 as f32,
        buffer.width() as f32,
        buffer.height() as f32,
    );
    let view = BAND_VIEW.with(|slot| slot.borrow().clone());
    loop {
        let Some((node, node_clip, turn)) = stack.pop() else {
            if fixed.is_empty() {
                break;
            }
            // Node ids follow document order.
            fixed.sort_by_key(|id| (styles.get(id).and_then(|s| s.z_index).unwrap_or(0), *id));
            stack.extend(fixed.drain(..).rev().map(|id| (id, None, Turn::Fixed)));
            continue;
        };
        if turn == Turn::First && florui_layout::is_viewport_fixed(arena, styles, node) {
            fixed.push(node);
            continue;
        }
        if turn == Turn::Outline {
            let in_view = view.as_ref().is_none_or(|view| view.own.contains(&node));
            let mask = node_clip.as_ref().and_then(|clip| clip.to_mask(buffer));
            if in_view && (node_clip.is_none() || mask.is_some()) {
                paint_outline_of(
                    buffer,
                    arena,
                    styles,
                    layouts,
                    node,
                    scale_factor,
                    mask.as_ref(),
                );
            }
            continue;
        }
        if view
            .as_ref()
            .is_some_and(|view| !view.subtree.contains(&node))
        {
            continue;
        }
        if styles
            .get(&node)
            .is_some_and(|s| s.display == florui_style::Display::None)
        {
            continue;
        }
        if florui_layout::is_flowed_inline(arena, styles, node) {
            // An inline element flowing in a context's lines is drawn by that
            // context (its box behind the text, then the text); only the
            // atomic boxes inside it are walked.
            stack.extend(
                paint_order(styles, Some(Display::Inline), arena.children(node))
                    .into_iter()
                    .rev()
                    .map(|id| (id, node_clip.clone(), Turn::First)),
            );
            continue;
        }
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
            // A box fixed to the viewport is not cut by it, though.
            let mut below = arena.children(node).to_vec();
            while let Some(descendant) = below.pop() {
                if florui_layout::is_viewport_fixed(arena, styles, descendant) {
                    fixed.push(descendant);
                } else {
                    below.extend_from_slice(arena.children(descendant));
                }
            }
            continue;
        }
        if styles.get(&node).is_some_and(florui_layout::preserves_3d) {
            scene3d::paint_scene(
                buffer,
                arena,
                styles,
                layouts,
                font,
                node,
                scale_factor,
                node_clip,
                text_inputs,
                images,
            );
            continue;
        }
        let node_abs = absolute_position(arena, layouts, node);
        let transform = resolve_projection(arena, styles, layouts, node, node_abs, scale_factor);
        if backface_is_hidden(styles, layouts, node, node_abs, &transform) {
            continue;
        }
        let has_effect = blend::isolates(arena, styles, node)
            || styles
                .get(&node)
                .is_some_and(ComputedStyle::has_group_effect);
        if opacity < 1.0 || !transform.is_identity() || has_effect {
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
        // Siblings and their descendants mostly share one clip; rasterizing a
        // surface-sized mask for each of them dominates a clipped scroll box.
        if let Some(clip) = node_clip.as_ref()
            && mask_cache.as_ref().is_none_or(|(cached, _)| cached != clip)
        {
            mask_cache = Some((clip.clone(), clip.to_mask(buffer)));
            let solid = match (&mask_cache, clip.solid_interior(buffer)) {
                (Some((_, Some(mask))), Some(interior)) => {
                    Some((std::ptr::from_ref(mask) as usize, interior))
                }
                _ => None,
            };
            buffer.solid_clip.set(solid);
        }
        let node_mask = match (&node_clip, &mask_cache) {
            (Some(_), Some((_, mask))) => mask.as_ref(),
            _ => None,
        };
        let invisible = match &view {
            Some(view) => !view.own.contains(&node),
            None => ink_is_outside_clip(
                arena,
                styles,
                layouts,
                font,
                node,
                node_abs,
                [Some(&surface_rect), node_clip.as_ref()],
                scale_factor,
            ),
        };
        // A clip wholly outside this surface (a band or a part of the frame that
        // lies elsewhere) has no mask, and no mask must not mean no clip.
        let invisible = invisible || (node_clip.is_some() && node_mask.is_none());
        if !invisible {
            paint_node(
                buffer,
                arena,
                styles,
                layouts,
                font,
                node,
                scale_factor,
                node_mask,
                text_inputs,
                images,
            );
        }
        if styles.get(&node).is_some_and(border_style::has_outline) {
            stack.push((node, node_clip.clone(), Turn::Outline));
        }
        let child_clip = clip_for_children(arena, styles, layouts, node, node_clip, scale_factor);
        let child_display = styles.get(&node).map(|s| s.display);
        stack.extend(
            paint_order(styles, child_display, arena.children(node))
                .into_iter()
                .rev()
                .map(|id| (id, child_clip.clone(), Turn::First)),
        );
    }
    // The mask this described goes away with `mask_cache`.
    buffer.solid_clip.set(None);
}

#[cfg(test)]
thread_local! {
    /// Lets a test paint the same tree without the off-screen shortcut, as the
    /// reference the shortcut must match pixel for pixel.
    static CULLING_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SOLID_CLIP_OFF: Cell<bool> = const { Cell::new(false) };
    static SOLID_CLIP_DROPS: Cell<u32> = const { Cell::new(0) };
}

/// Whether everything [`paint_node`] would draw for `node` lies outside at
/// least one of `clips` (the surface being painted and the node's clip, if
/// any), so painting it can be skipped. Conservative: when in doubt it says no.
///
/// What a node draws is its border box, its box shadows (which reach past the
/// box by their offset, spread and blur) and its own text, which can overflow
/// the box when the box is smaller than the text. The text's extent comes from
/// the metrics memo, which the layout pass already filled for the same text
/// and width. Images, control decorations and mixed inline content are never
/// skipped; their extent is not known without more work than painting them.
#[allow(clippy::too_many_arguments)]
fn ink_is_outside_clip(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    (x, y): (f32, f32),
    clips: [Option<&ClipRect>; 2],
    scale_factor: f32,
) -> bool {
    #[cfg(test)]
    if CULLING_OFF.with(|off| off.get()) {
        return false;
    }
    let Some(&layout) = layouts.get(&node) else {
        return false;
    };
    let style = styles.get(&node);
    let outside = |(x0, y0, x1, y1): (f32, f32, f32, f32)| {
        clips
            .iter()
            .flatten()
            .any(|clip| x1 <= clip.x0 || x0 >= clip.x1 || y1 <= clip.y0 || y0 >= clip.y1)
    };

    let reach = style.map_or(0.0, |s| {
        s.box_shadow
            .iter()
            .map(|shadow| {
                shadow.offset_x.abs().max(shadow.offset_y.abs())
                    + shadow.spread_radius.abs()
                    + shadow.blur_radius * 2.0
            })
            .fold(0.0, f32::max)
    }) * scale_factor.max(1.0);
    let reach = reach.max(style.map_or(0.0, |s| ink_reach(s, scale_factor)));
    let ink = (
        x - reach,
        y - reach,
        x + layout.width + reach,
        y + layout.height + reach,
    );
    if !outside(ink) {
        return false;
    }

    let tag = arena.tag(node);
    if matches!(tag, "img" | "icon") || is_control_icon_node(arena, node) {
        return false;
    }
    // A field cuts its own text at its padding box.
    if matches!(tag, "input" | "textarea") {
        return true;
    }
    // An element holding only text counts as an inline formatting context,
    // but shapes that one text in the container's own font, exactly as the
    // plain path does. Anything mixed with other elements is left alone.
    if !florui_layout::anonymous_blocks(arena, styles, node).is_empty() {
        return false;
    }
    let text = if florui_layout::is_inline_formatting_context(arena, styles, node) {
        match arena.inline_items(node) {
            [florui_style::InlineItem::Text(text)] => text.as_str(),
            _ => return false,
        }
    } else {
        arena.text_content(node)
    };
    if text.is_empty() {
        return true;
    }

    let border = scaled_border(style.map_or(NO_BORDER, |s| s.border), scale_factor);
    let padding = scaled_padding(
        style.map_or(
            florui_style::Edges {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
            |s| s.padding,
        ),
        scale_factor,
    );
    let content_x = x + border.left.width + padding.left;
    let content_y = y + border.top.width + padding.top;
    let content_width =
        (layout.width - border.left.width - border.right.width - padding.left - padding.right)
            .max(0.0);
    let font_size = style.map_or(16.0, |s| s.font_size);
    let metrics = font.probe_memoized(
        florui_layout::node_text_style(style),
        text,
        Some(wrap_width(content_width, scale_factor)),
    );
    // A glyph can overhang its advance box, and right-to-left lines start at
    // the far edge; one em of slack on every side, and the wider of the
    // shaped and wrapping widths either way, covers both.
    let em = font_size * scale_factor;
    let span = metrics.width.max(content_width / scale_factor) * scale_factor;
    outside((
        content_x - span - em,
        content_y - em,
        content_x + span + em,
        content_y + metrics.height * scale_factor + em,
    ))
}

/// A mask of `node`'s padding box (its border box less the border),
/// combined with an `incoming` mask, or `None` when nothing is left to
/// paint.
fn padding_box_mask(
    buffer: &Surface,
    (x, y, width, height): (f32, f32, f32, f32),
    border: florui_style::Edges<florui_style::BorderSide>,
    incoming: Option<&Mask>,
) -> Option<Mask> {
    let mut mask = ClipRect::from_xywh(
        x + border.left.width,
        y + border.top.width,
        (width - border.left.width - border.right.width).max(0.0),
        (height - border.top.width - border.bottom.width).max(0.0),
    )
    .to_mask(buffer)?;
    if let Some(incoming) = incoming {
        for (own, outer) in mask.data_mut().iter_mut().zip(incoming.data()) {
            *own = ((*own as u16 * *outer as u16) / 255) as u8;
        }
    }
    Some(mask)
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
    arena: &Tree,
    styles: &StyleMap,
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
    let border = scaled_border(
        styles.get(&node).map_or(NO_BORDER, |s| s.border),
        scale_factor,
    );
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

/// How much wider than the box a line of text may be before it wraps, in
/// logical pixels. A box sized to its text is exactly as wide as the text, and
/// scaling that width to painted pixels and back by another factor can come
/// out a hair short, wrapping the last word.
const WRAP_SLACK: f32 = 0.05;

/// The width, in logical pixels, text is wrapped at in a box whose content is
/// `content_width` painted pixels wide. At a scale of one the division is exact,
/// and the width stays the one layout measured, which keys the text caches.
fn wrap_width(content_width: f32, scale_factor: f32) -> f32 {
    if scale_factor == 1.0 {
        content_width
    } else {
        content_width / scale_factor + WRAP_SLACK
    }
}

/// [`wrap_width`] where a local of the same name shadows it.
fn self_wrap(content_width: f32, scale_factor: f32) -> f32 {
    wrap_width(content_width, scale_factor)
}

/// `border` with its widths in painted pixels: they are authored in logical
/// pixels, like every length, and the boxes they sit in are already scaled.
fn scaled_border(
    mut border: florui_style::Edges<florui_style::BorderSide>,
    scale_factor: f32,
) -> florui_style::Edges<florui_style::BorderSide> {
    for side in [
        &mut border.top,
        &mut border.right,
        &mut border.bottom,
        &mut border.left,
    ] {
        side.width *= scale_factor;
    }
    border
}

/// `padding` in painted pixels, as [`scaled_border`] does for a border.
fn scaled_padding(
    padding: florui_style::Edges<f32>,
    scale_factor: f32,
) -> florui_style::Edges<f32> {
    florui_style::Edges {
        top: padding.top * scale_factor,
        right: padding.right * scale_factor,
        bottom: padding.bottom * scale_factor,
        left: padding.left * scale_factor,
    }
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
    style: florui_style::BorderLineStyle::Solid,
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

/// Where the pointer is over a number input's spinner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerHover {
    None,
    Up,
    Down,
}

// Spinner geometry, measured in Edge on a 200x30 number input: two 7x5
// arrows centred 11.5px in from the right edge, the up arrow 3.5px above
// the input's vertical centre and the down arrow 5.5px below it. The
// pointer targets are the two halves of a 15x18 box around them (up arrow
// rows 6..15, down arrow rows 15..24 of the 30px field).
const SPINNER_RIGHT_INSET: f32 = 4.0;
const SPINNER_WIDTH: f32 = 15.0;
const SPINNER_HEIGHT: f32 = 18.0;
const SPINNER_IDLE: Rgba = Rgba::opaque(139, 139, 139);
const SPINNER_HOVER: Rgba = Rgba::opaque(99, 99, 99);

/// Edge's default placeholder color, `rgb(117, 117, 117)`, whatever the
/// field's own text color is.
const PLACEHOLDER_COLOR: Rgba = Rgba::opaque(117, 117, 117);

/// Width of a textarea's vertical scrollbar, and of the resize corner
/// that shortens it.
pub const SCROLLBAR_WIDTH: f32 = 15.0;
const SCROLLBAR_BUTTON: f32 = 18.0;
const SCROLLBAR_MIN_THUMB: f32 = 17.0;
const SCROLLBAR_THUMB_WIDTH: f32 = 9.0;
const SCROLLBAR_TRACK: Rgba = Rgba::opaque(252, 252, 252);
const SCROLLBAR_IDLE: Rgba = Rgba::opaque(139, 139, 139);
const SCROLLBAR_ACTIVE: Rgba = Rgba::opaque(99, 99, 99);
const RESIZER_COLOR: Rgba = Rgba::opaque(101, 101, 101);

/// The part of a textarea's scrollbar a point is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarPart {
    None,
    ButtonUp,
    ButtonDown,
    Thumb,
    TrackBefore,
    TrackAfter,
}

/// A textarea's scrollbar as painted: how far it is scrolled and how far it
/// can go, the visible height, and the part to draw in its active color
/// (under the pointer, or being dragged).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarPaint {
    pub scroll: f32,
    pub max_scroll: f32,
    pub viewport: f32,
    pub active: ScrollbarPart,
}

/// Where a textarea's scrollbar parts are, in logical pixels relative to
/// its border box, measured in Edge: 15px wide against the right border,
/// two 18px buttons, and a thumb 9px wide, at least 17px long, riding the
/// track between them. A resize corner takes the last 15px of height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollbarGeometry {
    pub up: (f32, f32, f32, f32),
    pub down: (f32, f32, f32, f32),
    pub thumb: (f32, f32, f32, f32),
    pub track_top: f32,
    pub track_bottom: f32,
    pub bar_x: f32,
}

pub fn scrollbar_geometry(
    (width, height): (f32, f32),
    border: (f32, f32, f32, f32),
    resizer: bool,
    scrollbar: ScrollbarPaint,
) -> ScrollbarGeometry {
    let (border_left, border_top, border_right, border_bottom) = border;
    let _ = border_left;
    let x = width - border_right - SCROLLBAR_WIDTH;
    let bar_height =
        height - border_top - border_bottom - if resizer { SCROLLBAR_WIDTH } else { 0.0 };
    let top = border_top;
    let track_top = top + SCROLLBAR_BUTTON;
    let track_bottom = top + bar_height - SCROLLBAR_BUTTON;
    let track_len = (track_bottom - track_top).max(0.0);
    let total = (scrollbar.viewport + scrollbar.max_scroll).max(1.0);
    let thumb_len = (track_len * scrollbar.viewport / total)
        .max(SCROLLBAR_MIN_THUMB)
        .min(track_len);
    let fraction = if scrollbar.max_scroll > 0.0 {
        (scrollbar.scroll / scrollbar.max_scroll).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let thumb_top = track_top + ((track_len - thumb_len) * fraction).round();
    ScrollbarGeometry {
        up: (x, top, SCROLLBAR_WIDTH, SCROLLBAR_BUTTON),
        down: (x, track_bottom, SCROLLBAR_WIDTH, SCROLLBAR_BUTTON),
        thumb: (
            x + (SCROLLBAR_WIDTH - SCROLLBAR_THUMB_WIDTH) / 2.0,
            thumb_top,
            SCROLLBAR_THUMB_WIDTH,
            thumb_len,
        ),
        track_top,
        track_bottom,
        bar_x: x,
    }
}

/// The scrollbar part under `point` (relative to the border box), or
/// [`ScrollbarPart::None`] outside it.
pub fn scrollbar_part_at(geometry: &ScrollbarGeometry, point: (f32, f32)) -> ScrollbarPart {
    let inside = |(x, y, w, h): (f32, f32, f32, f32)| {
        point.0 >= x && point.0 < x + w && point.1 >= y && point.1 < y + h
    };
    if inside(geometry.up) {
        ScrollbarPart::ButtonUp
    } else if inside(geometry.down) {
        ScrollbarPart::ButtonDown
    } else if point.0 >= geometry.bar_x
        && point.0 < geometry.bar_x + SCROLLBAR_WIDTH
        && point.1 >= geometry.track_top
        && point.1 < geometry.track_bottom
    {
        let (_, thumb_top, _, thumb_len) = geometry.thumb;
        if point.1 < thumb_top {
            ScrollbarPart::TrackBefore
        } else if point.1 >= thumb_top + thumb_len {
            ScrollbarPart::TrackAfter
        } else {
            ScrollbarPart::Thumb
        }
    } else {
        ScrollbarPart::None
    }
}

/// Whether `point` (relative to the border box) is over the resize corner.
pub fn is_over_resizer(
    (width, height): (f32, f32),
    border: (f32, f32, f32, f32),
    point: (f32, f32),
) -> bool {
    let (_, _, border_right, border_bottom) = border;
    point.0 >= width - border_right - SCROLLBAR_WIDTH
        && point.0 < width - border_right
        && point.1 >= height - border_bottom - SCROLLBAR_WIDTH
        && point.1 < height - border_bottom
}
/// Which arrow of a number input's spinner the point `(px, py)` is over,
/// for an input whose border box is `(x, y, width, height)` in the same
/// space as the point.
pub fn spinner_half_at(
    (x, y, width, height): (f32, f32, f32, f32),
    (px, py): (f32, f32),
) -> SpinnerHover {
    let right = x + width - SPINNER_RIGHT_INSET;
    let mid = y + height / 2.0;
    let inside = px >= right - SPINNER_WIDTH
        && px < right
        && py >= mid - SPINNER_HEIGHT / 2.0
        && py < mid + SPINNER_HEIGHT / 2.0;
    match (inside, py < mid) {
        (false, _) => SpinnerHover::None,
        (true, true) => SpinnerHover::Up,
        (true, false) => SpinnerHover::Down,
    }
}

/// Fills the triangle `points` (absolute canvas pixels) in `color`.
fn fill_triangle(buffer: &mut Surface, points: [(f32, f32); 3], color: Rgba, clip: Option<&Mask>) {
    let mut path = PathBuilder::new();
    path.move_to(points[0].0, points[0].1);
    path.line_to(points[1].0, points[1].1);
    path.line_to(points[2].0, points[2].1);
    path.close();
    let Some(path) = path.finish() else {
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

/// Paints a textarea's scrollbar against `border_box` (absolute canvas
/// pixels), and its resize corner if `resizer`.
#[allow(clippy::too_many_arguments)]
fn paint_scrollbar_and_resizer(
    buffer: &mut Surface,
    (bx, by, bw, bh): (f32, f32, f32, f32),
    border: (f32, f32, f32, f32),
    scrollbar: Option<ScrollbarPaint>,
    resizer: bool,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let s = scale_factor;
    let logical = (bw / s, bh / s);
    let border = (border.0 / s, border.1 / s, border.2 / s, border.3 / s);
    if let Some(scrollbar) = scrollbar {
        let g = scrollbar_geometry(logical, border, resizer, scrollbar);
        let bar_height = g.down.1 + g.down.3 - g.up.1;
        fill_rect(
            buffer,
            bx + g.bar_x * s,
            by + g.up.1 * s,
            SCROLLBAR_WIDTH * s,
            bar_height * s,
            SCROLLBAR_TRACK,
            clip,
        );
        let color = |part: ScrollbarPart| {
            if scrollbar.active == part {
                SCROLLBAR_ACTIVE
            } else {
                SCROLLBAR_IDLE
            }
        };
        let (tx, ty, tw, th) = g.thumb;
        fill_rect(
            buffer,
            bx + tx * s,
            by + ty * s,
            tw * s,
            th * s,
            color(ScrollbarPart::Thumb),
            clip,
        );
        let cx = bx + (g.bar_x + SCROLLBAR_WIDTH / 2.0) * s;
        let up_top = by + (g.up.1 + 6.0) * s;
        fill_triangle(
            buffer,
            [
                (cx, up_top),
                (cx - 3.5 * s, up_top + 6.0 * s),
                (cx + 3.5 * s, up_top + 6.0 * s),
            ],
            color(ScrollbarPart::ButtonUp),
            clip,
        );
        let down_top = by + (g.down.1 + 6.0) * s;
        fill_triangle(
            buffer,
            [
                (cx - 3.5 * s, down_top),
                (cx + 3.5 * s, down_top),
                (cx, down_top + 6.0 * s),
            ],
            color(ScrollbarPart::ButtonDown),
            clip,
        );
    }
    if resizer {
        // Two diagonals against the bottom-right corner, measured in Edge:
        // 7px long starting 10px from the right and 4px from the bottom,
        // and a shorter 3px one beside it.
        let right = bx + bw;
        let bottom = by + bh;
        for (offset, length) in [(10.0, 7), (6.0, 3)] {
            for i in 0..length {
                let x = right - (offset - i as f32) * s;
                let y = bottom - (4.0 + i as f32) * s;
                fill_rect(buffer, x, y, s, s, RESIZER_COLOR, clip);
            }
        }
    }
}

fn paint_spinner(
    buffer: &mut Surface,
    (x, y, width, height): (f32, f32, f32, f32),
    hover: SpinnerHover,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let s = scale_factor;
    let cx = x + width - (SPINNER_RIGHT_INSET + SPINNER_WIDTH / 2.0) * s;
    let mid = y + height / 2.0;
    let arrow = |buffer: &mut Surface, cy: f32, up: bool, color: Rgba| {
        let (half_w, half_h) = (3.5 * s, 2.5 * s);
        let (apex_y, base_y) = if up {
            (cy - half_h, cy + half_h)
        } else {
            (cy + half_h, cy - half_h)
        };
        let mut path = PathBuilder::new();
        path.move_to(cx, apex_y);
        path.line_to(cx - half_w, base_y);
        path.line_to(cx + half_w, base_y);
        path.close();
        let Some(path) = path.finish() else {
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
    };
    let color = |own: SpinnerHover| {
        if hover == own {
            SPINNER_HOVER
        } else {
            SPINNER_IDLE
        }
    };
    arrow(buffer, mid - 3.5 * s, true, color(SpinnerHover::Up));
    arrow(buffer, mid + 5.5 * s, false, color(SpinnerHover::Down));
}

/// `functions` with each blur radius in the canvas's own pixels. A filter's
/// lengths are CSS pixels; the canvas of a HiDPI window is physical, so a
/// `blur(8px)` is 16 canvas pixels at a scale factor of 2. Brightness,
/// contrast and saturate are unitless and unchanged; a drop shadow's offset and
/// blur are lengths.
fn scaled_filters(functions: &[FilterFunction], scale_factor: f32) -> Vec<FilterFunction> {
    functions
        .iter()
        .map(|function| match *function {
            FilterFunction::Blur(radius) => FilterFunction::Blur(radius * scale_factor),
            FilterFunction::DropShadow { x, y, blur, color } => FilterFunction::DropShadow {
                x: x * scale_factor,
                y: y * scale_factor,
                blur: blur * scale_factor,
                color,
            },
            other => other,
        })
        .collect()
}

/// Total padding `filters`' own combined blur reach needs on every side
/// — zero for the functions that are pointwise and never read a neighboring
/// pixel, and the offset plus the blur for a drop shadow. Matched exhaustively (no wildcard
/// arm) so a future [`FilterFunction`] variant fails to compile here
/// until someone classifies its own extent, rather than silently
/// contributing zero padding for a filter that actually needs some.
fn filter_inflation(filters: &[FilterFunction], scale_factor: f32) -> f32 {
    filters
        .iter()
        .map(|function| match function {
            FilterFunction::Blur(radius) => blur::kernel_radius(*radius * scale_factor) as f32,
            FilterFunction::DropShadow { x, y, blur, .. } => {
                x.abs().max(y.abs()) * scale_factor
                    + blur::kernel_radius(*blur * scale_factor) as f32
            }
            FilterFunction::Brightness(_)
            | FilterFunction::Contrast(_)
            | FilterFunction::Saturate(_)
            | FilterFunction::Grayscale(_)
            | FilterFunction::HueRotate(_)
            | FilterFunction::Invert(_)
            | FilterFunction::Opacity(_)
            | FilterFunction::Sepia(_)
            // Its reach depends on the box it is applied to: see [`node_filter_inflation`].
            | FilterFunction::Url(_) => 0.0,
        })
        .sum()
}

/// [`filter_inflation`] for the filters of `node`, with the reach of the SVG
/// `<filter>` regions that its `url()` items name, which depends on its box.
pub(crate) fn node_filter_inflation(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
    scale_factor: f32,
) -> f32 {
    let Some(style) = styles.get(&node) else {
        return 0.0;
    };
    let mut pad = filter_inflation(&style.filter, scale_factor);
    if let Some(layout) = layouts.get(&node) {
        for function in &style.filter {
            if let FilterFunction::Url(url) = function {
                pad += svg_ref::filter_reach(arena, styles, url, (layout.width, layout.height))
                    * scale_factor;
            }
        }
    }
    pad
}

/// Everything `node` paints itself, without its children: its box, the reach of
/// its outer shadows and the room its text can take, as a rectangle
/// `(x0, y0, x1, y1)` that holds all of it.
#[allow(clippy::too_many_arguments)]
fn own_extent(
    arena: &Tree,
    styles: &StyleMap,
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
        let ring = ink_reach(style, scale_factor);
        min_x -= ring;
        min_y -= ring;
        max_x += ring;
        max_y += ring;
    }

    let border = scaled_border(style.map_or(NO_BORDER, |s| s.border), scale_factor);
    let no_padding = florui_style::Edges {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };
    let padding = scaled_padding(style.map_or(no_padding, |s| s.padding), scale_factor);
    let content_x = abs_x + border.left.width + padding.left;
    let content_y = abs_y + border.top.width + padding.top;
    let content_width =
        (layout.width - border.left.width - border.right.width - padding.left - padding.right)
            .max(0.0);
    let wrap_width = wrap_width(content_width, scale_factor);
    let font_size = style.map_or(16.0, |s| s.font_size);
    // Glyph ink can overhang its own advance/line box (an italic swash,
    // a tall diacritic) — one `font_size` of slack on every side, same
    // reasoning as `rasterize_and_blur`'s own padding: cheap, and the
    // 1c visibility clamp already tightens this back down wherever the
    // extra room isn't actually visible.
    let ink_pad = font_size * scale_factor;
    let mut shaped_extent = None;
    let blocks = florui_layout::anonymous_blocks(arena, styles, node);
    if !blocks.is_empty() {
        for block in blocks {
            let Some(&at) = layouts.get(&block) else {
                continue;
            };
            if let Some(shaped) = florui_layout::shape_anonymous_block(
                font,
                arena,
                styles,
                (layouts, scale_factor),
                block,
                self_wrap(at.width, scale_factor),
            ) {
                min_x = min_x.min(abs_x + at.x - ink_pad);
                min_y = min_y.min(abs_y + at.y - ink_pad);
                max_x = max_x.max(abs_x + at.x + shaped.layout.width * scale_factor + ink_pad);
                max_y = max_y.max(abs_y + at.y + shaped.layout.height * scale_factor + ink_pad);
            }
        }
    } else if florui_layout::is_inline_formatting_context(arena, styles, node) {
        if let Some(shaped) = florui_layout::shape_inline_formatting_context(
            font,
            arena,
            styles,
            (layouts, scale_factor),
            node,
            wrap_width,
        ) {
            shaped_extent = Some((shaped.layout.width, shaped.layout.height));
        }
    } else {
        let text = arena.text_content(node);
        if !text.is_empty() {
            let shaped =
                font.shape_wrapped(florui_layout::node_text_style(style), text, wrap_width);
            shaped_extent = Some((shaped.width, shaped.height));
        }
    }
    if let Some((shaped_width, shaped_height)) = shaped_extent {
        min_x = min_x.min(content_x - ink_pad);
        min_y = min_y.min(content_y - ink_pad);
        max_x = max_x.max(content_x + shaped_width * scale_factor + ink_pad);
        max_y = max_y.max(content_y + shaped_height * scale_factor + ink_pad);
    }

    Some((min_x, min_y, max_x, max_y))
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
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    abs_x: f32,
    abs_y: f32,
    scale_factor: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = own_extent(
        arena,
        styles,
        layouts,
        font,
        node,
        abs_x,
        abs_y,
        scale_factor,
    )?;
    let layout = *layouts.get(&node)?;
    let style = styles.get(&node);
    let border = scaled_border(style.map_or(NO_BORDER, |s| s.border), scale_factor);

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
        let (cx0, cy0, cx1, cy1) = if styles.get(&child).is_some_and(florui_layout::preserves_3d) {
            // A 3D context places its own boxes; what it inks is already in
            // this box's plane.
            match scene3d::scene_extent(arena, styles, layouts, font, child, scale_factor) {
                Some(bounds) => bounds,
                None => continue,
            }
        } else {
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
            let child_transform = resolve_projection(
                arena,
                styles,
                layouts,
                child,
                (child_abs_x, child_abs_y),
                scale_factor,
            );
            if backface_is_hidden(
                styles,
                layouts,
                child,
                (child_abs_x, child_abs_y),
                &child_transform,
            ) {
                continue;
            }
            let child_has_effect = child_style.is_some_and(ComputedStyle::has_group_effect)
                || blend::isolates(arena, styles, child);
            let child_is_group =
                child_opacity < 1.0 || !child_transform.is_identity() || child_has_effect;

            let (cx0, cy0, cx1, cy1) = if child_is_group {
                let filter_pad = node_filter_inflation(arena, styles, layouts, child, scale_factor);
                let (fx0, fy0, fx1, fy1) = (
                    child_extent.0 - filter_pad,
                    child_extent.1 - filter_pad,
                    child_extent.2 + filter_pad,
                    child_extent.3 + filter_pad,
                );
                match child_transform.map_rect_aabb(fx0, fy0, fx1, fy1) {
                    Some(bounds) => bounds,
                    None => continue,
                }
            } else {
                child_extent
            };
            (cx0, cy0, cx1, cy1)
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
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
    target: TargetRegion,
    transform: Projection,
    own_only: bool,
) -> GroupSizing {
    #[cfg(test)]
    if FORCE_FULL_TARGET_GROUPS.with(|flag| flag.get()) {
        return record_sizing(GroupSizing::FullTarget(FallbackReason::AreaReachesTarget));
    }

    let extent = if own_only {
        own_extent(
            arena,
            styles,
            layouts,
            font,
            node,
            node_abs.0,
            node_abs.1,
            scale_factor,
        )
    } else {
        subtree_extent(
            arena,
            styles,
            layouts,
            font,
            node,
            node_abs.0,
            node_abs.1,
            scale_factor,
        )
    };
    let Some((x0, y0, x1, y1)) = extent else {
        return record_sizing(GroupSizing::FullTarget(FallbackReason::NothingToPaint));
    };

    let pad = node_filter_inflation(arena, styles, layouts, node, scale_factor);
    let (x0, y0, x1, y1) = (x0 - pad, y0 - pad, x1 + pad, y1 + pad);

    // 1c: clamp to what could possibly be seen — `target`'s own visible
    // rect, mapped back through `transform` into this group's own
    // pre-transform space (an inverted, non-invertible `transform`
    // — `scale(0)`, degenerate — skips the clamp rather than folding to
    // nothing; the unclamped extent is still a correct, if looser,
    // bound). Inflated by the same filter reach as above: a pixel just
    // inside the clamped edge still needs blur samples from just
    // outside it.
    let (x0, y0, x1, y1) = match transform.affine().and_then(|affine| affine.invert()) {
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
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
    clip: Option<ClipRect>,
    opacity: f32,
    transform: Projection,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    let Some(group) = render_group(
        buffer,
        arena,
        styles,
        layouts,
        font,
        node,
        node_abs,
        scale_factor,
        clip.clone(),
        transform,
        false,
        text_inputs,
        images,
    ) else {
        return;
    };
    let _composite_span = florui_profile::span(florui_profile::Phase::RasterComposite);
    let blend = styles
        .get(&node)
        .map_or(BlendMode::Normal, |style| style.mix_blend_mode);
    composite_group(buffer, &group, clip, opacity, transform, blend);
}

/// Renders `node`'s box and, unless `own_only`, its whole subtree into a fresh
/// surface sized to what it can ink and filtered by its own `filter`; `None`
/// when nothing of it can show. `transform` is where the surface will be placed,
/// which bounds how much of it is worth painting.
#[allow(clippy::too_many_arguments)]
fn render_group(
    buffer: &Surface,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
    clip: Option<ClipRect>,
    transform: Projection,
    own_only: bool,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> Option<Surface> {
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
            own_only,
        )
    };
    let (width, height, origin) = match sizing {
        GroupSizing::Empty => return None,
        GroupSizing::Bounded {
            width,
            height,
            origin,
        } => (width, height, origin),
        GroupSizing::FullTarget(_) => (buffer.width(), buffer.height(), buffer.origin),
    };
    let mut group = Surface::new(width, height, origin)?;
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
    if !own_only {
        let content_clip =
            clip_for_children(arena, styles, layouts, node, clip.clone(), scale_factor);
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
    }
    paint_outline_of(
        &mut group,
        arena,
        styles,
        layouts,
        node,
        scale_factor,
        inner_mask.as_ref(),
    );

    let filter = styles.get(&node).map_or(&[][..], |s| &s.filter[..]);
    svg_ref::apply_chain(
        arena,
        styles,
        &scaled_filters(filter, scale_factor),
        &mut group,
        node_abs,
        layouts.get(&node),
        scale_factor,
        &mut |pixmap, functions| apply_filters(pixmap, functions),
    );
    if let (Some(style), Some(layout)) = (styles.get(&node), layouts.get(&node)) {
        apply_clip_path(
            &mut group,
            arena,
            styles,
            style,
            layout,
            node_abs,
            scale_factor,
        );
        mask::apply_mask(
            &mut group,
            arena,
            styles,
            style,
            layout,
            node_abs,
            scale_factor,
        );
    }
    Some(group)
}

/// Cuts `group` to `style`'s `clip-path`: every pixel is scaled by how much of it
/// the shape covers (antialiased at the edge), and a shape with nothing inside
/// it clears the group. Runs after the filter and before opacity and the
/// transform, which is the order of the specification; the shape is in the
/// box's own space, so it moves with the box.
fn apply_clip_path(
    group: &mut Surface,
    arena: &Tree,
    styles: &StyleMap,
    style: &ComputedStyle,
    layout: &BoxLayout,
    node_abs: (f32, f32),
    scale_factor: f32,
) {
    if let florui_style::ClipPath::Url(url) = &style.clip_path {
        svg_ref::clip(arena, styles, url, group, node_abs, layout, scale_factor);
        return;
    }
    let Some(geometry) =
        florui_layout::resolve_clip_path(style, layout, node_abs.0, node_abs.1, scale_factor)
    else {
        return;
    };
    let mut builder = tiny_skia::PathBuilder::new();
    for contour in &geometry.contours {
        let mut points = contour.iter();
        let Some(&(x, y)) = points.next() else {
            continue;
        };
        builder.move_to(x, y);
        for &(x, y) in points {
            builder.line_to(x, y);
        }
        builder.close();
    }
    let (width, height) = (group.pixmap.width(), group.pixmap.height());
    let Some(path) = builder.finish() else {
        // Nothing is inside the shape.
        group.pixmap.fill(tiny_skia::Color::TRANSPARENT);
        return;
    };
    let Some(mut mask) = Mask::new(width, height) else {
        return;
    };
    let rule = if geometry.even_odd {
        FillRule::EvenOdd
    } else {
        FillRule::Winding
    };
    mask.fill_path(&path, rule, true, surface_transform(group));
    for (pixel, &coverage) in group.pixmap.pixels_mut().iter_mut().zip(mask.data()) {
        if coverage == 255 {
            continue;
        }
        let scale = |channel: u8| mul_div_255(channel, coverage);
        *pixel = PremultipliedColorU8::from_rgba(
            scale(pixel.red()),
            scale(pixel.green()),
            scale(pixel.blue()),
            scale(pixel.alpha()),
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
}

/// Places a rendered group on `buffer` through `transform`, at `opacity`,
/// blended with what is there by `blend`, and within `clip`.
fn composite_group(
    buffer: &mut Surface,
    group: &Surface,
    clip: Option<ClipRect>,
    opacity: f32,
    transform: Projection,
    blend: BlendMode,
) {
    let Some(affine) = transform.affine() else {
        let composite_mask = clip.and_then(|c| c.to_mask(buffer));
        if blend == BlendMode::Normal {
            projection::draw_projected(
                &mut buffer.pixmap,
                buffer.origin,
                &group.pixmap,
                group.origin,
                &transform.0,
                None,
                opacity,
                composite_mask.as_ref(),
            );
            return;
        }
        // The projection only knows how to lay one surface over another, so a
        // blended one is projected on its own first and that is what is blended.
        let Some(mut projected) = Pixmap::new(buffer.pixmap.width(), buffer.pixmap.height()) else {
            return;
        };
        projection::draw_projected(
            &mut projected,
            buffer.origin,
            &group.pixmap,
            group.origin,
            &transform.0,
            None,
            1.0,
            None,
        );
        let paint = PixmapPaint {
            opacity,
            blend_mode: blend::to_tiny_skia(blend),
            ..Default::default()
        };
        draw_shifted(
            &mut buffer.pixmap,
            &projected,
            (0, 0),
            &paint,
            composite_mask.as_ref(),
        );
        return;
    };
    // A turned or scaled group is sampled between its pixels; nearest-pixel
    // sampling leaves the edge of a rotated disc in steps.
    let paint = PixmapPaint {
        opacity,
        blend_mode: blend::to_tiny_skia(blend),
        quality: if affine.is_translate() {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bilinear
        },
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
        .pre_concat(affine);
    let composite_mask = clip.and_then(|c| c.to_mask(buffer));
    // A group that is only moved is drawn by the same pixels shifted by a whole
    // number, without the per-pixel sampling a general transform needs.
    match whole_pixel_shift(to_target) {
        Some((dx, dy)) => draw_shifted(
            &mut buffer.pixmap,
            &group.pixmap,
            (group.origin.0 + dx, group.origin.1 + dy),
            &paint,
            composite_mask.as_ref(),
        ),
        None => buffer.pixmap.draw_pixmap(
            group.origin.0,
            group.origin.1,
            group.pixmap.as_ref(),
            &paint,
            to_target,
            composite_mask.as_ref(),
        ),
    }
}

/// Draws `source` with its top left at `(x, y)` in `target`, untransformed.
/// Only the part inside `target` is handed to the drawing, so the pixels
/// that fall outside are never looked at: a pixmap drawn partly out of its
/// target leaves a stray column along the far edge in tiny-skia.
fn draw_shifted(
    target: &mut Pixmap,
    source: &Pixmap,
    (x, y): (i32, i32),
    paint: &PixmapPaint,
    mask: Option<&Mask>,
) {
    let (x0, y0) = (x.max(0), y.max(0));
    let x1 = x
        .saturating_add(source.width() as i32)
        .min(target.width() as i32);
    let y1 = y
        .saturating_add(source.height() as i32)
        .min(target.height() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    // A clip that covers the whole drawn area (the usual rectangular one) is
    // the same as no clip.
    let mask = mask.filter(|mask| !mask_is_full(mask, target.width(), (x0, y0, x1, y1)));
    if paint.opacity == 1.0
        && mask.is_none()
        && paint.blend_mode == tiny_skia::BlendMode::SourceOver
    {
        blit_over(target, source, (x, y), (x0, y0, x1, y1));
        return;
    }
    let inside = IntRect::from_xywh(x0 - x, y0 - y, (x1 - x0) as u32, (y1 - y0) as u32);
    if (x0, y0, x1, y1) == (x, y, x + source.width() as i32, y + source.height() as i32) {
        target.draw_pixmap(x, y, source.as_ref(), paint, Transform::identity(), mask);
    } else if let Some(visible) = inside.and_then(|rect| source.clone_rect(rect)) {
        target.draw_pixmap(x0, y0, visible.as_ref(), paint, Transform::identity(), mask);
    }
}

/// Whether `mask` (as large as a target `width` wide) is fully opaque over the
/// pixels `(x0, y0, x1, y1)`.
fn mask_is_full(mask: &Mask, width: u32, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> bool {
    if mask.width() != width {
        return false;
    }
    let data = mask.data();
    (y0..y1).all(|row| {
        let start = row as usize * width as usize;
        data.get(start + x0 as usize..start + x1 as usize)
            .is_some_and(|cells| cells.iter().all(|&cell| cell == 255))
    })
}

/// `source`, with its top left at `(x, y)`, composited over `target` within
/// `(x0, y0, x1, y1)` (which lies inside both): an opaque pixel is copied, a
/// clear one skipped, and any other is blended as `source + target * (255 -
/// alpha) / 255` on every channel, rounded, which is the same rounding
/// tiny-skia's own compositing gives. Several times faster than going through
/// its general pipeline.
fn blit_over(
    target: &mut Pixmap,
    source: &Pixmap,
    (x, y): (i32, i32),
    (x0, y0, x1, y1): (i32, i32, i32, i32),
) {
    let (target_width, source_width) = (target.width() as usize, source.width() as usize);
    let count = (x1 - x0) as usize * 4;
    let source_data = source.data();
    let pixels = (x1 - x0) as usize * (y1 - y0) as usize;
    gradient::for_rows(
        target.data_mut(),
        target_width,
        (y0 as usize, y1 as usize),
        pixels,
        |row, line| {
            let from = ((row as i32 - y) as usize * source_width + (x0 - x) as usize) * 4;
            let src = &source_data[from..from + count];
            let dst = &mut line[x0 as usize * 4..x0 as usize * 4 + count];
            for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                match s[3] {
                    0 => {}
                    255 => d.copy_from_slice(s),
                    alpha => {
                        let keep = 255 - u32::from(alpha);
                        for (out, &over) in d.iter_mut().zip(s) {
                            let mixed = u32::from(over) + (u32::from(*out) * keep + 127) / 255;
                            *out = mixed.min(255) as u8;
                        }
                    }
                }
            }
        },
    );
}

/// The whole-pixel shift that draws the same pixels as `transform` when it
/// is a pure translation, or `None` for anything else (a scale, a rotation, a
/// skew) and for a shift too close to half a pixel. The general path moves the
/// group's edge and samples its pixels by two slightly different rules, so at a
/// half pixel it leaves a seam that no whole shift reproduces; that case is
/// left to it.
fn whole_pixel_shift(transform: Transform) -> Option<(i32, i32)> {
    let Transform {
        sx,
        kx,
        ky,
        sy,
        tx,
        ty,
    } = transform;
    if sx != 1.0 || sy != 1.0 || kx != 0.0 || ky != 0.0 {
        return None;
    }
    let shift = |t: f32| {
        let near_half = (t - t.floor() - 0.5).abs() < HALF_PIXEL_BAND;
        (t.is_finite() && t.abs() < 1.0e6 && !near_half).then(|| t.round() as i32)
    };
    Some((shift(tx)?, shift(ty)?))
}

/// How near half a pixel a translation may be and still be left to the general path.
const HALF_PIXEL_BAND: f32 = 1.0e-3;

/// `node`'s transforms (and its parent's `perspective`) about its
/// `transform-origin`, as a [`Projection`] of its plane — the identity (a cheap
/// no-op composite, taken by [`paint_nodes`]'s own fast path) for `none`.
/// `node_abs` is the node's absolute border-box position in the canvas's
/// physical pixels and `scale_factor` converts authored lengths to them. The
/// math is [`florui_layout::resolve_node_transform`], shared with hit testing
/// so a node is hit where it is drawn.
fn resolve_projection(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
) -> Projection {
    Projection(florui_layout::resolve_node_transform(
        arena,
        styles,
        layouts,
        node,
        node_abs.0,
        node_abs.1,
        scale_factor,
    ))
}

/// Whether `node` is `backface-visibility: hidden` and turned to show its
/// back, so that it (with everything it holds) is not drawn.
fn backface_is_hidden(
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
    node_abs: (f32, f32),
    projection: &Projection,
) -> bool {
    // Almost no node hides its back: look at the box only for one that does.
    let Some(style) = styles.get(&node).filter(|s| s.backface_hidden) else {
        return false;
    };
    let Some(layout) = layouts.get(&node) else {
        return false;
    };
    florui_layout::hides_its_back(style, layout, &projection.0, node_abs)
}

/// Maps an axis-aligned rectangle's own four corners through the affine
/// `transform` and returns the axis-aligned bounding box of the result, as
/// `(min_x, min_y, max_x, max_y)`; [`group_extent`] uses it to map the visible
/// target back into a group's own space.
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
#[allow(clippy::too_many_arguments)]
fn apply_backdrop_filter(
    buffer: &mut Surface,
    svg: (&Tree, &StyleMap),
    outline: &RoundedRect,
    functions: &[FilterFunction],
    glass: Option<&GlassMaterial>,
    scale_factor: f32,
    clip: Option<&Mask>,
    fast_blur: bool,
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
    // The glass material refracts what is behind the panel before the
    // filters run, so a blur softens the lens.
    if let Some(params) =
        glass.and_then(|material| material::Params::resolve(material, scale_factor))
        && let Some(refracted) = refract_backdrop(buffer, outline, (x0, y0), &backdrop, &params)
    {
        backdrop = refracted;
    }
    // A `url()` item goes through the SVG renderer, between the native stretches.
    let scaled = scaled_filters(functions, scale_factor);
    let origin = (buffer.origin.0 + x0, buffer.origin.1 + y0);
    let mut start = 0;
    for (index, function) in scaled.iter().enumerate() {
        let FilterFunction::Url(url) = function else {
            continue;
        };
        backdrop = filtered_backdrop(backdrop, &scaled[start..index], fast_blur);
        backdrop = svg_ref::filter_pixmap(
            svg.0,
            svg.1,
            url,
            backdrop,
            origin,
            (x, y, width, height),
            scale_factor,
        );
        start = index + 1;
    }
    let backdrop = filtered_backdrop(backdrop, &scaled[start..], fast_blur);
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

/// Runs `functions` over `backdrop`.
///
/// A blur reads past the pixels it writes; over a backdrop that is cut to the
/// panel those would be transparent and the panel's edges would fade toward
/// nothing, so the cut is extended by mirroring first. The filters after the
/// last blur look at one pixel at a time, so they run on the part that is
/// kept, not on the margin that was only there to be blurred; the result is
/// the same.
fn filtered_backdrop(
    mut backdrop: Pixmap,
    functions: &[FilterFunction],
    fast_blur: bool,
) -> Pixmap {
    let margin = blur_margin(functions);
    let after_blur = functions
        .iter()
        .rposition(|function| matches!(function, FilterFunction::Blur(radius) if *radius > 0.0))
        .map_or(0, |last| last + 1);
    let (until_blur, after_blur) = functions.split_at(after_blur);
    let filtered = (margin > 0)
        .then(|| {
            let (width, height) = (backdrop.width(), backdrop.height());
            let mut padded = mirror_padded(&backdrop, margin)?;
            apply_filters_with(&mut padded, until_blur, fast_blur);
            cropped(&padded, margin, width, height)
        })
        .flatten();
    match filtered {
        Some(filtered) => {
            backdrop = filtered;
            apply_filters(&mut backdrop, after_blur);
        }
        None => apply_filters_with(&mut backdrop, functions, fast_blur),
    }
    backdrop
}

/// What the glass material will actually do for a request, in CSS pixels:
/// the values after the limits of its contract, and whether the request was
/// cut. `None` for `quality: off`, which is the basic glass. This is what a
/// status line should show instead of the requested values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlassEffective {
    pub refraction: f32,
    pub edge: f32,
    pub quality: florui_style::GlassQuality,
    pub clamped: bool,
}

/// See [`GlassEffective`].
pub fn glass_effective(material: &GlassMaterial) -> Option<GlassEffective> {
    material::Params::resolve(material, 1.0).map(|params| GlassEffective {
        refraction: params.refraction,
        edge: params.edge,
        quality: params.quality,
        clamped: params.clamped,
    })
}

/// `backdrop` (the surface's pixels at `(x0, y0)`, the part of the panel on
/// the surface) refracted by the glass material, reading as far past the
/// panel's box as the material can displace.
fn refract_backdrop(
    buffer: &Surface,
    outline: &RoundedRect,
    (x0, y0): (i32, i32),
    backdrop: &Pixmap,
    params: &material::Params,
) -> Option<Pixmap> {
    let reach = params.reach() as i32;
    let (ex0, ey0) = ((x0 - reach).max(0), (y0 - reach).max(0));
    let ex1 = (x0 + backdrop.width() as i32 + reach).min(buffer.width() as i32);
    let ey1 = (y0 + backdrop.height() as i32 + reach).min(buffer.height() as i32);
    let expanded = buffer.pixmap.clone_rect(IntRect::from_xywh(
        ex0,
        ey0,
        (ex1 - ex0) as u32,
        (ey1 - ey0) as u32,
    )?)?;
    let (lx, ly) = buffer.local(outline.x, outline.y);
    let region = material::Region {
        width: backdrop.width(),
        height: backdrop.height(),
        outline_width: outline.width,
        outline_height: outline.height,
        offset: (x0 as f32 - lx, y0 as f32 - ly),
    };
    let (refracted, moved) = material::refract(
        &material::Source {
            pixmap: &expanded,
            panel_x: (x0 - ex0) as u32,
            panel_y: (y0 - ey0) as u32,
        },
        &region,
        outline,
        params,
    )?;
    florui_profile::count(florui_profile::Counter::GlassRefractions, 1);
    florui_profile::count(florui_profile::Counter::GlassPixelsMoved, u64::from(moved));
    if params.clamped {
        florui_profile::count(florui_profile::Counter::GlassClamped, 1);
    }
    Some(refracted)
}

/// Adds the glass material's rim light over what is painted of the panel.
fn paint_glass_rim(
    buffer: &mut Surface,
    outline: &RoundedRect,
    material: &GlassMaterial,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let Some(params) = material::Params::resolve(material, scale_factor) else {
        return;
    };
    let (lx, ly) = buffer.local(outline.x, outline.y);
    let x0 = lx.max(0.0).floor() as i32;
    let y0 = ly.max(0.0).floor() as i32;
    let x1 = (lx + outline.width).min(buffer.width() as f32).ceil() as i32;
    let y1 = (ly + outline.height).min(buffer.height() as f32).ceil() as i32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let region = material::Region {
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
        outline_width: outline.width,
        outline_height: outline.height,
        offset: (x0 as f32 - lx, y0 as f32 - ly),
    };
    let stride = buffer.width() as usize;
    let start = y0 as usize * stride + x0 as usize;
    let coverage = |col: u32, row: u32| {
        clip.map_or(255, |mask| {
            mask.data()[(y0 as usize + row as usize) * stride + x0 as usize + col as usize]
        })
    };
    material::add_rim_light(
        &mut buffer.pixmap.pixels_mut()[start..],
        stride,
        &region,
        outline,
        &params,
        coverage,
    );
}

/// How far a chain's blurs reach beyond the pixels they read: the Gaussian
/// kernel's own radius, summed over the chain (each blur reads what the
/// previous one produced).
fn blur_margin(functions: &[FilterFunction]) -> u32 {
    functions
        .iter()
        .map(|function| match *function {
            FilterFunction::Blur(radius) if radius > 0.0 => blur::kernel_radius(radius),
            _ => 0,
        })
        .sum()
}

/// Where `index` falls when the `size` pixels of an edge are mirrored
/// outward, the edge pixel itself included (`-1` reads `0`, `-2` reads `1`),
/// the way a backdrop is extended before it is blurred.
fn mirrored(index: i64, size: i64) -> i64 {
    let period = 2 * size;
    let wrapped = index.rem_euclid(period);
    if wrapped >= size {
        period - 1 - wrapped
    } else {
        wrapped
    }
}

/// `source` with `margin` pixels added on every side, each filled by mirroring
/// the nearest edge. A backdrop is blurred over this instead of over
/// transparent surroundings, which would fade its edges toward nothing.
fn mirror_padded(source: &Pixmap, margin: u32) -> Option<Pixmap> {
    let (width, height) = (source.width() as usize, source.height() as usize);
    let margin_px = margin as usize;
    let padded_width = width + 2 * margin_px;
    let padded_height = height + 2 * margin_px;
    let mut padded = Pixmap::new(padded_width as u32, padded_height as u32)?;
    let pixels = source.pixels();
    let edge = |index: usize| mirrored(index as i64 - margin_px as i64, width as i64) as usize;
    let left: Vec<usize> = (0..margin_px).map(edge).collect();
    let right: Vec<usize> = (margin_px + width..padded_width).map(edge).collect();
    for (row, destination) in padded
        .pixels_mut()
        .chunks_exact_mut(padded_width)
        .enumerate()
    {
        let source_row = mirrored(row as i64 - margin_px as i64, height as i64) as usize;
        let source_row = &pixels[source_row * width..(source_row + 1) * width];
        let (head, rest) = destination.split_at_mut(margin_px);
        let (middle, tail) = rest.split_at_mut(width);
        middle.copy_from_slice(source_row);
        for (pixel, &column) in head.iter_mut().zip(&left) {
            *pixel = source_row[column];
        }
        for (pixel, &column) in tail.iter_mut().zip(&right) {
            *pixel = source_row[column];
        }
    }
    Some(padded)
}

/// The `width` x `height` pixels of `padded` that sit `margin` in from its
/// corner.
fn cropped(padded: &Pixmap, margin: u32, width: u32, height: u32) -> Option<Pixmap> {
    padded.clone_rect(IntRect::from_xywh(
        margin as i32,
        margin as i32,
        width,
        height,
    )?)
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
    apply_filters_with(pixmap, functions, false);
}

/// [`apply_filters`], with `fast` letting a large blur run on a smaller copy
/// (see [`blur_pixmap_reduced`]).
fn apply_filters_with(pixmap: &mut Pixmap, functions: &[FilterFunction], fast: bool) {
    let (width, height) = (pixmap.width(), pixmap.height());
    for function in functions {
        match *function {
            FilterFunction::Blur(radius) if fast && blur::reduction_for(radius) > 1 => {
                blur_pixmap_reduced(pixmap, radius, blur::reduction_for(radius));
            }
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
            FilterFunction::Grayscale(amount) => {
                color_matrix_in_place(pixmap, grayscale_matrix(amount), 0.0)
            }
            FilterFunction::Sepia(amount) => {
                color_matrix_in_place(pixmap, sepia_matrix(amount), 0.0)
            }
            FilterFunction::HueRotate(degrees) => {
                color_matrix_in_place(pixmap, hue_rotate_matrix(degrees), 0.0)
            }
            FilterFunction::Invert(amount) => invert_in_place(pixmap, amount),
            FilterFunction::Opacity(amount) => opacity_in_place(pixmap, amount),
            FilterFunction::DropShadow { x, y, blur, color } => {
                drop_shadow_in_place(pixmap, x, y, blur, color)
            }
            // Drawn by the SVG renderer, not here: see [`svg_ref::apply_chain`].
            FilterFunction::Url(_) => {}
        }
    }
}

/// A blur of `radius_px` done on a copy of `pixmap` shrunk by `factor`, with
/// the sigma shrunk the same, and enlarged back over it. Costs about
/// `1 / factor^2` of the full blur; the result is the same blur a little
/// softer at fine detail, which is why it is opt-in.
fn blur_pixmap_reduced(pixmap: &mut Pixmap, radius_px: f32, factor: usize) {
    let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
    let (small_w, small_h) = blur::shrunk_size(width, height, factor);
    let Some(mut small) = Pixmap::new(small_w as u32, small_h as u32) else {
        return;
    };
    small
        .data_mut()
        .copy_from_slice(&blur::shrink_rgba(pixmap.data(), width, height, factor));
    blur_pixmap_in_place(
        &mut small,
        small_w as u32,
        small_h as u32,
        radius_px / factor as f32,
    );
    blur::enlarge_rgba(
        small.data(),
        (small_w, small_h),
        pixmap.data_mut(),
        (width, height),
        factor,
    );
    for pixel in pixmap.pixels_mut() {
        let alpha = pixel.alpha();
        *pixel = PremultipliedColorU8::from_rgba(
            pixel.red().min(alpha),
            pixel.green().min(alpha),
            pixel.blue().min(alpha),
            alpha,
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
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
    // A wide blur is three box passes per axis, whose cost does not grow
    // with the radius; a narrow one is the exact kernel below. Either way every
    // premultiplied channel is blurred the same, so the color of a pixel never
    // exceeds its alpha except by a rounding step, which is cut back.
    if blur::box_blur_rgba_in_place(pixmap.data_mut(), width, height, radius_px) {
        for pixel in pixmap.pixels_mut() {
            let alpha = pixel.alpha();
            *pixel = PremultipliedColorU8::from_rgba(
                pixel.red().min(alpha),
                pixel.green().min(alpha),
                pixel.blue().min(alpha),
                alpha,
            )
            .unwrap_or(PremultipliedColorU8::TRANSPARENT);
        }
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

/// `grayscale(amount)` as the 3x3 matrix of the Filter Effects spec; `amount`
/// is cut to `0..=1`.
fn grayscale_matrix(amount: f32) -> [[f32; 3]; 3] {
    let s = 1.0 - amount.clamp(0.0, 1.0);
    [
        [
            0.2126 + 0.7874 * s,
            0.7152 - 0.7152 * s,
            0.0722 - 0.0722 * s,
        ],
        [
            0.2126 - 0.2126 * s,
            0.7152 + 0.2848 * s,
            0.0722 - 0.0722 * s,
        ],
        [
            0.2126 - 0.2126 * s,
            0.7152 - 0.7152 * s,
            0.0722 + 0.9278 * s,
        ],
    ]
}

/// `sepia(amount)`, likewise.
fn sepia_matrix(amount: f32) -> [[f32; 3]; 3] {
    let s = 1.0 - amount.clamp(0.0, 1.0);
    [
        [0.393 + 0.607 * s, 0.769 - 0.769 * s, 0.189 - 0.189 * s],
        [0.349 - 0.349 * s, 0.686 + 0.314 * s, 0.168 - 0.168 * s],
        [0.272 - 0.272 * s, 0.534 - 0.534 * s, 0.131 + 0.869 * s],
    ]
}

/// `hue-rotate(degrees)`, likewise.
fn hue_rotate_matrix(degrees: f32) -> [[f32; 3]; 3] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [
        [
            0.213 + cos * 0.787 - sin * 0.213,
            0.715 - cos * 0.715 - sin * 0.715,
            0.072 - cos * 0.072 + sin * 0.928,
        ],
        [
            0.213 - cos * 0.213 + sin * 0.143,
            0.715 + cos * 0.285 + sin * 0.140,
            0.072 - cos * 0.072 - sin * 0.283,
        ],
        [
            0.213 - cos * 0.213 - sin * 0.787,
            0.715 - cos * 0.715 + sin * 0.715,
            0.072 + cos * 0.928 + sin * 0.072,
        ],
    ]
}

/// Replaces each pixel's straight color `c` by `matrix * c + bias` (the bias in
/// `0..=1` of full scale) and keeps its alpha. Linear in the color, so it is
/// done on the premultiplied values as they are, with no division by alpha:
/// the alpha scales both sides, the bias becomes `bias * alpha`, and the cut to
/// `0..=255` on the straight color is a cut to `0..=alpha` on the product.
fn color_matrix_in_place(pixmap: &mut Pixmap, matrix: [[f32; 3]; 3], bias: f32) {
    for pixel in pixmap.pixels_mut() {
        let alpha = pixel.alpha();
        if alpha == 0 {
            continue;
        }
        let (r, g, b) = (
            pixel.red() as f32,
            pixel.green() as f32,
            pixel.blue() as f32,
        );
        let ceiling = alpha as f32;
        let offset = bias * ceiling;
        let channel = |row: [f32; 3]| {
            ((row[0] * r + row[1] * g + row[2] * b + offset).clamp(0.0, ceiling) + 0.5) as u8
        };
        *pixel = PremultipliedColorU8::from_rgba(
            channel(matrix[0]),
            channel(matrix[1]),
            channel(matrix[2]),
            alpha,
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
}

/// `invert(amount)`: each color channel `c` becomes `amount + c * (1 - 2 *
/// amount)`; `amount` is cut to `0..=1`.
fn invert_in_place(pixmap: &mut Pixmap, amount: f32) {
    let amount = amount.clamp(0.0, 1.0);
    let k = 1.0 - 2.0 * amount;
    color_matrix_in_place(
        pixmap,
        [[k, 0.0, 0.0], [0.0, k, 0.0], [0.0, 0.0, k]],
        amount,
    );
}

/// The filter list's `opacity(amount)`: every channel, alpha included, scaled
/// by `amount` (cut to `0..=1`).
fn opacity_in_place(pixmap: &mut Pixmap, amount: f32) {
    let amount = amount.clamp(0.0, 1.0);
    for pixel in pixmap.pixels_mut() {
        let scale = |channel: u8| ((channel as f32) * amount + 0.5) as u8;
        let alpha = scale(pixel.alpha());
        *pixel = PremultipliedColorU8::from_rgba(
            scale(pixel.red()).min(alpha),
            scale(pixel.green()).min(alpha),
            scale(pixel.blue()).min(alpha),
            alpha,
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
}

/// `drop-shadow(x y blur color)` on `pixmap`'s own pixels: the shape of what
/// is painted (its alpha) in `color`, blurred by a standard deviation of
/// `blur`, moved by `(x, y)` and put behind the content. The pixmap must have
/// room for the shadow around the content ([`filter_inflation`] sizes it).
fn drop_shadow_in_place(pixmap: &mut Pixmap, x: f32, y: f32, blur: f32, color: Rgba) {
    let (width, height) = (pixmap.width(), pixmap.height());
    let Some(mut shadow) = Pixmap::new(width, height) else {
        return;
    };
    let tint = f32::from(color.a) / 255.0;
    for (source, target) in pixmap.pixels().iter().zip(shadow.pixels_mut()) {
        let coverage = f32::from(source.alpha()) / 255.0 * tint;
        if coverage <= 0.0 {
            continue;
        }
        let scaled = |channel: u8| (f32::from(channel) * coverage + 0.5) as u8;
        let alpha = (coverage * 255.0 + 0.5) as u8;
        *target = PremultipliedColorU8::from_rgba(
            scaled(color.r).min(alpha),
            scaled(color.g).min(alpha),
            scaled(color.b).min(alpha),
            alpha,
        )
        .unwrap_or(PremultipliedColorU8::TRANSPARENT);
    }
    blur_pixmap_in_place(&mut shadow, width, height, blur);

    let Some(mut moved) = Pixmap::new(width, height) else {
        return;
    };
    let whole = x.fract() == 0.0 && y.fract() == 0.0;
    moved.draw_pixmap(
        0,
        0,
        shadow.as_ref(),
        &PixmapPaint {
            quality: if whole {
                FilterQuality::Nearest
            } else {
                FilterQuality::Bilinear
            },
            ..Default::default()
        },
        Transform::from_translate(x, y),
        None,
    );
    moved.draw_pixmap(
        0,
        0,
        pixmap.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    pixmap.data_mut().copy_from_slice(moved.data());
}

/// `saturate()`'s own luminance-preserving mix: real CSS's own saturate
/// matrix (CSS Filter Effects' own reference to SVG's `feColorMatrix
/// type="saturate"`) is algebraically identical to mixing each channel
/// toward its own Rec.-601-ish luma at `1 - factor` and keeping the rest
/// at `factor` — `factor` `1.0` is a no-op, `0.0` is grayscale, and above
/// `1.0` oversaturates.
///
/// The mix is linear in the color channels, so it is done on the premultiplied
/// values as they are (the alpha scales both sides of it), with no division,
/// and each result is cut to `0..=alpha`, which is the same bound as
/// `0..=255` on the straight color.
fn saturate_premultiplied(pixel: PremultipliedColorU8, factor: f32) -> PremultipliedColorU8 {
    let alpha = pixel.alpha();
    if alpha == 0 {
        return pixel;
    }
    let (r, g, b) = (
        pixel.red() as f32,
        pixel.green() as f32,
        pixel.blue() as f32,
    );
    let luma = 0.213 * r + 0.715 * g + 0.072 * b;
    let ceiling = alpha as f32;
    let mix = |channel: f32| ((luma + (channel - luma) * factor).clamp(0.0, ceiling) + 0.5) as u8;
    PremultipliedColorU8::from_rgba(mix(r), mix(g), mix(b), alpha)
        .unwrap_or(PremultipliedColorU8::TRANSPARENT)
}

/// The earlier form of [`saturate_premultiplied`], which divides by alpha
/// to the straight color and back, kept as the oracle the division-free one is
/// compared with.
#[cfg(test)]
fn saturate_by_unpremultiplying(pixel: PremultipliedColorU8, factor: f32) -> PremultipliedColorU8 {
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
/// within the one shared parent `nodes` already came from. Under a flex or
/// grid parent the `order` of an item then breaks the tie within one level,
/// ahead of document order.
pub fn paint_order(
    styles: &StyleMap,
    parent_display: Option<Display>,
    nodes: &[NodeId],
) -> Vec<NodeId> {
    let parent_is_flex_or_grid = parent_display.is_some_and(Display::is_flex_or_grid);
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
    ordered.sort_by_key(|id| match styles.get(id) {
        Some(style) if parent_is_flex_or_grid => style.item_stacking(),
        Some(style) if style.position != Position::Static => (style.z_index.unwrap_or(0), 0),
        _ => (0, 0),
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
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    scale_factor: f32,
    clip: Option<&Mask>,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    if let Some(&layout) = layouts.get(&node) {
        florui_profile::count(florui_profile::Counter::NodesPainted, 1);
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
        // A hidden box keeps its place and paints nothing of its own; a
        // descendant that is visible again still paints, and so does an
        // inline element of its text.
        let shown = style.is_none_or(|s| s.visibility.is_visible());

        let backdrop_filter: &[FilterFunction] = if shown {
            style.map_or(&[][..], |s| &s.backdrop_filter[..])
        } else {
            &[]
        };
        // The material applies to a node that also has a `backdrop-filter`.
        let glass = style
            .filter(|_| !backdrop_filter.is_empty())
            .and_then(|s| match &s.glass {
                GlassSpec::Material(material) => Some(material),
                _ => None,
            });
        let fast_blur = style.is_some_and(|s| s.backdrop_blur == florui_style::BackdropBlur::Fast);
        {
            let _span = florui_profile::span(florui_profile::Phase::RasterBackdrop);
            apply_backdrop_filter(
                buffer,
                (arena, styles),
                &outline,
                backdrop_filter,
                glass,
                scale_factor,
                clip,
                fast_blur,
            );
        }

        // The layout border stays: it is where the content sits.
        let border = scaled_border(style.map_or(NO_BORDER, |s| s.border), scale_factor);
        let box_shadow: &[florui_style::BoxShadow] = if shown {
            style.map_or(&[][..], |s| &s.box_shadow[..])
        } else {
            &[]
        };

        // Real CSS's own painting order for a normal-flow box with no
        // stacking context: outer (non-inset) shadows sit behind
        // everything else — background, then border, then content — while
        // inset shadows sit on top of the background but still behind the
        // border and content. See [`paint_box_shadows`]'s own doc for the
        // painted shape.
        {
            let _span = florui_profile::span(florui_profile::Phase::RasterShadows);
            paint_box_shadows_clipped(buffer, &outline, border, box_shadow, false, clip);
        }

        let background = style.map_or(Rgba::TRANSPARENT, |s| s.background_color);
        let blended_layers = style.filter(|s| shown && background::blends(&s.background_layers));
        // Where the glyphs of the box and what is in it are, for a layer that
        // is clipped to the text.
        let text_cover = style
            .filter(|s| {
                shown
                    && (background::clips_to_text(&s.background_layers)
                        || s.background_color_clip == florui_style::BackgroundBox::Text)
            })
            .and_then(|_| {
                let region = background::Region::of(buffer, &outline)?;
                text_coverage(
                    buffer,
                    region,
                    arena,
                    styles,
                    layouts,
                    font,
                    node,
                    scale_factor,
                )
            });
        // `background-color` is cut to the clip of the bottom layer.
        let color_clip = style.map_or(florui_style::BackgroundBox::Border, |s| {
            s.background_color_clip
        });
        let color_padding = scaled_padding(
            style.map_or(
                florui_style::Edges {
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                },
                |s| s.padding,
            ),
            scale_factor,
        );
        let color_cover = text_cover.as_ref().map(|cover| &cover.pixmap);
        if let Some(style) = blended_layers {
            let _span = florui_profile::span(florui_profile::Phase::RasterLayers);
            background::paint_blended(
                buffer,
                &outline,
                border,
                scaled_padding(style.padding, scale_factor),
                &style.background_layers,
                scale_factor,
                clip,
                text_cover.as_ref().map(|cover| &cover.pixmap),
                &|group| {
                    if background.a == 0 {
                        return;
                    }
                    if color_clip != florui_style::BackgroundBox::Border {
                        background::fill_clipped(
                            group,
                            &outline,
                            border,
                            color_padding,
                            color_clip,
                            background,
                            None,
                            color_cover,
                        );
                    } else if rounded {
                        fill_rounded_rect(group, &outline, background, None);
                    } else {
                        fill_rect(group, x, y, layout.width, layout.height, background, None);
                    }
                },
            );
        } else if shown && background.a != 0 {
            let _span = florui_profile::span(florui_profile::Phase::RasterFill);
            if color_clip != florui_style::BackgroundBox::Border {
                background::fill_clipped(
                    buffer,
                    &outline,
                    border,
                    color_padding,
                    color_clip,
                    background,
                    clip,
                    color_cover,
                );
            } else if rounded {
                fill_rounded_rect(buffer, &outline, background, clip);
            } else {
                fill_rect(buffer, x, y, layout.width, layout.height, background, clip);
            }
        }

        if let Some(style) = style
            && shown
            && blended_layers.is_none()
            && !style.background_layers.is_empty()
        {
            let _span = florui_profile::span(florui_profile::Phase::RasterLayers);
            background::paint_layers(
                buffer,
                &outline,
                border,
                scaled_padding(style.padding, scale_factor),
                &style.background_layers,
                scale_factor,
                clip,
                text_cover.as_ref().map(|cover| &cover.pixmap),
            );
        }

        {
            let _span = florui_profile::span(florui_profile::Phase::RasterShadows);
            paint_box_shadows_clipped(buffer, &outline, border, box_shadow, true, clip);
        }

        if shown {
            let _span = florui_profile::span(florui_profile::Phase::RasterBorders);
            let image_drawn = style
                .and_then(|s| s.border_image.as_deref())
                .is_some_and(|image| {
                    border_image::paint(buffer, &outline, &border, image, scale_factor, clip)
                });
            if !image_drawn {
                paint_box_border(buffer, &outline, border, clip);
            }
        }
        if let Some(material) = glass {
            let _span = florui_profile::span(florui_profile::Phase::RasterBackdrop);
            paint_glass_rim(buffer, &outline, material, scale_factor, clip);
        }

        let _content_span = florui_profile::span(florui_profile::Phase::RasterContent);

        let color = style.map_or(Rgba::opaque(0, 0, 0), |s| s.color);
        let anonymous = if arena.children(node).is_empty() {
            Vec::new()
        } else {
            florui_layout::anonymous_blocks(arena, styles, node)
        };
        let no_padding = florui_style::Edges {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        };
        let padding = scaled_padding(style.map_or(no_padding, |s| s.padding), scale_factor);
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
        let wrap_width = wrap_width(content_width, scale_factor);

        // Only the inline elements of a context can show themselves again.
        if !shown
            && anonymous.is_empty()
            && !florui_layout::is_inline_formatting_context(arena, styles, node)
        {
            return;
        }

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
                    (wrap_width, content_width / scale_factor),
                    scale_factor,
                    clip,
                );
            }
        } else if matches!(arena.tag(node), "input" | "textarea") {
            if let Some(paint) = text_inputs.and_then(|inputs| inputs.get(&node)) {
                // A text field's own text is cut at its padding box (an
                // ancestor's clip still applies): a textarea scrolls it, and
                // an input's typed or placeholder text may be longer than it.
                let Some(own_clip) =
                    padding_box_mask(buffer, (x, y, layout.width, layout.height), border, clip)
                else {
                    return;
                };
                let clip = Some(&own_clip);
                let content_x = content_x - paint.scroll.0 * scale_factor;
                let content_y = content_y - paint.scroll.1 * scale_factor;
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
                let declared = [style
                    .filter(|s| s.text_decoration.lines.any() && s.visibility.is_visible())
                    .map(|s| decoration::Declared {
                        decoration: s.text_decoration,
                        color: s.color,
                        font_size: s.font_size,
                    })];
                let effective = [vec![0]];
                let decorate = |buffer: &mut Surface, phase| {
                    decoration::paint(
                        buffer,
                        &paint.runs,
                        &declared,
                        &effective,
                        (content_x, content_y),
                        scale_factor,
                        clip,
                        phase,
                    );
                };
                decorate(buffer, decoration::Phase::UnderAndOver);
                text_shadow::paint(
                    buffer,
                    &paint.runs,
                    (content_x, content_y),
                    style.map_or(&[][..], |s| &s.text_shadow[..]),
                    scale_factor,
                    clip,
                );
                paint_shaped_runs(
                    buffer,
                    &paint.runs,
                    content_x,
                    content_y,
                    color,
                    scale_factor,
                    clip,
                );
                decorate(buffer, decoration::Phase::LineThrough);
                if let Some(placeholder) = &paint.placeholder {
                    paint_shaped_runs(
                        buffer,
                        placeholder,
                        content_x,
                        content_y,
                        style
                            .and_then(|s| s.placeholder_color)
                            .unwrap_or(PLACEHOLDER_COLOR),
                        scale_factor,
                        clip,
                    );
                }
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
                if paint.scrollbar.is_some() || paint.resizer {
                    paint_scrollbar_and_resizer(
                        buffer,
                        (x, y, layout.width, layout.height),
                        (
                            border.left.width,
                            border.top.width,
                            border.right.width,
                            border.bottom.width,
                        ),
                        paint.scrollbar,
                        paint.resizer,
                        scale_factor,
                        clip,
                    );
                }
                if let Some(hover) = paint.spinner {
                    paint_spinner(
                        buffer,
                        (x, y, layout.width, layout.height),
                        hover,
                        scale_factor,
                        clip,
                    );
                }
            }
        } else if arena.tag(node) == "svg" {
            if shown {
                paint_inline_svg(
                    buffer,
                    (arena, styles),
                    node,
                    (content_x, content_y, content_width, content_height),
                    scale_factor,
                    clip,
                );
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
        } else if !anonymous.is_empty() {
            // Runs of text and inline elements between block children: each is
            // an anonymous block, painted where layout put it, as a context
            // of its own.
            for &block in &anonymous {
                let Some(&at) = layouts.get(&block) else {
                    continue;
                };
                if let Some(shaped) = florui_layout::shape_anonymous_block(
                    font,
                    arena,
                    styles,
                    (layouts, scale_factor),
                    block,
                    self_wrap(at.width, scale_factor),
                ) {
                    let origin = (x + at.x, y + at.y);
                    let (runs, cuts) =
                        inline_runs_for(font, style, &shaped, at.width / scale_factor);
                    paint_inline_context(
                        buffer,
                        (arena, styles),
                        &shaped,
                        &runs,
                        &cuts,
                        origin,
                        color,
                        scale_factor,
                        clip,
                    );
                }
            }
        } else if florui_layout::is_inline_formatting_context(arena, styles, node) {
            // A real mixed text/inline-element node: rebuilt and
            // reshaped fresh here, since this crate doesn't share layout's
            // own internal Taffy tree — deterministic at the same final
            // width, the same reasoning `paint_text`'s own doc gives for
            // the plain single-style case below. Each inline element paints
            // its own box behind the text and its own color, shadow and
            // underline on it.
            if let Some(shaped) = florui_layout::shape_inline_formatting_context(
                font,
                arena,
                styles,
                (layouts, scale_factor),
                node,
                wrap_width,
            ) {
                let is_button = arena.tag(node) == "button";
                let (dx, dy) = text_offset(
                    is_button,
                    style,
                    (content_width, content_height),
                    (
                        shaped.layout.width * scale_factor,
                        shaped.layout.height * scale_factor,
                    ),
                );
                let origin = (content_x + dx, content_y + dy);
                let (runs, cuts) =
                    inline_runs_for(font, style, &shaped, content_width / scale_factor);
                paint_inline_context(
                    buffer,
                    (arena, styles),
                    &shaped,
                    &runs,
                    &cuts,
                    origin,
                    color,
                    scale_factor,
                    clip,
                );
            }
        } else {
            let text = arena.text_content(node);
            let is_button = arena.tag(node) == "button";
            let (dx, dy) = match style {
                Some(style) if !text.is_empty() && places_its_text(is_button, Some(style)) => {
                    let text_style = florui_layout::text_style_of(style);
                    let metrics = font.measure_wrapped(text_style, text, wrap_width);
                    text_offset(
                        is_button,
                        Some(style),
                        (content_width, content_height),
                        (metrics.width * scale_factor, metrics.height * scale_factor),
                    )
                }
                _ => (0.0, 0.0),
            };
            paint_node_text(
                buffer,
                font,
                arena,
                node,
                style,
                color,
                (content_x + dx, content_y + dy),
                (wrap_width, content_width / scale_factor),
                scale_factor,
                clip,
            );
        }
    }
}

/// Whether [`text_offset`] can move this node's text at all, so a node that
/// never needs it is not measured.
fn places_its_text(is_button: bool, style: Option<&ComputedStyle>) -> bool {
    style.is_some_and(|s| is_button || matches!(s.display, Display::Flex | Display::InlineFlex))
}

/// How far a node's own text sits from the content box's corner. In a flex
/// container the text is the only flex item, so `justify-content` and
/// `align-items` place it. Otherwise a `<button>` centers it vertically;
/// `text-align` is the text engine's.
fn text_offset(
    is_button: bool,
    style: Option<&ComputedStyle>,
    (content_width, content_height): (f32, f32),
    (text_width, text_height): (f32, f32),
) -> (f32, f32) {
    let Some(style) = style else {
        return (0.0, 0.0);
    };
    let free_x = (content_width - text_width).max(0.0);
    let free_y = (content_height - text_height).max(0.0);
    if !matches!(style.display, Display::Flex | Display::InlineFlex) {
        return (0.0, if is_button { free_y / 2.0 } else { 0.0 });
    }
    let (main_free, cross_free, row) = match style.flex_direction {
        FlexDirection::Row | FlexDirection::RowReverse => (free_x, free_y, true),
        FlexDirection::Column | FlexDirection::ColumnReverse => (free_y, free_x, false),
    };
    let reversed = matches!(
        style.flex_direction,
        FlexDirection::RowReverse | FlexDirection::ColumnReverse
    );
    let main = match style.justify_content {
        Some(
            ContentAlignment::Center
            | ContentAlignment::SpaceAround
            | ContentAlignment::SpaceEvenly,
        ) => main_free / 2.0,
        Some(ContentAlignment::End | ContentAlignment::FlexEnd) => main_free,
        _ => 0.0,
    };
    let main = if reversed && !matches!(style.justify_content, Some(ContentAlignment::Center)) {
        main_free - main
    } else {
        main
    };
    let cross = match style.align_items {
        Some(ItemAlignment::Center) => cross_free / 2.0,
        Some(ItemAlignment::End | ItemAlignment::FlexEnd) => cross_free,
        _ => 0.0,
    };
    if row { (main, cross) } else { (cross, main) }
}

#[allow(clippy::too_many_arguments)]
fn paint_node_text(
    buffer: &mut Surface,
    font: &mut Font,
    arena: &Tree,
    node: NodeId,
    style: Option<&ComputedStyle>,
    color: Rgba,
    (x, y): (f32, f32),
    (wrap_width, content_width): (f32, f32),
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let text = arena.text_content(node);
    if text.is_empty() {
        return;
    }
    paint_text(
        buffer,
        font,
        TextPaint {
            text,
            style: florui_layout::node_text_style(style),
            color,
            x,
            y,
            wrap_width,
            scale_factor,
            clip,
            shadows: style.map_or(&[][..], |s| &s.text_shadow[..]),
            ellipsis: style
                .and_then(text_overflow_mark)
                .map(|mark| (mark, content_width)),
            decoration: style
                .filter(|s| s.text_decoration.lines.any() && s.visibility.is_visible())
                .map(|s| decoration::Declared {
                    decoration: s.text_decoration,
                    color: s.color,
                    font_size: s.font_size,
                }),
        },
    );
}

/// What `text-overflow` puts where a line that is wider than its box is cut,
/// for a box that clips: the ellipsis, or the author's string.
fn text_overflow_mark(style: &ComputedStyle) -> Option<String> {
    if !style.overflow_clips {
        return None;
    }
    match &style.text_overflow {
        florui_style::TextOverflow::Clip => None,
        florui_style::TextOverflow::Ellipsis => Some("\u{2026}".to_owned()),
        florui_style::TextOverflow::Text(text) => Some(text.clone()),
    }
}

/// The glyph runs of an inline context, with its overflowing lines cut where
/// `text-overflow` says so, and for each line cut where its content now ends.
fn inline_runs_for<'a>(
    font: &mut Font,
    style: Option<&ComputedStyle>,
    inline: &'a florui_layout::InlineShaped,
    limit: f32,
) -> (
    std::borrow::Cow<'a, [florui_text::ShapedRun]>,
    Vec<(u32, f32)>,
) {
    let Some(mark) = style.and_then(text_overflow_mark) else {
        return (std::borrow::Cow::Borrowed(&inline.layout.runs), Vec::new());
    };
    let mut runs = inline.layout.runs.clone();
    let cuts = font.ellipsize_runs(
        &mut runs,
        florui_layout::node_text_style(style),
        &mark,
        limit,
        0,
    );
    if cuts.is_empty() {
        (std::borrow::Cow::Borrowed(&inline.layout.runs), cuts)
    } else {
        (std::borrow::Cow::Owned(runs), cuts)
    }
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
    blit_image(
        buffer,
        &source,
        content_x,
        content_y,
        rect,
        scale_factor,
        clip,
    );
}

/// [`paint_image`] of pixels that are premultiplied already.
fn blit_image(
    buffer: &mut Surface,
    source: &Pixmap,
    content_x: f32,
    content_y: f32,
    rect: florui_layout::BoxLayout,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
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
        (rect.width * scale_factor) / source.width() as f32,
        (rect.height * scale_factor) / source.height() as f32,
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

/// An inline svg's straight pixels, held weakly (they go when the renderer lets
/// them go, and the address stays this svg's while the pointer lives), and the
/// same premultiplied.
type InlineSvg = (std::rc::Weak<Vec<u8>>, std::rc::Rc<Pixmap>);

thread_local! {
    /// The inline svgs last drawn, premultiplied and ready to blit, each by the
    /// pixels it was made from, which the renderer hands out again while nothing
    /// about the svg changes.
    static INLINE_SVGS: std::cell::RefCell<Vec<InlineSvg>> = const { std::cell::RefCell::new(Vec::new()) };
}

const INLINE_SVGS_KEPT: usize = 16;

/// `rgba` of `size` as a premultiplied pixmap, made once for as long as the
/// renderer hands out the same pixels.
fn inline_svg_pixmap(rgba: &std::rc::Rc<Vec<u8>>, size: (u32, u32)) -> Option<std::rc::Rc<Pixmap>> {
    let this = std::rc::Rc::downgrade(rgba);
    if let Some(found) = INLINE_SVGS.with(|kept| {
        kept.borrow()
            .iter()
            .find(|(pixels, _)| std::rc::Weak::ptr_eq(pixels, &this))
            .map(|(_, pixmap)| std::rc::Rc::clone(pixmap))
    }) {
        return Some(found);
    }
    let image = florui_assets::RasterImage {
        rgba: rgba.to_vec(),
        width: size.0,
        height: size.1,
    };
    let pixmap = std::rc::Rc::new(premultiplied_pixmap(&image)?);
    INLINE_SVGS.with(|kept| {
        let mut kept = kept.borrow_mut();
        kept.retain(|(pixels, _)| pixels.strong_count() > 0);
        if kept.len() >= INLINE_SVGS_KEPT {
            kept.remove(0);
        }
        kept.push((this, std::rc::Rc::clone(&pixmap)));
    });
    Some(pixmap)
}

/// An inline `<svg>` drawn whole by the SVG renderer into its content box
/// (`x, y, width, height`, painted pixels), which holds the picture at one pixel
/// to each of its own.
fn paint_inline_svg(
    buffer: &mut Surface,
    (arena, styles): (&Tree, &StyleMap),
    node: NodeId,
    (x, y, width, height): (f32, f32, f32, f32),
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let size = (
        width.round().max(0.0) as u32,
        height.round().max(0.0) as u32,
    );
    if size.0 == 0 || size.1 == 0 {
        return;
    }
    let css = (width / scale_factor, height / scale_factor);
    let Some(rgba) = florui_svg::render_svg(arena, styles, node, css, size) else {
        return;
    };
    let Some(pixmap) = inline_svg_pixmap(&rgba, size) else {
        return;
    };
    let rect = florui_layout::BoxLayout {
        x: 0.0,
        y: 0.0,
        width: size.0 as f32 / scale_factor,
        height: size.1 as f32 / scale_factor,
        margin: florui_layout::BoxMargin::ZERO,
    };
    blit_image(buffer, &pixmap, x, y, rect, scale_factor, clip);
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
    // Each edge goes to the nearest pixel, as a browser snaps a box; the
    // rasterizer would cut the fraction off the position and the size instead.
    let x0 = lx.round().max(0.0);
    let y0 = ly.round().max(0.0);
    let x1 = (lx + width).round().max(0.0).min(buffer.width() as f32);
    let y1 = (ly + height).round().max(0.0).min(buffer.height() as f32);
    // A box that snaps to nothing paints nothing; the rasterizer would draw a
    // hairline for an empty rect.
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let Some(rect) = Rect::from_ltrb(x0, y0, x1, y1) else {
        return;
    };

    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = false;
    let clip = buffer.mask_for(clip, (x0, y0, x1, y1));
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

/// The four sides of a border as quadrilaterals, top first: the diagonals
/// run from each outer corner through the matching inner one.
fn side_wedges(outer: &RoundedRect, inner: &RoundedRect) -> [[(f32, f32); 4]; 4] {
    let (x0, y0) = (outer.x, outer.y);
    let (x1, y1) = (outer.x + outer.width, outer.y + outer.height);
    let (ix0, iy0) = (inner.x, inner.y);
    let (ix1, iy1) = (inner.x + inner.width, inner.y + inner.height);
    // The sides meet along the diagonal from each outer corner through its
    // inner one, carried on as far in as the corner is round: a rounded
    // corner's ring lies along that line, further in than the border is wide.
    let reach = outer
        .radii
        .iter()
        .fold(0.0f32, |most, &(h, v)| most.max(h).max(v));
    let along = |from: (f32, f32), through: (f32, f32)| {
        let (dx, dy) = (through.0 - from.0, through.1 - from.1);
        let step = dx.abs().max(dy.abs()).max(1.0e-3);
        let scale = 1.0 + reach / step;
        (from.0 + dx * scale, from.1 + dy * scale)
    };
    let (top_left, top_right) = (along((x0, y0), (ix0, iy0)), along((x1, y0), (ix1, iy0)));
    let (bottom_right, bottom_left) = (along((x1, y1), (ix1, iy1)), along((x0, y1), (ix0, iy1)));
    [
        [(x0, y0), (x1, y0), top_right, top_left],
        [(x1, y0), (x1, y1), bottom_right, top_right],
        [(x1, y1), (x0, y1), bottom_left, bottom_right],
        [(x0, y1), (x0, y0), top_left, bottom_left],
    ]
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

    let wedges = side_wedges(outer, &inner);
    // Each side is the ring cut to its wedge. The cut is a mask the size of the
    // area the border covers, in a layer of that size, so a small box costs a
    // small mask and not four the size of the window; the layer is then put over
    // the buffer through the ancestors' clip.
    let (local_x0, local_y0) = buffer.local(outer.x, outer.y);
    let rx0 = (local_x0.floor() as i32).max(0);
    let ry0 = (local_y0.floor() as i32).max(0);
    let rx1 = ((local_x0 + outer.width).ceil() as i32).min(buffer.width() as i32);
    let ry1 = ((local_y0 + outer.height).ceil() as i32).min(buffer.height() as i32);
    if rx1 <= rx0 || ry1 <= ry0 {
        return;
    }
    let (width, height) = ((rx1 - rx0) as u32, (ry1 - ry0) as u32);
    let Some(mut layer) = Surface::new(
        width,
        height,
        (buffer.origin.0 + rx0, buffer.origin.1 + ry0),
    ) else {
        return;
    };
    let layer_transform = surface_transform(&layer);
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
        let Some(mut mask) = Mask::new(width, height) else {
            continue;
        };
        mask.fill_path(&wedge, FillRule::Winding, true, layer_transform);
        let mut paint = Paint::default();
        paint.set_color_rgba8(side.color.r, side.color.g, side.color.b, side.color.a);
        paint.anti_alias = true;
        layer.pixmap.fill_path(
            &ring,
            &paint,
            FillRule::EvenOdd,
            layer_transform,
            Some(&mask),
        );
    }
    draw_shifted(
        &mut buffer.pixmap,
        &layer.pixmap,
        (rx0, ry0),
        &PixmapPaint::default(),
        clip,
    );
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
/// [`paint_box_shadows`] under an ancestor's clip. A shadow reaches past its box,
/// so a box that sits half out of a scrolled or `overflow: hidden` ancestor
/// would paint its shadow over whatever is beyond that clip. When the clip does
/// not change anything where the shadows land they are painted directly;
/// otherwise they are painted into a layer just large enough to hold them, and
/// the layer is composited through the clip.
fn paint_box_shadows_clipped(
    buffer: &mut Surface,
    outline: &RoundedRect,
    border: florui_style::Edges<florui_style::BorderSide>,
    shadows: &[florui_style::BoxShadow],
    inset: bool,
    clip: Option<&Mask>,
) {
    let Some(mask) = clip else {
        return paint_box_shadows(buffer, outline, border, shadows, inset);
    };
    // Where these shadows can land: an inset one stays inside the box, an outer
    // one is its shape moved, grown by its spread and softened by its blur.
    let mut reach: Option<(f32, f32, f32, f32)> = None;
    for shadow in shadows
        .iter()
        .filter(|s| s.inset == inset && s.color.a != 0)
    {
        let (x0, y0, w, h) = if inset {
            (outline.x, outline.y, outline.width, outline.height)
        } else {
            let (x, y, w, h) =
                outset_shadow_extent(shadow, outline.x, outline.y, outline.width, outline.height);
            let soften = blur::kernel_radius(shadow.blur_radius / 2.0) as f32;
            (x - soften, y - soften, w + 2.0 * soften, h + 2.0 * soften)
        };
        reach = Some(match reach {
            None => (x0, y0, x0 + w, y0 + h),
            Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x0 + w), d.max(y0 + h)),
        });
    }
    let Some((rx0, ry0, rx1, ry1)) = reach else {
        return;
    };
    let (lx0, ly0) = buffer.local(rx0, ry0);
    let (lx1, ly1) = buffer.local(rx1, ry1);
    let x0 = (lx0.floor().max(0.0)) as i32;
    let y0 = (ly0.floor().max(0.0)) as i32;
    let x1 = (lx1.ceil() as i32).min(buffer.width() as i32);
    let y1 = (ly1.ceil() as i32).min(buffer.height() as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    if buffer
        .mask_for(Some(mask), (x0 as f32, y0 as f32, x1 as f32, y1 as f32))
        .is_none()
    {
        return paint_box_shadows(buffer, outline, border, shadows, inset);
    }
    let Some(mut layer) = Surface::new(
        (x1 - x0) as u32,
        (y1 - y0) as u32,
        (buffer.origin.0 + x0, buffer.origin.1 + y0),
    ) else {
        return;
    };
    paint_box_shadows(&mut layer, outline, border, shadows, inset);
    draw_shifted(
        &mut buffer.pixmap,
        &layer.pixmap,
        (x0, y0),
        &PixmapPaint::default(),
        Some(mask),
    );
}

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
        // Drawn in a layer the size of the shape, so the mask is that size and
        // not the buffer's. Its place and size come from the shape alone, not from
        // what part of the window is being painted, so the same shape is the same
        // pixels whichever part it is painted in.
        let (layer_x, layer_y) = (fill.x.floor() as i32 - 1, fill.y.floor() as i32 - 1);
        let layer_right = (fill.x + fill.width).ceil() as i32 + 1;
        let layer_bottom = (fill.y + fill.height).ceil() as i32 + 1;
        let (width, height) = (
            (layer_right - layer_x) as u32,
            (layer_bottom - layer_y) as u32,
        );
        let (x0, y0) = (layer_x - buffer.origin.0, layer_y - buffer.origin.1);
        if layer_right <= buffer.origin.0
            || layer_bottom <= buffer.origin.1
            || x0 >= buffer.width() as i32
            || y0 >= buffer.height() as i32
        {
            return;
        }
        let Some(mut layer) = Surface::new(width, height, (layer_x, layer_y)) else {
            return;
        };
        let transform = surface_transform(&layer);
        let Some(mut mask) = Mask::new(width, height) else {
            return;
        };
        match exclude.path() {
            Some(exclude_path) => {
                mask.fill_path(&exclude_path, FillRule::Winding, true, transform);
                mask.invert();
            }
            None => mask.data_mut().fill(255),
        }
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            shadow.color.r,
            shadow.color.g,
            shadow.color.b,
            shadow.color.a,
        );
        paint.anti_alias = true;
        layer
            .pixmap
            .fill_path(&path, &paint, FillRule::Winding, transform, Some(&mask));
        draw_shifted(
            &mut buffer.pixmap,
            &layer.pixmap,
            (x0, y0),
            &PixmapPaint::default(),
            None,
        );
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
        if shadow.inset { &padding } else { outline }
            .solid_interior()
            .map(|rect| (rect, shadow.inset)),
        |canvas_x, canvas_y| {
            if shadow.inset {
                outline_contains_pixel(&padding, canvas_x, canvas_y)
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
    florui_profile::count(florui_profile::Counter::ShadowBlurs, 1);
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
    florui_profile::count(florui_profile::Counter::ShadowBlurs, 1);
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
    coverage: Arc<Vec<u8>>,
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

    // The mask depends only on the buffer size, sigma and the shape relative
    // to the buffer, not on where the buffer lands on the canvas, so equal
    // shadows at whole-pixel offsets share one (see `shadow_cache`).
    let local = shape.translated(-(origin_x as f32), -(origin_y as f32));
    let key = shadow_cache::ShadowKey::new(width, height, sigma, &local);
    let coverage = shadow_cache::coverage(key, || blurred_coverage(width, height, sigma, &local));

    Some(BlurredShape {
        origin_x,
        origin_y,
        width,
        height,
        coverage,
    })
}

/// The blurred silhouette of `local` in a `width x height` buffer, one byte of
/// coverage a pixel.
///
/// A box far from the shape's corners and from the buffer's edges sees the same
/// straight edge all along it, so the columns between the two left and right
/// margins (and the rows between the top and bottom ones) are copies of one
/// another. A big shape is therefore blurred as a small one, with a stretch of
/// its middle taken out, and the middle put back by copying: the work follows
/// the rim of the shape, not its area.
fn blurred_coverage(width: u32, height: u32, sigma: f32, local: &RoundedRect) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let reach = local
        .radii
        .iter()
        .fold(0.0f32, |most, &(rx, ry)| most.max(rx).max(ry))
        + blur::kernel_radius(sigma) as f32;
    // How far in from a side of the buffer the straight edge is not yet the
    // same all along: the side's distance to the shape, then its corner's reach.
    let margin = |to_shape: f32| ((to_shape + reach).ceil() + 2.0).max(0.0) as usize;
    let (left, right) = (
        margin(local.x),
        margin(width as f32 - local.x - local.width),
    );
    let (top, bottom) = (
        margin(local.y),
        margin(height as f32 - local.y - local.height),
    );
    // Two columns (rows) are kept of the middle; the rest is taken out when
    // that is worth the trouble.
    let taken = |len: usize, near: usize, far: usize| {
        let spare = len.saturating_sub(near + far + 2);
        if spare >= MIDDLE_WORTH_TAKING {
            spare
        } else {
            0
        }
    };
    let (out_x, out_y) = (taken(w, left, right), taken(h, top, bottom));
    if out_x == 0 && out_y == 0 {
        return direct_coverage(width, height, sigma, local);
    }
    let small_shape = RoundedRect {
        width: local.width - out_x as f32,
        height: local.height - out_y as f32,
        ..*local
    };
    let (small_w, small_h) = (w - out_x, h - out_y);
    let small = direct_coverage(small_w as u32, small_h as u32, sigma, &small_shape);
    let mut coverage = vec![0u8; w * h];
    for y in 0..h {
        let from = if y < top {
            y
        } else if y >= h - bottom {
            y - out_y
        } else {
            top
        };
        let source = &small[from * small_w..(from + 1) * small_w];
        let row = &mut coverage[y * w..(y + 1) * w];
        row[..left].copy_from_slice(&source[..left]);
        row[left..w - right].fill(source[left]);
        row[w - right..].copy_from_slice(&source[small_w - right..]);
    }
    coverage
}

/// How many columns or rows of middle are worth taking out and putting back.
const MIDDLE_WORTH_TAKING: usize = 16;

/// [`blurred_coverage`] by blurring the whole buffer.
fn direct_coverage(width: u32, height: u32, sigma: f32, local: &RoundedRect) -> Vec<u8> {
    let mut coverage = vec![0u8; (width as usize) * (height as usize)];
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
        stamp_rounded(&mut coverage, width, height, local);
    }
    if !blur::box_blur_plane_in_place(&mut coverage, width, height, sigma) {
        blur::gaussian_blur_in_place(&mut coverage, width, height, sigma);
    }
    coverage
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
        Some(((x + 1.0, y + 1.0, x + width - 1.0, y + height - 1.0), false)),
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
        Some((
            (
                padding_x + 1.0,
                padding_y + 1.0,
                padding_x + padding_width - 1.0,
                padding_y + padding_height - 1.0,
            ),
            true,
        )),
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
    known: Option<((f32, f32, f32, f32), bool)>,
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
    let (surface_x, surface_y) = (buffer.origin.0 as i64, buffer.origin.1 as i64);
    let (mask_x, mask_y) = (blurred.origin_x as i64, blurred.origin_y as i64);
    let blend = ShadowBlend::new(color);

    // The mask columns that land on the surface, worked out once rather than
    // tested for every pixel.
    let first_column = (surface_x - mask_x).max(0);
    let end_column = (surface_x + surface_width - mask_x).min(blurred.width as i64);
    if first_column >= end_column {
        return;
    }
    let row_width = buffer.width();
    let pixels = buffer.pixmap.pixels_mut();

    for local_y in 0..blurred.height {
        let canvas_y = mask_y + local_y as i64;
        let buffer_y = canvas_y - surface_y;
        if buffer_y < 0 || buffer_y >= surface_height {
            continue;
        }
        let coverage_row =
            &blurred.coverage[(local_y * blurred.width) as usize..][..blurred.width as usize];
        // The columns of this row whose answer from `allowed` is already known
        // (the centers of those pixels lie in the rectangle `known` names).
        let (known_from, known_to, known_allowed) = match known {
            Some(((kx0, ky0, kx1, ky1), result))
                if (canvas_y as f32 + 0.5) >= ky0 && (canvas_y as f32 + 0.5) < ky1 =>
            {
                let from = ((kx0 - 0.5).ceil() as i64 - mask_x).clamp(first_column, end_column);
                let to = ((kx1 - 0.5).ceil() as i64 - mask_x).clamp(from, end_column);
                (from, to, result)
            }
            _ => (end_column, end_column, false),
        };
        let mut local_x = first_column;
        while local_x < end_column {
            let known_here = local_x >= known_from && local_x < known_to;
            if known_here && !known_allowed {
                // Nothing in the known stretch is drawn: on to its end.
                local_x = known_to;
                continue;
            }
            let column = local_x;
            local_x += 1;
            let local_x = column;
            // Checked first: most of a mask is zero, and `allowed` (a
            // rounded-outline test) is the dearest part of a pixel.
            let raw = coverage_row[local_x as usize];
            let coverage = if invert { 255 - raw } else { raw };
            if coverage == 0 {
                continue;
            }

            let canvas_x = mask_x + local_x;
            if !known_here && !allowed(canvas_x as f32, canvas_y as f32) {
                continue;
            }

            let index = (buffer_y as u32 * row_width + (canvas_x - surface_x) as u32) as usize;
            pixels[index] = blend.over(pixels[index], coverage);
        }
    }
}

/// [`blend_source_over`] for one shadow color: everything that depends only
/// on the color and the coverage is worked out once per coverage value, so a
/// pixel needs lookups and the destination's own arithmetic. Produces exactly
/// what [`blend_source_over`] does.
struct ShadowBlend {
    src_alpha: [u8; 256],
    src_red: [u8; 256],
    src_green: [u8; 256],
    src_blue: [u8; 256],
}

impl ShadowBlend {
    fn new(color: Rgba) -> Self {
        let mut blend = Self {
            src_alpha: [0; 256],
            src_red: [0; 256],
            src_green: [0; 256],
            src_blue: [0; 256],
        };
        for coverage in 0..=255u8 {
            let alpha = mul_div_255(color.a, coverage);
            let slot = coverage as usize;
            blend.src_alpha[slot] = alpha;
            blend.src_red[slot] = mul_div_255(color.r, alpha);
            blend.src_green[slot] = mul_div_255(color.g, alpha);
            blend.src_blue[slot] = mul_div_255(color.b, alpha);
        }
        blend
    }

    fn over(&self, dst: PremultipliedColorU8, coverage: u8) -> PremultipliedColorU8 {
        let slot = coverage as usize;
        let src_a = self.src_alpha[slot];
        let inv = 255 - src_a;
        let out_a = (src_a as u16 + mul_div_255(dst.alpha(), inv) as u16).min(255) as u8;
        let channel = |src: u8, dst: u8| -> u8 {
            let value = src as u16 + mul_div_255(dst, inv) as u16;
            value.min(out_a as u16) as u8
        };
        PremultipliedColorU8::from_rgba(
            channel(self.src_red[slot], dst.red()),
            channel(self.src_green[slot], dst.green()),
            channel(self.src_blue[slot], dst.blue()),
            out_a,
        )
        .unwrap_or(dst)
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
#[cfg(test)]
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
    style: florui_text::TextStyle,
    color: Rgba,
    x: f32,
    y: f32,
    wrap_width: f32,
    scale_factor: f32,
    clip: Option<&'a Mask>,
    shadows: &'a [florui_style::TextShadow],
    /// The mark a clipped line ends in, and the width of the box it is cut at.
    ellipsis: Option<(String, f32)>,
    /// The decoration the element declares, if any.
    decoration: Option<decoration::Declared>,
}

fn paint_text(buffer: &mut Surface, font: &mut Font, params: TextPaint<'_>) {
    let TextPaint {
        text,
        style,
        color,
        x,
        y,
        wrap_width,
        scale_factor,
        clip,
        shadows,
        ellipsis,
        decoration,
    } = params;
    let mut shaped = font.shape_wrapped(style, text, wrap_width);
    if let Some((mark, limit)) = &ellipsis {
        font.ellipsize_runs(&mut shaped.runs, style, mark, *limit, 0);
    }
    let declared = [decoration];
    let effective = [vec![0]];
    let decorate = |buffer: &mut Surface, phase| {
        decoration::paint(
            buffer,
            &shaped.runs,
            &declared,
            &effective,
            (x, y),
            scale_factor,
            clip,
            phase,
        );
    };
    decorate(buffer, decoration::Phase::UnderAndOver);
    text_shadow::paint(buffer, &shaped.runs, (x, y), shadows, scale_factor, clip);
    paint_shaped_runs(buffer, &shaped.runs, x, y, color, scale_factor, clip);
    decorate(buffer, decoration::Phase::LineThrough);
}

/// Where the glyphs of `node` and of what is in it land, as the alpha of a clear
/// surface like `buffer`: the box is painted again with nothing but its text,
/// in opaque black whatever its colour, which is what `background-clip: text`
/// cuts a background to.
#[allow(clippy::too_many_arguments)]
fn text_coverage(
    buffer: &Surface,
    region: background::Region,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    scale_factor: f32,
) -> Option<Surface> {
    let mut cover = region.surface(buffer)?;
    // Only the box and what is in it, as the styles of the same boxes with all
    // they paint but text taken away.
    let mut text_styles = StyleMap::new();
    let mut below = vec![node];
    while let Some(id) = below.pop() {
        if let Some(original) = styles.get(&id) {
            let mut style = original.clone();
            let style = &mut style;
            style.background_color = Rgba::TRANSPARENT;
            style.background_layers.clear();
            style.background_color_clip = florui_style::BackgroundBox::Border;
            style.box_shadow.clear();
            style.border_image = None;
            for side in [
                &mut style.border.top,
                &mut style.border.right,
                &mut style.border.bottom,
                &mut style.border.left,
            ] {
                side.color = Rgba::TRANSPARENT;
            }
            style.outline.color = Rgba::TRANSPARENT;
            style.text_shadow.clear();
            style.text_decoration = Default::default();
            style.color = Rgba::opaque(0, 0, 0);
            style.backdrop_filter.clear();
            style.glass = Default::default();
            style.opacity = 1.0;
            style.filter.clear();
            style.mix_blend_mode = BlendMode::Normal;
            text_styles.insert(id, style.clone());
        }
        below.extend_from_slice(arena.children(id));
    }
    paint_node(
        &mut cover,
        arena,
        &text_styles,
        layouts,
        font,
        node,
        scale_factor,
        None,
        None,
        None,
    );
    let content_clip = clip_for_children(arena, &text_styles, layouts, node, None, scale_factor);
    let child_display = text_styles.get(&node).map(|s| s.display);
    paint_nodes(
        &mut cover,
        arena,
        &text_styles,
        layouts,
        font,
        child_display,
        arena.children(node),
        scale_factor,
        content_clip,
        None,
        None,
    );
    Some(cover)
}

/// The same for the glyphs of an inline element and the elements in it, in the
/// lines of the context they flow in.
fn inline_text_coverage(
    buffer: &Surface,
    region: background::Region,
    inline: &florui_layout::InlineShaped,
    runs: &[florui_text::ShapedRun],
    element: usize,
    (origin_x, origin_y): (f32, f32),
    scale_factor: f32,
) -> Option<Surface> {
    let mut cover = region.surface(buffer)?;
    let within = |mut run_element: usize| {
        // Element `0` is the container, its own parent.
        while run_element != element && run_element != 0 {
            run_element = inline.parents[run_element];
        }
        run_element == element
    };
    let owned: Vec<florui_text::ShapedRun> = runs
        .iter()
        .filter(|run| within(run.element))
        .cloned()
        .collect();
    paint_shaped_runs(
        &mut cover,
        &owned,
        origin_x,
        origin_y,
        Rgba::opaque(0, 0, 0),
        scale_factor,
        None,
    );
    Some(cover)
}

/// The colour of an inline element's box, cut to the box its `background-clip` names.
fn paint_inline_color(
    buffer: &mut Surface,
    (x, y, width, height): (f32, f32, f32, f32),
    outline: &RoundedRect,
    (border, padding): (
        florui_style::Edges<florui_style::BorderSide>,
        florui_style::Edges<f32>,
    ),
    (color, to): (Rgba, florui_style::BackgroundBox),
    clip: Option<&Mask>,
    text: Option<&Pixmap>,
) {
    if to != florui_style::BackgroundBox::Border {
        background::fill_clipped(buffer, outline, border, padding, to, color, clip, text);
    } else if outline.is_square() {
        fill_rect(buffer, x, y, width, height, color, clip);
    } else {
        fill_rounded_rect(buffer, outline, color, clip);
    }
}

/// Whether an inline element is painted as a unit of its own, as a stacking
/// context is: it fades, filters, clips, masks or blends all of its fragments, and
/// what is in it, together.
fn paints_as_group(style: &ComputedStyle) -> bool {
    style.opacity < 1.0 || style.has_group_effect()
}

/// What an inline context paints as groups, and the way to paint it.
struct InlineEffects<'a> {
    arena: &'a Tree,
    styles: &'a StyleMap,
    inline: &'a florui_layout::InlineShaped,
    runs: &'a [florui_text::ShapedRun],
    cuts: &'a [(u32, f32)],
    origin: (f32, f32),
    color: Rgba,
    scale: f32,
    /// For each element, the nearest element (itself included) that is a group;
    /// `None` for the container and what is in no group.
    owners: Vec<Option<usize>>,
}

/// An inline context's elements, boxes, text and outlines: the whole of what the
/// context paints on `buffer`, the elements that are groups each by itself.
#[allow(clippy::too_many_arguments)]
fn paint_inline_context(
    buffer: &mut Surface,
    (arena, styles): (&Tree, &StyleMap),
    inline: &florui_layout::InlineShaped,
    runs: &[florui_text::ShapedRun],
    cuts: &[(u32, f32)],
    origin: (f32, f32),
    color: Rgba,
    scale: f32,
    clip: Option<&Mask>,
) {
    let mut owners: Vec<Option<usize>> = Vec::with_capacity(inline.nodes.len());
    for (element, node) in inline.nodes.iter().enumerate() {
        let group = element != 0 && styles.get(node).is_some_and(paints_as_group);
        owners.push(if group {
            Some(element)
        } else if element == 0 {
            None
        } else {
            owners[inline.parents[element]]
        });
    }
    let effects = InlineEffects {
        arena,
        styles,
        inline,
        runs,
        cuts,
        origin,
        color,
        scale,
        owners,
    };
    effects.paint_level(buffer, None, clip);
}

impl InlineEffects<'_> {
    /// What belongs to `level` (the elements whose nearest group is it), then the
    /// groups directly inside it.
    fn paint_level(&self, surface: &mut Surface, level: Option<usize>, clip: Option<&Mask>) {
        if level.is_none() && self.owners.iter().all(Option::is_none) {
            // No group at all: everything, as it always was.
            self.paint_elements(surface, &|_| true, self.runs, clip);
            return;
        }
        let own = |element: usize| self.owners.get(element).copied().flatten() == level;
        let mine: Vec<florui_text::ShapedRun> = self
            .runs
            .iter()
            .filter(|run| own(run.element))
            .cloned()
            .collect();
        self.paint_elements(surface, &own, &mine, clip);
        for root in 1..self.owners.len() {
            let parent_owner = self.owners[self.inline.parents[root]];
            if self.owners[root] == Some(root) && parent_owner == level {
                self.paint_group(surface, root, clip);
            }
        }
    }

    fn paint_elements(
        &self,
        surface: &mut Surface,
        include: &dyn Fn(usize) -> bool,
        runs: &[florui_text::ShapedRun],
        clip: Option<&Mask>,
    ) {
        paint_inline_boxes(
            surface,
            self.styles,
            self.inline,
            self.runs,
            self.cuts,
            self.origin,
            self.scale,
            clip,
            include,
        );
        paint_inline_runs(
            surface,
            self.styles,
            self.inline,
            runs,
            self.origin,
            self.color,
            self.scale,
            clip,
        );
        paint_inline_outlines(
            surface,
            self.styles,
            self.inline,
            self.cuts,
            self.origin,
            self.scale,
            clip,
            include,
        );
    }

    /// Whether `element` is `root` or in it.
    fn within(&self, mut element: usize, root: usize) -> bool {
        while element != root && element != 0 {
            element = self.inline.parents[element];
        }
        element == root
    }

    /// The border box of a fragment, `(x, y, width, height)` in painted pixels.
    fn fragment_box(&self, fragment: &florui_text::InlineFragment) -> Option<(f32, f32, f32, f32)> {
        let style = self.styles.get(&self.inline.nodes[fragment.element])?;
        let above = style.padding.top + style.border.top.width;
        let below = style.padding.bottom + style.border.bottom.width;
        Some((
            self.origin.0 + fragment.x * self.scale,
            self.origin.1 + (fragment.y - above) * self.scale,
            fragment.width * self.scale,
            (fragment.ascent + fragment.descent + above + below) * self.scale,
        ))
    }

    /// `root` and what is in it painted on a surface of their own, given the
    /// effects of `root`, and put on `surface`. The effects are of the first
    /// fragment's box, as in Chromium, and take in all of them.
    fn paint_group(&self, surface: &mut Surface, root: usize, clip: Option<&Mask>) {
        let Some(style) = self.styles.get(&self.inline.nodes[root]) else {
            return;
        };
        let mut first = None;
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for fragment in &self.inline.layout.fragments {
            if !self.within(fragment.element, root) {
                continue;
            }
            let Some((x, y, w, h)) = self.fragment_box(fragment) else {
                continue;
            };
            if fragment.element == root && first.is_none() {
                first = Some((x, y, w, h));
            }
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + w);
            y1 = y1.max(y + h);
        }
        let Some((fx, fy, fw, fh)) = first else {
            return;
        };
        let layout = florui_layout::BoxLayout {
            x: 0.0,
            y: 0.0,
            width: fw / self.scale,
            height: fh / self.scale,
            margin: florui_layout::BoxMargin {
                top: style.margin.top.unwrap_or(0.0),
                right: style.margin.right.unwrap_or(0.0),
                bottom: style.margin.bottom.unwrap_or(0.0),
                left: style.margin.left.unwrap_or(0.0),
            },
        };
        // Room for what a filter spreads, for the shadows of the elements, and for
        // glyphs that reach past their line.
        let shadow_reach = (1..self.owners.len())
            .filter(|&element| self.within(element, root))
            .filter_map(|element| self.styles.get(&self.inline.nodes[element]))
            .flat_map(|style| style.box_shadow.iter().filter(|shadow| !shadow.inset))
            .map(|shadow| {
                shadow.offset_x.abs().max(shadow.offset_y.abs())
                    + shadow.blur_radius
                    + shadow.spread_radius.max(0.0)
            })
            .fold(0.0, f32::max);
        let mut reach = 2.0 * self.scale
            + shadow_reach * self.scale
            + filter_inflation(&style.filter, self.scale);
        for function in &style.filter {
            if let FilterFunction::Url(url) = function {
                reach += svg_ref::filter_reach(
                    self.arena,
                    self.styles,
                    url,
                    (fw / self.scale, fh / self.scale),
                ) * self.scale;
            }
        }
        let (left, top) = surface.local(x0 - reach, y0 - reach);
        let (right, bottom) = surface.local(x1 + reach, y1 + reach);
        let gx0 = left.floor().max(0.0) as i32;
        let gy0 = top.floor().max(0.0) as i32;
        let gx1 = (right.ceil().min(surface.pixmap.width() as f32)) as i32;
        let gy1 = (bottom.ceil().min(surface.pixmap.height() as f32)) as i32;
        if gx1 <= gx0 || gy1 <= gy0 {
            return;
        }
        let Some(mut group) = Surface::new(
            (gx1 - gx0) as u32,
            (gy1 - gy0) as u32,
            (surface.origin.0 + gx0, surface.origin.1 + gy0),
        ) else {
            return;
        };
        self.paint_level(&mut group, Some(root), None);
        let node_abs = (fx, fy);
        svg_ref::apply_chain(
            self.arena,
            self.styles,
            &scaled_filters(&style.filter, self.scale),
            &mut group,
            node_abs,
            Some(&layout),
            self.scale,
            &mut |pixmap, functions| apply_filters(pixmap, functions),
        );
        apply_clip_path(
            &mut group,
            self.arena,
            self.styles,
            style,
            &layout,
            node_abs,
            self.scale,
        );
        mask::apply_mask(
            &mut group,
            self.arena,
            self.styles,
            style,
            &layout,
            node_abs,
            self.scale,
        );
        let paint = PixmapPaint {
            opacity: style.opacity,
            blend_mode: blend::to_tiny_skia(style.mix_blend_mode),
            ..Default::default()
        };
        draw_shifted(&mut surface.pixmap, &group.pixmap, (gx0, gy0), &paint, clip);
    }
}

/// What each inline element of `inline` draws behind its text: its background
/// and border, one box for each line it appears on. A box that continues on
/// the next line has no border, padding or rounded corners at that end, as CSS
/// slices a box across lines.
#[allow(clippy::too_many_arguments)]
fn paint_inline_boxes(
    buffer: &mut Surface,
    styles: &StyleMap,
    inline: &florui_layout::InlineShaped,
    runs: &[florui_text::ShapedRun],
    cuts: &[(u32, f32)],
    (origin_x, origin_y): (f32, f32),
    scale_factor: f32,
    clip: Option<&Mask>,
    include: &dyn Fn(usize) -> bool,
) {
    let square = (
        florui_style::LengthPercentage::default(),
        florui_style::LengthPercentage::default(),
    );
    for fragment in &inline.layout.fragments {
        if !include(fragment.element) {
            continue;
        }
        let Some(style) = styles.get(&inline.nodes[fragment.element]) else {
            continue;
        };
        if !style.visibility.is_visible() {
            continue;
        }
        // A line cut by `text-overflow` ends where its mark does.
        let mut fragment = *fragment;
        if let Some(&(_, end)) = cuts.iter().find(|(line, _)| *line == fragment.line) {
            if fragment.x >= end {
                continue;
            }
            fragment.width = fragment.width.min(end - fragment.x);
        }
        let fragment = &fragment;
        let mut border = style.border;
        let has_border = [border.top, border.right, border.bottom, border.left]
            .iter()
            .any(|side| side.width > 0.0);
        if style.background_color.a == 0
            && style.background_layers.is_empty()
            && !has_border
            && style.box_shadow.is_empty()
        {
            continue;
        }
        let mut padding = style.padding;
        let mut radius = style.border_radius;
        if !fragment.starts {
            border.left.width = 0.0;
            padding.left = 0.0;
            radius.top_left = square;
            radius.bottom_left = square;
        }
        if !fragment.ends {
            border.right.width = 0.0;
            padding.right = 0.0;
            radius.top_right = square;
            radius.bottom_right = square;
        }
        let above = padding.top + border.top.width;
        let below = padding.bottom + border.bottom.width;
        let x = origin_x + fragment.x * scale_factor;
        let y = origin_y + (fragment.y - above) * scale_factor;
        let width = fragment.width * scale_factor;
        let height = (fragment.ascent + fragment.descent + above + below) * scale_factor;
        let outline = RoundedRect::new(x, y, width, height, &radius, scale_factor);
        let border = scaled_border(border, scale_factor);
        // Outer shadows are behind everything the box paints, one for each fragment.
        paint_box_shadows_clipped(buffer, &outline, border, &style.box_shadow, false, clip);
        let color_clip = style.background_color_clip;
        let text_cover = (background::clips_to_text(&style.background_layers)
            || color_clip == florui_style::BackgroundBox::Text)
            .then(|| {
                let region = background::Region::of(buffer, &outline)?;
                inline_text_coverage(
                    buffer,
                    region,
                    inline,
                    runs,
                    fragment.element,
                    (origin_x, origin_y),
                    scale_factor,
                )
            })
            .flatten();
        if background::blends(&style.background_layers) {
            let color = style.background_color;
            background::paint_blended(
                buffer,
                &outline,
                border,
                scaled_padding(padding, scale_factor),
                &style.background_layers,
                scale_factor,
                clip,
                text_cover.as_ref().map(|cover| &cover.pixmap),
                &|group| {
                    if color.a == 0 {
                        return;
                    }
                    paint_inline_color(
                        group,
                        (x, y, width, height),
                        &outline,
                        (border, scaled_padding(padding, scale_factor)),
                        (color, color_clip),
                        None,
                        text_cover.as_ref().map(|cover| &cover.pixmap),
                    );
                },
            );
        } else {
            if style.background_color.a != 0 {
                paint_inline_color(
                    buffer,
                    (x, y, width, height),
                    &outline,
                    (border, scaled_padding(padding, scale_factor)),
                    (style.background_color, color_clip),
                    clip,
                    text_cover.as_ref().map(|cover| &cover.pixmap),
                );
            }
            if !style.background_layers.is_empty() {
                background::paint_layers(
                    buffer,
                    &outline,
                    border,
                    scaled_padding(padding, scale_factor),
                    &style.background_layers,
                    scale_factor,
                    clip,
                    text_cover.as_ref().map(|cover| &cover.pixmap),
                );
            }
        }
        paint_box_shadows_clipped(buffer, &outline, border, &style.box_shadow, true, clip);
        if has_border {
            paint_box_border(buffer, &outline, border, clip);
        }
    }
}

/// The outlines of the inline elements of a context, over their text: one ring
/// round the line boxes of each element.
#[allow(clippy::too_many_arguments)]
fn paint_inline_outlines(
    buffer: &mut Surface,
    styles: &StyleMap,
    inline: &florui_layout::InlineShaped,
    cuts: &[(u32, f32)],
    (origin_x, origin_y): (f32, f32),
    scale_factor: f32,
    clip: Option<&Mask>,
    include: &dyn Fn(usize) -> bool,
) {
    type Rects = Vec<(f32, f32, f32, f32)>;
    let mut boxes: Vec<(usize, Rects)> = Vec::new();
    for fragment in &inline.layout.fragments {
        if !include(fragment.element) {
            continue;
        }
        let Some(style) = styles.get(&inline.nodes[fragment.element]) else {
            continue;
        };
        if !border_style::has_outline(style) {
            continue;
        }
        let mut fragment = *fragment;
        if let Some(&(_, end)) = cuts.iter().find(|(line, _)| *line == fragment.line) {
            if fragment.x >= end {
                continue;
            }
            fragment.width = fragment.width.min(end - fragment.x);
        }
        let above = style.padding.top + style.border.top.width;
        let below = style.padding.bottom + style.border.bottom.width;
        let rect = (
            origin_x + fragment.x * scale_factor,
            origin_y + (fragment.y - above) * scale_factor,
            fragment.width * scale_factor,
            (fragment.ascent + fragment.descent + above + below) * scale_factor,
        );
        match boxes
            .iter_mut()
            .find(|(element, _)| *element == fragment.element)
        {
            Some((_, rects)) => rects.push(rect),
            None => boxes.push((fragment.element, vec![rect])),
        }
    }
    for (element, rects) in boxes {
        if let Some(style) = styles.get(&inline.nodes[element]) {
            border_style::paint_outline_rects(buffer, &style.outline, &rects, scale_factor, clip);
        }
    }
}

/// The glyph runs of `inline`, each in the color and shadow of its own
/// element, with the decorations the elements declare.
#[allow(clippy::too_many_arguments)]
fn paint_inline_runs(
    buffer: &mut Surface,
    styles: &StyleMap,
    inline: &florui_layout::InlineShaped,
    runs: &[florui_text::ShapedRun],
    (origin_x, origin_y): (f32, f32),
    container_color: Rgba,
    scale_factor: f32,
    clip: Option<&Mask>,
) {
    let declared: Vec<Option<decoration::Declared>> = inline
        .nodes
        .iter()
        .map(|node| {
            styles
                .get(node)
                .filter(|s| s.text_decoration.lines.any() && s.visibility.is_visible())
                .map(|s| decoration::Declared {
                    decoration: s.text_decoration,
                    color: s.color,
                    font_size: s.font_size,
                })
        })
        .collect();
    let effective = decoration::effective_lists(&declared, &inline.parents);
    let decorate = |buffer: &mut Surface, phase| {
        decoration::paint(
            buffer,
            runs,
            &declared,
            &effective,
            (origin_x, origin_y),
            scale_factor,
            clip,
            phase,
        );
    };
    decorate(buffer, decoration::Phase::UnderAndOver);
    let mut start = 0;
    while start < runs.len() {
        let element = runs[start].element;
        let mut end = start + 1;
        while end < runs.len() && runs[end].element == element {
            end += 1;
        }
        let group = &runs[start..end];
        let style = styles.get(&inline.nodes[element]);
        if style.is_some_and(|s| !s.visibility.is_visible()) {
            start = end;
            continue;
        }
        let color = style.map_or(container_color, |s| s.color);
        text_shadow::paint(
            buffer,
            group,
            (origin_x, origin_y),
            style.map_or(&[][..], |s| &s.text_shadow[..]),
            scale_factor,
            clip,
        );
        paint_shaped_runs(buffer, group, origin_x, origin_y, color, scale_factor, clip);
        start = end;
    }
    decorate(buffer, decoration::Phase::LineThrough);
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
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    // Runs a font has no bold for are drawn thickened, apart from the rest, each
    // at the stroke its own size gives.
    let split = runs.iter().any(|run| run.embolden);
    let fill = |buffer: &mut Surface, path: &tiny_skia::Path| {
        buffer
            .pixmap
            .fill_path(path, &paint, FillRule::Winding, Transform::identity(), clip);
    };
    let mut builder = PathBuilder::new();
    append_glyphs(
        &mut builder,
        runs,
        x,
        y,
        scale_factor,
        split.then_some(false),
    );
    if let Some(path) = builder.finish() {
        fill(buffer, &path);
    }
    for run in runs.iter().filter(|run| run.embolden) {
        let mut builder = PathBuilder::new();
        append_glyphs(
            &mut builder,
            std::slice::from_ref(run),
            x,
            y,
            scale_factor,
            Some(true),
        );
        let Some(path) = builder.finish() else {
            continue;
        };
        fill(buffer, &path);
        // As Skia thickens: the outline stroked as wide as a fraction of the size.
        let size = run.font_size * scale_factor;
        let ratio = 1.0 / 24.0 + (1.0 / 32.0 - 1.0 / 24.0) * ((size - 9.0) / 27.0).clamp(0.0, 1.0);
        let stroke = tiny_skia::Stroke {
            width: size * ratio,
            ..tiny_skia::Stroke::default()
        };
        buffer
            .pixmap
            .stroke_path(&path, &paint, &stroke, Transform::identity(), clip);
    }
}

/// The outline of glyph `glyph` of `run` at `size`, from `instance` or, the
/// first time, from the font (parsed once into `outlines` for the run).
pub(crate) fn glyph_outline<'a>(
    instance: &glyph_cache::Instance,
    run: &'a florui_text::ShapedRun,
    glyph: u32,
    size: f32,
    outlines: &mut Option<Option<skrifa::outline::OutlineGlyphCollection<'a>>>,
) -> std::rc::Rc<[glyph_cache::Segment]> {
    instance.outline(glyph, || {
        let outlines = outlines.get_or_insert_with(|| {
            FontRef::from_index(run.font.data.data(), run.font.index)
                .ok()
                .map(|font_ref| font_ref.outline_glyphs())
        });
        let Some(outline) = outlines
            .as_ref()
            .and_then(|outlines| outlines.get(GlyphId::new(glyph)))
        else {
            return Vec::new();
        };
        // Without the run's coordinates, every glyph draws at the font's
        // default variable instance regardless of what was actually shaped: a
        // bold run would measure wider (Parley resolves the wght axis correctly
        // for layout) but paint no bolder at all.
        let coords: Vec<NormalizedCoord> = run
            .normalized_coords
            .iter()
            .map(|&bits| NormalizedCoord::from_bits(bits))
            .collect();
        let mut recorder = glyph_cache::Recorder::default();
        let settings = DrawSettings::unhinted(GlyphSize::new(size), LocationRef::new(&coords));
        let _ = outline.draw(settings, &mut recorder);
        recorder.0
    })
}

/// Appends every glyph of `runs` to `builder`, positioned from `(x, y)` and
/// scaled by `scale_factor`, through the glyph outline cache.
fn append_glyphs(
    builder: &mut PathBuilder,
    runs: &[florui_text::ShapedRun],
    x: f32,
    y: f32,
    scale_factor: f32,
    only: Option<bool>,
) {
    #[cfg(test)]
    if DIRECT_GLYPHS.with(std::cell::Cell::get) {
        return append_glyphs_direct(builder, runs, x, y, scale_factor);
    }
    for run in runs {
        if only.is_some_and(|bold| bold != run.embolden) {
            continue;
        }
        let size = run.font_size * scale_factor;
        let instance = glyph_cache::instance(
            run.font.data.id(),
            run.font.index,
            size,
            &run.normalized_coords,
        );
        // The font is parsed and the outlines extracted only for a glyph the
        // cache has not seen at this size yet.
        let mut outlines = None;
        for glyph in &run.glyphs {
            let segments = glyph_outline(&instance, run, glyph.id, size, &mut outlines);
            glyph_cache::replay(
                builder,
                x + glyph.x * scale_factor,
                y + glyph.y * scale_factor,
                &segments,
                run.skew,
            );
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Lets a test paint with outlines extracted directly each time, the
    /// reference the cache has to match to the pixel.
    static DIRECT_GLYPHS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What [`append_glyphs`] replaced: every glyph's outline drawn straight into
/// the path.
#[cfg(test)]
fn append_glyphs_direct(
    builder: &mut PathBuilder,
    runs: &[florui_text::ShapedRun],
    x: f32,
    y: f32,
    scale_factor: f32,
) {
    for run in runs {
        let Ok(font_ref) = FontRef::from_index(run.font.data.data(), run.font.index) else {
            continue;
        };
        let outlines = font_ref.outline_glyphs();
        let size = GlyphSize::new(run.font_size * scale_factor);
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
                builder,
                origin_x: x + glyph.x * scale_factor,
                origin_y: y + glyph.y * scale_factor,
            };
            let settings = DrawSettings::unhinted(size, location);
            let _ = outline.draw(settings, &mut pen);
        }
    }
}

/// Feeds a glyph's outline (font units, Y-up from its own baseline origin)
/// into a [`PathBuilder`] (screen space, Y-down), offsetting by the
/// glyph's pen position and flipping the Y axis for every emitted point.
#[cfg(test)]
struct GlyphPen<'a> {
    builder: &'a mut PathBuilder,
    origin_x: f32,
    origin_y: f32,
}

#[cfg(test)]
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
    /// A scroll box at fractional edges holding rows with opaque and translucent
    /// backgrounds, borders on every side, rounded corners, text, and rows cut by
    /// the clip's top and bottom edges. Painted with and without the shortcut that
    /// drops the clip mask from a fill lying wholly inside it. Returns the canvas
    /// and how many fills dropped it.
    fn clip_scene(shortcut: bool, rounded_clip: bool, scale: f32) -> (Canvas, u32) {
        let div = |class: &str, children: Vec<Element>| {
            Element::node("div", vec![("class".into(), class.into())], children)
        };
        let rows: Vec<Element> = (0..14)
            .map(|i| {
                let class = match i % 4 {
                    0 => "row opaque",
                    1 => "row glass",
                    2 => "row framed",
                    _ => "row round",
                };
                div(class, vec![Element::text(format!("row {i}"))])
            })
            .collect();
        let tree = div(
            "page",
            vec![div(if rounded_clip { "clip curved" } else { "clip" }, rows)],
        );
        let css = "
            .page { width: 260px; display: flex; flex-direction: column; }
            .clip { margin-left: 10.4px; margin-top: 20.6px; width: 150.3px; height: 90.7px;
                    overflow: hidden; display: flex; flex-direction: column; }
            .curved { border-radius: 14px; }
            .row { height: 17.3px; flex-shrink: 0; margin: 1.4px 3px 0 3px; color: #eee; font-size: 11px; }
            .row:first-child { margin-top: -9.2px; }
            .clip > .row:last-child { margin-bottom: 0; }
            .opaque { background-color: #334455; }
            .glass { background-color: rgba(200, 80, 80, 0.5); }
            .framed { background-color: #223322; border: 2px solid #ffaa00;
                      border-bottom: 1.5px solid rgba(255, 255, 255, 0.6); }
            .round { background-color: #552255; border-radius: 6px; }
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
        SOLID_CLIP_OFF.with(|off| off.set(!shortcut));
        SOLID_CLIP_DROPS.with(|drops| drops.set(0));
        let canvas = paint_to_buffer(
            &mut font,
            (260.0 * scale) as u32,
            (150.0 * scale) as u32,
            Rgba::opaque(10, 10, 12),
            &arena,
            &styles,
            &layouts,
            scale,
        );
        SOLID_CLIP_OFF.with(|off| off.set(false));
        (canvas, SOLID_CLIP_DROPS.with(Cell::get))
    }

    #[test]
    fn dropping_the_clip_mask_from_a_fill_inside_it_changes_no_pixel() {
        for rounded_clip in [false, true] {
            for scale in [1.0, 1.5] {
                let (with, drops) = clip_scene(true, rounded_clip, scale);
                let (without, _) = clip_scene(false, rounded_clip, scale);
                let differing: Vec<usize> = with
                    .data()
                    .chunks(4)
                    .zip(without.data().chunks(4))
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(i, _)| i)
                    .collect();
                let width = with.width() as usize;
                assert!(
                    differing.is_empty(),
                    "rounded clip: {rounded_clip}, scale {scale}: {} pixels differ, the first at (x {}, y {}): {:?} with the shortcut, {:?} without",
                    differing.len(),
                    differing[0] % width,
                    differing[0] / width,
                    &with.data()[differing[0] * 4..differing[0] * 4 + 4],
                    &without.data()[differing[0] * 4..differing[0] * 4 + 4],
                );
                if rounded_clip {
                    assert_eq!(drops, 0, "a rounded clip's corners need the mask");
                } else {
                    assert!(drops > 0, "scale {scale}: the shortcut was never taken");
                }
            }
        }
    }

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
    fn assert_bounded_and_full_target_paint_identically(paint: impl FnMut() -> Canvas) {
        assert_bounded_and_full_target_paint_within(0, paint);
    }

    /// The same check for a group that is sampled between its pixels, where
    /// the two surfaces round the sample position differently by up to
    /// `tolerance` of a channel level.
    fn assert_bounded_and_full_target_paint_within(
        tolerance: u8,
        mut paint: impl FnMut() -> Canvas,
    ) {
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
                let close = a.red().abs_diff(b.red()) <= tolerance
                    && a.green().abs_diff(b.green()) <= tolerance
                    && a.blue().abs_diff(b.blue()) <= tolerance
                    && a.alpha().abs_diff(b.alpha()) <= tolerance;
                assert!(
                    close,
                    "pixel ({x}, {y}): {:?} against {:?} differs between a bounded group surface and a \
                     target-sized one — bounding changed real output, not just its cost",
                    (a.red(), a.green(), a.blue(), a.alpha()),
                    (b.red(), b.green(), b.blue(), b.alpha()),
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
    fn display_none_paints_nothing_for_the_node_or_its_text_or_children() {
        let tree: Element =
            view! { <div class="gone">{"Hello"}<div class="inner">{"World"}</div></div> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".gone { display: none; background-color: #ffffff; color: #ffffff; } \
             .inner { background-color: #ffffff; color: #ffffff; }",
        )
        .unwrap();
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
            60,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        let lit = (0..60)
            .flat_map(|x| (0..40).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel_rgb(&buffer, x, y) != [0, 0, 0])
            .count();
        assert_eq!(lit, 0);
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
    fn a_selects_label_follows_the_letter_spacing_and_line_height_of_its_style() {
        let ink = |extra: &str| {
            let tree: Element = view! { <select class="sel">{"Medium"}</select> };
            let buffer = paint_control_icon(
                tree,
                &format!(
                    ".sel {{ width: 200px; height: 100px; border: none; padding: 0px; \
                     background-color: transparent; color: #ffffff; {extra} }}"
                ),
                200,
                100,
            );
            let lit: Vec<(u32, u32)> = (0..170)
                .flat_map(|x| (0..100).map(move |y| (x, y)))
                .filter(|&(x, y)| pixel_rgb(&buffer, x, y) != [0, 0, 0])
                .collect();
            let right = lit.iter().map(|&(x, _)| x).max().unwrap_or(0);
            let top = lit.iter().map(|&(_, y)| y).min().unwrap_or(0);
            (right, top)
        };
        let (plain_right, plain_top) = ink("");
        let (spaced_right, _) = ink("letter-spacing: 6px;");
        let (_, tall_top) = ink("line-height: 60px;");
        assert!(
            spaced_right >= plain_right + 20,
            "six letters spaced 6px apart: {plain_right} -> {spaced_right}"
        );
        assert!(
            tall_top >= plain_top + 15,
            "a 60px line puts the text lower: {plain_top} -> {tall_top}"
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
                        margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            front,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                    margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                    margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                    margin: florui_layout::BoxMargin::ZERO,
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
    fn paint_order_follows_order_within_a_z_index_level_and_only_for_flex_and_grid_items() {
        let tree: Element = view! {
            <div class="row">
                <div class="a" />
                <div class="b" />
                <div class="c" />
                <div class="d" />
            </div>
        };
        // `d` has the highest z-index, so the highest level whatever its order;
        // `a`, `b` and `c` share one and go by order, `b` and `c` tying.
        let css = ".row { display: flex; } .a { order: 5; } .b { order: 1; } .c { order: 1; } \
                   .d { order: -9; z-index: 1; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let children = arena.children(arena.roots()[0]).to_vec();
        let (a, b, c, d) = (children[0], children[1], children[2], children[3]);

        let flex = paint_order(&styles, Some(florui_style::Display::Flex), &children);
        assert_eq!(flex, vec![b, c, a, d]);
        let grid = paint_order(&styles, Some(florui_style::Display::Grid), &children);
        assert_eq!(grid, vec![b, c, a, d]);
        // In a block the property means nothing.
        let block = paint_order(&styles, Some(florui_style::Display::Block), &children);
        assert_eq!(block, children);
    }

    #[test]
    fn an_absolutely_positioned_child_of_a_flex_container_stacks_as_order_zero() {
        let tree: Element = view! {
            <div class="row">
                <div class="abs" />
                <div class="item" />
            </div>
        };
        // Chromium paints the item over the absolute child whatever the
        // child's `order`: the child counts as 0, below the item at 1.
        let css = ".row { display: flex; } .abs { position: absolute; order: 10; } \
                   .item { position: relative; order: 1; }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let children = arena.children(arena.roots()[0]).to_vec();
        let ordered = paint_order(&styles, Some(florui_style::Display::Flex), &children);
        assert_eq!(ordered, children);
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
                margin: florui_layout::BoxMargin::ZERO,
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
    fn a_hidden_box_paints_nothing_of_its_own_but_a_visible_child_still_paints() {
        let tree: Element = view! {
            <div class="card">
                <div class="hid">
                    <div class="back" />
                </div>
            </div>
        };
        let css = "
            .card { width: 40px; height: 20px; background-color: #1e1e22; }
            .hid { visibility: hidden; width: 20px; height: 20px; background-color: #ff0000;
                   border: 2px solid #00ff00; box-shadow: 0 0 0 4px #ff00ff; }
            .back { visibility: visible; width: 8px; height: 8px; background-color: #0000ff; }
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
            20,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        let card = [0x1e, 0x1e, 0x22];
        assert_eq!(
            pixel_rgb(&buffer, 15, 15),
            card,
            "background of the hidden box"
        );
        assert_eq!(pixel_rgb(&buffer, 0, 10), card, "border of the hidden box");
        assert_eq!(pixel_rgb(&buffer, 26, 10), card, "shadow of the hidden box");
        assert_eq!(pixel_rgb(&buffer, 5, 5), [0, 0, 0xff], "the visible child");
    }

    #[test]
    fn a_fractional_width_is_painted_rounded_like_a_browser_does() {
        let tree: Element = view! { <div class="a" /> };
        let css = ".a { width: 78.945px; height: 10px; background-color: #ff0000; }";
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
            10,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        let red = (0..100)
            .filter(|&x| pixel_rgb(&buffer, x, 5) == [0xff, 0, 0])
            .count();
        assert_eq!(red, 79);
    }

    #[test]
    fn a_fixed_box_paints_over_later_siblings_and_outside_its_clipping_parent() {
        let tree: Element = view! {
            <div class="page">
                <div class="clip">
                    <div class="pinned" />
                </div>
                <div class="later" />
            </div>
        };
        let css = "
            .page { width: 60px; height: 40px; background-color: #000000; }
            .clip { width: 10px; height: 10px; overflow: hidden; }
            .pinned { position: fixed; top: 20px; left: 30px; width: 20px; height: 10px;
                      background-color: #ff0000; }
            .later { width: 60px; height: 40px; background-color: #00ff00; }
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
        let layouts = florui_layout::compute_layout(
            &mut font,
            &arena,
            &styles,
            Size {
                width: taffy::AvailableSpace::Definite(60.0),
                height: taffy::AvailableSpace::Definite(40.0),
            },
        )
        .unwrap();
        let buffer = paint_to_buffer(
            &mut font,
            60,
            40,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        assert_eq!(
            pixel_rgb(&buffer, 35, 25),
            [0xff, 0, 0],
            "over the later box"
        );
        assert_eq!(
            pixel_rgb(&buffer, 5, 35),
            [0, 0xff, 0],
            "the later box elsewhere"
        );
    }

    #[test]
    fn hidden_text_is_not_drawn_alone_or_as_an_inline_element() {
        let paint = |tree: Element, css: &str| {
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
            paint_to_buffer(
                &mut font,
                120,
                30,
                Rgba::opaque(0, 0, 0),
                &arena,
                &styles,
                &layouts,
                1.0,
            )
        };
        let inked = |buffer: &_, x0: u32, x1: u32| {
            (0..30).any(|y| (x0..x1).any(|x| pixel_rgb(buffer, x, y) != [0, 0, 0]))
        };
        let css = "p { color: #ffffff; font-size: 20px; margin: 0; } \
                   .gone { visibility: hidden; }";
        let alone: Element = view! { <p class="gone">"MMMM"</p> };
        assert!(
            !inked(&paint(alone, css), 0, 120),
            "hidden text paints nothing"
        );

        let inline: Element = view! { <p>"MM"<span class="gone">"WWWW"</span>"MM"</p> };
        let buffer = paint(inline, css);
        let visible: Element = view! { <p>"MM"<span>"WWWW"</span>"MM"</p> };
        let reference = paint(visible, css);
        // "MM" ends near x = 50 at this size; the span follows it.
        assert!(
            inked(&buffer, 0, 45),
            "the text before the hidden span is drawn"
        );
        assert!(
            inked(&reference, 55, 120),
            "the span would be drawn if visible"
        );
        assert!(
            !inked(&buffer, 55, 120),
            "the hidden span's glyphs are not drawn"
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            back,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 30.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            front,
            BoxLayout {
                x: 10.0,
                y: 10.0,
                width: 30.0,
                height: 30.0,
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                    margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            content,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            inner,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 15.0,
                height: 15.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            content,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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

    /// A red "H" of 40px in a 20px-padded box, painted with `css` on top of the
    /// base rules; the canvas is the box. Returns the canvas.
    fn render_shadowed_h(extra_css: &str) -> Canvas {
        let tree: Element = view! { <div class="wrap"><h2>{"H"}</h2></div> };
        let css = format!(
            ".wrap {{ padding: 24px; }} h2 {{ margin: 0; color: #ff0000; font-size: 40px; {extra_css} }}"
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
        let mut font = Font::load_embedded();
        let layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let node = arena.roots()[0];
        paint_to_buffer(
            &mut font,
            layouts[&node].width.ceil() as u32,
            layouts[&node].height.ceil() as u32,
            Rgba::TRANSPARENT,
            &arena,
            &styles,
            &layouts,
            1.0,
        )
    }

    /// The box around the pixels `matches` accepts, as `(x0, y0, x1, y1)`.
    fn ink_box(buffer: &Canvas, matches: impl Fn([u8; 4]) -> bool) -> Option<(u32, u32, u32, u32)> {
        let mut found: Option<(u32, u32, u32, u32)> = None;
        for y in 0..buffer.height() {
            for x in 0..buffer.width() {
                if matches(rgba(buffer, x, y)) {
                    found = Some(match found {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        found
    }

    fn is_red(p: [u8; 4]) -> bool {
        p[0] > 200 && p[1] < 40 && p[2] < 40 && p[3] > 200
    }

    fn is_green(p: [u8; 4]) -> bool {
        p[1] > 200 && p[0] < 40 && p[2] < 40 && p[3] > 200
    }

    #[test]
    fn a_text_shadow_is_the_glyphs_again_moved_by_its_offsets() {
        let plain = render_shadowed_h("");
        assert!(ink_box(&plain, is_green).is_none(), "no shadow, no green");
        let shadowed = render_shadowed_h("text-shadow: 7px 5px #00ff00;");
        let red = ink_box(&shadowed, is_red).expect("the text");
        assert_eq!(
            red,
            ink_box(&plain, is_red).expect("the text"),
            "the text itself does not move"
        );
        let green = ink_box(&shadowed, is_green).expect("the shadow");
        // The shadow is the same "H", seven to the right and five down; the
        // anti-aliased edges leave a pixel of play.
        for (shadow, text, shift) in [
            (green.0, red.0, 7),
            (green.1, red.1, 5),
            (green.2, red.2, 7),
            (green.3, red.3, 5),
        ] {
            assert!(
                shadow.abs_diff(text + shift) <= 1,
                "{green:?} against {red:?}"
            );
        }
        // Negative offsets go the other way.
        let back = render_shadowed_h("text-shadow: -6px -4px #00ff00;");
        let green = ink_box(&back, is_green).expect("the shadow");
        assert!(
            green.0.abs_diff(red.0 - 6) <= 1 && green.1.abs_diff(red.1 - 4) <= 1,
            "{green:?}"
        );
    }

    #[test]
    fn the_text_is_drawn_over_its_shadow() {
        // A shadow with no offset is hidden behind the glyphs it copies.
        let page = render_shadowed_h("text-shadow: 0 0 #00ff00;");
        let plain = render_shadowed_h("");
        let (x0, y0, x1, y1) = ink_box(&page, is_red).expect("the text");
        let mut solid = 0;
        for y in y0..=y1 {
            for x in x0..=x1 {
                if is_red(rgba(&plain, x, y)) {
                    assert!(is_red(rgba(&page, x, y)), "ink at ({x}, {y}) stayed on top");
                    solid += 1;
                }
            }
        }
        assert!(solid > 50, "the glyph has body: {solid}");
    }

    #[test]
    fn a_blurred_shadow_spreads_soft_beyond_the_glyphs() {
        let hard = render_shadowed_h("text-shadow: 1px 1px #0000ff;");
        let soft = render_shadowed_h("text-shadow: 0 0 10px #0000ff;");
        let text = ink_box(&soft, is_red).expect("the text");
        let bluish = |p: [u8; 4]| p[2] > 8 && p[0] < 100 && p[3] > 8;
        let halo = ink_box(&soft, bluish).expect("the blur");
        // The blur reaches about its radius past the glyphs, on every side.
        assert!(
            halo.0 + 6 <= text.0 && halo.2 >= text.2 + 6,
            "{halo:?} around {text:?}"
        );
        assert!(
            halo.1 + 6 <= text.1 && halo.3 >= text.3 + 6,
            "{halo:?} around {text:?}"
        );
        // About twice its sigma of 5 at the visible threshold, never the 20 or so a
        // radius mistaken for the sigma would give.
        for reach in [
            text.0 - halo.0,
            halo.2 - text.2,
            text.1 - halo.1,
            halo.3 - text.3,
        ] {
            assert!((6..=16).contains(&reach), "the halo reaches {reach}px");
        }
        // Soft: there are pixels that are partly blue, not only none or all.
        let partial = (0..soft.height())
            .flat_map(|y| (0..soft.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                (40..=200).contains(&rgba(&soft, x, y)[3]) && rgba(&soft, x, y)[2] > 0
            })
            .count();
        assert!(partial > 100, "a gradient of coverage: {partial}");
        // The same shadow with no blur is a hard copy: opaque blue, nothing soft.
        let hard_blue = |p: [u8; 4]| p[2] > 200 && p[0] < 40 && p[3] > 200;
        assert!(ink_box(&hard, hard_blue).is_some() || ink_box(&hard, |p| p[3] > 0).is_some());
    }

    #[test]
    fn the_first_shadow_is_on_top() {
        let is_blue = |p: [u8; 4]| p[2] > 200 && p[0] < 40 && p[1] < 40 && p[3] > 200;
        let page = render_shadowed_h("text-shadow: 8px 8px #00ff00, 8px 8px #0000ff;");
        assert!(ink_box(&page, is_green).is_some(), "the first shadow shows");
        assert!(
            ink_box(&page, is_blue).is_none(),
            "the second is entirely under it"
        );
        let swapped = render_shadowed_h("text-shadow: 8px 8px #0000ff, 8px 8px #00ff00;");
        assert!(
            ink_box(&swapped, is_green).is_none(),
            "now the blue one covers the green"
        );
        assert!(ink_box(&swapped, is_blue).is_some());
    }

    #[test]
    fn a_shadow_that_is_transparent_paints_nothing() {
        let page = render_shadowed_h("text-shadow: 5px 5px rgba(0, 255, 0, 0);");
        assert!(ink_box(&page, |p| p[1] > 0 && p[0] == 0).is_none());
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

    /// `css` painted over `tree`, `size` px, text in black on white, a
    /// decoration in red wherever the css makes it so.
    fn paint_text_css(tree: &Element, css: &str, size: (u32, u32)) -> Canvas {
        let arena = Arena::build(tree);
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
        paint_to_buffer(
            &mut font,
            size.0,
            size.1,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            1.0,
        )
    }

    fn is_pure_red(pixel: [u8; 3]) -> bool {
        pixel == [255, 0, 0]
    }

    /// The rows with at least `min` pure red pixels.
    fn red_rows(canvas: &Canvas, size: (u32, u32), min: u32) -> Vec<u32> {
        (0..size.1)
            .filter(|&y| {
                (0..size.0)
                    .filter(|&x| is_pure_red(pixel_rgb(canvas, x, y)))
                    .count() as u32
                    >= min
            })
            .collect()
    }

    #[test]
    fn a_decoration_line_is_a_tenth_of_the_font_size_thick_and_sits_where_chromium_puts_it() {
        for (size, thick) in [
            (12, 1),
            (16, 1),
            (19, 1),
            (20, 2),
            (29, 2),
            (30, 3),
            (40, 4),
        ] {
            let tree: Element = view! { <div class="d">"HHHH"</div> };
            let css = format!(
                ".d {{ font-size: {size}px; line-height: 60px; text-decoration: underline red; }}"
            );
            let canvas = paint_text_css(&tree, &css, (120, 60));
            let rows = red_rows(&canvas, (120, 60), 20);
            assert_eq!(rows.len(), thick, "{size}px: rows {rows:?}");
            // The baseline of a 60px line of this font: ascent and descent
            // rounded, the rest of the line split in two, the smaller half above.
            let mut font = Font::load_embedded();
            let metrics = font.measure(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, size as f32, 400.0)
                    .with_line_height(florui_text::LineHeight::Length(60.0)),
                "H",
            );
            let above = rows[0] as f32 - metrics.baseline;
            let gap = (thick as f32 / 2.0).ceil().max(1.0);
            assert!(
                (above - gap).abs() <= 1.0,
                "{size}px: underline starts {above} below the baseline, wanted {gap}"
            );
        }
    }

    #[test]
    fn overline_and_line_through_are_drawn_above_and_through_the_text() {
        let tree: Element = view! { <div class="d">"HHHH"</div> };
        let canvas = paint_text_css(
            &tree,
            ".d { font-size: 20px; line-height: 60px; text-decoration: overline line-through red; }",
            (120, 60),
        );
        let rows = red_rows(&canvas, (120, 60), 20);
        assert_eq!(rows.len(), 4, "two lines of 2px: {rows:?}");
        let mut font = Font::load_embedded();
        let baseline = font
            .measure(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 20.0, 400.0)
                    .with_line_height(florui_text::LineHeight::Length(60.0)),
                "H",
            )
            .baseline;
        let at = |y: u32| y as f32 - baseline;
        assert!(
            at(rows[0]) < -20.0,
            "the overline is above the capitals: {rows:?}"
        );
        assert!(
            (at(rows[2]) + 8.0).abs() <= 1.5,
            "the strike is a third of the ascent up: {rows:?}"
        );
    }

    #[test]
    fn a_decoration_leaves_out_what_the_ink_reaches_unless_asked_not_to() {
        let columns = |css: &str| {
            let tree: Element = view! { <div class="d">"gjpqy"</div> };
            let canvas = paint_text_css(&tree, css, (140, 50));
            let rows = red_rows(&canvas, (140, 50), 10);
            let row = rows[0];
            (0..140)
                .filter(|&x| is_pure_red(pixel_rgb(&canvas, x, row)))
                .count()
        };
        let base = ".d { font-size: 28px; line-height: 50px; text-decoration: underline red; ";
        let skipping = columns(&format!("{base} }}"));
        let continuous = columns(&format!("{base} text-decoration-skip-ink: none; }}"));
        assert!(
            continuous > skipping + 10,
            "skip-ink leaves out the stretches around the descenders ({skipping} against {continuous})"
        );
    }

    #[test]
    fn a_decoration_keeps_the_color_of_the_element_that_declared_it() {
        let tree: Element = view! {
            <div class="outer">"ab "<span class="inner">"cd"</span>" ef"</div>
        };
        // Not red, so only the decoration can be: it runs under the green text.
        let canvas = paint_text_css(
            &tree,
            ".outer { font-size: 20px; line-height: 40px; text-decoration: underline #ff0000; \
             color: #000000; } .inner { color: #00ff00; }",
            (120, 40),
        );
        let rows = red_rows(&canvas, (120, 40), 40);
        assert!(!rows.is_empty());
        let row = rows[0];
        let red_columns = (0..120)
            .filter(|&x| is_pure_red(pixel_rgb(&canvas, x, row)))
            .count();
        assert!(
            red_columns > 50,
            "one line across all three runs: {red_columns}"
        );
    }

    #[test]
    fn dotted_dashed_and_double_lines_have_their_shapes() {
        let case = |style: &str| {
            let tree: Element = view! { <div class="d">"HHHHHHHH"</div> };
            let canvas = paint_text_css(
                &tree,
                &format!(
                    ".d {{ font-size: 20px; line-height: 40px; text-decoration: underline {style} red; }}"
                ),
                (150, 40),
            );
            let rows = red_rows(&canvas, (150, 40), 4);
            let row = rows[0];
            let on: Vec<bool> = (0..120)
                .map(|x| is_pure_red(pixel_rgb(&canvas, x, row)))
                .collect();
            (rows, on)
        };
        let (rows, on) = case("dotted");
        assert_eq!(rows.len(), 2);
        // The gap is fitted to the line, so later dots sit between pixels.
        assert_eq!(&on[..5], &[true, true, false, false, true]);
        let (rows, on) = case("dashed");
        assert_eq!(rows.len(), 2);
        assert!(
            on[..6].iter().all(|&dash| dash),
            "a dash of three thicknesses"
        );
        assert!(on[6..9].iter().any(|&dash| !dash), "then a gap");
        let (rows, _) = case("double");
        assert_eq!(rows.len(), 4, "two lines of two rows: {rows:?}");
        assert_eq!(rows[2] - rows[1], 2, "one row between them");
    }

    #[test]
    fn a_dashed_border_leaves_gaps_and_a_solid_one_does_not() {
        let tree: Element = view! { <div class="d" /> };
        let paint = |style: &str| {
            paint_text_css(
                &tree,
                &format!(".d {{ width: 88px; height: 8px; border: 4px {style} #ff0000; }}"),
                (100, 20),
            )
        };
        let row = |canvas: &Canvas| {
            (0..96)
                .filter(|&x| is_pure_red(pixel_rgb(canvas, x, 1)))
                .count()
        };
        assert_eq!(row(&paint("solid")), 96);
        let dashed = row(&paint("dashed"));
        assert!(dashed > 40 && dashed < 90, "{dashed}");
    }

    #[test]
    fn an_outline_is_drawn_over_the_content_and_takes_no_room() {
        let tree: Element = view! { <div class="o"><div class="in" /></div> };
        let css = ".o { width: 40px; height: 40px; margin: 10px; outline: 6px solid #ff0000; \
                   outline-offset: -3px; } .in { width: 40px; height: 40px; background-color: #0000ff; }";
        let canvas = paint_text_css(&tree, css, (80, 80));
        // The ring lies over the child's blue along the box's edge, not outside it.
        assert_eq!(pixel_rgb(&canvas, 11, 30), [255, 0, 0]);
        assert_eq!(pixel_rgb(&canvas, 20, 30), [0, 0, 255]);
        assert_eq!(pixel_rgb(&canvas, 5, 30), [255, 255, 255]);
    }

    #[test]
    fn a_border_image_replaces_the_border_and_a_missing_one_leaves_it() {
        let tree: Element = view! { <div class="a" /> };
        let css = |source: &str| {
            format!(
                ".a {{ width: 20px; height: 20px; border: 6px solid #ff0000; \
                 border-image: {source} 1; }}"
            )
        };
        let green = paint_text_css(&tree, &css("linear-gradient(#00ff00, #00ff00)"), (40, 40));
        assert_eq!(pixel_rgb(&green, 2, 20), [0, 255, 0]);
        let missing = paint_text_css(&tree, &css("url(no/such/picture.png)"), (40, 40));
        assert_eq!(
            pixel_rgb(&missing, 2, 20),
            [255, 0, 0],
            "falls back to the border"
        );
    }

    #[test]
    fn an_inline_element_draws_its_border_style_and_its_outline() {
        let tree: Element = view! {
            <div class="p">"ab "<span class="d">"cdef"</span>" gh "<span class="o">"ijkl"</span></div>
        };
        let css = ".p { font-size: 20px; line-height: 40px; margin: 10px; } \
                   .d { border: 4px dashed #ff0000; } \
                   .o { outline: 3px solid #00ff00; }";
        let canvas = paint_text_css(&tree, css, (260, 70));
        let count = |color: [u8; 3]| {
            (0..260)
                .flat_map(|x| (0..70).map(move |y| (x, y)))
                .filter(|&(x, y)| pixel_rgb(&canvas, x, y) == color)
                .count()
        };
        assert!(count([255, 0, 0]) > 40, "the dashed border has red in it");
        assert!(count([0, 255, 0]) > 100, "the outline has green in it");
    }

    #[test]
    fn the_lines_of_a_wrapped_inline_share_one_outline_without_edges_between() {
        let tree: Element = view! {
            <div class="p"><span class="o">"aaaa bbbb cccc dddd"</span></div>
        };
        let css = ".p { width: 70px; font-size: 20px; line-height: 40px; margin: 10px; } \
                   .o { outline: 3px solid #00ff00; }";
        let canvas = paint_text_css(&tree, css, (110, 190));
        // Between two lines the ring has no bar across the text: the pixel
        // midway down the gap between the first two lines' text is not ring.
        let ring_rows = (0..190)
            .filter(|&y| pixel_rgb(&canvas, 40, y) == [0, 255, 0])
            .count();
        assert!(
            ring_rows <= 12,
            "only the top and bottom of the union: {ring_rows}"
        );
    }

    #[test]
    fn the_focus_ring_is_two_pixels_thick_and_sits_at_the_offset() {
        let tree: Element = view! { <div class="o" /> };
        let canvas = paint_text_css(
            &tree,
            ".o { margin: 20px; width: 40px; height: 20px; outline: 9px auto #ff0000; \
             outline-offset: 5px; }",
            (100, 70),
        );
        let reds: Vec<u32> = (0..100)
            .filter(|&x| pixel_rgb(&canvas, x, 30) == [255, 0, 0])
            .collect();
        assert_eq!(
            reds,
            vec![15, 16, 63, 64],
            "2px, its outer edge 5px outside the box"
        );
    }

    #[test]
    fn a_decoration_on_text_with_no_inline_element_is_drawn_too() {
        let tree: Element = view! { <div class="d">"plain text only"</div> };
        let canvas = paint_text_css(
            &tree,
            ".d { font-size: 20px; line-height: 40px; text-decoration: line-through red; }",
            (200, 40),
        );
        assert_eq!(red_rows(&canvas, (200, 40), 40).len(), 2);
    }

    /// Positions and colors measured in Edge on a 200x30 number input whose
    /// border box starts at `(0, 0)`: the up arrow occupies rows 8..13 and
    /// the down arrow rows 17..22 around the column 11.5px in from the
    /// right edge, `rgb(139)` idle and `rgb(99)` under the pointer.
    fn paint_number_spinner(hover: SpinnerHover) -> Canvas {
        let tree: Element = view! { <input type="number" class="n" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".n { width: 200px; height: 30px; border: none; padding: 0px; \
             background-color: #ffffff; }",
        )
        .unwrap();
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
        let mut text_inputs = HashMap::new();
        text_inputs.insert(
            node,
            TextInputPaint {
                runs: Vec::new(),
                caret_rect: None,
                selection_rects: Vec::new(),
                compose_rect: None,
                show_caret: false,
                spinner: Some(hover),
                scroll: (0.0, 0.0),
                placeholder: None,
                scrollbar: None,
                resizer: false,
            },
        );
        paint_to_buffer_with_desktop_extras(
            &mut font,
            200,
            30,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            1.0,
            Some(&text_inputs),
            None,
        )
    }

    /// Paints a 60x20 text input holding `runs` (or a placeholder) and
    /// returns the canvas, 200px wide so any overflow past the box shows.
    fn paint_text_field(
        runs: Vec<florui_text::ShapedRun>,
        placeholder: Option<Vec<florui_text::ShapedRun>>,
    ) -> Canvas {
        paint_text_field_styled(runs, placeholder, "")
    }

    fn paint_text_field_styled(
        runs: Vec<florui_text::ShapedRun>,
        placeholder: Option<Vec<florui_text::ShapedRun>>,
        extra_css: &str,
    ) -> Canvas {
        let tree: Element = view! { <input type="text" class="f" /> };
        let arena = Arena::build(&tree);
        let css = format!(
            ".f {{ width: 60px; height: 20px; border: none; padding: 0px; \
             background-color: #ffffff; color: #000000; {extra_css} }}"
        );
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
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        let node = arena.roots()[0];
        let mut text_inputs = HashMap::new();
        text_inputs.insert(
            node,
            TextInputPaint {
                runs,
                caret_rect: None,
                selection_rects: Vec::new(),
                compose_rect: None,
                show_caret: false,
                spinner: None,
                scroll: (0.0, 0.0),
                placeholder,
                scrollbar: None,
                resizer: false,
            },
        );
        paint_to_buffer_with_desktop_extras(
            &mut font,
            200,
            20,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            1.0,
            Some(&text_inputs),
            None,
        )
    }

    #[test]
    fn a_placeholder_paints_in_edges_gray_and_never_past_the_field() {
        let mut font = Font::load_embedded();
        let long = font
            .shape(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                "A placeholder much longer than sixty pixels",
            )
            .runs;
        let buffer = paint_text_field(Vec::new(), Some(long));
        let mut darkest = 255;
        let mut outside = 0;
        for x in 0..200 {
            for y in 0..20 {
                let [r, ..] = pixel_rgb(&buffer, x, y);
                if r < 250 {
                    darkest = darkest.min(r);
                    if x >= 60 {
                        outside += 1;
                    }
                }
            }
        }
        assert_eq!(
            darkest, 117,
            "the placeholder's own gray, not the text color"
        );
        assert_eq!(outside, 0, "nothing is painted past the 60px field");
    }

    #[test]
    fn the_placeholder_color_property_recolors_the_placeholder() {
        let mut font = Font::load_embedded();
        let runs = font
            .shape(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                "Hint",
            )
            .runs;
        let buffer = paint_text_field_styled(
            Vec::new(),
            Some(runs),
            "--florui-placeholder-color: #ff0000;",
        );
        let reddest = (0..60)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .map(|(x, y)| pixel_rgb(&buffer, x, y))
            .filter(|[r, g, ..]| r > g)
            .map(|[r, g, b, ..]| (r, g, b))
            .max_by_key(|&(r, g, _)| r as i32 - g as i32)
            .expect("some red glyph pixel");
        assert_eq!(reddest, (255, 0, 0));
    }
    #[test]
    fn the_text_of_an_input_is_decorated_like_any_other_text() {
        let mut font = Font::load_embedded();
        let runs = font
            .shape(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                "Hi",
            )
            .runs;
        let plain = paint_text_field_styled(runs.clone(), None, "");
        let underlined = paint_text_field_styled(
            runs,
            None,
            "font-size: 16px; text-decoration: underline #ff0000;",
        );
        let reds = |buffer: &Canvas| {
            (0..60)
                .flat_map(|x| (0..20).map(move |y| (x, y)))
                .filter(|&(x, y)| pixel_rgb(buffer, x, y) == [255, 0, 0])
                .count()
        };
        assert_eq!(reds(&plain), 0);
        assert!(reds(&underlined) >= 10, "{}", reds(&underlined));
    }

    #[test]
    fn each_emboldened_run_is_thickened_by_its_own_size() {
        let mut font = Font::load_embedded();
        let style = |size: f32| {
            florui_text::TextStyle::new(florui_text::FontFamily::Monospace, size, 700.0)
        };
        let small = font.shape(style(12.0), "H").runs;
        let large = font.shape(style(40.0), "H").runs;
        assert!(small[0].embolden && large[0].embolden);
        let paint = |groups: &[&[florui_text::ShapedRun]]| {
            let mut surface = Surface::new(80, 60, (0, 0)).unwrap();
            for group in groups {
                paint_shaped_runs(
                    &mut surface,
                    group,
                    2.0,
                    2.0,
                    Rgba::opaque(0, 0, 0),
                    1.0,
                    None,
                );
            }
            surface.pixmap.data().to_vec()
        };
        let together: Vec<_> = small.iter().chain(&large).cloned().collect();
        let (a, b) = (paint(&[&together]), paint(&[&small, &large]));
        let differing = a.iter().zip(&b).filter(|(p, q)| p != q).count();
        assert_eq!(differing, 0, "of {} bytes", a.len());
    }

    #[test]
    fn a_hidden_input_draws_no_decoration() {
        let mut font = Font::load_embedded();
        let runs = font
            .shape(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                "Hi",
            )
            .runs;
        let hidden = paint_text_field_styled(
            runs,
            None,
            "visibility: hidden; text-decoration: underline #ff0000;",
        );
        let reds = (0..60)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel_rgb(&hidden, x, y) == [255, 0, 0])
            .count();
        assert_eq!(reds, 0);
    }

    #[test]
    fn typed_text_longer_than_its_input_is_cut_at_the_edge() {
        let mut font = Font::load_embedded();
        let runs = font
            .shape(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                "typed text that is far too long for the box",
            )
            .runs;
        let buffer = paint_text_field(runs, None);
        let outside = (60..200)
            .flat_map(|x| (0..20).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel_rgb(&buffer, x, y) != [255, 255, 255])
            .count();
        assert_eq!(outside, 0);
    }

    fn bar(scroll: f32, active: ScrollbarPart) -> ScrollbarPaint {
        // A 200x80 textarea (78 tall inside its border) whose text is 344
        // tall, as measured in Edge.
        ScrollbarPaint {
            scroll,
            max_scroll: 266.0,
            viewport: 78.0,
            active,
        }
    }

    /// Measured in Edge on a 200x80 textarea at (40, 40): the scrollbar sits
    /// at x=224..238, buttons at y=41..58 and 86..103, and the thumb runs
    /// 59..75 at the top, 63..79 at scrollTop 111, and ends near 85 at the end.
    #[test]
    fn the_scrollbar_geometry_matches_what_edge_measured() {
        let border = (1.0, 1.0, 1.0, 1.0);
        let g = |scroll| {
            scrollbar_geometry(
                (200.0, 80.0),
                border,
                true,
                bar(scroll, ScrollbarPart::None),
            )
        };
        let (top, mid, end) = (g(0.0), g(111.0), g(266.0));
        assert_eq!(top.bar_x, 184.0, "x=224 in the page");
        assert_eq!(top.up, (184.0, 1.0, 15.0, 18.0), "buttons 18px tall");
        assert_eq!(top.down.1, 46.0, "the lower button starts at y=86");
        assert_eq!(
            top.thumb,
            (187.0, 19.0, 9.0, 17.0),
            "thumb at y=59, 9 wide, 17 long"
        );
        assert_eq!(mid.thumb.1, 23.0, "y=63 at scrollTop 111");
        assert_eq!(
            end.thumb.1 + end.thumb.3,
            46.0,
            "ends against the lower button"
        );
    }

    #[test]
    fn the_part_under_the_pointer_is_found_from_the_same_geometry() {
        let border = (1.0, 1.0, 1.0, 1.0);
        let g = scrollbar_geometry((200.0, 80.0), border, true, bar(0.0, ScrollbarPart::None));
        let at = |x, y| scrollbar_part_at(&g, (x, y));
        assert_eq!(at(190.0, 10.0), ScrollbarPart::ButtonUp);
        assert_eq!(at(190.0, 60.0), ScrollbarPart::ButtonDown);
        assert_eq!(at(190.0, 25.0), ScrollbarPart::Thumb);
        assert_eq!(at(190.0, 40.0), ScrollbarPart::TrackAfter);
        let scrolled =
            scrollbar_geometry((200.0, 80.0), border, true, bar(266.0, ScrollbarPart::None));
        assert_eq!(
            scrollbar_part_at(&scrolled, (190.0, 22.0)),
            ScrollbarPart::TrackBefore
        );
        assert_eq!(at(100.0, 25.0), ScrollbarPart::None, "the text area itself");
        assert!(is_over_resizer((200.0, 80.0), border, (190.0, 70.0)));
        assert!(!is_over_resizer((200.0, 80.0), border, (190.0, 50.0)));
    }

    fn paint_textarea_chrome(scrollbar: Option<ScrollbarPaint>, resizer: bool) -> Canvas {
        let tree: Element = view! { <textarea class="t"></textarea> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".t { width: 198px; height: 78px; border: 1px solid #767676; padding: 0px; \
             background-color: #ffffff; }",
        )
        .unwrap();
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
        let mut text_inputs = HashMap::new();
        text_inputs.insert(
            node,
            TextInputPaint {
                runs: Vec::new(),
                caret_rect: None,
                selection_rects: Vec::new(),
                compose_rect: None,
                show_caret: false,
                spinner: None,
                scroll: (0.0, 0.0),
                placeholder: None,
                scrollbar,
                resizer,
            },
        );
        paint_to_buffer_with_desktop_extras(
            &mut font,
            200,
            80,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            1.0,
            Some(&text_inputs),
            None,
        )
    }

    #[test]
    fn the_scrollbar_paints_its_track_thumb_and_arrows_in_edges_colors() {
        let idle = paint_textarea_chrome(Some(bar(0.0, ScrollbarPart::None)), true);
        assert_eq!(pixel_rgb(&idle, 186, 30), [252, 252, 252], "the track");
        assert_eq!(
            pixel_rgb(&idle, 190, 25),
            [139, 139, 139],
            "the thumb, idle"
        );
        assert_eq!(
            pixel_rgb(&idle, 191, 10),
            [139, 139, 139],
            "the up arrow, idle"
        );
        assert_eq!(
            pixel_rgb(&idle, 191, 55),
            [139, 139, 139],
            "the down arrow, idle"
        );
        assert_eq!(
            pixel_rgb(&idle, 150, 40),
            [255, 255, 255],
            "text area untouched"
        );
        let hover_thumb = paint_textarea_chrome(Some(bar(0.0, ScrollbarPart::Thumb)), true);
        assert_eq!(pixel_rgb(&hover_thumb, 190, 25), [99, 99, 99]);
        assert_eq!(
            pixel_rgb(&hover_thumb, 191, 10),
            [139, 139, 139],
            "only the thumb"
        );
        let hover_down = paint_textarea_chrome(Some(bar(0.0, ScrollbarPart::ButtonDown)), true);
        assert_eq!(pixel_rgb(&hover_down, 191, 55), [99, 99, 99]);
    }

    #[test]
    fn the_resize_corner_paints_two_diagonals_where_edge_does() {
        let canvas = paint_textarea_chrome(None, true);
        // Relative to the bottom-right corner (200, 80): the long line runs
        // from (190, 76) up to (196, 70), the short one from (194, 76) to (196, 74).
        assert_eq!(pixel_rgb(&canvas, 190, 76), [101, 101, 101]);
        assert_eq!(pixel_rgb(&canvas, 193, 73), [101, 101, 101]);
        assert_eq!(pixel_rgb(&canvas, 196, 70), [101, 101, 101]);
        assert_eq!(pixel_rgb(&canvas, 194, 76), [101, 101, 101]);
        assert_eq!(pixel_rgb(&canvas, 196, 74), [101, 101, 101]);
        assert_eq!(
            pixel_rgb(&canvas, 192, 70),
            [255, 255, 255],
            "off the lines"
        );
        let without = paint_textarea_chrome(None, false);
        assert_eq!(
            pixel_rgb(&without, 193, 73),
            [255, 255, 255],
            "no resize, no corner"
        );
    }

    #[test]
    fn a_number_inputs_spinner_paints_two_arrows_where_edge_does() {
        let buffer = paint_number_spinner(SpinnerHover::None);
        // Column 11.5px in from the right edge is x = 188; up arrow centre
        // ~3.5px above the middle (y = 11), down arrow ~5.5px below (y = 20).
        assert_eq!(pixel_rgb(&buffer, 188, 11), [139, 139, 139], "up arrow");
        assert_eq!(pixel_rgb(&buffer, 188, 20), [139, 139, 139], "down arrow");
        assert_eq!(
            pixel_rgb(&buffer, 150, 15),
            [255, 255, 255],
            "the field body"
        );
        assert_eq!(
            pixel_rgb(&buffer, 188, 15),
            [255, 255, 255],
            "between the arrows"
        );
    }

    #[test]
    fn only_the_arrow_under_the_pointer_darkens() {
        let up = paint_number_spinner(SpinnerHover::Up);
        assert_eq!(pixel_rgb(&up, 188, 11), [99, 99, 99]);
        assert_eq!(pixel_rgb(&up, 188, 20), [139, 139, 139]);
        let down = paint_number_spinner(SpinnerHover::Down);
        assert_eq!(pixel_rgb(&down, 188, 11), [139, 139, 139]);
        assert_eq!(pixel_rgb(&down, 188, 20), [99, 99, 99]);
    }

    #[test]
    fn the_spinner_halves_split_a_box_around_the_arrows() {
        // Measured in Edge on a 200x30 field: the up arrow darkens for
        // rows 6..14, the down arrow for rows 15..23, columns 181..195.
        let field = (0.0, 0.0, 200.0, 30.0);
        assert_eq!(spinner_half_at(field, (188.0, 6.0)), SpinnerHover::Up);
        assert_eq!(spinner_half_at(field, (188.0, 14.9)), SpinnerHover::Up);
        assert_eq!(spinner_half_at(field, (188.0, 15.0)), SpinnerHover::Down);
        assert_eq!(spinner_half_at(field, (188.0, 23.9)), SpinnerHover::Down);
        assert_eq!(spinner_half_at(field, (188.0, 5.9)), SpinnerHover::None);
        assert_eq!(spinner_half_at(field, (188.0, 24.0)), SpinnerHover::None);
        assert_eq!(spinner_half_at(field, (181.0, 10.0)), SpinnerHover::Up);
        assert_eq!(spinner_half_at(field, (180.9, 10.0)), SpinnerHover::None);
        assert_eq!(spinner_half_at(field, (195.9, 10.0)), SpinnerHover::Up);
        assert_eq!(spinner_half_at(field, (196.0, 10.0)), SpinnerHover::None);
    }

    #[test]
    fn a_text_input_paints_its_caret_in_its_own_declared_color() {
        let tree: Element =
            view! { <input type="text" value="Hi" style="color: #ff0000; width: 60px;" /> };
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
            .measure(
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                "Hi",
            )
            .width;
        let mut text_inputs = HashMap::new();
        text_inputs.insert(
            node,
            TextInputPaint {
                runs: font
                    .shape(
                        florui_text::TextStyle::new(
                            florui_text::FontFamily::SansSerif,
                            16.0,
                            400.0,
                        ),
                        "Hi",
                    )
                    .runs,
                caret_rect: Some((text_width + 4.0, 0.0, text_width + 4.0, height as f32)),
                selection_rects: Vec::new(),
                compose_rect: None,
                show_caret: true,
                spinner: None,
                scroll: (0.0, 0.0),
                placeholder: None,
                scrollbar: None,
                resizer: false,
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
    fn line_height_moves_the_ink_by_half_the_extra_leading() {
        // The first and last row with red ink, and the box height, for one
        // line of text under `css`.
        fn ink_rows(css: &str) -> (u32, u32, f32) {
            let tree: Element = view! { <div class="t">{"H"}</div> };
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
            let (width, height) = (layouts[&node].width.ceil() as u32, 80);
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
            let inked = |py: u32| (0..width).any(|px| pixel_rgb(&buffer, px, py)[0] > 0x20);
            let first = (0..height).find(|&py| inked(py)).expect("some ink");
            let last = (0..height).rev().find(|&py| inked(py)).expect("some ink");
            (first, last, layouts[&node].height)
        }

        let plain = ink_rows(".t { color: #ff0000; font-size: 20px; }");
        let tall = ink_rows(".t { color: #ff0000; font-size: 20px; line-height: 60px; }");
        assert!((tall.2 - 60.0).abs() < 0.01, "the box is one 60px line");
        let shift = (60.0 - plain.2) / 2.0;
        for (name, moved) in [("first", tall.0 - plain.0), ("last", tall.1 - plain.1)] {
            assert!(
                (moved as f32 - shift).abs() <= 1.0,
                "the {name} ink row moved by {moved}, expected about {shift}"
            );
        }
    }

    /// The first and last column with red ink in each 30px band of a 200px
    /// wide div under `css` (`line-height: 30px` puts one line in each).
    fn ink_columns_per_line(css: &str, text: &str, width: u32) -> Vec<(u32, u32)> {
        let tree = Element::node(
            "div",
            vec![("class".to_string(), "t".to_string())],
            vec![Element::text(text)],
        );
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
        let height = 120;
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
        (0..height / 30)
            .filter_map(|band| {
                let inked: Vec<u32> = (0..width)
                    .filter(|&px| {
                        (band * 30..band * 30 + 30).any(|py| pixel_rgb(&buffer, px, py)[0] > 0x40)
                    })
                    .collect();
                Some((*inked.first()?, *inked.last()?))
            })
            .collect()
    }

    #[test]
    fn text_align_places_every_line_not_only_a_single_one() {
        let css = |align: &str, width: u32| {
            format!(
                ".t {{ color: #ff0000; width: {width}px; line-height: 30px; text-align: {align}; }}"
            )
        };
        let text = "aaaa bbbb cccc dddd eeee ffff gggg";
        let left = ink_columns_per_line(&css("left", 120), text, 120);
        let center = ink_columns_per_line(&css("center", 120), text, 120);
        let right = ink_columns_per_line(&css("right", 120), text, 120);
        assert!(left.len() >= 3 && center.len() == left.len() && right.len() == left.len());
        for line in 0..left.len() {
            // The same ink, moved: the start of a centered line sits between
            // the start and the end ones, and a right line ends at the edge.
            assert!(center[line].0 >= left[line].0 && right[line].0 >= center[line].0);
            assert!(
                right[line].1 + 3 >= 119,
                "line {line} ends at {}",
                right[line].1
            );
        }
        // Lines of different widths start at different columns when centered.
        assert!(center.iter().any(|c| c.0 != center[0].0));
    }

    #[test]
    fn justify_stretches_every_line_but_the_last_to_the_edge() {
        let css = |align: &str| {
            format!(
                ".t {{ color: #ff0000; width: 120px; line-height: 30px; text-align: {align}; }}"
            )
        };
        let text = "aaaa bbbb cccc dddd eeee ffff gggg";
        let plain = ink_columns_per_line(&css("left"), text, 120);
        let justified = ink_columns_per_line(&css("justify"), text, 120);
        assert_eq!(plain.len(), justified.len());
        let last = justified.len() - 1;
        for (line, columns) in justified.iter().take(last).enumerate() {
            assert!(
                columns.1 + 3 >= 119,
                "line {line} reaches {} of 120",
                columns.1
            );
        }
        assert_eq!(
            justified[last], plain[last],
            "the last line stays at the start"
        );
    }

    /// A paragraph of inline content under `css`, painted 200 x 120 on black.
    fn painted_paragraph(
        css: &str,
        children: Vec<Element>,
    ) -> (Canvas, Arena, StyleMap, HashMap<usize, BoxLayout>) {
        let tree = Element::node(
            "div",
            vec![("class".to_string(), "p".to_string())],
            children,
        );
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
        let canvas = paint_to_buffer(
            &mut font,
            200,
            120,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        (canvas, arena, styles, layouts)
    }

    fn span_of(class: &str, text: &str) -> Element {
        Element::node(
            "span",
            vec![("class".to_string(), class.to_string())],
            vec![Element::text(text)],
        )
    }

    #[test]
    fn text_around_a_block_is_painted_in_its_own_lines_and_not_over_the_block() {
        // Two 22px lines of text with a 30px blue block between them.
        let (canvas, ..) = painted_paragraph(
            ".p { width: 200px; color: #ffffff; } .blk { height: 30px; background-color: #0000ff; }",
            vec![
                Element::text("text before"),
                Element::node(
                    "div",
                    vec![("class".to_string(), "blk".to_string())],
                    vec![],
                ),
                Element::text("text after"),
            ],
        );
        let white_rows = |from: u32, to: u32| {
            (from..to)
                .filter(|&y| {
                    (0..200).any(|x| {
                        let [r, g, b] = pixel_rgb(&canvas, x, y);
                        r > 0xc0 && g > 0xc0 && b > 0xc0
                    })
                })
                .count()
        };
        assert!(white_rows(0, 22) > 5, "the text before the block");
        assert_eq!(white_rows(22, 52), 0, "nothing over the block");
        assert!(white_rows(52, 74) > 5, "the text after the block");
        assert_eq!(pixel_rgb(&canvas, 100, 36), [0, 0, 0xff]);
    }

    #[test]
    fn a_span_beside_a_block_paints_its_own_background_where_layout_put_it() {
        let (canvas, arena, _styles, layouts) = painted_paragraph(
            ".p { width: 200px; color: #ffffff; } .blk { height: 30px; } .g { background-color: #00ff00; color: #000000; }",
            vec![
                Element::node(
                    "div",
                    vec![("class".to_string(), "blk".to_string())],
                    vec![],
                ),
                span_of("g", "below"),
            ],
        );
        let span = arena.children(arena.roots()[0])[1];
        let rect = layouts[&span];
        assert!(rect.y >= 30.0, "the span is under the block");
        let [r, g, b] = pixel_rgb(&canvas, rect.x as u32 + 1, rect.y as u32 + 1);
        assert!(g > 0xc0 && r < 0x40 && b < 0x40);
    }

    #[test]
    fn text_overflow_ends_a_clipped_line_in_an_ellipsis_and_clip_does_not() {
        // The rows of ink in the last 12 columns of a 90px box: three dots
        // sit on the baseline, while the cut letter fills the x-height.
        let ink_rows = |overflow: &str| {
            let (canvas, ..) = painted_paragraph(
                &format!(
                    ".p {{ width: 90px; overflow: hidden; white-space: nowrap; color: #ffffff; text-overflow: {overflow}; }}"
                ),
                vec![Element::text("The quick brown fox jumps")],
            );
            (0..30u32)
                .filter(|&y| {
                    (78..90).any(|x| {
                        let [r, g, b] = pixel_rgb(&canvas, x, y);
                        r > 0x80 && g > 0x80 && b > 0x80
                    })
                })
                .count()
        };
        let ellipsis = ink_rows("ellipsis");
        let clip = ink_rows("clip");
        assert!(ellipsis > 0 && ellipsis <= 4, "{ellipsis} rows of dots");
        assert!(clip > ellipsis + 4, "{clip} rows of cut letters");
    }

    #[test]
    fn a_span_paints_its_own_color() {
        let (canvas, ..) = painted_paragraph(
            ".p { width: 200px; color: #ffffff; } .r { color: #ff0000; }",
            vec![Element::text("white "), span_of("r", "red")],
        );
        let mut red = 0;
        let mut white = 0;
        for y in 0..30 {
            for x in 0..200 {
                let [r, g, b] = pixel_rgb(&canvas, x, y);
                if r > 0xc0 && g < 0x40 && b < 0x40 {
                    red += 1;
                }
                if r > 0xc0 && g > 0xc0 && b > 0xc0 {
                    white += 1;
                }
            }
        }
        assert!(red > 20, "{red} red pixels");
        assert!(white > 20, "{white} white pixels");
    }

    #[test]
    fn a_span_paints_its_background_over_its_own_box_only() {
        let (canvas, arena, _styles, layouts) = painted_paragraph(
            ".p { width: 200px; color: #ffffff; } .g { background-color: #00ff00; color: #000000; }",
            vec![
                Element::text("ab "),
                span_of("g", "green"),
                Element::text(" cd"),
            ],
        );
        let span = arena.children(arena.roots()[0])[0];
        let rect = layouts[&span];
        let green = |x: u32, y: u32| {
            let [r, g, b] = pixel_rgb(&canvas, x, y);
            g > 0xc0 && r < 0x40 && b < 0x40
        };
        // The corner pixels inside the box are green...
        assert!(green(rect.x as u32 + 1, rect.y as u32 + 1));
        assert!(green(
            (rect.x + rect.width) as u32 - 2,
            (rect.y + rect.height) as u32 - 2
        ));
        // ...and just outside it on every side they are not.
        assert!(!green(rect.x as u32 - 2, rect.y as u32 + 5));
        assert!(!green((rect.x + rect.width) as u32 + 2, rect.y as u32 + 5));
        assert!(!green(rect.x as u32 + 5, (rect.y + rect.height) as u32 + 2));
    }

    #[test]
    fn a_box_that_wraps_is_sliced_with_no_border_at_the_cut() {
        // A span with a blue 4px border, wrapped across two lines: the first
        // line has its left border and no right one, the second the opposite.
        let (canvas, arena, _styles, layouts) = painted_paragraph(
            ".p { width: 120px; color: #ffffff; line-height: 40px; } \
             .b { border: 4px solid #0000ff; }",
            vec![Element::text("aaa "), span_of("b", "bbb ccc ddd")],
        );
        let span = arena.children(arena.roots()[0])[0];
        let rect = layouts[&span];
        let blue = |x: u32, y: u32| {
            let [r, g, b] = pixel_rgb(&canvas, x, y);
            b > 0xc0 && r < 0x40 && g < 0x40
        };
        let first_row = rect.y as u32 + 4 + 6;
        // The first line starts the box after "aaa ": blue to the right of the
        // line's start, none at it.
        assert!(
            (10..120).any(|x| blue(x, first_row)),
            "left border of the first line"
        );
        assert!(!blue(1, first_row));
        // The second line continues it: nothing blue at the line's left edge.
        let second_row = 40 + 14;
        assert!(
            !blue(1, second_row),
            "no left border where the box continues"
        );
        // And that line ends the box: blue at its right end.
        let right = (0..120).rev().find(|&x| blue(x, second_row));
        assert!(right.is_some(), "right border of the last line");
    }

    #[test]
    fn a_span_is_underlined_only_where_it_or_a_parent_says() {
        let (canvas, arena, styles, layouts) = painted_paragraph(
            ".p { width: 200px; color: #ffffff; } .u { text-decoration: underline; }",
            vec![Element::text("plain "), span_of("u", "under")],
        );
        let container = arena.roots()[0];
        let span = arena.children(container)[0];
        let mut font = Font::load_embedded();
        let shaped = florui_layout::shape_inline_formatting_context(
            &mut font,
            &arena,
            &styles,
            (&layouts, 1.0),
            container,
            200.0,
        )
        .unwrap();
        let baseline = shaped.layout.baseline;
        let rect = layouts[&span];
        let lit = |x: u32, y: u32| pixel_rgb(&canvas, x, y)[0] > 0x80;
        let underlined = |from: u32, to: u32| {
            (baseline as u32 + 1..baseline as u32 + 4).any(|y| {
                let covered = (from..to).filter(|&x| lit(x, y)).count() as u32;
                covered * 10 >= (to - from) * 8
            })
        };
        assert!(underlined(
            rect.x as u32 + 1,
            (rect.x + rect.width) as u32 - 1
        ));
        assert!(!underlined(2, rect.x as u32 - 6));
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
                florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, 16.0, 400.0),
                text,
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
                        margin: florui_layout::BoxMargin::ZERO,
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
                            margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
    fn a_shadow_skips_the_pixels_whose_answer_is_known_without_changing_any() {
        let outline = RoundedRect {
            x: 20.5,
            y: 18.0,
            width: 90.0,
            height: 70.0,
            radii: [(14.0, 14.0), (6.0, 6.0), (20.0, 20.0), (0.0, 0.0)],
        };
        for inset in [false, true] {
            let shape = if inset {
                outline.inset(3.0, 3.0, 3.0, 3.0)
            } else {
                outline.grown(4.0)
            };
            let blurred =
                rasterize_and_blur(shape.x, shape.y, shape.width, shape.height, &shape, 12.0)
                    .expect("a shadow");
            let draw = |known| {
                let mut surface = Surface::new(140, 110, (0, 0)).expect("a surface");
                composite_blurred_shadow(
                    &mut surface,
                    &blurred,
                    Rgba::opaque(200, 30, 30),
                    inset,
                    known,
                    |x, y| outline_contains_pixel(&outline, x, y) == inset,
                );
                surface.pixmap.data().to_vec()
            };
            let interior = outline.solid_interior().expect("room inside");
            assert_eq!(draw(Some((interior, inset))), draw(None), "inset: {inset}");
        }
    }

    #[test]
    fn a_rounded_border_of_four_colors_is_painted_all_round_its_corners() {
        // 6px sides (1px at a scale of 6) on a 60px box with 25.8px corners: the arc
        // of each corner lies well inside the box's corner square, along its diagonal.
        let page = single_box_buffer_at(
            ".box { background-color: #000000; border: 1px solid; border-radius: 4.3px; \
             border-color: #ff0000 #00ff00 #0000ff #ffff00; }",
            80,
            6.0,
            0.0,
            0.0,
        );
        // Half way through the ring, at 45 degrees from the arc's center: the
        // diagonal where two sides meet. A pixel to either side of it belongs to
        // one of them.
        let c = 25.8 - 22.8 * std::f32::consts::FRAC_1_SQRT_2;
        let at = |x: f32, y: f32| {
            let p = page.pixel(x as u32, y as u32).expect("inside the page");
            [p.red(), p.green(), p.blue()]
        };
        assert_eq!(at(c - 2.0, c + 2.0), [255, 255, 0], "top left, left side");
        assert_eq!(at(c + 2.0, c - 2.0), [255, 0, 0], "top left, top side");
        assert_eq!(
            at(60.0 - c + 2.0, c + 2.0),
            [0, 255, 0],
            "top right, right side"
        );
        assert_eq!(
            at(60.0 - c - 2.0, c - 2.0),
            [255, 0, 0],
            "top right, top side"
        );
        assert_eq!(
            at(60.0 - c + 2.0, 60.0 - c - 2.0),
            [0, 255, 0],
            "bottom right, right side"
        );
        assert_eq!(
            at(60.0 - c - 2.0, 60.0 - c + 2.0),
            [0, 0, 255],
            "bottom right, bottom side"
        );
        assert_eq!(
            at(c - 2.0, 60.0 - c - 2.0),
            [255, 255, 0],
            "bottom left, left side"
        );
        assert_eq!(
            at(c + 2.0, 60.0 - c + 2.0),
            [0, 0, 255],
            "bottom left, bottom side"
        );
    }

    #[test]
    fn a_hard_rounded_shadow_is_what_a_window_sized_mask_would_give() {
        let outline = RoundedRect {
            x: 30.3,
            y: 24.6,
            width: 60.0,
            height: 44.0,
            radii: [(10.0, 10.0), (4.0, 4.0), (16.0, 16.0), (0.0, 0.0)],
        };
        for inset in [false, true] {
            let shadow = florui_style::BoxShadow {
                offset_x: 3.0,
                offset_y: 5.0,
                blur_radius: 0.0,
                spread_radius: if inset { 2.0 } else { 3.0 },
                color: Rgba {
                    r: 200,
                    g: 40,
                    b: 40,
                    a: 160,
                },
                inset,
            };
            let mut near = Surface::new(120, 100, (0, 0)).expect("a surface");
            paint_rounded_shadow(&mut near, &outline, NO_BORDER, &shadow);

            // The same shadow through a mask as large as the surface.
            let mut whole = Surface::new(120, 100, (0, 0)).expect("a surface");
            let (fill, exclude) = if inset {
                let hole = outline
                    .inset(0.0, 0.0, 0.0, 0.0)
                    .inset(2.0, 2.0, 2.0, 2.0)
                    .translated(3.0, 5.0);
                (outline, hole)
            } else {
                let grown = outline.grown(3.0).translated(3.0, 5.0);
                (grown, outline)
            };
            let transform = surface_transform(&whole);
            let mut mask = Mask::new(120, 100).expect("a mask");
            mask.fill_path(
                &exclude.path().expect("a path"),
                FillRule::Winding,
                true,
                transform,
            );
            mask.invert();
            let mut paint = Paint::default();
            paint.set_color_rgba8(200, 40, 40, 160);
            paint.anti_alias = true;
            whole.pixmap.fill_path(
                &fill.path().expect("a path"),
                &paint,
                FillRule::Winding,
                transform,
                Some(&mask),
            );
            // Moving the shape into a smaller layer moves where its curved edge
            // is sampled by a rounding error: a few edge pixels differ a little.
            let diffs: Vec<u8> = near
                .pixmap
                .data()
                .iter()
                .zip(whole.pixmap.data())
                .map(|(a, b)| a.abs_diff(*b))
                .collect();
            assert!(diffs.iter().all(|&d| d <= 12), "inset {inset}");
            assert!(
                diffs.iter().filter(|&&d| d > 1).count() <= 16,
                "inset {inset}: only the edge differs"
            );
        }
    }

    #[test]
    fn a_small_frame_stretches_to_the_size_asked() {
        let mut small = Pixmap::new(4, 3).expect("a pixmap");
        small.fill(tiny_skia::Color::from_rgba8(200, 100, 50, 255));
        let big = stretched(&small, 10, 7).expect("a pixmap");
        assert_eq!((big.width(), big.height()), (10, 7));
        for pixel in big.pixels() {
            assert_eq!(
                (pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()),
                (200, 100, 50, 255),
                "a flat frame stays flat, edges included"
            );
        }
        assert!(stretched(&small, 0, 7).is_none());
    }

    #[test]
    fn a_small_frame_is_blended_between_its_pixels_when_stretched() {
        let mut small = Pixmap::new(2, 1).expect("a pixmap");
        small.data_mut()[..4].copy_from_slice(&[0, 0, 0, 255]);
        small.data_mut()[4..].copy_from_slice(&[200, 200, 200, 255]);
        let wide = stretched(&small, 8, 3).expect("a pixmap");
        let reds: Vec<u8> = (0..8)
            .map(|x| wide.pixel(x, 1).expect("inside").red())
            .collect();
        assert!(reds.windows(2).all(|pair| pair[0] <= pair[1]), "{reds:?}");
        assert_eq!(reds[0], 0, "the left edge is the left pixel");
        assert_eq!(reds[7], 200, "the right edge is the right pixel");
        assert!(reds[3] > 0 && reds[3] < 200, "the middle is both: {reds:?}");
        let rows: Vec<u8> = (0..3)
            .map(|y| wide.pixel(4, y).expect("inside").red())
            .collect();
        assert_eq!(rows[0], rows[2], "rows of a one row frame are alike");
    }

    #[test]
    fn a_border_and_padding_scale_with_the_display() {
        // 10px box at a scale of 3: a 30px box, its 1px border 3px wide and its
        // 1px padding 3px, so the content box starts 6px in.
        let page = single_box_buffer_at(
            ".box { border: 1px solid #0000ff; padding: 1px; \
             background-clip: content-box; \
             background-image: linear-gradient(#ff0000, #ff0000); }",
            40,
            3.0,
            0.0,
            0.0,
        );
        let at = |x, y| {
            let p = page.pixel(x, y).expect("inside the page");
            [p.red(), p.green(), p.blue()]
        };
        assert_eq!(at(1, 15), [0, 0, 255], "inside the border");
        assert_eq!(at(4, 15), [0, 0, 0], "the padding is neither");
        assert_eq!(at(5, 15), [0, 0, 0], "to its last pixel");
        assert_eq!(at(6, 15), [255, 0, 0], "the content box starts after both");
        assert_eq!(at(23, 15), [255, 0, 0], "to its last pixel");
        assert_eq!(at(24, 15), [0, 0, 0], "before the padding on the far side");
        assert_eq!(at(28, 15), [0, 0, 255], "and the border");
        // The same down the page.
        assert_eq!(at(15, 1), [0, 0, 255]);
        assert_eq!(at(15, 5), [0, 0, 0]);
        assert_eq!(at(15, 6), [255, 0, 0]);
        assert_eq!(at(15, 23), [255, 0, 0]);
        assert_eq!(at(15, 24), [0, 0, 0]);
        assert_eq!(at(15, 28), [0, 0, 255]);
    }

    #[test]
    fn a_line_in_a_box_sized_to_it_does_not_wrap_at_any_scale() {
        let text = "A WORKSHOP DESK FOR THINGS TO DO";
        let mut font = Font::load_embedded();
        let family = florui_text::FontFamily::SansSerif;
        let line = font.measure(florui_text::TextStyle::new(family, 11.0, 700.0), text);
        // Scaling the width of the line to painted pixels and back by another
        // factor can come out a hair short of it, which wrapped its last word.
        for scale in (0..400).map(|step| 0.3 + step as f32 * 0.0071) {
            let wrapped = font.measure_wrapped(
                florui_text::TextStyle::new(family, 11.0, 700.0),
                text,
                wrap_width(line.width * scale, scale),
            );
            assert_eq!(wrapped.height, line.height, "wrapped at a scale of {scale}");
        }
    }

    #[test]
    fn a_shadow_blurred_as_a_small_shape_is_the_one_blurred_whole() {
        let radii_sets: [[(f32, f32); 4]; 3] = [
            [(0.0, 0.0); 4],
            [(14.0, 14.0); 4],
            [(10.0, 6.0), (3.0, 3.0), (18.0, 18.0), (0.0, 0.0)],
        ];
        for radii in radii_sets {
            for (x, y) in [(0.0, 0.0), (0.5, 0.25), (3.75, 2.0)] {
                for sigma in [1.0f32, 3.0, 6.0, 9.0] {
                    let pad = blur::kernel_radius(sigma) as f32;
                    let (shape_w, shape_h) = (260.3f32, 140.6f32);
                    let (width, height) = (
                        (shape_w + 2.0 * pad + x).ceil() as u32 + 3,
                        (shape_h + 2.0 * pad + y).ceil() as u32 + 3,
                    );
                    let local = RoundedRect {
                        x: pad + x,
                        y: pad + y,
                        width: shape_w,
                        height: shape_h,
                        radii,
                    };
                    assert_eq!(
                        blurred_coverage(width, height, sigma, &local),
                        direct_coverage(width, height, sigma, &local),
                        "radii {radii:?}, offset ({x}, {y}), sigma {sigma}"
                    );
                }
            }
        }
    }

    #[test]
    fn what_a_clip_hides_is_not_painted_on_a_surface_that_lies_elsewhere() {
        // A box that clips, at rows 0..40, over a child that is far taller than it
        // is. A surface that shows rows 100..150 only sees what the child would
        // paint beyond the clip, which is nothing.
        let tree: Element = view! { <div class="wrap"><div class="inner"></div></div> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".wrap { overflow: hidden; } .inner { background-color: #ff0000; }",
        )
        .expect("a stylesheet");
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let wrap = arena.roots()[0];
        let inner = arena.children(wrap)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            wrap,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 60.0,
                height: 40.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            inner,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 60.0,
                height: 200.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        let mut font = Font::load_embedded();
        let mut surface = Surface::new(60, 50, (0, 100)).expect("a surface");
        paint_nodes(
            &mut surface,
            &arena,
            &styles,
            &layouts,
            &mut font,
            None,
            arena.roots(),
            1.0,
            None,
            None,
            None,
        );
        assert!(
            surface
                .pixmap
                .pixels()
                .iter()
                .all(|pixel| pixel.alpha() == 0),
            "the clip hides the child here, and nothing is painted"
        );
    }

    #[test]
    fn a_turned_disc_keeps_a_smooth_edge() {
        // A disc turned about its own center is the same disc, so the edge
        // pixels should still be the unturned ones, not the nearest pixel of
        // an edge already softened once.
        let disc = |extra: &str| {
            single_box_buffer(
                &format!(".box {{ background-color: #ff0000; border-radius: 5px; {extra} }}"),
                80,
                4.0,
            )
        };
        let flat = disc("");
        let turned = disc("transform: rotate(30deg);");
        let worst = (0..80)
            .flat_map(|y| (0..80).map(move |x| (x, y)))
            .map(|(x, y)| {
                let a = flat.pixel(x, y).expect("inside").red();
                let b = turned.pixel(x, y).expect("inside").red();
                a.abs_diff(b)
            })
            .max()
            .expect("pixels");
        assert!(
            worst <= 90,
            "the turned edge is {worst} levels from the flat one"
        );
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
    fn skew_shears_the_box_about_its_centre() {
        let skewed = single_box_buffer(
            ".box { background-color: #ff0000; transform: skewX(30deg); }",
            30,
            1.0,
        );
        let red = [255, 0, 0];
        // The 10x10 box at the origin shears by tan(30deg) per pixel of
        // height about its centre: its top edge moves left, its bottom edge right.
        assert_eq!(pixel_rgb(&skewed, 11, 9), red, "bottom edge moved right");
        assert_eq!(
            pixel_rgb(&skewed, 11, 0),
            [0, 0, 0],
            "top edge is not there"
        );
        assert_eq!(
            pixel_rgb(&skewed, 0, 9),
            [0, 0, 0],
            "bottom-left corner left"
        );
        assert_eq!(pixel_rgb(&skewed, 0, 0), red, "top-left corner stays");
        let upright = single_box_buffer(".box { background-color: #ff0000; }", 30, 1.0);
        assert_eq!(pixel_rgb(&upright, 11, 9), [0, 0, 0]);
    }

    #[test]
    fn the_individual_properties_draw_where_their_transform_equivalents_do() {
        let pairs = [
            ("translate: 6px 3px", "transform: translate(6px, 3px)"),
            ("rotate: 20deg", "transform: rotate(20deg)"),
            ("scale: 1.5", "transform: scale(1.5)"),
            (
                "translate: 4px 2px; rotate: 15deg; scale: 1.2",
                "transform: translate(4px, 2px) rotate(15deg) scale(1.2)",
            ),
        ];
        for (individual, transform) in pairs {
            let a = single_box_buffer_at(
                &format!(".box {{ background-color: #ff0000; {individual}; }}"),
                40,
                1.0,
                12.0,
                12.0,
            );
            let b = single_box_buffer_at(
                &format!(".box {{ background-color: #ff0000; {transform}; }}"),
                40,
                1.0,
                12.0,
                12.0,
            );
            for y in 0..40 {
                for x in 0..40 {
                    assert_eq!(
                        pixel_rgb(&a, x, y),
                        pixel_rgb(&b, x, y),
                        "{individual} vs {transform} at ({x}, {y})"
                    );
                }
            }
        }
        let moved = single_box_buffer(
            ".box { background-color: #ff0000; translate: 20px 0; }",
            40,
            1.0,
        );
        assert_eq!(pixel_rgb(&moved, 25, 5), [255, 0, 0]);
        assert_eq!(pixel_rgb(&moved, 5, 5), [0, 0, 0]);
    }

    const WHITE: [u8; 3] = [255, 255, 255];
    const RED: [u8; 3] = [255, 0, 0];
    const BLUE: [u8; 3] = [0, 0, 255];
    const GREEN: [u8; 3] = [0, 255, 0];

    fn same_pixels(a: &Canvas, b: &Canvas, size: (u32, u32)) {
        for y in 0..size.1 {
            for x in 0..size.0 {
                assert_eq!(pixel_rgb(a, x, y), pixel_rgb(b, x, y), "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_parents_perspective_draws_what_perspective_in_the_childs_transform_does() {
        let tree: Element = view! { <div class="stage"><div class="card" /></div> };
        let by_parent = paint_text_css(
            &tree,
            ".stage { width: 100px; height: 100px; perspective: 100px; } \
             .card { width: 100px; height: 100px; background-color: #ff0000; \
                     transform: rotateY(60deg); }",
            (140, 100),
        );
        let by_function = paint_text_css(
            &tree,
            ".stage { width: 100px; height: 100px; } \
             .card { width: 100px; height: 100px; background-color: #ff0000; \
                     transform: perspective(100px) rotateY(60deg); }",
            (140, 100),
        );
        same_pixels(&by_parent, &by_function, (140, 100));
        // And it is foreshortened: the left half grows toward the viewer, the
        // right half shrinks away (measured against Chrome in the fixtures).
        assert_eq!(pixel_rgb(&by_parent, 20, 50), RED);
        assert_eq!(
            pixel_rgb(&by_parent, 20, 3),
            RED,
            "the near edge is taller than the box"
        );
        assert_eq!(
            pixel_rgb(&by_parent, 85, 50),
            WHITE,
            "the far edge is short of it"
        );
        assert_eq!(pixel_rgb(&by_parent, 70, 5), WHITE);
    }

    #[test]
    fn a_perspective_free_turn_only_narrows_the_box() {
        let tree: Element = view! { <div class="card" /> };
        let canvas = paint_text_css(
            &tree,
            ".card { width: 100px; height: 40px; background-color: #ff0000; \
                    transform: rotateY(60deg); }",
            (120, 60),
        );
        // cos(60deg) = 0.5: 50px wide about the centre (50, 20), full height.
        assert_eq!(pixel_rgb(&canvas, 30, 20), RED);
        assert_eq!(pixel_rgb(&canvas, 70, 20), RED);
        assert_eq!(pixel_rgb(&canvas, 20, 20), WHITE);
        assert_eq!(pixel_rgb(&canvas, 80, 20), WHITE);
        assert_eq!(pixel_rgb(&canvas, 50, 2), RED);
    }

    #[test]
    fn a_box_behind_the_eye_is_not_drawn() {
        let tree: Element = view! { <div class="stage"><div class="card" /></div> };
        let canvas = paint_text_css(
            &tree,
            ".stage { width: 100px; height: 100px; perspective: 100px; } \
             .card { width: 100px; height: 100px; background-color: #ff0000; \
                     transform: translateZ(150px); }",
            (200, 200),
        );
        for (x, y) in [(50, 50), (0, 0), (150, 150), (199, 199)] {
            assert_eq!(pixel_rgb(&canvas, x, y), WHITE, "at ({x}, {y})");
        }
    }

    #[test]
    fn a_hidden_back_is_not_painted_and_a_visible_one_is() {
        let tree: Element = view! { <div class="box" /> };
        let hidden = paint_text_css(
            &tree,
            ".box { width: 50px; height: 50px; background-color: #ff0000; \
                   backface-visibility: hidden; transform: rotateY(180deg); }",
            (60, 60),
        );
        assert_eq!(pixel_rgb(&hidden, 25, 25), WHITE);
        let visible = paint_text_css(
            &tree,
            ".box { width: 50px; height: 50px; background-color: #ff0000; \
                   transform: rotateY(180deg); }",
            (60, 60),
        );
        assert_eq!(pixel_rgb(&visible, 25, 25), RED);
        let front = paint_text_css(
            &tree,
            ".box { width: 50px; height: 50px; background-color: #ff0000; \
                   backface-visibility: hidden; }",
            (60, 60),
        );
        assert_eq!(pixel_rgb(&front, 25, 25), RED);
    }

    #[test]
    fn a_two_faced_card_shows_the_face_that_looks_at_the_viewer() {
        let tree: Element = view! {
            <div class="card">
                <div class="front" />
                <div class="back" />
            </div>
        };
        let css = |turn: &str| {
            format!(
                ".card {{ width: 50px; height: 50px; transform-style: preserve-3d; \
                         transform: {turn}; }} \
                 .front, .back {{ position: absolute; width: 50px; height: 50px; \
                                  backface-visibility: hidden; }} \
                 .front {{ background-color: #ff0000; }} \
                 .back {{ background-color: #0000ff; transform: rotateY(180deg); }}"
            )
        };
        let upright = paint_text_css(&tree, &css("none"), (60, 60));
        assert_eq!(pixel_rgb(&upright, 25, 25), RED);
        let flipped = paint_text_css(&tree, &css("rotateY(180deg)"), (60, 60));
        assert_eq!(pixel_rgb(&flipped, 25, 25), BLUE);
    }

    #[test]
    fn planes_in_a_3d_context_are_drawn_by_depth_not_by_tree_order() {
        let tree: Element = view! {
            <div class="ctx">
                <div class="near" />
                <div class="far" />
            </div>
        };
        let css = |style: &str| {
            format!(
                ".ctx {{ width: 60px; height: 60px; transform-style: {style}; }} \
                 .near, .far {{ position: absolute; width: 60px; height: 60px; }} \
                 .near {{ background-color: #ff0000; transform: translateZ(40px); }} \
                 .far {{ background-color: #0000ff; }}"
            )
        };
        let deep = paint_text_css(&tree, &css("preserve-3d"), (70, 70));
        assert_eq!(pixel_rgb(&deep, 30, 30), RED, "the nearer plane is on top");
        let flat = paint_text_css(&tree, &css("flat"), (70, 70));
        assert_eq!(
            pixel_rgb(&flat, 30, 30),
            BLUE,
            "flat, the later box is on top"
        );
    }

    #[test]
    fn crossing_planes_are_cut_where_they_meet() {
        let tree: Element = view! {
            <div class="ctx">
                <div class="flat" />
                <div class="upright" />
            </div>
        };
        let canvas = paint_text_css(
            &tree,
            ".ctx { width: 100px; height: 100px; perspective: 300px; \
                    transform-style: preserve-3d; } \
             .flat { position: absolute; width: 100px; height: 100px; \
                     background-color: #00ff00; } \
             .upright { position: absolute; width: 100px; height: 100px; \
                        background-color: #ff0000; transform: rotateY(60deg); }",
            (140, 100),
        );
        // The turned plane's left half is in front of the flat one and its
        // right half behind it, whatever the tree order says.
        assert_eq!(pixel_rgb(&canvas, 30, 50), RED);
        assert_eq!(pixel_rgb(&canvas, 70, 50), GREEN);
        // Tree order alone would paint the turned plane over the flat one.
        let flat_parent = paint_text_css(
            &tree,
            ".ctx { width: 100px; height: 100px; perspective: 300px; } \
             .flat { position: absolute; width: 100px; height: 100px; \
                     background-color: #00ff00; } \
             .upright { position: absolute; width: 100px; height: 100px; \
                        background-color: #ff0000; transform: rotateY(60deg); }",
            (140, 100),
        );
        assert_eq!(pixel_rgb(&flat_parent, 70, 50), RED);
    }

    #[test]
    fn a_plane_in_a_3d_context_keeps_its_own_opacity_and_a_flattening_property_flattens() {
        let tree: Element = view! {
            <div class="ctx">
                <div class="near" />
                <div class="far" />
            </div>
        };
        let css = |ctx_extra: &str, near_extra: &str| {
            format!(
                ".ctx {{ width: 60px; height: 60px; transform-style: preserve-3d; {ctx_extra} }} \
                 .near, .far {{ position: absolute; width: 60px; height: 60px; }} \
                 .near {{ background-color: #ff0000; transform: translateZ(40px); {near_extra} }} \
                 .far {{ background-color: #0000ff; }}"
            )
        };
        // Half-transparent red over the blue plane behind it.
        let faded = paint_text_css(&tree, &css("", "opacity: 0.5;"), (70, 70));
        let [r, g, b] = pixel_rgb(&faded, 30, 30);
        assert!(
            (125..=130).contains(&r) && g == 0 && (125..=130).contains(&b),
            "{r} {g} {b}"
        );
        // `overflow` on the context makes it flat again, so tree order rules.
        let clipped = paint_text_css(&tree, &css("overflow: hidden;", ""), (70, 70));
        assert_eq!(pixel_rgb(&clipped, 30, 30), BLUE);
        let translucent = paint_text_css(&tree, &css("opacity: 0.99;", ""), (70, 70));
        assert_ne!(pixel_rgb(&translucent, 30, 30), RED);
    }

    #[test]
    fn an_ancestors_clip_cuts_a_3d_context() {
        let tree: Element = view! {
            <div class="clip"><div class="ctx"><div class="plane" /></div></div>
        };
        let canvas = paint_text_css(
            &tree,
            ".clip { width: 40px; height: 40px; overflow: hidden; } \
             .ctx { width: 80px; height: 80px; transform-style: preserve-3d; } \
             .plane { width: 80px; height: 80px; background-color: #ff0000; \
                      transform: rotateX(20deg); }",
            (90, 90),
        );
        assert_eq!(pixel_rgb(&canvas, 20, 20), RED);
        assert_eq!(pixel_rgb(&canvas, 60, 20), WHITE);
        assert_eq!(pixel_rgb(&canvas, 20, 60), WHITE);
    }

    #[test]
    fn the_individual_translate_scale_and_rotate_axis_forms_apply() {
        let tree: Element = view! { <div class="box" /> };
        let moved = paint_text_css(
            &tree,
            ".box { width: 20px; height: 20px; background-color: #ff0000; translate: 30px 10px 99px; }",
            (80, 50),
        );
        assert_eq!(pixel_rgb(&moved, 40, 20), RED);
        assert_eq!(pixel_rgb(&moved, 5, 5), WHITE);
        let flattened = paint_text_css(
            &tree,
            ".box { width: 20px; height: 20px; background-color: #ff0000; scale: 1 1 0; }",
            (40, 40),
        );
        assert_eq!(pixel_rgb(&flattened, 10, 10), RED);
        let edge_on = paint_text_css(
            &tree,
            ".box { width: 20px; height: 20px; background-color: #ff0000; rotate: y 90deg; }",
            (40, 40),
        );
        assert_eq!(
            pixel_rgb(&edge_on, 10, 10),
            WHITE,
            "seen edge-on it has no area"
        );
    }

    #[test]
    fn projected_groups_paint_the_same_on_a_bounded_and_a_full_surface() {
        let tree: Element = view! {
            <div class="stage">
                <div class="card" />
                <div class="ctx"><div class="a" /><div class="b" /></div>
            </div>
        };
        let css = ".stage { width: 200px; height: 120px; perspective: 150px; } \
                   .card { position: absolute; left: 10px; top: 10px; width: 80px; height: 60px; \
                           background-color: #ff0000; transform: rotateY(40deg) rotateX(20deg); } \
                   .ctx { position: absolute; left: 110px; top: 20px; width: 60px; height: 60px; \
                          transform-style: preserve-3d; transform: rotateY(-30deg); } \
                   .a { position: absolute; width: 60px; height: 60px; background-color: #00aa00; } \
                   .b { position: absolute; width: 60px; height: 60px; background-color: #0000ff; \
                        transform: rotateY(90deg); }";
        assert_bounded_and_full_target_paint_within(2, || paint_text_css(&tree, css, (220, 130)));
    }

    #[test]
    fn a_3d_context_paints_the_inline_content_of_its_boxes_once() {
        let tree: Element = view! {
            <div class="ctx">"Hello "<span class="mark">"flowing words"</span>" end"</div>
        };
        let css = |style: &str| {
            format!(
                ".ctx {{ width: 200px; height: 40px; font-size: 16px; \
                         transform-style: {style}; }} \
                 .mark {{ background-color: #ffd0d0; color: #000000; }}"
            )
        };
        let flat = paint_text_css(&tree, &css("flat"), (220, 50));
        let preserved = paint_text_css(&tree, &css("preserve-3d"), (220, 50));
        same_pixels(&flat, &preserved, (220, 50));
    }

    #[test]
    fn backface_visibility_follows_the_accumulated_turn_only_inside_a_3d_context() {
        // Each case is what Chrome 153 draws for a red 100x100 child with
        // backface-visibility: hidden under a parent turned by 180deg.
        let tree: Element = view! { <div class="p"><div class="c" /></div> };
        let css = |parent_extra: &str, child_extra: &str| {
            format!(
                ".p {{ width: 100px; height: 100px; transform: rotateY(180deg); {parent_extra} }} \
                 .c {{ width: 100px; height: 100px; background-color: #ff0000; \
                       backface-visibility: hidden; {child_extra} }}"
            )
        };
        let flat = paint_text_css(&tree, &css("", ""), (110, 110));
        assert_eq!(
            pixel_rgb(&flat, 50, 50),
            RED,
            "a flat parent does not turn the child away"
        );
        let preserved =
            paint_text_css(&tree, &css("transform-style: preserve-3d;", ""), (110, 110));
        assert_eq!(pixel_rgb(&preserved, 50, 50), WHITE, "a 3D context does");
        let own_turn = paint_text_css(&tree, &css("", "transform: rotateY(180deg);"), (110, 110));
        assert_eq!(
            pixel_rgb(&own_turn, 50, 50),
            WHITE,
            "the child's own turn is judged on its own"
        );
    }

    #[test]
    fn a_transparent_plane_in_a_3d_context_paints_nothing() {
        let tree: Element = view! {
            <div class="ctx">
                <div class="base" />
                <div class="overlay" />
            </div>
        };
        let canvas = paint_text_css(
            &tree,
            ".ctx { width: 60px; height: 60px; transform-style: preserve-3d; } \
             .base, .overlay { position: absolute; width: 60px; height: 60px; } \
             .base { background-color: #0000ff; } \
             .overlay { background-color: #ff0000; opacity: 0; transform: translateZ(40px); }",
            (70, 70),
        );
        assert_eq!(pixel_rgb(&canvas, 30, 30), BLUE);
    }

    /// The color Chrome 153 paints at the middle of a 30x30 box over white for
    /// each source and `filter`; the rows were read off a screenshot.
    #[test]
    fn the_color_filters_give_the_colors_chrome_gives() {
        let rows: &[(&str, &str, [u8; 3])] = &[
            ("#3366cc", "grayscale(1)", [99, 99, 99]),
            ("#3366cc", "grayscale(0.5)", [75, 100, 151]),
            ("#3366cc", "grayscale(0.25)", [63, 101, 178]),
            ("#3366cc", "sepia(1)", [137, 122, 95]),
            ("#3366cc", "sepia(0.4)", [85, 110, 160]),
            ("#3366cc", "hue-rotate(30deg)", [110, 83, 214]),
            ("#3366cc", "hue-rotate(90deg)", [204, 62, 146]),
            ("#3366cc", "hue-rotate(180deg)", [146, 95, 0]),
            ("#3366cc", "hue-rotate(-60deg)", [0, 132, 110]),
            ("#3366cc", "invert(1)", [204, 153, 51]),
            ("#3366cc", "invert(0.3)", [97, 117, 158]),
            ("#3366cc", "opacity(0.5)", [153, 179, 230]),
            ("#3366cc", "opacity(0.2)", [214, 224, 245]),
            ("#3366cc", "grayscale(0.5) invert(0.5)", [128, 128, 128]),
            ("#3366cc", "sepia(0.5) hue-rotate(40deg)", [123, 103, 151]),
            ("rgba(204,102,51,0.6)", "grayscale(1)", [174, 174, 174]),
            ("rgba(204,102,51,0.6)", "grayscale(0.5)", [199, 168, 153]),
            ("rgba(204,102,51,0.6)", "sepia(1)", [203, 192, 172]),
            ("rgba(204,102,51,0.6)", "hue-rotate(90deg)", [133, 191, 124]),
            (
                "rgba(204,102,51,0.6)",
                "hue-rotate(180deg)",
                [124, 185, 215],
            ),
            ("rgba(204,102,51,0.6)", "invert(1)", [133, 194, 224]),
            ("rgba(204,102,51,0.6)", "invert(0.3)", [197, 172, 160]),
            ("rgba(204,102,51,0.6)", "opacity(0.5)", [240, 209, 194]),
            ("rgba(204,102,51,0.6)", "opacity(0.2)", [249, 237, 231]),
            (
                "rgba(204,102,51,0.6)",
                "grayscale(0.5) invert(0.5)",
                [178, 178, 178],
            ),
            (
                "rgba(204,102,51,0.6)",
                "sepia(0.5) hue-rotate(40deg)",
                [187, 187, 140],
            ),
            ("#ffffff", "sepia(1)", [255, 255, 239]),
            ("#ffffff", "sepia(0.4)", [255, 255, 249]),
            ("#ffffff", "invert(1)", [0, 0, 0]),
            ("#ffffff", "invert(0.3)", [178, 178, 178]),
            ("#ffffff", "sepia(0.5) hue-rotate(40deg)", [250, 255, 248]),
            ("#000000", "invert(1)", [255, 255, 255]),
            ("#000000", "invert(0.3)", [76, 76, 76]),
            ("#000000", "opacity(0.5)", [128, 128, 128]),
            ("#000000", "opacity(0.2)", [204, 204, 204]),
        ];
        let tree: Element = view! { <div class="b" /> };
        for (source, filter, [r, g, b]) in rows {
            let canvas = paint_text_css(
                &tree,
                &format!(
                    ".b {{ width: 30px; height: 30px; background: {source}; filter: {filter}; }}"
                ),
                (40, 40),
            );
            let got = pixel_rgb(&canvas, 15, 15);
            let near = |a: u8, e: u8| a.abs_diff(e) <= 1;
            assert!(
                near(got[0], *r) && near(got[1], *g) && near(got[2], *b),
                "{source} {filter}: got {got:?}, Chrome gives {:?}",
                [r, g, b]
            );
        }
    }

    #[test]
    fn a_drop_shadow_falls_off_as_chromes_does() {
        let tree: Element = view! { <div class="b" /> };
        let canvas = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background: #ff0000; \
                 filter: drop-shadow(6px 6px 4px #000000); }",
            (140, 90),
        );
        // Chrome 153, along a row through the middle of the box and past its
        // right edge at x = 100 (the shadow's own edge is at 106).
        let chrome: &[(u32, u8)] = &[
            (100, 25),
            (101, 37),
            (102, 53),
            (103, 72),
            (104, 93),
            (106, 139),
            (108, 183),
            (110, 218),
            (112, 240),
            (114, 251),
        ];
        for &(x, expected) in chrome {
            let [r, g, b] = pixel_rgb(&canvas, x, 25);
            assert!(
                r == g && g == b,
                "a black shadow is gray at {x}: {r} {g} {b}"
            );
            assert!(
                r.abs_diff(expected) <= 6,
                "x = {x}: got {r}, Chrome gives {expected}"
            );
        }
        // And the same down the bottom edge.
        for &(offset, expected) in chrome {
            let y = offset - 100 + 50;
            let got = pixel_rgb(&canvas, 50, y)[0];
            assert!(
                got.abs_diff(expected) <= 6,
                "y = {y}: got {got}, Chrome gives {expected}"
            );
        }
        // The box itself is untouched and stays in front.
        assert_eq!(pixel_rgb(&canvas, 50, 25), RED);
        // Nothing above or to the left of the box: the shadow only falls
        // down and to the right.
        assert_eq!(pixel_rgb(&canvas, 50, 0)[0], 255);
    }

    #[test]
    fn a_drop_shadow_of_a_translucent_color_and_a_second_function_compose_in_order() {
        let tree: Element = view! { <div class="b" /> };
        let half = paint_text_css(
            &tree,
            ".b { width: 20px; height: 20px; background: #ff0000; \
                 filter: drop-shadow(10px 0 0 rgba(0, 0, 255, 0.5)); }",
            (50, 30),
        );
        // A half-transparent blue shadow over white, with no blur (Chrome 153
        // gives 127, 127, 255; 25% gives 191 and 80% gives 51).
        assert_eq!(pixel_rgb(&half, 25, 10), [127, 127, 255]);
        // `invert()` after it also inverts the shadow; before it, only the box.
        let after = paint_text_css(
            &tree,
            ".b { width: 20px; height: 20px; background: #ff0000; \
                 filter: drop-shadow(10px 0 0 #0000ff) invert(1); }",
            (50, 30),
        );
        assert_eq!(pixel_rgb(&after, 5, 10), [0, 255, 255]);
        assert_eq!(pixel_rgb(&after, 25, 10), [255, 255, 0]);
        let before = paint_text_css(
            &tree,
            ".b { width: 20px; height: 20px; background: #ff0000; \
                 filter: invert(1) drop-shadow(10px 0 0 #0000ff); }",
            (50, 30),
        );
        assert_eq!(pixel_rgb(&before, 5, 10), [0, 255, 255]);
        assert_eq!(pixel_rgb(&before, 25, 10), [0, 0, 255]);
    }

    #[test]
    fn a_drop_shadow_scales_with_the_device_pixel_ratio() {
        let css = ".box { background-color: #ff0000; filter: drop-shadow(4px 0 0 #0000ff); }";
        let one = single_box_buffer(css, 40, 1.0);
        let two = single_box_buffer(css, 80, 2.0);
        // The box is 10px wide (20 device pixels at 2x) on a black canvas; its
        // blue shadow reaches 4px (8 device pixels) past it.
        assert_eq!(pixel_rgb(&one, 12, 5), [0, 0, 255]);
        assert_eq!(pixel_rgb(&one, 15, 5), [0, 0, 0]);
        assert_eq!(pixel_rgb(&two, 26, 10), [0, 0, 255]);
        assert_eq!(pixel_rgb(&two, 29, 10), [0, 0, 0]);
    }

    #[test]
    fn the_surface_of_a_filtered_group_has_room_for_its_drop_shadow() {
        let tree: Element = view! { <div class="b" /> };
        // Far from the canvas edge, so the shadow must come from the group's
        // own surface, which has to be sized for it.
        let canvas = paint_text_css(
            &tree,
            ".b { margin: 30px; width: 20px; height: 20px; background: #ff0000; \
                 filter: drop-shadow(0 0 6px #000000); }",
            (80, 80),
        );
        let [r, ..] = pixel_rgb(&canvas, 28, 40);
        assert!(r < 250, "the shadow reaches past the box: {r}");
        let [r, ..] = pixel_rgb(&canvas, 14, 40);
        assert!(r >= 250, "but not as far as 16px: {r}");
    }

    #[test]
    fn a_filtered_group_paints_the_same_on_a_bounded_and_a_full_surface() {
        let tree: Element = view! { <div class="b" /> };
        for filter in [
            "drop-shadow(8px 8px 6px #000000)",
            "drop-shadow(-12px 4px 0 #0000ff) grayscale(0.5)",
            "sepia(0.7) drop-shadow(0 0 10px #ff0000)",
        ] {
            let css = format!(
                ".b {{ margin: 120px; width: 30px; height: 20px; background: #3366cc; filter: {filter}; }}"
            );
            assert_bounded_and_full_target_paint_within(1, || {
                paint_text_css(&tree, &css, (300, 300))
            });
        }
    }

    fn clipped(clip: &str, size: (u32, u32)) -> Canvas {
        let tree: Element = view! { <div class="b" /> };
        paint_text_css(
            &tree,
            &format!(
                ".b {{ width: 100px; height: 50px; background-color: #ff0000; clip-path: {clip}; }}"
            ),
            size,
        )
    }

    #[test]
    fn a_circle_clip_is_antialiased_as_chromes_is() {
        let canvas = clipped("circle(25px at 50% 50%)", (110, 60));
        // Chrome 153 (green channel, so 255 is white and 0 is red), along the
        // middle row of a box placed at 20 and down its middle column.
        for &((x, y), chrome) in &[
            ((24u32, 25u32), 255u8),
            ((25, 25), 15),
            ((26, 25), 0),
            ((73, 25), 0),
            ((74, 25), 15),
            ((75, 25), 255),
            ((50, 0), 12),
            ((50, 1), 0),
        ] {
            let got = pixel_rgb(&canvas, x, y)[1];
            assert!(
                got.abs_diff(chrome) <= 16,
                "({x}, {y}): got {got}, Chrome gives {chrome}"
            );
        }
        // Outside it the box does not paint, inside it does.
        assert_eq!(pixel_rgb(&canvas, 2, 2), WHITE);
        assert_eq!(pixel_rgb(&canvas, 97, 47), WHITE);
        assert_eq!(pixel_rgb(&canvas, 50, 25), RED);
    }

    #[test]
    fn a_polygon_clip_is_half_covered_on_a_diagonal_pixel() {
        let canvas = clipped("polygon(0 0, 100% 0, 50% 100%)", (110, 60));
        // Chrome 153: the left edge of the triangle crosses the middle row at
        // x = 25 exactly, so that pixel is covered by half (G = 127). The mask is
        // filled by tiny-skia, whose supersampling rounds that tie to 6/16 (G =
        // 159) where Chrome's analytic edge gives 8/16: an edge pixel differs by
        // about an eighth of coverage, nothing more.
        let g = pixel_rgb(&canvas, 25, 25)[1];
        assert!(g.abs_diff(127) <= 40, "{g}");
        assert_eq!(pixel_rgb(&canvas, 20, 25), WHITE);
        assert_eq!(pixel_rgb(&canvas, 30, 25), RED);
        assert_eq!(pixel_rgb(&canvas, 5, 40), WHITE);
        assert_eq!(pixel_rgb(&canvas, 50, 45), RED);
        let g = pixel_rgb(&canvas, 74, 25)[1];
        assert!(g.abs_diff(127) <= 40, "{g}");
    }

    #[test]
    fn an_inset_clip_rounds_its_corners_as_chromes_does() {
        let canvas = clipped("inset(10px 20px round 8px)", (110, 60));
        // Chrome 153 (green channel) around the top left corner of the rect at
        // x 20, y 10, with a radius of 8.
        for &((x, y), chrome) in &[
            ((24u32, 10u32), 199u8),
            ((26, 10), 61),
            ((28, 10), 0),
            ((22, 11), 247),
            ((23, 11), 118),
            ((24, 11), 3),
            ((20, 14), 197),
            ((20, 16), 55),
            ((20, 18), 0),
        ] {
            let got = pixel_rgb(&canvas, x, y)[1];
            assert!(
                got.abs_diff(chrome) <= 40,
                "({x}, {y}): got {got}, Chrome gives {chrome}"
            );
        }
        assert_eq!(pixel_rgb(&canvas, 50, 25), RED);
        assert_eq!(pixel_rgb(&canvas, 10, 25), WHITE);
        assert_eq!(pixel_rgb(&canvas, 50, 5), WHITE);
    }

    #[test]
    fn a_clip_cuts_the_children_the_borders_and_the_shadow_of_a_box() {
        let tree: Element = view! { <div class="b"><div class="kid" /></div> };
        let canvas = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background-color: #ff0000; border: 6px solid #0000ff; \
                 box-shadow: 8px 8px 0 #00aa00; box-sizing: border-box; clip-path: inset(0 50% 0 0); } \
             .kid { width: 120px; height: 20px; background-color: #ffff00; }",
            (130, 70),
        );
        // The left half is drawn (the border, the child), the right half is gone,
        // and so is the shadow that fell outside the box.
        assert_eq!(pixel_rgb(&canvas, 2, 25), [0, 0, 255]);
        assert_eq!(pixel_rgb(&canvas, 20, 10), [255, 255, 0]);
        assert_eq!(pixel_rgb(&canvas, 70, 10), WHITE);
        assert_eq!(pixel_rgb(&canvas, 70, 56), WHITE);
        assert_eq!(pixel_rgb(&canvas, 20, 56), WHITE);
    }

    #[test]
    fn an_empty_clip_clears_the_box_and_a_box_clip_keeps_a_rounded_outline() {
        let none = clipped("inset(30px 0 30px 0)", (110, 60));
        assert_eq!(pixel_rgb(&none, 50, 25), WHITE);
        let tree: Element = view! { <div class="b" /> };
        let rounded = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background-color: #ff0000; border-radius: 30px; \
                 clip-path: content-box; padding: 10px; box-sizing: border-box; }",
            (110, 60),
        );
        // The content box (80x30) is inset by 10px and rounded by the 20px left of
        // a 30px radius, so the corner of the box is empty and the middle is not.
        assert_eq!(pixel_rgb(&rounded, 50, 25), RED);
        assert_eq!(pixel_rgb(&rounded, 11, 11), WHITE);
        assert_eq!(pixel_rgb(&rounded, 5, 25), WHITE);
    }

    #[test]
    fn a_clip_follows_the_transform_of_its_box_and_comes_before_its_opacity() {
        let tree: Element = view! { <div class="b" /> };
        let canvas = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background-color: #ff0000; \
                 clip-path: inset(0 50% 0 0); transform: translate(30px, 0); opacity: 0.5; }",
            (160, 60),
        );
        // The left half of the box, moved 30px right and half faded.
        assert_eq!(pixel_rgb(&canvas, 20, 25), WHITE);
        let [r, g, b] = pixel_rgb(&canvas, 50, 25);
        assert!(
            r == 255 && (126..=129).contains(&g) && g == b,
            "{r} {g} {b}"
        );
        assert_eq!(pixel_rgb(&canvas, 100, 25), WHITE);
    }

    #[test]
    fn a_clipped_group_paints_the_same_on_a_bounded_and_a_full_surface() {
        let tree: Element = view! { <div class="b"><div class="kid" /></div> };
        for clip in [
            "circle(30px at 40% 50%)",
            "polygon(evenodd, 0 0, 100% 0, 0 100%, 100% 100%)",
            "inset(5px round 12px)",
        ] {
            let css = format!(
                ".b {{ margin: 90px; width: 100px; height: 60px; background: #3366cc; \
                      clip-path: {clip}; }} .kid {{ width: 40px; height: 40px; background: #ff0000; }}"
            );
            assert_bounded_and_full_target_paint_within(1, || {
                paint_text_css(&tree, &css, (300, 300))
            });
        }
    }

    /// A 100x50 red box at the origin with `declarations` on it.
    fn masked(declarations: &str, size: (u32, u32)) -> Canvas {
        let tree: Element = view! { <div class="b" /> };
        paint_text_css(
            &tree,
            &format!(
                ".b {{ width: 100px; height: 50px; background-color: #ff0000; {declarations} }}"
            ),
            size,
        )
    }

    /// The green channel at `(x, y)`: 255 where the red box is not and 0 where it is.
    fn g(canvas: &Canvas, x: u32, y: u32) -> u8 {
        pixel_rgb(canvas, x, y)[1]
    }

    fn assert_g(canvas: &Canvas, points: &[(u32, u32, u8)], tolerance: u8) {
        for &(x, y, chrome) in points {
            let got = g(canvas, x, y);
            assert!(
                got.abs_diff(chrome) <= tolerance,
                "({x}, {y}): got {got}, Chrome gives {chrome}"
            );
        }
    }

    #[test]
    fn an_alpha_mask_fades_the_box_as_chrome_does() {
        let canvas = masked(
            "mask-image: linear-gradient(to right, #000, transparent);",
            (110, 60),
        );
        // Chrome 153, along the middle row.
        assert_g(
            &canvas,
            &[
                (0, 25, 1),
                (10, 25, 27),
                (25, 25, 65),
                (50, 25, 129),
                (75, 25, 193),
                (90, 25, 231),
                (99, 25, 254),
            ],
            4,
        );
    }

    #[test]
    fn luminance_masks_by_the_srgb_luminance_times_the_alpha() {
        let ramp = "mask-image: linear-gradient(to right, #000, #fff);";
        let luminance = masked(&format!("{ramp} mask-mode: luminance;"), (110, 60));
        assert_g(
            &luminance,
            &[
                (0, 25, 254),
                (10, 25, 228),
                (25, 25, 190),
                (50, 25, 126),
                (75, 25, 63),
                (90, 25, 24),
                (99, 25, 1),
            ],
            4,
        );
        // The same ramp by its alpha is opaque everywhere, with or without `alpha`.
        for mode in ["mask-mode: alpha;", ""] {
            let alpha = masked(&format!("{ramp} {mode}"), (110, 60));
            assert_g(&alpha, &[(0, 25, 0), (50, 25, 0), (99, 25, 0)], 0);
        }
        // Pure red is 0.2126 of the way: G = 255 * (1 - 0.2126).
        let red = masked(
            "mask-image: linear-gradient(#f00, #f00); mask-mode: luminance;",
            (110, 60),
        );
        assert_g(&red, &[(10, 25, 201), (50, 25, 201)], 1);
    }

    #[test]
    fn a_mask_tile_is_sized_placed_and_repeated_as_a_background_is() {
        let tiled = masked(
            "mask-image: linear-gradient(#000, transparent); mask-size: 20px 25px; mask-repeat: repeat;",
            (110, 60),
        );
        assert_g(
            &tiled,
            &[
                (20, 0, 5),
                (20, 5, 56),
                (20, 10, 107),
                (20, 20, 209),
                (20, 24, 250),
                (20, 25, 5),
                (20, 30, 56),
            ],
            4,
        );
        let corner = masked(
            "mask-image: linear-gradient(#000, #000); mask-size: 40px 20px; mask-repeat: no-repeat; \
             mask-position: 100% 100%;",
            (110, 60),
        );
        assert_g(
            &corner,
            &[
                (10, 40, 255),
                (59, 40, 255),
                (60, 40, 0),
                (99, 40, 0),
                (80, 29, 255),
            ],
            0,
        );
        let space = masked(
            "mask-image: linear-gradient(#000, #000); mask-size: 30px 50px; mask-repeat: space no-repeat;",
            (110, 60),
        );
        assert_g(
            &space,
            &[
                (0, 25, 0),
                (29, 25, 0),
                (30, 25, 255),
                (35, 25, 0),
                (64, 25, 0),
                (65, 25, 255),
                (70, 25, 0),
            ],
            0,
        );
        let round = masked(
            "mask-image: linear-gradient(#000, #000); mask-size: 30px 50px; mask-repeat: round no-repeat;",
            (110, 60),
        );
        assert_g(
            &round,
            &[(0, 25, 0), (30, 25, 0), (60, 25, 0), (99, 25, 0)],
            0,
        );
        let edge = masked(
            "mask-image: linear-gradient(#000, #000); mask-position: right 10px bottom 5px; \
             mask-size: 30px 20px; mask-repeat: no-repeat;",
            (110, 60),
        );
        assert_g(
            &edge,
            &[
                (59, 30, 255),
                (60, 30, 0),
                (89, 30, 0),
                (90, 30, 255),
                (70, 24, 255),
                (70, 25, 0),
                (70, 44, 0),
                (70, 45, 255),
            ],
            0,
        );
    }

    #[test]
    fn mask_clip_and_origin_pick_the_boxes() {
        let padding = masked(
            "border: 8px solid #0000ff; box-sizing: border-box; \
             mask-image: linear-gradient(#000, #000); mask-clip: padding-box;",
            (110, 60),
        );
        // The border (8px) is cut away and the padding box shows.
        assert_g(
            &padding,
            &[
                (0, 25, 255),
                (7, 25, 255),
                (8, 25, 0),
                (91, 25, 0),
                (92, 25, 255),
                (99, 25, 255),
            ],
            0,
        );
        let content = masked(
            "padding: 10px; box-sizing: border-box; mask-image: linear-gradient(#000, #000); \
             mask-origin: content-box; mask-clip: border-box; mask-repeat: no-repeat; mask-size: 50% 50%;",
            (110, 60),
        );
        // A 40x15 tile at the content box's corner, (10, 10).
        assert_g(
            &content,
            &[
                (9, 20, 255),
                (10, 20, 0),
                (49, 20, 0),
                (50, 20, 255),
                (20, 9, 255),
                (20, 24, 0),
                (20, 25, 255),
            ],
            0,
        );
        // A shadow outside the border box is cut by the default `border-box`
        // clip, and `margin-box` is the border box too (Chrome 153); `no-clip`
        // lets a mask that covers it show it.
        let shadow = "box-shadow: 12px 12px 0 #00aa00;";
        let cut = masked(
            &format!("{shadow} mask-image: linear-gradient(#000, #000);"),
            (130, 80),
        );
        assert_eq!(pixel_rgb(&cut, 105, 25), WHITE);
        assert_eq!(pixel_rgb(&cut, 50, 55), WHITE);
        let margin = masked(
            &format!(
                "{shadow} mask-image: linear-gradient(#000, #000); mask-clip: margin-box; mask-origin: margin-box;"
            ),
            (130, 80),
        );
        assert_eq!(pixel_rgb(&margin, 105, 25), WHITE);
        let free = masked(
            &format!(
                "{shadow} mask-image: linear-gradient(#000, #000); mask-size: 200px 100px; \
                 mask-repeat: no-repeat; mask-position: -20px -10px; mask-clip: no-clip;"
            ),
            (130, 80),
        );
        assert_eq!(pixel_rgb(&free, 105, 25), [0, 170, 0]);
        assert_eq!(pixel_rgb(&free, 111, 25), [0, 170, 0]);
        assert_eq!(pixel_rgb(&free, 113, 25), WHITE);
    }

    /// A backdrop box and a box with `declarations` over its right half; the
    /// pixel where they overlap.
    fn blended(source: &str, declarations: &str) -> [u8; 3] {
        let tree: Element = view! { <div class="r"><div class="a" /><div class="b" /></div> };
        let canvas = paint_text_css(
            &tree,
            &format!(
                ".r {{ position: relative; width: 90px; height: 40px; }} \
                 .a {{ position: absolute; left: 0; top: 0; width: 60px; height: 40px; \
                 background-color: rgb(64, 128, 192); }} \
                 .b {{ position: absolute; left: 30px; top: 0; width: 60px; height: 40px; \
                 background-color: {source}; {declarations} }}"
            ),
            (100, 50),
        );
        pixel_rgb(&canvas, 45, 20)
    }

    #[test]
    fn mix_blend_mode_matches_chrome_for_every_mode() {
        // Chrome 153: backdrop rgb(64, 128, 192) under rgb(192, 96, 32).
        let source = "rgb(192, 96, 32)";
        for (mode, chrome) in [
            ("normal", [192, 96, 32]),
            ("multiply", [48, 48, 24]),
            ("screen", [208, 176, 200]),
            ("overlay", [96, 97, 145]),
            ("darken", [64, 96, 32]),
            ("lighten", [192, 128, 192]),
            ("color-dodge", [255, 205, 220]),
            ("color-burn", [1, 0, 0]),
            ("hard-light", [160, 96, 48]),
            ("soft-light", [96, 112, 156]),
            ("difference", [128, 32, 160]),
            ("exclusion", [160, 128, 176]),
            ("hue", [175, 98, 47]),
            ("saturation", [51, 131, 211]),
            ("color", [190, 94, 30]),
            ("luminosity", [66, 130, 194]),
            ("plus-lighter", [255, 224, 224]),
        ] {
            let got = blended(source, &format!("mix-blend-mode: {mode};"));
            for channel in 0..3 {
                assert!(
                    got[channel].abs_diff(chrome[channel]) <= 2,
                    "{mode}: got {got:?}, Chrome gives {chrome:?}"
                );
            }
        }
    }

    #[test]
    fn a_translucent_blended_box_mixes_by_its_alpha() {
        // Chrome 153: the same colours with the source at half alpha.
        let source = "rgba(192, 96, 32, 0.5)";
        for (mode, chrome) in [
            ("normal", [128, 112, 112]),
            ("multiply", [56, 88, 108]),
            ("screen", [136, 152, 196]),
        ] {
            let got = blended(source, &format!("mix-blend-mode: {mode};"));
            for channel in 0..3 {
                assert!(
                    got[channel].abs_diff(chrome[channel]) <= 2,
                    "{mode}: got {got:?}, Chrome gives {chrome:?}"
                );
            }
        }
    }

    /// The backdrop box under a wrapper (with `wrapper`) holding a multiplied box.
    fn blended_in_wrapper(wrapper: &str) -> [u8; 3] {
        let tree: Element =
            view! { <div class="r"><div class="a" /><div class="w"><div class="b" /></div></div> };
        let canvas = paint_text_css(
            &tree,
            &format!(
                ".r {{ position: relative; width: 90px; height: 40px; }} \
                 .a {{ position: absolute; left: 0; top: 0; width: 60px; height: 40px; \
                 background-color: rgb(64, 128, 192); }} \
                 .w {{ position: absolute; left: 0; top: 0; width: 90px; height: 40px; {wrapper} }} \
                 .b {{ position: absolute; left: 30px; top: 0; width: 60px; height: 40px; \
                 background-color: rgb(192, 96, 32); mix-blend-mode: multiply; }}"
            ),
            (100, 50),
        );
        pixel_rgb(&canvas, 45, 20)
    }

    #[test]
    fn a_stacking_context_keeps_what_blends_inside_it_to_itself() {
        let multiplied = [48, 48, 24];
        // Nothing isolates the box: it multiplies with the backdrop.
        assert_eq!(blended_in_wrapper(""), multiplied);
        // Each of these makes the wrapper a stacking context, which blends
        // against its own (empty) content instead: the box keeps its colour.
        for wrapper in [
            "z-index: 1;",
            "isolation: isolate;",
            "opacity: 0.99;",
            "transform: translateX(0.5px);",
        ] {
            let got = blended_in_wrapper(wrapper);
            for channel in 0..3 {
                let kept = [192u8, 96, 32][channel];
                assert!(
                    got[channel].abs_diff(kept) <= 3,
                    "{wrapper}: got {got:?}, expected the box's own colour"
                );
            }
        }
    }

    /// The colour at the middle of a 30x30 box with `declarations`.
    fn background_blended(declarations: &str) -> [u8; 3] {
        let tree: Element = view! { <div class="b" /> };
        let canvas = paint_text_css(
            &tree,
            &format!(".b {{ width: 30px; height: 30px; {declarations} }}"),
            (40, 40),
        );
        pixel_rgb(&canvas, 15, 15)
    }

    #[test]
    fn background_blend_mode_matches_chrome() {
        let flat = |color: &str| format!("linear-gradient({color}, {color})");
        let (orange, blue) = ("rgb(192, 96, 32)", "rgb(64, 128, 192)");
        let cases = [
            // A layer is blended with the colour under it.
            (
                format!(
                    "background-color: {blue}; background-image: {}; background-blend-mode: multiply;",
                    flat(orange)
                ),
                [48, 48, 24],
            ),
            (
                format!(
                    "background-color: {blue}; background-image: {}; background-blend-mode: hue;",
                    flat(orange)
                ),
                [175, 98, 47],
            ),
            // And with the layer below it.
            (
                format!(
                    "background-image: {}, {}; background-blend-mode: difference, normal;",
                    flat(orange),
                    flat(blue)
                ),
                [128, 32, 160],
            ),
            (
                format!(
                    "background-color: rgb(255, 0, 0); background-image: {}, {}; \
                     background-blend-mode: multiply, normal;",
                    flat(orange),
                    flat(blue)
                ),
                [48, 48, 24],
            ),
            // The bottom layer's mode blends it with the colour, and the layer above covers it.
            (
                format!(
                    "background-color: rgb(255, 0, 0); background-image: {}, {}; \
                     background-blend-mode: normal, multiply;",
                    flat(orange),
                    flat(blue)
                ),
                [192, 96, 32],
            ),
            (
                format!(
                    "background-color: {blue}; background-image: {}; background-blend-mode: multiply;",
                    flat("rgba(192, 96, 32, 0.5)")
                ),
                [56, 88, 108],
            ),
        ];
        for (declarations, chrome) in cases {
            let got = background_blended(&declarations);
            for channel in 0..3 {
                assert!(
                    got[channel].abs_diff(chrome[channel]) <= 2,
                    "{declarations}: got {got:?}, Chrome gives {chrome:?}"
                );
            }
        }
    }

    #[test]
    fn background_blend_mode_stays_inside_the_box_and_its_rounding() {
        let tree: Element = view! { <div class="b" /> };
        let canvas = paint_text_css(
            &tree,
            ".b { width: 30px; height: 30px; border-radius: 15px; \
             background-color: rgb(64, 128, 192); \
             background-image: linear-gradient(rgb(192, 96, 32), rgb(192, 96, 32)); \
             background-blend-mode: multiply; }",
            (40, 40),
        );
        assert_eq!(pixel_rgb(&canvas, 15, 15), [48, 48, 24]);
        // The corner is outside the rounding and the page shows through; the
        // blend does not reach the white page behind the box.
        assert_eq!(pixel_rgb(&canvas, 2, 2), WHITE);
        assert_eq!(pixel_rgb(&canvas, 35, 35), WHITE);
    }

    /// `inner` in a box with `declarations`, on a white page.
    fn text_clipped(tree: &Element, css: &str) -> Canvas {
        paint_text_css(tree, css, (260, 110))
    }

    fn assert_same_canvas(a: &Canvas, b: &Canvas, what: &str) {
        let mut worst = 0u8;
        for (x, y) in (0..110).flat_map(|y| (0..260).map(move |x| (x, y))) {
            let (p, q) = (pixel_rgb(a, x, y), pixel_rgb(b, x, y));
            for channel in 0..3 {
                worst = worst.max(p[channel].abs_diff(q[channel]));
            }
        }
        assert!(worst <= 2, "{what}: differs by {worst}");
    }

    #[test]
    fn background_clip_text_shows_the_layer_where_the_glyphs_are() {
        let flat = "linear-gradient(#ff0000, #ff0000)";
        let font = "font-size: 64px; font-weight: 700; line-height: 80px; padding: 10px;";
        // A block with text of its own: the same pixels as that text in the layer's colour.
        let tree: Element = view! { <div class="t">"Mg"</div> };
        let clipped = text_clipped(
            &tree,
            &format!(
                ".t {{ {font} background-image: {flat}; -webkit-background-clip: text; color: transparent; }}"
            ),
        );
        let plain = text_clipped(&tree, &format!(".t {{ {font} color: #ff0000; }}"));
        assert_same_canvas(&clipped, &plain, "block");
        let inked = (0..110)
            .flat_map(|y| (0..260).map(move |x| (x, y)))
            .filter(|&(x, y)| pixel_rgb(&clipped, x, y) != WHITE)
            .count();
        assert!(inked > 300, "the glyphs show: {inked} pixels");
        // Without the clip the layer fills the whole box and the text is invisible.
        let filled = text_clipped(
            &tree,
            &format!(".t {{ {font} background-image: {flat}; color: transparent; }}"),
        );
        assert_eq!(pixel_rgb(&filled, 100, 20), [255, 0, 0]);
        assert_eq!(pixel_rgb(&clipped, 100, 20), pixel_rgb(&plain, 100, 20));
        // Children's text counts, and what is not text does not.
        let nested: Element = view! { <div class="t"><span class="s">"Mg"</span></div> };
        let through = text_clipped(
            &nested,
            &format!(
                ".t {{ {font} background-image: {flat}; background-clip: text; color: transparent; }}"
            ),
        );
        assert_same_canvas(&through, &plain, "children");
    }

    #[test]
    fn background_clip_text_on_an_inline_element_cuts_to_its_own_glyphs() {
        let flat = "linear-gradient(#ff0000, #ff0000)";
        let font = "font-size: 48px; font-weight: 700; line-height: 70px;";
        let tree: Element = view! { <p class="p">"ab "<span class="s">"Mg"</span>" cd"</p> };
        let clipped = text_clipped(
            &tree,
            &format!(
                ".p {{ margin: 0; padding: 10px; {font} color: #0000ff; }} \
                 .s {{ background-image: {flat}; -webkit-background-clip: text; color: transparent; }}"
            ),
        );
        // The same line with the span's text just in red.
        let plain = text_clipped(
            &tree,
            &format!(
                ".p {{ margin: 0; padding: 10px; {font} color: #0000ff; }} .s {{ color: #ff0000; }}"
            ),
        );
        assert_same_canvas(&clipped, &plain, "inline");
    }

    #[test]
    fn background_clip_text_on_an_inline_element_reaches_the_elements_in_it() {
        let flat = "linear-gradient(#ff0000, #ff0000)";
        let font = "font-size: 48px; font-weight: 700; line-height: 70px;";
        let tree: Element =
            view! { <p class="p">"ab "<span class="s"><span class="n">"Mg"</span></span>" cd"</p> };
        let clipped = text_clipped(
            &tree,
            &format!(
                ".p {{ margin: 0; padding: 10px; {font} color: #0000ff; }} \
                 .s {{ background-image: {flat}; background-clip: text; color: transparent; }} \
                 .n {{ font-style: normal; }}"
            ),
        );
        let plain = text_clipped(
            &tree,
            &format!(
                ".p {{ margin: 0; padding: 10px; {font} color: #0000ff; }} .s {{ color: #ff0000; }} \
                 .n {{ font-style: normal; }}"
            ),
        );
        assert_same_canvas(&clipped, &plain, "nested inline");
    }

    #[test]
    fn background_clip_text_ignores_the_opacity_of_what_is_in_the_box() {
        let flat = "linear-gradient(#ff0000, #ff0000)";
        let font = "font-size: 64px; font-weight: 700; line-height: 80px; padding: 10px;";
        let tree: Element = view! { <div class="t"><span class="s">"Mg"</span></div> };
        let faded = text_clipped(
            &tree,
            &format!(
                ".t {{ {font} background-image: {flat}; background-clip: text; color: transparent; }} \
                 .s {{ opacity: 0.3; filter: blur(0px); }}"
            ),
        );
        let plain = text_clipped(&tree, &format!(".t {{ {font} color: #ff0000; }}"));
        let inside = (0..110)
            .flat_map(|y| (0..260).map(move |x| (x, y)))
            .find(|&(x, y)| pixel_rgb(&plain, x, y) == [255, 0, 0])
            .unwrap();
        assert_eq!(pixel_rgb(&faded, inside.0, inside.1), [255, 0, 0]);
    }

    #[test]
    fn text_clipped_and_whole_box_layers_blend_with_each_other_as_chrome_does() {
        let a = "linear-gradient(rgb(208, 80, 64), rgb(208, 80, 64))";
        let b = "linear-gradient(rgb(64, 128, 192), rgb(64, 128, 192))";
        let font = "font-size: 64px; font-weight: 700; line-height: 80px; padding: 10px; \
                    color: transparent; background-color: #8080ff;";
        let tree: Element = view! { <div class="t">"Mg"</div> };
        let plain = text_clipped(
            &tree,
            ".t { font-size: 64px; font-weight: 700; line-height: 80px; padding: 10px; color: #ff0000; }",
        );
        let inside = (0..110)
            .flat_map(|y| (0..260).map(move |x| (x, y)))
            .find(|&(x, y)| pixel_rgb(&plain, x, y) == [255, 0, 0])
            .unwrap();
        let outside = (130, 15);
        // Chrome 153, a layer of each kind: `(layers, clips, blends)` and what is
        // inside a glyph and outside it.
        let cases = [
            // The text layer multiplies with the box layer under it.
            (
                "text, border-box",
                "multiply, normal",
                [52, 40, 48],
                [64, 128, 192],
            ),
            // The box layer multiplies with the text layer under it, and with
            // nothing outside the glyphs, where only the layer shows.
            (
                "border-box, text",
                "multiply, normal",
                [52, 40, 48],
                [208, 80, 64],
            ),
            // The bottom box layer blends with the colour, which it covers.
            (
                "text, border-box",
                "normal, multiply",
                [208, 80, 64],
                [32, 64, 192],
            ),
        ];
        for (clips, blends, glyph, around) in cases {
            let canvas = text_clipped(
                &tree,
                &format!(
                    ".t {{ {font} background-image: {a}, {b}; background-clip: {clips}; \
                     background-blend-mode: {blends}; }}"
                ),
            );
            for (name, point, chrome) in [("glyph", inside, glyph), ("outside", outside, around)] {
                let got = pixel_rgb(&canvas, point.0, point.1);
                for channel in 0..3 {
                    assert!(
                        got[channel].abs_diff(chrome[channel]) <= 2,
                        "{clips} / {blends}, {name}: got {got:?}, Chrome gives {chrome:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn background_clip_text_follows_the_shorthand_and_blend_modes() {
        let flat = "linear-gradient(#ff0000, #ff0000)";
        let font = "font-size: 64px; font-weight: 700; line-height: 80px; padding: 10px;";
        let tree: Element = view! { <div class="t">"Mg"</div> };
        let plain = text_clipped(&tree, &format!(".t {{ {font} color: #ff0000; }}"));
        let shorthand = text_clipped(
            &tree,
            &format!(".t {{ {font} background: {flat} text; color: transparent; }}"),
        );
        assert_same_canvas(&shorthand, &plain, "shorthand");
        // A text layer blends with what is under it, which is the layer below and
        // the colour (Chrome 153: the colour, the 50% green over it, then the red
        // multiplied is rgb(52, 60, 32)).
        let multiplied = text_clipped(
            &tree,
            &format!(
                ".t {{ {font} color: transparent; background-color: #8080ff; \
                 background-image: linear-gradient(rgb(208, 80, 64), rgb(208, 80, 64)), \
                 linear-gradient(rgba(0, 255, 0, 0.5), rgba(0, 255, 0, 0.5)); \
                 background-clip: text; background-blend-mode: multiply, normal; }}"
            ),
        );
        let inside = (0..110)
            .flat_map(|y| (0..260).map(move |x| (x, y)))
            .find(|&(x, y)| pixel_rgb(&plain, x, y) == [255, 0, 0])
            .unwrap();
        let got = pixel_rgb(&multiplied, inside.0, inside.1);
        for (channel, chrome) in [52u8, 60, 32].into_iter().enumerate() {
            assert!(got[channel].abs_diff(chrome) <= 2, "got {got:?}");
        }
        assert_eq!(pixel_rgb(&multiplied, 2, 2), WHITE);
        // The bottom layer is laid over the colour, not blended with it.
        let bottom = text_clipped(
            &tree,
            &format!(
                ".t {{ {font} color: transparent; background-color: #8080ff; \
                 background-image: linear-gradient(rgba(208, 80, 64, 0.5), rgba(208, 80, 64, 0.5)); \
                 background-clip: text; background-blend-mode: multiply; }}"
            ),
        );
        let got = pixel_rgb(&bottom, inside.0, inside.1);
        for (channel, chrome) in [168u8, 104, 159].into_iter().enumerate() {
            assert!(got[channel].abs_diff(chrome) <= 2, "got {got:?}");
        }
    }

    #[test]
    fn the_colour_follows_the_clip_of_the_bottom_layer() {
        let tree: Element = view! { <div class="t" /> };
        let paint = |declarations: &str| {
            text_clipped(
                &tree,
                &format!(
                    ".t {{ width: 40px; height: 20px; border: 10px solid transparent; padding: 10px; \
                     background-color: #ff0000; {declarations} }}"
                ),
            )
        };
        let border = paint("");
        assert_eq!(pixel_rgb(&border, 5, 30), [255, 0, 0]);
        let padding = paint("background-clip: padding-box;");
        assert_eq!(pixel_rgb(&padding, 5, 30), WHITE);
        assert_eq!(pixel_rgb(&padding, 15, 30), [255, 0, 0]);
        let content = paint("background-clip: content-box;");
        assert_eq!(pixel_rgb(&content, 15, 30), WHITE);
        assert_eq!(pixel_rgb(&content, 25, 30), [255, 0, 0]);
        // The bottom layer's clip decides, not the top one's.
        let layered = paint(
            "background-image: linear-gradient(#00ff00, #00ff00), linear-gradient(#0000ff, #0000ff); \
             background-clip: border-box, padding-box;",
        );
        assert_eq!(pixel_rgb(&layered, 5, 30), [0, 255, 0]);
        assert_eq!(pixel_rgb(&layered, 15, 30), [0, 255, 0]);
    }

    #[test]
    fn mask_box_image_masks_by_the_alpha_of_its_nine_slice_image() {
        let source = "-webkit-mask-box-image-source: linear-gradient(#000, #000); \
                      -webkit-mask-box-image-width: 10px;";
        // With `fill` the middle is drawn too: the box shows whole.
        let filled = masked(
            &format!("{source} -webkit-mask-box-image-slice: 5 fill;"),
            (110, 60),
        );
        assert_g(&filled, &[(50, 25, 0), (3, 3, 0), (96, 46, 0)], 0);
        // Without it only the ring between the box's edge and 10px in shows.
        let ring = masked(
            &format!("{source} -webkit-mask-box-image-slice: 5;"),
            (110, 60),
        );
        assert_g(
            &ring,
            &[(3, 25, 0), (50, 3, 0), (50, 25, 255), (96, 25, 0)],
            0,
        );
        // The shorthand fills the middle unless told otherwise.
        let shorthand = masked(
            "-webkit-mask-box-image: linear-gradient(#000, #000) 5 / 10px;",
            (110, 60),
        );
        assert_g(&shorthand, &[(50, 25, 0), (3, 3, 0)], 0);
        // A source that is not an image hides nothing and masks nothing more.
        let none = masked("-webkit-mask-box-image: none;", (110, 60));
        assert_g(&none, &[(50, 25, 0)], 0);
    }

    /// `(x0, y0, x1, y1)` of the pixels of exactly `color`; `None` when there are none.
    fn bounds_of_color(canvas: &Canvas, color: [u8; 3]) -> Option<(u32, u32, u32, u32)> {
        let (width, height) = (canvas.width(), canvas.height());
        let mut bounds: Option<(u32, u32, u32, u32)> = None;
        for y in 0..height {
            for x in 0..width {
                if pixel_rgb(canvas, x, y) == color {
                    bounds = Some(match bounds {
                        None => (x, y, x, y),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                    });
                }
            }
        }
        bounds
    }

    #[test]
    fn an_inline_element_is_faded_filtered_and_masked_as_a_unit() {
        let tree: Element = view! { <div class="p">"aa "<span class="s">"bbbb"</span>" cc"</div> };
        let css = |rule: &str| {
            paint_text_css(
                &tree,
                &format!(".p {{ font-size: 20px; }} .s {{ background-color: #ff0000; {rule} }}"),
                (120, 40),
            )
        };
        // Chrome 153: half of red over white, grey for grayscale, and the mask by half.
        let faded = css("opacity: 0.5;");
        assert!(bounds_of_color(&faded, [255, 0, 0]).is_none());
        assert!(bounds_of_color(&faded, [255, 128, 128]).is_some());
        let grey = css("filter: grayscale(1);");
        assert!(bounds_of_color(&grey, [255, 0, 0]).is_none());
        assert!(bounds_of_color(&grey, [54, 54, 54]).is_some());
        let masked = css("mask-image: linear-gradient(#000 50%, transparent 50%);");
        let whole = css("");
        let (_, y0, _, y1) = bounds_of_color(&whole, [255, 0, 0]).expect("a box");
        let (_, my0, _, my1) = bounds_of_color(&masked, [255, 0, 0]).expect("half of it");
        assert_eq!(my0, y0);
        assert!(
            my1 < y1 && my1 >= (y0 + y1) / 2 - 2,
            "{my0}..{my1} of {y0}..{y1}"
        );
    }

    #[test]
    fn a_clip_of_an_inline_element_is_the_box_of_its_first_fragment() {
        let tree: Element = view! {
            <div class="p">"aa "<span class="s">"bbbbbb cccccc dddd"</span>" ee"</div>
        };
        let css = |rule: &str| {
            paint_text_css(
                &tree,
                &format!(
                    ".p {{ width: 120px; font-size: 20px; line-height: 30px; }} \
                     .s {{ background-color: #ff0000; padding: 0 4px; {rule} }}"
                ),
                (140, 120),
            )
        };
        // Chrome 153: the box of the first line is all there is to clip to, and what
        // is on the other lines is cut away.
        let whole = bounds_of_color(&css(""), [255, 0, 0]).expect("boxes");
        let clipped = bounds_of_color(&css("clip-path: border-box;"), [255, 0, 0]).expect("a box");
        assert!(whole.3 > clipped.3 + 20, "{whole:?} against {clipped:?}");
        assert!(clipped.3 - clipped.1 < 30);
    }

    #[test]
    fn an_inline_element_has_its_shadow_and_a_margin_box_that_takes_in_its_margins() {
        let tree: Element = view! { <div class="p">"aa "<span class="s">"bb"</span>" cc"</div> };
        let css = |rule: &str| {
            paint_text_css(
                &tree,
                &format!(
                    ".p {{ margin: 60px; font-size: 20px; line-height: 30px; }} \
                     .s {{ margin: 6px 14px; padding: 0 5px; {rule} }}"
                ),
                (200, 160),
            )
        };
        let shadow = "box-shadow: 0 0 0 40px #ff0000;";
        let all = bounds_of_color(&css(shadow), [255, 0, 0]).expect("a shadow");
        let (x0, y0, x1, y1) = bounds_of_color(
            &css(&format!("{shadow} clip-path: margin-box;")),
            [255, 0, 0],
        )
        .expect("a clipped shadow");
        // Chrome 153: the shadow is cut to the margin box, which takes in the
        // horizontal margins of 14px and the vertical ones of 6px.
        let border =
            bounds_of_color(&css("background-color: #ff0000;"), [255, 0, 0]).expect("a box");
        assert!(
            all.0 < x0 && all.2 > x1,
            "the shadow is wider than the clip"
        );
        assert!((x0 + 14).abs_diff(border.0) <= 1, "the left margin");
        assert!(x1.abs_diff(border.2 + 14) <= 1, "the right margin");
        assert!((y0 + 6).abs_diff(border.1) <= 1, "the top margin");
        assert!(
            y1 >= border.3 + 5 && y1 <= border.3 + 7,
            "the bottom margin"
        );
    }

    #[test]
    fn an_inline_svg_is_premultiplied_once_for_as_long_as_its_pixels_stay() {
        let pixels = std::rc::Rc::new([200u8, 100, 50, 128].repeat(4));
        let first = inline_svg_pixmap(&pixels, (2, 2)).expect("a pixmap");
        let again = inline_svg_pixmap(&pixels, (2, 2)).expect("a pixmap");
        assert!(std::rc::Rc::ptr_eq(&first, &again));
        // Premultiplied by the alpha of 128.
        let pixel = first.pixels()[0];
        assert_eq!((pixel.red(), pixel.alpha()), (100, 128));
        let other = std::rc::Rc::new([200u8, 100, 50, 128].repeat(4));
        let separate = inline_svg_pixmap(&other, (2, 2)).expect("a pixmap");
        assert!(!std::rc::Rc::ptr_eq(&first, &separate));
        // What the renderer let go is not kept.
        drop(pixels);
        drop(other);
        let last = std::rc::Rc::new([9u8, 9, 9, 255].repeat(4));
        inline_svg_pixmap(&last, (2, 2)).expect("a pixmap");
        assert_eq!(INLINE_SVGS.with(|kept| kept.borrow().len()), 1);
    }

    #[test]
    fn an_inline_svg_is_a_box_drawn_whole_by_the_svg_renderer() {
        let tree: Element = view! {
            <div class="r">
                <svg width="40" height="30">
                    <rect width="20" height="30" fill="#ff0000" />
                </svg>
                <svg viewBox="0 0 10 10" width="40" height="40" class="v">
                    <rect width="5" height="10" fill="#0000ff" />
                </svg>
                <div class="after" />
            </div>
        };
        let canvas = paint_text_css(
            &tree,
            ".after { width: 10px; height: 10px; background-color: #00aa00; } svg { display: block; }",
            (60, 100),
        );
        // The first svg is 40x30 at the origin: its rect fills the left half.
        assert_eq!(pixel_rgb(&canvas, 10, 15), [255, 0, 0]);
        assert_eq!(pixel_rgb(&canvas, 30, 15), WHITE);
        // The second follows it as a block (here), and its viewBox scales the content.
        assert_eq!(pixel_rgb(&canvas, 10, 50), [0, 0, 255]);
        assert_eq!(pixel_rgb(&canvas, 30, 50), WHITE);
        // What follows is after both, so the svgs took the room of their sizes.
        assert_eq!(pixel_rgb(&canvas, 5, 75), [0, 170, 0]);
        assert_eq!(pixel_rgb(&canvas, 30, 65), WHITE);
    }

    #[test]
    fn css_sizes_and_styles_an_inline_svg() {
        let tree: Element = view! {
            <div class="r">
                <svg class="s" viewBox="0 0 10 10">
                    <rect class="k" width="10" height="10" />
                    <circle cx="5" cy="5" r="2" fill="currentColor" />
                </svg>
            </div>
        };
        let canvas = paint_text_css(
            &tree,
            ".s { width: 50px; height: 50px; color: #ffffff; } .k { fill: #ff0000; }",
            (60, 60),
        );
        // The rules size the svg and give the rect its fill and the circle its colour.
        assert_eq!(pixel_rgb(&canvas, 5, 5), [255, 0, 0]);
        assert_eq!(pixel_rgb(&canvas, 25, 25), WHITE);
        assert_eq!(pixel_rgb(&canvas, 55, 55), WHITE);
        // A viewBox alone fills the width and takes its height from the ratio.
        let filled = paint_text_css(&tree, ".r { width: 60px; } .k { fill: #ff0000; }", (60, 60));
        assert_eq!(pixel_rgb(&filled, 5, 55), [255, 0, 0]);
    }

    #[test]
    fn a_style_element_in_an_inline_svg_is_read_by_the_svg_renderer() {
        let tree: Element = view! {
            <div class="r">
                <svg width="40" height="20">
                    <style>{".k { fill: #00aa00; }"}</style>
                    <rect class="k" width="20" height="20" />
                </svg>
            </div>
        };
        let canvas = paint_text_css(&tree, "", (50, 30));
        assert_eq!(pixel_rgb(&canvas, 10, 10), [0, 170, 0]);
        assert_eq!(pixel_rgb(&canvas, 30, 10), WHITE);
    }

    #[test]
    fn svg_text_takes_monospace_and_sans_serif_from_the_engines_fonts() {
        let tree: Element = view! {
            <div class="r">
                <svg width="120" height="40">
                    <text x="0" y="30" font-size="28" font-family="monospace">iiii</text>
                </svg>
                <svg width="120" height="40">
                    <text x="0" y="30" font-size="28" font-family="sans-serif">iiii</text>
                </svg>
            </div>
        };
        let canvas = paint_text_css(&tree, "", (130, 90));
        let extent = |y0: u32| {
            let columns: Vec<u32> = (0..130)
                .filter(|&x| (y0..y0 + 40).any(|y| pixel_rgb(&canvas, x, y)[0] < 128))
                .collect();
            columns.last().copied().unwrap_or(0) - columns.first().copied().unwrap_or(0)
        };
        // Monospace letters take the width of a wide one; sans-serif ones do not.
        assert!(
            extent(0) > extent(40) + 20,
            "{} against {}",
            extent(0),
            extent(40)
        );
    }
    #[test]
    fn an_inline_svg_draws_its_text_and_uses_the_definitions_of_another() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <linearGradient id="g" x1="0" x2="1" y1="0" y2="0">
                            <stop offset="0" stop-color="#ff0000" />
                            <stop offset="1" stop-color="#ff0000" />
                        </linearGradient>
                    </defs>
                </svg>
                <svg width="80" height="40">
                    <rect x="0" y="0" width="20" height="40" fill="url(#g)" />
                    <text x="24" y="30" font-size="28" fill="#000000">Hi</text>
                </svg>
            </div>
        };
        let canvas = paint_text_css(&tree, "", (90, 50));
        assert_eq!(pixel_rgb(&canvas, 10, 20), [255, 0, 0]);
        // Dark pixels where the letters are, none to the right of them.
        let dark = (24..60)
            .flat_map(|x| (5..35).map(move |y| (x, y)))
            .filter(|&(x, y)| pixel_rgb(&canvas, x, y)[0] < 100)
            .count();
        assert!(dark > 40, "the text is drawn: {dark} dark pixels");
        assert_eq!(pixel_rgb(&canvas, 75, 20), WHITE);
    }
    #[test]
    fn a_mask_url_to_an_inline_svg_mask_masks_the_box() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <mask id="m" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="50">
                            <rect x="0" y="0" width="50" height="50" fill="#ffffff" />
                            <rect x="50" y="0" width="25" height="50" fill="#808080" />
                        </mask>
                        <mask id="a" mask-type="alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="50">
                            <rect x="0" y="0" width="100" height="50" fill="#000000" fill-opacity="0.5" />
                        </mask>
                    </defs>
                </svg>
                <div class="b" />
            </div>
        };
        let canvas = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background-color: #ff0000; mask: url(#m); }",
            (110, 60),
        );
        // Chrome 153: the white half shows, the grey a half, the rest is gone.
        assert_eq!(pixel_rgb(&canvas, 20, 25), [255, 0, 0]);
        assert_eq!(pixel_rgb(&canvas, 90, 25), WHITE);
        let half = pixel_rgb(&canvas, 60, 25);
        assert!(
            (126..=129).contains(&half[1]) && half[0] == 255,
            "half: {half:?}"
        );
        // The same through `mask-image` and the prefixed form.
        for declaration in ["mask-image: url(#m)", "-webkit-mask-image: url(#m)"] {
            let canvas = paint_text_css(
                &tree,
                &format!(
                    ".b {{ width: 100px; height: 50px; background-color: #ff0000; {declaration}; }}"
                ),
                (110, 60),
            );
            assert_eq!(pixel_rgb(&canvas, 20, 25), [255, 0, 0], "{declaration}");
            assert_eq!(pixel_rgb(&canvas, 90, 25), WHITE, "{declaration}");
        }
        // `mask-mode` reads the mask as it says, whatever the mask's own type is
        // (Chrome 153), and the properties that place an image do not apply.
        let by = |declaration: &str| {
            pixel_rgb(
                &paint_text_css(
                    &tree,
                    &format!(
                        ".b {{ width: 100px; height: 50px; background-color: #ff0000; {declaration}; }}"
                    ),
                    (110, 60),
                ),
                60,
                25,
            )
        };
        assert_eq!(by("mask: url(#m); mask-mode: alpha"), [255, 0, 0]);
        assert_eq!(by("mask: url(#a); mask-mode: luminance"), WHITE);
        let half = by("mask: url(#a)");
        assert!((126..=129).contains(&half[1]), "alpha mask: {half:?}");
        assert_eq!(
            by(
                "mask: url(#m); mask-size: 10px 10px; mask-position: 40px 0; mask-clip: content-box"
            ),
            by("mask: url(#m)"),
            "a mask element is not placed like an image"
        );
        // A reference to nothing hides the box.
        let missing = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background-color: #ff0000; mask: url(#nope); }",
            (110, 60),
        );
        assert_eq!(pixel_rgb(&missing, 20, 25), WHITE);
    }

    #[test]
    fn css_styles_the_inline_svg_a_reference_names() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <mask id="s" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="50">
                            <rect class="k" x="0" y="0" width="50" height="50" />
                        </mask>
                        <clipPath id="e"><rect class="e" x="0" y="0" width="100" height="50" /></clipPath>
                    </defs>
                </svg>
                <div class="b" />
            </div>
        };
        let paint = |css: &str, declaration: &str| {
            paint_text_css(
                &tree,
                &format!(
                    "{css} .b {{ width: 100px; height: 50px; background-color: #ff0000; {declaration}; }}"
                ),
                (110, 60),
            )
        };
        // Without a fill the rect is black and the luminance mask hides everything.
        assert_eq!(pixel_rgb(&paint("", "mask: url(#s)"), 20, 25), WHITE);
        // A rule gives it the fill, and the left half shows.
        let shown = paint(".k { fill: #ffffff; }", "mask: url(#s)");
        assert_eq!(pixel_rgb(&shown, 20, 25), [255, 0, 0]);
        assert_eq!(pixel_rgb(&shown, 80, 25), WHITE);
        // `mask-type: alpha` from a rule reads the black rect by its alpha.
        let alpha = paint("mask { mask-type: alpha; }", "mask: url(#s)");
        assert_eq!(pixel_rgb(&alpha, 20, 25), [255, 0, 0]);
        // A clip path's `clip-rule`, and `display: none` on a shape of it.
        let hidden = paint(".e { display: none; }", "clip-path: url(#e)");
        assert_eq!(pixel_rgb(&hidden, 20, 25), WHITE);
    }

    #[test]
    fn a_backdrop_filter_url_filters_what_is_behind_the_box() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <filter id="g">
                            <feColorMatrix type="matrix"
                                values="0 0 0 0 0  0 0 0 0 1  0 0 0 0 0  0 0 0 1 0" />
                        </filter>
                    </defs>
                </svg>
                <div class="back" />
                <div class="front" />
            </div>
        };
        let canvas = paint_text_css(
            &tree,
            ".r { position: relative; width: 120px; height: 60px; } \
             .back { position: absolute; left: 0; top: 0; width: 60px; height: 60px; \
             background-color: #ff0000; } \
             .front { position: absolute; left: 30px; top: 10px; width: 60px; height: 40px; \
             backdrop-filter: url(#g); }",
            (130, 70),
        );
        // Chrome 153: the matrix makes green of what is behind the box, on red
        // and on the page alike, and only inside the box.
        assert_eq!(pixel_rgb(&canvas, 45, 30), [0, 255, 0]);
        assert_eq!(pixel_rgb(&canvas, 80, 30), [0, 255, 0]);
        assert_eq!(pixel_rgb(&canvas, 10, 30), [255, 0, 0]);
        assert_eq!(pixel_rgb(&canvas, 110, 30), WHITE);
    }
    #[test]
    fn a_clip_path_url_to_an_inline_svg_clip_path_clips_the_box() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <clipPath id="c"><circle cx="50" cy="25" r="20" /></clipPath>
                        <clipPath id="u" clipPathUnits="objectBoundingBox">
                            <rect x="0" y="0" width="0.5" height="1" />
                        </clipPath>
                    </defs>
                </svg>
                <div class="b" />
            </div>
        };
        let paint = |declaration: &str| {
            paint_text_css(
                &tree,
                &format!(
                    ".b {{ width: 100px; height: 50px; background-color: #ff0000; {declaration}; }}"
                ),
                (110, 60),
            )
        };
        let circle = paint("clip-path: url(#c)");
        assert_eq!(pixel_rgb(&circle, 50, 25), [255, 0, 0]);
        assert_eq!(pixel_rgb(&circle, 5, 5), WHITE);
        assert_eq!(pixel_rgb(&circle, 95, 45), WHITE);
        let half = paint("clip-path: url(#u)");
        assert_eq!(pixel_rgb(&half, 20, 25), [255, 0, 0]);
        assert_eq!(pixel_rgb(&half, 70, 25), WHITE);
        // One that names nothing clips nothing.
        let missing = paint("clip-path: url(#nope)");
        assert_eq!(pixel_rgb(&missing, 95, 45), [255, 0, 0]);
    }

    #[test]
    fn an_object_bounding_box_clip_url_takes_the_css_box_on_a_scaled_canvas() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <clipPath id="u" clipPathUnits="objectBoundingBox">
                            <rect x="0" y="0" width="0.5" height="1" />
                        </clipPath>
                    </defs>
                </svg>
                <div class="b" />
            </div>
        };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(
            ".b { width: 100px; height: 50px; background-color: #ff0000; clip-path: url(#u); }",
        )
        .unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut font = Font::load_embedded();
        let mut layouts =
            florui_layout::compute_layout(&mut font, &arena, &styles, Size::MAX_CONTENT).unwrap();
        for layout in layouts.values_mut() {
            layout.x *= 2.0;
            layout.y *= 2.0;
            layout.width *= 2.0;
            layout.height *= 2.0;
        }
        let canvas = paint_to_buffer(
            &mut font,
            220,
            120,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            2.0,
        );
        // Half of the box of 200 by 100 device pixels, not all of it.
        assert_eq!(pixel_rgb(&canvas, 80, 50), [255, 0, 0]);
        assert_eq!(pixel_rgb(&canvas, 140, 50), WHITE);
    }

    #[test]
    fn a_filter_url_to_an_inline_svg_filter_filters_the_box() {
        let tree: Element = view! {
            <div class="r">
                <svg width="0" height="0">
                    <defs>
                        <filter id="g">
                            <feColorMatrix type="matrix"
                                values="0 0 0 0 0  0 0 0 0 1  0 0 0 0 0  0 0 0 1 0" />
                        </filter>
                        <filter id="o" x="0" y="0" width="2" height="1">
                            <feOffset dx="30" dy="0" />
                        </filter>
                    </defs>
                </svg>
                <div class="b" />
            </div>
        };
        let paint = |declaration: &str, size: (u32, u32)| {
            paint_text_css(
                &tree,
                &format!(
                    ".b {{ width: 40px; height: 40px; margin-left: 10px; background-color: #ff0000; {declaration}; }}"
                ),
                size,
            )
        };
        let green = paint("filter: url(#g)", (110, 60));
        assert_eq!(pixel_rgb(&green, 30, 20), [0, 255, 0]);
        assert_eq!(pixel_rgb(&green, 5, 20), WHITE);
        // The region of the filter is the box's, here wide enough to hold the move.
        let moved = paint("filter: url(#o)", (110, 60));
        assert_eq!(pixel_rgb(&moved, 15, 20), WHITE);
        assert_eq!(pixel_rgb(&moved, 70, 20), [255, 0, 0]);
        // In a list, after a native function.
        let listed = paint("filter: invert(1) url(#g)", (110, 60));
        assert_eq!(pixel_rgb(&listed, 30, 20), [0, 255, 0]);
    }
    #[test]
    fn layers_combine_by_their_composite_operator() {
        let box_layer = "linear-gradient(#000, #000)";
        let circle = "radial-gradient(circle at 50% 50%, #000 20px, transparent 20px)";
        let by = |operator: &str| {
            masked(
                &format!("mask-image: {box_layer}, {circle}; mask-composite: {operator}, add;"),
                (110, 60),
            )
        };
        // Chrome 153: subtract leaves the box with the disc cut out.
        let subtract = by("subtract");
        assert_g(
            &subtract,
            &[(5, 25, 0), (50, 25, 255), (70, 25, 0), (95, 25, 0)],
            0,
        );
        let band = "linear-gradient(to right, #000 60%, transparent 60%)";
        let disc = "radial-gradient(circle at 50% 50%, #000 22px, transparent 22px)";
        let with = |operator: &str| {
            masked(
                &format!("mask-image: {band}, {disc}; mask-composite: {operator}, add;"),
                (110, 60),
            )
        };
        let intersect = with("intersect");
        assert_g(
            &intersect,
            &[
                (5, 25, 255),
                (40, 25, 0),
                (50, 25, 0),
                (59, 25, 0),
                (60, 25, 255),
                (70, 25, 255),
            ],
            0,
        );
        let exclude = with("exclude");
        assert_g(
            &exclude,
            &[
                (5, 25, 0),
                (40, 25, 255),
                (50, 25, 255),
                (59, 25, 255),
                (60, 25, 0),
                (70, 25, 0),
            ],
            0,
        );
        let add = with("add");
        assert_g(
            &add,
            &[
                (5, 25, 0),
                (40, 25, 0),
                (60, 25, 0),
                (70, 25, 0),
                (95, 25, 255),
            ],
            0,
        );
        // A layer that subtracts from one that is somewhere else removes nothing,
        // and the layer below it is removed from the picture entirely.
        let apart = masked(
            "mask-image: linear-gradient(#000, #000), linear-gradient(#000, #000); \
             mask-size: 60px 30px, 60px 30px; mask-position: 0 0, 100% 100%; \
             mask-repeat: no-repeat; mask-composite: subtract, add;",
            (110, 60),
        );
        assert_g(
            &apart,
            &[
                (5, 15, 0),
                (59, 15, 0),
                (60, 15, 255),
                (70, 40, 255),
                (95, 40, 255),
            ],
            0,
        );
    }

    #[test]
    fn a_mask_whose_image_cannot_be_read_hides_the_box_and_a_none_layer_masks_nothing() {
        let missing = masked("mask-image: url(does-not-exist.png);", (110, 60));
        assert_eq!(pixel_rgb(&missing, 50, 25), WHITE);
        // An unreadable layer under a readable one masks nothing by itself.
        let with_gradient = masked(
            "mask-image: url(does-not-exist.png), linear-gradient(#000, #000);",
            (110, 60),
        );
        assert_eq!(pixel_rgb(&with_gradient, 50, 25), RED);
        let none_then_fade = masked(
            "mask-image: none, linear-gradient(#000, transparent);",
            (110, 60),
        );
        assert_g(
            &none_then_fade,
            &[(50, 0, 3), (50, 25, 130), (50, 49, 252)],
            4,
        );
        let none = masked("mask-image: none;", (110, 60));
        assert_eq!(pixel_rgb(&none, 50, 25), RED);
    }

    #[test]
    fn a_mask_cuts_the_children_with_the_box() {
        let tree: Element = view! { <div class="b"><div class="kid" /></div> };
        let canvas = paint_text_css(
            &tree,
            ".b { width: 100px; height: 50px; background-color: #ff0000; \
                 mask-image: linear-gradient(#000, #000); mask-size: 50px 50px; \
                 mask-repeat: no-repeat; mask-position: center; } \
             .kid { margin: 10px; width: 30px; height: 20px; background-color: #0000ff; }",
            (110, 60),
        );
        // The child is blue inside the tile (x 25..75) and gone outside it.
        assert_eq!(pixel_rgb(&canvas, 30, 20), [0, 0, 255]);
        assert_eq!(pixel_rgb(&canvas, 20, 20), WHITE);
        assert_eq!(pixel_rgb(&canvas, 5, 5), WHITE);
        assert_eq!(pixel_rgb(&canvas, 50, 45), RED);
    }

    #[test]
    fn an_svg_mask_is_read_by_alpha_or_by_luminance() {
        let half = "url(\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='40' height='20'>\
                    <rect width='20' height='20' fill='%23000'/>\
                    <rect x='20' width='20' height='20' fill='%23fff' fill-opacity='0.5'/></svg>\")";
        let by = |mode: &str| {
            masked(
                &format!("mask-image: {half}; mask-size: 100% 100%; {mode}"),
                (110, 60),
            )
        };
        // Chrome 153: by alpha the black half is opaque and the white one half so.
        let alpha = by("mask-mode: alpha;");
        assert_g(
            &alpha,
            &[(5, 25, 0), (45, 25, 0), (55, 25, 127), (95, 25, 127)],
            3,
        );
        // By luminance the black half is nothing and the white one half.
        let luminance = by("mask-mode: luminance;");
        assert_g(
            &luminance,
            &[(5, 25, 255), (45, 25, 255), (55, 25, 127), (95, 25, 127)],
            3,
        );
    }

    #[test]
    fn the_mask_shorthand_paints_what_its_longhands_do() {
        let short = masked(
            "mask: linear-gradient(#000, transparent) 0 0 / 50% 100% no-repeat, \
                   linear-gradient(#000, #000) 100% 0 / 20px 100% no-repeat;",
            (110, 60),
        );
        let long = masked(
            "mask-image: linear-gradient(#000, transparent), linear-gradient(#000, #000); \
             mask-position: 0 0, 100% 0; mask-size: 50% 100%, 20px 100%; mask-repeat: no-repeat;",
            (110, 60),
        );
        same_pixels(&short, &long, (110, 60));
        // Chrome 153: a fade on the left half, a strip at the right.
        assert_g(
            &short,
            &[
                (2, 25, 130),
                (25, 25, 130),
                (48, 25, 130),
                (52, 25, 255),
                (79, 25, 255),
                (81, 25, 0),
                (99, 25, 0),
            ],
            4,
        );
        assert_g(
            &short,
            &[
                (10, 0, 3),
                (10, 10, 54),
                (10, 25, 130),
                (10, 40, 207),
                (10, 49, 252),
            ],
            4,
        );
    }

    #[test]
    fn a_masked_group_paints_the_same_on_a_bounded_and_a_full_surface() {
        let tree: Element = view! { <div class="b"><div class="kid" /></div> };
        for mask in [
            "mask-image: radial-gradient(circle at 40% 50%, #000 20px, transparent 40px);",
            "mask: linear-gradient(#000, transparent) 0 0 / 50% 100% no-repeat, \
                   linear-gradient(#000, #000) 100% 0 / 20px 100% no-repeat;",
            "mask-image: linear-gradient(#000, #000), linear-gradient(#000, #000); \
             mask-size: 60px 40px, 40px 60px; mask-composite: exclude, add; mask-repeat: no-repeat;",
        ] {
            let css = format!(
                ".b {{ margin: 90px; width: 100px; height: 60px; background: #3366cc; {mask} }} \
                 .kid {{ width: 40px; height: 40px; background: #ff0000; }}"
            );
            assert_bounded_and_full_target_paint_within(1, || {
                paint_text_css(&tree, &css, (300, 300))
            });
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
    fn grayscale_turns_a_box_gray_on_the_canvas() {
        let gray = single_box_buffer(
            ".box { background-color: #ff0000; filter: grayscale(1); }",
            20,
            1.0,
        );
        // Red's luminance, 0.2126 * 255, in every channel.
        assert_eq!(pixel_rgb(&gray, 5, 5), [54, 54, 54]);
        let plain = single_box_buffer(".box { background-color: #ff0000; }", 20, 1.0);
        assert_eq!(pixel_rgb(&plain, 5, 5), [255, 0, 0]);
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

    #[test]
    fn the_tabled_shadow_blend_equals_the_plain_one_for_every_coverage() {
        let colors = [
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 128,
            },
            Rgba {
                r: 255,
                g: 128,
                b: 0,
                a: 255,
            },
            Rgba {
                r: 10,
                g: 20,
                b: 30,
                a: 0,
            },
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            Rgba {
                r: 1,
                g: 2,
                b: 3,
                a: 77,
            },
        ];
        // Valid premultiplied destinations: channels never exceed alpha.
        let mut destinations = Vec::new();
        for alpha in [0u8, 1, 37, 128, 254, 255] {
            for channel in [0u8, 1, alpha / 2, alpha] {
                destinations.push(
                    PremultipliedColorU8::from_rgba(
                        channel.min(alpha),
                        alpha / 3,
                        channel.min(alpha),
                        alpha,
                    )
                    .unwrap(),
                );
            }
        }
        for color in colors {
            let blend = ShadowBlend::new(color);
            for coverage in 0..=255u8 {
                for &dst in &destinations {
                    assert_eq!(
                        blend.over(dst, coverage),
                        blend_source_over(dst, color, coverage),
                        "{color:?} at coverage {coverage} over {dst:?}"
                    );
                }
            }
        }
    }

    /// A white page with one 20x20 rounded, blur-shadowed card at each of
    /// `positions`, laid out by hand.
    fn shadowed_cards(positions: &[(f32, f32)]) -> Canvas {
        let cards = positions
            .iter()
            .map(|_| view! { <div class="card"></div> })
            .collect::<Vec<_>>();
        let tree = Element::node("div", vec![("class".into(), "page".into())], cards);
        let css = ".page { background-color: #ffffff; } \
                   .card { background-color: #3366cc; border-radius: 6px; \
                   box-shadow: 0 4px 12px rgba(0, 0, 0, 0.5); }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let page = arena.roots()[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            page,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 120.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        for (&card, &(x, y)) in arena.children(page).iter().zip(positions) {
            layouts.insert(
                card,
                BoxLayout {
                    x,
                    y,
                    width: 20.0,
                    height: 20.0,
                    margin: florui_layout::BoxMargin::ZERO,
                },
            );
        }
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            200,
            120,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            1.0,
        )
    }

    #[test]
    fn equal_shadows_at_whole_pixel_offsets_share_one_blurred_mask() {
        shadow_cache::clear_on_this_thread();
        let before = shadow_cache::computed_on_this_thread();
        let positions: Vec<(f32, f32)> = (0..6)
            .flat_map(|row| {
                (0..6).map(move |col| (20.0 + col as f32 * 30.0, 15.0 + row as f32 * 10.0))
            })
            .collect();

        shadowed_cards(&positions);

        assert_eq!(
            shadow_cache::computed_on_this_thread() - before,
            1,
            "36 identical cards blur one mask"
        );
    }

    #[test]
    fn a_shadow_painted_from_the_cache_is_identical_to_a_cold_one() {
        let positions = [(20.0, 20.0), (60.5, 20.25), (100.0, 50.0), (20.0, 70.0)];
        shadow_cache::clear_on_this_thread();
        let cold = shadowed_cards(&positions);
        let warm = shadowed_cards(&positions);
        shadow_cache::clear_on_this_thread();
        let cold_again = shadowed_cards(&positions);

        assert_eq!(
            cold.data(),
            warm.data(),
            "a hit paints what computing paints"
        );
        assert_eq!(cold.data(), cold_again.data());
    }

    #[test]
    fn a_fractional_offset_gets_its_own_mask() {
        shadow_cache::clear_on_this_thread();
        let before = shadow_cache::computed_on_this_thread();

        shadowed_cards(&[(20.0, 20.0), (60.0, 20.0), (100.5, 20.0), (140.5, 20.0)]);

        assert_eq!(
            shadow_cache::computed_on_this_thread() - before,
            2,
            "whole-pixel and half-pixel placements are different masks, each shared"
        );
    }

    #[test]
    fn a_repeated_paint_computes_nothing_new() {
        shadow_cache::clear_on_this_thread();
        shadowed_cards(&[(20.0, 20.0), (70.0, 40.0)]);
        let after_first = shadow_cache::computed_on_this_thread();

        shadowed_cards(&[(20.0, 20.0), (70.0, 40.0)]);

        assert_eq!(shadow_cache::computed_on_this_thread(), after_first);
    }

    /// One `width x height` box at the origin of a `canvas x canvas` page,
    /// painted with `css` for `.box`.
    fn background_box(css: &str, (canvas, width, height): (u32, f32, f32), scale: f32) -> Canvas {
        let tree: Element = view! { <div class="box"></div> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut layouts = HashMap::new();
        layouts.insert(
            arena.roots()[0],
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: width * scale,
                height: height * scale,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            canvas,
            canvas,
            Rgba::TRANSPARENT,
            &arena,
            &styles,
            &layouts,
            scale,
        )
    }

    fn rgba(buffer: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let pixel = buffer.pixel(x, y).expect("pixel is within the canvas");
        [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
    }

    #[test]
    fn a_linear_gradient_fills_the_box_from_its_first_color_to_its_last() {
        let page = background_box(
            ".box { background-image: linear-gradient(to bottom, #000000, #ffffff); }",
            (60, 40.0, 40.0),
            1.0,
        );
        assert!(rgba(&page, 20, 0)[0] <= 8, "the top is the first color");
        assert!(rgba(&page, 20, 39)[0] >= 247, "the bottom is the last");
        let middle = rgba(&page, 20, 20)[0];
        assert!(
            (110..=146).contains(&middle),
            "the middle is in between: {middle}"
        );
        assert_eq!(
            rgba(&page, 5, 20),
            rgba(&page, 35, 20),
            "no change across the line"
        );
        assert_eq!(rgba(&page, 50, 20), [0, 0, 0, 0], "nothing outside the box");
    }

    #[test]
    fn the_background_color_shows_through_a_translucent_gradient() {
        let page = background_box(
            ".box { background-color: #0000ff; background-image: linear-gradient(to right, transparent, #ff0000); }",
            (60, 40.0, 40.0),
            1.0,
        );
        let [r, _, b, a] = rgba(&page, 0, 20);
        assert!(
            r <= 8 && b >= 247 && a == 255,
            "the left is the blue underneath: {r} {b}"
        );
        let [r, _, b, _] = rgba(&page, 39, 20);
        assert!(r >= 247 && b <= 8, "the right is the red on top: {r} {b}");
    }

    #[test]
    fn the_first_layer_is_on_top() {
        let page = background_box(
            ".box { background-image: linear-gradient(#ff0000, #ff0000), linear-gradient(#0000ff, #0000ff); }",
            (30, 20.0, 20.0),
            1.0,
        );
        assert_eq!(rgba(&page, 10, 10), [255, 0, 0, 255]);
    }

    #[test]
    fn a_gradient_is_cut_to_the_rounded_corners() {
        let page = background_box(
            ".box { border-radius: 12px; background-image: linear-gradient(#ff0000, #ff0000); }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(rgba(&page, 0, 0)[3], 0, "the corner is cut");
        assert_eq!(rgba(&page, 20, 20), [255, 0, 0, 255]);
        assert_eq!(
            rgba(&page, 20, 0),
            [255, 0, 0, 255],
            "the straight edge stays"
        );
    }

    #[test]
    fn background_size_and_repeat_lay_tiles_out() {
        // 10px tiles of a left-to-right black-to-white ramp, repeated.
        let page = background_box(
            ".box { background-image: linear-gradient(to right, #000000, #ffffff); background-size: 10px 10px; }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert!(rgba(&page, 0, 0)[0] <= 20 && rgba(&page, 9, 0)[0] >= 235);
        assert!(rgba(&page, 10, 0)[0] <= 20, "the ramp starts again");
        assert_eq!(
            rgba(&page, 3, 5),
            rgba(&page, 33, 35),
            "every tile is the same"
        );

        let once = background_box(
            ".box { background-color: #00ff00; background-image: linear-gradient(#ff0000, #ff0000); background-size: 10px 10px; background-repeat: no-repeat; }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(rgba(&once, 5, 5), [255, 0, 0, 255]);
        assert_eq!(rgba(&once, 15, 5), [0, 255, 0, 255], "no second tile");

        let placed = background_box(
            ".box { background-image: linear-gradient(#ff0000, #ff0000); background-size: 10px 10px; background-repeat: no-repeat; background-position: right bottom; }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(rgba(&placed, 35, 35), [255, 0, 0, 255]);
        assert_eq!(rgba(&placed, 5, 5)[3], 0);
    }

    #[test]
    fn the_origin_and_clip_boxes_pick_where_the_gradient_sits_and_what_it_covers() {
        // A 4px border and 6px padding: the content box starts at 10.
        let base = "border: 4px solid #000000; padding: 6px; background-image: linear-gradient(#ff0000, #ff0000); background-repeat: no-repeat;";
        let border_box = background_box(
            &format!(".box {{ {base} background-clip: border-box; }}"),
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(
            rgba(&border_box, 1, 1)[0],
            0,
            "the border is painted over the layer"
        );
        let content_box = background_box(
            &format!(".box {{ {base} background-clip: content-box; }}"),
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(
            rgba(&content_box, 8, 20),
            [0, 0, 0, 0],
            "the padding is not covered"
        );
        assert_eq!(rgba(&content_box, 12, 20), [255, 0, 0, 255]);
    }

    #[test]
    fn lengths_scale_with_the_display_but_percentages_do_not() {
        let page = background_box(
            ".box { background-image: linear-gradient(#ff0000, #ff0000); background-size: 10px 50%; background-repeat: no-repeat; }",
            (80, 20.0, 20.0),
            2.0,
        );
        // A 40x40 painted box; the tile is 20 wide and 20 tall.
        assert_eq!(rgba(&page, 19, 19), [255, 0, 0, 255]);
        assert_eq!(rgba(&page, 21, 5)[3], 0);
        assert_eq!(rgba(&page, 5, 21)[3], 0);
    }

    #[test]
    fn a_gradient_painted_again_unchanged_is_not_rendered_again() {
        // Colors and sizes no other test uses, so the cache starts cold for them.
        let css = ".box { background-image: linear-gradient(#123457, #fedcb8); background-size: 33px 21px; }";
        let rendered = || layer_cache::rendered_on_this_thread();
        let before = rendered();
        // Painted once, it goes straight onto the frame and is kept nowhere.
        let first = background_box(css, (50, 47.0, 47.0), 1.0);
        assert_eq!(rendered() - before, 0, "a layer seen once is not kept");
        // Painted again, it is a layer worth keeping.
        let second = background_box(css, (50, 47.0, 47.0), 1.0);
        assert_eq!(rendered() - before, 1);
        let third = background_box(css, (50, 47.0, 47.0), 1.0);
        assert_eq!(rendered() - before, 1, "the third frame reuses the layer");
        assert_eq!(
            first.data(),
            second.data(),
            "drawn straight or kept, the same pixels"
        );
        assert_eq!(second.data(), third.data());
        // Anything that changes the pixels is a layer of its own: the color, the scale.
        let other = css.replace("#123457", "#123458");
        background_box(&other, (50, 47.0, 47.0), 1.0);
        background_box(&other, (50, 47.0, 47.0), 1.0);
        assert_eq!(rendered() - before, 2);
        background_box(css, (100, 47.0, 47.0), 2.0);
        background_box(css, (100, 47.0, 47.0), 2.0);
        assert_eq!(rendered() - before, 3);
    }

    #[test]
    fn a_rounded_layer_is_the_same_drawn_straight_or_kept() {
        // Colors and sizes no other test uses, so the cache starts cold for them.
        let css = ".box { border-radius: 9px; background-image: radial-gradient(circle at 30% 20%, #345612, #ab34cd80); background-size: 100% 100%; }";
        let straight = background_box(css, (60, 47.3, 41.6), 1.0);
        let kept = background_box(css, (60, 47.3, 41.6), 1.0);
        let reused = background_box(css, (60, 47.3, 41.6), 1.0);
        assert_eq!(
            kept.data(),
            reused.data(),
            "a kept layer is put on the same way each time"
        );
        let worst = straight
            .data()
            .iter()
            .zip(kept.data())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .expect("pixels");
        assert_eq!(
            worst, 0,
            "drawn straight differs from kept by {worst} levels"
        );
        assert_eq!(rgba(&kept, 0, 0)[3], 0, "the corner is cut");
        assert!(rgba(&kept, 25, 20)[3] > 0, "and the inside is not");
        assert_eq!(
            rgba(&straight, 0, 0)[3],
            0,
            "the corner is cut when drawn straight too"
        );
    }

    /// A `data:` URL of a `width x height` PNG, `left` on its left half and
    /// `right` on its right half.
    fn two_color_png_url(width: u32, height: u32, left: [u8; 4], right: [u8; 4]) -> String {
        use base64::Engine as _;
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("a header");
            let data: Vec<u8> = (0..height)
                .flat_map(|_| (0..width).flat_map(|x| if x < width / 2 { left } else { right }))
                .collect();
            writer.write_image_data(&data).expect("pixels");
        }
        format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn a_png_background_is_drawn_at_its_own_size_and_tiles() {
        let url = two_color_png_url(10, 10, [255, 0, 0, 255], [0, 0, 255, 255]);
        let page = background_box(
            &format!(".box {{ background-image: url({url}); }}"),
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(
            rgba(&page, 2, 2),
            [255, 0, 0, 255],
            "the left half of the first tile"
        );
        assert_eq!(rgba(&page, 7, 2), [0, 0, 255, 255], "the right half");
        assert_eq!(
            rgba(&page, 12, 2),
            [255, 0, 0, 255],
            "the next tile starts again"
        );
        assert_eq!(
            rgba(&page, 37, 37),
            [0, 0, 255, 255],
            "down to the far corner"
        );
        let once = background_box(
            &format!(".box {{ background-image: url({url}); background-repeat: no-repeat; }}"),
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(rgba(&once, 2, 2), [255, 0, 0, 255]);
        assert_eq!(rgba(&once, 12, 2)[3], 0, "no second tile");
    }

    #[test]
    fn a_png_background_follows_background_size_and_the_display_scale() {
        let url = two_color_png_url(10, 10, [255, 0, 0, 255], [0, 0, 255, 255]);
        let covered = background_box(
            &format!(".box {{ background-image: url({url}); background-size: cover; }}"),
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(
            rgba(&covered, 10, 20),
            [255, 0, 0, 255],
            "stretched to the box"
        );
        assert_eq!(rgba(&covered, 30, 20), [0, 0, 255, 255]);
        let scaled = background_box(
            &format!(".box {{ background-image: url({url}); background-repeat: no-repeat; }}"),
            (80, 40.0, 40.0),
            2.0,
        );
        assert_eq!(
            rgba(&scaled, 8, 5),
            [255, 0, 0, 255],
            "ten css pixels are twenty painted ones"
        );
        assert_eq!(rgba(&scaled, 15, 5), [0, 0, 255, 255]);
        assert_eq!(rgba(&scaled, 22, 5)[3], 0, "and no more");
    }

    #[test]
    fn an_svg_background_is_drawn_sharp_at_the_size_it_is_shown() {
        let svg = "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'><rect width='5' height='10' fill='#ff0000'/><rect x='5' width='5' height='10' fill='#0000ff'/></svg>";
        let page = background_box(
            &format!(
                ".box {{ background-image: url(\"{svg}\"); background-size: 40px 40px; background-repeat: no-repeat; }}"
            ),
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(rgba(&page, 10, 20), [255, 0, 0, 255]);
        assert_eq!(
            rgba(&page, 19, 20),
            [255, 0, 0, 255],
            "the edge is where the SVG puts it, at 40px"
        );
        assert_eq!(rgba(&page, 21, 20), [0, 0, 255, 255]);
    }

    #[test]
    fn a_file_image_is_painted_again_when_the_file_changes() {
        let dir =
            std::env::temp_dir().join(format!("florui-background-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory");
        let path = dir.join("tile.png");
        let write = |color: [u8; 4], side: u32| {
            let url = two_color_png_url(side, side, color, color);
            let encoded = url.split(',').nth(1).expect("base64");
            use base64::Engine as _;
            std::fs::write(
                &path,
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .expect("bytes"),
            )
            .expect("written");
        };
        let css = format!(
            ".box {{ background-image: url({}); background-repeat: no-repeat; }}",
            path.display().to_string().replace('\\', "/")
        );
        write([255, 0, 0, 255], 4);
        let first = background_box(&css, (20, 10.0, 10.0), 1.0);
        assert_eq!(rgba(&first, 1, 1), [255, 0, 0, 255]);
        // A new color, the same size and the same stylesheet: the new file shows.
        write([0, 255, 0, 255], 4);
        let second = background_box(&css, (20, 10.0, 10.0), 1.0);
        assert_eq!(rgba(&second, 1, 1), [0, 255, 0, 255]);
        std::fs::remove_file(&path).expect("removed");
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn an_image_sits_on_whole_pixels_even_when_the_free_space_is_odd() {
        // 25px wide, a 10px image centered: 7.5px from the left, drawn at 8.
        let url = two_color_png_url(10, 10, [255, 0, 0, 255], [255, 0, 0, 255]);
        let page = background_box(
            &format!(
                ".box {{ background-image: url({url}); background-repeat: no-repeat; background-position: 50% 0; }}"
            ),
            (25, 25.0, 25.0),
            1.0,
        );
        assert_eq!(rgba(&page, 7, 2)[3], 0, "not blended into pixel 7");
        assert_eq!(rgba(&page, 8, 2), [255, 0, 0, 255]);
        assert_eq!(rgba(&page, 17, 2), [255, 0, 0, 255]);
        assert_eq!(rgba(&page, 18, 2)[3], 0);
    }

    #[test]
    fn an_image_that_cannot_be_read_paints_nothing_and_the_rest_still_paints() {
        let page = background_box(
            ".box { background-color: #00ff00; background-image: url(no/such/file.png), linear-gradient(#ff0000, #ff0000); background-size: auto, 10px 10px; background-repeat: repeat, no-repeat; }",
            (30, 20.0, 20.0),
            1.0,
        );
        assert_eq!(rgba(&page, 5, 5), [255, 0, 0, 255], "the gradient layer");
        assert_eq!(
            rgba(&page, 15, 15),
            [0, 255, 0, 255],
            "the color under the missing image"
        );
    }

    #[test]
    fn an_image_layer_is_under_the_layers_before_it() {
        let url = two_color_png_url(10, 10, [255, 0, 0, 255], [255, 0, 0, 255]);
        let page = background_box(
            &format!(
                ".box {{ background-image: linear-gradient(#00ff00, #00ff00), url({url}); background-size: 5px 5px, auto; background-repeat: no-repeat; }}"
            ),
            (30, 20.0, 20.0),
            1.0,
        );
        assert_eq!(
            rgba(&page, 2, 2),
            [0, 255, 0, 255],
            "the gradient first in the list is on top"
        );
        assert_eq!(
            rgba(&page, 7, 7),
            [255, 0, 0, 255],
            "the image shows where it does not reach"
        );
    }

    #[test]
    fn a_translucent_image_blends_over_the_color_under_it() {
        let url = two_color_png_url(4, 4, [0, 0, 255, 128], [0, 0, 255, 128]);
        let page = background_box(
            &format!(
                ".box {{ background-color: #ff0000; background-image: url({url}); background-repeat: no-repeat; }}"
            ),
            (20, 10.0, 10.0),
            1.0,
        );
        let [r, _, b, a] = rgba(&page, 1, 1);
        assert_eq!(a, 255);
        assert!(
            (120..=135).contains(&r) && (120..=135).contains(&b),
            "half red, half blue: {r} {b}"
        );
    }

    /// A `.wrap` that clips (`overflow: hidden`), 40 x 20, holding a 40 x 60
    /// `.inner` painted with `inner_css` and laid out by hand.
    fn clipped_inner(inner_css: &str, inner_top: f32) -> Canvas {
        let tree: Element = view! { <div class="wrap"><div class="inner"></div></div> };
        let css = format!(".wrap {{ overflow: hidden; }} .inner {{ {inner_css} }}");
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(&css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let wrap = arena.roots()[0];
        let inner = arena.children(wrap)[0];
        let mut layouts = HashMap::new();
        layouts.insert(
            wrap,
            BoxLayout {
                x: 0.0,
                y: 20.0,
                width: 40.0,
                height: 20.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            inner,
            BoxLayout {
                x: 0.0,
                y: inner_top,
                width: 40.0,
                height: 60.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            40,
            80,
            Rgba::TRANSPARENT,
            &arena,
            &styles,
            &layouts,
            1.0,
        )
    }

    #[test]
    fn a_rounded_border_with_differing_side_colors_is_cut_by_an_overflow_clip_above_it() {
        let page = clipped_inner(
            "border: 6px solid; border-color: #ff0000 #00ff00 #0000ff #ffff00; border-radius: 12px;",
            -10.0,
        );
        for y in (0..20).chain(40..80) {
            for x in 0..40 {
                assert_eq!(rgba(&page, x, y)[3], 0, "({x}, {y}) is outside the clip");
            }
        }
        // Inside the clip the left side (yellow) and the right side (green) show.
        assert!(
            is_color(rgba(&page, 2, 30), [255, 255, 0]),
            "{:?}",
            rgba(&page, 2, 30)
        );
        assert!(
            is_color(rgba(&page, 37, 30), [0, 255, 0]),
            "{:?}",
            rgba(&page, 37, 30)
        );
    }

    fn is_color(pixel: [u8; 4], rgb: [u8; 3]) -> bool {
        pixel[3] == 255 && pixel[..3] == rgb
    }

    #[test]
    fn a_box_shadow_is_cut_by_an_overflow_clip_above_it() {
        // The same hand-laid tree as the layers: the inner box sits where the
        // clip is only partly over it, with an inset highlight and a drop shadow.
        for (what, css) in [
            (
                "an outer shadow",
                "background-color: #ff0000; box-shadow: 0 0 0 6px #00ff00;",
            ),
            (
                "a blurred outer shadow",
                "background-color: #ff0000; box-shadow: 0 0 8px 4px #00ff00;",
            ),
            (
                "an inset shadow",
                "background-color: #ff0000; box-shadow: inset 0 0 0 6px #00ff00;",
            ),
        ] {
            let page = clipped_inner(css, 10.0);
            // The clip is rows 20..40 of a 40 x 80 page; nothing may be drawn outside it.
            for y in (0..20).chain(40..80) {
                for x in 0..40 {
                    assert_eq!(
                        rgba(&page, x, y)[3],
                        0,
                        "{what}: ({x}, {y}) is outside the clip"
                    );
                }
            }
            assert!(
                (20..40).any(|y| rgba(&page, 20, y)[3] > 0),
                "{what}: and it shows inside"
            );
        }
    }

    #[test]
    fn a_background_layer_is_cut_by_an_overflow_clip_above_it() {
        for (what, css) in [
            ("a color", "background-color: #ff0000;"),
            (
                "a gradient",
                "background-image: linear-gradient(#ff0000, #ff0000);",
            ),
            (
                "a tiled gradient",
                "background-image: repeating-linear-gradient(to bottom, #ff0000 0 3px, transparent 3px 6px);",
            ),
        ] {
            // The inner box is laid out 15px above the clip (its parent's own
            // top is at 20): its top part must not show above the clip.
            let page = clipped_inner(css, 0.0);
            assert_eq!(rgba(&page, 20, 5)[3], 0, "{what}: nothing above the clip");
            assert_eq!(rgba(&page, 20, 50)[3], 0, "{what}: nothing below the clip");
            let inside = (20..40).any(|y| rgba(&page, 20, y)[3] > 0);
            assert!(inside, "{what}: but it shows inside the clip");
        }
    }

    #[test]
    fn radial_and_conic_gradients_paint() {
        let radial = background_box(
            ".box { background-image: radial-gradient(circle closest-side, #ffffff, #000000); }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert!(rgba(&radial, 20, 20)[0] >= 240, "white at the center");
        assert!(rgba(&radial, 0, 20)[0] <= 20 && rgba(&radial, 20, 0)[0] <= 20);
        let conic = background_box(
            ".box { background-image: conic-gradient(#ff0000 0 25%, #00ff00 0 50%, #0000ff 0 75%, #ffff00 0); }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(
            rgba(&conic, 30, 10),
            [255, 0, 0, 255],
            "upper right quarter"
        );
        assert_eq!(rgba(&conic, 30, 30), [0, 255, 0, 255], "lower right");
        assert_eq!(rgba(&conic, 10, 30), [0, 0, 255, 255], "lower left");
        assert_eq!(rgba(&conic, 10, 10), [255, 255, 0, 255], "upper left");
    }

    #[test]
    fn a_gradient_goes_under_the_inset_shadow_and_the_border() {
        let page = background_box(
            ".box { border: 3px solid #00ff00; background-image: linear-gradient(#ff0000, #ff0000); box-shadow: inset 0 0 0 5px #0000ff; }",
            (40, 40.0, 40.0),
            1.0,
        );
        assert_eq!(rgba(&page, 1, 20), [0, 255, 0, 255], "the border on top");
        assert_eq!(
            rgba(&page, 5, 20),
            [0, 0, 255, 255],
            "then the inset shadow"
        );
        assert_eq!(rgba(&page, 20, 20), [255, 0, 0, 255], "the gradient inside");
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            glass,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
                margin: florui_layout::BoxMargin::ZERO,
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
    fn a_backdrop_blur_continues_the_colors_to_the_panels_edge_instead_of_fading() {
        // A uniform red backdrop blurs to the same red everywhere, the edge
        // included: the blur reads past the edge by mirroring it, not as
        // transparent pixels that would pull the panel toward the canvas.
        let buffer = backdrop_over_red_buffer("backdrop-filter: blur(6px);");
        for (x, y) in [(0, 0), (19, 0), (0, 19), (19, 19), (0, 10), (10, 0)] {
            assert_eq!(
                pixel_rgb(&buffer, x, y),
                [0xff, 0, 0],
                "the panel's edge pixel at ({x}, {y}) faded"
            );
        }
    }

    #[test]
    fn mirroring_reflects_an_edge_outward_with_the_edge_pixel_included() {
        // Four pixels: -1 reads 0, -2 reads 1, 4 reads 3, 5 reads 2.
        assert_eq!(mirrored(-1, 4), 0);
        assert_eq!(mirrored(-2, 4), 1);
        assert_eq!(mirrored(0, 4), 0);
        assert_eq!(mirrored(3, 4), 3);
        assert_eq!(mirrored(4, 4), 3);
        assert_eq!(mirrored(5, 4), 2);
        // Further out than one reflection keeps bouncing between the edges.
        assert_eq!(mirrored(8, 4), 0);
        assert_eq!(mirrored(-5, 4), 3);
    }

    /// Red on the left and blue on the right, a `blur(4px)` panel over both,
    /// painted at `scale` (every box in canvas pixels, as a HiDPI window
    /// passes them).
    fn blurred_red_and_blue(scale: f32) -> Canvas {
        let tree: Element = view! {
            <div class="back">
                <div class="left"></div>
                <div class="right"></div>
                <div class="glass"></div>
            </div>
        };
        let css = ".left { background-color: #ff0000; } .right { background-color: #0000ff; } \
                   .glass { backdrop-filter: blur(4px); }";
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(css).unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let back = arena.roots()[0];
        let children = arena.children(back);
        let (width, height) = (60.0 * scale, 20.0 * scale);
        let mut layouts = HashMap::new();
        let mut place = |node, x: f32, w: f32| {
            layouts.insert(
                node,
                BoxLayout {
                    x,
                    y: 0.0,
                    width: w,
                    height,
                    margin: florui_layout::BoxMargin::ZERO,
                },
            );
        };
        place(back, 0.0, width);
        place(children[0], 0.0, width / 2.0);
        place(children[1], width / 2.0, width / 2.0);
        place(children[2], 0.0, width);
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            width as u32,
            height as u32,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            scale,
        )
    }

    /// How many pixels of the middle row are neither the pure red nor the
    /// pure blue: the width of the blurred seam.
    fn seam_width(buffer: &Canvas) -> u32 {
        let y = buffer.height() / 2;
        (0..buffer.width())
            .filter(|&x| {
                let [red, _, blue] = pixel_rgb(buffer, x, y);
                red > 8 && red < 247 && blue > 8 && blue < 247
            })
            .count() as u32
    }

    #[test]
    fn a_backdrop_blur_is_in_css_pixels_so_a_scaled_canvas_blurs_over_twice_as_many() {
        let at_one = seam_width(&blurred_red_and_blue(1.0));
        let at_two = seam_width(&blurred_red_and_blue(2.0));
        assert!(at_one >= 4, "the seam at scale 1 is {at_one} pixels wide");
        assert!(
            at_two as f32 >= at_one as f32 * 1.6,
            "at scale 2 the seam is {at_two} pixels wide against {at_one} at scale 1: the blur \
             radius was not scaled to the canvas"
        );
    }

    /// Twelve alternating red and blue stripes, 10 px wide and 80 tall, with
    /// a 80x60 glass panel at (20, 10) styled by `glass_css`, painted at
    /// `scale`.
    fn glass_over_stripes(glass_css: &str, scale: f32) -> Canvas {
        let tree: Element = view! {
            <div class="back">
                <div class="red"></div><div class="blue"></div>
                <div class="red"></div><div class="blue"></div>
                <div class="red"></div><div class="blue"></div>
                <div class="red"></div><div class="blue"></div>
                <div class="red"></div><div class="blue"></div>
                <div class="red"></div><div class="blue"></div>
                <div class="glass"></div>
            </div>
        };
        let css = format!(
            ".red {{ background-color: #ff0000; }} .blue {{ background-color: #0000ff; }} \
             .glass {{ border-radius: 12px; {glass_css} }}"
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
        let back = arena.roots()[0];
        let children = arena.children(back);
        let mut layouts = HashMap::new();
        let mut place = |node, x: f32, y: f32, w: f32, h: f32| {
            layouts.insert(
                node,
                BoxLayout {
                    x: x * scale,
                    y: y * scale,
                    width: w * scale,
                    height: h * scale,
                    margin: florui_layout::BoxMargin::ZERO,
                },
            );
        };
        place(back, 0.0, 0.0, 120.0, 80.0);
        for (index, stripe) in children[..12].iter().enumerate() {
            place(*stripe, index as f32 * 10.0, 0.0, 10.0, 80.0);
        }
        place(children[12], 20.0, 10.0, 80.0, 60.0);
        let mut font = Font::load_embedded();
        paint_to_buffer(
            &mut font,
            (120.0 * scale) as u32,
            (80.0 * scale) as u32,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            scale,
        )
    }

    fn differing(a: &Canvas, b: &Canvas) -> Vec<(u32, u32)> {
        (0..a.height())
            .flat_map(|y| (0..a.width()).map(move |x| (x, y)))
            .filter(|&(x, y)| a.pixel(x, y) != b.pixel(x, y))
            .collect()
    }

    const IDENTITY_FILTER: &str = "backdrop-filter: brightness(1);";

    #[test]
    fn the_effective_glass_settings_report_the_limits_and_off() {
        let material = |refraction, edge, quality| florui_style::GlassMaterial {
            refraction,
            edge,
            light_angle: 0.0,
            light_strength: 0.0,
            quality,
        };
        let full = florui_style::GlassQuality::Full;
        let kept = glass_effective(&material(8.0, 20.0, full)).expect("in effect");
        assert_eq!(
            (kept.refraction, kept.edge, kept.clamped),
            (8.0, 20.0, false)
        );
        let cut = glass_effective(&material(48.0, 28.0, full)).expect("in effect");
        assert_eq!((cut.refraction, cut.clamped), (28.0, true));
        let off = material(8.0, 20.0, florui_style::GlassQuality::Off);
        assert_eq!(glass_effective(&off), None);
    }

    #[test]
    fn the_glass_material_moves_the_backdrop_only_in_the_panels_edge_band() {
        let basic = glass_over_stripes(IDENTITY_FILTER, 1.0);
        let refracted = glass_over_stripes(
            &format!(
                "{IDENTITY_FILTER} --florui-glass: refract; --florui-glass-refraction: 8px; \
                 --florui-glass-edge: 16px;"
            ),
            1.0,
        );
        let moved = differing(&basic, &refracted);
        assert!(moved.len() > 50, "only {} pixels moved", moved.len());
        for (x, y) in moved {
            let inside = (20..100).contains(&x) && (10..70).contains(&y);
            assert!(inside, "({x}, {y}) is outside the panel");
            let deep = (37..83).contains(&x) && (27..53).contains(&y);
            assert!(!deep, "({x}, {y}) is deeper than the band");
        }
    }

    #[test]
    fn zero_refraction_and_the_off_quality_are_exactly_the_basic_glass() {
        let basic = glass_over_stripes(IDENTITY_FILTER, 1.0);
        for extra in [
            "--florui-glass: refract; --florui-glass-refraction: 0px;",
            "--florui-glass: refract; --florui-glass-quality: off; --florui-glass-light-strength: 1;",
        ] {
            let other = glass_over_stripes(&format!("{IDENTITY_FILTER} {extra}"), 1.0);
            assert_eq!(differing(&basic, &other), vec![], "{extra}");
        }
    }

    #[test]
    fn the_material_needs_a_backdrop_filter() {
        let plain = glass_over_stripes("", 1.0);
        let asked = glass_over_stripes(
            "--florui-glass: refract; --florui-glass-light-strength: 1;",
            1.0,
        );
        assert_eq!(differing(&plain, &asked), vec![]);
    }

    #[test]
    fn the_rim_light_brightens_the_side_facing_the_light_only() {
        let unlit = glass_over_stripes(IDENTITY_FILTER, 1.0);
        let lit = glass_over_stripes(
            &format!(
                "{IDENTITY_FILTER} --florui-glass: refract; --florui-glass-refraction: 0px; \
                 --florui-glass-light-angle: 270deg; --florui-glass-light-strength: 1;"
            ),
            1.0,
        );
        let sum = |c: &Canvas, x, y| {
            pixel_rgb(c, x, y)
                .iter()
                .map(|v| u32::from(*v))
                .sum::<u32>()
        };
        assert!(
            sum(&lit, 21, 40) > sum(&unlit, 21, 40),
            "the left edge faces the light"
        );
        assert_eq!(
            sum(&lit, 98, 40),
            sum(&unlit, 98, 40),
            "the right edge faces away"
        );
        assert_eq!(
            sum(&lit, 60, 40),
            sum(&unlit, 60, 40),
            "the interior is not lit"
        );
        for (x, y) in differing(&unlit, &lit) {
            let p = lit.pixel(x, y).expect("in range");
            assert!(p.red() <= p.alpha() && p.green() <= p.alpha() && p.blue() <= p.alpha());
        }
    }

    #[test]
    fn the_glass_material_scales_with_the_canvas() {
        // At twice the scale the same CSS lights and moves the same band, in
        // twice the pixels: nothing lands outside the (scaled) panel.
        let css = format!(
            "{IDENTITY_FILTER} --florui-glass: refract; --florui-glass-refraction: 8px; \
             --florui-glass-edge: 16px;"
        );
        let basic = glass_over_stripes(IDENTITY_FILTER, 2.0);
        let refracted = glass_over_stripes(&css, 2.0);
        let moved = differing(&basic, &refracted);
        assert!(moved.len() > 200);
        assert!(
            moved
                .iter()
                .all(|&(x, y)| (40..200).contains(&x) && (20..140).contains(&y))
        );
        assert!(
            moved
                .iter()
                .all(|&(x, y)| !((74..166).contains(&x) && (54..106).contains(&y)))
        );
    }

    #[test]
    fn a_refraction_request_above_the_limit_is_counted_as_clamped() {
        use florui_profile::Counter;

        florui_profile::start(false);
        glass_over_stripes(
            &format!(
                "{IDENTITY_FILTER} --florui-glass: refract; --florui-glass-refraction: 40px; \
                 --florui-glass-edge: 12px;"
            ),
            1.0,
        );
        glass_over_stripes(
            &format!("{IDENTITY_FILTER} --florui-glass: refract; --florui-glass-refraction: 6px;"),
            1.0,
        );
        florui_profile::finish_frame(Vec::new());
        let frames = florui_profile::frames();
        florui_profile::stop();
        assert_eq!(frames[0].counter(Counter::GlassRefractions), 2);
        assert_eq!(frames[0].counter(Counter::GlassClamped), 1);
        assert!(frames[0].counter(Counter::GlassPixelsMoved) > 0);
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
        assert_bounded_and_full_target_paint_within(1, || {
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
    fn saturating_without_dividing_gives_the_same_pixels_as_unpremultiplying() {
        let mut worst = 0u8;
        let mut differing = 0u32;
        let mut total = 0u32;
        for alpha in (1..=255u16).step_by(2) {
            for r in (0..=alpha).step_by(17) {
                for g in (0..=alpha).step_by(23) {
                    for b in (0..=alpha).step_by(29) {
                        let pixel =
                            PremultipliedColorU8::from_rgba(r as u8, g as u8, b as u8, alpha as u8)
                                .expect("premultiplied");
                        for factor in [0.0f32, 0.4, 1.0, 1.5, 3.0] {
                            let (new, old) = (
                                saturate_premultiplied(pixel, factor),
                                saturate_by_unpremultiplying(pixel, factor),
                            );
                            total += 1;
                            let d = new
                                .red()
                                .abs_diff(old.red())
                                .max(new.green().abs_diff(old.green()))
                                .max(new.blue().abs_diff(old.blue()))
                                .max(new.alpha().abs_diff(old.alpha()));
                            if d > 0 {
                                differing += 1;
                            }
                            worst = worst.max(d);
                            assert!(
                                new.red() <= new.alpha()
                                    && new.green() <= new.alpha()
                                    && new.blue() <= new.alpha()
                            );
                        }
                    }
                }
            }
        }
        assert!(worst <= 1, "the largest difference is {worst}");
        assert!(
            f64::from(differing) < f64::from(total) * 0.02,
            "{differing} of {total} pixels differ by a rounding step"
        );
    }

    #[test]
    fn a_filter_after_the_blur_gives_the_same_pixels_whether_or_not_it_sees_the_margin() {
        let mut backdrop = Pixmap::new(80, 60).expect("a pixmap");
        for (index, pixel) in backdrop.pixels_mut().iter_mut().enumerate() {
            let (x, y) = (index % 80, index / 80);
            let (r, g, b) = if (x / 15 + y / 15) % 2 == 0 {
                (230, 40, 70)
            } else {
                (30, 200, 160)
            };
            *pixel = PremultipliedColorU8::from_rgba(r, g, b, 255).expect("opaque");
        }
        for chain in [
            vec![FilterFunction::Blur(6.0), FilterFunction::Saturate(1.6)],
            vec![FilterFunction::Saturate(1.6), FilterFunction::Blur(6.0)],
            vec![
                FilterFunction::Blur(4.0),
                FilterFunction::Brightness(0.8),
                FilterFunction::Blur(5.0),
                FilterFunction::Contrast(1.3),
            ],
        ] {
            let margin = blur_margin(&chain);
            let mut padded = mirror_padded(&backdrop, margin).expect("padded");
            apply_filters(&mut padded, &chain);
            let reference = cropped(&padded, margin, 80, 60).expect("cropped");
            let ours = filtered_backdrop(backdrop.clone(), &chain, false);
            assert_eq!(ours.data(), reference.data(), "{chain:?}");
        }
    }

    fn checker_backdrop(width: u32, height: u32) -> Pixmap {
        let mut backdrop = Pixmap::new(width, height).expect("a pixmap");
        for (index, pixel) in backdrop.pixels_mut().iter_mut().enumerate() {
            let (x, y) = (index as u32 % width, index as u32 / width);
            let (r, g, b) = if (x / 23 + y / 19) % 2 == 0 {
                (230, 40, 70)
            } else {
                (30, 200, 160)
            };
            *pixel = PremultipliedColorU8::from_rgba(r, g, b, 255).expect("opaque");
        }
        backdrop
    }

    /// The mean and the largest difference, over all channels, between the
    /// exact blur and the fast one of the same chain.
    fn fast_against_exact(chain: &[FilterFunction]) -> (f64, u8) {
        let backdrop = checker_backdrop(320, 220);
        let exact = filtered_backdrop(backdrop.clone(), chain, false);
        let fast = filtered_backdrop(backdrop, chain, true);
        let mut total = 0u64;
        let mut worst = 0u8;
        for (a, b) in exact.data().iter().zip(fast.data()) {
            let d = a.abs_diff(*b);
            total += u64::from(d);
            worst = worst.max(d);
        }
        (total as f64 / exact.data().len() as f64, worst)
    }

    #[test]
    fn a_blur_too_small_to_shrink_is_the_same_bytes_in_both_modes() {
        let backdrop = checker_backdrop(120, 90);
        for sigma in [3.0f32, 5.5] {
            let chain = [FilterFunction::Blur(sigma), FilterFunction::Saturate(1.4)];
            assert_eq!(
                filtered_backdrop(backdrop.clone(), &chain, false).data(),
                filtered_backdrop(backdrop.clone(), &chain, true).data(),
                "sigma {sigma}"
            );
        }
    }

    #[test]
    fn a_fast_blur_stays_close_to_the_exact_one_at_each_reduction() {
        // Measured on hard-edged blocks: 0.16 and 2 at sigma 8, 0.01 and 1 at 20.
        for (sigma, mean, worst) in [(8.0f32, 0.5, 6u8), (20.0, 0.1, 4)] {
            let (measured_mean, measured_worst) =
                fast_against_exact(&[FilterFunction::Blur(sigma)]);
            assert!(
                measured_mean > 0.0,
                "sigma {sigma}: the fast blur gave the exact bytes, so it did not run"
            );
            assert!(
                measured_mean < mean && measured_worst <= worst,
                "sigma {sigma}: mean {measured_mean}, worst {measured_worst}"
            );
        }
    }

    #[test]
    fn a_fast_blur_keeps_a_flat_backdrop_flat_to_the_edge_and_the_color_under_alpha() {
        let mut backdrop = Pixmap::new(97, 61).expect("a pixmap");
        backdrop.fill(tiny_skia::Color::from_rgba8(90, 140, 200, 255));
        let blurred = filtered_backdrop(backdrop.clone(), &[FilterFunction::Blur(20.0)], true);
        for (a, b) in backdrop.data().iter().zip(blurred.data()) {
            assert!(a.abs_diff(*b) <= 1, "{a} against {b}");
        }
        let mut translucent = checker_backdrop(97, 61);
        for pixel in translucent.pixels_mut() {
            *pixel = PremultipliedColorU8::from_rgba(
                pixel.red() / 2,
                pixel.green() / 2,
                pixel.blue() / 2,
                128,
            )
            .expect("premultiplied");
        }
        let blurred = filtered_backdrop(translucent, &[FilterFunction::Blur(9.0)], true);
        assert!(
            blurred
                .pixels()
                .iter()
                .all(|p| p.red() <= p.alpha() && p.green() <= p.alpha() && p.blue() <= p.alpha())
        );
    }

    #[test]
    fn a_moved_group_is_drawn_by_the_same_pixels_as_the_general_transform() {
        let mut group = Pixmap::new(37, 29).expect("a pixmap");
        for (index, pixel) in group.pixels_mut().iter_mut().enumerate() {
            let value = (index * 7 % 251) as u8;
            *pixel = PremultipliedColorU8::from_rgba(
                value / 2,
                value / 3,
                value / 4,
                value.max(value / 2 + 1),
            )
            .unwrap_or(PremultipliedColorU8::TRANSPARENT);
        }
        let paint = PixmapPaint {
            opacity: 0.8,
            ..Default::default()
        };
        let mut checked = 0;
        for tx in [
            -9.6f32, -3.51, -0.51, -0.49, 0.0, 0.25, 0.49, 0.51, 1.0, 7.75, 12.4, 40.2,
        ] {
            for ty in [-4.6f32, -0.51, 0.0, 0.49, 3.3, 9.7] {
                let transform = Transform::from_translate(tx, ty);
                let mut general = Pixmap::new(96, 72).expect("a pixmap");
                general.fill(tiny_skia::Color::from_rgba8(20, 60, 90, 255));
                let mut shifted = general.clone();
                general.draw_pixmap(20, 15, group.as_ref(), &paint, transform, None);
                let (dx, dy) = whole_pixel_shift(transform).expect("a translation");
                shifted.draw_pixmap(
                    20 + dx,
                    15 + dy,
                    group.as_ref(),
                    &paint,
                    Transform::identity(),
                    None,
                );
                assert_eq!(general.data(), shifted.data(), "translate({tx}, {ty})");
                checked += 1;
            }
        }
        assert_eq!(checked, 72);
        // Exactly or nearly half a pixel is left to the general path.
        for half in [-9.5f32, -0.5, 0.5, 1.5, 12.5, 0.5004] {
            assert_eq!(
                whole_pixel_shift(Transform::from_translate(half, 0.0)),
                None,
                "{half}"
            );
        }
        assert_eq!(whole_pixel_shift(Transform::from_scale(2.0, 2.0)), None);
        assert_eq!(whole_pixel_shift(Transform::from_rotate(10.0)), None);
        assert_eq!(
            whole_pixel_shift(Transform::from_row(1.0, 0.0, 0.1, 1.0, 5.0, 0.0)),
            None
        );
    }

    #[test]
    fn the_direct_blit_rounds_like_the_general_drawing_over_any_backdrop() {
        let mut source = Pixmap::new(41, 33).expect("a pixmap");
        for (index, pixel) in source.pixels_mut().iter_mut().enumerate() {
            let alpha = match index % 5 {
                0 => 0,
                1 => 255,
                _ => (index * 37 % 254 + 1) as u8,
            };
            let color = (index * 91 % 256) as u8;
            *pixel =
                PremultipliedColorU8::from_rgba(color.min(alpha), (color / 2).min(alpha), 0, alpha)
                    .expect("premultiplied");
        }
        for backdrop in [
            tiny_skia::Color::from_rgba8(0, 0, 0, 0),
            tiny_skia::Color::from_rgba8(20, 60, 90, 255),
            tiny_skia::Color::from_rgba8(200, 30, 140, 128),
        ] {
            for (x, y) in [(10, 8), (-7, -5), (30, 20), (-60, 3), (80, 80)] {
                let mut direct = Pixmap::new(64, 48).expect("a pixmap");
                direct.fill(backdrop);
                let paint = PixmapPaint::default();
                draw_shifted(&mut direct, &source, (x, y), &paint, None);
                let mut reference = Pixmap::new(64, 48).expect("a pixmap");
                reference.fill(backdrop);
                let (x0, y0) = (x.max(0), y.max(0));
                let (x1, y1) = ((x + 41).min(64), (y + 33).min(48));
                if x1 > x0 && y1 > y0 {
                    let rect =
                        IntRect::from_xywh(x0 - x, y0 - y, (x1 - x0) as u32, (y1 - y0) as u32)
                            .expect("a rect");
                    let visible = source.clone_rect(rect).expect("a crop");
                    reference.draw_pixmap(
                        x0,
                        y0,
                        visible.as_ref(),
                        &paint,
                        Transform::identity(),
                        None,
                    );
                }
                assert_eq!(
                    direct.data(),
                    reference.data(),
                    "at ({x}, {y}) over {backdrop:?}"
                );
            }
        }
    }

    #[test]
    fn a_clip_is_ignored_only_where_it_is_fully_opaque() {
        let mut source = Pixmap::new(20, 12).expect("a pixmap");
        source.fill(tiny_skia::Color::from_rgba8(200, 90, 40, 255));
        let paint = PixmapPaint::default();
        let mut full = Mask::new(48, 32).expect("a mask");
        full.fill_path(
            &tiny_skia::PathBuilder::from_rect(
                tiny_skia::Rect::from_xywh(0.0, 0.0, 48.0, 32.0).expect("a rect"),
            ),
            FillRule::Winding,
            false,
            Transform::identity(),
        );
        assert!(mask_is_full(&full, 48, (5, 4, 25, 16)));
        // A mask that is half open in one corner of the drawn area is not full
        // there, and the drawing then honors it.
        let mut cut = full.clone();
        cut.data_mut()[4 * 48 + 5] = 128;
        assert!(!mask_is_full(&cut, 48, (5, 4, 25, 16)));
        assert!(mask_is_full(&cut, 48, (6, 4, 25, 16)));
        let mut with_mask = Pixmap::new(48, 32).expect("a pixmap");
        draw_shifted(&mut with_mask, &source, (5, 4), &paint, Some(&cut));
        let partly = with_mask.pixel(5, 4).map_or(0, |p| p.alpha());
        assert!(
            (100..=156).contains(&partly),
            "the half-open pixel is half drawn: {partly}"
        );
        assert_eq!(with_mask.pixel(6, 4).map(|p| p.alpha()), Some(255));
        let mut without = Pixmap::new(48, 32).expect("a pixmap");
        draw_shifted(&mut without, &source, (5, 4), &paint, Some(&full));
        assert_eq!(without.pixel(5, 4).map(|p| p.alpha()), Some(255));
    }

    #[test]
    fn a_moved_group_that_runs_off_the_canvas_is_the_part_of_the_unclipped_drawing() {
        let mut group = Pixmap::new(37, 29).expect("a pixmap");
        group.fill(tiny_skia::Color::from_rgba8(200, 90, 40, 255));
        let paint = PixmapPaint::default();
        for (tx, ty) in [
            (-9.6f32, -4.6f32),
            (50.3, 20.2),
            (-40.0, 10.0),
            (60.0, -30.7),
        ] {
            let (dx, dy) =
                whole_pixel_shift(Transform::from_translate(tx, ty)).expect("a translation");
            // The same drawing on a canvas with 100 pixels to spare on every side.
            let mut wide = Pixmap::new(296, 272).expect("a pixmap");
            wide.draw_pixmap(
                100 + 3 + dx,
                100 + 15 + dy,
                group.as_ref(),
                &paint,
                Transform::identity(),
                None,
            );
            let expected = wide
                .clone_rect(IntRect::from_xywh(100, 100, 96, 72).expect("a rect"))
                .expect("a crop");
            let mut small = Pixmap::new(96, 72).expect("a pixmap");
            draw_shifted(&mut small, &group, (3 + dx, 15 + dy), &paint, None);
            assert_eq!(small.data(), expected.data(), "translate({tx}, {ty})");
        }
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
    fn a_box_with_no_width_paints_no_pixel() {
        // Chrome 153 paints nothing for a flex item squeezed to 0px; the
        // rasterizer drew a one pixel line for an empty rect.
        let tree: Element = view! { <div class="box" /> };
        let arena = Arena::build(&tree);
        let rules = florui_style::parse_stylesheet(".box { background-color: #ff0000; }").unwrap();
        let styles = florui_style::compute(
            &arena,
            &rules,
            &InteractionState::new(),
            florui_style::Viewport::default(),
            &mut florui_style::AnimationTimeline::default(),
        );
        let mut layouts = HashMap::new();
        layouts.insert(
            arena.roots()[0],
            BoxLayout {
                x: 10.0,
                y: 5.0,
                width: 0.0,
                height: 20.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        let mut font = Font::load_embedded();
        let canvas = paint_to_buffer(
            &mut font,
            30,
            30,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        for x in 8..13 {
            assert_eq!(pixel_rgb(&canvas, x, 10), [255, 255, 255], "column {x}");
        }
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
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            inner,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
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
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            back,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 40.0,
                height: 40.0,
                margin: florui_layout::BoxMargin::ZERO,
            },
        );
        layouts.insert(
            glass,
            BoxLayout {
                x: 0.0,
                y: 0.0,
                width: 20.0,
                height: 20.0,
                margin: florui_layout::BoxMargin::ZERO,
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

    /// A scroll box (or, unclipped, a page longer than its canvas) whose edge only
    /// hides nodes below it, plus two nodes whose
    /// own box is outside the clip but whose ink is not: a short box above the
    /// clip with text spilling down into it, and a row below it with a shadow
    /// thrown back up. Painted with and without the off-screen shortcut.
    fn list_scene(culling: bool, clipped: bool) -> (Canvas, u32, u32) {
        let height = if clipped { 140 } else { 100 };
        let div = |class: &str, children: Vec<Element>| {
            Element::node("div", vec![("class".into(), class.into())], children)
        };
        let mut children = vec![div("spacer", Vec::new())];
        for i in 0..10 {
            let class = if i == 0 { "row first" } else { "row" };
            children.push(div(class, vec![Element::text(format!("row {i}"))]));
        }
        children.push(div(
            "pull",
            vec![Element::text("aaaa bbbb cccc dddd eeee ffff")],
        ));
        let tree = div(if clipped { "clip" } else { "page" }, children);
        let css = "
            .page { width: 200px; display: flex; flex-direction: column; }
            .clip { width: 200px; height: 100px; overflow: hidden;
                    display: flex; flex-direction: column; }
            .spacer { height: 100px; flex-shrink: 0; }
            .row { height: 20px; flex-shrink: 0; background-color: #223; color: #eee;
                   font-size: 12px; }
            .first { box-shadow: 0px -40px 4px rgba(255, 0, 0, 0.9); }
            .pull { margin-top: -330px; width: 30px; height: 10px; flex-shrink: 0;
                    color: #00ff00; font-size: 14px; }
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
        CULLING_OFF.with(|off| off.set(!culling));
        let buffer = paint_to_buffer(
            &mut font,
            200,
            height,
            Rgba::opaque(0, 0, 0),
            &arena,
            &styles,
            &layouts,
            1.0,
        );
        CULLING_OFF.with(|off| off.set(false));
        (buffer, 200, height)
    }

    #[test]
    fn skipping_nodes_outside_the_clip_changes_no_pixel() {
        for clipped in [true, false] {
            let (with, width, height) = list_scene(true, clipped);
            let (without, _, _) = list_scene(false, clipped);
            assert_eq!(with.data(), without.data(), "clipped: {clipped}");

            // Not vacuous: the two nodes that look skippable did paint inside
            // the visible area.
            let has = |predicate: &dyn Fn([u8; 3]) -> bool, rows: std::ops::Range<u32>| {
                rows.into_iter()
                    .any(|py| (0..width).any(|px| predicate(pixel_rgb(&with, px, py))))
            };
            assert!(
                has(&|p| p[1] > 0x80 && p[0] < 0x40 && p[2] < 0x40, 0..30),
                "text spilling out of a box above the area is drawn (clipped: {clipped})"
            );
            assert!(
                has(&|p| p[0] > 0x40 && p[1] < 0x20 && p[2] < 0x20, 50..100),
                "a shadow thrown up from a row below the area is drawn (clipped: {clipped})"
            );
            assert!(height >= 100);
        }
    }

    #[test]
    fn skipping_nodes_outside_the_clip_shows_in_the_painted_count() {
        use florui_profile::Counter;

        for clipped in [true, false] {
            let painted = |culling: bool| {
                florui_profile::start(false);
                list_scene(culling, clipped);
                florui_profile::finish_frame(Vec::new());
                let frames = florui_profile::frames();
                florui_profile::stop();
                frames[0].counter(Counter::NodesPainted)
            };
            let (with, without) = (painted(true), painted(false));
            assert!(
                with < without,
                "{with} nodes painted with the shortcut, {without} without (clipped: {clipped})"
            );
        }
    }

    /// The off-screen shortcut sizes a text-only element's ink with the plain
    /// text metrics, while painting shapes it through the inline path; the
    /// two have to agree for it to be a safe bound.
    #[test]
    fn plain_and_inline_shaping_measure_single_style_text_alike() {
        let words = "Item number 17 with a few more words to wrap onto several lines";
        let mut font = Font::load_embedded();
        for (size, weight) in [(11.0, 400.0), (14.0, 400.0), (14.0, 700.0), (27.0, 400.0)] {
            let css = format!(".t {{ font-size: {size}px; font-weight: {weight}; }}");
            let tree: Element = Element::node(
                "div",
                vec![("class".into(), "t".into())],
                vec![Element::text(words)],
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
            let node = arena.roots()[0];
            assert!(florui_layout::is_inline_formatting_context(
                &arena, &styles, node
            ));
            for width in [40.0, 90.0, 200.0, 4000.0] {
                let inline = florui_layout::shape_inline_formatting_context(
                    &mut font,
                    &arena,
                    &styles,
                    (&HashMap::new(), 1.0),
                    node,
                    width,
                )
                .unwrap();
                let plain = font.measure_memoized(
                    &mut None,
                    florui_text::TextStyle::new(florui_text::FontFamily::SansSerif, size, weight),
                    words,
                    Some(width),
                );
                assert_eq!(
                    (inline.layout.width, inline.layout.height),
                    (plain.width, plain.height),
                    "{size}px weight {weight} wrapped at {width}"
                );
            }
        }
    }

    /// A page of text in several sizes and weights, one paragraph mixing a
    /// bold span into regular text, painted with outlines extracted directly
    /// (`direct`) or through the glyph cache.
    fn text_page(direct: bool, scale_factor: f32) -> Canvas {
        let div = |class: &str, children: Vec<Element>| {
            Element::node("div", vec![("class".into(), class.into())], children)
        };
        let tree = div(
            "page",
            vec![
                div("small", vec![Element::text("Small text, 0123456789 fjgqy")]),
                div("big", vec![Element::text("Bold Heading Hg")]),
                div(
                    "para",
                    vec![
                        Element::text("Mixed paragraph with "),
                        Element::node(
                            "span",
                            vec![("class".into(), "strong".into())],
                            vec![Element::text("a bold span")],
                        ),
                        Element::text(" inside regular text that wraps onto more lines."),
                    ],
                ),
            ],
        );
        let css = "
            .page { width: 180px; display: flex; flex-direction: column; }
            .small { font-size: 11px; color: #123456; }
            .big { font-size: 22px; font-weight: 700; color: #a01010; }
            .para { font-size: 14px; color: #202020; }
            .strong { font-weight: 700; }
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
        let physical: HashMap<florui_style::NodeId, BoxLayout> = layouts
            .iter()
            .map(|(&id, l)| {
                (
                    id,
                    BoxLayout {
                        x: l.x * scale_factor,
                        y: l.y * scale_factor,
                        width: l.width * scale_factor,
                        height: l.height * scale_factor,
                        margin: florui_layout::BoxMargin::ZERO,
                    },
                )
            })
            .collect();
        DIRECT_GLYPHS.with(|d| d.set(direct));
        let buffer = paint_to_buffer(
            &mut font,
            (200.0 * scale_factor) as u32,
            (160.0 * scale_factor) as u32,
            Rgba::opaque(255, 255, 255),
            &arena,
            &styles,
            &physical,
            scale_factor,
        );
        DIRECT_GLYPHS.with(|d| d.set(false));
        buffer
    }

    #[test]
    fn cached_glyph_outlines_paint_the_same_pixels_as_extracting_them_each_time() {
        for scale_factor in [1.0, 1.5] {
            let direct = text_page(true, scale_factor);
            let cold = text_page(false, scale_factor);
            let warm = text_page(false, scale_factor);
            assert!(
                direct.data().iter().any(|&byte| byte != 255),
                "the page has ink, so the comparison is not vacuous (scale {scale_factor})"
            );
            assert_eq!(
                direct.data(),
                cold.data(),
                "cold cache, scale {scale_factor}"
            );
            assert_eq!(
                direct.data(),
                warm.data(),
                "warm cache, scale {scale_factor}"
            );
        }
    }
}
