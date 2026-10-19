//! The grid container and grid item properties read back from the style
//! engine: track lists with `repeat()`, `minmax()` and `fit-content()`, named
//! lines, `grid-template-areas`, the implicit tracks and the auto-placement
//! flow.

use crate::LengthPercentage;

/// One side of a track's sizing function, or a whole one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridBreadth {
    /// A length. A pure percentage is [`Self::Percent`]; the rest is a
    /// `calc()` mixing both, which layout resolves against the container.
    Length(f32),
    /// A fraction of the container, `1.0` being all of it.
    Percent(f32),
    /// A `calc()` of a length and a percentage together.
    Calc(LengthPercentage),
    /// A share of the free space, in `fr`.
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
}

/// The limit of a `fit-content()` track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridLimit {
    Length(f32),
    Percent(f32),
    Calc(LengthPercentage),
}

/// One track's sizing function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GridTrackSize {
    Breadth(GridBreadth),
    /// `minmax(min, max)`.
    MinMax(GridBreadth, GridBreadth),
    /// `fit-content(limit)`.
    FitContent(GridLimit),
}

impl GridTrackSize {
    pub const AUTO: Self = Self::Breadth(GridBreadth::Auto);

    pub const fn length(px: f32) -> Self {
        Self::Breadth(GridBreadth::Length(px))
    }

    pub const fn fr(share: f32) -> Self {
        Self::Breadth(GridBreadth::Fr(share))
    }
}

/// How many times a `repeat()` repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridRepeatCount {
    Count(u16),
    /// As many as fit, keeping the empty ones.
    AutoFill,
    /// As many as fit, collapsing the empty ones.
    AutoFit,
}

/// A `repeat()` of a track list.
#[derive(Debug, Clone, PartialEq)]
pub struct GridRepeat {
    pub count: GridRepeatCount,
    pub tracks: Vec<GridTrackSize>,
    /// The names of each line the repetition has: empty, or one list for
    /// each track and one more.
    pub line_names: Vec<Vec<String>>,
}

/// One entry of a track list.
#[derive(Debug, Clone, PartialEq)]
pub enum GridTemplateComponent {
    Track(GridTrackSize),
    Repeat(GridRepeat),
}

/// `grid-template-columns` or `grid-template-rows`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridTemplate {
    pub components: Vec<GridTemplateComponent>,
    /// The names of the lines before each component and after the last one,
    /// or empty when no line is named.
    pub line_names: Vec<Vec<String>>,
}

impl GridTemplate {
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// The tracks of a list with no `repeat()`.
    pub fn tracks(&self) -> Vec<GridTrackSize> {
        self.components
            .iter()
            .filter_map(|component| match component {
                GridTemplateComponent::Track(track) => Some(*track),
                GridTemplateComponent::Repeat(_) => None,
            })
            .collect()
    }
}

/// A named area of `grid-template-areas`, in grid lines counted from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridArea {
    pub name: String,
    pub row_start: u16,
    pub row_end: u16,
    pub column_start: u16,
    pub column_end: u16,
}

/// `grid-template-areas`: the named areas and the size of the template, which
/// the unnamed cells (`.`) can make larger than the areas.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridTemplateAreas {
    pub areas: Vec<GridArea>,
    pub rows: u16,
    pub columns: u16,
}

/// One line of a `grid-column`/`grid-row` placement — `grid-*-start`/`-end`
/// each resolve to one of these.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GridPlacement {
    #[default]
    Auto,
    Line(i16),
    Span(u16),
    /// The nth line of that name; an area name stands for the line its area
    /// starts or ends at.
    NamedLine(String, i16),
    /// A span up to the nth line of that name.
    NamedSpan(String, u16),
}

/// `grid-auto-flow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridAutoFlow {
    #[default]
    Row,
    Column,
    RowDense,
    ColumnDense,
}

impl GridBreadth {
    /// A mixed `calc()` worked out against `basis`, the container's size on
    /// that axis, or `auto` when the size is not known (a percentage of it
    /// is `auto` then).
    fn resolve_calc(self, basis: Option<f32>) -> Self {
        match self {
            Self::Calc(calc) => basis.map_or(Self::Auto, |basis| {
                Self::Length(calc.resolve(basis).max(0.0))
            }),
            other => other,
        }
    }

    fn has_calc(self) -> bool {
        matches!(self, Self::Calc(_))
    }
}

impl GridTrackSize {
    /// Whether the track holds a `calc()` of a length and a percentage.
    pub fn has_calc(self) -> bool {
        match self {
            Self::Breadth(breadth) => breadth.has_calc(),
            Self::MinMax(min, max) => min.has_calc() || max.has_calc(),
            Self::FitContent(limit) => matches!(limit, GridLimit::Calc(_)),
        }
    }

    /// The track with each such `calc()` worked out against `basis`.
    pub fn resolve_calc(self, basis: Option<f32>) -> Self {
        match self {
            Self::Breadth(breadth) => Self::Breadth(breadth.resolve_calc(basis)),
            Self::MinMax(min, max) => {
                Self::MinMax(min.resolve_calc(basis), max.resolve_calc(basis))
            }
            Self::FitContent(GridLimit::Calc(calc)) => basis.map_or(Self::AUTO, |basis| {
                Self::FitContent(GridLimit::Length(calc.resolve(basis).max(0.0)))
            }),
            other => other,
        }
    }
}

impl GridTemplate {
    /// Whether any track holds a `calc()` of a length and a percentage.
    pub fn has_calc(&self) -> bool {
        self.components.iter().any(|component| match component {
            GridTemplateComponent::Track(track) => track.has_calc(),
            GridTemplateComponent::Repeat(repeat) => repeat.tracks.iter().any(|t| t.has_calc()),
        })
    }

    /// The template with each such `calc()` worked out against `basis`.
    pub fn resolve_calc(&self, basis: Option<f32>) -> Self {
        let mut resolved = self.clone();
        for component in &mut resolved.components {
            match component {
                GridTemplateComponent::Track(track) => *track = track.resolve_calc(basis),
                GridTemplateComponent::Repeat(repeat) => {
                    for track in &mut repeat.tracks {
                        *track = track.resolve_calc(basis);
                    }
                }
            }
        }
        resolved
    }
}
