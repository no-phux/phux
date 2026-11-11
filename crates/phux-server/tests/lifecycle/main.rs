//! Server, session, and terminal lifecycle integration tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod accessibility_tree;
mod agent_session;
mod agent_session_producer;
mod attach_roles;
mod common;
mod conditional_kill;
mod crash_restart;
mod document_input;
mod document_names;
mod idempotency;
mod keep_empty;
mod kill_visibility;
mod lease_ttl;
mod mirrors;
mod no_terminal_profile;
mod overlay_startup;
mod pixel_surface;
mod plugin_startup;
mod producer_liveness;
mod resource_frames;
mod retain_on_exit;
mod server_lifecycle;
mod spawn_terminal;
mod typed_document;
