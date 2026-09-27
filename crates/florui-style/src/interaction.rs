//! Which nodes `:hover`/`:focus`/`:focus-visible`/`:active` currently
//! match.
//!
//! This is deliberately not wired to real pointer/keyboard events: no
//! event system exists yet. A caller (today, a test; eventually a real
//! runtime) decides which [`NodeId`]s are in which state and builds this
//! directly — [`crate::cascade::compute`] only consumes it.

use std::collections::HashSet;

use crate::tree::NodeId;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InteractionState {
    hovered: HashSet<NodeId>,
    focused: HashSet<NodeId>,
    active: HashSet<NodeId>,
    focus_visible: HashSet<NodeId>,
    /// Which `<a>` `href`s count as visited for `:link`/`:visited` — keyed
    /// by the href string itself, not a `NodeId`: the same URL linked from
    /// two different nodes is visited or not as one fact, not per-node
    /// state like hover/focus are.
    visited: HashSet<String>,
}

impl InteractionState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_hovered(mut self, id: NodeId) -> Self {
        self.hovered.insert(id);
        self
    }

    pub fn with_focused(mut self, id: NodeId) -> Self {
        self.focused.insert(id);
        self
    }

    pub fn with_active(mut self, id: NodeId) -> Self {
        self.active.insert(id);
        self
    }

    /// A subset of `focused`: whether `:focus-visible` (not just `:focus`)
    /// should match — real CSS shows this only for keyboard-driven focus,
    /// not a mouse click. The caller is responsible for keeping this a
    /// subset; nothing here enforces it.
    pub fn with_focus_visible(mut self, id: NodeId) -> Self {
        self.focus_visible.insert(id);
        self
    }

    pub fn is_hovered(&self, id: NodeId) -> bool {
        self.hovered.contains(&id)
    }

    pub fn is_focused(&self, id: NodeId) -> bool {
        self.focused.contains(&id)
    }

    pub fn is_active(&self, id: NodeId) -> bool {
        self.active.contains(&id)
    }

    pub fn is_focus_visible(&self, id: NodeId) -> bool {
        self.focus_visible.contains(&id)
    }

    /// Replaces the whole visited-href set at once — the caller's own
    /// store already owns the real set (see `florui-platform`'s
    /// `VisitedLinks`); this just copies it in for one render, matching
    /// how every other field here is rebuilt fresh per render too.
    pub fn with_visited(mut self, hrefs: &HashSet<String>) -> Self {
        self.visited = hrefs.clone();
        self
    }

    pub fn is_visited(&self, href: &str) -> bool {
        self.visited.contains(href)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_each_state_independently() {
        let state = InteractionState::new()
            .with_hovered(1)
            .with_focused(2)
            .with_active(3);
        assert!(state.is_hovered(1));
        assert!(!state.is_hovered(2));
        assert!(state.is_focused(2));
        assert!(state.is_active(3));
        assert!(!state.is_active(1));
    }

    #[test]
    fn visited_tracks_by_href_not_node_id() {
        let visited = HashSet::from(["https://example.com".to_string()]);
        let state = InteractionState::new().with_visited(&visited);
        assert!(state.is_visited("https://example.com"));
        assert!(!state.is_visited("https://other.example"));
    }

    #[test]
    fn focus_visible_is_independent_of_focused() {
        let state = InteractionState::new()
            .with_focused(1)
            .with_focus_visible(1);
        assert!(state.is_focused(1));
        assert!(state.is_focus_visible(1));

        let mouse_focused = InteractionState::new().with_focused(2);
        assert!(mouse_focused.is_focused(2));
        assert!(!mouse_focused.is_focus_visible(2));
    }
}
