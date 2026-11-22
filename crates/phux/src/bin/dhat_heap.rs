//! Heap-profiling entry point for `phux`, gated by the `dhat-heap` feature.
//!
//! A separate binary so dhat's unsilenceable shutdown summary never reaches
//! the `phux` binary integration tests spawn under `--all-features`.
//! Run: `cargo run --features dhat-heap --bin phux-dhat-heap -- server`; a
//! clean shutdown writes `./dhat-heap.json` (view it in `dh_view`).

#![forbid(unsafe_code)]

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> std::process::ExitCode {
    // Must outlive everything else here — its Drop is what flushes
    // `dhat-heap.json`. Bind to `_dhat` (NOT `_`, which would drop
    // immediately) so the guard lives until `main` returns.
    let _dhat = dhat::Profiler::new_heap();
    phux::set_checkout_distros(concat!(env!("CARGO_MANIFEST_DIR"), "/../../distros"));
    phux::run()
}
