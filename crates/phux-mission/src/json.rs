//! `phux.mission/v1` documents. The JSON Schema committed beside this crate
//! describes the same shapes.

use serde::Serialize;

use phux_work::{Applied, Objective};

/// Schema name carried on every mission JSON document.
pub const SCHEMA: &str = "phux.mission/v1";

/// Numeric schema version carried beside [`SCHEMA`].
pub const SCHEMA_VERSION: u32 = 1;

/// Which mission verb produced a success document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verb {
    /// `mission add`.
    Add,
    /// `mission list`.
    List,
    /// `mission show`.
    Show,
    /// `mission edit`.
    Edit,
    /// `mission start`.
    Start,
    /// `mission archive`.
    Archive,
    /// `mission restore`.
    Restore,
}

/// One objective, as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskDocument {
    /// Lowercase hex of the opaque 16-byte id.
    pub id: String,
    /// Display alias, `repo/n`.
    pub alias: String,
    /// Repo the alias was allocated under.
    pub repo: String,
    /// Task title.
    pub title: String,
    /// Task description. Empty when none was given.
    pub description: String,
    /// `todo`, `working`, `queued`, or `needs_you`.
    pub state: String,
    /// Structural revision, starting at 1.
    pub revision: u64,
    /// Whether the task is archived.
    pub archived: bool,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 last structural update.
    pub updated_at: String,
}

impl TaskDocument {
    /// Project `objective` into the JSON task object.
    #[must_use]
    pub fn from_objective(objective: &Objective) -> Self {
        Self {
            id: objective.id.to_hex(),
            alias: objective.alias.display(),
            repo: objective.repo.clone(),
            title: objective.title.clone(),
            description: objective.description.clone(),
            state: objective.state.as_str().to_owned(),
            revision: objective.revision,
            archived: objective.archived,
            created_at: objective.created_at.clone(),
            updated_at: objective.updated_at.clone(),
        }
    }
}

/// `add`, `edit`, `archive`, and `restore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MutationDocument {
    /// [`SCHEMA`].
    pub schema: &'static str,
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The verb that wrote.
    pub verb: Verb,
    /// Hex operation id.
    pub operation_id: String,
    /// True when this id was already admitted and nothing was written.
    pub idempotent_replay: bool,
    /// The objective after the write, or the stored result on replay.
    pub task: TaskDocument,
}

impl MutationDocument {
    /// Build the success document for `applied`.
    #[must_use]
    pub fn from_applied(verb: Verb, applied: &Applied) -> Self {
        Self {
            schema: SCHEMA,
            schema_version: SCHEMA_VERSION,
            verb,
            operation_id: applied.operation_id.to_hex(),
            idempotent_replay: applied.idempotent_replay,
            task: TaskDocument::from_objective(&applied.objective),
        }
    }
}

/// `show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShowDocument {
    /// [`SCHEMA`].
    pub schema: &'static str,
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Always [`Verb::Show`].
    pub verb: Verb,
    /// The objective.
    pub task: TaskDocument,
}

impl ShowDocument {
    /// Build the `show` document.
    #[must_use]
    pub fn new(objective: &Objective) -> Self {
        Self {
            schema: SCHEMA,
            schema_version: SCHEMA_VERSION,
            verb: Verb::Show,
            task: TaskDocument::from_objective(objective),
        }
    }
}

/// `list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListDocument {
    /// [`SCHEMA`].
    pub schema: &'static str,
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Always [`Verb::List`].
    pub verb: Verb,
    /// Tasks in repo, then alias number, order.
    pub tasks: Vec<TaskDocument>,
}

impl ListDocument {
    /// Build the `list` document.
    #[must_use]
    pub fn new(objectives: &[Objective]) -> Self {
        Self {
            schema: SCHEMA,
            schema_version: SCHEMA_VERSION,
            verb: Verb::List,
            tasks: objectives
                .iter()
                .map(TaskDocument::from_objective)
                .collect(),
        }
    }
}

/// The run a `start` created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunDocument {
    /// Lowercase hex of the run id.
    pub id: String,
    /// Per-task attempt number, starting at 1.
    pub ordinal: u64,
    /// Harness kind.
    pub agent_kind: String,
    /// Model, or empty.
    pub model: String,
    /// `running` or `queued`.
    pub state: String,
}

/// The worker binding. Null on a queued start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BindingDocument {
    /// Session name.
    pub session: String,
    /// Organization session key.
    pub session_key: String,
    /// Worker pane id, decimal.
    pub terminal_id: String,
    /// Worktree path.
    pub worktree_path: String,
    /// Branch name.
    pub branch: String,
    /// Tab label, such as `#3 Billing page`.
    pub tab_title: String,
}

/// `start`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StartDocument {
    /// [`SCHEMA`].
    pub schema: &'static str,
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Always [`Verb::Start`].
    pub verb: Verb,
    /// Hex operation id.
    pub operation_id: String,
    /// True when this id was already admitted and nothing was written.
    pub idempotent_replay: bool,
    /// `started` or `queued`.
    pub disposition: String,
    /// Set when the task was queued.
    pub queue_position: Option<u64>,
    /// The objective after the write.
    pub task: TaskDocument,
    /// The run row.
    pub run: RunDocument,
    /// The binding, or null when queued.
    pub binding: Option<BindingDocument>,
}

impl StartDocument {
    /// Build the `start` document.
    #[must_use]
    pub fn from_applied(applied: &phux_work::StartApplied) -> Self {
        Self {
            schema: SCHEMA,
            schema_version: SCHEMA_VERSION,
            verb: Verb::Start,
            operation_id: applied.operation_id.to_hex(),
            idempotent_replay: applied.idempotent_replay,
            disposition: applied.disposition.clone(),
            queue_position: applied.queue_position,
            task: TaskDocument::from_objective(&applied.objective),
            run: RunDocument {
                id: applied.run_id.to_hex(),
                ordinal: applied.ordinal,
                agent_kind: applied.agent_kind.clone(),
                model: applied.model.clone(),
                state: applied.run_state.clone(),
            },
            binding: applied.binding.as_ref().map(|binding| BindingDocument {
                session: binding.session.clone(),
                session_key: binding.session_key.clone(),
                terminal_id: binding.terminal_id.clone(),
                worktree_path: binding.worktree_path.clone(),
                branch: binding.branch.clone(),
                tab_title: binding.tab_title.clone(),
            }),
        }
    }
}
