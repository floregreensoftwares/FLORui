//! A counting allocator for the heap measurements. Counting is off unless a
//! run asks for it, so the timing runs pay one relaxed load per allocation
//! and nothing else; heap numbers come from their own runs.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

pub struct CountingAllocator;

/// Starts counting. Memory allocated before this call is not tracked, so a
/// run enables it before it builds anything.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    /// Bytes live now.
    pub current: usize,
    /// The most bytes live at once since counting began.
    pub peak: usize,
    /// Allocations made, and the bytes they asked for, since counting began.
    pub allocations: usize,
    pub bytes: usize,
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        current: CURRENT.load(Ordering::Relaxed),
        peak: PEAK.load(Ordering::Relaxed),
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        bytes: BYTES.load(Ordering::Relaxed),
    }
}

fn grew(size: usize) {
    let now = CURRENT.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(now, Ordering::Relaxed);
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    BYTES.fetch_add(size, Ordering::Relaxed);
}

fn shrank(size: usize) {
    // Memory freed that was allocated before counting began must not wrap.
    let _ = CURRENT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
        Some(c.saturating_sub(size))
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() && ENABLED.load(Ordering::Relaxed) {
            grew(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        if ENABLED.load(Ordering::Relaxed) {
            shrank(layout.size());
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, new_size) };
        if !resized.is_null() && ENABLED.load(Ordering::Relaxed) {
            shrank(layout.size());
            grew(new_size);
        }
        resized
    }
}
