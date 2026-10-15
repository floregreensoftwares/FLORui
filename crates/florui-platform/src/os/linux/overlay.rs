//! Click-through on Linux. `Passthrough` is `winit`'s cursor hit test
//! switch (an empty input region on X11 and Wayland). `Selective` needs a
//! per-region input shape, which only X11 has; Wayland reports it
//! unsupported rather than pretending.

use super::x11;
use winit::window::Window;

use crate::window_controls::{InputMode, ScreenRect};

pub(crate) fn set_input_mode(window: &Window, mode: InputMode) -> bool {
    match mode {
        InputMode::Normal => {
            let _ = window.set_cursor_hittest(true);
            if let Some(id) = x11::window_id(window) {
                shape::reset(id);
            }
            true
        }
        InputMode::Passthrough => {
            if let Some(id) = x11::window_id(window) {
                shape::reset(id);
            }
            window.set_cursor_hittest(false).is_ok()
        }
        InputMode::Selective => match x11::window_id(window) {
            Some(id) => {
                let _ = window.set_cursor_hittest(true);
                // Nothing is interactive until the first sync.
                shape::set_rects(id, &[])
            }
            None => false,
        },
    }
}

pub(crate) fn sync_input_regions(window: &Window, regions: &[ScreenRect]) {
    let (Some(id), Ok(origin)) = (x11::window_id(window), window.inner_position()) else {
        return;
    };
    shape::set_rects(id, &relative_rects(regions, origin.x, origin.y));
}

/// `(x, y, width, height)` relative to the window, empty rectangles dropped
/// and sizes clamped to what the protocol can carry.
fn relative_rects(
    regions: &[ScreenRect],
    origin_x: i32,
    origin_y: i32,
) -> Vec<(i16, i16, u16, u16)> {
    regions
        .iter()
        .filter(|r| r.right > r.left && r.bottom > r.top)
        .map(|r| {
            (
                (r.left - origin_x).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (r.top - origin_y).clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                (r.right - r.left).clamp(1, u16::MAX as i32) as u16,
                (r.bottom - r.top).clamp(1, u16::MAX as i32) as u16,
            )
        })
        .collect()
}

mod shape {
    use x11rb::connection::Connection;
    use x11rb::protocol::shape::{ConnectionExt, SK, SO};
    use x11rb::protocol::xproto::{ClipOrdering, Rectangle};

    use super::x11::with_connection;

    /// Makes the whole window receive input again.
    pub(super) fn reset(window: u32) {
        with_connection(|conn, _root| {
            conn.shape_mask(SO::SET, SK::INPUT, window, 0, 0, x11rb::NONE)
                .ok()?;
            conn.flush().ok()
        });
    }

    /// Only these rectangles receive input; an empty list is click-through.
    pub(super) fn set_rects(window: u32, rects: &[(i16, i16, u16, u16)]) -> bool {
        let rects: Vec<Rectangle> = rects
            .iter()
            .map(|&(x, y, width, height)| Rectangle {
                x,
                y,
                width,
                height,
            })
            .collect();
        with_connection(|conn, _root| {
            conn.shape_rectangles(
                SO::SET,
                SK::INPUT,
                ClipOrdering::UNSORTED,
                window,
                0,
                0,
                &rects,
            )
            .ok()?;
            conn.flush().ok()
        })
        .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn regions_become_window_relative() {
        assert_eq!(
            relative_rects(&[rect(110, 220, 150, 260)], 100, 200),
            vec![(10, 20, 40, 40)]
        );
    }

    #[test]
    fn empty_regions_are_dropped() {
        assert!(relative_rects(&[rect(5, 5, 5, 9), rect(5, 5, 9, 5)], 0, 0).is_empty());
    }
}
