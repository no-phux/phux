//! Organization commands, their refusals, and what applying one yields.
//!
//! These are core-local types (R02): the server maps wire commands onto
//! [`Command`] and [`Refusal`] back onto the wire. Reads (`GetOrganization`,
//! `ArrangementGet`) are not commands here; the server answers them from
//! [`crate::Org`] directly.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::activation::{ExecState, ExitPolicy, Outcome, ProcIdentity, SessionLifetime};
use crate::actor::Verb;
use crate::arrangement::{ArrangementOwner, Group};
use crate::keys::{ActivationId, ArrangementKey, OperationId, ResourceKey, Revision, SessionKey};
use crate::org::{RevisionPin, Transition};
use crate::resource::{LaunchSpec, ResourceKind};
use crate::session::Attrs;

/// Most resources one [`Op::ResourceMoveMany`] may move.
pub const MAX_MOVE_BATCH: usize = 256;

/// A command: the caller's operation id plus what to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command {
    /// Caller-generated; a retry reuses it.
    pub operation: OperationId,
    /// What to do.
    pub op: Op,
}

/// A resource to create inside [`Op::SessionCreate`] or [`Op::ResourceCreate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewResource {
    /// Which kind.
    pub kind: ResourceKind,
    /// Optional label.
    #[serde(default)]
    pub name: Option<String>,
    /// How it starts. `Some` commits a launch intent in the same
    /// transaction unless `launch_mode` defers it; a Terminal requires one.
    #[serde(default)]
    pub launch: Option<LaunchSpec>,
    /// What an observed exit does to the record.
    pub on_exit: ExitPolicy,
    /// The resource it is bound to (an agent conversation's Terminal).
    #[serde(default)]
    pub parent: Option<ResourceKey>,
    /// Whether `launch` starts now or waits for [`Op::ResourceRelaunch`].
    #[serde(default)]
    pub launch_mode: LaunchMode,
}

/// Whether a created resource starts executing in its creating transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchMode {
    /// Commit the launch intent: the first activation starts.
    #[default]
    Immediate,
    /// Store the launch spec on a dormant record with no activation;
    /// [`Op::ResourceRelaunch`] starts it.
    Deferred,
}

/// A source session and the revision the caller observed, for
/// [`Op::ResourceMoveMany`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePin {
    /// The source session.
    pub session: SessionKey,
    /// Its revision as observed.
    pub expected: Revision,
}

/// How a termination is delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminateMode {
    /// Ask politely (`SIGHUP`/`SIGTERM` for a Terminal).
    Graceful,
    /// Kill.
    Force,
}

/// Every mutation core knows how to apply (CONTRACTS §04, minus workspace
/// commands), plus executor reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Create a session, optionally with one resource in the same
    /// transaction. `Ephemeral` requires `resource`.
    SessionCreate {
        /// Unique label.
        name: String,
        /// Lifetime.
        lifetime: SessionLifetime,
        /// Descriptive attributes.
        attrs: Attrs,
        /// A first member, created with the session.
        resource: Option<NewResource>,
    },
    /// Relabel a session.
    SessionRename {
        /// Target.
        session: SessionKey,
        /// Observed revision.
        expected: Revision,
        /// New unique label.
        name: String,
    },
    /// Switch lifetime. `Ephemeral` on an empty session deletes it.
    SessionSetLifetime {
        /// Target.
        session: SessionKey,
        /// Observed revision.
        expected: Revision,
        /// New lifetime.
        lifetime: SessionLifetime,
    },
    /// Delete an empty session and its arrangement.
    SessionDelete {
        /// Target.
        session: SessionKey,
    },
    /// Create a resource in a session (the Inbox session when `None`).
    ResourceCreate {
        /// Destination; `None` means the designated Inbox session.
        session: Option<SessionKey>,
        /// The resource.
        resource: NewResource,
    },
    /// Move one resource between sessions. Membership only.
    ResourceMove {
        /// The resource.
        resource: ResourceKey,
        /// Its current session, as observed.
        from: SessionKey,
        /// Observed revision of `from`.
        from_expected: Revision,
        /// Destination.
        to: SessionKey,
        /// Observed revision of `to`.
        to_expected: Revision,
    },
    /// Move a fixed set of resources into one session, atomically.
    ResourceMoveMany {
        /// The resources, at most [`MAX_MOVE_BATCH`], no duplicates.
        resources: Vec<ResourceKey>,
        /// Every session a resource currently belongs to, with its observed
        /// revision.
        sources: Vec<SourcePin>,
        /// Destination.
        to: SessionKey,
        /// Observed revision of `to`.
        to_expected: Revision,
    },
    /// Clear an imported session's `awaiting_organization` marker.
    InboxResolve {
        /// Target.
        session: SessionKey,
        /// Observed revision.
        expected: Revision,
    },
    /// Apply a stored receipt's inverse.
    OrganizationUndo {
        /// The operation to undo.
        original: OperationId,
        /// Revisions the caller observed after the original; each must
        /// still be current. The receipt's own pins are checked too.
        expected: Vec<RevisionPin>,
    },
    /// Relabel a resource.
    ResourceRename {
        /// Target.
        resource: ResourceKey,
        /// Observed revision.
        expected: Revision,
        /// New label, or `None` to clear.
        name: Option<String>,
    },
    /// Ask the executor to end the current (or lost) activation.
    ResourceTerminate {
        /// Target.
        resource: ResourceKey,
        /// The activation the caller observed.
        activation: ActivationId,
        /// Delivery.
        mode: TerminateMode,
    },
    /// Remove a resource whose execution is proven inactive.
    ResourceRemove {
        /// Target.
        resource: ResourceKey,
    },
    /// Start a new activation from the record's launch spec.
    ResourceRelaunch {
        /// Target.
        resource: ResourceKey,
        /// Observed revision.
        expected: Revision,
    },
    /// Replace an owner's arrangement document.
    ArrangementReplace {
        /// Whose arrangement.
        owner: ArrangementOwner,
        /// Observed arrangement revision; [`Revision::ZERO`] when none
        /// exists yet.
        expected: Revision,
        /// The new document.
        groups: Vec<Group>,
    },
    /// Executor report: the activation holds a live handle. System only.
    ActivationStarted {
        /// Target.
        resource: ResourceKey,
        /// The activation.
        activation: ActivationId,
        /// The host process, when the kind runs one.
        proc: Option<ProcIdentity>,
    },
    /// Executor report: the activation ended. System only.
    ActivationEnded {
        /// Target.
        resource: ResourceKey,
        /// The activation.
        activation: ActivationId,
        /// [`ExecState::Completed`] or [`ExecState::Failed`].
        state: ExecState,
        /// How it ended.
        outcome: Outcome,
    },
    /// Executor report: the owner lost its handle. System only.
    ActivationLost {
        /// Target.
        resource: ResourceKey,
        /// The activation.
        activation: ActivationId,
    },
}

impl Op {
    /// Whether this is an executor report rather than a user command.
    #[must_use]
    pub const fn is_report(&self) -> bool {
        matches!(
            self,
            Self::ActivationStarted { .. }
                | Self::ActivationEnded { .. }
                | Self::ActivationLost { .. }
        )
    }
}

/// Why a command was refused. Nothing was changed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Refusal {
    /// The actor lacks the verb.
    #[error("unauthorized: needs {verb:?}")]
    Unauthorized {
        /// The verb required.
        verb: Verb,
    },
    /// A named record does not exist.
    #[error("not found")]
    NotFound,
    /// An expected revision is not current.
    #[error("conflict: current revision {current}")]
    Conflict {
        /// The current revision of the first mismatched record.
        current: Revision,
    },
    /// The named activation is not the current one, or not in a state the
    /// command acts on.
    #[error("stale activation")]
    Stale {
        /// The current activation, if any.
        current: Option<ActivationId>,
    },
    /// The session still has members.
    #[error("session has {members} members")]
    NotEmpty {
        /// How many.
        members: u32,
    },
    /// The command is not supported in this form.
    #[error("unsupported: {capability}")]
    Unsupported {
        /// What is missing.
        capability: String,
    },
    /// An argument is invalid.
    #[error("invalid {field}: {reason}")]
    InvalidArgument {
        /// Which argument.
        field: &'static str,
        /// Why.
        reason: String,
    },
    /// The name is already used by a live session.
    #[error("name taken")]
    NameTaken,
    /// A lost activation blocks removal or relaunch.
    #[error("execution uncertain: activation is lost")]
    ExecutionUncertain,
    /// The same operation id arrived with a different payload.
    #[error("duplicate operation id with a different payload")]
    DuplicateOperation,
    /// The durable organization store could not commit the transition, so
    /// nothing changed. Retrying the same operation may succeed.
    #[error("the organization store failed: {reason}")]
    StoreFailure {
        /// What the store reported.
        reason: String,
    },
}

impl Refusal {
    pub(crate) fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidArgument {
            field,
            reason: reason.into(),
        }
    }
}

/// Work the server must perform after committing a transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Start `activation` of `resource` from `spec`; report back with
    /// [`Op::ActivationStarted`] or [`Op::ActivationEnded`].
    Launch {
        /// Target.
        resource: ResourceKey,
        /// The new activation, recorded as `Starting`.
        activation: ActivationId,
        /// How to start it.
        spec: LaunchSpec,
    },
    /// End `activation` of `resource`; report back with
    /// [`Op::ActivationEnded`].
    Terminate {
        /// Target.
        resource: ResourceKey,
        /// The activation to end.
        activation: ActivationId,
        /// Delivery.
        mode: TerminateMode,
        /// The recorded host process, for an orphan the executor holds no
        /// handle to.
        proc: Option<ProcIdentity>,
    },
}

/// Keys a command created.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Created {
    /// A new session.
    #[serde(default)]
    pub session: Option<SessionKey>,
    /// A new resource.
    #[serde(default)]
    pub resource: Option<ResourceKey>,
    /// A new arrangement.
    #[serde(default)]
    pub arrangement: Option<ArrangementKey>,
}

/// The caller-visible result of an applied command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    /// Committed.
    Ok {
        /// The primary edited record's new revision, when there is one.
        revision: Option<Revision>,
        /// Keys created.
        created: Created,
    },
    /// Committed; execution effects follow as events.
    Accepted {
        /// Keys created.
        created: Created,
    },
}

/// A successfully applied command: the transition to commit, the effects to
/// run after it commits, and the reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// The command's operation id.
    pub operation: OperationId,
    /// Records to write, events to publish, and the receipt to store.
    /// Empty when `replayed`.
    pub transition: Transition,
    /// Execution work, in order. Empty when `replayed`.
    pub effects: Vec<Effect>,
    /// What the caller gets back.
    pub reply: Reply,
    /// The operation id matched a stored receipt with the same payload;
    /// nothing was re-applied.
    pub replayed: bool,
}
