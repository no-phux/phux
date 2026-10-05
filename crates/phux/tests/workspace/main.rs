//! Workspace inspection, worktree serialization, and command output contracts.

#[path = "../common/runner.rs"]
mod runner;

#[path = "../common/ambient.rs"]
mod common;

mod output_hygiene;
mod workspace_inspect;
mod worktree_json;
