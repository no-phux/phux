//! Server-side connections: accept, authenticate, dispatch, and publish replies.
//!
//! This is the daemon's handling of connected peers. Client dialing, reconnect
//! policy, and UI orchestration belong to `phux-client-runtime`.
//! Each private module owns its implementation and imports its dependencies;
//! only the server operations below are shared with the rest of the runtime.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private implementation modules re-export crate-scoped operations through this facade; pub would violate unreachable_pub"
)]

mod commands;
pub(crate) mod dispatch;
pub(in crate::runtime) mod frames;
pub(crate) mod hello;
mod lifecycle;
mod metadata_mutation;
mod metadata_read;
pub(in crate::runtime) mod metadata_subscriptions;
pub(crate) mod plumbing;
mod session_create;
mod unsupported;
mod writer;

pub mod pane_events;
pub mod pane_exit;
pub mod socket;

pub(crate) use crate::runtime::apps::terminal::agent_state::spawn_agent_state_drain;
pub(crate) use crate::runtime::apps::terminal::terminal_streams::abort_output_pumps;
pub(super) use commands::{Route, route};
#[cfg(test)]
pub(super) use dispatch::handle_client;
pub(crate) use lifecycle::{
    accept_loop, detach_and_release_consumer_state, fire_client_detached,
    release_revoked_consumer_state,
};
pub(crate) use metadata_subscriptions::broadcast_event;
pub(crate) use pane_events::{PaneEvents, ensure_event_pump, reap_pane_journaling_close};
pub(super) use pane_exit::journal_pane_closed;
pub(crate) use pane_exit::{broadcast_resource_closed, spawn_resource_exit_watcher};
pub(super) use session_create::detach_clients_of_killed_session;
pub(crate) use socket::{handle_existing_socket, prepare_socket_dir, secure_socket_file};

#[cfg(all(test, feature = "native-engine", not(target_arch = "wasm32")))]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "tests"
)]
mod fatal_preflight_close_tests;
#[cfg(test)]
mod metadata_tests;
#[cfg(test)]
mod writer_close_tests;
