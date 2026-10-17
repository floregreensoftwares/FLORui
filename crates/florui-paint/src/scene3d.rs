//! A `transform-style: preserve-3d` context: the boxes in it are planes in one
//! shared 3D space, drawn in depth order rather than in tree order.
//!
//! Each box of the context is painted flat into a surface of its own (a box
//! that preserves 3D holds only itself; any other holds its whole subtree). The
//! surfaces are then placed through their full 4x4 matrices. Planes that cross
//! are cut along the line where they meet and the pieces drawn back to front,
//! by a binary space partition over the planes in the space the viewer sees
//! (after the perspective divide, where the viewer looks straight down `-z`),
//! the way a browser's compositor orders a 3D rendering context.

use florui_layout::{ContextPlane, Matrix4, NEAR_W, context_planes};

use super::projection::draw_projected;
use super::*;

/// Distances in that space below this count as lying on a plane.
const ON_PLANE: f64 = 1.0e-5;

struct Plane {
    /// The plane's matrix through its context, in absolute coordinates.
    matrix: Matrix4,
    surface: Surface,
    opacity: f32,
}

/// A corner of a piece of a plane: where it is on the plane's surface, and
/// where that lands in homogeneous coordinates.
#[derive(Clone, Copy)]
struct Vertex {
    uv: (f64, f64),
    h: [f64; 4],
}

impl Vertex {
    fn lerp(a: Vertex, b: Vertex, t: f64) -> Vertex {
        Vertex {
            uv: (
                a.uv.0 + (b.uv.0 - a.uv.0) * t,
                a.uv.1 + (b.uv.1 - a.uv.1) * t,
            ),
            h: std::array::from_fn(|k| a.h[k] + (b.h[k] - a.h[k]) * t),
        }
    }

    fn ndc(&self) -> [f64; 3] {
        [
            self.h[0] / self.h[3],
            self.h[1] / self.h[3],
            self.h[2] / self.h[3],
        ]
    }
}

#[derive(Clone)]
struct Piece {
    plane: usize,
    vertices: Vec<Vertex>,
    /// The piece's plane in the viewer's space, see [`plane_of`].
    equation: ([f64; 3], f64),
}

impl Piece {
    /// `None` for a piece that is edge-on or has no area, which draws nothing.
    fn new(plane: usize, vertices: Vec<Vertex>) -> Option<Piece> {
        if vertices.len() < 3 {
            return None;
        }
        let equation = plane_of(&vertices)?;
        Some(Piece {
            plane,
            vertices,
            equation,
        })
    }
}

/// Paints `root`, which preserves 3D and whose parent does not, and everything
/// in its context onto `buffer`.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_scene(
    buffer: &mut Surface,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    root: NodeId,
    scale_factor: f32,
    clip: Option<ClipRect>,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) {
    let planes = gather_planes(
        buffer,
        arena,
        styles,
        layouts,
        font,
        root,
        scale_factor,
        clip.as_ref(),
        text_inputs,
        images,
    );
    let mut pieces: Vec<Piece> = Vec::with_capacity(planes.len());
    for (index, plane) in planes.iter().enumerate() {
        let (ox, oy) = (
            f64::from(plane.surface.origin.0),
            f64::from(plane.surface.origin.1),
        );
        let (w, h) = (
            f64::from(plane.surface.pixmap.width()),
            f64::from(plane.surface.pixmap.height()),
        );
        let corners = [(ox, oy), (ox + w, oy), (ox + w, oy + h), (ox, oy + h)];
        let vertices: Vec<Vertex> = corners
            .iter()
            .map(|&(u, v)| Vertex {
                uv: (u, v),
                h: plane.matrix.map_homogeneous((u, v, 0.0)),
            })
            .collect();
        if let Some(piece) = clip_to_front(&vertices).and_then(|v| Piece::new(index, v)) {
            pieces.push(piece);
        }
    }

    let ordered = depth_order(pieces);
    let mask = clip.as_ref().and_then(|c| c.to_mask(buffer));
    for piece in ordered {
        let plane = &planes[piece.plane];
        let outline: Vec<(f32, f32)> = piece
            .vertices
            .iter()
            .map(|v| ((v.h[0] / v.h[3]) as f32, (v.h[1] / v.h[3]) as f32))
            .collect();
        draw_projected(
            &mut buffer.pixmap,
            buffer.origin,
            &plane.surface.pixmap,
            plane.surface.origin,
            &plane.matrix.flatten(),
            Some(&outline),
            plane.opacity,
            mask.as_ref(),
        );
    }
}

/// The boxes of the context of `root` that draw something, in painting order.
fn visible_planes(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    root: NodeId,
    scale_factor: f32,
) -> Vec<ContextPlane> {
    let order = |style: &ComputedStyle, children: &[NodeId]| {
        paint_order(styles, Some(style.display), children)
    };
    context_planes(arena, styles, layouts, root, scale_factor, &order)
        .into_iter()
        .filter(|plane| {
            let (Some(style), Some(layout)) = (styles.get(&plane.node), layouts.get(&plane.node))
            else {
                return false;
            };
            style.opacity > 0.0 && !plane.is_hidden_back(style, layout)
        })
        .collect()
}

/// What the whole context of `root` can ink, in the plane `root`'s parent is
/// painted on: every box's own extent (its subtree's, for one that does not
/// preserve 3D) placed through its matrix, flattened.
pub(super) fn scene_extent(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    root: NodeId,
    scale_factor: f32,
) -> Option<(f32, f32, f32, f32)> {
    let mut bounds: Option<(f32, f32, f32, f32)> = None;
    for boxed in visible_planes(arena, styles, layouts, root, scale_factor) {
        let extent = if boxed.preserves {
            own_extent(
                arena,
                styles,
                layouts,
                font,
                boxed.node,
                boxed.abs.0,
                boxed.abs.1,
                scale_factor,
            )
        } else {
            subtree_extent(
                arena,
                styles,
                layouts,
                font,
                boxed.node,
                boxed.abs.0,
                boxed.abs.1,
                scale_factor,
            )
        };
        let Some((x0, y0, x1, y1)) = extent else {
            continue;
        };
        let pad = styles
            .get(&boxed.node)
            .map_or(0.0, |s| filter_inflation(&s.filter, scale_factor));
        let Some((a, b, c, d)) =
            boxed
                .matrix
                .flatten()
                .map_rect_bounds((x0 - pad, y0 - pad, x1 + pad, y1 + pad))
        else {
            continue;
        };
        bounds = Some(match bounds {
            Some((p, q, r, s)) => (p.min(a), q.min(b), r.max(c), s.max(d)),
            None => (a, b, c, d),
        });
    }
    bounds
}

/// Every visible box of the context under `root`, painted into a surface of
/// its own.
#[allow(clippy::too_many_arguments)]
fn gather_planes(
    buffer: &Surface,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    root: NodeId,
    scale_factor: f32,
    clip: Option<&ClipRect>,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> Vec<Plane> {
    visible_planes(arena, styles, layouts, root, scale_factor)
        .into_iter()
        .filter_map(|boxed| {
            render_plane(
                buffer,
                arena,
                styles,
                layouts,
                font,
                boxed.node,
                boxed.abs,
                scale_factor,
                clip,
                boxed.matrix,
                boxed.preserves,
                text_inputs,
                images,
            )
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_plane(
    buffer: &Surface,
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    font: &mut Font,
    node: NodeId,
    node_abs: (f32, f32),
    scale_factor: f32,
    clip: Option<&ClipRect>,
    context: Matrix4,
    own_only: bool,
    text_inputs: Option<&HashMap<NodeId, TextInputPaint>>,
    images: Option<&HashMap<NodeId, ImagePaint>>,
) -> Option<Plane> {
    let opacity = styles.get(&node).map_or(1.0, |s| s.opacity);
    let surface = render_group(
        buffer,
        arena,
        styles,
        layouts,
        font,
        node,
        node_abs,
        scale_factor,
        clip.cloned(),
        Projection(context.flatten()),
        own_only,
        text_inputs,
        images,
    )?;
    Some(Plane {
        matrix: context,
        surface,
        opacity,
    })
}

/// `vertices` cut to the half space in front of the eye; `None` when nothing
/// is left.
fn clip_to_front(vertices: &[Vertex]) -> Option<Vec<Vertex>> {
    let mut out = Vec::with_capacity(vertices.len() + 1);
    for i in 0..vertices.len() {
        let (a, b) = (vertices[i], vertices[(i + 1) % vertices.len()]);
        let (a_in, b_in) = (a.h[3] > NEAR_W, b.h[3] > NEAR_W);
        if a_in {
            out.push(a);
        }
        if a_in != b_in {
            let t = (NEAR_W - a.h[3]) / (b.h[3] - a.h[3]);
            out.push(Vertex::lerp(a, b, t));
        }
    }
    (out.len() >= 3).then_some(out)
}

/// The pieces in the order to draw them: the far side of every cut before the
/// near side, and pieces on one plane in tree order.
fn depth_order(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out = Vec::with_capacity(pieces.len());
    order_into(pieces, &mut out);
    out
}

/// The plane of a piece in the viewer's space: a normal `n` pointing toward
/// the viewer (`n.z >= 0`) and `d`, so that `n . p + d` is the signed
/// distance of `p` from it. `None` for a piece that is edge-on or has no area,
/// which draws nothing.
fn plane_of(vertices: &[Vertex]) -> Option<([f64; 3], f64)> {
    let points: Vec<[f64; 3]> = vertices.iter().map(Vertex::ndc).collect();
    // Newell's method: the normal of a polygon from all of its edges.
    let mut n = [0.0f64; 3];
    for i in 0..points.len() {
        let (a, b) = (points[i], points[(i + 1) % points.len()]);
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if !length.is_finite() || length < 1.0e-9 || n[2].abs() < length * 1.0e-9 {
        return None;
    }
    if n[2] < 0.0 {
        n = [-n[0], -n[1], -n[2]];
    }
    let n = [n[0] / length, n[1] / length, n[2] / length];
    let centroid = points.iter().fold([0.0; 3], |acc, p| {
        [acc[0] + p[0], acc[1] + p[1], acc[2] + p[2]]
    });
    let count = points.len() as f64;
    let c = [
        centroid[0] / count,
        centroid[1] / count,
        centroid[2] / count,
    ];
    Some((n, -(n[0] * c[0] + n[1] * c[1] + n[2] * c[2])))
}

fn order_into(mut pieces: Vec<Piece>, out: &mut Vec<Piece>) {
    if pieces.is_empty() {
        return;
    }
    let splitter = pieces.remove(0);
    let (n, d) = splitter.equation;
    let (mut front, mut back, mut before, mut after) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for piece in pieces {
        let sides: Vec<f64> = piece
            .vertices
            .iter()
            .map(|v| {
                let p = v.ndc();
                n[0] * p[0] + n[1] * p[1] + n[2] * p[2] + d
            })
            .collect();
        let any_front = sides.iter().any(|&s| s > ON_PLANE);
        let any_back = sides.iter().any(|&s| s < -ON_PLANE);
        match (any_front, any_back) {
            (true, false) => front.push(piece),
            (false, true) => back.push(piece),
            (false, false) => {
                // On the splitter's plane: tree order decides.
                if piece.plane < splitter.plane {
                    before.push(piece);
                } else {
                    after.push(piece);
                }
            }
            (true, true) => {
                let (near, far) = split(&piece, &n, d);
                if let Some(near) = near {
                    front.push(near);
                }
                if let Some(far) = far {
                    back.push(far);
                }
            }
        }
    }
    order_into(back, out);
    out.extend(before);
    out.push(splitter);
    out.extend(after);
    order_into(front, out);
}

/// `piece` cut along the plane `n . p + d = 0`: the part in front and the part
/// behind it.
fn split(piece: &Piece, n: &[f64; 3], d: f64) -> (Option<Piece>, Option<Piece>) {
    // The cut is exact in homogeneous coordinates (`n . (x, y, z) + d * w`
    // is linear along an edge), so the new corners lie on the plane.
    let signed = |v: &Vertex| n[0] * v.h[0] + n[1] * v.h[1] + n[2] * v.h[2] + d * v.h[3];
    let (mut front, mut back) = (Vec::new(), Vec::new());
    let count = piece.vertices.len();
    for i in 0..count {
        let (a, b) = (piece.vertices[i], piece.vertices[(i + 1) % count]);
        let (fa, fb) = (signed(&a), signed(&b));
        let a_ndc = {
            let p = a.ndc();
            n[0] * p[0] + n[1] * p[1] + n[2] * p[2] + d
        };
        if a_ndc >= -ON_PLANE {
            front.push(a);
        }
        if a_ndc <= ON_PLANE {
            back.push(a);
        }
        if (fa > 0.0 && fb < 0.0) || (fa < 0.0 && fb > 0.0) {
            let cut = Vertex::lerp(a, b, fa / (fa - fb));
            front.push(cut);
            back.push(cut);
        }
    }
    (
        Piece::new(piece.plane, front),
        Piece::new(piece.plane, back),
    )
}
