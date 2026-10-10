//! The layers of a node's `background-image`, in the shape paint reads them:
//! the image or gradient, and how it is sized, placed, repeated and clipped.
//! Everything that needs the node's box (a percentage, a corner direction, a
//! `closest-side` radius) is left unresolved here and settled by paint.

use crate::{LengthPercentage, Rgba};

/// One color stop or interpolation hint of a linear or radial gradient.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GradientItem {
    /// A color, at a position along the gradient line when it has one.
    Stop {
        color: Rgba,
        position: Option<LengthPercentage>,
    },
    /// Where the transition between the two stops around it is halfway.
    Hint(LengthPercentage),
}

/// One color stop or hint of a conic gradient; positions are turns, so a
/// quarter turn is `0.25`, whether written as an angle or a percentage.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConicItem {
    Stop { color: Rgba, position: Option<f32> },
    Hint(f32),
}

/// Where a linear gradient's line points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LinearDirection {
    /// Degrees clockwise from "to top"; `to right` is 90.
    Angle(f32),
    /// `to <corner>`: the line is perpendicular to the diagonal of the other
    /// two corners, so its angle depends on the box.
    Corner { right: bool, bottom: bool },
}

/// How far a radial gradient reaches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RadialSize {
    /// A circle of this radius in pixels.
    Circle(f32),
    /// An ellipse with these radii; percentages are of the gradient box.
    Ellipse(LengthPercentage, LengthPercentage),
    /// A circle or ellipse sized by where the center sits in the box.
    Extent { circle: bool, extent: RadialExtent },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadialExtent {
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LinearGradient {
    pub direction: LinearDirection,
    pub items: Vec<GradientItem>,
    pub repeating: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadialGradient {
    pub size: RadialSize,
    /// The center, as a position in the gradient box.
    pub center: (LengthPercentage, LengthPercentage),
    pub items: Vec<GradientItem>,
    pub repeating: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConicGradient {
    /// Degrees clockwise from the top at which the first stop sits.
    pub from: f32,
    pub center: (LengthPercentage, LengthPercentage),
    pub items: Vec<ConicItem>,
    pub repeating: bool,
}

/// What a layer paints.
#[derive(Debug, Clone, PartialEq)]
pub enum BackgroundImage {
    Linear(LinearGradient),
    Radial(RadialGradient),
    Conic(ConicGradient),
}

/// `background-size` for one layer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundSize {
    /// `<width> <height>`; `None` is `auto`.
    Explicit(Option<LengthPercentage>, Option<LengthPercentage>),
    Cover,
    Contain,
}

/// One axis of `background-repeat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundRepeat {
    Repeat,
    Space,
    Round,
    NoRepeat,
}

/// The box `background-origin` positions against or `background-clip` cuts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundBox {
    Border,
    Padding,
    Content,
}

/// One layer of `background-image`, with the properties that go with it.
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundLayer {
    pub image: BackgroundImage,
    pub size: BackgroundSize,
    /// `background-position`: the offset from the origin box's start, each
    /// axis a length plus a fraction of the free space (box minus image).
    pub position: (LengthPercentage, LengthPercentage),
    pub repeat: (BackgroundRepeat, BackgroundRepeat),
    pub origin: BackgroundBox,
    pub clip: BackgroundBox,
}
