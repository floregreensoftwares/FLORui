//! Linux implementations of the platform services. Each module has the same
//! name and entry points as its `windows` counterpart.

pub mod accessibility;
pub mod app_id;
pub mod file_dialog;
pub mod menu;
pub mod open_url;
pub mod overlay;
mod popup_x11;
pub mod single_instance;
pub mod tray;
pub mod wayland;
mod x11;

pub(crate) use wayland::{ShmPresenter, shm_presenter};
pub(crate) use x11::pointer_in_window;

use std::sync::{Arc, OnceLock};

use winit::window::Window;

static DISPLAY_WAKER: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Lets a thread that waits on the display server wake the event loop. Set
/// once, with the first window.
pub(crate) fn display_waker() -> Option<Arc<dyn Fn() + Send + Sync>> {
    DISPLAY_WAKER.get().cloned()
}

/// Gives `window`'s display server what it needs to reach the event loop, and
/// attaches to a Wayland compositor's own objects when there is one.
pub(crate) fn init_display(window: &Window, waker: Arc<dyn Fn() + Send + Sync>) {
    let _ = DISPLAY_WAKER.set(Arc::clone(&waker));
    wayland::init(window, waker);
}
