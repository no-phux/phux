//! Exercise the exact pinned arena without GPUI's full upstream unit-test
//! crate, whose published archive omits image/font fixtures and dev macros.
//! `desktop-source` materializes this file before the native test gate runs.

#[allow(
    dead_code,
    unsafe_code,
    reason = "test the pinned framework's arena implementation, including its existing unsafe internals"
)]
#[path = "../../toolchain/gpui/gpui-fast/src/arena.rs"]
mod arena;

/// A real-time observation of the five-second grace period, separate from the
/// deterministic unit tests. Run alone so other allocations do not skew it.
#[test]
#[ignore = "native memory observation; run alone with --ignored --nocapture"]
fn measure_transient_arena_memory() {
    use phux_perf::ProcessStats;
    const MIB: usize = 1024 * 1024;
    let before = ProcessStats::physical_footprint_bytes();
    let mut arena = arena::Arena::new(MIB);
    for _ in 0..4096 {
        arena.alloc(|| [0x5a_u8; 4096]);
    }
    let peak_capacity = arena.capacity();
    let loaded = ProcessStats::physical_footprint_bytes();
    let heap_loaded = phux_perf::alloc::native::stats();
    arena.clear();
    arena.alloc(|| [0x5a_u8; 4096]);
    arena.clear();
    std::thread::sleep(std::time::Duration::from_millis(5100));
    arena.alloc(|| [0x5a_u8; 4096]);
    arena.clear();
    let settled = ProcessStats::physical_footprint_bytes();
    let heap_settled = phux_perf::alloc::native::stats();
    let returned = phux_perf::alloc::native::release_unused();
    let relieved = ProcessStats::physical_footprint_bytes();
    assert_eq!(peak_capacity, 16 * MIB);
    assert_eq!(arena.capacity(), 2 * MIB);
    eprintln!(
        "arena memory bytes: baseline={before:?}, loaded={loaded:?}, settled={settled:?}, relieved={relieved:?}, returned={returned:?}, malloc_loaded={heap_loaded:?}, malloc_settled={heap_settled:?}, capacity_before={peak_capacity}, capacity_after={}",
        arena.capacity()
    );
}
