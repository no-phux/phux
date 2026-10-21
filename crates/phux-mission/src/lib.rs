//! Mission control: the task vocabulary people and agents see, composed over
//! the work store in `phux-work`.
//!
//! This crate owns the `phux.mission/v1` documents. It never opens the
//! store; it renders what a [`phux_work::Client`] returns. The board, Lead,
//! and `start` land here.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod json;
mod start;
mod state;

pub use json::{
    BindingDocument, ListDocument, MutationDocument, RunDocument, SCHEMA, SCHEMA_VERSION,
    ShowDocument, StartDocument, TaskDocument, Verb,
};
pub use start::{
    DEFAULT_MAX_AGENTS, DEFAULT_RULES, START_LINE, TaskPaths, branch_name, brief_argv,
    brief_markdown, build_env, default_target_dir, integration_ids, parse_agent, slug, tab_title,
    task_paths,
};
pub use state::{
    AgentReport, Projection, ProjectionInput, RunPhase, TerminalFact, observation_operation,
    project,
};
