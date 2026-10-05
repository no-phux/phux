//! What the test runner tells this process about the build, read at run time.
//!
//! `cargo test` exports `CARGO_MANIFEST_DIR` and `CARGO_BIN_EXE_<name>` to the
//! test process; nextest exports `CARGO_MANIFEST_DIR` and
//! `NEXTEST_BIN_EXE_<name>`. Reading them here, rather than through `env!`,
//! keeps checkout-specific paths out of the compiled test, so a
//! content-addressed build cache (mbx) can reuse it from another worktree.

#![allow(dead_code, reason = "not every test binary uses every lookup")]
#![allow(unreachable_pub, reason = "shared by sibling integration-test crates")]
#![allow(
    clippy::expect_used,
    reason = "a test outside a Cargo runner cannot proceed"
)]

use std::path::Path;
use std::sync::OnceLock;

/// Absolute path of the `phux` binary Cargo built for this test run.
pub fn phux_bin() -> &'static str {
    static PHUX: OnceLock<String> = OnceLock::new();
    PHUX.get_or_init(|| {
        ["CARGO_BIN_EXE_phux", "NEXTEST_BIN_EXE_phux"]
            .into_iter()
            .find_map(|key| std::env::var(key).ok())
            .expect("run under `cargo test` or `cargo nextest`, which export the phux binary path")
    })
}

/// This crate's directory (`crates/phux`).
pub fn manifest_dir() -> &'static Path {
    static DIR: OnceLock<String> = OnceLock::new();
    Path::new(DIR.get_or_init(|| {
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("run under `cargo test` or `cargo nextest`, which export CARGO_MANIFEST_DIR")
    }))
}
