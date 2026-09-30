//! Keyed measurement and scroll-anchor contracts for a future virtualized
//! collection: "design identities and
//! measurement hooks early; deliver after layout, scrolling, and focus
//! are functional." This module is exactly that early design: real,
//! tested types a virtualized list will build on, not a virtualized list
//! itself. Mounting a bounded visible range with overscan, recycling
//! render resources, and the actual scrolling/focus/accessibility policy
//! all remain later work.
//!
//! [`Key`] is reused as-is for item identity: "item identity is a data key, never a recycled row index" is already
//! exactly what [`use_child_scope_keyed`](crate::use_child_scope_keyed)
//! enforces for keyed children; a virtualized list is only a stricter
//! consumer of the same identity, not a reason for a second one.

use std::collections::HashMap;

use crate::Key;

/// One item's extent along a collection's scroll axis: exact once real
/// layout has measured it (e.g. via
/// `florui_platform::use_committed_size`), an estimate before then.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Extent {
    Estimated(f32),
    Measured(f32),
}

impl Extent {
    pub fn value(self) -> f32 {
        match self {
            Extent::Estimated(value) | Extent::Measured(value) => value,
        }
    }

    pub fn is_measured(self) -> bool {
        matches!(self, Extent::Measured(_))
    }
}

/// Per-key extents for a virtualized collection: every key starts at a
/// shared default estimate and is refined in place once something
/// actually measures it — "refine measurements without repeatedly
/// relaying out the entire dataset". A key never
/// measured keeps costing nothing beyond that one estimate lookup.
#[derive(Debug, Clone)]
pub struct KeyedExtents {
    measured: HashMap<Key, f32>,
    default_estimate: f32,
}

impl KeyedExtents {
    /// `default_estimate` is what an unmeasured key reports — typically a
    /// representative row height picked up front ("estimated
    /// extents for unmeasured items").
    pub fn new(default_estimate: f32) -> Self {
        Self {
            measured: HashMap::new(),
            default_estimate,
        }
    }

    /// This key's best currently-known extent: its real measurement if
    /// one has ever been recorded, otherwise the shared estimate.
    pub fn get(&self, key: &Key) -> Extent {
        match self.measured.get(key) {
            Some(&value) => Extent::Measured(value),
            None => Extent::Estimated(self.default_estimate),
        }
    }

    /// Records a real measurement for `key`, replacing whatever estimate
    /// preceded it. Idempotent: measuring the same, unchanged extent
    /// again is a no-op a caller doesn't need to guard against itself.
    pub fn measure(&mut self, key: Key, extent: f32) {
        self.measured.insert(key, extent);
    }

    /// Drops a key's recorded measurement — e.g. once it's evicted from
    /// the dataset and its identity no longer applies to anything, so a
    /// later key that happens to reuse the same value starts from the
    /// estimate again rather than inheriting a stale one.
    pub fn forget(&mut self, key: &Key) {
        self.measured.remove(key);
    }

    /// Total extent across `keys`, in order — measured where known,
    /// estimated otherwise — for a scroll range's size or an item's
    /// offset without a full up-front layout pass over the whole dataset.
    pub fn total(&self, keys: &[Key]) -> f32 {
        keys.iter().map(|key| self.get(key).value()).sum()
    }
}

/// Preserves a visual anchor across a height correction, insertion,
/// removal, or font change: "preserve a
/// keyed scroll anchor and intra-item offset during height corrections."
/// A raw scroll pixel offset alone can't survive any of those, since
/// every extent above the anchor may have just changed — resolving
/// against the *current* keys and extents is what makes it durable.
#[derive(Debug, Clone, PartialEq)]
pub struct ScrollAnchor {
    pub key: Key,
    /// How far into this key's own item the anchor sits — e.g. a
    /// scroll position that stopped mid-item, not exactly at its top.
    pub offset_within_item: f32,
}

impl ScrollAnchor {
    pub fn new(key: Key, offset_within_item: f32) -> Self {
        Self {
            key,
            offset_within_item,
        }
    }

    /// The scroll offset that keeps this anchor's item at the same
    /// viewport position, given `keys` in their current display order
    /// and `extents`'s now-current measurements. `None` when the
    /// anchor's key is no longer present — a deleted anchor
    /// has nothing left to
    /// resolve against; the caller decides the fallback (e.g. the
    /// nearest surviving key), this contract does not guess one.
    pub fn resolve(&self, keys: &[Key], extents: &KeyedExtents) -> Option<f32> {
        let index = keys.iter().position(|key| key == &self.key)?;
        let before: f32 = extents.total(&keys[..index]);
        Some(before + self.offset_within_item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmeasured_key_reports_the_default_estimate() {
        let extents = KeyedExtents::new(40.0);
        assert_eq!(extents.get(&Key::from("a")), Extent::Estimated(40.0));
    }

    #[test]
    fn measuring_a_key_replaces_its_estimate() {
        let mut extents = KeyedExtents::new(40.0);
        extents.measure(Key::from("a"), 72.0);
        assert_eq!(extents.get(&Key::from("a")), Extent::Measured(72.0));
        assert!(extents.get(&Key::from("a")).is_measured());
    }

    #[test]
    fn forgetting_a_key_falls_back_to_the_estimate_again() {
        let mut extents = KeyedExtents::new(40.0);
        extents.measure(Key::from("a"), 72.0);
        extents.forget(&Key::from("a"));
        assert_eq!(extents.get(&Key::from("a")), Extent::Estimated(40.0));
    }

    #[test]
    fn total_mixes_measured_and_estimated_extents() {
        let mut extents = KeyedExtents::new(40.0);
        extents.measure(Key::from("a"), 72.0);
        let keys = vec![Key::from("a"), Key::from("b"), Key::from("c")];

        assert_eq!(extents.total(&keys), 72.0 + 40.0 + 40.0);
    }

    #[test]
    fn scroll_anchor_resolves_to_the_sum_of_extents_before_it_plus_its_own_offset() {
        let mut extents = KeyedExtents::new(40.0);
        extents.measure(Key::from("a"), 72.0);
        extents.measure(Key::from("b"), 50.0);
        let keys = vec![Key::from("a"), Key::from("b"), Key::from("c")];
        let anchor = ScrollAnchor::new(Key::from("c"), 5.0);

        assert_eq!(anchor.resolve(&keys, &extents), Some(72.0 + 50.0 + 5.0));
    }

    #[test]
    fn scroll_anchor_shifts_when_an_item_is_inserted_before_it() {
        let extents = KeyedExtents::new(40.0);
        let anchor = ScrollAnchor::new(Key::from("b"), 0.0);

        let before_insert = vec![Key::from("a"), Key::from("b")];
        assert_eq!(anchor.resolve(&before_insert, &extents), Some(40.0));

        let after_insert = vec![Key::from("new"), Key::from("a"), Key::from("b")];
        assert_eq!(
            anchor.resolve(&after_insert, &extents),
            Some(80.0),
            "an item inserted ahead of the anchor must push its resolved offset down"
        );
    }

    #[test]
    fn scroll_anchor_does_not_resolve_once_its_key_is_deleted() {
        let extents = KeyedExtents::new(40.0);
        let anchor = ScrollAnchor::new(Key::from("gone"), 0.0);
        let keys = vec![Key::from("a"), Key::from("b")];

        assert_eq!(anchor.resolve(&keys, &extents), None);
    }
}
