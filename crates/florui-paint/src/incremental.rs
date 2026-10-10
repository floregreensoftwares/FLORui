//! Painting only what changed since the last frame.
//!
//! A window that is painted again after a hover or a click has changed in a few
//! boxes, and painting all of it again costs the whole window. [`Repainter`]
//! keeps the last frame and a note of what each box looked like; the next frame
//! compares the boxes with the notes, takes the room the changed ones can have
//! inked (before and after) as the part to paint again, and paints that part
//! alone over the kept pixels. Whatever it cannot be sure of (a new size, a new
//! scale, a part that is most of the window) it paints whole, so the result is
//! always the pixels a whole paint gives.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use florui_layout::BoxLayout;
use florui_style::{Arena, ComputedStyle, NodeId, Rgba};
use florui_text::Font;

use super::{
    Canvas, ImagePaint, Surface, TextInputPaint, absolute_position, filter_inflation,
    map_rect_aabb, own_extent, paint_nodes, resolve_transform, subtree_extent, to_tiny_skia_color,
};

/// A rectangle `(x0, y0, x1, y1)` in painted pixels.
type Area = (f32, f32, f32, f32);

/// Everything one frame is painted from.
pub struct PaintInputs<'a> {
    pub font: &'a mut Font,
    pub width: u32,
    pub height: u32,
    pub canvas_color: Rgba,
    pub arena: &'a Arena,
    pub styles: &'a HashMap<NodeId, ComputedStyle>,
    /// In painted pixels.
    pub layouts: &'a HashMap<NodeId, BoxLayout>,
    pub scale_factor: f32,
    pub text_inputs: Option<&'a HashMap<NodeId, TextInputPaint>>,
    pub images: Option<&'a HashMap<NodeId, ImagePaint>>,
}

/// What painting the last frame left, and how the next one was painted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Repaint {
    /// The whole frame was painted.
    Whole,
    /// Nothing had changed, so the frame is the one before it.
    Nothing,
    /// Only this part, `(x0, y0, x1, y1)` in painted pixels, was painted again.
    Part(i32, i32, i32, i32),
}

/// What a box looked like when it was last painted, so a change shows.
struct Note {
    style: Option<ComputedStyle>,
    layout: Option<BoxLayout>,
    /// What the tree says about the box: its tag, classes, attributes, text,
    /// parent and children.
    shape: u64,
    /// The room the box can ink, with the boxes it moves or filters.
    ink: Area,
    /// Painted from state this module cannot see change (an editable field's
    /// caret and selection), so painted again whenever any frame is.
    always: bool,
    /// Which decoded image it paints, as an address.
    image: usize,
    /// The reach of its backdrop filter or glass, `None` without one.
    backdrop: Option<f32>,
}

struct Memo {
    canvas: Canvas,
    width: u32,
    height: u32,
    canvas_color: Rgba,
    scale_factor: f32,
    notes: HashMap<NodeId, Note>,
}

/// Paints frames, keeping the last to paint only what changed in the next.
#[derive(Default)]
pub struct Repainter {
    memo: Option<Memo>,
    last: Option<Repaint>,
}

/// More of the window than this is painted whole: bookkeeping for a part that
/// large costs more than it saves.
const MOST_OF_THE_WINDOW: f32 = 0.6;

/// Room around a changed box that its anti-aliased edge can reach.
const FRINGE: f32 = 2.0;

/// Pixels painted beyond the part and thrown away.
const EDGE_ROOM: i32 = 16;

impl Repainter {
    pub fn new() -> Self {
        Self::default()
    }

    /// How the last frame was painted.
    pub fn last(&self) -> Option<Repaint> {
        self.last
    }

    /// Forgets the last frame: the next one is painted whole.
    pub fn forget(&mut self) {
        self.memo = None;
    }

    /// Paints the frame `inputs` describe and returns it.
    pub fn paint(&mut self, inputs: PaintInputs<'_>) -> &Canvas {
        let PaintInputs {
            font,
            width,
            height,
            canvas_color,
            arena,
            styles,
            layouts,
            scale_factor,
            text_inputs,
            images,
        } = inputs;
        let (kept_canvas, kept_notes) = match self.memo.take() {
            Some(memo)
                if memo.width == width
                    && memo.height == height
                    && memo.canvas_color == canvas_color
                    && memo.scale_factor == scale_factor =>
            {
                (Some(memo.canvas), Some(memo.notes))
            }
            _ => (None, None),
        };

        let fresh = take_notes(
            font,
            arena,
            styles,
            layouts,
            scale_factor,
            text_inputs,
            images,
        );
        let part = kept_notes
            .as_ref()
            .and_then(|old| damage(old, &fresh, (width, height)));

        let (canvas, repaint) = match (kept_canvas, kept_notes, part) {
            (Some(canvas), Some(_), Some(Damage::Nothing)) => (canvas, Repaint::Nothing),
            (Some(mut canvas), Some(_), Some(Damage::Part(area))) => {
                let (x0, y0, x1, y1) = (
                    (area.0.floor() as i32).max(0),
                    (area.1.floor() as i32).max(0),
                    (area.2.ceil() as i32).min(width as i32),
                    (area.3.ceil() as i32).min(height as i32),
                );
                repaint_part(
                    &mut canvas,
                    (x0, y0, x1, y1),
                    font,
                    canvas_color,
                    arena,
                    styles,
                    layouts,
                    scale_factor,
                    text_inputs,
                    images,
                );
                (canvas, Repaint::Part(x0, y0, x1, y1))
            }
            _ => (
                super::paint_to_buffer_with_desktop_extras(
                    font,
                    width,
                    height,
                    canvas_color,
                    arena,
                    styles,
                    layouts,
                    scale_factor,
                    text_inputs,
                    images,
                ),
                Repaint::Whole,
            ),
        };
        self.last = Some(repaint);
        &self
            .memo
            .insert(Memo {
                canvas,
                width,
                height,
                canvas_color,
                scale_factor,
                notes: fresh,
            })
            .canvas
    }
}

enum Damage {
    Nothing,
    Part(Area),
}

/// The part of the frame that has to be painted again, or `None` when that is
/// too much to be worth it.
fn damage(
    old: &HashMap<NodeId, Note>,
    new: &HashMap<NodeId, Note>,
    size: (u32, u32),
) -> Option<Damage> {
    let mut part: Option<Area> = None;
    let mut add = |ink: Area| {
        part = Some(match part {
            None => ink,
            Some(held) => union(held, ink),
        });
    };
    for (id, note) in new {
        match old.get(id) {
            Some(before) if same(before, note) && !note.always => {}
            Some(before) => {
                add(before.ink);
                add(note.ink);
            }
            None => add(note.ink),
        }
    }
    for (id, before) in old {
        if !new.contains_key(id) {
            add(before.ink);
        }
    }
    let Some(mut part) = part else {
        return Some(Damage::Nothing);
    };
    // A backdrop filter or glass reads the pixels around the box it filters, so a
    // change within its reach changes it, and what it filters has to be there to
    // be filtered: the whole box is painted again.
    loop {
        let mut grew = false;
        for note in new.values() {
            let Some(reach) = note.backdrop else { continue };
            let near = (
                part.0 - reach,
                part.1 - reach,
                part.2 + reach,
                part.3 + reach,
            );
            if overlaps(near, note.ink) && !contains(part, note.ink) {
                part = union(part, note.ink);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let part = (
        part.0 - FRINGE,
        part.1 - FRINGE,
        part.2 + FRINGE,
        part.3 + FRINGE,
    );
    let clamped = (
        part.0.max(0.0),
        part.1.max(0.0),
        part.2.min(size.0 as f32),
        part.3.min(size.1 as f32),
    );
    if clamped.2 <= clamped.0 || clamped.3 <= clamped.1 {
        return Some(Damage::Nothing);
    }
    let area = (clamped.2 - clamped.0) * (clamped.3 - clamped.1);
    if area > MOST_OF_THE_WINDOW * size.0 as f32 * size.1 as f32 {
        return None;
    }
    Some(Damage::Part(clamped))
}

fn same(a: &Note, b: &Note) -> bool {
    a.shape == b.shape
        && a.layout == b.layout
        && a.image == b.image
        && a.style == b.style
        && a.backdrop == b.backdrop
}

fn union(a: Area, b: Area) -> Area {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

fn overlaps(a: Area, b: Area) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

fn contains(outer: Area, inner: Area) -> bool {
    outer.0 <= inner.0 && outer.1 <= inner.1 && outer.2 >= inner.2 && outer.3 >= inner.3
}

/// Writes a note for every box of the tree.
fn take_notes(
    font: &mut Font,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> HashMap<NodeId, Note> {
    let ids = arena.find_all(|_, _| true);
    let mut notes = HashMap::with_capacity(ids.len());
    // The box that moves or filters everything below it, found from the top
    // down so a box under one is painted again with it.
    let mut top_group: HashMap<NodeId, Option<NodeId>> = HashMap::with_capacity(ids.len());
    let mut group_ink: HashMap<NodeId, Area> = HashMap::new();
    for &id in &ids {
        let above = arena
            .parent(id)
            .and_then(|parent| top_group.get(&parent).copied().flatten());
        let own = styles
            .get(&id)
            .is_some_and(|s| !s.transform.is_empty() || !s.filter.is_empty());
        top_group.insert(id, above.or(own.then_some(id)));
    }
    for &id in &ids {
        let style = styles.get(&id);
        let layout = layouts.get(&id).copied();
        let (x, y) = absolute_position(arena, layouts, id);
        let ink = match top_group.get(&id).copied().flatten() {
            Some(top) => *group_ink
                .entry(top)
                .or_insert_with(|| group_extent(font, arena, styles, layouts, top, scale_factor)),
            None => own_extent(arena, styles, layouts, font, id, x, y, scale_factor)
                .unwrap_or((x, y, x, y)),
        };
        let backdrop = style.and_then(|s| {
            let blur = filter_inflation(&s.backdrop_filter, scale_factor);
            let glass = matches!(s.glass, florui_style::GlassSpec::Material(_));
            (!s.backdrop_filter.is_empty() || glass).then_some(
                blur + if glass {
                    GLASS_REACH * scale_factor
                } else {
                    0.0
                },
            )
        });
        notes.insert(
            id,
            Note {
                style: style.cloned(),
                layout,
                shape: shape_of(arena, id),
                ink,
                always: text_inputs.is_some_and(|inputs| inputs.contains_key(&id)),
                image: images
                    .and_then(|images| images.get(&id))
                    .map_or(0, |image| std::sync::Arc::as_ptr(image) as usize),
                backdrop,
            },
        );
    }
    notes
}

/// How far, in logical pixels, a glass panel's refraction can move what lies
/// behind it, beyond its blur.
const GLASS_REACH: f32 = 64.0;

/// What `top` and everything under it paint, once it is moved or filtered.
fn group_extent(
    font: &mut Font,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    top: NodeId,
    scale_factor: f32,
) -> Area {
    let (x, y) = absolute_position(arena, layouts, top);
    let Some((x0, y0, x1, y1)) =
        subtree_extent(arena, styles, layouts, font, top, x, y, scale_factor)
    else {
        return (x, y, x, y);
    };
    let (Some(style), Some(layout)) = (styles.get(&top), layouts.get(&top)) else {
        return (x0, y0, x1, y1);
    };
    let pad = filter_inflation(&style.filter, scale_factor);
    let transform = resolve_transform(style, layout, x, y, scale_factor);
    map_rect_aabb(x0 - pad, y0 - pad, x1 + pad, y1 + pad, transform)
}

/// What the tree says about `id`, as a number.
fn shape_of(arena: &Arena, id: NodeId) -> u64 {
    let mut hasher = DefaultHasher::new();
    arena.tag(id).hash(&mut hasher);
    arena.classes(id).hash(&mut hasher);
    arena.attrs(id).hash(&mut hasher);
    arena.text_content(id).hash(&mut hasher);
    for item in arena.inline_items(id) {
        match item {
            florui_style::InlineItem::Text(text) => text.hash(&mut hasher),
            florui_style::InlineItem::Element(node) => node.hash(&mut hasher),
        }
    }
    arena.parent(id).hash(&mut hasher);
    arena.children(id).hash(&mut hasher);
    hasher.finish()
}

/// Paints `area` of `canvas` again, on a surface of its own that is just that
/// part: cleared to the canvas color, then every box that reaches into it, in
/// paint order. What lies outside the surface is cut by its edge, with no mask
/// to round a pixel differently from the way the whole frame has it.
#[allow(clippy::too_many_arguments)]
fn repaint_part(
    canvas: &mut Canvas,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
    font: &mut Font,
    canvas_color: Rgba,
    arena: &Arena,
    styles: &HashMap<NodeId, ComputedStyle>,
    layouts: &HashMap<NodeId, BoxLayout>,
    scale_factor: f32,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    // The part is painted with room all round it, and only the part is kept:
    // a shape cut by the edge of a surface comes out a little different within
    // a few pixels of that edge.
    let (canvas_width, canvas_height) = (canvas.width() as i32, canvas.height() as i32);
    let (px0, py0) = ((x0 - EDGE_ROOM).max(0), (y0 - EDGE_ROOM).max(0));
    let (px1, py1) = (
        (x1 + EDGE_ROOM).min(canvas_width),
        (y1 + EDGE_ROOM).min(canvas_height),
    );
    let Some(mut part) = Surface::new((px1 - px0) as u32, (py1 - py0) as u32, (px0, py0)) else {
        return;
    };
    part.pixmap.fill(to_tiny_skia_color(canvas_color));
    paint_nodes(
        &mut part,
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
    let part_width = (px1 - px0) as usize;
    let from = part.pixmap.data();
    let row = (x1 - x0) as usize * 4;
    let target = canvas.data_mut();
    for line in y0..y1 {
        let at = (((line - py0) as usize) * part_width + (x0 - px0) as usize) * 4;
        let to = (line as usize * canvas_width as usize + x0 as usize) * 4;
        target[to..to + row].copy_from_slice(&from[at..at + row]);
    }
}
