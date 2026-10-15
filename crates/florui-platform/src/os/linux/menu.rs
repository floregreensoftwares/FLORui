//! The window manager's own window menu (Restore/Move/Size/Close). Wayland
//! has it in `winit`; X11 does not, so the request is sent to the window
//! manager directly (`_GTK_SHOW_WINDOW_MENU`, which mutter and KWin answer).
//! There is no native context menu here: GTK is the only toolkit that draws
//! one for a foreign window, and it cannot anchor to a Wayland surface it
//! does not own.

use std::sync::Mutex;

use winit::dpi::PhysicalPosition;
use winit::window::Window;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};

use super::x11;

/// Where the pointer last was, in window coordinates.
static LAST_CURSOR: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));

pub(crate) fn note_cursor(x: f64, y: f64) {
    if let Ok(mut last) = LAST_CURSOR.lock() {
        *last = (x, y);
    }
}

/// Window-relative position for a screen point. Wayland reports no window
/// origin, and its callers already pass window-relative values.
fn relative(origin: Option<(i32, i32)>, screen_x: i32, screen_y: i32) -> (i32, i32) {
    let (ox, oy) = origin.unwrap_or((0, 0));
    (screen_x - ox, screen_y - oy)
}

fn origin(window: &Window) -> Option<(i32, i32)> {
    window.inner_position().ok().map(|p| (p.x, p.y))
}

pub(crate) fn show_system_menu_at(window: &Window, screen_x: i32, screen_y: i32) {
    if let Some(id) = x11::window_id(window) {
        request_on_x11(id, screen_x, screen_y);
        return;
    }
    let (x, y) = relative(origin(window), screen_x, screen_y);
    window.show_window_menu(PhysicalPosition::new(x, y));
}

pub(crate) fn show_system_menu_at_cursor(window: &Window) {
    let Ok((x, y)) = LAST_CURSOR.lock().map(|last| *last) else {
        return;
    };
    if let Some(id) = x11::window_id(window) {
        let (ox, oy) = origin(window).unwrap_or((0, 0));
        request_on_x11(id, ox + x as i32, oy + y as i32);
        return;
    }
    window.show_window_menu(PhysicalPosition::new(x, y));
}

/// Asks the window manager to show `window`'s menu at a root-window point.
fn request_on_x11(window: u32, root_x: i32, root_y: i32) {
    x11::with_connection(|conn, root| {
        let atom = conn
            .intern_atom(false, b"_GTK_SHOW_WINDOW_MENU")
            .ok()?
            .reply()
            .ok()?
            .atom;
        // Device 0 is the core pointer; the last two words are unused.
        let event =
            ClientMessageEvent::new(32, window, atom, [0, root_x as u32, root_y as u32, 0, 0]);
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )
        .ok()?;
        conn.flush().ok()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screen_point_becomes_window_relative() {
        assert_eq!(relative(Some((100, 50)), 130, 90), (30, 40));
    }

    #[test]
    fn without_a_window_origin_the_point_is_already_relative() {
        assert_eq!(relative(None, 30, 40), (30, 40));
    }
}
