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

    builder_setters! {
        const fn with_lifetime(lifetime: SessionLifetime) => lifetime;
        const fn with_awaiting_organization(awaiting: bool) => awaiting_organization;
        const fn with_arrangement(arrangement: Option<ArrangementKey>) => arrangement;
    }

    /// Builder setter for [`Self::attrs`].
    #[must_use]
    pub fn with_attrs(mut self, attrs: Vec<(String, String)>) -> Self {
        self.attrs = attrs;
        self
    }

    builder_setters! {
        const fn with_members(members: u32) => members;
        const fn with_live(live: Option<SessionId>) => live;
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
    /// A server stop cut the launch off before any process was recorded,
    /// unlike a launch that failed. Never set by older servers.
    pub launch_interrupted: bool,
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
            launch_interrupted: false,
        }
    }

    builder_setters! {
        const fn with_outcome(outcome: Option<Outcome>) => outcome;
        const fn with_launch_interrupted(interrupted: bool) => launch_interrupted;
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

    /// Whether a server stop ended it, so `phux restore` relaunches it: its
    /// program ended unobserved, or (a Terminal) its launch was cut off. A
    /// launch that failed is neither.
    #[must_use]
    pub fn ended_by_restart(&self) -> bool {
        self.activation.is_some_and(|a| {
            (a.state == ExecState::Completed && a.outcome == Some(Outcome::Unobserved))
                || (a.launch_interrupted
                    && a.state == ExecState::Failed
                    && self.kind == ResourceKind::Terminal)
        })
    }

    builder_setters! {
        fn with_name(name: Option<String>) => name;
        const fn with_kind(kind: ResourceKind) => kind;
        const fn with_on_exit(on_exit: ExitPolicy) => on_exit;
        const fn with_activation(activation: Option<ActivationInfo>) => activation;
        fn with_live(live: Option<ResourceId>) => live;
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

    builder_setters! {
        /// Set the designated Intake session without changing its wire field tag.
        const fn with_intake(intake: Option<SessionKey>) => inbox;
    }

    /// Legacy alias for [`Self::with_intake`].
    #[must_use]
    pub const fn with_inbox(self, inbox: Option<SessionKey>) -> Self {
        self.with_intake(inbox)
    }

    builder_setters! {
        fn with_sessions(sessions: Vec<SessionRecord>) => sessions;
        fn with_resources(resources: Vec<ResourceRecord>) => resources;
    }

    /// Builder setter for the three totals.
    #[must_use]
    pub const fn with_totals(mut self, sessions: u32, resources: u32, intake: u32) -> Self {
        self.total_sessions = sessions;
        self.total_resources = resources;
        self.total_intake = intake;
        self
    }

    builder_setters! {
        fn with_next_cursor(next_cursor: Option<Vec<u8>>) => next_cursor;
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
    /// `Some(true)` for newly created, `Some(false)` for existing.
    pub ensured_created: Option<bool>,
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
            ensured_created: None,
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

    builder_setters! {
        /// Mark a replay of a retained operation.
        const fn with_replayed(replayed: bool) => replayed;
        const fn with_ensured_created(ensured_created: Option<bool>) => ensured_created;
        const fn with_session(session: Option<SessionKey>) => session;
        const fn with_resource(resource: Option<ResourceKey>) => resource;
        fn with_target(target: Option<LiveTarget>) => target;
        fn with_revisions(revisions: Vec<RecordRevision>) => revisions;
        const fn with_prior_awaiting(prior_awaiting: Option<bool>) => prior_awaiting;
        const fn with_members(members: Option<u32>) => members;
    }
}
