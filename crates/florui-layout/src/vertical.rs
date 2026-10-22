//! Layout in a vertical writing mode.
//!
//! The Taffy algorithms lay out horizontal boxes only, so the content of a box with a
//! vertical `writing-mode` is laid out in a frame turned a quarter: its lines run along
//! the horizontal axis, and where they and the boxes stack comes out below them. The
//! styles the algorithms read are the physical ones with the axes swapped
//! ([`view_of`]), and what they lay out is turned back to the screen's axes when the
//! boxes are read out ([`rect_to_physical`], [`sides_to_physical`]).
//!
//! In that frame the left edge is the top edge of the screen; the top edge is the right
//! edge in `vertical-rl` and the left edge in `vertical-lr`.

use taffy::{
    Baselines, CollapsibleMarginSet, Layout, LayoutInput, LayoutOutput, Line, Point, Rect,
    RequestedAxis, Size, Style,
};

/// The writing mode of a box: which frame its content is laid out in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Flow {
    #[default]
    Horizontal,
    /// Lines run down, and stack from the right edge towards the left.
    VerticalRl,
    /// Lines run down, and stack from the left edge towards the right.
    VerticalLr,
}

impl Flow {
    pub(crate) fn of(mode: florui_style::WritingMode) -> Self {
        match mode {
            florui_style::WritingMode::HorizontalTb => Flow::Horizontal,
            florui_style::WritingMode::VerticalRl => Flow::VerticalRl,
            florui_style::WritingMode::VerticalLr => Flow::VerticalLr,
        }
    }

    pub(crate) fn is_vertical(self) -> bool {
        self != Flow::Horizontal
    }
}

fn swap<T>(size: Size<T>) -> Size<T> {
    Size {
        width: size.height,
        height: size.width,
    }
}

/// The sides of a physical box as the frame of `flow` sees them.
fn sides<T: Copy>(rect: Rect<T>, flow: Flow) -> Rect<T> {
    let (top, bottom) = if flow == Flow::VerticalRl {
        (rect.right, rect.left)
    } else {
        (rect.left, rect.right)
    };
    Rect {
        left: rect.top,
        right: rect.bottom,
        top,
        bottom,
    }
}

/// `style`, the physical style of a box, as the algorithms of a context in the frame of
/// `flow` have to read it: the axes swapped, and the logical properties (which already
/// follow the flow of the lines) left as they are.
pub(crate) fn view_of(style: &Style, flow: Flow) -> Style {
    let mut view = style.clone();
    view.size = swap(style.size);
    view.min_size = swap(style.min_size);
    view.max_size = swap(style.max_size);
    view.margin = sides(style.margin, flow);
    view.padding = sides(style.padding, flow);
    view.border = sides(style.border, flow);
    view.inset = sides(style.inset, flow);
    view.overflow = Point {
        x: style.overflow.y,
        y: style.overflow.x,
    };
    view.aspect_ratio = style.aspect_ratio.map(|ratio| 1.0 / ratio);
    view
}

/// What `size` is in the other frame.
pub(crate) fn swapped<T>(size: Size<T>) -> Size<T> {
    swap(size)
}

/// What a parent asks of a child, for a child that lays its content out in the other
/// frame. Its margins have nothing to collapse into: a box of another flow is the root
/// of a context of its own.
pub(crate) fn transpose_input(input: LayoutInput) -> LayoutInput {
    LayoutInput {
        axis: match input.axis {
            RequestedAxis::Horizontal => RequestedAxis::Vertical,
            RequestedAxis::Vertical => RequestedAxis::Horizontal,
            RequestedAxis::Both => RequestedAxis::Both,
        },
        known_dimensions: swap(input.known_dimensions),
        known_dimensions_are_definite: swap(input.known_dimensions_are_definite),
        parent_size: swap(input.parent_size),
        available_space: swap(input.available_space),
        vertical_margins_are_collapsible: Line::FALSE,
        ..input
    }
}

/// What a child that laid its content out in the other frame tells its parent, in the
/// parent's frame. It has no baseline the parent could align to, and no margins to
/// collapse through.
pub(crate) fn transpose_output(output: LayoutOutput) -> LayoutOutput {
    LayoutOutput {
        size: swap(output.size),
        scrollable_overflow_rect: Rect {
            left: output.scrollable_overflow_rect.top,
            right: output.scrollable_overflow_rect.bottom,
            top: output.scrollable_overflow_rect.left,
            bottom: output.scrollable_overflow_rect.right,
        },
        baselines: Baselines::NONE,
        top_margin: CollapsibleMarginSet::ZERO,
        bottom_margin: CollapsibleMarginSet::ZERO,
        margins_can_collapse_through: false,
    }
}

/// The rectangle at `(x, y)` of `width` by `height` in the frame of `flow`, in a box
/// whose width on the screen is `container_width`, on the screen: `(x, y, width,
/// height)`.
pub(crate) fn rect_to_physical(
    flow: Flow,
    container_width: f32,
    (x, y, width, height): (f32, f32, f32, f32),
) -> (f32, f32, f32, f32) {
    match flow {
        Flow::Horizontal => (x, y, width, height),
        Flow::VerticalLr => (y, x, height, width),
        Flow::VerticalRl => (container_width - y - height, x, height, width),
    }
}

/// The four widths of a box in the frame of `flow` as they are on the screen's sides.
pub(crate) fn sides_to_physical(rect: Rect<f32>, flow: Flow) -> Rect<f32> {
    match flow {
        Flow::Horizontal => rect,
        Flow::VerticalLr => Rect {
            top: rect.left,
            bottom: rect.right,
            left: rect.top,
            right: rect.bottom,
        },
        Flow::VerticalRl => Rect {
            top: rect.left,
            bottom: rect.right,
            left: rect.bottom,
            right: rect.top,
        },
    }
}

/// `layout`, made in the frame of `flow` inside a box that is `container_width` wide on
/// the screen, as it is on the screen.
pub(crate) fn layout_to_physical(layout: &Layout, flow: Flow, container_width: f32) -> Layout {
    if flow == Flow::Horizontal {
        return *layout;
    }
    let (x, y, width, height) = rect_to_physical(
        flow,
        container_width,
        (
            layout.location.x,
            layout.location.y,
            layout.size.width,
            layout.size.height,
        ),
    );
    Layout {
        location: Point { x, y },
        size: Size { width, height },
        scrollable_overflow_rect: Rect {
            left: layout.scrollable_overflow_rect.top,
            right: layout.scrollable_overflow_rect.bottom,
            top: layout.scrollable_overflow_rect.left,
            bottom: layout.scrollable_overflow_rect.right,
        },
        scrollbar_size: swap(layout.scrollbar_size),
        border: sides_to_physical(layout.border, flow),
        padding: sides_to_physical(layout.padding, flow),
        margin: sides_to_physical(layout.margin, flow),
        ..*layout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::{Dimension, LengthPercentage, LengthPercentageAuto};

    fn styled() -> Style {
        Style {
            size: Size {
                width: Dimension::length(10.0),
                height: Dimension::length(20.0),
            },
            margin: Rect {
                left: LengthPercentageAuto::length(1.0),
                right: LengthPercentageAuto::length(2.0),
                top: LengthPercentageAuto::length(3.0),
                bottom: LengthPercentageAuto::length(4.0),
            },
            padding: Rect {
                left: LengthPercentage::length(5.0),
                right: LengthPercentage::length(6.0),
                top: LengthPercentage::length(7.0),
                bottom: LengthPercentage::length(8.0),
            },
            ..Style::default()
        }
    }

    #[test]
    fn the_size_and_the_sides_of_a_style_are_turned_to_the_frame_of_the_flow() {
        let rl = view_of(&styled(), Flow::VerticalRl);
        assert_eq!(rl.size.width, Dimension::length(20.0));
        assert_eq!(rl.size.height, Dimension::length(10.0));
        // Top of the screen is the left of the frame; the right edge is its top.
        assert_eq!(rl.margin.left, LengthPercentageAuto::length(3.0));
        assert_eq!(rl.margin.right, LengthPercentageAuto::length(4.0));
        assert_eq!(rl.margin.top, LengthPercentageAuto::length(2.0));
        assert_eq!(rl.margin.bottom, LengthPercentageAuto::length(1.0));
        let lr = view_of(&styled(), Flow::VerticalLr);
        assert_eq!(lr.padding.top, LengthPercentage::length(5.0));
        assert_eq!(lr.padding.bottom, LengthPercentage::length(6.0));
    }

    #[test]
    fn a_rectangle_of_the_frame_is_put_back_where_the_screen_has_it() {
        // In a box 100 wide on the screen, a 30x10 rectangle at (5, 20) of the frame.
        let rect = (5.0, 20.0, 30.0, 10.0);
        assert_eq!(
            rect_to_physical(Flow::VerticalLr, 100.0, rect),
            (20.0, 5.0, 10.0, 30.0)
        );
        assert_eq!(
            rect_to_physical(Flow::VerticalRl, 100.0, rect),
            (70.0, 5.0, 10.0, 30.0)
        );
    }
}
