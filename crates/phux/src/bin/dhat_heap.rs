//! Heap-profiling entry point for `phux`, gated by the `dhat-heap` feature.
//!
//! Deliberately a SEPARATE binary target (`required-features =
//! ["dhat-heap"]` in Cargo.toml) rather than a `#[cfg(feature =
//! "dhat-heap")]` branch inside the `phux` binary's own `main`. See
//! `src/lib.rs`'s module doc ("Why this is a library crate") for the full
//! story: dhat's `Profiler` guard unconditionally `eprintln!`s a shutdown
//! summary with no way to silence it through dhat's public API, so having
//! this allocator swap live inside the `phux` binary meant `cargo test -p
//! phux --all-features` built the exact binary integration tests spawn via
//! `CARGO_BIN_EXE_phux` with a profiler wired in — and every test asserting
//! clean stderr would flake. Keeping it in a separate binary means the
//! `dhat-heap` feature no longer touches the `phux` binary AT ALL, so
//! `--all-features` is safe to combine with `cargo test` regardless of
//! which features are on.
//!
//! Run with: `cargo run --features dhat-heap --bin phux-dhat-heap -- server`
//! (or any other `phux` subcommand in place of `server`). On clean shutdown
//! the profiler guard's Drop writes `./dhat-heap.json`, viewable at
//! <https://nnethercote.github.io/dh_view/dh_view.html>. The instrumented
//! allocator is significantly slower than the system allocator — debug /
//! profiling use only.

#![forbid(unsafe_code)]

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> std::process::ExitCode {
    // Must outlive everything else here — its Drop is what flushes
    // `dhat-heap.json`. Bind to `_dhat` (NOT `_`, which would drop
    // immediately) so the guard lives until `main` returns.
    let _dhat = dhat::Profiler::new_heap();
    phux::run()
}
