//! Host enrollment, remote targets, credentials, and fleet transport tests.

#[path = "../common/ambient.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;

mod host_enroll;
mod host_lifecycle;
mod pair_credentials;
mod partial_fleet;
mod production_state_guard;
mod remote_target_cli;
mod stdio_bridge_e2e;
