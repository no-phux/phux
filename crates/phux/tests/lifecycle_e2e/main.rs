//! Lane-selected failure, idle-exit, upgrade, and remote acceptance tests.

#![allow(
    clippy::duplicate_mod,
    reason = "retain each suite's common-module tests and state when consolidating binaries"
)]

mod failure_ux_e2e;
mod idle_exit_e2e;
mod remote_session_verbs_e2e;
mod remote_target_e2e;
mod upgrade_e2e;
mod whoami_e2e;
