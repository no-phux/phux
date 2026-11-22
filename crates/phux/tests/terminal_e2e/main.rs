//! Lane-selected terminal geometry, fleet sidebar, and plugin surface
//! acceptance tests.

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/listeners.rs"]
mod listeners;
#[path = "../common/paired.rs"]
mod paired;
#[path = "../common/runner.rs"]
mod runner;

mod attach_roles_e2e;
mod fleet_sidebar_e2e;
mod native_e2e;
mod plugin_surfaces_e2e;
mod quic_restore_e2e;
mod resize_e2e;
mod spatial_e2e;
mod update_notice_e2e;
