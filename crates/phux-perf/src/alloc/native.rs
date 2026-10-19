//! Native allocator accounting, distinct from the opt-in Rust net counter.
//! Capacity includes nonresident pages and is not a physical-memory bill.

use serde::Serialize;

/// An approximate, process-wide snapshot across macOS malloc zones. Includes
/// Rust/System and native framework allocations, but not all VM/GPU mappings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HeapStats {
    /// Bytes in allocated blocks, including allocations made before profiling.
    pub in_use_bytes: u64,
    /// Bytes reserved by malloc, including reusable and possibly nonresident pages.
    pub capacity_bytes: u64,
}

/// Query native malloc zones without enabling allocation counting. Unsupported
/// platforms return `None`; no guessed RSS-minus-heap breakdown is substituted.
#[must_use]
pub fn stats() -> Option<HeapStats> {
    platform::stats()
}

/// Ask the native allocator to return unused pages.
///
/// Never discards live blocks or changes application state. May block on allocator locks: call off the UI
/// thread. Returns the allocator's released-byte count, not an RSS delta.
/// Unsupported platforms return `None`; a supported no-op returns `Some(0)`.
#[must_use]
pub fn release_unused() -> Option<u64> {
    platform::release_unused()
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "native malloc accounting and pressure relief have no safe bindings"
)]
mod platform {
    use super::HeapStats;

    unsafe extern "C" {
        // Public malloc/malloc.h API, available since macOS 10.7.
        fn malloc_zone_pressure_relief(zone: *mut libc::malloc_zone_t, goal: usize) -> usize;
    }

    pub(super) fn stats() -> Option<HeapStats> {
        let mut stats = std::mem::MaybeUninit::<libc::malloc_statistics_t>::zeroed();
        // SAFETY: NULL selects all zones; the output points to aligned writable
        // storage of the documented statistics type. No pointer is retained.
        unsafe { libc::malloc_zone_statistics(std::ptr::null_mut(), stats.as_mut_ptr()) };
        // SAFETY: the synchronous call wrote the output (also zero-initialized).
        let stats = unsafe { stats.assume_init() };
        Some(HeapStats {
            in_use_bytes: u64::try_from(stats.size_in_use).ok()?,
            capacity_bytes: u64::try_from(stats.size_allocated).ok()?,
        })
    }

    pub(super) fn release_unused() -> Option<u64> {
        // SAFETY: NULL examines all zones, zero requests best-effort maximal
        // release. libmalloc owns and synchronizes all memory being reclaimed.
        u64::try_from(unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) }).ok()
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::HeapStats;

    pub(super) const fn stats() -> Option<HeapStats> {
        None
    }
    pub(super) const fn release_unused() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_accounting_and_relief_preserve_live_allocations() {
        let block = std::hint::black_box(vec![0x5a_u8; 8 * 1024 * 1024]);
        if cfg!(target_os = "macos") {
            let stats = stats().expect("native malloc statistics");
            assert!(stats.in_use_bytes >= block.len() as u64);
            assert!(stats.capacity_bytes > 0);
            assert!(release_unused().is_some());
        } else {
            assert_eq!(stats(), None);
            assert_eq!(release_unused(), None);
        }
        assert!(
            block.iter().all(|byte| *byte == 0x5a),
            "relief must not discard live memory"
        );
    }
}
