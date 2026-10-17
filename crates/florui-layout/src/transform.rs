//! The matrix a node's transforms resolve to. Painting and hit testing both
//! read it from here, so a node is hit exactly where it is drawn.
//!
//! A node's box is a plane, so a flat (non-`preserve-3d`) node only ever needs
//! the 3x3 homography of its `z = 0` plane ([`Matrix4::homography`]); the full
//! 4x4 matters inside a `preserve-3d` context, where planes keep their depth
//! until the context is flattened into its own plane.

use florui_style::StyleMap;
use std::collections::HashMap;

use florui_style::{ComputedStyle, NodeId, TransformFunction, Tree};

use crate::{BoxLayout, absolute_position};

/// A 4x4 transform in CSS's `matrix3d()` order: the array is column-major, so
/// `m[column * 4 + row]` and a point maps as `x' = m[0]*x + m[4]*y + m[8]*z +
/// m[12]` (and likewise `y'`, `z'`, `w'` from rows 1 to 3). A 2D `matrix(a, b,
/// c, d, e, f)` is `[a, b, 0, 0, c, d, 0, 0, 0, 0, 1, 0, e, f, 0, 1]`.
///
/// Held in `f64`: a chain of rotations and a perspective divide loses visible
/// precision in `f32`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix4(pub [f64; 16]);

/// Points whose `w` is below this are behind the eye and are not drawn.
pub const NEAR_W: f64 = 1.0e-6;

impl Matrix4 {
    pub const IDENTITY: Matrix4 = Matrix4([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);

    pub fn translate(x: f64, y: f64, z: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[12] = x;
        m.0[13] = y;
        m.0[14] = z;
        m
    }

    pub fn scale(x: f64, y: f64, z: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0] = x;
        m.0[5] = y;
        m.0[10] = z;
        m
    }

    /// A rotation by `degrees` about the axis `(x, y, z)` (any length; the
    /// zero vector is no rotation), exactly CSS's `rotate3d()`: about z it is
    /// clockwise on screen, y pointing down.
    pub fn rotate(x: f64, y: f64, z: f64, degrees: f64) -> Self {
        let length = (x * x + y * y + z * z).sqrt();
        if !length.is_finite() || length == 0.0 {
            return Self::IDENTITY;
        }
        let (x, y, z) = (x / length, y / length, z / length);
        let half = degrees.to_radians() / 2.0;
        let (sin, cos) = half.sin_cos();
        let (sc, sq) = (sin * cos, sin * sin);
        Matrix4([
            1.0 - 2.0 * (y * y + z * z) * sq,
            2.0 * (x * y * sq + z * sc),
            2.0 * (x * z * sq - y * sc),
            0.0,
            2.0 * (x * y * sq - z * sc),
            1.0 - 2.0 * (x * x + z * z) * sq,
            2.0 * (y * z * sq + x * sc),
            0.0,
            2.0 * (x * z * sq + y * sc),
            2.0 * (y * z * sq - x * sc),
            1.0 - 2.0 * (x * x + y * y) * sq,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    /// CSS `skew(ax, ay)`: x shears with the tangent of `ax_degrees`, y with
    /// that of `ay_degrees`.
    pub fn skew(ax_degrees: f64, ay_degrees: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[4] = ax_degrees.to_radians().tan();
        m.0[1] = ay_degrees.to_radians().tan();
        m
    }

    /// CSS `perspective(d)`; an infinite or non-positive `d` is no perspective.
    pub fn perspective(d: f64) -> Self {
        let mut m = Self::IDENTITY;
        if d.is_finite() && d > 0.0 {
            m.0[11] = -1.0 / d;
        }
        m
    }

    /// CSS `matrix(a, b, c, d, e, f)`.
    pub fn from_2d(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        let mut m = Self::IDENTITY;
        m.0[0] = a;
        m.0[1] = b;
        m.0[4] = c;
        m.0[5] = d;
        m.0[12] = e;
        m.0[13] = f;
        m
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// `self` applied after `inner`: the product `self * inner`.
    pub fn after(self, inner: Matrix4) -> Matrix4 {
        let (a, b) = (&self.0, &inner.0);
        let mut out = [0.0; 16];
        for column in 0..4 {
            for row in 0..4 {
                out[column * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[column * 4 + k]).sum();
            }
        }
        Matrix4(out)
    }

    /// The matrix as it acts on the `z = 0` plane alone, with its own `z`
    /// output zeroed: whatever lies in front of or behind the plane no longer
    /// shows. This is what flattening a 3D context into its plane does.
    pub fn flatten(&self) -> Matrix4 {
        let mut m = self.0;
        for column in 0..4 {
            m[column * 4 + 2] = 0.0;
        }
        m[8] = 0.0;
        m[9] = 0.0;
        m[10] = 1.0;
        m[11] = 0.0;
        Matrix4(m)
    }

    /// The 3x3 homography of the `z = 0` plane, row-major: `(x, y, 1)` maps to
    /// `(h0*x + h1*y + h2, h3*x + h4*y + h5, h6*x + h7*y + h8)` with the last
    /// component the divisor.
    pub fn homography(&self) -> [f64; 9] {
        let m = &self.0;
        [m[0], m[4], m[12], m[1], m[5], m[13], m[3], m[7], m[15]]
    }

    /// The inverse of [`Self::homography`], row-major, or `None` when the
    /// plane is edge-on or flattened.
    pub fn inverse_homography(&self) -> Option<[f64; 9]> {
        invert3(&self.homography())
    }
    /// Whether the plane is exactly where it started, whatever the matrix does
    /// off the plane.
    pub fn is_plane_identity(&self) -> bool {
        self.homography() == [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    }

    /// The plane's `(a, b, c, d, e, f)` when it stays a 2D affine map (no
    /// perspective on the plane), in CSS `matrix()` order.
    pub fn plane_affine(&self) -> Option<[f64; 6]> {
        let h = self.homography();
        (h[6] == 0.0 && h[7] == 0.0 && h[8] == 1.0).then(|| [h[0], h[3], h[1], h[4], h[2], h[5]])
    }

    /// `(x, y, z)` mapped to homogeneous `[x', y', z', w']`.
    pub fn map_homogeneous(&self, (x, y, z): (f64, f64, f64)) -> [f64; 4] {
        let m = &self.0;
        [
            m[0] * x + m[4] * y + m[8] * z + m[12],
            m[1] * x + m[5] * y + m[9] * z + m[13],
            m[2] * x + m[6] * y + m[10] * z + m[14],
            m[3] * x + m[7] * y + m[11] * z + m[15],
        ]
    }

    /// The plane point `(x, y)` on screen, or `None` behind the eye.
    pub fn map_point(&self, (x, y): (f32, f32)) -> Option<(f32, f32)> {
        let [px, py, _, w] = self.map_homogeneous((f64::from(x), f64::from(y), 0.0));
        (w > NEAR_W).then(|| ((px / w) as f32, (py / w) as f32))
    }

    /// The plane point that lands on screen at `(x, y)`, or `None` when the
    /// plane is edge-on or flattened, or the point is only reached behind the
    /// eye.
    pub fn unmap_point(&self, (x, y): (f32, f32)) -> Option<(f32, f32)> {
        let inverse = invert3(&self.homography())?;
        let (x, y) = (f64::from(x), f64::from(y));
        let u = inverse[0] * x + inverse[1] * y + inverse[2];
        let v = inverse[3] * x + inverse[4] * y + inverse[5];
        let w = inverse[6] * x + inverse[7] * y + inverse[8];
        if !w.is_finite() || w == 0.0 {
            return None;
        }
        let (u, v) = (u / w, v / w);
        let [_, _, _, forward] = self.map_homogeneous((u, v, 0.0));
        (forward > NEAR_W && u.is_finite() && v.is_finite()).then_some((u as f32, v as f32))
    }

    /// The screen bounding box `(x0, y0, x1, y1)` of the rectangle on the
    /// plane, with the part behind the eye cut away; `None` when none of it is
    /// in front.
    pub fn map_rect_bounds(&self, rect: (f32, f32, f32, f32)) -> Option<(f32, f32, f32, f32)> {
        let polygon = self.project_quad(rect);
        if polygon.is_empty() {
            return None;
        }
        let mut bounds = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for (x, y) in polygon {
            bounds = (
                bounds.0.min(x),
                bounds.1.min(y),
                bounds.2.max(x),
                bounds.3.max(y),
            );
        }
        Some(bounds)
    }

    /// The outline of the rectangle `(x0, y0, x1, y1)` on screen, clipped to
    /// the half space in front of the eye (`w > 0`).
    pub fn project_quad(&self, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> Vec<(f32, f32)> {
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
            .map(|(x, y)| self.map_homogeneous((f64::from(x), f64::from(y), 0.0)));
        let mut clipped: Vec<[f64; 4]> = Vec::with_capacity(5);
        for i in 0..4 {
            let (current, next) = (corners[i], corners[(i + 1) % 4]);
            let (current_in, next_in) = (current[3] > NEAR_W, next[3] > NEAR_W);
            if current_in {
                clipped.push(current);
            }
            if current_in != next_in {
                let t = (NEAR_W - current[3]) / (next[3] - current[3]);
                clipped.push(std::array::from_fn(|k| {
                    current[k] + t * (next[k] - current[k])
                }));
            }
        }
        clipped
            .into_iter()
            .map(|p| ((p[0] / p[3]) as f32, (p[1] / p[3]) as f32))
            .collect()
    }

    /// Whether the plane shows its back to the viewer. The sign of the
    /// determinant of three corners' homogeneous coordinates is the sign of
    /// the transformed normal along the line of sight, and stays right when
    /// perspective makes some corners' `w` differ; an untransformed plane
    /// gives a positive one.
    pub fn faces_away(&self, (x0, y0, x1, y1): (f32, f32, f32, f32)) -> bool {
        let map = |x: f32, y: f32| self.map_homogeneous((f64::from(x), f64::from(y), 0.0));
        let (a, b, c) = (map(x0, y0), map(x1, y0), map(x1, y1));
        let h = |p: [f64; 4]| [p[0], p[1], p[3]];
        let (a, b, c) = (h(a), h(b), h(c));
        let det = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
        det < 0.0
    }
}

/// The inverse of a row-major 3x3, or `None` when it is singular.
fn invert3(h: &[f64; 9]) -> Option<[f64; 9]> {
    let minor = |a: f64, b: f64, c: f64, d: f64| a * d - b * c;
    let c00 = minor(h[4], h[5], h[7], h[8]);
    let c01 = -minor(h[3], h[5], h[6], h[8]);
    let c02 = minor(h[3], h[4], h[6], h[7]);
    let det = h[0] * c00 + h[1] * c01 + h[2] * c02;
    if !det.is_finite() || det == 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        c00 * inv,
        -minor(h[1], h[2], h[7], h[8]) * inv,
        minor(h[1], h[2], h[4], h[5]) * inv,
        c01 * inv,
        minor(h[0], h[2], h[6], h[8]) * inv,
        -minor(h[0], h[2], h[3], h[5]) * inv,
        c02 * inv,
        -minor(h[0], h[1], h[6], h[7]) * inv,
        minor(h[0], h[1], h[3], h[4]) * inv,
    ])
}

/// `style`'s own transforms (`translate`, `rotate`, `scale`, then the
/// `transform` list, in that order) folded into one matrix about its
/// `transform-origin`, for a node whose border box sits at `(x, y)` with
/// `layout`'s size. `scale_factor` converts the authored lengths to the
/// caller's pixels (`1.0` for logical ones). A parent's `perspective` is not
/// included; see [`resolve_node_transform`].
pub fn resolve_transform(
    style: &ComputedStyle,
    layout: &BoxLayout,
    x: f32,
    y: f32,
    scale_factor: f32,
) -> Matrix4 {
    if !style.has_transform() {
        return Matrix4::IDENTITY;
    }
    let sf = f64::from(scale_factor);
    let (width, height) = (f64::from(layout.width), f64::from(layout.height));
    let length = |lp: florui_style::LengthPercentage, basis: f64| {
        f64::from(lp.length) * sf + f64::from(lp.percentage) * basis
    };

    let mut list = Matrix4::IDENTITY;
    if let Some((tx, ty, tz)) = style.translate {
        list = list.after(Matrix4::translate(
            length(tx, width),
            length(ty, height),
            f64::from(tz) * sf,
        ));
    }
    if let Some((ax, ay, az, degrees)) = style.rotate {
        list = list.after(Matrix4::rotate(
            f64::from(ax),
            f64::from(ay),
            f64::from(az),
            f64::from(degrees),
        ));
    }
    if let Some((sx, sy, sz)) = style.scale {
        list = list.after(Matrix4::scale(f64::from(sx), f64::from(sy), f64::from(sz)));
    }
    for function in &style.transform {
        let next = match *function {
            TransformFunction::Translate(tx, ty) => {
                Matrix4::translate(length(tx, width), length(ty, height), 0.0)
            }
            TransformFunction::Translate3d(tx, ty, tz) => {
                Matrix4::translate(length(tx, width), length(ty, height), f64::from(tz) * sf)
            }
            TransformFunction::Scale(sx, sy) => Matrix4::scale(f64::from(sx), f64::from(sy), 1.0),
            TransformFunction::Scale3d(sx, sy, sz) => {
                Matrix4::scale(f64::from(sx), f64::from(sy), f64::from(sz))
            }
            TransformFunction::Rotate(degrees) => {
                Matrix4::rotate(0.0, 0.0, 1.0, f64::from(degrees))
            }
            TransformFunction::Rotate3d { x, y, z, degrees } => {
                Matrix4::rotate(f64::from(x), f64::from(y), f64::from(z), f64::from(degrees))
            }
            TransformFunction::Skew(ax, ay) => Matrix4::skew(f64::from(ax), f64::from(ay)),
            TransformFunction::Perspective(d) => Matrix4::perspective(f64::from(d) * sf),
            TransformFunction::Matrix { a, b, c, d, e, f } => Matrix4::from_2d(
                f64::from(a),
                f64::from(b),
                f64::from(c),
                f64::from(d),
                f64::from(e) * sf,
                f64::from(f) * sf,
            ),
            TransformFunction::Matrix3d(m) => {
                // Authored in CSS pixels: lengths grow with the device scale, the
                // per-length perspective terms shrink with it.
                let mut m = m.map(f64::from);
                for translation in &mut m[12..15] {
                    *translation *= sf;
                }
                for perspective in [3, 7, 11] {
                    m[perspective] /= sf;
                }
                Matrix4(m)
            }
        };
        list = list.after(next);
    }

    let (origin_x_lp, origin_y_lp) = style.transform_origin;
    let origin_x = f64::from(x) + length(origin_x_lp, width);
    let origin_y = f64::from(y) + length(origin_y_lp, height);
    let origin_z = f64::from(style.transform_origin_z) * sf;
    Matrix4::translate(origin_x, origin_y, origin_z)
        .after(list)
        .after(Matrix4::translate(-origin_x, -origin_y, -origin_z))
}

/// The matrix `parent`'s `perspective` applies to its children, about its
/// `perspective-origin` in the parent's border box at `(parent_x, parent_y)`;
/// the identity when it has none.
pub fn perspective_matrix(
    parent: &ComputedStyle,
    parent_layout: &BoxLayout,
    parent_x: f32,
    parent_y: f32,
    scale_factor: f32,
) -> Matrix4 {
    let Some(distance) = parent.perspective else {
        return Matrix4::IDENTITY;
    };
    let sf = f64::from(scale_factor);
    let (origin_x_lp, origin_y_lp) = parent.perspective_origin;
    let origin_x = f64::from(parent_x)
        + f64::from(origin_x_lp.length) * sf
        + f64::from(origin_x_lp.percentage) * f64::from(parent_layout.width);
    let origin_y = f64::from(parent_y)
        + f64::from(origin_y_lp.length) * sf
        + f64::from(origin_y_lp.percentage) * f64::from(parent_layout.height);
    Matrix4::translate(origin_x, origin_y, 0.0)
        .after(Matrix4::perspective(f64::from(distance) * sf))
        .after(Matrix4::translate(-origin_x, -origin_y, 0.0))
}

/// `node`'s own matrix as its parent's rendering sees it: the parent's
/// `perspective` (if any) applied after the node's own transforms. `x` and `y`
/// are the node's absolute border-box position, in the caller's pixels.
pub fn resolve_node_transform(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    node: NodeId,
    x: f32,
    y: f32,
    scale_factor: f32,
) -> Matrix4 {
    let Some(style) = styles.get(&node) else {
        return Matrix4::IDENTITY;
    };
    // A node with no transform of its own stays where it is on its own plane,
    // whatever its parent's perspective does to what is off the plane.
    if !style.has_transform() {
        return Matrix4::IDENTITY;
    }
    let Some(layout) = layouts.get(&node) else {
        return Matrix4::IDENTITY;
    };
    let own = resolve_transform(style, layout, x, y, scale_factor);
    let Some(parent) = arena.parent(node) else {
        return own;
    };
    let (Some(parent_style), Some(parent_layout)) = (styles.get(&parent), layouts.get(&parent))
    else {
        return own;
    };
    if parent_style.perspective.is_none() {
        return own;
    }
    perspective_matrix(
        parent_style,
        parent_layout,
        x - layout.x,
        y - layout.y,
        scale_factor,
    )
    .after(own)
}

/// Whether `style`'s `transform-style: preserve-3d` takes effect. CSS forces
/// it back to `flat` for anything that must be flattened to be drawn as a
/// unit: a clip (`overflow`), `opacity` below one, or a `filter`.
pub fn preserves_3d(style: &ComputedStyle) -> bool {
    style.preserve_3d
        && !style.overflow_clips
        && style.opacity >= 1.0
        && style.filter.is_empty()
        && style.backdrop_filter.is_empty()
}

/// Where a node sits while a tree is walked from its roots: the matrix of the
/// plane that holds it (`base`) and, inside a `preserve-3d` context, the
/// matrix accumulated from the context's root down to the node (`context`).
#[derive(Clone, Copy)]
struct Chain {
    base: Matrix4,
    context: Option<Matrix4>,
}

impl Chain {
    const ROOT: Chain = Chain {
        base: Matrix4::IDENTITY,
        context: None,
    };

    /// The node's plane on screen, and the chain its children continue with.
    fn step(self, local: Matrix4, preserves_3d: bool) -> (Matrix4, Chain) {
        let context = self.context.map_or(local, |parent| parent.after(local));
        let screen = self.base.after(context.flatten());
        let next = if preserves_3d {
            Chain {
                base: self.base,
                context: Some(context),
            }
        } else {
            Chain {
                base: screen,
                context: None,
            }
        };
        (screen, next)
    }
}

fn bounds_on_screen(
    screen: &Matrix4,
    x: f32,
    y: f32,
    layout: &BoxLayout,
) -> Option<(f32, f32, f32, f32)> {
    let (x0, y0, x1, y1) = (x, y, x + layout.width, y + layout.height);
    let (bx0, by0, bx1, by1) = if screen.is_plane_identity() {
        (x0, y0, x1, y1)
    } else {
        screen.map_rect_bounds((x0, y0, x1, y1))?
    };
    Some((bx0, by0, bx1 - bx0, by1 - by0))
}

/// Every laid-out node's on-screen bounding box `(x, y, width, height)`: its
/// border box with its own `transform` and every ancestor's applied, the
/// same place painting puts it and hit testing finds it. `layouts` and
/// `scale_factor` are the caller's pixels (logical layouts with `1.0`).
pub fn screen_bounds(
    arena: &Tree,
    layouts: &HashMap<NodeId, BoxLayout>,
    styles: &StyleMap,
    scale_factor: f32,
) -> HashMap<NodeId, (f32, f32, f32, f32)> {
    let mut bounds = HashMap::with_capacity(layouts.len());
    let mut stack: Vec<(NodeId, Chain)> = arena
        .roots()
        .iter()
        .rev()
        .map(|&root| (root, Chain::ROOT))
        .collect();
    while let Some((node, chain)) = stack.pop() {
        let mut next = chain;
        if let Some(&layout) = layouts.get(&node) {
            let (x, y) = absolute_position(arena, layouts, node);
            let local = resolve_node_transform(arena, styles, layouts, node, x, y, scale_factor);
            let preserves = styles.get(&node).is_some_and(preserves_3d);
            let (screen, after) = chain.step(local, preserves);
            next = after;
            if let Some(rect) = bounds_on_screen(&screen, x, y, &layout) {
                bounds.insert(node, rect);
            }
        }
        for &child in arena.children(node).iter().rev() {
            stack.push((child, next));
        }
    }
    bounds
}

/// A box that takes part in a `preserve-3d` context: where it is, the matrix
/// that places it through the context, and whether its children are planes of
/// the same context.
#[derive(Debug, Clone, Copy)]
pub struct ContextPlane {
    pub node: NodeId,
    /// The box's absolute border-box position.
    pub abs: (f32, f32),
    /// Its matrix through the context, in absolute coordinates, in the plane
    /// the context's root is placed on.
    pub matrix: Matrix4,
    pub preserves: bool,
}

/// Every box of the 3D context rooted at `root` (a box that preserves 3D),
/// in painting order, with the matrix that places it. Boxes that draw nothing
/// (`display: none`, fully transparent) and their subtrees are left out.
/// `order` arranges the children of a node that preserves 3D for painting.
pub fn context_planes(
    arena: &Tree,
    styles: &StyleMap,
    layouts: &HashMap<NodeId, BoxLayout>,
    root: NodeId,
    scale_factor: f32,
    order: &dyn Fn(&ComputedStyle, &[NodeId]) -> Vec<NodeId>,
) -> Vec<ContextPlane> {
    let mut found = Vec::new();
    let mut stack: Vec<(NodeId, Option<Matrix4>)> = vec![(root, None)];
    while let Some((node, parent_context)) = stack.pop() {
        let (Some(style), Some(_)) = (styles.get(&node), layouts.get(&node)) else {
            continue;
        };
        if style.display == florui_style::Display::None || style.opacity <= 0.0 {
            continue;
        }
        if crate::is_flowed_inline(arena, styles, node) {
            // Painted by the box whose lines it flows in; only the atomic boxes
            // inside it are planes of their own.
            for child in order(style, arena.children(node)).into_iter().rev() {
                stack.push((child, parent_context));
            }
            continue;
        }
        let abs = absolute_position(arena, layouts, node);
        let local =
            resolve_node_transform(arena, styles, layouts, node, abs.0, abs.1, scale_factor);
        let matrix = parent_context.map_or(local, |parent| parent.after(local));
        let preserves = preserves_3d(style);
        found.push(ContextPlane {
            node,
            abs,
            matrix,
            preserves,
        });
        if preserves {
            for child in order(style, arena.children(node)).into_iter().rev() {
                stack.push((child, Some(matrix)));
            }
        }
    }
    found
}

impl ContextPlane {
    /// Whether the box shows its back, which `backface-visibility: hidden`
    /// hides.
    pub fn is_hidden_back(&self, style: &ComputedStyle, layout: &BoxLayout) -> bool {
        style.backface_hidden
            && self.matrix.faces_away((
                self.abs.0,
                self.abs.1,
                self.abs.0 + layout.width,
                self.abs.1 + layout.height,
            ))
    }
}

/// Whether `node` is a `backface-visibility: hidden` box showing its back, with
/// `local` its own matrix (parent's perspective included) and `abs` its
/// absolute position; it and everything it holds are then not drawn or hit.
pub fn hides_its_back(
    style: &ComputedStyle,
    layout: &BoxLayout,
    local: &Matrix4,
    abs: (f32, f32),
) -> bool {
    style.backface_hidden
        && !local.is_plane_identity()
        && local.faces_away((abs.0, abs.1, abs.0 + layout.width, abs.1 + layout.height))
}

/// One node's [`screen_bounds`] entry, walking down from its root instead of
/// the whole tree; `None` for a node with no layout or wholly behind the eye.
pub fn node_screen_bounds(
    arena: &Tree,
    layouts: &HashMap<NodeId, BoxLayout>,
    styles: &StyleMap,
    node: NodeId,
    scale_factor: f32,
) -> Option<(f32, f32, f32, f32)> {
    layouts.get(&node)?;
    let mut path = vec![node];
    while let Some(parent) = arena.parent(*path.last()?) {
        path.push(parent);
    }
    let mut chain = Chain::ROOT;
    let mut result = None;
    for &step in path.iter().rev() {
        let Some(&layout) = layouts.get(&step) else {
            continue;
        };
        let (x, y) = absolute_position(arena, layouts, step);
        let local = resolve_node_transform(arena, styles, layouts, step, x, y, scale_factor);
        let preserves = styles.get(&step).is_some_and(preserves_3d);
        let (screen, next) = chain.step(local, preserves);
        chain = next;
        result = bounds_on_screen(&screen, x, y, &layout);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    fn rotate_z(degrees: f64) -> Matrix4 {
        Matrix4::rotate(0.0, 0.0, 1.0, degrees)
    }

    #[test]
    fn a_matrix_and_its_inverse_undo_each_other() {
        let m = Matrix4::translate(10.0, -4.0, 0.0)
            .after(rotate_z(37.0))
            .after(Matrix4::scale(2.0, 0.5, 1.0));
        let back = m.map_point((3.0, 8.0)).expect("in front");
        assert!(close(m.unmap_point(back).expect("invertible"), (3.0, 8.0)));
    }

    #[test]
    fn after_applies_the_inner_matrix_first() {
        let m = Matrix4::translate(10.0, 0.0, 0.0).after(Matrix4::scale(2.0, 2.0, 1.0));
        assert!(close(m.map_point((1.0, 1.0)).unwrap(), (12.0, 2.0)));
    }

    #[test]
    fn rotation_is_clockwise_with_y_down() {
        assert!(close(
            rotate_z(90.0).map_point((1.0, 0.0)).unwrap(),
            (0.0, 1.0)
        ));
    }

    #[test]
    fn a_flattening_matrix_has_no_inverse() {
        assert!(
            Matrix4::scale(0.0, 1.0, 1.0)
                .unmap_point((1.0, 1.0))
                .is_none()
        );
    }

    fn assert_matrix(actual: Matrix4, expected: [f64; 16]) {
        for (i, (a, e)) in actual.0.iter().zip(expected).enumerate() {
            assert!(
                (a - e).abs() < 1e-5,
                "entry {i}: {a} vs {e} in {:?}",
                actual.0
            );
        }
    }

    // The matrices below are what Chrome 153 reports for `getComputedStyle`.
    #[test]
    fn three_d_rotation_matches_chromes_matrices() {
        use std::f64::consts::FRAC_1_SQRT_2;
        assert_matrix(
            Matrix4::rotate(0.0, 1.0, 0.0, 35.0),
            [
                0.819152, 0.0, -0.573576, 0.0, 0.0, 1.0, 0.0, 0.0, 0.573576, 0.0, 0.819152, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ],
        );
        assert_matrix(
            Matrix4::rotate(1.0, 0.0, 0.0, 45.0),
            [
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
                FRAC_1_SQRT_2,
                FRAC_1_SQRT_2,
                0.0,
                0.0,
                -FRAC_1_SQRT_2,
                FRAC_1_SQRT_2,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
            ],
        );
        assert_matrix(
            Matrix4::rotate(1.0, 1.0, 0.0, 30.0),
            [
                0.933013, 0.0669873, -0.353553, 0.0, 0.0669873, 0.933013, 0.353553, 0.0, 0.353553,
                -0.353553, 0.866025, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        );
    }

    #[test]
    fn skew_and_perspective_match_chromes_matrices() {
        assert_matrix(
            Matrix4::skew(20.0, 10.0),
            [
                1.0, 0.176327, 0.0, 0.0, 0.36397, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0,
                1.0,
            ],
        );
        assert_matrix(
            Matrix4::perspective(200.0).after(Matrix4::rotate(0.0, 1.0, 0.0, 35.0)),
            [
                0.819152,
                0.0,
                -0.573576,
                0.00286788,
                0.0,
                1.0,
                0.0,
                0.0,
                0.573576,
                0.0,
                0.819152,
                -0.00409576,
                0.0,
                0.0,
                0.0,
                1.0,
            ],
        );
    }

    #[test]
    fn a_zero_axis_is_no_rotation() {
        assert!(Matrix4::rotate(0.0, 0.0, 0.0, 90.0).is_identity());
    }

    #[test]
    fn a_plane_behind_the_eye_is_not_mapped() {
        let m = Matrix4::perspective(200.0).after(Matrix4::translate(0.0, 0.0, 250.0));
        assert_eq!(m.map_point((10.0, 10.0)), None);
        assert!(m.project_quad((0.0, 0.0, 10.0, 10.0)).is_empty());
        let front = Matrix4::perspective(200.0).after(Matrix4::translate(0.0, 0.0, 100.0));
        assert!(close(front.map_point((10.0, 10.0)).unwrap(), (20.0, 20.0)));
    }

    #[test]
    fn a_quad_crossing_the_eye_plane_is_cut() {
        // Rotated about y so one edge comes toward the viewer past the eye.
        let m = Matrix4::translate(100.0, 0.0, 0.0)
            .after(Matrix4::perspective(100.0))
            .after(Matrix4::rotate(0.0, 1.0, 0.0, -80.0));
        let quad = m.project_quad((0.0, 0.0, 400.0, 50.0));
        assert!(quad.len() >= 3);
        assert!(quad.iter().all(|p| p.0.is_finite() && p.1.is_finite()));
    }

    #[test]
    fn backs_are_detected_by_the_normal() {
        let rect = (0.0, 0.0, 100.0, 50.0);
        assert!(!Matrix4::IDENTITY.faces_away(rect));
        assert!(Matrix4::rotate(0.0, 1.0, 0.0, 180.0).faces_away(rect));
        assert!(!Matrix4::rotate(0.0, 1.0, 0.0, 80.0).faces_away(rect));
        assert!(Matrix4::rotate(0.0, 1.0, 0.0, 100.0).faces_away(rect));
        assert!(Matrix4::rotate(1.0, 0.0, 0.0, 180.0).faces_away(rect));
        // A mirror alone is a back too, as in CSS.
        assert!(Matrix4::scale(-1.0, 1.0, 1.0).faces_away(rect));
        // With perspective it is the viewer's side that counts, as in Chrome:
        // a plane turned past edge-on from the origin, still seen from its
        // front by an eye off to the side (measured: a `backface-visibility:
        // hidden` box draws exactly like a visible one there).
        let eye_at = |x: f64| {
            Matrix4::translate(x, 25.0, 0.0)
                .after(Matrix4::perspective(100.0))
                .after(Matrix4::translate(-x, -25.0, 0.0))
        };
        let turned = Matrix4::rotate(0.0, 1.0, 0.0, 100.0);
        assert!(!eye_at(600.0).after(turned).faces_away(rect));
        assert!(eye_at(-500.0).after(turned).faces_away(rect));
        assert!(
            !eye_at(50.0)
                .after(Matrix4::rotate(0.0, 1.0, 0.0, 80.0))
                .faces_away(rect)
        );
    }

    #[test]
    fn flattening_drops_what_lies_off_the_plane() {
        let m = Matrix4::translate(0.0, 0.0, 50.0).after(Matrix4::rotate(0.0, 1.0, 0.0, 45.0));
        let flat = m.flatten();
        assert_eq!(flat.homography(), m.homography());
        let [x, _, z, w] = flat.map_homogeneous((10.0, 0.0, 0.0));
        assert!((x - 10.0 * 45f64.to_radians().cos()).abs() < 1e-9);
        assert_eq!((z, w), (0.0, 1.0));
    }
}
