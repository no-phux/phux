//! Terminal behavior over the wire: input routing, output, process facets, and
//! history against a running server. The engine alone is `phux-terminal`'s
//! `engine` suite.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod common;
mod history_cursor_race;
mod input;
mod kip_roundtrip;
mod lagged_consumer_resync;
mod live_output;
mod process_facet;
