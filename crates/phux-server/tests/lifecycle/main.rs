//! Server, session, and terminal lifecycle integration tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod agent_session;
mod agent_session_producer;
mod agent_session_stream_frames;
mod attach_roles;
mod common;
mod conditional_kill;
mod crash_restart;
mod idempotency;
mod keep_empty;
mod kill_visibility;
mod lease_ttl;
mod overlay_startup;
mod plugin_startup;
mod retain_on_exit;
mod server_lifecycle;
mod spawn_terminal;
