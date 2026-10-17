//! A whole frame painted in horizontal bands, one band to a task, at once.
//!
//! Painting is slow for the area it covers, and bands of the frame do not
//! depend on one another: each is painted on a surface of its own, with room
//! beyond the band that is thrown away (a shape cut by the edge of a surface
//! comes out a little different within a few pixels of that edge), and its
//! rows are copied into the frame. The tasks run on the thread pool, each
//! with a font of its own made from the same fonts, and with the caches every
//! thread keeps for itself.
//! The bands read the tree from several threads. They read a [`Tree`], which
//! is the tree without the handlers its nodes declared (those are `Rc`s, and the
//! [`florui_style::Arena`] that holds them cannot be shared); painting is given
//! the tree alone, so it cannot reach a handler and nothing here is unsafe.

use florui_style::StyleMap;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Mutex;

use florui_layout::BoxLayout;
use florui_style::{NodeId, Rgba, Tree};
use florui_text::{Font, FontSeed};

use super::incremental::node_inks;
use super::material::Params;
use super::{
    BAND_VIEW, BandView, Canvas, ImagePaint, Surface, TextInputPaint, absolute_position,
    paint_nodes, to_tiny_skia_color,
};

/// A rectangle `(x0, y0, x1, y1)` in painted pixels.
type Ink = (f32, f32, f32, f32);

fn union(a: Ink, b: Ink) -> Ink {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// The rows of a band: where it starts and ends, and the bytes of the frame there.
type Band<'a> = (i32, i32, &'a mut [u8]);

/// What every band reads.
struct Shared<'a> {
    arena: &'a Tree,
    styles: &'a StyleMap,
    layouts: &'a HashMap<NodeId, BoxLayout>,
    text_inputs: Option<&'a HashMap<NodeId, TextInputPaint>>,
    images: Option<&'a HashMap<NodeId, ImagePaint>>,
}

impl<'a> Shared<'a> {
    /// Everything is read through a method, so that a closure takes the whole of
    /// `Shared` and not one of its fields.
    #[allow(clippy::type_complexity)]
    fn parts(
        &self,
    ) -> (
        &'a Tree,
        &'a StyleMap,
        &'a HashMap<NodeId, BoxLayout>,
        Option<&'a HashMap<NodeId, TextInputPaint>>,
        Option<&'a HashMap<NodeId, ImagePaint>>,
    ) {
        (
            self.arena,
            self.styles,
            self.layouts,
            self.text_inputs,
            self.images,
        )
    }
}

/// Rows of room painted above and below a band and thrown away.
const EDGE_ROOM: i32 = 12;

/// A band is at least this many rows: thinner ones are mostly edge.
const LEAST_BAND_ROWS: usize = 48;

/// Frames of fewer pixels than this are painted in one piece.
const LEAST_PIXELS: usize = 250_000;

thread_local! {
    /// This thread's own font, made from the seed of the frame's.
    static BAND_FONT: RefCell<Option<Font>> = const { RefCell::new(None) };
}

fn with_font<R>(seed: &FontSeed, work: impl FnOnce(&mut Font) -> R) -> R {
    // The font is taken out of its slot while it is used: a task that waits for
    // work it has handed to the pool may be given another band to paint on this
    // very thread, and that band must not find the font borrowed.
    let kept = BAND_FONT.with(|slot| slot.borrow_mut().take());
    let mut font = match kept {
        Some(font) if font.generation() == seed.generation() => font,
        _ => Font::from_seed(seed),
    };
    let result = work(&mut font);
    BAND_FONT.with(|slot| *slot.borrow_mut() = Some(font));
    result
}

/// Whether a frame of `width x height` is worth painting in bands.
pub(crate) fn worth_it(width: u32, height: u32) -> bool {
    rayon::current_num_threads() > 1
        && width as usize * height as usize >= LEAST_PIXELS
        && height as usize >= 2 * LEAST_BAND_ROWS
}

/// The rows, `(top, bottom)`, of the frame that a backdrop filter, glass, a
/// transform or a filter works on: a band edge inside them would cut what it
/// blurs, refracts, turns or scales, which is sampled between pixels.
fn filtered_rows(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    inks: &HashMap<NodeId, Ink>,
    scale_factor: f32,
) -> Vec<(f32, f32)> {
    arena
        .find_all(|_, _| true)
        .into_iter()
        .filter_map(|id| {
            let (style, layout) = (styles.get(&id)?, layouts.get(&id)?);
            let reach = match &style.glass {
                florui_style::GlassSpec::Material(material) => {
                    Params::resolve(material, scale_factor).map_or(0.0, |p| p.reach() as f32)
                }
                _ => 0.0,
            };
            if style.has_transform() || style.preserve_3d || !style.filter.is_empty() {
                return inks.get(&id).map(|ink| (ink.1, ink.3));
            }
            (!style.backdrop_filter.is_empty() || reach > 0.0).then(|| {
                let (_, y) = absolute_position(arena, layouts, id);
                (y - reach, y + layout.height + reach)
            })
        })
        .collect()
}

/// Where the bands of a frame `rows` high start and end, as rows `0 = first <
/// ... < last = rows`: evenly, but never inside a range of `filtered`.
fn band_edges(rows: usize, filtered: &[(f32, f32)]) -> Vec<usize> {
    // A band to a thread at most: each thread keeps the same band from frame to
    // frame, so what it has learnt (its caches, its font) is of use to the next.
    let bands = rayon::current_num_threads()
        .min(rows / LEAST_BAND_ROWS)
        .max(1);
    let each = rows.div_ceil(bands);
    let mut edges = vec![0];
    for index in 1..bands {
        let mut edge = (index * each) as f32;
        for &(top, bottom) in filtered {
            if edge > top && edge < bottom {
                edge = top.max(0.0).floor();
            }
        }
        let edge = (edge as usize).min(rows);
        if edge > *edges.last().expect("edges start with 0") {
            edges.push(edge);
        }
    }
    edges.push(rows);
    edges
}

/// The frame `inputs` describe, painted in bands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint(
    font: &mut Font,
    width: u32,
    height: u32,
    canvas_color: Rgba,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> Canvas {
    let mut canvas =
        Canvas::new(width, height).expect("a banded paint requires a nonzero-sized canvas");
    let seed = font.seed();
    let shared = Shared {
        arena,
        styles,
        layouts,
        text_inputs,
        images,
    };
    let (w, h) = (width as usize, height as usize);
    // The room each box inks is worked out once, with the font that has shaped
    // the text before; a band then paints the boxes that reach it.
    let own_inks = node_inks(font, arena, styles, layouts, scale_factor);
    let edges = band_edges(
        h,
        &filtered_rows(arena, styles, layouts, &own_inks, scale_factor),
    );
    let mut subtree_inks = own_inks.clone();
    // Children before parents: the ids come parents first.
    for id in arena.find_all(|_, _| true).into_iter().rev() {
        if let (Some(parent), Some(&ink)) = (arena.parent(id), subtree_inks.get(&id)) {
            subtree_inks
                .entry(parent)
                .and_modify(|held| *held = union(*held, ink))
                .or_insert(ink);
        }
    }
    let own_inks: Vec<(NodeId, Ink)> = own_inks.into_iter().collect();
    let subtree_inks: Vec<(NodeId, Ink)> = subtree_inks.into_iter().collect();
    // The rows of each band, cut from the frame in order.
    let mut rest = canvas.data_mut();
    let mut bands = Vec::with_capacity(edges.len() - 1);
    for pair in edges.windows(2) {
        let (head, tail) = rest.split_at_mut((pair[1] - pair[0]) * w * 4);
        bands.push((pair[0] as i32, pair[1] as i32, head));
        rest = tail;
    }
    let bands: Vec<Mutex<Option<Band<'_>>>> = bands
        .into_iter()
        .map(|band| Mutex::new(Some(band)))
        .collect();
    // Band number `index` is painted by thread number `index`, every frame.
    rayon::broadcast(|context| {
        let Some(slot) = bands.get(context.index()) else {
            return;
        };
        let Some((first, last, chunk)) = slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let (top, bottom) = ((first - EDGE_ROOM).max(0), (last + EDGE_ROOM).min(h as i32));
        let Some(mut band) = Surface::new(width, (bottom - top) as u32, (0, top)) else {
            return;
        };
        band.pixmap.fill(to_tiny_skia_color(canvas_color));
        let (arena, styles, layouts, text_inputs, images) = shared.parts();
        let (band_top, band_bottom) = (top as f32, bottom as f32);
        let reaching = |inks: &[(NodeId, Ink)]| -> HashSet<NodeId> {
            inks.iter()
                .filter(|(_, ink)| {
                    ink.1 < band_bottom && ink.3 > band_top && ink.0 < width as f32 && ink.2 > 0.0
                })
                .map(|(id, _)| *id)
                .collect()
        };
        let view = BandView {
            own: reaching(&own_inks),
            subtree: reaching(&subtree_inks),
        };
        let before = BAND_VIEW.with(|slot| slot.replace(Some(Rc::new(view))));
        with_font(&seed, |font| {
            paint_nodes(
                &mut band,
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
        });
        BAND_VIEW.with(|slot| *slot.borrow_mut() = before);
        let from = band.pixmap.data();
        let start = ((first - top) as usize) * w * 4;
        chunk.copy_from_slice(&from[start..start + chunk.len()]);
    });
    canvas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_cut_evenly_and_never_through_a_filtered_range() {
        let plain = band_edges(700, &[]);
        assert_eq!(plain.first(), Some(&0));
        assert_eq!(plain.last(), Some(&700));
        assert!(plain.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            plain
                .windows(2)
                .all(|pair| pair[1] - pair[0] >= LEAST_BAND_ROWS || plain.len() == 2),
            "{plain:?}"
        );
        // A glass panel over rows 100..400: no edge falls inside it.
        let cut = band_edges(700, &[(100.0, 400.0)]);
        assert!(
            cut.iter().all(|&edge| edge <= 100 || edge >= 400),
            "{cut:?}"
        );
        assert_eq!(cut.last(), Some(&700));
        assert!(cut.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// Painting reads a tree, and the handlers in it are the part that cannot be
    /// shared between threads: this keeps every file that paints from asking the
    /// tree for one, which is what [`super::Shared`] relies on.
    #[test]
    fn painting_never_reaches_the_handlers_of_the_tree() {
        let sources = [
            ("lib.rs", include_str!("lib.rs")),
            ("background.rs", include_str!("background.rs")),
            ("incremental.rs", include_str!("incremental.rs")),
            ("material.rs", include_str!("material.rs")),
            ("text_shadow.rs", include_str!("text_shadow.rs")),
            ("rounded.rs", include_str!("rounded.rs")),
            ("image_source.rs", include_str!("image_source.rs")),
        ];
        // Spelled in pieces so this list is not itself a match.
        let forbidden = [
            ["arena", ".handler("].concat(),
            [".submit", "_handler("].concat(),
            [".selection", "_handler("].concat(),
            [".value", "_binding("].concat(),
            [".value", "_handler("].concat(),
        ];
        for (file, text) in sources {
            for call in &forbidden {
                assert!(
                    !text.contains(call.as_str()),
                    "{file} asks the tree for a handler ({call}), which bands painted on other threads must not"
                );
            }
        }
    }
}
