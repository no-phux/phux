//! Thin entry point for the `phux` binary.
//!
//! All the real logic lives in `src/lib.rs`'s [`phux::run`]. Kept this
//! small on purpose: see `src/lib.rs`'s module doc ("Why this is a library
//! crate") for why `dhat-heap` heap profiling is NOT wired in here, and
//! `src/bin/dhat_heap.rs` for where it lives instead.

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    phux::run()
}
