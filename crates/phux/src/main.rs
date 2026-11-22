//! Thin entry point for the `phux` binary.
//!
//! All the real logic lives in `src/lib.rs`'s [`phux::run`]. Kept this
//! small on purpose: see `src/lib.rs`'s module doc for why `dhat-heap` heap
//! profiling is NOT wired in here, and `src/bin/dhat_heap.rs` for where it
//! lives instead.

#![forbid(unsafe_code)]

/// Counts heap traffic while accounting is on (`PHUX_DEV=1`, or
/// `phux perf alloc on` against a running server); one relaxed load per
/// allocation otherwise.
#[global_allocator]
static ALLOC: phux_perf::alloc::Counting = phux_perf::alloc::Counting::system();

fn main() -> std::process::ExitCode {
    phux::set_checkout_distros(concat!(env!("CARGO_MANIFEST_DIR"), "/../../distros"));
    phux::run()
}
