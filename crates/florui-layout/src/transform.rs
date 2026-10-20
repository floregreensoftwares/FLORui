//! The matrix a node's transforms resolve to. Painting and hit testing both
//! read it from here, so a node is hit exactly where it is drawn.
//!
//! A node's box is a plane, so a flat (non-`preserve-3d`) node only ever needs
//! the 3x3 homography of its `z = 0` plane ([`Matrix4::homography`]); the full
//! 4x4 matters inside a `preserve-3d` context, where planes keep their depth
//! until the context is flattened into its own plane.

use florui_style::StyleMap;
use std::collections::HashMap;

use florui_style::{ComputedStyle, NodeId, Tree};
pub use florui_style::{Matrix4, NEAR_W};

use crate::{BoxLayout, absolute_position};

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
    // The box `transform-origin` and the percentages of `translate` are of.
    let (x, y, width, height) = match style.transform_box {
        florui_style::TransformBox::View
        | florui_style::TransformBox::Border
        | florui_style::TransformBox::Stroke => (
            f64::from(x),
            f64::from(y),
            f64::from(layout.width),
            f64::from(layout.height),
        ),
        florui_style::TransformBox::Content | florui_style::TransformBox::Fill => {
            let inset = |border: f32, padding: f32| (f64::from(border) + f64::from(padding)) * sf;
            let (top, left) = (
                inset(style.border.top.width, style.padding.top),
                inset(style.border.left.width, style.padding.left),
            );
            let (bottom, right) = (
                inset(style.border.bottom.width, style.padding.bottom),
                inset(style.border.right.width, style.padding.right),
            );
            let (w, h) = (f64::from(layout.width), f64::from(layout.height));
            (
                f64::from(x) + left,
                f64::from(y) + top,
                (w - left - right).max(0.0),
                (h - top - bottom).max(0.0),
            )
        }
    };
    florui_style::transform_matrix(
        style,
        (x, y, width, height),
        style.transform_origin,
        scale_factor,
    )
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
/// unit: a clip (`overflow`), `opacity` below one, a `filter`, `clip-path`,
/// `mask`, a blend mode, or `isolation: isolate`.
pub fn preserves_3d(style: &ComputedStyle) -> bool {
    style.preserve_3d
        && !style.overflow_clips
        && style.opacity >= 1.0
        && !style.isolate
        && !style.has_group_effect()
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
/// in painting order, with the matrix that places it. `display: none` boxes and
/// their subtrees are left out; a fully transparent box stays, since it is still
/// hit (a painter skips it itself).
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
        if style.display == florui_style::Display::None {
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
        // A `display: contents` element has no plane, and its transform nothing to move.
        if crate::is_contents(styles, step) {
            continue;
        }
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
