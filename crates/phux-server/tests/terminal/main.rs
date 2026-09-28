//! Terminal actors, input encoding, synthesis, and replay integration tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod actor;
mod common;
mod history_cursor_race;
mod input;
mod key_encode_snapshot;
mod kip_roundtrip;
mod lagged_consumer_resync;
mod live_output;
mod process_facet;
mod synthesis;
