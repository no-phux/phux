//! Heap accounting: a global allocator that counts, off until asked.
//!
//! A binary opts in with
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: phux_perf::alloc::Counting = phux_perf::alloc::Counting::system();
//! ```
//!
//! and nothing is counted until [`enable`] runs (the binary calls it for
//! `PHUX_DEV=1`, or a `PERF_CONTROL` turns it on). Disabled, an allocation
//! pays one relaxed load. Enabled, it pays three relaxed read-modify-writes on
//! shared words, which contend across threads: a dev-mode cost, not a
//! shipping one.
//!
//! Live bytes are net since the last [`enable`]: blocks allocated before it
//! and freed after it count negative, so the figure is a trend, not an
//! inventory. Peak is the highest net reached since then.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::Relaxed};

use serde::{Deserialize, Serialize};

static ENABLED: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static FREES: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicI64 = AtomicI64::new(0);
static PEAK: AtomicI64 = AtomicI64::new(0);
/// Whether a [`Counting`] allocator is installed in this process, so a
/// report can tell "off" from "not wired".
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Heap figures since accounting was last enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AllocStats {
    /// Allocations (including the allocating half of each realloc).
    pub allocs: u64,
    /// Bytes requested by those allocations.
    pub alloc_bytes: u64,
    /// Deallocations (including the freeing half of each realloc).
    pub frees: u64,
    /// Net bytes allocated and not yet freed; negative when blocks from
    /// before [`enable`] were freed after it.
    pub live_bytes: i64,
    /// Highest `live_bytes` reached.
    pub peak_bytes: i64,
}

impl AllocStats {
    /// Activity since `prev`: counts become interval figures, live and peak
    /// stay current readings.
    #[must_use]
    pub const fn delta(&self, prev: &Self) -> Self {
        Self {
            allocs: self.allocs.saturating_sub(prev.allocs),
            alloc_bytes: self.alloc_bytes.saturating_sub(prev.alloc_bytes),
            frees: self.frees.saturating_sub(prev.frees),
            live_bytes: self.live_bytes,
            peak_bytes: self.peak_bytes,
        }
    }
}

/// Start counting from zero; already counting, keep counting.
pub fn enable() {
    if ENABLED.load(Relaxed) {
        return;
    }
    ALLOCS.store(0, Relaxed);
    ALLOC_BYTES.store(0, Relaxed);
    FREES.store(0, Relaxed);
    LIVE.store(0, Relaxed);
    PEAK.store(0, Relaxed);
    ENABLED.store(true, Relaxed);
}

/// Stop counting. [`stats`] then reports nothing rather than frozen figures
/// a reader would take for current ones.
pub fn disable() {
    ENABLED.store(false, Relaxed);
}

/// Is accounting on?
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

/// Is a [`Counting`] allocator serving this process?
#[must_use]
pub fn installed() -> bool {
    INSTALLED.load(Relaxed)
}

/// Current figures, or `None` when accounting is off or no [`Counting`]
/// allocator is installed.
#[must_use]
pub fn stats() -> Option<AllocStats> {
    if !installed() || !enabled() {
        return None;
    }
    Some(AllocStats {
        allocs: ALLOCS.load(Relaxed),
        alloc_bytes: ALLOC_BYTES.load(Relaxed),
        frees: FREES.load(Relaxed),
        live_bytes: LIVE.load(Relaxed),
        peak_bytes: PEAK.load(Relaxed),
    })
}

fn size_i64(size: usize) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

fn note_alloc(size: usize) {
    // Load first: a store on every allocation would bounce this cache line
    // between cores even with accounting off.
    if !INSTALLED.load(Relaxed) {
        INSTALLED.store(true, Relaxed);
    }
    if !ENABLED.load(Relaxed) {
        return;
    }
    ALLOCS.fetch_add(1, Relaxed);
    ALLOC_BYTES.fetch_add(size as u64, Relaxed);
    let live = LIVE.fetch_add(size_i64(size), Relaxed) + size_i64(size);
    if live > PEAK.load(Relaxed) {
        PEAK.fetch_max(live, Relaxed);
    }
}

fn note_free(size: usize) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    FREES.fetch_add(1, Relaxed);
    LIVE.fetch_sub(size_i64(size), Relaxed);
}

/// A [`GlobalAlloc`] that forwards to `A` and counts while [`enabled`].
#[derive(Debug)]
pub struct Counting<A = System>(A);

impl Counting<System> {
    /// Count on top of the system allocator.
    #[must_use]
    pub const fn system() -> Self {
        Self(System)
    }
}

impl<A> Counting<A> {
    /// Count on top of `inner`.
    #[must_use]
    pub const fn new(inner: A) -> Self {
        Self(inner)
    }
}

// SAFETY: every method forwards the caller's arguments unchanged to the inner
// allocator, which upholds the `GlobalAlloc` contract; the bookkeeping only
// touches atomics and never allocates, so it cannot recurse.
#[allow(
    unsafe_code,
    reason = "implementing GlobalAlloc is unsafe by definition; see the SAFETY note"
)]
unsafe impl<A: GlobalAlloc> GlobalAlloc for Counting<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let ptr = unsafe { self.0.alloc(layout) };
        if !ptr.is_null() {
            note_alloc(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc_zeroed`'s contract.
        let ptr = unsafe { self.0.alloc_zeroed(layout) };
        if !ptr.is_null() {
            note_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds `dealloc`'s contract.
        unsafe { self.0.dealloc(ptr, layout) };
        note_free(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        let new = unsafe { self.0.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            note_free(layout.size());
            note_alloc(new_size);
        }
        new
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_delta_keeps_live_and_peak() {
        let a = AllocStats {
            allocs: 10,
            alloc_bytes: 1000,
            frees: 4,
            live_bytes: 600,
            peak_bytes: 700,
        };
        let b = AllocStats {
            allocs: 15,
            alloc_bytes: 1500,
            frees: 9,
            live_bytes: 500,
            peak_bytes: 900,
        };
        let d = b.delta(&a);
        assert_eq!(d.allocs, 5);
        assert_eq!(d.alloc_bytes, 500);
        assert_eq!(d.frees, 5);
        assert_eq!(d.live_bytes, 500);
        assert_eq!(d.peak_bytes, 900);
    }

    #[test]
    fn counting_allocator_accounts_a_block_while_enabled() {
        // Drive the allocator directly: the test binary's global allocator is
        // the system one, so nothing else lands in these figures except other
        // tests' direct calls, which this crate does not make.
        let counting = Counting::system();
        let layout = Layout::from_size_align(4096, 8).unwrap_or_else(|e| panic!("{e}"));
        enable();
        // SAFETY: a non-zero-size layout; the block is freed with the same one.
        #[allow(unsafe_code, reason = "exercising GlobalAlloc directly")]
        unsafe {
            let ptr = counting.alloc(layout);
            assert!(!ptr.is_null());
            let during = stats().unwrap_or_default();
            assert_eq!(during.allocs, 1);
            assert_eq!(during.live_bytes, 4096);
            let grown = counting.realloc(ptr, layout, 8192);
            assert!(!grown.is_null());
            let grown_layout = Layout::from_size_align(8192, 8).unwrap_or_else(|e| panic!("{e}"));
            counting.dealloc(grown, grown_layout);
        }
        let after = stats().unwrap_or_default();
        disable();
        assert_eq!(after.allocs, 2, "alloc plus the allocating half of realloc");
        assert_eq!(after.frees, 2, "the freeing half of realloc plus dealloc");
        assert_eq!(after.live_bytes, 0);
        assert_eq!(after.peak_bytes, 8192);
        assert!(installed());
        assert_eq!(stats(), None, "off reports nothing, not frozen figures");
    }
}
