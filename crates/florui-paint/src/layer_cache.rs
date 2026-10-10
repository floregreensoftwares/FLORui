//! A cache of rendered background layers.
//!
//! A gradient layer's pixels are a pure function of the layer, the scale and
//! where its tiles fall in its own region, and a still gradient is painted
//! again every frame. The key is the exact text of all of those, so a hit is
//! what rendering again would give. Bounded by bytes, least recently used first;
//! a layer larger than a quarter of the budget is not kept.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tiny_skia::Pixmap;

/// Budget for the layers one thread keeps between paints.
const CAPACITY_BYTES: usize = 48 * 1024 * 1024;

struct Entry {
    pixels: Rc<Pixmap>,
    last_used: u64,
}

struct LayerCache {
    entries: HashMap<String, Entry>,
    bytes: usize,
    capacity: usize,
    tick: u64,
    rendered: usize,
}

impl LayerCache {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            capacity,
            tick: 0,
            rendered: 0,
        }
    }

    fn get(&mut self, key: &str) -> Option<Rc<Pixmap>> {
        self.tick += 1;
        let entry = self.entries.get_mut(key)?;
        entry.last_used = self.tick;
        Some(Rc::clone(&entry.pixels))
    }

    #[cfg(test)]
    fn get_or_render(
        &mut self,
        key: String,
        render: impl FnOnce() -> Option<Pixmap>,
    ) -> Option<Rc<Pixmap>> {
        if let Some(hit) = self.get(&key) {
            return Some(hit);
        }
        let pixels = render()?;
        Some(self.insert(key, pixels))
    }

    /// Keeps `pixels` under `key` (unless too big) and hands them back.
    fn insert(&mut self, key: String, pixels: Pixmap) -> Rc<Pixmap> {
        self.rendered += 1;
        let pixels = Rc::new(pixels);
        let size = pixels.data().len();
        if size > self.capacity / 4 {
            return pixels;
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
                self.bytes -= evicted.pixels.data().len();
            }
        }
        self.bytes += size;
        self.entries.insert(
            key,
            Entry {
                pixels: Rc::clone(&pixels),
                last_used: self.tick,
            },
        );
        pixels
    }
}

thread_local! {
    static CACHE: RefCell<LayerCache> = RefCell::new(LayerCache::with_capacity(CAPACITY_BYTES));
}

/// The pixels for `key` from this thread's cache, rendered with `render` if
/// they are not there. `render` runs with the cache free, so it may use it.
pub(crate) fn get_or_render(
    key: String,
    render: impl FnOnce() -> Option<Pixmap>,
) -> Option<Rc<Pixmap>> {
    if let Some(hit) = CACHE.with(|cache| cache.borrow_mut().get(&key)) {
        return Some(hit);
    }
    let pixels = render()?;
    Some(CACHE.with(|cache| cache.borrow_mut().insert(key, pixels)))
}

/// How many layers this thread has rendered (misses), for tests.
#[cfg(test)]
pub(crate) fn rendered_on_this_thread() -> usize {
    CACHE.with(|cache| cache.borrow().rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixmap(side: u32) -> Option<Pixmap> {
        Pixmap::new(side, side)
    }

    #[test]
    fn the_same_key_renders_once() {
        let mut cache = LayerCache::with_capacity(1 << 20);
        let first = cache
            .get_or_render("a".into(), || pixmap(8))
            .expect("a layer");
        let second = cache
            .get_or_render("a".into(), || panic!("a hit must not render"))
            .expect("a layer");
        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(cache.rendered, 1);
        cache.get_or_render("b".into(), || pixmap(8));
        assert_eq!(cache.rendered, 2);
    }

    #[test]
    fn the_least_recently_used_layer_goes_first_when_the_budget_is_full() {
        // Each 6 x 6 layer is 144 bytes; the budget of 600 holds four, and a
        // layer over a quarter of it (150 bytes) is not kept at all.
        let mut cache = LayerCache::with_capacity(600);
        for key in ["a", "b", "c", "d"] {
            cache.get_or_render(key.into(), || pixmap(6));
        }
        // Touch a, then add e: b is the oldest and goes.
        cache.get_or_render("a".into(), || panic!("kept"));
        cache.get_or_render("e".into(), || pixmap(6));
        assert!(cache.entries.contains_key("a") && cache.entries.contains_key("e"));
        assert!(!cache.entries.contains_key("b"));
        assert!(cache.bytes <= 600);
    }

    #[test]
    fn a_layer_over_a_quarter_of_the_budget_is_returned_but_not_kept() {
        let mut cache = LayerCache::with_capacity(600);
        let big = cache
            .get_or_render("big".into(), || pixmap(16))
            .expect("a layer");
        assert_eq!(big.width(), 16);
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn a_failed_render_is_not_kept() {
        let mut cache = LayerCache::with_capacity(1 << 20);
        assert!(cache.get_or_render("x".into(), || None).is_none());
        assert!(cache.entries.is_empty());
    }
}
