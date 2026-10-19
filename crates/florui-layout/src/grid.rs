//! The grid properties of a style, as Taffy takes them.

use florui_style::{
    GridAutoFlow, GridBreadth, GridLimit, GridPlacement as StylePlacement, GridRepeatCount,
    GridTemplate, GridTemplateAreas, GridTemplateComponent as StyleComponent, GridTrackSize,
};
use taffy::{
    GridTemplateArea, GridTemplateComponent, GridTemplateRepetition, MaxTrackSizingFunction,
    MinTrackSizingFunction, RepetitionCount, TrackSizingFunction,
};

pub(crate) fn to_template(template: &GridTemplate) -> Vec<GridTemplateComponent<String>> {
    template
        .components
        .iter()
        .map(|component| match component {
            StyleComponent::Track(track) => GridTemplateComponent::Single(to_track(*track)),
            StyleComponent::Repeat(repeat) => {
                GridTemplateComponent::Repeat(GridTemplateRepetition {
                    count: match repeat.count {
                        GridRepeatCount::Count(count) => RepetitionCount::Count(count),
                        GridRepeatCount::AutoFill => RepetitionCount::AutoFill,
                        GridRepeatCount::AutoFit => RepetitionCount::AutoFit,
                    },
                    tracks: repeat.tracks.iter().copied().map(to_track).collect(),
                    line_names: repeat.line_names.clone(),
                })
            }
        })
        .collect()
}

/// The names of the lines: one list for each component and one more, as Taffy
/// reads them.
///
/// Taffy walks the repeats through this list, so a list whose only names are
/// inside a `repeat()` still needs its (empty) entries.
pub(crate) fn to_line_names(template: &GridTemplate) -> Vec<Vec<String>> {
    let repeats_name_lines = template.components.iter().any(|component| {
        matches!(component, StyleComponent::Repeat(repeat) if !repeat.line_names.is_empty())
    });
    if template.line_names.is_empty() && repeats_name_lines {
        return vec![Vec::new(); template.components.len() + 1];
    }
    template.line_names.clone()
}

pub(crate) fn to_tracks(tracks: &[GridTrackSize]) -> Vec<TrackSizingFunction> {
    tracks.iter().copied().map(to_track).collect()
}

fn to_track(track: GridTrackSize) -> TrackSizingFunction {
    match track {
        GridTrackSize::Breadth(breadth) => TrackSizingFunction {
            min: if matches!(breadth, GridBreadth::Fr(_)) {
                MinTrackSizingFunction::auto()
            } else {
                to_min(breadth)
            },
            max: to_max(breadth),
        },
        GridTrackSize::MinMax(min, max) => TrackSizingFunction {
            min: to_min(min),
            max: to_max(max),
        },
        GridTrackSize::FitContent(limit) => TrackSizingFunction {
            min: MinTrackSizingFunction::auto(),
            max: match limit {
                GridLimit::Length(px) => MaxTrackSizingFunction::fit_content_px(px),
                GridLimit::Percent(fraction) => {
                    MaxTrackSizingFunction::fit_content_percent(fraction)
                }
                GridLimit::Calc(calc) => MaxTrackSizingFunction::fit_content_px(calc.length),
            },
        },
    }
}

fn to_min(breadth: GridBreadth) -> MinTrackSizingFunction {
    match breadth {
        GridBreadth::Length(px) => MinTrackSizingFunction::length(px),
        GridBreadth::Percent(fraction) => MinTrackSizingFunction::percent(fraction),
        GridBreadth::Calc(calc) => MinTrackSizingFunction::length(calc.length),
        GridBreadth::Fr(_) | GridBreadth::Auto => MinTrackSizingFunction::auto(),
        GridBreadth::MinContent => MinTrackSizingFunction::min_content(),
        GridBreadth::MaxContent => MinTrackSizingFunction::max_content(),
    }
}

fn to_max(breadth: GridBreadth) -> MaxTrackSizingFunction {
    match breadth {
        GridBreadth::Length(px) => MaxTrackSizingFunction::length(px),
        GridBreadth::Percent(fraction) => MaxTrackSizingFunction::percent(fraction),
        GridBreadth::Calc(calc) => MaxTrackSizingFunction::length(calc.length),
        GridBreadth::Fr(share) => MaxTrackSizingFunction::fr(share),
        GridBreadth::Auto => MaxTrackSizingFunction::auto(),
        GridBreadth::MinContent => MaxTrackSizingFunction::min_content(),
        GridBreadth::MaxContent => MaxTrackSizingFunction::max_content(),
    }
}

pub(crate) fn to_areas(
    areas: &Option<GridTemplateAreas>,
) -> Option<taffy::GridTemplateAreas<String>> {
    areas.as_ref().map(|areas| taffy::GridTemplateAreas {
        areas: areas
            .areas
            .iter()
            .map(|area| GridTemplateArea {
                name: area.name.clone(),
                row_start: area.row_start,
                row_end: area.row_end,
                column_start: area.column_start,
                column_end: area.column_end,
            })
            .collect(),
        row_count: areas.rows,
        column_count: areas.columns,
    })
}

pub(crate) fn to_auto_flow(flow: GridAutoFlow) -> taffy::GridAutoFlow {
    match flow {
        GridAutoFlow::Row => taffy::GridAutoFlow::Row,
        GridAutoFlow::Column => taffy::GridAutoFlow::Column,
        GridAutoFlow::RowDense => taffy::GridAutoFlow::RowDense,
        GridAutoFlow::ColumnDense => taffy::GridAutoFlow::ColumnDense,
    }
}

pub(crate) fn to_placement_line(
    value: &(StylePlacement, StylePlacement),
) -> taffy::Line<taffy::GridPlacement<String>> {
    taffy::Line {
        start: to_placement(&value.0),
        end: to_placement(&value.1),
    }
}

fn to_placement(value: &StylePlacement) -> taffy::GridPlacement<String> {
    match value {
        StylePlacement::Auto => taffy::GridPlacement::Auto,
        StylePlacement::Line(index) => taffy::style_helpers::line(*index),
        StylePlacement::Span(count) => taffy::GridPlacement::Span(*count),
        StylePlacement::NamedLine(name, index) => {
            taffy::GridPlacement::NamedLine(name.clone(), *index)
        }
        StylePlacement::NamedSpan(name, count) => {
            taffy::GridPlacement::NamedSpan(name.clone(), *count)
        }
    }
}
