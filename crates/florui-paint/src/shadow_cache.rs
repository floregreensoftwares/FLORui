//! A cache of blurred `box-shadow` coverage masks.
//!
//! A blurred mask is a pure function of its buffer size, the blur's sigma,
//! and the shape's outline relative to the buffer's own origin. Two shadows
//! whose shapes sit at whole-pixel offsets from each other (a grid of equal
//! cards, or the same card repainted next frame) therefore share one mask,
//! and only the cheap per-pixel blend onto the canvas differs. The key is the
//! exact bit pattern of every input, so a hit returns what recomputing would,
//! never an approximation.
//!
//! Bounded by bytes, evicting the least recently used mask first, and a mask
//! larger than a quarter of the budget is not kept at all.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use florui_style::RoundedRect;

/// Budget for the masks one thread keeps between paints.
const CAPACITY_BYTES: usize = 16 * 1024 * 1024;

/// What a worker thread of the pool keeps: every thread has a cache of its own.
const WORKER_CAPACITY_BYTES: usize = 4 * 1024 * 1024;

fn capacity() -> usize {
    if rayon::current_thread_index().is_some() {
        WORKER_CAPACITY_BYTES
    } else {
        CAPACITY_BYTES
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ShadowKey {
    width: u32,
    height: u32,
    sigma: u32,
    shape: [u32; 12],
}

impl ShadowKey {
    pub(crate) fn new(width: u32, height: u32, sigma_px: f32, shape: &RoundedRect) -> Self {
        let mut bits = [0u32; 12];
        let fields = [shape.x, shape.y, shape.width, shape.height];
        for (slot, value) in bits.iter_mut().zip(fields) {
            *slot = value.to_bits();
        }
        for (corner, (h, v)) in shape.radii.iter().enumerate() {
            bits[4 + corner * 2] = h.to_bits();
            bits[5 + corner * 2] = v.to_bits();
        }
        Self {
            width,
            height,
            sigma: sigma_px.to_bits(),
            shape: bits,
        }
    }
}

struct Entry {
    coverage: Arc<Vec<u8>>,
    last_used: u64,
}

pub(crate) struct ShadowCache {
    entries: HashMap<ShadowKey, Entry>,
    bytes: usize,
    capacity: usize,
    tick: u64,
    computed: usize,
}

impl ShadowCache {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            capacity,
            tick: 0,
            computed: 0,
        }
    }

    /// The mask for `key`, computing and keeping it on a miss.
    fn get_or_compute(
        &mut self,
        key: ShadowKey,
        compute: impl FnOnce() -> Vec<u8>,
    ) -> Arc<Vec<u8>> {
        self.tick += 1;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = self.tick;
            florui_profile::count(florui_profile::Counter::ShadowMasksReused, 1);
            return Arc::clone(&entry.coverage);
        }
        florui_profile::count(florui_profile::Counter::ShadowMasksBlurred, 1);
        self.computed += 1;
        let coverage = Arc::new(compute());
        let size = coverage.len();
        if size > self.capacity / 4 {
            return coverage;
        }
        while self.bytes + size > self.capacity {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(evicted) = self.entries.remove(&oldest) {
                let freed = evicted.coverage.len();
                self.bytes -= freed;
                florui_profile::gauge::sub(florui_profile::gauge::Gauge::ShadowCache, freed);
            }
        }
        self.bytes += size;
        florui_profile::gauge::add(florui_profile::gauge::Gauge::ShadowCache, size);
        self.entries.insert(
            key,
            Entry {
                coverage: Arc::clone(&coverage),
                last_used: self.tick,
            },
        );
        coverage
    }
}

impl Drop for ShadowCache {
    /// A thread that ends takes its masks with it.
    fn drop(&mut self) {
        florui_profile::gauge::sub(florui_profile::gauge::Gauge::ShadowCache, self.bytes);
    }
}

thread_local! {
    static CACHE: RefCell<ShadowCache> = RefCell::new(ShadowCache::with_capacity(capacity()));
}

/// The masks the threads painting bands of a frame share: a shadow that crosses
/// several bands is blurred once, by whichever band asks first, not once for each.
struct SharedMasks {
    entries: HashMap<ShadowKey, SharedEntry>,
    bytes: usize,
    tick: u64,
}

struct SharedEntry {
    cell: Arc<OnceLock<Arc<Vec<u8>>>>,
    /// Zero until the mask exists.
    size: usize,
    last_used: u64,
}

/// What the shared masks may hold in all.
const SHARED_CAPACITY_BYTES: usize = 64 * 1024 * 1024;

static SHARED: Mutex<Option<SharedMasks>> = Mutex::new(None);

fn shared_coverage(key: ShadowKey, compute: impl FnOnce() -> Vec<u8>) -> Arc<Vec<u8>> {
    let (cell, fresh) = {
        let mut guard = SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shared = guard.get_or_insert_with(|| SharedMasks {
            entries: HashMap::new(),
            bytes: 0,
            tick: 0,
        });
        shared.tick += 1;
        let tick = shared.tick;
        let fresh = !shared.entries.contains_key(&key);
        let entry = shared
            .entries
            .entry(key.clone())
            .or_insert_with(|| SharedEntry {
                cell: Arc::new(OnceLock::new()),
                size: 0,
                last_used: tick,
            });
        entry.last_used = tick;
        (Arc::clone(&entry.cell), fresh)
    };
    let mut computed_here = false;
    let mask = Arc::clone(cell.get_or_init(|| {
        computed_here = true;
        florui_profile::count(florui_profile::Counter::ShadowMasksBlurred, 1);
        Arc::new(compute())
    }));
    if !computed_here {
        florui_profile::count(florui_profile::Counter::ShadowMasksReused, 1);
    }
    if fresh {
        let mut guard = SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(shared) = guard.as_mut() {
            let size = mask.len();
            if let Some(entry) = shared.entries.get_mut(&key) {
                entry.size = size;
            }
            shared.bytes += size;
            florui_profile::gauge::add(florui_profile::gauge::Gauge::SharedShadows, size);
            while shared.bytes > SHARED_CAPACITY_BYTES {
                let Some(oldest) = shared
                    .entries
                    .iter()
                    .filter(|(_, entry)| entry.size > 0)
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                if let Some(evicted) = shared.entries.remove(&oldest) {
                    shared.bytes -= evicted.size;
                    florui_profile::gauge::sub(
                        florui_profile::gauge::Gauge::SharedShadows,
                        evicted.size,
                    );
                }
            }
        }
    }
    mask
}

/// The mask for `key` from this thread's cache, computed with `compute` if
/// it is not there; on a thread of the pool, from the cache the pool shares.
/// `compute` must not paint a shadow itself.
pub(crate) fn coverage(key: ShadowKey, compute: impl FnOnce() -> Vec<u8>) -> Arc<Vec<u8>> {
    if rayon::current_thread_index().is_some() {
        return shared_coverage(key, compute);
    }
    CACHE.with(|cache| cache.borrow_mut().get_or_compute(key, compute))
}

/// How many masks this thread has computed (misses), for tests.
#[cfg(test)]
pub(crate) fn computed_on_this_thread() -> usize {
    CACHE.with(|cache| cache.borrow().computed)
}

/// Drops every kept mask on this thread, for tests that need a cold start.
#[cfg(test)]
pub(crate) fn clear_on_this_thread() {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.entries.clear();
        florui_profile::gauge::sub(florui_profile::gauge::Gauge::ShadowCache, cache.bytes);
        cache.bytes = 0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u32) -> ShadowKey {
        ShadowKey::new(
            10,
            10,
            2.0,
            &RoundedRect::square(seed as f32, 0.0, 5.0, 5.0),
        )
    }

    #[test]
    fn the_same_key_computes_once_and_shares_the_mask() {
        let mut cache = ShadowCache::with_capacity(1000);
        let first = cache.get_or_compute(key(1), || vec![7; 100]);
        let second = cache.get_or_compute(key(1), || panic!("must not recompute"));
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(cache.computed, 1);
    }

    #[test]
    fn any_differing_input_is_a_different_key() {
        let base = RoundedRect::square(0.0, 0.0, 5.0, 5.0);
        let reference = ShadowKey::new(10, 10, 2.0, &base);
        assert!(reference != ShadowKey::new(11, 10, 2.0, &base));
        assert!(reference != ShadowKey::new(10, 11, 2.0, &base));
        assert!(reference != ShadowKey::new(10, 10, 2.5, &base));
        assert!(reference != ShadowKey::new(10, 10, 2.0, &base.translated(0.25, 0.0)));
        let mut rounded = base;
        rounded.radii[2] = (1.0, 1.0);
        assert!(reference != ShadowKey::new(10, 10, 2.0, &rounded));
        assert!(reference == ShadowKey::new(10, 10, 2.0, &RoundedRect::square(0.0, 0.0, 5.0, 5.0)));
    }

    #[test]
    fn the_least_recently_used_mask_is_evicted_first_and_the_budget_holds() {
        let mut cache = ShadowCache::with_capacity(400);
        cache.get_or_compute(key(1), || vec![0; 100]);
        cache.get_or_compute(key(2), || vec![0; 100]);
        cache.get_or_compute(key(3), || vec![0; 100]);
        cache.get_or_compute(key(1), || panic!("still cached")); // refresh 1
        cache.get_or_compute(key(4), || vec![0; 100]);
        assert!(cache.bytes <= 400);
        // Adding a fifth evicts the oldest, which is now key 2.
        cache.get_or_compute(key(5), || vec![0; 100]);
        assert!(cache.bytes <= 400);
        cache.get_or_compute(key(1), || panic!("1 was used recently and stays"));
        let before = cache.computed;
        cache.get_or_compute(key(2), || vec![0; 100]);
        assert_eq!(cache.computed, before + 1, "2 was evicted and recomputes");
    }

    #[test]
    fn a_mask_over_a_quarter_of_the_budget_is_returned_but_not_kept() {
        let mut cache = ShadowCache::with_capacity(400);
        let big = cache.get_or_compute(key(1), || vec![1; 150]);
        assert_eq!(big.len(), 150);
        assert_eq!(cache.bytes, 0);
        cache.get_or_compute(key(1), || vec![1; 150]);
        assert_eq!(cache.computed, 2, "not kept, so computed again");
    }
}
