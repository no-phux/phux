//! Lane-selected agent automation and workspace archive acceptance tests.
//! nextest preserves per-test process isolation across these suite modules.

#[path = "../common/mod.rs"]
mod common;
#[path = "../common/runner.rs"]
mod runner;

mod acp_e2e;
mod agent_list_e2e;
mod agent_log_validation;
mod agent_record_e2e;
mod agent_session_e2e;
mod controller_client_e2e;
mod examples_e2e;
mod kind_dispatch_e2e;
mod plugin_agent_bench_e2e;
mod resource_e2e;
mod run_wait_e2e;
mod workspace_archive_e2e;
