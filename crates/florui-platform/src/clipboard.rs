//! Real OS clipboard access for text-input copy/cut/paste — Ctrl+C/X/V,
//! handled entirely inside [`crate::desktop`]'s own keyboard dispatch, so
//! this is never reachable from component code via `use_context` the way
//! [`crate::window_controls::WindowControls`] is: nothing outside the
//! desktop host's own event handling needs it.
//!
//! One instance per process, not per window — a real OS clipboard is a
//! process-level resource, not scoped to any one window.

use std::cell::RefCell;

/// What text editing needs from a clipboard: the real OS one in the desktop
/// host, [`MemoryClipboard`] anywhere else.
pub trait ClipboardAccess {
    /// The current text, if any; `None` means there is nothing to paste.
    fn get_text(&self) -> Option<String>;
    /// Replaces the contents with `text`.
    fn set_text(&self, text: String);
}

/// A clipboard held in memory, for tests and hosts with no OS clipboard.
#[derive(Default)]
pub struct MemoryClipboard(RefCell<Option<String>>);

impl MemoryClipboard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ClipboardAccess for MemoryClipboard {
    fn get_text(&self) -> Option<String> {
        self.0.borrow().clone()
    }

    fn set_text(&self, text: String) {
        *self.0.borrow_mut() = Some(text);
    }
}

/// Wraps a real `arboard::Clipboard`. Construction can fail (no clipboard
/// service available) — logged once and left unavailable for the rest of
/// the process, the same "log and continue, don't crash" precedent
/// `crate::drag_drop::register`'s own doc already establishes for a
/// similar optional OS capability.
pub(crate) struct Clipboard {
    arboard: RefCell<Result<arboard::Clipboard, String>>,
    /// The "unavailable" notice is logged once, when a clipboard is first
    /// needed and none is there.
    reported: std::cell::Cell<bool>,
}

impl Clipboard {
    pub(crate) fn new() -> Self {
        Self {
            arboard: RefCell::new(arboard::Clipboard::new().map_err(|error| error.to_string())),
            reported: std::cell::Cell::new(false),
        }
    }

    fn unavailable(&self, why: &str) {
        if !self.reported.replace(true) {
            eprintln!("florui-platform: system clipboard unavailable: {why}");
        }
    }
}

impl ClipboardAccess for Clipboard {
    /// `None` either way (nothing there, or reading failed) is treated as
    /// "nothing to paste," not an error a caller needs to react to.
    fn get_text(&self) -> Option<String> {
        // A Wayland session has its own clipboard; the X11 one (through
        // `arboard`) is for X11 sessions and for a compositor without it.
        #[cfg(target_os = "linux")]
        if crate::os::linux::wayland::has_clipboard() {
            return crate::os::linux::wayland::clipboard_text();
        }
        match self.arboard.borrow_mut().as_mut() {
            Ok(clipboard) => clipboard.get_text().ok(),
            Err(why) => {
                self.unavailable(why);
                None
            }
        }
    }

    /// A failure (or an unavailable clipboard) is logged, not surfaced: a
    /// keyboard shortcut has no error-reporting UI to report it to.
    fn set_text(&self, text: String) {
        #[cfg(target_os = "linux")]
        if crate::os::linux::wayland::set_clipboard_text(&text) {
            return;
        }
        match self.arboard.borrow_mut().as_mut() {
            Ok(clipboard) => {
                if let Err(error) = clipboard.set_text(text) {
                    eprintln!("florui-platform: could not write to the system clipboard: {error}");
                }
            }
            Err(why) => self.unavailable(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_in_memory_clipboard_returns_what_was_last_written() {
        let clipboard = MemoryClipboard::new();
        assert_eq!(clipboard.get_text(), None, "nothing to paste at first");
        clipboard.set_text("one".to_string());
        clipboard.set_text("two".to_string());
        assert_eq!(clipboard.get_text(), Some("two".to_string()));
    }
}
