//! Attach, bootstrap, reconnect, transport, and consumer convergence integration tests.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
#![allow(clippy::future_not_send, reason = "LocalSet-driven tests")]

mod attach_snapshot;
mod attach_targets;
mod attach_terminal_closed;
mod bootstrap_compression;
mod common;
mod concurrent_attach_l2;
mod detach_terminal;
mod end_to_end;
mod hello_survives_detach;
mod lagged_attach_terminal_resync;
mod native_kernel;
mod reattach_multipane_input;
mod reattach_other_session;
mod reconnect_scenario;
mod release_bootstrap_milestones;
mod statesync_convergence;
mod ws_attach;
mod wt_attach;
