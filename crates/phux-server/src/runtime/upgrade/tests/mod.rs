//! Upgrade-orchestration tests, split by theme (helpers in `support`, then
//! resume/hub/validate, agent-seal/rollback, and exec-failure/capture
//! coverage) to keep each file under the repo's per-file size budget.

mod agent_seal_rollback;
mod exec_failures_and_capture;
mod pending_evidence;
mod resume_hub_and_validate;
mod support;
