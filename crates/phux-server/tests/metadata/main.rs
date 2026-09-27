//! Agent events, metadata replies, journal, and telemetry integration tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]
#![allow(
    clippy::similar_names,
    reason = "tests name clients and their streams alike"
)]

mod agent_asked;
mod agent_events;
mod common;
mod event_journal;
mod get_perf;
mod list_directory;
mod metadata_reply;
mod path_query;
mod whoami;
