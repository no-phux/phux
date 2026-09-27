//! Heavy stress storms, `#[ignore]`d into `just stress` (post-merge + nightly).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod perf_bursty_output;
mod stress_churn;
mod stress_output_extremes;
mod stress_resize;
mod stress_spawn_kill;
