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
    /// `todo`, `working`, or `needs_you`.
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
