//! The context menu on X11: an override-redirect window at the pointer, with
//! the pointer and the keyboard grabbed to it. The window has an alpha channel
//! where the screen has a 32-bit visual, so the corners are round.
//!
//! The menu is driven from the application's event loop, not by a loop of its
//! own: a window manager that gets no answer to its pings while a menu is up
//! calls the application "not responding". [`Session::poll`] handles what the
//! server has sent, and the descriptor of its connection lets a thread wake the
//! loop when there is something.

use std::os::fd::{AsRawFd, RawFd};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    ColormapAlloc, ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, GrabMode, GrabStatus,
    ImageFormat, VisualClass, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::{COPY_DEPTH_FROM_PARENT, CURRENT_TIME, NONE};

use crate::menu::ContextMenuOutcome;
use crate::menu_popup::{MenuPopup, PopupFrame, PopupKey};

/// X keycodes are evdev codes plus eight, on every X server a desktop runs.
fn key_for(keycode: u8) -> Option<PopupKey> {
    match keycode {
        111 => Some(PopupKey::Up),
        116 => Some(PopupKey::Down),
        110 => Some(PopupKey::Home),
        115 => Some(PopupKey::End),
        36 | 104 | 65 => Some(PopupKey::Activate),
        9 => Some(PopupKey::Dismiss),
        _ => None,
    }
}

/// Where a `size` popup goes for a pointer at `at` on a `screen`: at the
/// pointer, moved left or above it where it would run off the screen.
fn place(at: (i32, i32), size: (i32, i32), screen: (i32, i32)) -> (i32, i32) {
    let x = if at.0 + size.0 > screen.0 {
        (screen.0 - size.0).max(0)
    } else {
        at.0
    };
    let y = if at.1 + size.1 > screen.1 {
        (at.1 - size.1).max(0)
    } else {
        at.1
    };
    (x, y)
}

/// A menu that is open: its window, and the connection that owns it.
pub(crate) struct Session {
    conn: RustConnection,
    id: u32,
    gc: u32,
    colormap: Option<u32>,
    depth: u8,
    scale: f64,
    keyboard_grabbed: bool,
}

/// Opens `popup` at the pointer and draws it. `None` when the screen or the
/// grab is not there, in which case nothing is left behind.
pub(crate) fn open(popup: &mut MenuPopup) -> Option<Session> {
    let (conn, screen_number) = RustConnection::connect(None).ok()?;
    let screen = conn.setup().roots.get(screen_number)?;
    let root = screen.root;
    let scale = popup.scale();
    let (width, height) = {
        let (w, h) = popup.size();
        (
            (w * scale as f32).round() as u16,
            (h * scale as f32).round() as u16,
        )
    };
    let pointer = conn.query_pointer(root).ok()?.reply().ok()?;
    let (x, y) = place(
        (i32::from(pointer.root_x), i32::from(pointer.root_y)),
        (i32::from(width), i32::from(height)),
        (
            i32::from(screen.width_in_pixels),
            i32::from(screen.height_in_pixels),
        ),
    );

    // A 32-bit TrueColor visual, for the corners; the default one otherwise.
    let argb = screen
        .allowed_depths
        .iter()
        .filter(|depth| depth.depth == 32)
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| visual.class == VisualClass::TRUE_COLOR && visual.red_mask == 0xff0000);
    let id = conn.generate_id().ok()?;
    let mut attributes = CreateWindowAux::new()
        .override_redirect(1)
        .background_pixel(0)
        .border_pixel(0)
        .event_mask(
            EventMask::EXPOSURE
                | EventMask::POINTER_MOTION
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::LEAVE_WINDOW
                | EventMask::KEY_PRESS,
        );
    let (depth, visual, colormap) = match argb {
        Some(visual) => {
            let colormap = conn.generate_id().ok()?;
            conn.create_colormap(ColormapAlloc::NONE, colormap, root, visual.visual_id)
                .ok()?;
            attributes = attributes.colormap(colormap);
            (32, visual.visual_id, Some(colormap))
        }
        None => (COPY_DEPTH_FROM_PARENT, 0, None),
    };
    conn.create_window(
        depth,
        id,
        root,
        x as i16,
        y as i16,
        width,
        height,
        0,
        WindowClass::INPUT_OUTPUT,
        visual,
        &attributes,
    )
    .ok()?;
    let gc = conn.generate_id().ok()?;
    conn.create_gc(gc, id, &CreateGCAux::new()).ok()?;
    conn.map_window(id).ok()?;
    let mut session = Session {
        conn,
        id,
        gc,
        colormap,
        depth: if depth == COPY_DEPTH_FROM_PARENT {
            24
        } else {
            depth
        },
        scale,
        keyboard_grabbed: false,
    };
    let pointer_grab = session
        .conn
        .grab_pointer(
            false,
            id,
            EventMask::POINTER_MOTION | EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            CURRENT_TIME,
        )
        .ok()
        .and_then(|cookie| cookie.reply().ok());
    session.keyboard_grabbed = session
        .conn
        .grab_keyboard(false, id, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .ok()
        .and_then(|cookie| cookie.reply().ok())
        .is_some();
    // Without the pointer grab a click elsewhere would never reach the menu;
    // dropping the session undoes what was done.
    if pointer_grab.is_none_or(|grab| grab.status != GrabStatus::SUCCESS) {
        return None;
    }
    session.draw(&popup.frame());
    Some(session)
}

impl Session {
    /// The descriptor of the connection to the server, for waiting on.
    pub(crate) fn fd(&self) -> RawFd {
        self.conn.stream().as_raw_fd()
    }

    /// Handles what the server has sent. `Some` once the menu is chosen from
    /// or dismissed.
    pub(crate) fn poll(&mut self, popup: &mut MenuPopup) -> Option<ContextMenuOutcome> {
        let scale = self.scale;
        let logical = |value: i16| (f64::from(value) / scale) as f32;
        let mut changed = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            match event {
                Event::Expose(expose) if expose.count == 0 => changed = true,
                Event::MotionNotify(motion) => {
                    popup.pointer_moved(logical(motion.event_x), logical(motion.event_y));
                    changed = true;
                }
                Event::LeaveNotify(_) => {
                    popup.pointer_left();
                    changed = true;
                }
                Event::ButtonPress(press) => {
                    let at = Some((logical(press.event_x), logical(press.event_y)));
                    if let Some(outcome) = popup.pointer_pressed(at) {
                        return Some(outcome);
                    }
                }
                Event::KeyPress(press) => {
                    if let Some(key) = key_for(press.detail) {
                        if let Some(outcome) = popup.key_pressed(key) {
                            return Some(outcome);
                        }
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if changed {
            self.draw(&popup.frame());
        }
        None
    }

    fn draw(&self, frame: &PopupFrame) {
        let _ = self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.id,
            self.gc,
            frame.width as u16,
            frame.height as u16,
            0,
            0,
            0,
            self.depth,
            &frame.bgra,
        );
        let _ = self.conn.flush();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.conn.ungrab_pointer(CURRENT_TIME);
        if self.keyboard_grabbed {
            let _ = self.conn.ungrab_keyboard(CURRENT_TIME);
        }
        let _ = self.conn.destroy_window(self.id);
        let _ = self.conn.free_gc(self.gc);
        if let Some(colormap) = self.colormap {
            let _ = self.conn.free_colormap(colormap);
        }
        let _ = self.conn.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_a_menu_answers_to() {
        assert_eq!(key_for(111), Some(PopupKey::Up));
        assert_eq!(key_for(116), Some(PopupKey::Down));
        assert_eq!(key_for(36), Some(PopupKey::Activate));
        assert_eq!(key_for(65), Some(PopupKey::Activate));
        assert_eq!(key_for(9), Some(PopupKey::Dismiss));
        assert_eq!(key_for(38), None);
    }

    #[test]
    fn a_menu_stays_on_the_screen() {
        let screen = (1920, 1080);
        assert_eq!(place((100, 100), (200, 300), screen), (100, 100));
        assert_eq!(place((1800, 100), (200, 300), screen), (1720, 100));
        assert_eq!(place((100, 900), (200, 300), screen), (100, 600));
        assert_eq!(place((1900, 1000), (200, 300), screen), (1720, 700));
        assert_eq!(place((10, 10), (4000, 4000), screen), (0, 0));
    }
}
