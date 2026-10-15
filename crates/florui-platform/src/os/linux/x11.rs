//! A per-thread X11 connection and the X11 window id of a `winit` window, for
//! the requests `winit` does not make itself.

use std::cell::RefCell;

use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;
use x11rb::connection::Connection;
use x11rb::rust_connection::RustConnection;

struct Connected {
    conn: RustConnection,
    root: u32,
}

thread_local! {
    static CONNECTION: RefCell<Option<Connected>> = const { RefCell::new(None) };
}

/// Runs `f` with the connection and the root window, connecting on first use.
pub(super) fn with_connection<T>(f: impl FnOnce(&RustConnection, u32) -> Option<T>) -> Option<T> {
    CONNECTION.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = RustConnection::connect(None)
                .ok()
                .and_then(|(conn, screen)| {
                    let root = conn.setup().roots.get(screen)?.root;
                    Some(Connected { conn, root })
                });
        }
        let connected = slot.as_ref()?;
        f(&connected.conn, connected.root)
    })
}

/// `None` on Wayland, where there is no X11 window.
pub(super) fn window_id(window: &Window) -> Option<u32> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(handle) => u32::try_from(handle.window).ok(),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    }
}
