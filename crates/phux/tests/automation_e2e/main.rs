//! Lane-selected agent automation and workspace archive acceptance tests.
//! nextest preserves per-test process isolation across these suite modules.

#![allow(
    clippy::duplicate_mod,
    reason = "retain each suite's common-module tests and state when consolidating binaries"
)]

mod agent_record_e2e;
mod agent_session_e2e;
mod plugin_agent_bench_e2e;
mod run_wait_e2e;
mod workspace_archive_e2e;
