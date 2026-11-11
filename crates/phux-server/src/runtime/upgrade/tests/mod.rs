//! Upgrade-orchestration tests, split by theme (helpers in `support`, then
//! resume/hub/validate, agent-seal/rollback, exec-failure/capture, and
//! native-agent cut coverage) to keep each file under the repo's per-file
//! size budget.

mod agent_seal_rollback;
mod documents;
mod exec_failures_and_capture;
mod native_agent_cut;
mod pinning;
mod resume_hub_and_validate;
mod support;
