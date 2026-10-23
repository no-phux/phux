//! Organization records and replies: session and resource records, pages, arrangement values, commits, and delegations.

use super::{
    ActivationId, ArrangementDoc, ArrangementKey, ArrangementOwner, AuthorityId, ExecState,
    ExitPolicy, LiveTarget, OperationId, Outcome, RecordRevision, ResourceId, ResourceKey,
    ResourceKind, Revision, ServerInstance, SessionId, SessionKey, SessionLifetime,
};

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
    /// Whether it is imported work awaiting organization (Intake).
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
    /// Safe launch diagnostic, optional for older peers.
    pub diagnostic: Option<crate::api::LaunchDiagnostic>,
    /// In-memory executor evidence awaits durable commit.
    pub persistence_pending: bool,
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
            diagnostic: None,
            persistence_pending: false,
        }
    }

    /// Builder setter for [`Self::outcome`].
    #[must_use]
    pub const fn with_outcome(mut self, outcome: Option<Outcome>) -> Self {
        self.outcome = outcome;
        self
    }

    /// Attach safe execution evidence and persistence uncertainty.
    #[must_use]
    pub const fn with_evidence(
        mut self,
        diagnostic: Option<crate::api::LaunchDiagnostic>,
        pending: bool,
    ) -> Self {
        self.diagnostic = diagnostic;
        self.persistence_pending = pending;
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
    /// The designated Intake session; legacy field name and wire tag retained.
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

    /// The designated Intake session. The `inbox` field is a legacy wire/API name.
    #[must_use]
    pub const fn intake(&self) -> Option<SessionKey> {
        self.inbox
    }

    /// Set the designated Intake session without changing its wire field tag.
    #[must_use]
    pub const fn with_intake(mut self, intake: Option<SessionKey>) -> Self {
        self.inbox = intake;
        self
    }

    /// Legacy alias for [`Self::with_intake`].
    #[must_use]
    pub const fn with_inbox(self, inbox: Option<SessionKey>) -> Self {
        self.with_intake(inbox)
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
    /// How many members a `SESSION_KILL` ended; absent on a replayed one.
    pub members: Option<u32>,
    /// The arrangement edited by a placed create.
    pub arrangement: Option<ArrangementKey>,
    /// The stable entry created there.
    pub entry: Option<[u8; 16]>,
    /// Current execution observation, not a promise of completion.
    pub activation: Option<ActivationInfo>,
    /// Whether the retained operation was replayed without new effects.
    pub replayed: bool,
    /// Durable authority of a placed creation.
    pub authority: Option<AuthorityId>,
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
            members: None,
            arrangement: None,
            entry: None,
            activation: None,
            replayed: false,
            authority: None,
        }
    }

    /// Include the placed create's durable presentation identity and current execution.
    #[must_use]
    pub const fn with_creation(
        mut self,
        authority: Option<AuthorityId>,
        arrangement: Option<ArrangementKey>,
        entry: Option<[u8; 16]>,
        activation: Option<ActivationInfo>,
    ) -> Self {
        self.authority = authority;
        self.arrangement = arrangement;
        self.entry = entry;
        self.activation = activation;
        self
    }

    /// Mark a replay of a retained operation.
    #[must_use]
    pub const fn with_replayed(mut self, replayed: bool) -> Self {
        self.replayed = replayed;
        self
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

    /// Builder setter for [`Self::members`].
    #[must_use]
    pub const fn with_members(mut self, members: Option<u32>) -> Self {
        self.members = members;
        self
    }
}
