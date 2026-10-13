//! Organization commands, results, and events (`docs/spec/L1.md` §5.7).
//!
//! Sessions and resources are persistent records named by 16-byte keys;
//! live handles (`SessionId`, `ResourceId`) name current execution. Every
//! organization body is field-tagged TLV in one id space
//! ([`field::org`]), so a field means the same thing in a command, a value,
//! and an event, and every body is additive. Gated on
//! [`ServerFeatureExt::Organization`](crate::caps::ServerFeatureExt::Organization).

use bytes::BytesMut;

use crate::ids::{
    ActivationId, ArrangementKey, AuthorityId, LeaseEpoch, OperationId, ResourceId, ResourceKey,
    ResourceKind, Revision, ServerInstance, SessionId, SessionKey, WorkspaceKey,
};
use crate::kinds::Verb;
use crate::wire::decode::{Decoder, utf8_value};
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;

use super::codec::{
    decode_actor_ref, decode_server_instance, decode_terminal_id, encode_actor_ref,
    encode_server_instance, encode_terminal_id,
};
use super::{
    ActorRef, AgentEvent, Command, EVENT_TAG_ACTIVATION_CHANGED, EVENT_TAG_ARRANGEMENT_CHANGED,
    EVENT_TAG_INTAKE_CHANGED, EVENT_TAG_LEASE_CHANGED, EVENT_TAG_MEMBERSHIP_CHANGED,
    EVENT_TAG_RESOURCE_CREATED, EVENT_TAG_RESOURCE_REMOVED, EVENT_TAG_RESOURCE_RENAMED,
    EVENT_TAG_SESSION_CREATED, EVENT_TAG_SESSION_DELETED, EVENT_TAG_SESSION_LIFETIME_CHANGED,
    EVENT_TAG_SESSION_RENAMED, MAX_HISTORY_CURSOR_BYTES,
};

/// Most bytes in an organization name (a session, resource, or group name).
pub const MAX_ORG_NAME_BYTES: usize = 256;
/// Most bytes in an encoded arrangement document.
pub const MAX_ARRANGEMENT_BYTES: usize = 64 * 1024;
/// Most items in one bulk move, and most revision pins in one undo.
pub const MAX_ORG_BATCH: usize = 256;
/// Most descriptive attributes on one session.
pub const MAX_ORG_ATTRS: usize = 64;
/// Most argv elements or environment pairs in one `LaunchSpec`.
pub const MAX_LAUNCH_LIST: usize = 256;
/// Most bytes in a `LaunchSpec` restore record.
pub const MAX_LAUNCH_RESTORE_BYTES: usize = 16 * 1024;
/// Most bytes in an organization page cursor.
pub const MAX_ORG_CURSOR_BYTES: usize = MAX_HISTORY_CURSOR_BYTES;
/// Deepest split nesting an arrangement group may hold.
pub const MAX_ARRANGEMENT_DEPTH: usize = crate::wire::info::MAX_LAYOUT_DEPTH;

wire_enum! { to_u8 / from_u8;
/// Whether a session outlives its last resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SessionLifetime {
    /// Survives empty; the default for a named session.
    #[default]
    Persistent = 0,
    /// Deleted in the same transaction that removes its last resource; never
    /// empty after a commit.
    Ephemeral = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// How `RESOURCE_TERMINATE` ends an activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TerminateMode {
    /// Ask the process to exit (SIGTERM for a Terminal).
    #[default]
    Graceful = 0,
    /// End it now (SIGKILL for a Terminal).
    Force = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// What happens to a resource record when its execution ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExitPolicy {
    /// Remove the record: an interactive shell closes when it exits.
    #[default]
    Remove = 0,
    /// Keep the record with its outcome, for inspection or relaunch.
    Retain = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// Whether a created resource starts executing in the same transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LaunchMode {
    /// Commit the launch intent with the record: the first activation starts.
    #[default]
    Immediate = 0,
    /// Store the launch spec without an activation. The record stays dormant
    /// until a `RESOURCE_RELAUNCH` starts it.
    Deferred = 1,
}
}

wire_enum! { to_u8 / from_u8;
/// Where one activation is in its execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExecState {
    /// Launch intent committed; the executor has not reported running.
    Starting = 0,
    /// Executing.
    Running = 1,
    /// A termination was requested and has not completed.
    Stopping = 2,
    /// Ended; the outcome says how.
    Completed = 3,
    /// Could not start.
    Failed = 4,
    /// Recorded live under a previous server instance, and its process may
    /// still run: remove and relaunch refuse until it is resolved.
    Lost = 5,
    /// A lost activation the owner released from management.
    Abandoned = 6,
}
}

wire_enum! { to_u8 / from_u8;
/// Axis of an arrangement split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitDirection {
    /// Side by side.
    Horizontal = 0,
    /// Stacked.
    Vertical = 1,
}
}

/// How an activation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// The process exited with this status.
    Exited(i32),
    /// The process was ended by this signal.
    Signaled(i32),
    /// The server did not observe the end (it ended while no server ran).
    Unobserved,
}

const OUTCOME_EXITED: u8 = 0;
const OUTCOME_SIGNALED: u8 = 1;
const OUTCOME_UNOBSERVED: u8 = 2;

/// The current execution of a resource, as a kind operation addresses it: a
/// stale activation fails before any effect.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveTarget {
    /// The live handle.
    pub resource: ResourceId,
    /// The server instance whose id space `resource` belongs to.
    pub instance: ServerInstance,
    /// The activation the caller means.
    pub activation: ActivationId,
}

/// How a resource's execution starts and restarts. Kind-specific; a
/// Terminal reads every field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct LaunchSpec {
    /// Argv; empty = the default shell.
    pub command: Vec<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Environment pairs, in order.
    pub env: Vec<(String, String)>,
    /// `TERM` override.
    pub term: Option<String>,
    /// Kind-specific restore record, e.g. the agent conversation a
    /// relaunched Terminal resumes.
    pub restore: Option<Vec<u8>>,
}

impl LaunchSpec {
    /// A launch of `command` (empty = the default shell).
    #[must_use]
    pub const fn new(command: Vec<String>) -> Self {
        Self {
            command,
            cwd: None,
            env: Vec::new(),
            term: None,
            restore: None,
        }
    }

    /// Builder setter for [`Self::cwd`].
    #[must_use]
    pub fn with_cwd(mut self, cwd: Option<String>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Builder setter for [`Self::env`].
    #[must_use]
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Builder setter for [`Self::term`].
    #[must_use]
    pub fn with_term(mut self, term: Option<String>) -> Self {
        self.term = term;
        self
    }

    /// Builder setter for [`Self::restore`].
    #[must_use]
    pub fn with_restore(mut self, restore: Option<Vec<u8>>) -> Self {
        self.restore = restore;
        self
    }
}

/// Which records a `GET_ORGANIZATION` page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OrgFilter {
    /// Every session and resource the caller inventories.
    #[default]
    All,
    /// One session and its members.
    Session(SessionKey),
    /// The sessions one workspace links.
    Workspace(WorkspaceKey),
    /// The Inbox: the designated Inbox session and sessions awaiting
    /// organization, with their members.
    Intake,
}

const FILTER_ALL: u8 = 0;
const FILTER_SESSION: u8 = 1;
const FILTER_WORKSPACE: u8 = 2;
const FILTER_INTAKE: u8 = 3;

/// One resource of a `RESOURCE_MOVE_MANY` batch, with the source session
/// revision the caller saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MoveItem {
    /// The resource to move.
    pub resource: ResourceKey,
    /// Its current session.
    pub from: SessionKey,
    /// The source session's expected revision.
    pub from_expected: Revision,
}

/// One record's revision: a commit's resulting revision, or a revision an
/// undo expects to still hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordRevision {
    /// The record's key bytes (a session, resource, arrangement, or
    /// workspace key; keys are unique across kinds per authority).
    pub key: [u8; 16],
    /// The revision.
    pub revision: Revision,
}

/// The record an arrangement belongs to. On the wire: a `SESSION` field or a
/// `WORKSPACE` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArrangementOwner {
    /// A session's arrangement; it may name only that session's members.
    Session(SessionKey),
    /// A workspace's arrangement.
    Workspace(WorkspaceKey),
}

/// A leaf of an arrangement: one presented entry of one target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryRef {
    /// Random id of this entry, stable while the entry exists.
    pub entry: [u8; 16],
    /// What it shows.
    pub target: EntryTarget,
}

/// What an arrangement entry shows. Only a [`Self::Member`] is checked
/// against the owning session's members.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryTarget {
    /// A resource record of this authority, by key.
    Member(ResourceKey),
    /// A resource a federation satellite serves, by its satellite-local id.
    Satellite {
        /// The satellite host token, 1..=255 bytes.
        host: crate::ids::SatelliteHost,
        /// The satellite-local resource id.
        id: u32,
    },
}

const TARGET_MEMBER: u8 = 0;
const TARGET_SATELLITE: u8 = 1;
/// Most bytes in a satellite entry's host token.
const MAX_ENTRY_HOST_BYTES: usize = 255;

/// A node of an arrangement group's split tree.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrangementNode {
    /// One entry.
    Leaf(EntryRef),
    /// Two children sharing the parent's rectangle.
    Split {
        /// The split axis.
        dir: SplitDirection,
        /// Fraction given to `first`, in the open interval `(0, 1)`.
        ratio: f32,
        /// Left or top child.
        first: Box<Self>,
        /// Right or bottom child.
        second: Box<Self>,
    },
}

/// One group of an arrangement (a tab): a name and one split tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ArrangementGroup {
    /// Random id of the group, stable while it exists.
    pub id: [u8; 16],
    /// Display name, at most 256 bytes.
    pub name: Option<String>,
    /// The split tree.
    pub tree: ArrangementNode,
}

/// A presentation of resources: ordered groups, each one split tree whose
/// leaves name resources by key. Encoded at most 64 KiB.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArrangementDoc {
    /// The groups, in display order.
    pub groups: Vec<ArrangementGroup>,
}

/// One session record in an organization page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SessionRecord {
    /// The persistent key.
    pub key: SessionKey,
    /// Unique among live sessions on the authority.
    pub name: String,
    /// Whether it outlives its last resource.
    pub lifetime: SessionLifetime,
    /// Whether it is imported work awaiting organization (the Inbox).
    pub awaiting_organization: bool,
    /// Its arrangement, if it has one.
    pub arrangement: Option<ArrangementKey>,
    /// Descriptive attributes (project, directory, host label), in order.
    pub attrs: Vec<(String, String)>,
    /// Number of member resources.
    pub members: u32,
    /// The record's revision.
    pub revision: Revision,
    /// Its live handle on this server instance.
    pub live: Option<SessionId>,
}

impl SessionRecord {
    /// A persistent, organized session with no members.
    #[must_use]
    pub fn new(key: SessionKey, name: impl Into<String>, revision: Revision) -> Self {
        Self {
            key,
            name: name.into(),
            lifetime: SessionLifetime::Persistent,
            awaiting_organization: false,
            arrangement: None,
            attrs: Vec::new(),
            members: 0,
            revision,
            live: None,
        }
    }

    /// Builder setter for [`Self::lifetime`].
    #[must_use]
    pub const fn with_lifetime(mut self, lifetime: SessionLifetime) -> Self {
        self.lifetime = lifetime;
        self
    }

    /// Builder setter for [`Self::awaiting_organization`].
    #[must_use]
    pub const fn with_awaiting_organization(mut self, awaiting: bool) -> Self {
        self.awaiting_organization = awaiting;
        self
    }

    /// Builder setter for [`Self::arrangement`].
    #[must_use]
    pub const fn with_arrangement(mut self, arrangement: Option<ArrangementKey>) -> Self {
        self.arrangement = arrangement;
        self
    }

    /// Builder setter for [`Self::attrs`].
    #[must_use]
    pub fn with_attrs(mut self, attrs: Vec<(String, String)>) -> Self {
        self.attrs = attrs;
        self
    }

    /// Builder setter for [`Self::members`].
    #[must_use]
    pub const fn with_members(mut self, members: u32) -> Self {
        self.members = members;
        self
    }

    /// Builder setter for [`Self::live`].
    #[must_use]
    pub const fn with_live(mut self, live: Option<SessionId>) -> Self {
        self.live = live;
        self
    }
}

/// The current or most recent execution of a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ActivationInfo {
    /// The activation id.
    pub id: ActivationId,
    /// The server instance that started it.
    pub instance: ServerInstance,
    /// Where it is.
    pub state: ExecState,
    /// How it ended, once it has.
    pub outcome: Option<Outcome>,
}

impl ActivationInfo {
    /// An activation with no outcome.
    #[must_use]
    pub const fn new(id: ActivationId, instance: ServerInstance, state: ExecState) -> Self {
        Self {
            id,
            instance,
            state,
            outcome: None,
        }
    }

    /// Builder setter for [`Self::outcome`].
    #[must_use]
    pub const fn with_outcome(mut self, outcome: Option<Outcome>) -> Self {
        self.outcome = outcome;
        self
    }
}

/// One resource record in an organization page.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceRecord {
    /// The persistent key.
    pub key: ResourceKey,
    /// The one session it belongs to.
    pub session: SessionKey,
    /// Display name.
    pub name: Option<String>,
    /// What backs it.
    pub kind: ResourceKind,
    /// What happens to the record when execution ends.
    pub on_exit: ExitPolicy,
    /// The current or most recent activation.
    pub activation: Option<ActivationInfo>,
    /// The record's revision.
    pub revision: Revision,
    /// Its live handle on this server instance, while it executes here.
    pub live: Option<ResourceId>,
}

impl ResourceRecord {
    /// A Terminal record with no activation.
    #[must_use]
    pub const fn new(key: ResourceKey, session: SessionKey, revision: Revision) -> Self {
        Self {
            key,
            session,
            name: None,
            kind: ResourceKind::Terminal,
            on_exit: ExitPolicy::Remove,
            activation: None,
            revision,
            live: None,
        }
    }

    /// Builder setter for [`Self::name`].
    #[must_use]
    pub fn with_name(mut self, name: Option<String>) -> Self {
        self.name = name;
        self
    }

    /// Builder setter for [`Self::kind`].
    #[must_use]
    pub const fn with_kind(mut self, kind: ResourceKind) -> Self {
        self.kind = kind;
        self
    }

    /// Builder setter for [`Self::on_exit`].
    #[must_use]
    pub const fn with_on_exit(mut self, on_exit: ExitPolicy) -> Self {
        self.on_exit = on_exit;
        self
    }

    /// Builder setter for [`Self::activation`].
    #[must_use]
    pub const fn with_activation(mut self, activation: Option<ActivationInfo>) -> Self {
        self.activation = activation;
        self
    }

    /// Builder setter for [`Self::live`].
    #[must_use]
    pub fn with_live(mut self, live: Option<ResourceId>) -> Self {
        self.live = live;
        self
    }
}

/// One bounded page of the organization (`GET_ORGANIZATION`'s reply).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OrganizationPage {
    /// The authority the records belong to.
    pub authority: AuthorityId,
    /// The designated Inbox session, when the caller observes it.
    pub inbox: Option<SessionKey>,
    /// Session records on this page.
    pub sessions: Vec<SessionRecord>,
    /// Resource records on this page.
    pub resources: Vec<ResourceRecord>,
    /// Sessions matching the filter across all pages.
    pub total_sessions: u32,
    /// Resources matching the filter across all pages.
    pub total_resources: u32,
    /// Matching sessions awaiting organization.
    pub total_intake: u32,
    /// The organization revision the page was cut at.
    pub snapshot_revision: Revision,
    /// The event-journal `seq` at the cut: subscribe after it.
    pub journal_seq: u64,
    /// The next page's cursor; `None` on the last page.
    pub next_cursor: Option<Vec<u8>>,
}

impl OrganizationPage {
    /// An empty last page.
    #[must_use]
    pub const fn new(
        authority: AuthorityId,
        snapshot_revision: Revision,
        journal_seq: u64,
    ) -> Self {
        Self {
            authority,
            inbox: None,
            sessions: Vec::new(),
            resources: Vec::new(),
            total_sessions: 0,
            total_resources: 0,
            total_intake: 0,
            snapshot_revision,
            journal_seq,
            next_cursor: None,
        }
    }

    /// Builder setter for [`Self::inbox`].
    #[must_use]
    pub const fn with_inbox(mut self, inbox: Option<SessionKey>) -> Self {
        self.inbox = inbox;
        self
    }

    /// Builder setter for [`Self::sessions`].
    #[must_use]
    pub fn with_sessions(mut self, sessions: Vec<SessionRecord>) -> Self {
        self.sessions = sessions;
        self
    }

    /// Builder setter for [`Self::resources`].
    #[must_use]
    pub fn with_resources(mut self, resources: Vec<ResourceRecord>) -> Self {
        self.resources = resources;
        self
    }

    /// Builder setter for the three totals.
    #[must_use]
    pub const fn with_totals(mut self, sessions: u32, resources: u32, intake: u32) -> Self {
        self.total_sessions = sessions;
        self.total_resources = resources;
        self.total_intake = intake;
        self
    }

    /// Builder setter for [`Self::next_cursor`].
    #[must_use]
    pub fn with_next_cursor(mut self, next_cursor: Option<Vec<u8>>) -> Self {
        self.next_cursor = next_cursor;
        self
    }
}

/// An arrangement and its revision (`ARRANGEMENT_GET`'s reply).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct ArrangementValue {
    /// The arrangement's key.
    pub key: ArrangementKey,
    /// The record that owns it.
    pub owner: ArrangementOwner,
    /// The revision `document` is at.
    pub revision: Revision,
    /// The document.
    pub document: ArrangementDoc,
}

impl ArrangementValue {
    /// An arrangement value.
    #[must_use]
    pub const fn new(
        key: ArrangementKey,
        owner: ArrangementOwner,
        revision: Revision,
        document: ArrangementDoc,
    ) -> Self {
        Self {
            key,
            owner,
            revision,
            document,
        }
    }
}

/// A committed organization edit (the reply to a mutating organization
/// command): the receipt a client keeps for undo.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Committed {
    /// The operation that committed.
    pub operation: OperationId,
    /// The primary record's resulting revision.
    pub revision: Revision,
    /// The session the operation created.
    pub session: Option<SessionKey>,
    /// The resource the operation created.
    pub resource: Option<ResourceKey>,
    /// The live target of an execution the operation started.
    pub target: Option<LiveTarget>,
    /// Every affected record's resulting revision; an undo expects them.
    pub revisions: Vec<RecordRevision>,
    /// The intake state an `INBOX_RESOLVE` cleared.
    pub prior_awaiting: Option<bool>,
}

/// The longest delegated credential id a decoder accepts.
pub const MAX_CREDENTIAL_ID_BYTES: usize = 256;

/// The longest delegated-credential socket path a decoder accepts.
pub const MAX_SOCKET_PATH_BYTES: usize = 4096;

/// A minted delegated agent credential (the reply to `DELEGATE_AGENT`,
/// ADR-0163 §5): an agent dials [`Self::socket_path`] and acts as
/// [`Self::credential_id`] on [`Self::session`] until [`Self::expires_at_ms`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Delegation {
    /// The credential id; `REVOKE_DELEGATION` names it.
    pub credential_id: String,
    /// The session the credential is scoped to.
    pub session: SessionKey,
    /// The owner-only socket every connection of the agent arrives on.
    pub socket_path: String,
    /// When the credential stops admitting anything, Unix milliseconds.
    pub expires_at_ms: u64,
}

impl Delegation {
    /// A minted credential.
    #[must_use]
    pub const fn new(
        credential_id: String,
        session: SessionKey,
        socket_path: String,
        expires_at_ms: u64,
    ) -> Self {
        Self {
            credential_id,
            session,
            socket_path,
            expires_at_ms,
        }
    }
}

impl Committed {
    /// A receipt with only the operation and the primary revision.
    #[must_use]
    pub const fn new(operation: OperationId, revision: Revision) -> Self {
        Self {
            operation,
            revision,
            session: None,
            resource: None,
            target: None,
            revisions: Vec::new(),
            prior_awaiting: None,
        }
    }

    /// Builder setter for [`Self::session`].
    #[must_use]
    pub const fn with_session(mut self, session: Option<SessionKey>) -> Self {
        self.session = session;
        self
    }

    /// Builder setter for [`Self::resource`].
    #[must_use]
    pub const fn with_resource(mut self, resource: Option<ResourceKey>) -> Self {
        self.resource = resource;
        self
    }

    /// Builder setter for [`Self::target`].
    #[must_use]
    pub fn with_target(mut self, target: Option<LiveTarget>) -> Self {
        self.target = target;
        self
    }

    /// Builder setter for [`Self::revisions`].
    #[must_use]
    pub fn with_revisions(mut self, revisions: Vec<RecordRevision>) -> Self {
        self.revisions = revisions;
        self
    }

    /// Builder setter for [`Self::prior_awaiting`].
    #[must_use]
    pub const fn with_prior_awaiting(mut self, prior_awaiting: Option<bool>) -> Self {
        self.prior_awaiting = prior_awaiting;
        self
    }
}

/// Why an organization command was refused (`COMMAND_RESULT` tag `0x03`).
///
/// Sent only in reply to an organization command; a legacy command, and a
/// legacy write the server translates, never receives one, because an older
/// decoder fails the whole frame on an unknown result tag.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The caller's grant lacks `verb` on the command's subject. The same
    /// refusal answers an absent subject, so existence does not leak.
    Unauthorized {
        /// The verb that was missing.
        verb: Verb,
    },
    /// A named record does not exist.
    NotFound,
    /// The expected revision is not the record's current one.
    Conflict {
        /// The record's current revision.
        current: Revision,
    },
    /// The live target names an activation that is not current.
    Stale {
        /// The current activation, if any.
        current: Option<ActivationId>,
    },
    /// The session still has members.
    NotEmpty {
        /// How many.
        members: u32,
    },
    /// The server does not support what the command needs.
    Unsupported {
        /// The missing capability, by name.
        capability: String,
    },
    /// An argument broke a rule the codec cannot check.
    InvalidArgument {
        /// The argument's name.
        field: String,
        /// Why it was refused.
        reason: String,
    },
    /// Another live session has the name.
    NameTaken,
    /// A lost activation blocks the operation until it is resolved.
    ExecutionUncertain,
    /// The input lease is held by a principal the caller cannot take it from.
    LeaseHeld {
        /// The holder.
        holder: ActorRef,
    },
    /// The operation id was used before with a different payload.
    DuplicateOperation,
    /// The server's durable organization store could not commit, so nothing
    /// changed; the same operation may be retried.
    StoreFailure {
        /// What the store reported.
        reason: String,
    },
    /// A refusal tag this build does not know, body kept verbatim. Produced
    /// only by the decoder.
    Unknown {
        /// The unrecognised tag.
        tag: u8,
        /// The body bytes.
        body: Vec<u8>,
    },
}

const REFUSAL_UNAUTHORIZED: u8 = 0x00;
const REFUSAL_NOT_FOUND: u8 = 0x01;
const REFUSAL_CONFLICT: u8 = 0x02;
const REFUSAL_STALE: u8 = 0x03;
const REFUSAL_NOT_EMPTY: u8 = 0x04;
const REFUSAL_UNSUPPORTED: u8 = 0x05;
const REFUSAL_INVALID_ARGUMENT: u8 = 0x06;
const REFUSAL_NAME_TAKEN: u8 = 0x07;
const REFUSAL_EXECUTION_UNCERTAIN: u8 = 0x08;
const REFUSAL_LEASE_HELD: u8 = 0x09;
const REFUSAL_DUPLICATE_OPERATION: u8 = 0x0a;
const REFUSAL_STORE_FAILURE: u8 = 0x0b;

impl Refusal {
    /// The stable lowercase name consumers print.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Unauthorized { .. } => "unauthorized",
            Self::NotFound => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::Stale { .. } => "stale",
            Self::NotEmpty { .. } => "not_empty",
            Self::Unsupported { .. } => "unsupported",
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::NameTaken => "name_taken",
            Self::ExecutionUncertain => "execution_uncertain",
            Self::LeaseHeld { .. } => "lease_held",
            Self::DuplicateOperation => "duplicate_operation",
            Self::StoreFailure { .. } => "store_failure",
            Self::Unknown { .. } => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Field helpers.
// ---------------------------------------------------------------------------

fn write_key(enc: &mut Encoder<'_>, field_id: u32, key: &[u8; 16]) {
    enc.write_field(field_id, key);
}

fn write_u64(enc: &mut Encoder<'_>, field_id: u32, value: u64) {
    enc.write_field(field_id, &value.to_be_bytes());
}

fn write_u32(enc: &mut Encoder<'_>, field_id: u32, value: u32) {
    enc.write_field(field_id, &value.to_be_bytes());
}

fn write_u8(enc: &mut Encoder<'_>, field_id: u32, value: u8) {
    enc.write_field(field_id, &[value]);
}

fn key_bytes(value: &[u8]) -> Result<[u8; 16], DecodeError> {
    value
        .try_into()
        .map_err(|_| DecodeError::InvalidOrganizationKey)
}

fn read_key<K>(value: &[u8], new: fn([u8; 16]) -> Option<K>) -> Result<K, DecodeError> {
    new(key_bytes(value)?).ok_or(DecodeError::InvalidOrganizationKey)
}

fn read_operation(value: &[u8]) -> Result<OperationId, DecodeError> {
    OperationId::new(key_bytes(value)?).ok_or(DecodeError::InvalidIdempotencyKey)
}

fn read_u64(value: &[u8]) -> Result<u64, DecodeError> {
    Decoder::new(value).read_u64_be()
}

fn read_u32(value: &[u8]) -> Result<u32, DecodeError> {
    Decoder::new(value).read_u32_be()
}

fn read_u8(value: &[u8]) -> Result<u8, DecodeError> {
    Decoder::new(value).read_u8()
}

/// A UTF-8 value of at most `max` bytes.
fn bounded_str(value: &[u8], max: usize) -> Result<String, DecodeError> {
    if value.len() > max {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    utf8_value(value)
}

fn bounded_dec_str(dec: &mut Decoder<'_>, max: usize) -> Result<String, DecodeError> {
    let text = dec.read_str()?;
    if text.len() > max {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    Ok(text.to_owned())
}

/// Push onto a repeated field, refusing more than `max` entries.
fn push_bounded<T>(list: &mut Vec<T>, item: T, max: usize) -> Result<(), DecodeError> {
    if list.len() >= max {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    list.push(item);
    Ok(())
}

fn enum_byte<T>(
    field: &'static str,
    value: Option<u8>,
    from: fn(u8) -> Option<T>,
) -> Result<Option<T>, DecodeError> {
    value
        .map(|raw| from(raw).ok_or_else(|| DecodeError::unknown_enum(field, raw)))
        .transpose()
}

fn req<T>(value: Option<T>) -> Result<T, DecodeError> {
    value.ok_or(DecodeError::UnexpectedEof)
}

fn encode_pair(enc: &mut Encoder<'_>, field_id: u32, pair: &(String, String)) {
    enc.write_field_with(field_id, |e| {
        e.write_str(&pair.0);
        e.write_str(&pair.1);
    });
}

fn decode_pair(value: &[u8], max: usize) -> Result<(String, String), DecodeError> {
    let mut dec = Decoder::new(value);
    let key = bounded_dec_str(&mut dec, max)?;
    let value = bounded_dec_str(&mut dec, max)?;
    Ok((key, value))
}

fn encode_live_target(target: &LiveTarget, enc: &mut Encoder<'_>) {
    encode_terminal_id(&target.resource, enc);
    encode_server_instance(&target.instance, enc);
    enc.write_u64_be(target.activation.get());
}

fn decode_live_target(value: &[u8]) -> Result<LiveTarget, DecodeError> {
    let mut dec = Decoder::new(value);
    let resource = decode_terminal_id(&mut dec)?;
    let instance = decode_server_instance(&mut dec)?;
    let activation = ActivationId::new(dec.read_u64_be()?);
    Ok(LiveTarget {
        resource,
        instance,
        activation,
    })
}

fn encode_outcome(outcome: Outcome, enc: &mut Encoder<'_>) {
    match outcome {
        Outcome::Exited(code) => {
            enc.write_u8(OUTCOME_EXITED);
            enc.write_u32_be(u32::from_be_bytes(code.to_be_bytes()));
        }
        Outcome::Signaled(signal) => {
            enc.write_u8(OUTCOME_SIGNALED);
            enc.write_u32_be(u32::from_be_bytes(signal.to_be_bytes()));
        }
        Outcome::Unobserved => enc.write_u8(OUTCOME_UNOBSERVED),
    }
}

fn decode_outcome(value: &[u8]) -> Result<Outcome, DecodeError> {
    let mut dec = Decoder::new(value);
    let tag = dec.read_u8()?;
    let int = |dec: &mut Decoder<'_>| -> Result<i32, DecodeError> {
        Ok(i32::from_be_bytes(dec.read_u32_be()?.to_be_bytes()))
    };
    match tag {
        OUTCOME_EXITED => Ok(Outcome::Exited(int(&mut dec)?)),
        OUTCOME_SIGNALED => Ok(Outcome::Signaled(int(&mut dec)?)),
        OUTCOME_UNOBSERVED => Ok(Outcome::Unobserved),
        other => Err(DecodeError::unknown_enum("Outcome", other)),
    }
}

fn encode_filter(filter: OrgFilter, enc: &mut Encoder<'_>) {
    match filter {
        OrgFilter::All => enc.write_u8(FILTER_ALL),
        OrgFilter::Session(key) => {
            enc.write_u8(FILTER_SESSION);
            for byte in key.as_bytes() {
                enc.write_u8(*byte);
            }
        }
        OrgFilter::Workspace(key) => {
            enc.write_u8(FILTER_WORKSPACE);
            for byte in key.as_bytes() {
                enc.write_u8(*byte);
            }
        }
        OrgFilter::Intake => enc.write_u8(FILTER_INTAKE),
    }
}

fn decode_filter(value: &[u8]) -> Result<OrgFilter, DecodeError> {
    let (&tag, rest) = value.split_first().ok_or(DecodeError::UnexpectedEof)?;
    match tag {
        FILTER_ALL => Ok(OrgFilter::All),
        FILTER_SESSION => read_key(rest, SessionKey::new).map(OrgFilter::Session),
        FILTER_WORKSPACE => read_key(rest, WorkspaceKey::new).map(OrgFilter::Workspace),
        FILTER_INTAKE => Ok(OrgFilter::Intake),
        other => Err(DecodeError::unknown_enum("OrgFilter", other)),
    }
}

fn encode_launch(launch: &LaunchSpec, enc: &mut Encoder<'_>) {
    for arg in &launch.command {
        enc.write_field(field::org_launch::COMMAND, arg.as_bytes());
    }
    if let Some(cwd) = &launch.cwd {
        enc.write_field(field::org_launch::CWD, cwd.as_bytes());
    }
    for pair in &launch.env {
        encode_pair(enc, field::org_launch::ENV, pair);
    }
    if let Some(term) = &launch.term {
        enc.write_field(field::org_launch::TERM, term.as_bytes());
    }
    if let Some(restore) = &launch.restore {
        enc.write_field(field::org_launch::RESTORE, restore);
    }
}

fn decode_launch(value: &[u8]) -> Result<LaunchSpec, DecodeError> {
    let mut dec = Decoder::new(value);
    let mut launch = LaunchSpec::default();
    while let Some((id, value)) = dec.read_field()? {
        match id {
            field::org_launch::COMMAND => {
                push_bounded(&mut launch.command, utf8_value(value)?, MAX_LAUNCH_LIST)?;
            }
            field::org_launch::CWD => launch.cwd = Some(utf8_value(value)?),
            field::org_launch::ENV => {
                let pair = decode_pair(value, usize::MAX)?;
                push_bounded(&mut launch.env, pair, MAX_LAUNCH_LIST)?;
            }
            field::org_launch::TERM => launch.term = Some(utf8_value(value)?),
            field::org_launch::RESTORE => {
                if value.len() > MAX_LAUNCH_RESTORE_BYTES {
                    return Err(DecodeError::OrganizationLimitExceeded);
                }
                launch.restore = Some(value.to_vec());
            }
            _ => {}
        }
    }
    Ok(launch)
}

fn encode_move_item(item: &MoveItem, enc: &mut Encoder<'_>) {
    enc.write_field_with(field::org::MOVE_ITEM, |e| {
        for byte in item.resource.as_bytes().iter().chain(item.from.as_bytes()) {
            e.write_u8(*byte);
        }
        e.write_u64_be(item.from_expected.get());
    });
}

fn decode_move_item(value: &[u8]) -> Result<MoveItem, DecodeError> {
    if value.len() < 32 {
        return Err(DecodeError::UnexpectedEof);
    }
    let (resource, rest) = value.split_at(16);
    let (from, expected) = rest.split_at(16);
    Ok(MoveItem {
        resource: read_key(resource, ResourceKey::new)?,
        from: read_key(from, SessionKey::new)?,
        from_expected: Revision::new(read_u64(expected)?),
    })
}

fn encode_pin(pin: &RecordRevision, enc: &mut Encoder<'_>) {
    enc.write_field_with(field::org::PIN, |e| {
        for byte in &pin.key {
            e.write_u8(*byte);
        }
        e.write_u64_be(pin.revision.get());
    });
}

fn decode_pin(value: &[u8]) -> Result<RecordRevision, DecodeError> {
    if value.len() < 16 {
        return Err(DecodeError::UnexpectedEof);
    }
    let (key, revision) = value.split_at(16);
    let key = key_bytes(key)?;
    if key == [0; 16] {
        return Err(DecodeError::InvalidOrganizationKey);
    }
    Ok(RecordRevision {
        key,
        revision: Revision::new(read_u64(revision)?),
    })
}

// ---------------------------------------------------------------------------
// Arrangement documents.
// ---------------------------------------------------------------------------

const NODE_LEAF: u8 = 0;
const NODE_SPLIT: u8 = 1;

/// Write an arrangement document positionally: `u16 group_count`, then per
/// group `id: bytes16 || name: optional<str> || node`; a node is `0x00 ||
/// entry: bytes16 || target` or `0x01 || dir: u8 || ratio: f32 ||
/// node || node`; a target is `0x00 || resource: bytes16` (a member) or `0x01 ||
/// host: str || id: u32` (a satellite resource).
pub(in crate::wire) fn encode_arrangement_doc(doc: &ArrangementDoc, enc: &mut Encoder<'_>) {
    enc.write_u16_be(u16::try_from(doc.groups.len()).unwrap_or(u16::MAX));
    for group in &doc.groups {
        for byte in &group.id {
            enc.write_u8(*byte);
        }
        enc.write_option(group.name.as_deref(), Encoder::write_str);
        encode_node(&group.tree, enc);
    }
}

fn encode_node(node: &ArrangementNode, enc: &mut Encoder<'_>) {
    match node {
        ArrangementNode::Leaf(entry) => {
            enc.write_u8(NODE_LEAF);
            for byte in &entry.entry {
                enc.write_u8(*byte);
            }
            match &entry.target {
                EntryTarget::Member(resource) => {
                    enc.write_u8(TARGET_MEMBER);
                    for byte in resource.as_bytes() {
                        enc.write_u8(*byte);
                    }
                }
                EntryTarget::Satellite { host, id } => {
                    enc.write_u8(TARGET_SATELLITE);
                    enc.write_str(host.as_str());
                    enc.write_u32_be(*id);
                }
            }
        }
        ArrangementNode::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            enc.write_u8(NODE_SPLIT);
            enc.write_u8(dir.to_u8());
            enc.write_f32_be(*ratio);
            encode_node(first, enc);
            encode_node(second, enc);
        }
    }
}

/// Read an arrangement document, refusing one over
/// [`MAX_ARRANGEMENT_BYTES`], nested deeper than [`MAX_ARRANGEMENT_DEPTH`],
/// with a ratio outside `(0, 1)`, or with trailing bytes.
pub(in crate::wire) fn decode_arrangement_doc(value: &[u8]) -> Result<ArrangementDoc, DecodeError> {
    if value.len() > MAX_ARRANGEMENT_BYTES {
        return Err(DecodeError::OrganizationLimitExceeded);
    }
    let mut dec = Decoder::new(value);
    let count = usize::from(dec.read_u16_be()?);
    let mut groups = dec.bounded_capacity(count);
    for _ in 0..count {
        let mut id = [0; 16];
        for byte in &mut id {
            *byte = dec.read_u8()?;
        }
        let name = dec.read_option("Option<str> tag", |d| {
            bounded_dec_str(d, MAX_ORG_NAME_BYTES)
        })?;
        let tree = decode_node(&mut dec, 0)?;
        groups.push(ArrangementGroup { id, name, tree });
    }
    if !dec.at_body_end() {
        return Err(DecodeError::InvalidArrangement);
    }
    Ok(ArrangementDoc { groups })
}

fn decode_node(dec: &mut Decoder<'_>, depth: usize) -> Result<ArrangementNode, DecodeError> {
    if depth >= MAX_ARRANGEMENT_DEPTH {
        return Err(DecodeError::LayoutTooDeep);
    }
    match dec.read_u8()? {
        NODE_LEAF => {
            let mut entry = [0; 16];
            for byte in &mut entry {
                *byte = dec.read_u8()?;
            }
            let target = match dec.read_u8()? {
                TARGET_MEMBER => {
                    let mut resource = [0; 16];
                    for byte in &mut resource {
                        *byte = dec.read_u8()?;
                    }
                    EntryTarget::Member(
                        ResourceKey::new(resource).ok_or(DecodeError::InvalidOrganizationKey)?,
                    )
                }
                TARGET_SATELLITE => {
                    let host = dec.read_str()?;
                    if host.is_empty() || host.len() > MAX_ENTRY_HOST_BYTES {
                        return Err(DecodeError::InvalidArrangement);
                    }
                    let host = crate::ids::SatelliteHost::new(host);
                    EntryTarget::Satellite {
                        host,
                        id: dec.read_u32_be()?,
                    }
                }
                other => return Err(DecodeError::unknown_enum("EntryTarget", other)),
            };
            Ok(ArrangementNode::Leaf(EntryRef { entry, target }))
        }
        NODE_SPLIT => {
            let raw_dir = dec.read_u8()?;
            let dir = SplitDirection::from_u8(raw_dir)
                .ok_or_else(|| DecodeError::unknown_enum("SplitDirection", raw_dir))?;
            let ratio = dec.read_f32_be()?;
            if ratio.is_nan() || ratio <= 0.0 || ratio >= 1.0 {
                return Err(DecodeError::InvalidArrangement);
            }
            let first = Box::new(decode_node(dec, depth + 1)?);
            let second = Box::new(decode_node(dec, depth + 1)?);
            Ok(ArrangementNode::Split {
                dir,
                ratio,
                first,
                second,
            })
        }
        other => Err(DecodeError::unknown_enum("ArrangementNode", other)),
    }
}

fn write_document(enc: &mut Encoder<'_>, doc: &ArrangementDoc) {
    enc.write_field_with(field::org::DOCUMENT, |e| encode_arrangement_doc(doc, e));
}

// ---------------------------------------------------------------------------
// The shared field accumulator.
// ---------------------------------------------------------------------------

/// Every `field::org` field a body carried. A body's decoder picks the ones
/// it defines; an unknown id is skipped, so bodies stay additive.
#[derive(Default)]
struct Fields {
    operation: Option<OperationId>,
    session: Option<SessionKey>,
    resource: Option<ResourceKey>,
    expected: Option<Revision>,
    name: Option<String>,
    lifetime: Option<u8>,
    attrs: Vec<(String, String)>,
    launch: Option<Box<LaunchSpec>>,
    kind: Option<ResourceKind>,
    on_exit: Option<u8>,
    from: Option<SessionKey>,
    from_revision: Option<Revision>,
    to: Option<SessionKey>,
    to_revision: Option<Revision>,
    target: Option<LiveTarget>,
    mode: Option<u8>,
    activation: Option<ActivationId>,
    arrangement: Option<ArrangementKey>,
    document: Option<ArrangementDoc>,
    filter: Option<OrgFilter>,
    page_size: Option<u32>,
    cursor: Option<Vec<u8>>,
    undo: Option<OperationId>,
    move_items: Vec<MoveItem>,
    pins: Vec<RecordRevision>,
    revision: Option<Revision>,
    live_session: Option<SessionId>,
    live_resource: Option<ResourceId>,
    state: Option<u8>,
    outcome: Option<Outcome>,
    epoch: Option<LeaseEpoch>,
    holder: Option<ActorRef>,
    awaiting: Option<bool>,
    instance: Option<ServerInstance>,
    members: Option<u32>,
    workspace: Option<WorkspaceKey>,
    ttl_secs: Option<u32>,
    credential: Option<String>,
    socket_path: Option<String>,
    expires_at: Option<u64>,
    launch_mode: Option<u8>,
}

impl Fields {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per field id of the shared organization id space"
    )]
    fn read(value: &[u8]) -> Result<Self, DecodeError> {
        Self::read_from(&mut Decoder::new(value))
    }

    /// Read fields from `dec` to the end of its body.
    fn read_from(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let mut out = Self::default();
        while let Some((id, value)) = dec.read_field()? {
            match id {
                field::org::OPERATION => out.operation = Some(read_operation(value)?),
                field::org::SESSION => out.session = Some(read_key(value, SessionKey::new)?),
                field::org::RESOURCE => out.resource = Some(read_key(value, ResourceKey::new)?),
                field::org::EXPECTED => out.expected = Some(Revision::new(read_u64(value)?)),
                field::org::NAME => out.name = Some(bounded_str(value, MAX_ORG_NAME_BYTES)?),
                field::org::LIFETIME => out.lifetime = Some(read_u8(value)?),
                field::org::ATTR => {
                    let pair = decode_pair(value, MAX_ORG_NAME_BYTES)?;
                    push_bounded(&mut out.attrs, pair, MAX_ORG_ATTRS)?;
                }
                field::org::LAUNCH => out.launch = Some(Box::new(decode_launch(value)?)),
                field::org::KIND => out.kind = Some(ResourceKind::from_wire(read_u8(value)?)),
                field::org::ON_EXIT => out.on_exit = Some(read_u8(value)?),
                field::org::LAUNCH_MODE => out.launch_mode = Some(read_u8(value)?),
                field::org::FROM => out.from = Some(read_key(value, SessionKey::new)?),
                field::org::FROM_REVISION => {
                    out.from_revision = Some(Revision::new(read_u64(value)?));
                }
                field::org::TO => out.to = Some(read_key(value, SessionKey::new)?),
                field::org::TO_REVISION => out.to_revision = Some(Revision::new(read_u64(value)?)),
                field::org::TARGET => out.target = Some(decode_live_target(value)?),
                field::org::MODE => out.mode = Some(read_u8(value)?),
                field::org::ACTIVATION => {
                    out.activation = Some(ActivationId::new(read_u64(value)?));
                }
                field::org::ARRANGEMENT => {
                    out.arrangement = Some(read_key(value, ArrangementKey::new)?);
                }
                field::org::DOCUMENT => out.document = Some(decode_arrangement_doc(value)?),
                field::org::FILTER => out.filter = Some(decode_filter(value)?),
                field::org::PAGE_SIZE => out.page_size = Some(read_u32(value)?),
                field::org::CURSOR => {
                    if value.len() > MAX_ORG_CURSOR_BYTES {
                        return Err(DecodeError::OrganizationLimitExceeded);
                    }
                    out.cursor = Some(value.to_vec());
                }
                field::org::UNDO => out.undo = Some(read_operation(value)?),
                field::org::MOVE_ITEM => {
                    push_bounded(&mut out.move_items, decode_move_item(value)?, MAX_ORG_BATCH)?;
                }
                field::org::PIN => push_bounded(&mut out.pins, decode_pin(value)?, MAX_ORG_BATCH)?,
                field::org::REVISION => out.revision = Some(Revision::new(read_u64(value)?)),
                field::org::LIVE_SESSION => {
                    out.live_session = Some(SessionId::new(read_u32(value)?));
                }
                field::org::LIVE_RESOURCE => {
                    out.live_resource = Some(decode_terminal_id(&mut Decoder::new(value))?);
                }
                field::org::STATE => out.state = Some(read_u8(value)?),
                field::org::OUTCOME => out.outcome = Some(decode_outcome(value)?),
                field::org::EPOCH => out.epoch = Some(LeaseEpoch::new(read_u64(value)?)),
                field::org::HOLDER => {
                    out.holder = Some(decode_actor_ref(&mut Decoder::new(value))?);
                }
                field::org::AWAITING => out.awaiting = Some(read_u8(value)? != 0),
                field::org::INSTANCE => {
                    out.instance = Some(ServerInstance::new(key_bytes(value)?));
                }
                field::org::MEMBERS => out.members = Some(read_u32(value)?),
                field::org::WORKSPACE => {
                    out.workspace = Some(read_key(value, WorkspaceKey::new)?);
                }
                field::org::TTL_SECS => out.ttl_secs = Some(read_u32(value)?),
                field::org::CREDENTIAL => {
                    out.credential = Some(bounded_str(value, MAX_CREDENTIAL_ID_BYTES)?);
                }
                field::org::SOCKET_PATH => {
                    out.socket_path = Some(bounded_str(value, MAX_SOCKET_PATH_BYTES)?);
                }
                field::org::EXPIRES_AT => out.expires_at = Some(read_u64(value)?),
                _ => {}
            }
        }
        Ok(out)
    }

    fn operation(&self) -> Result<OperationId, DecodeError> {
        req(self.operation)
    }

    fn session(&self) -> Result<SessionKey, DecodeError> {
        req(self.session)
    }

    fn resource(&self) -> Result<ResourceKey, DecodeError> {
        req(self.resource)
    }

    fn expected(&self) -> Result<Revision, DecodeError> {
        req(self.expected)
    }

    /// An arrangement's owner: exactly one of `SESSION` and `WORKSPACE`.
    const fn owner(&self) -> Result<ArrangementOwner, DecodeError> {
        match (self.session, self.workspace) {
            (Some(session), None) => Ok(ArrangementOwner::Session(session)),
            (None, Some(workspace)) => Ok(ArrangementOwner::Workspace(workspace)),
            (None, None) => Err(DecodeError::UnexpectedEof),
            (Some(_), Some(_)) => Err(DecodeError::InvalidArrangement),
        }
    }

    fn lifetime(&self) -> Result<Option<SessionLifetime>, DecodeError> {
        enum_byte("SessionLifetime", self.lifetime, SessionLifetime::from_u8)
    }

    fn on_exit(&self) -> Result<Option<ExitPolicy>, DecodeError> {
        enum_byte("ExitPolicy", self.on_exit, ExitPolicy::from_u8)
    }

    fn launch_mode(&self) -> Result<Option<LaunchMode>, DecodeError> {
        enum_byte("LaunchMode", self.launch_mode, LaunchMode::from_u8)
    }

    fn state(&self) -> Result<Option<ExecState>, DecodeError> {
        enum_byte("ExecState", self.state, ExecState::from_u8)
    }
}

fn write_operation(enc: &mut Encoder<'_>, operation: &OperationId) {
    write_key(enc, field::org::OPERATION, operation.as_bytes());
}

fn write_session(enc: &mut Encoder<'_>, field_id: u32, key: &SessionKey) {
    write_key(enc, field_id, key.as_bytes());
}

fn write_resource(enc: &mut Encoder<'_>, key: &ResourceKey) {
    write_key(enc, field::org::RESOURCE, key.as_bytes());
}

fn write_name(enc: &mut Encoder<'_>, name: &str) {
    enc.write_field(field::org::NAME, name.as_bytes());
}

fn write_actor(enc: &mut Encoder<'_>, field_id: u32, actor: &ActorRef) {
    enc.write_field_with(field_id, |e| encode_actor_ref(actor, e));
}

// ---------------------------------------------------------------------------
// Commands.
// ---------------------------------------------------------------------------

/// The nested tag of an organization command, or `None` for another
/// command.
const fn org_command_tag(command: &Command) -> Option<u8> {
    use super::{
        COMMAND_TAG_ARRANGEMENT_GET, COMMAND_TAG_ARRANGEMENT_REPLACE, COMMAND_TAG_GET_ORGANIZATION,
        COMMAND_TAG_INBOX_RESOLVE, COMMAND_TAG_ORGANIZATION_UNDO, COMMAND_TAG_RESOURCE_CREATE,
        COMMAND_TAG_RESOURCE_MOVE, COMMAND_TAG_RESOURCE_MOVE_MANY, COMMAND_TAG_RESOURCE_RELAUNCH,
        COMMAND_TAG_RESOURCE_REMOVE, COMMAND_TAG_RESOURCE_RENAME, COMMAND_TAG_RESOURCE_TERMINATE,
        COMMAND_TAG_SESSION_CREATE, COMMAND_TAG_SESSION_DELETE, COMMAND_TAG_SESSION_RENAME,
        COMMAND_TAG_SESSION_SET_LIFETIME,
    };
    Some(match command {
        Command::GetOrganization { .. } => COMMAND_TAG_GET_ORGANIZATION,
        Command::SessionCreate { .. } => COMMAND_TAG_SESSION_CREATE,
        Command::SessionRename { .. } => COMMAND_TAG_SESSION_RENAME,
        Command::SessionSetLifetime { .. } => COMMAND_TAG_SESSION_SET_LIFETIME,
        Command::SessionDelete { .. } => COMMAND_TAG_SESSION_DELETE,
        Command::ResourceCreate { .. } => COMMAND_TAG_RESOURCE_CREATE,
        Command::ResourceMove { .. } => COMMAND_TAG_RESOURCE_MOVE,
        Command::ResourceMoveMany { .. } => COMMAND_TAG_RESOURCE_MOVE_MANY,
        Command::ResourceRename { .. } => COMMAND_TAG_RESOURCE_RENAME,
        Command::ResourceTerminate { .. } => COMMAND_TAG_RESOURCE_TERMINATE,
        Command::ResourceRemove { .. } => COMMAND_TAG_RESOURCE_REMOVE,
        Command::ResourceRelaunch { .. } => COMMAND_TAG_RESOURCE_RELAUNCH,
        Command::InboxResolve { .. } => COMMAND_TAG_INBOX_RESOLVE,
        Command::OrganizationUndo { .. } => COMMAND_TAG_ORGANIZATION_UNDO,
        Command::ArrangementGet { .. } => COMMAND_TAG_ARRANGEMENT_GET,
        Command::ArrangementReplace { .. } => COMMAND_TAG_ARRANGEMENT_REPLACE,
        Command::DelegateAgent { .. } => super::COMMAND_TAG_DELEGATE_AGENT,
        Command::RevokeDelegation { .. } => super::COMMAND_TAG_REVOKE_DELEGATION,
        _ => return None,
    })
}

/// Write an organization command: its nested tag, then its field-tagged
/// body. `false`, writing nothing, for a command that is not an
/// organization command.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization command, kept as one flat encode table"
)]
pub(in crate::wire) fn encode_org_command(command: &Command, enc: &mut Encoder<'_>) -> bool {
    let Some(tag) = org_command_tag(command) else {
        return false;
    };
    enc.write_u8(tag);
    match command {
        Command::GetOrganization {
            filter,
            page_size,
            cursor,
        } => {
            enc.write_field_with(field::org::FILTER, |e| encode_filter(*filter, e));
            write_u32(enc, field::org::PAGE_SIZE, *page_size);
            if let Some(cursor) = cursor {
                enc.write_field(field::org::CURSOR, cursor);
            }
        }
        Command::SessionCreate {
            operation,
            name,
            lifetime,
            attrs,
            launch,
        } => {
            write_operation(enc, operation);
            write_name(enc, name);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
            for pair in attrs {
                encode_pair(enc, field::org::ATTR, pair);
            }
            if let Some(launch) = launch {
                enc.write_field_with(field::org::LAUNCH, |e| encode_launch(launch, e));
            }
        }
        Command::SessionRename {
            operation,
            session,
            expected,
            name,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u64(enc, field::org::EXPECTED, expected.get());
            write_name(enc, name);
        }
        Command::SessionSetLifetime {
            operation,
            session,
            expected,
            lifetime,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u64(enc, field::org::EXPECTED, expected.get());
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
        }
        Command::SessionDelete {
            operation,
            session,
            expected,
        }
        | Command::InboxResolve {
            operation,
            session,
            expected,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u64(enc, field::org::EXPECTED, expected.get());
        }
        Command::ResourceCreate {
            operation,
            session,
            kind,
            launch,
            name,
            on_exit,
            launch_mode,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            if let Some(name) = name {
                write_name(enc, name);
            }
            if let Some(launch) = launch {
                enc.write_field_with(field::org::LAUNCH, |e| encode_launch(launch, e));
            }
            write_u8(enc, field::org::KIND, kind.as_wire());
            write_u8(enc, field::org::ON_EXIT, on_exit.to_u8());
            if *launch_mode != LaunchMode::Immediate {
                write_u8(enc, field::org::LAUNCH_MODE, launch_mode.to_u8());
            }
        }
        Command::ResourceMove {
            operation,
            resource,
            from,
            from_expected,
            to,
            to_expected,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
            write_session(enc, field::org::FROM, from);
            write_u64(enc, field::org::FROM_REVISION, from_expected.get());
            write_session(enc, field::org::TO, to);
            write_u64(enc, field::org::TO_REVISION, to_expected.get());
        }
        Command::ResourceMoveMany {
            operation,
            items,
            to,
            to_expected,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::TO, to);
            write_u64(enc, field::org::TO_REVISION, to_expected.get());
            for item in items {
                encode_move_item(item, enc);
            }
        }
        Command::ResourceRename {
            operation,
            resource,
            expected,
            name,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
            write_u64(enc, field::org::EXPECTED, expected.get());
            if let Some(name) = name {
                write_name(enc, name);
            }
        }
        Command::ResourceTerminate {
            operation,
            target,
            mode,
        } => {
            write_operation(enc, operation);
            enc.write_field_with(field::org::TARGET, |e| encode_live_target(target, e));
            write_u8(enc, field::org::MODE, mode.to_u8());
        }
        Command::ResourceRemove {
            operation,
            resource,
            expected,
        }
        | Command::ResourceRelaunch {
            operation,
            resource,
            expected,
        } => {
            write_operation(enc, operation);
            write_resource(enc, resource);
            write_u64(enc, field::org::EXPECTED, expected.get());
        }
        Command::OrganizationUndo {
            operation,
            undo,
            expected,
        } => {
            write_operation(enc, operation);
            write_key(enc, field::org::UNDO, undo.as_bytes());
            for pin in expected {
                encode_pin(pin, enc);
            }
        }
        Command::ArrangementGet { arrangement } => {
            write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
        }
        Command::ArrangementReplace {
            operation,
            arrangement,
            expected,
            document,
        } => {
            write_operation(enc, operation);
            write_u64(enc, field::org::EXPECTED, expected.get());
            write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
            write_document(enc, document);
        }
        Command::DelegateAgent {
            operation,
            session,
            ttl_secs,
        } => {
            write_operation(enc, operation);
            write_session(enc, field::org::SESSION, session);
            write_u32(enc, field::org::TTL_SECS, *ttl_secs);
        }
        Command::RevokeDelegation {
            operation,
            credential_id,
        } => {
            write_operation(enc, operation);
            enc.write_field(field::org::CREDENTIAL, credential_id.as_bytes());
        }
        _ => return false,
    }
    true
}

/// Decode the organization command with nested tag `tag` from its
/// field-tagged body, or `None` (reading nothing) for another tag.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization command, kept as one flat decode table"
)]
pub(in crate::wire) fn decode_org_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    use super::{
        COMMAND_TAG_ARRANGEMENT_GET, COMMAND_TAG_ARRANGEMENT_REPLACE, COMMAND_TAG_GET_ORGANIZATION,
        COMMAND_TAG_INBOX_RESOLVE, COMMAND_TAG_ORGANIZATION_UNDO, COMMAND_TAG_RESOURCE_CREATE,
        COMMAND_TAG_RESOURCE_MOVE, COMMAND_TAG_RESOURCE_MOVE_MANY, COMMAND_TAG_RESOURCE_RELAUNCH,
        COMMAND_TAG_RESOURCE_REMOVE, COMMAND_TAG_RESOURCE_RENAME, COMMAND_TAG_RESOURCE_TERMINATE,
        COMMAND_TAG_SESSION_CREATE, COMMAND_TAG_SESSION_DELETE, COMMAND_TAG_SESSION_RENAME,
        COMMAND_TAG_SESSION_SET_LIFETIME,
    };
    let delegation = [
        super::COMMAND_TAG_DELEGATE_AGENT,
        super::COMMAND_TAG_REVOKE_DELEGATION,
    ];
    if !(COMMAND_TAG_GET_ORGANIZATION..=COMMAND_TAG_ARRANGEMENT_REPLACE).contains(&tag)
        && !delegation.contains(&tag)
    {
        return Ok(None);
    }
    let f = Fields::read_from(dec)?;
    let command = match tag {
        COMMAND_TAG_GET_ORGANIZATION => Command::GetOrganization {
            filter: f.filter.unwrap_or_default(),
            page_size: f.page_size.unwrap_or(0),
            cursor: f.cursor,
        },
        COMMAND_TAG_SESSION_CREATE => Command::SessionCreate {
            operation: f.operation()?,
            lifetime: f.lifetime()?.unwrap_or_default(),
            name: req(f.name)?,
            attrs: f.attrs,
            launch: f.launch,
        },
        COMMAND_TAG_SESSION_RENAME => Command::SessionRename {
            operation: f.operation()?,
            session: f.session()?,
            expected: f.expected()?,
            name: req(f.name)?,
        },
        COMMAND_TAG_SESSION_SET_LIFETIME => Command::SessionSetLifetime {
            operation: f.operation()?,
            session: f.session()?,
            expected: f.expected()?,
            lifetime: req(f.lifetime()?)?,
        },
        COMMAND_TAG_SESSION_DELETE => Command::SessionDelete {
            operation: f.operation()?,
            session: f.session()?,
            expected: f.expected()?,
        },
        COMMAND_TAG_RESOURCE_CREATE => Command::ResourceCreate {
            operation: f.operation()?,
            session: f.session()?,
            on_exit: f.on_exit()?.unwrap_or_default(),
            kind: f.kind.unwrap_or_default(),
            launch_mode: f.launch_mode()?.unwrap_or_default(),
            launch: f.launch,
            name: f.name,
        },
        COMMAND_TAG_RESOURCE_MOVE => Command::ResourceMove {
            operation: f.operation()?,
            resource: f.resource()?,
            from: req(f.from)?,
            from_expected: req(f.from_revision)?,
            to: req(f.to)?,
            to_expected: req(f.to_revision)?,
        },
        COMMAND_TAG_RESOURCE_MOVE_MANY => Command::ResourceMoveMany {
            operation: f.operation()?,
            to: req(f.to)?,
            to_expected: req(f.to_revision)?,
            items: f.move_items,
        },
        COMMAND_TAG_RESOURCE_RENAME => Command::ResourceRename {
            operation: f.operation()?,
            resource: f.resource()?,
            expected: f.expected()?,
            name: f.name,
        },
        COMMAND_TAG_RESOURCE_TERMINATE => Command::ResourceTerminate {
            operation: f.operation()?,
            target: req(f.target)?,
            mode: enum_byte("TerminateMode", f.mode, TerminateMode::from_u8)?.unwrap_or_default(),
        },
        COMMAND_TAG_RESOURCE_REMOVE => Command::ResourceRemove {
            operation: f.operation()?,
            resource: f.resource()?,
            expected: f.expected()?,
        },
        COMMAND_TAG_RESOURCE_RELAUNCH => Command::ResourceRelaunch {
            operation: f.operation()?,
            resource: f.resource()?,
            expected: f.expected()?,
        },
        COMMAND_TAG_INBOX_RESOLVE => Command::InboxResolve {
            operation: f.operation()?,
            session: f.session()?,
            expected: f.expected()?,
        },
        COMMAND_TAG_ORGANIZATION_UNDO => Command::OrganizationUndo {
            operation: f.operation()?,
            undo: req(f.undo)?,
            expected: f.pins,
        },
        COMMAND_TAG_ARRANGEMENT_GET => Command::ArrangementGet {
            arrangement: req(f.arrangement)?,
        },
        COMMAND_TAG_ARRANGEMENT_REPLACE => Command::ArrangementReplace {
            operation: f.operation()?,
            arrangement: req(f.arrangement)?,
            expected: f.expected()?,
            document: req(f.document)?,
        },
        super::COMMAND_TAG_DELEGATE_AGENT => Command::DelegateAgent {
            operation: f.operation()?,
            session: f.session()?,
            ttl_secs: f.ttl_secs.unwrap_or(0),
        },
        super::COMMAND_TAG_REVOKE_DELEGATION => Command::RevokeDelegation {
            operation: f.operation()?,
            credential_id: req(f.credential)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

// ---------------------------------------------------------------------------
// Results and values.
// ---------------------------------------------------------------------------

/// Write a refusal: `tag: u8 || body: bytes` (`u32` length), the body
/// positional per tag, so an unknown tag is skipped whole.
pub(in crate::wire) fn encode_refusal(refusal: &Refusal, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    let tag = {
        let mut e = Encoder::new(&mut body);
        match refusal {
            Refusal::Unauthorized { verb } => {
                e.write_u8(verb.bit());
                REFUSAL_UNAUTHORIZED
            }
            Refusal::NotFound => REFUSAL_NOT_FOUND,
            Refusal::Conflict { current } => {
                e.write_u64_be(current.get());
                REFUSAL_CONFLICT
            }
            Refusal::Stale { current } => {
                e.write_option(*current, |e, id| e.write_u64_be(id.get()));
                REFUSAL_STALE
            }
            Refusal::NotEmpty { members } => {
                e.write_u32_be(*members);
                REFUSAL_NOT_EMPTY
            }
            Refusal::Unsupported { capability } => {
                e.write_str(capability);
                REFUSAL_UNSUPPORTED
            }
            Refusal::InvalidArgument { field, reason } => {
                e.write_str(field);
                e.write_str(reason);
                REFUSAL_INVALID_ARGUMENT
            }
            Refusal::NameTaken => REFUSAL_NAME_TAKEN,
            Refusal::ExecutionUncertain => REFUSAL_EXECUTION_UNCERTAIN,
            Refusal::LeaseHeld { holder } => {
                encode_actor_ref(holder, &mut e);
                REFUSAL_LEASE_HELD
            }
            Refusal::DuplicateOperation => REFUSAL_DUPLICATE_OPERATION,
            Refusal::StoreFailure { reason } => {
                e.write_str(reason);
                REFUSAL_STORE_FAILURE
            }
            Refusal::Unknown { tag, body: raw } => {
                for byte in raw {
                    e.write_u8(*byte);
                }
                *tag
            }
        }
    };
    enc.write_u8(tag);
    enc.write_bytes(&body);
}

/// Read a refusal; an unknown tag, or a known tag whose body names an
/// unknown verb, decodes as [`Refusal::Unknown`].
pub(in crate::wire) fn decode_refusal(dec: &mut Decoder<'_>) -> Result<Refusal, DecodeError> {
    let tag = dec.read_u8()?;
    let body = dec.read_bytes()?;
    let mut b = Decoder::new(body);
    let unknown = || Refusal::Unknown {
        tag,
        body: body.to_vec(),
    };
    let refusal = match tag {
        REFUSAL_UNAUTHORIZED => {
            let bit = b.read_u8()?;
            Verb::ALL
                .into_iter()
                .find(|verb| verb.bit() == bit)
                .map_or_else(unknown, |verb| Refusal::Unauthorized { verb })
        }
        REFUSAL_NOT_FOUND => Refusal::NotFound,
        REFUSAL_CONFLICT => Refusal::Conflict {
            current: Revision::new(b.read_u64_be()?),
        },
        REFUSAL_STALE => Refusal::Stale {
            current: b.read_option("Option<ActivationId> tag", |d| {
                Ok(ActivationId::new(d.read_u64_be()?))
            })?,
        },
        REFUSAL_NOT_EMPTY => Refusal::NotEmpty {
            members: b.read_u32_be()?,
        },
        REFUSAL_UNSUPPORTED => Refusal::Unsupported {
            capability: b.read_str()?.to_owned(),
        },
        REFUSAL_INVALID_ARGUMENT => Refusal::InvalidArgument {
            field: b.read_str()?.to_owned(),
            reason: b.read_str()?.to_owned(),
        },
        REFUSAL_NAME_TAKEN => Refusal::NameTaken,
        REFUSAL_EXECUTION_UNCERTAIN => Refusal::ExecutionUncertain,
        REFUSAL_LEASE_HELD => Refusal::LeaseHeld {
            holder: decode_actor_ref(&mut b)?,
        },
        REFUSAL_DUPLICATE_OPERATION => Refusal::DuplicateOperation,
        REFUSAL_STORE_FAILURE => Refusal::StoreFailure {
            reason: b.read_str()?.to_owned(),
        },
        _ => unknown(),
    };
    Ok(refusal)
}

fn encode_session_record(record: &SessionRecord, enc: &mut Encoder<'_>) {
    write_session(enc, field::org::SESSION, &record.key);
    write_name(enc, &record.name);
    write_u8(enc, field::org::LIFETIME, record.lifetime.to_u8());
    for pair in &record.attrs {
        encode_pair(enc, field::org::ATTR, pair);
    }
    if let Some(arrangement) = record.arrangement {
        write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
    }
    write_u64(enc, field::org::REVISION, record.revision.get());
    if let Some(live) = record.live {
        write_u32(enc, field::org::LIVE_SESSION, live.get());
    }
    if record.awaiting_organization {
        write_u8(enc, field::org::AWAITING, 1);
    }
    write_u32(enc, field::org::MEMBERS, record.members);
}

fn decode_session_record(value: &[u8]) -> Result<SessionRecord, DecodeError> {
    let f = Fields::read(value)?;
    Ok(SessionRecord {
        key: f.session()?,
        lifetime: f.lifetime()?.unwrap_or_default(),
        name: req(f.name)?,
        awaiting_organization: f.awaiting.unwrap_or(false),
        arrangement: f.arrangement,
        attrs: f.attrs,
        members: f.members.unwrap_or(0),
        revision: req(f.revision)?,
        live: f.live_session,
    })
}

fn encode_resource_record(record: &ResourceRecord, enc: &mut Encoder<'_>) {
    write_session(enc, field::org::SESSION, &record.session);
    write_resource(enc, &record.key);
    if let Some(name) = &record.name {
        write_name(enc, name);
    }
    write_u8(enc, field::org::KIND, record.kind.as_wire());
    write_u8(enc, field::org::ON_EXIT, record.on_exit.to_u8());
    if let Some(activation) = &record.activation {
        write_u64(enc, field::org::ACTIVATION, activation.id.get());
    }
    write_u64(enc, field::org::REVISION, record.revision.get());
    if let Some(live) = &record.live {
        enc.write_field_with(field::org::LIVE_RESOURCE, |e| encode_terminal_id(live, e));
    }
    if let Some(activation) = &record.activation {
        write_u8(enc, field::org::STATE, activation.state.to_u8());
        if let Some(outcome) = activation.outcome {
            enc.write_field_with(field::org::OUTCOME, |e| encode_outcome(outcome, e));
        }
        write_key(enc, field::org::INSTANCE, activation.instance.as_bytes());
    }
}

fn decode_resource_record(value: &[u8]) -> Result<ResourceRecord, DecodeError> {
    let f = Fields::read(value)?;
    let state = f.state()?;
    let activation = match (f.activation, f.instance, state) {
        (Some(id), Some(instance), Some(state)) => Some(ActivationInfo {
            id,
            instance,
            state,
            outcome: f.outcome,
        }),
        (None, _, _) => None,
        _ => return Err(DecodeError::UnexpectedEof),
    };
    Ok(ResourceRecord {
        key: f.resource()?,
        session: f.session()?,
        on_exit: f.on_exit()?.unwrap_or_default(),
        name: f.name,
        kind: f.kind.unwrap_or_default(),
        activation,
        revision: req(f.revision)?,
        live: f.live_resource,
    })
}

/// Write an `ORGANIZATION_PAGE` value body (field-tagged, `field::org_page`).
pub(in crate::wire) fn encode_organization_page(page: &OrganizationPage, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        write_key(
            &mut e,
            field::org_page::AUTHORITY,
            page.authority.as_bytes(),
        );
        if let Some(inbox) = page.inbox {
            write_key(&mut e, field::org_page::INBOX, inbox.as_bytes());
        }
        for record in &page.sessions {
            e.write_field_with(field::org_page::SESSION, |e| {
                encode_session_record(record, e);
            });
        }
        for record in &page.resources {
            e.write_field_with(field::org_page::RESOURCE, |e| {
                encode_resource_record(record, e);
            });
        }
        write_u32(&mut e, field::org_page::TOTAL_SESSIONS, page.total_sessions);
        write_u32(
            &mut e,
            field::org_page::TOTAL_RESOURCES,
            page.total_resources,
        );
        write_u32(&mut e, field::org_page::TOTAL_INTAKE, page.total_intake);
        write_u64(
            &mut e,
            field::org_page::SNAPSHOT_REVISION,
            page.snapshot_revision.get(),
        );
        write_u64(&mut e, field::org_page::JOURNAL_SEQ, page.journal_seq);
        if let Some(cursor) = &page.next_cursor {
            e.write_field(field::org_page::NEXT_CURSOR, cursor);
        }
    }
    enc.write_bytes(&body);
}

/// Read an `ORGANIZATION_PAGE` value body.
pub(in crate::wire) fn decode_organization_page(
    dec: &mut Decoder<'_>,
) -> Result<OrganizationPage, DecodeError> {
    let mut body = Decoder::new(dec.read_bytes()?);
    let mut authority = None;
    let mut inbox = None;
    let mut sessions = Vec::new();
    let mut resources = Vec::new();
    let mut totals = (0, 0, 0);
    let mut snapshot_revision = Revision::new(0);
    let mut journal_seq = 0;
    let mut next_cursor = None;
    while let Some((id, value)) = body.read_field()? {
        match id {
            field::org_page::AUTHORITY => authority = Some(read_key(value, AuthorityId::new)?),
            field::org_page::INBOX => inbox = Some(read_key(value, SessionKey::new)?),
            field::org_page::SESSION => sessions.push(decode_session_record(value)?),
            field::org_page::RESOURCE => resources.push(decode_resource_record(value)?),
            field::org_page::TOTAL_SESSIONS => totals.0 = read_u32(value)?,
            field::org_page::TOTAL_RESOURCES => totals.1 = read_u32(value)?,
            field::org_page::TOTAL_INTAKE => totals.2 = read_u32(value)?,
            field::org_page::SNAPSHOT_REVISION => {
                snapshot_revision = Revision::new(read_u64(value)?);
            }
            field::org_page::JOURNAL_SEQ => journal_seq = read_u64(value)?,
            field::org_page::NEXT_CURSOR => {
                if value.len() > MAX_ORG_CURSOR_BYTES {
                    return Err(DecodeError::OrganizationLimitExceeded);
                }
                next_cursor = Some(value.to_vec());
            }
            _ => {}
        }
    }
    Ok(
        OrganizationPage::new(req(authority)?, snapshot_revision, journal_seq)
            .with_inbox(inbox)
            .with_sessions(sessions)
            .with_resources(resources)
            .with_totals(totals.0, totals.1, totals.2)
            .with_next_cursor(next_cursor),
    )
}

/// Write an `ARRANGEMENT` value body (field-tagged, `field::org`).
pub(in crate::wire) fn encode_arrangement_value(value: &ArrangementValue, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        write_owner(&mut e, value.owner);
        write_key(&mut e, field::org::ARRANGEMENT, value.key.as_bytes());
        write_document(&mut e, &value.document);
        write_u64(&mut e, field::org::REVISION, value.revision.get());
    }
    enc.write_bytes(&body);
}

/// Read an `ARRANGEMENT` value body.
pub(in crate::wire) fn decode_arrangement_value(
    dec: &mut Decoder<'_>,
) -> Result<ArrangementValue, DecodeError> {
    let f = Fields::read(dec.read_bytes()?)?;
    Ok(ArrangementValue {
        key: req(f.arrangement)?,
        owner: f.owner()?,
        revision: req(f.revision)?,
        document: req(f.document)?,
    })
}

/// Write an arrangement's owner: its `SESSION` or `WORKSPACE` field.
fn write_owner(enc: &mut Encoder<'_>, owner: ArrangementOwner) {
    match owner {
        ArrangementOwner::Session(session) => write_session(enc, field::org::SESSION, &session),
        ArrangementOwner::Workspace(workspace) => {
            write_key(enc, field::org::WORKSPACE, workspace.as_bytes());
        }
    }
}

/// Write a `COMMITTED` value body (field-tagged, `field::org`).
pub(in crate::wire) fn encode_committed(committed: &Committed, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        write_operation(&mut e, &committed.operation);
        if let Some(session) = &committed.session {
            write_session(&mut e, field::org::SESSION, session);
        }
        if let Some(resource) = &committed.resource {
            write_resource(&mut e, resource);
        }
        if let Some(target) = &committed.target {
            e.write_field_with(field::org::TARGET, |e| encode_live_target(target, e));
        }
        for pin in &committed.revisions {
            encode_pin(pin, &mut e);
        }
        write_u64(&mut e, field::org::REVISION, committed.revision.get());
        if let Some(prior) = committed.prior_awaiting {
            write_u8(&mut e, field::org::AWAITING, u8::from(prior));
        }
    }
    enc.write_bytes(&body);
}

/// Read a `COMMITTED` value body.
pub(in crate::wire) fn decode_committed(dec: &mut Decoder<'_>) -> Result<Committed, DecodeError> {
    let f = Fields::read(dec.read_bytes()?)?;
    Ok(Committed {
        operation: f.operation()?,
        revision: req(f.revision)?,
        session: f.session,
        resource: f.resource,
        target: f.target,
        revisions: f.pins,
        prior_awaiting: f.awaiting,
    })
}

/// Write a `DELEGATION` value body: `u32`-length-prefixed `field::org`
/// fields.
pub(in crate::wire) fn encode_delegation(delegation: &Delegation, enc: &mut Encoder<'_>) {
    let mut body = BytesMut::new();
    {
        let mut e = Encoder::new(&mut body);
        e.write_field(field::org::CREDENTIAL, delegation.credential_id.as_bytes());
        write_session(&mut e, field::org::SESSION, &delegation.session);
        e.write_field(field::org::SOCKET_PATH, delegation.socket_path.as_bytes());
        write_u64(&mut e, field::org::EXPIRES_AT, delegation.expires_at_ms);
    }
    enc.write_bytes(&body);
}

/// Read a `DELEGATION` value body.
pub(in crate::wire) fn decode_delegation(dec: &mut Decoder<'_>) -> Result<Delegation, DecodeError> {
    let f = Fields::read(dec.read_bytes()?)?;
    Ok(Delegation {
        session: f.session()?,
        credential_id: req(f.credential)?,
        socket_path: req(f.socket_path)?,
        expires_at_ms: req(f.expires_at)?,
    })
}

// ---------------------------------------------------------------------------
// Events.
// ---------------------------------------------------------------------------

/// Write an organization event's field-tagged body and return its tag, or
/// `None` for an event that is not an organization event.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization event, kept as one flat encode table"
)]
pub(in crate::wire) fn encode_org_event(event: &AgentEvent, enc: &mut Encoder<'_>) -> Option<u8> {
    let tag = match event {
        AgentEvent::SessionCreated {
            session,
            name,
            lifetime,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_CREATED
        }
        AgentEvent::SessionRenamed {
            session,
            name,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_RENAMED
        }
        AgentEvent::SessionDeleted {
            session,
            name,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_name(enc, name);
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_DELETED
        }
        AgentEvent::SessionLifetimeChanged {
            session,
            lifetime,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_u8(enc, field::org::LIFETIME, lifetime.to_u8());
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_SESSION_LIFETIME_CHANGED
        }
        AgentEvent::MembershipChanged {
            resource,
            from,
            to,
            from_revision,
            to_revision,
        } => {
            write_resource(enc, resource);
            write_session(enc, field::org::FROM, from);
            write_u64(enc, field::org::FROM_REVISION, from_revision.get());
            write_session(enc, field::org::TO, to);
            write_u64(enc, field::org::TO_REVISION, to_revision.get());
            EVENT_TAG_MEMBERSHIP_CHANGED
        }
        AgentEvent::ResourceCreated {
            resource,
            session,
            kind,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_resource(enc, resource);
            write_u8(enc, field::org::KIND, kind.as_wire());
            EVENT_TAG_RESOURCE_CREATED
        }
        AgentEvent::ResourceRemoved {
            resource,
            session,
            kind,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_resource(enc, resource);
            write_u8(enc, field::org::KIND, kind.as_wire());
            EVENT_TAG_RESOURCE_REMOVED
        }
        AgentEvent::ActivationChanged {
            resource,
            activation,
            state,
            outcome,
        } => {
            write_resource(enc, resource);
            write_u64(enc, field::org::ACTIVATION, activation.get());
            write_u8(enc, field::org::STATE, state.to_u8());
            if let Some(outcome) = outcome {
                enc.write_field_with(field::org::OUTCOME, |e| encode_outcome(*outcome, e));
            }
            EVENT_TAG_ACTIVATION_CHANGED
        }
        AgentEvent::ArrangementChanged {
            arrangement,
            owner,
            revision,
        } => {
            write_owner(enc, *owner);
            write_key(enc, field::org::ARRANGEMENT, arrangement.as_bytes());
            if let Some(revision) = revision {
                write_u64(enc, field::org::REVISION, revision.get());
            }
            EVENT_TAG_ARRANGEMENT_CHANGED
        }
        AgentEvent::ResourceRenamed {
            resource,
            name,
            revision,
        } => {
            write_resource(enc, resource);
            if let Some(name) = name {
                write_name(enc, name);
            }
            write_u64(enc, field::org::REVISION, revision.get());
            EVENT_TAG_RESOURCE_RENAMED
        }
        AgentEvent::LeaseChanged {
            resource,
            epoch,
            holder,
        } => {
            write_resource(enc, resource);
            write_u64(enc, field::org::EPOCH, epoch.get());
            if let Some(holder) = holder {
                write_actor(enc, field::org::HOLDER, holder);
            }
            EVENT_TAG_LEASE_CHANGED
        }
        AgentEvent::IntakeChanged {
            session,
            awaiting_organization,
            revision,
        } => {
            write_session(enc, field::org::SESSION, session);
            write_u64(enc, field::org::REVISION, revision.get());
            write_u8(enc, field::org::AWAITING, u8::from(*awaiting_organization));
            EVENT_TAG_INTAKE_CHANGED
        }
        _ => return None,
    };
    Some(tag)
}

/// Decode an organization event body for `tag`. `None` for a tag that is
/// not an organization event, or a body that is malformed or names an
/// unknown enum value; the caller keeps either as [`AgentEvent::Unknown`].
pub(in crate::wire) fn decode_org_event(tag: u8, body: &[u8]) -> Option<AgentEvent> {
    (EVENT_TAG_SESSION_CREATED..=EVENT_TAG_RESOURCE_RENAMED)
        .contains(&tag)
        .then(|| org_event(tag, body).ok())
        .flatten()
}

fn org_event(tag: u8, body: &[u8]) -> Result<AgentEvent, DecodeError> {
    let f = Fields::read(body)?;
    let event = match tag {
        EVENT_TAG_SESSION_CREATED => AgentEvent::SessionCreated {
            session: f.session()?,
            lifetime: f.lifetime()?.unwrap_or_default(),
            revision: req(f.revision)?,
            name: req(f.name)?,
        },
        EVENT_TAG_SESSION_RENAMED => AgentEvent::SessionRenamed {
            session: f.session()?,
            revision: req(f.revision)?,
            name: req(f.name)?,
        },
        EVENT_TAG_SESSION_DELETED => AgentEvent::SessionDeleted {
            session: f.session()?,
            revision: req(f.revision)?,
            name: req(f.name)?,
        },
        EVENT_TAG_SESSION_LIFETIME_CHANGED => AgentEvent::SessionLifetimeChanged {
            session: f.session()?,
            lifetime: req(f.lifetime()?)?,
            revision: req(f.revision)?,
        },
        EVENT_TAG_MEMBERSHIP_CHANGED => AgentEvent::MembershipChanged {
            resource: f.resource()?,
            from: req(f.from)?,
            to: req(f.to)?,
            from_revision: req(f.from_revision)?,
            to_revision: req(f.to_revision)?,
        },
        EVENT_TAG_RESOURCE_CREATED => AgentEvent::ResourceCreated {
            resource: f.resource()?,
            session: f.session()?,
            kind: f.kind.unwrap_or_default(),
        },
        EVENT_TAG_RESOURCE_REMOVED => AgentEvent::ResourceRemoved {
            resource: f.resource()?,
            session: f.session()?,
            kind: f.kind.unwrap_or_default(),
        },
        EVENT_TAG_ACTIVATION_CHANGED => AgentEvent::ActivationChanged {
            resource: f.resource()?,
            state: req(f.state()?)?,
            activation: req(f.activation)?,
            outcome: f.outcome,
        },
        EVENT_TAG_ARRANGEMENT_CHANGED => AgentEvent::ArrangementChanged {
            arrangement: req(f.arrangement)?,
            owner: f.owner()?,
            revision: f.revision,
        },
        EVENT_TAG_RESOURCE_RENAMED => AgentEvent::ResourceRenamed {
            resource: f.resource()?,
            revision: req(f.revision)?,
            name: f.name,
        },
        EVENT_TAG_LEASE_CHANGED => AgentEvent::LeaseChanged {
            resource: f.resource()?,
            epoch: req(f.epoch)?,
            holder: f.holder,
        },
        EVENT_TAG_INTAKE_CHANGED => AgentEvent::IntakeChanged {
            session: f.session()?,
            awaiting_organization: req(f.awaiting)?,
            revision: req(f.revision)?,
        },
        _ => return Err(DecodeError::unknown_enum("AgentEvent", tag)),
    };
    Ok(event)
}
