//! A memo of shaped inline runs.
//!
//! An inline run (text mixed with inline elements, as in a paragraph with a
//! `<span>`) is shaped from its elements and items and wrapped at a width, and
//! layout asks for the same run at several widths in one pass and again on
//! every frame, then paint shapes it once more. The key is every input exactly
//! (each element's parent and style, each item's text, element and box or
//! spacer size, and the wrap width bits), so a hit returns what shaping again
//! would, never an approximation.
//!
//! Kept in two generations bounded by bytes, like the plain-text metrics memo:
//! new answers go into the current one; when it fills it becomes the previous
//! one and the old previous is dropped; a hit in the previous one moves back.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHasher};

use crate::{InlineElement, InlineItem, InlineLayout, StyleBits};

/// Bytes kept in total, across both generations.
pub(crate) const INLINE_MEMO_BYTES: usize = 16 * 1024 * 1024;

/// One item of a stored key, owning its text.
#[derive(PartialEq)]
enum Item {
    Text {
        text: Box<str>,
        element: usize,
    },
    Spacer {
        element: usize,
        inside: u32,
        margin: u32,
        end: bool,
    },
    Box {
        id: u64,
        width: u32,
        height: u32,
        baseline: u32,
        element: usize,
        vertical_align: [u32; 3],
    },
    Float {
        id: u64,
    },
}

impl Item {
    fn of(item: &InlineItem<'_>) -> Self {
        match *item {
            InlineItem::Text { text, element } => Item::Text {
                text: text.into(),
                element,
            },
            InlineItem::Spacer {
                element,
                inside,
                margin,
                end,
            } => Item::Spacer {
                element,
                inside: inside.to_bits(),
                margin: margin.to_bits(),
                end,
            },
            InlineItem::Box {
                id,
                width,
                height,
                baseline,
                element,
                vertical_align,
            } => Item::Box {
                id,
                width: width.to_bits(),
                height: height.to_bits(),
                baseline: baseline.to_bits(),
                element,
                vertical_align: vertical_align_bits(vertical_align),
            },
            InlineItem::Float { id } => Item::Float { id },
        }
    }

    fn is(&self, other: &InlineItem<'_>) -> bool {
        match (self, other) {
            (
                Item::Text { text, element },
                InlineItem::Text {
                    text: t,
                    element: e,
                },
            ) => **text == **t && element == e,
            _ => *self == Item::of(other),
        }
    }
}

fn vertical_align_bits(value: crate::VerticalAlign) -> [u32; 3] {
    value.bits()
}

/// One element of a stored key.
#[derive(PartialEq)]
struct Element {
    parent: usize,
    style: StyleBits,
}

impl Element {
    fn of(element: &InlineElement) -> Self {
        Self {
            parent: element.parent,
            style: element.style.bits(),
        }
    }
}

struct Entry {
    elements: Box<[Element]>,
    items: Box<[Item]>,
    width: Option<u32>,
    layout: Arc<InlineLayout>,
}

impl Entry {
    fn matches(
        &self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        width: Option<u32>,
    ) -> bool {
        self.width == width
            && self.elements.len() == elements.len()
            && self.items.len() == items.len()
            && self.elements.iter().zip(elements).all(|(stored, asked)| {
                stored.parent == asked.parent && stored.style == asked.style.bits()
            })
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

fn hash_of(elements: &[InlineElement], items: &[InlineItem<'_>], width: Option<u32>) -> u64 {
    #[cfg(test)]
    if COLLIDE.with(std::cell::Cell::get) {
        return 0;
    }
    let mut hasher = FxHasher::default();
    width.hash(&mut hasher);
    for element in elements {
        element.parent.hash(&mut hasher);
        element.style.bits().hash(&mut hasher);
    }
    for item in items {
        match *item {
            InlineItem::Text { text, element } => {
                0u8.hash(&mut hasher);
                text.hash(&mut hasher);
                element.hash(&mut hasher);
            }
            InlineItem::Spacer {
                element,
                inside,
                margin,
                end,
            } => {
                1u8.hash(&mut hasher);
                element.hash(&mut hasher);
                inside.to_bits().hash(&mut hasher);
                margin.to_bits().hash(&mut hasher);
                end.hash(&mut hasher);
            }
            InlineItem::Box {
                id,
                width,
                height,
                baseline,
                element,
                vertical_align,
            } => {
                2u8.hash(&mut hasher);
                id.hash(&mut hasher);
                width.to_bits().hash(&mut hasher);
                height.to_bits().hash(&mut hasher);
                baseline.to_bits().hash(&mut hasher);
                element.hash(&mut hasher);
                vertical_align_bits(vertical_align).hash(&mut hasher);
            }
            InlineItem::Float { id } => {
                3u8.hash(&mut hasher);
                id.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Roughly what an entry holds: the key's text and the shaped glyphs.
fn weight_of(items: &[InlineItem<'_>], elements: &[InlineElement], layout: &InlineLayout) -> usize {
    let text: usize = items
        .iter()
        .map(|item| match item {
            InlineItem::Text { text, .. } => text.len(),
            _ => 0,
        })
        .sum();
    let glyphs: usize = layout.runs.iter().map(|run| run.glyphs.len()).sum();
    128 + items.len() * 40
        + elements.len() * 96
        + text
        + glyphs * 16
        + layout.boxes.len() * 16
        + layout.fragments.len() * 40
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
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        width: Option<f32>,
    ) -> Option<Arc<InlineLayout>> {
        let width = width.map(f32::to_bits);
        let hash = hash_of(elements, items, width);
        if let Some(entry) = self
            .current
            .by_hash
            .get(&hash)
            .and_then(|bucket| bucket.iter().find(|e| e.matches(elements, items, width)))
        {
            return Some(Arc::clone(&entry.layout));
        }
        let layout = self
            .previous
            .by_hash
            .get(&hash)
            .and_then(|bucket| bucket.iter().find(|e| e.matches(elements, items, width)))
            .map(|entry| Arc::clone(&entry.layout))?;
        self.store(hash, elements, items, width, Arc::clone(&layout));
        Some(layout)
    }

    pub(crate) fn insert(
        &mut self,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        width: Option<f32>,
        layout: Arc<InlineLayout>,
    ) {
        let width = width.map(f32::to_bits);
        self.store(
            hash_of(elements, items, width),
            elements,
            items,
            width,
            layout,
        );
    }

    fn store(
        &mut self,
        hash: u64,
        elements: &[InlineElement],
        items: &[InlineItem<'_>],
        width: Option<u32>,
        layout: Arc<InlineLayout>,
    ) {
        let weight = weight_of(items, elements, &layout);
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
            elements: elements.iter().map(Element::of).collect(),
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
