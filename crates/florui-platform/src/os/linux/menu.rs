//! The window manager's own window menu (Restore/Move/Size/Close), and the
//! context menu. Wayland has the window menu in `winit`; X11 does not, so the
//! request is sent to the window manager directly (`_GTK_SHOW_WINDOW_MENU`,
//! which mutter and KWin answer). The context menu is drawn by Florui in a
//! popup surface of its own, because GTK is the only toolkit that draws one
//! for a foreign window and it cannot anchor to a Wayland surface it does not
//! own.

use std::cell::RefCell;
use std::os::fd::RawFd;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use winit::dpi::PhysicalPosition;
use winit::window::{Theme, Window};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ClientMessageEvent, ConnectionExt, EventMask};

use super::{display_waker, popup_x11, wayland, x11};
use crate::menu::{ContextMenuOutcome, MenuEntry};
use crate::menu_popup::MenuPopup;

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

type Callback = Box<dyn Fn(ContextMenuOutcome)>;

enum Surface {
    X11(Box<popup_x11::Session>),
    Wayland(Box<wayland::MenuSession>),
}

/// The menu that is open: what is drawn, what it is drawing, whom to tell.
struct Active {
    surface: Surface,
    popup: MenuPopup,
    callback: Callback,
    /// Wakes the loop for the X11 connection's events; stopped before the
    /// connection is closed.
    _watcher: Option<Watcher>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// A thread that waits for a descriptor and wakes the event loop when there is
/// something to read.
struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    fn new(fd: RawFd) -> Option<Self> {
        let wake = display_waker()?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: `poll` is one valid `pollfd` for the length of the call.
                let ready = unsafe { libc::poll(&mut poll, 1, 50) };
                if ready > 0 && poll.revents & libc::POLLIN != 0 {
                    wake();
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });
        Some(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens the menu at the pointer and returns; what is chosen, or that it was
/// dismissed, is given to `callback` from the event loop (see [`pump_menu`]).
/// While a menu is open another is dismissed at once.
pub(crate) fn show_context_menu_with(window: &Window, items: &[MenuEntry], callback: Callback) {
    if ACTIVE.with(|active| active.borrow().is_some()) {
        callback(ContextMenuOutcome::Dismissed);
        return;
    }
    let on_x11 = x11::window_id(window).is_some();
    // A Wayland buffer has a whole-number scale.
    let scale = if on_x11 {
        window.scale_factor()
    } else {
        window.scale_factor().ceil().max(1.0)
    };
    let dark = window.theme() == Some(Theme::Dark);
    let Some(mut popup) = MenuPopup::new(items, scale, dark) else {
        callback(ContextMenuOutcome::Dismissed);
        return;
    };
    let opened = if on_x11 {
        popup_x11::open(&mut popup).map(|session| Surface::X11(Box::new(session)))
    } else {
        // The pointer's last position over the window, in logical pixels.
        let at = LAST_CURSOR.lock().map(|last| *last).unwrap_or((0.0, 0.0));
        let factor = window.scale_factor();
        wayland::open_menu(
            window,
            &mut popup,
            ((at.0 / factor) as f32, (at.1 / factor) as f32),
        )
        .map(|session| Surface::Wayland(Box::new(session)))
    };
    let Some(surface) = opened else {
        callback(ContextMenuOutcome::Unavailable);
        return;
    };
    let watcher = match &surface {
        Surface::X11(session) => Watcher::new(session.fd()),
        Surface::Wayland(_) => None,
    };
    ACTIVE.with(|active| {
        *active.borrow_mut() = Some(Active {
            surface,
            popup,
            callback,
            _watcher: watcher,
        });
    });
}

/// Applies what the open menu has been sent. When it is chosen from or
/// dismissed it is closed, and the callback it was opened with is returned with
/// the outcome, for the caller to run once nothing is borrowed.
pub(crate) fn pump_menu() -> Option<(Callback, ContextMenuOutcome)> {
    ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let active = slot.as_mut()?;
        let outcome = match &mut active.surface {
            Surface::X11(session) => session.poll(&mut active.popup),
            Surface::Wayland(session) => wayland::advance_menu(session, &mut active.popup),
        }?;
        let done = slot.take()?;
        Some((done.callback, outcome))
    })
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
