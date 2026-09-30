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
pub(crate) struct Clipboard(RefCell<Option<arboard::Clipboard>>);

impl Clipboard {
    pub(crate) fn new() -> Self {
        match arboard::Clipboard::new() {
            Ok(clipboard) => Self(RefCell::new(Some(clipboard))),
            Err(error) => {
                eprintln!("florui-platform: system clipboard unavailable: {error}");
                Self(RefCell::new(None))
            }
        }
    }
}

impl ClipboardAccess for Clipboard {
    /// `None` either way (nothing there, or reading failed) is treated as
    /// "nothing to paste," not an error a caller needs to react to.
    fn get_text(&self) -> Option<String> {
        self.0.borrow_mut().as_mut()?.get_text().ok()
    }

    /// A failure (or an unavailable clipboard) is logged, not surfaced: a
    /// keyboard shortcut has no error-reporting UI to report it to.
    fn set_text(&self, text: String) {
        let mut clipboard = self.0.borrow_mut();
        let Some(clipboard) = clipboard.as_mut() else {
            return;
        };
        if let Err(error) = clipboard.set_text(text) {
            eprintln!("florui-platform: could not write to the system clipboard: {error}");
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
