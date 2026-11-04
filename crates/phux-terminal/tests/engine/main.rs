//! The Terminal engine driven directly, with no server: actors over their
//! channels, input encoding, and snapshot synthesis.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod actor;
mod common;
mod key_encode_snapshot;
mod statesync_convergence;
mod synthesis;
