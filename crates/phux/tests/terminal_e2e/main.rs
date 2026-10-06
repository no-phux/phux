//! Lane-selected terminal geometry, fleet sidebar, and plugin surface
//! acceptance tests.

#![allow(
    clippy::duplicate_mod,
    reason = "retain each suite's common-module tests and state when consolidating binaries"
)]

#[path = "../common/runner.rs"]
mod runner;

mod attach_roles_e2e;
mod fleet_sidebar_e2e;
mod plugin_surfaces_e2e;
mod quic_restore_e2e;
mod resize_e2e;
mod spatial_e2e;
