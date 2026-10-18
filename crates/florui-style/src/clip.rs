//! `clip-path`: the shape a box and everything in it is cut to.
//!
//! Lengths and percentages stay unresolved: a percentage is of the reference
//! box, whose size only layout knows.

use crate::cascade::LengthPercentage;

/// One coordinate pair, `(x, y)`, from the top left of the reference box.
pub type Point = (LengthPercentage, LengthPercentage);

/// The box a shape is laid out against. `fill-box` is the content box and
/// `stroke-box` and `view-box` the border box, which is what Chrome makes of
/// them for an element with a CSS layout box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReferenceBox {
    Margin,
    #[default]
    Border,
    Padding,
    Content,
}

/// A radius of `circle()` or `ellipse()`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShapeRadius {
    /// A length or percentage; a percentage of a circle's radius is of
    /// `sqrt((width^2 + height^2) / 2)`.
    Length(LengthPercentage),
    /// The distance from the center to the nearest side of the reference box.
    ClosestSide,
    /// The distance from the center to the farthest side.
    FarthestSide,
}

/// One segment of a `path()` or `shape()`, with every point absolute: relative
/// (`by`) points are added to the current point and the smooth curves are
/// written out with their reflected control point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathSegment {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Quad(Point, Point),
    Arc {
        radii: Point,
        /// The rotation of the ellipse's x axis, in degrees.
        rotation: f32,
        large: bool,
        /// Whether the arc is drawn clockwise (positive angle direction).
        clockwise: bool,
        to: Point,
    },
    Close,
}

/// A basic shape.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipShape {
    /// `inset()`, `rect()` and `xywh()`: the distances of the four edges from
    /// the reference box's, and the radius of each corner (`top left`, `top
    /// right`, `bottom right`, `bottom left`) as a horizontal and a vertical
    /// one.
    Inset {
        top: LengthPercentage,
        right: LengthPercentage,
        bottom: LengthPercentage,
        left: LengthPercentage,
        radii: [Point; 4],
    },
    Circle {
        center: Point,
        radius: ShapeRadius,
    },
    Ellipse {
        center: Point,
        radius_x: ShapeRadius,
        radius_y: ShapeRadius,
    },
    Polygon {
        even_odd: bool,
        points: Vec<Point>,
    },
    /// `path()` (its numbers are pixels) and `shape()`.
    Path {
        even_odd: bool,
        segments: Vec<PathSegment>,
    },
}

/// `clip-path`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ClipPath {
    /// `none`, and a `url()` to an SVG `<clipPath>`, which this crate has no
    /// way to define (Chrome too draws an unresolved one unclipped).
    #[default]
    None,
    /// A box alone: its edge, with its corners rounded as the box's own
    /// `border-radius` makes them.
    Box(ReferenceBox),
    /// A shape against a box.
    Shape(Box<ClipShape>, ReferenceBox),
}

/// A command of a `path()` or `shape()` as written: points are as authored,
/// relative (`by`) or absolute (`to`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RawCommand {
    Move {
        abs: bool,
        to: Point,
    },
    Line {
        abs: bool,
        to: Point,
    },
    HLine {
        abs: bool,
        x: LengthPercentage,
    },
    VLine {
        abs: bool,
        y: LengthPercentage,
    },
    Cubic {
        abs: bool,
        to: Point,
        c1: Point,
        c2: Point,
    },
    Quad {
        abs: bool,
        to: Point,
        c1: Point,
    },
    SmoothCubic {
        abs: bool,
        to: Point,
        c2: Point,
    },
    SmoothQuad {
        abs: bool,
        to: Point,
    },
    Arc {
        abs: bool,
        to: Point,
        radii: Point,
        rotation: f32,
        large: bool,
        clockwise: bool,
    },
    Close,
}

fn add(a: LengthPercentage, b: LengthPercentage) -> LengthPercentage {
    LengthPercentage {
        length: a.length + b.length,
        percentage: a.percentage + b.percentage,
    }
}

/// `commands` with every point absolute: a `by` point is added to the current
/// point (the one before the command, for its control points as well) and a
/// smooth curve is written out with the control point reflected from the curve
/// before it.
pub(crate) fn absolutize(commands: &[RawCommand]) -> Vec<PathSegment> {
    let zero = (LengthPercentage::default(), LengthPercentage::default());
    let mut out = Vec::with_capacity(commands.len());
    // The current point, where the subpath began, and the control point a
    // smooth curve reflects (with whether it came from a cubic or a quadratic).
    let (mut current, mut start): (Point, Point) = (zero, zero);
    let mut last_control: Option<(Point, bool)> = None;
    let reflect = |control: Option<(Point, bool)>, want_cubic: bool, at: Point| -> Point {
        match control {
            Some((c, cubic)) if cubic == want_cubic => (
                LengthPercentage {
                    length: 2.0 * at.0.length - c.0.length,
                    percentage: 2.0 * at.0.percentage - c.0.percentage,
                },
                LengthPercentage {
                    length: 2.0 * at.1.length - c.1.length,
                    percentage: 2.0 * at.1.percentage - c.1.percentage,
                },
            ),
            _ => at,
        }
    };
    let place = |abs: bool, p: Point, from: Point| -> Point {
        if abs {
            p
        } else {
            (add(from.0, p.0), add(from.1, p.1))
        }
    };
    for command in commands {
        match *command {
            RawCommand::Move { abs, to } => {
                current = place(abs, to, current);
                start = current;
                last_control = None;
                out.push(PathSegment::Move(current));
            }
            RawCommand::Line { abs, to } => {
                current = place(abs, to, current);
                last_control = None;
                out.push(PathSegment::Line(current));
            }
            RawCommand::HLine { abs, x } => {
                current = (if abs { x } else { add(current.0, x) }, current.1);
                last_control = None;
                out.push(PathSegment::Line(current));
            }
            RawCommand::VLine { abs, y } => {
                current = (current.0, if abs { y } else { add(current.1, y) });
                last_control = None;
                out.push(PathSegment::Line(current));
            }
            RawCommand::Cubic { abs, to, c1, c2 } => {
                let from = current;
                let (c1, c2) = (place(abs, c1, from), place(abs, c2, from));
                current = place(abs, to, from);
                last_control = Some((c2, true));
                out.push(PathSegment::Cubic(c1, c2, current));
            }
            RawCommand::Quad { abs, to, c1 } => {
                let from = current;
                let c1 = place(abs, c1, from);
                current = place(abs, to, from);
                last_control = Some((c1, false));
                out.push(PathSegment::Quad(c1, current));
            }
            RawCommand::SmoothCubic { abs, to, c2 } => {
                let from = current;
                let c1 = reflect(last_control, true, from);
                let c2 = place(abs, c2, from);
                current = place(abs, to, from);
                last_control = Some((c2, true));
                out.push(PathSegment::Cubic(c1, c2, current));
            }
            RawCommand::SmoothQuad { abs, to } => {
                let from = current;
                let c1 = reflect(last_control, false, from);
                current = place(abs, to, from);
                last_control = Some((c1, false));
                out.push(PathSegment::Quad(c1, current));
            }
            RawCommand::Arc {
                abs,
                to,
                radii,
                rotation,
                large,
                clockwise,
            } => {
                current = place(abs, to, current);
                last_control = None;
                out.push(PathSegment::Arc {
                    radii,
                    rotation,
                    large,
                    clockwise,
                    to: current,
                });
            }
            RawCommand::Close => {
                current = start;
                last_control = None;
                out.push(PathSegment::Close);
            }
        }
    }
    out
}
