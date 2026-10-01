//! A memo of shaped inline runs.
//!
//! An inline run (text mixed with inline elements, as in a paragraph with a
//! `<span>`) is shaped from its items and wrapped at a width, and layout asks
//! for the same run at several widths in one pass and again on every frame,
//! then paint shapes it once more. The key is every input exactly (each item's
//! text, size, weight and family or box size, and the wrap width bits), so a
//! hit returns what shaping again would, never an approximation.
//!
//! Kept in two generations bounded by bytes, like the plain-text metrics memo:
//! new answers go into the current one; when it fills it becomes the previous
//! one and the old previous is dropped; a hit in the previous one moves back.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHasher};

use crate::{FontFamily, InlineContent, InlineLayout};

/// Bytes kept in total, across both generations.
pub(crate) const INLINE_MEMO_BYTES: usize = 16 * 1024 * 1024;

/// One item of a stored key, owning its text.
enum Item {
    Text {
        text: Box<str>,
        font_size: u32,
        font_weight: u32,
        family: FontFamily,
    },
    Box {
        id: u64,
        width: u32,
        height: u32,
    },
}

impl Item {
    fn of(item: &InlineContent<'_>) -> Self {
        match *item {
            InlineContent::Text {
                text,
                font_size,
                font_weight,
                family,
            } => Item::Text {
                text: text.into(),
                font_size: font_size.to_bits(),
                font_weight: font_weight.to_bits(),
                family,
            },
            InlineContent::Box { id, width, height } => Item::Box {
                id,
                width: width.to_bits(),
                height: height.to_bits(),
            },
        }
    }

    fn is(&self, other: &InlineContent<'_>) -> bool {
        match (self, other) {
            (
                Item::Text {
                    text,
                    font_size,
                    font_weight,
                    family,
                },
                InlineContent::Text {
                    text: other_text,
                    font_size: other_size,
                    font_weight: other_weight,
                    family: other_family,
                },
            ) => {
                **text == **other_text
                    && *font_size == other_size.to_bits()
                    && *font_weight == other_weight.to_bits()
                    && family == other_family
            }
            (
                Item::Box { id, width, height },
                InlineContent::Box {
                    id: other_id,
                    width: other_width,
                    height: other_height,
                },
            ) => {
                id == other_id
                    && *width == other_width.to_bits()
                    && *height == other_height.to_bits()
            }
            _ => false,
        }
    }
}

struct Entry {
    items: Box<[Item]>,
    width: Option<u32>,
    layout: Arc<InlineLayout>,
}

impl Entry {
    fn matches(&self, items: &[InlineContent<'_>], width: Option<u32>) -> bool {
        self.width == width
            && self.items.len() == items.len()
            && self
                .items
                .iter()
                .zip(items)
                .all(|(stored, asked)| stored.is(asked))
    }
}

#[derive(Default)]
struct Generation {
    /// Entries by the hash of what was asked; a bucket holds the rare
    /// collisions, told apart by comparing the whole key.
    by_hash: FxHashMap<u64, Vec<Entry>>,
    bytes: usize,
}

#[cfg(test)]
thread_local! {
    /// Makes every key hash the same, so a test can check that the whole key,
    /// not the hash, tells two runs apart.
    pub(crate) static COLLIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn hash_of(items: &[InlineContent<'_>], width: Option<u32>) -> u64 {
    #[cfg(test)]
    if COLLIDE.with(std::cell::Cell::get) {
        return 0;
    }
    let mut hasher = FxHasher::default();
    width.hash(&mut hasher);
    for item in items {
        match *item {
            InlineContent::Text {
                text,
                font_size,
                font_weight,
                family,
            } => {
                0u8.hash(&mut hasher);
                text.hash(&mut hasher);
                font_size.to_bits().hash(&mut hasher);
                font_weight.to_bits().hash(&mut hasher);
                (family as u8).hash(&mut hasher);
            }
            InlineContent::Box { id, width, height } => {
                1u8.hash(&mut hasher);
                id.hash(&mut hasher);
                width.to_bits().hash(&mut hasher);
                height.to_bits().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Roughly what an entry holds: the key's text and the shaped glyphs.
fn weight_of(items: &[InlineContent<'_>], layout: &InlineLayout) -> usize {
    let text: usize = items
        .iter()
        .map(|item| match item {
            InlineContent::Text { text, .. } => text.len(),
            InlineContent::Box { .. } => 0,
        })
        .sum();
    let glyphs: usize = layout.runs.iter().map(|run| run.glyphs.len()).sum();
    128 + items.len() * 32 + text + glyphs * 16 + layout.boxes.len() * 16
}

pub(crate) struct InlineMemo {
    current: Generation,
    previous: Generation,
    generation_bytes: usize,
    #[cfg(test)]
    pub(crate) misses: usize,
}

impl InlineMemo {
    pub(crate) fn with_capacity(bytes: usize) -> Self {
        Self {
            current: Generation::default(),
            previous: Generation::default(),
            generation_bytes: (bytes / 2).max(1),
            #[cfg(test)]
            misses: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn entries(&self) -> usize {
        let count =
            |generation: &Generation| generation.by_hash.values().map(Vec::len).sum::<usize>();
        count(&self.current) + count(&self.previous)
    }

    pub(crate) fn get(
        &mut self,
        items: &[InlineContent<'_>],
        width: Option<f32>,
    ) -> Option<Arc<InlineLayout>> {
        let width = width.map(f32::to_bits);
        let hash = hash_of(items, width);
        if let Some(entry) = self
            .current
            .by_hash
            .get(&hash)
            .and_then(|bucket| bucket.iter().find(|e| e.matches(items, width)))
        {
            return Some(Arc::clone(&entry.layout));
        }
        let layout = self
            .previous
            .by_hash
            .get(&hash)
            .and_then(|bucket| bucket.iter().find(|e| e.matches(items, width)))
            .map(|entry| Arc::clone(&entry.layout))?;
        self.store(hash, items, width, Arc::clone(&layout));
        Some(layout)
    }

    pub(crate) fn insert(
        &mut self,
        items: &[InlineContent<'_>],
        width: Option<f32>,
        layout: Arc<InlineLayout>,
    ) {
        let width = width.map(f32::to_bits);
        self.store(hash_of(items, width), items, width, layout);
    }

    fn store(
        &mut self,
        hash: u64,
        items: &[InlineContent<'_>],
        width: Option<u32>,
        layout: Arc<InlineLayout>,
    ) {
        let weight = weight_of(items, &layout);
        if weight > self.generation_bytes {
            // One run bigger than a whole generation would empty the memo for
            // itself; it is shaped again next time instead.
            return;
        }
        if self.current.bytes + weight > self.generation_bytes {
            self.previous = std::mem::take(&mut self.current);
        }
        self.current.bytes += weight;
        self.current.by_hash.entry(hash).or_default().push(Entry {
            items: items.iter().map(Item::of).collect(),
            width,
            layout,
        });
    }

    pub(crate) fn clear(&mut self) {
        self.current = Generation::default();
        self.previous = Generation::default();
    }
}
