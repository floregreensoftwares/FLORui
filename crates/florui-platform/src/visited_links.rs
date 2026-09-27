//! Which `<a href>`s this app process has followed via
//! [`crate::WindowControls::open_url`] — feeds `:link`/`:visited`
//! styling. In-memory only, never written to disk: a browsing-history
//! artifact is more sensitive than anything else this crate persists
//! (compare `window_state.rs`'s own window-geometry-only scope), and
//! there's no existing privacy-review precedent here to lean on for
//! deciding what's safe to write. One instance per app process (like
//! [`crate::clipboard::Clipboard`]), shared across every window it
//! opens via [`Clone`] (cheap — an `Rc` handle to the same set, not a
//! copy) rather than tracked per window.
//!
//! Kept behind this small, deliberately storage-shaped API — not a bare
//! `HashSet` passed around directly — so a future persistent provider is
//! a new backing store behind the same two methods, not a redesign of
//! every caller.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

#[derive(Debug, Clone, Default)]
pub struct VisitedLinks(Rc<RefCell<HashSet<String>>>);

impl VisitedLinks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Only [`crate::WindowControls::open_url`] calls this, and only once
    /// it knows the open actually succeeded — see that method's own doc
    /// for why a cancelled or failed navigation must never reach here.
    pub(crate) fn mark_visited(&self, href: &str) {
        self.0.borrow_mut().insert(href.to_string());
    }

    /// A fresh, owned copy for [`florui_style::InteractionState::with_visited`]
    /// to consume — that type takes its own copy per render rather than
    /// borrowing, so this can't hand out a live view into the `RefCell`.
    pub(crate) fn snapshot(&self) -> HashSet<String> {
        self.0.borrow().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_nothing_visited() {
        let links = VisitedLinks::new();
        assert!(links.snapshot().is_empty());
    }

    #[test]
    fn marking_visited_is_reflected_immediately() {
        let links = VisitedLinks::new();
        links.mark_visited("https://example.com");
        let snapshot = links.snapshot();
        assert!(snapshot.contains("https://example.com"));
        assert!(!snapshot.contains("https://other.example"));
    }

    #[test]
    fn clones_share_the_same_underlying_set() {
        let links = VisitedLinks::new();
        let clone = links.clone();
        links.mark_visited("https://example.com");
        assert!(
            clone.snapshot().contains("https://example.com"),
            "a clone must see writes made through the original -- shared across windows depends on this"
        );
    }

    #[test]
    fn snapshot_reflects_current_state_at_the_time_it_was_taken() {
        let links = VisitedLinks::new();
        links.mark_visited("https://example.com");
        let snapshot = links.snapshot();
        links.mark_visited("https://other.example");
        assert!(snapshot.contains("https://example.com"));
        assert!(
            !snapshot.contains("https://other.example"),
            "a snapshot is a copy, not a live view"
        );
    }
}
