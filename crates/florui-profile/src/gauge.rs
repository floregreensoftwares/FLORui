//! What the framework holds in memory, by owner: one reading per cache or
//! buffer, summed over every thread that has one, with the most it has held at
//! once.
//!
//! A cache calls [`add`] when it takes bytes in and [`sub`] when it lets them go
//! (a cache that lives in a thread-local does so when the thread's copy is
//! dropped too), so the reading is the sum of all its copies. Nothing is kept
//! unless the build can measure ([`crate::ENABLED`]); otherwise a call is an
//! early return.
//!
//! These are the bytes a cache believes it holds, not what the allocator gave
//! it: the allocator's own overhead and a cache's bookkeeping are left out, so
//! the readings tell which owner is large, not the exact size of the process.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The cache or buffer a reading is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gauge {
    /// Rendered background layers, one cache per painting thread.
    LayerCache,
    /// Blurred shadow masks, one cache per painting thread.
    ShadowCache,
    /// Blurred shadow masks shared by every painting thread.
    SharedShadows,
    /// Outlines of glyphs, in path segments, one cache per painting thread.
    GlyphSegments,
    /// Shaped inline runs, one memo per font instance.
    InlineMemo,
    /// Measured plain text, in entries, one memo per font instance.
    TextMetrics,
    /// Pictures downloaded for an `<img>` or a `url(...)` and held decoded, ready to
    /// paint.
    RemoteImages,
    /// Downloads, in encoded bytes, that wait to be decoded or are being decoded.
    DecodeQueue,
    /// Frame buffers the painter keeps between frames and the presenter uploads.
    FrameBuffers,
    /// What the incremental painter remembers of each box to tell what changed,
    /// counting only the size of its records, not what they point to.
    PaintMemo,
}

/// What a reading counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Entries,
}

const COUNT: usize = 10;

impl Gauge {
    pub const ALL: [Gauge; COUNT] = [
        Gauge::LayerCache,
        Gauge::ShadowCache,
        Gauge::SharedShadows,
        Gauge::GlyphSegments,
        Gauge::InlineMemo,
        Gauge::TextMetrics,
        Gauge::RemoteImages,
        Gauge::DecodeQueue,
        Gauge::FrameBuffers,
        Gauge::PaintMemo,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Gauge::LayerCache => "layer-cache",
            Gauge::ShadowCache => "shadow-cache",
            Gauge::SharedShadows => "shared-shadows",
            Gauge::GlyphSegments => "glyph-segments",
            Gauge::InlineMemo => "inline-memo",
            Gauge::TextMetrics => "text-metrics",
            Gauge::RemoteImages => "remote-images",
            Gauge::DecodeQueue => "decode-queue",
            Gauge::FrameBuffers => "frame-buffers",
            Gauge::PaintMemo => "paint-memo",
        }
    }

    pub fn unit(self) -> Unit {
        match self {
            Gauge::GlyphSegments | Gauge::TextMetrics => Unit::Entries,
            _ => Unit::Bytes,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

static CURRENT: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static PEAK: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];

/// `amount` more is held by `gauge`.
pub fn add(gauge: Gauge, amount: usize) {
    if !crate::ENABLED || amount == 0 {
        return;
    }
    let i = gauge.index();
    let now = CURRENT[i].fetch_add(amount, Ordering::Relaxed) + amount;
    PEAK[i].fetch_max(now, Ordering::Relaxed);
}

/// `amount` less is held by `gauge`. Never goes below zero.
pub fn sub(gauge: Gauge, amount: usize) {
    if !crate::ENABLED || amount == 0 {
        return;
    }
    let _ = CURRENT[gauge.index()].fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
        Some(held.saturating_sub(amount))
    });
}

/// The share of a gauge that one owner (a window, a presenter) holds, kept up to
/// date with [`Holder::set`] and given back when it is dropped.
#[derive(Debug)]
pub struct Holder {
    gauge: Gauge,
    held: usize,
}

impl Holder {
    pub fn new(gauge: Gauge) -> Self {
        Self { gauge, held: 0 }
    }

    /// The owner holds `bytes` now.
    pub fn set(&mut self, bytes: usize) {
        if bytes > self.held {
            add(self.gauge, bytes - self.held);
        } else {
            sub(self.gauge, self.held - bytes);
        }
        self.held = bytes;
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        sub(self.gauge, self.held);
    }
}

/// One gauge as it reads now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub gauge: Gauge,
    pub current: usize,
    /// The most it held at once since the process began.
    pub peak: usize,
}

/// Every gauge, in the order of [`Gauge::ALL`].
pub fn read() -> Vec<Reading> {
    Gauge::ALL
        .iter()
        .map(|&gauge| Reading {
            gauge,
            current: CURRENT[gauge.index()].load(Ordering::Relaxed),
            peak: PEAK[gauge.index()].load(Ordering::Relaxed),
        })
        .collect()
}

/// What one gauge reads now.
pub fn current(gauge: Gauge) -> usize {
    CURRENT[gauge.index()].load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The statics are shared by every test of this crate that touches them, so a
    // test uses a gauge of its own and compares against what it read first.
    #[test]
    fn a_gauge_adds_up_and_remembers_its_peak() {
        let before = read()[Gauge::InlineMemo.index()];
        add(Gauge::InlineMemo, 300);
        add(Gauge::InlineMemo, 200);
        sub(Gauge::InlineMemo, 400);
        let after = read()[Gauge::InlineMemo.index()];
        if crate::ENABLED {
            assert_eq!(after.current, before.current + 100);
            assert!(after.peak >= before.current + 500);
        }
    }

    #[test]
    fn a_holder_gives_back_what_it_held() {
        if !crate::ENABLED {
            return;
        }
        let before = current(Gauge::PaintMemo);
        let mut holder = Holder::new(Gauge::PaintMemo);
        holder.set(1_000);
        holder.set(400);
        assert_eq!(current(Gauge::PaintMemo), before + 400);
        drop(holder);
        assert_eq!(current(Gauge::PaintMemo), before);
    }

    #[test]
    fn a_gauge_never_goes_below_zero() {
        sub(Gauge::GlyphSegments, usize::MAX);
        assert_eq!(current(Gauge::GlyphSegments), 0);
    }
}
