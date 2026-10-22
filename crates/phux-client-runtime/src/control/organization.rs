//! The organization replica: sessions, resources, arrangements, and the
//! Intake as one authority reports them (`docs/spec/L1.md` §5.7, CONTRACTS
//! §04-§07).
//!
//! [`OrganizationReplica`] is sans-IO. It is seeded from paged
//! `GET_ORGANIZATION` replies ([`OrganizationReplica::begin_seed`],
//! [`OrganizationReplica::apply_page`]), then kept current by the
//! organization events of the journal, applied in journal order
//! ([`OrganizationReplica::apply_event`]). Events that arrive while a seed is
//! in flight are buffered and replayed past the seed's cut
//! (`OrganizationPage::journal_seq`). A `journal_gap`, or an event that names
//! a record the replica cannot complete from the event alone, asks the
//! control plane for a refetch ([`ReplicaAction`]); the replica never guesses.
//!
//! Live handles come only from `GET_ORGANIZATION`, so the replica's
//! key-to-handle map ([`OrganizationReplica::handles`]) is what translates a
//! stored arrangement into the handle space a client edits.

use std::collections::{HashMap, HashSet};

use phux_client_core::layout::ArrangementDoc as HandleDoc;
use phux_client_core::layout::wire::{EntryBinding, HandleMap, from_protocol};
use phux_client_core::org_ops::is_inactive;
use phux_protocol::ResourceId;
use phux_protocol::ids::{
    ArrangementKey, AuthorityId, ResourceKey, Revision, SessionId, SessionKey,
};
use phux_protocol::wire::frame::{
    ActivationInfo, AgentEvent, ArrangementOwner, ArrangementValue, Command, OrgFilter,
    OrganizationPage, ResourceRecord, SessionRecord,
};

mod journal;
mod seed;

/// Records requested per `GET_ORGANIZATION` page.
pub const ORG_PAGE_SIZE: u32 = 512;

/// Most journal events buffered while a seed is in flight; past it the seed
/// is restarted rather than growing without bound.
const MAX_BUFFERED_EVENTS: usize = 4096;

/// Where the replica is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplicaState {
    /// Never seeded on this server, or the server does not advertise
    /// `ORGANIZATION`.
    #[default]
    Empty,
    /// A paged seed is in flight; events are buffered.
    Seeding,
    /// Seeded and following the journal.
    Live,
    /// The journal skipped events; a reseed is due.
    Stale,
}

/// What the control plane must do after feeding the replica.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplicaAction {
    /// Nothing.
    None,
    /// Records changed; tell the consumer.
    Changed,
    /// Send this `GET_ORGANIZATION` (the next page of a seed).
    NextPage(Command),
    /// The seed finished; records changed.
    Seeded,
    /// The journal skipped events: reseed from the first page.
    Reseed,
    /// Re-read one session's records (a record the event could not
    /// complete, such as a new resource's live handle).
    RefetchSession(SessionKey),
    /// Re-read an arrangement the replica holds.
    RefetchArrangement(ArrangementKey),
}

/// The Intake as this owner sees it: the designated Intake session and every
/// session awaiting organization, with their members.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntakeView {
    /// The designated Intake session. Legacy public field name for compatibility.
    pub inbox: Option<SessionKey>,
    /// Sessions in Intake: the designated one first, then those awaiting
    /// organization, in record order.
    pub sessions: Vec<SessionRecord>,
    /// Their members, in record order.
    pub resources: Vec<ResourceRecord>,
}

/// Legacy public spelling of [`IntakeView`].
pub type InboxView = IntakeView;

impl IntakeView {
    /// The designated Intake session, when the caller observes it.
    #[must_use]
    pub const fn intake(&self) -> Option<SessionKey> {
        self.inbox
    }
}

#[derive(Debug, Clone)]
struct Seed {
    filter: OrgFilter,
    page: Option<OrganizationPage>,
    sessions: Vec<SessionRecord>,
    resources: Vec<ResourceRecord>,
}

/// The replica. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct OrganizationReplica {
    state: ReplicaState,
    authority: Option<AuthorityId>,
    intake: Option<SessionKey>,
    sessions: Vec<SessionRecord>,
    resources: Vec<ResourceRecord>,
    arrangements: HashMap<ArrangementKey, ArrangementValue>,
    handles: HandleMap,
    snapshot_revision: Revision,
    journal_seq: u64,
    seed: Option<Seed>,
    buffered: Vec<(Option<u64>, AgentEvent)>,
}

impl OrganizationReplica {
    /// An empty replica.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the replica is.
    #[must_use]
    pub const fn state(&self) -> ReplicaState {
        self.state
    }

    /// Whether the replica is seeded and following the journal.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.state == ReplicaState::Live
    }

    /// The authority the records belong to, once a page named it.
    #[must_use]
    pub const fn authority(&self) -> Option<AuthorityId> {
        self.authority
    }

    /// Legacy alias for [`Self::intake`].
    #[must_use]
    pub const fn inbox(&self) -> Option<SessionKey> {
        self.intake()
    }

    /// The designated Intake session, when the caller observes it.
    #[must_use]
    pub const fn intake(&self) -> Option<SessionKey> {
        self.intake
    }

    /// The journal `seq` the records are current through.
    #[must_use]
    pub const fn journal_seq(&self) -> u64 {
        self.journal_seq
    }

    /// The organization revision the seed was cut at.
    #[must_use]
    pub const fn snapshot_revision(&self) -> Revision {
        self.snapshot_revision
    }

    /// Every session record, in page order.
    #[must_use]
    pub fn sessions(&self) -> &[SessionRecord] {
        &self.sessions
    }

    /// Every resource record, in page order.
    #[must_use]
    pub fn resources(&self) -> &[ResourceRecord] {
        &self.resources
    }

    /// The session record `key` names.
    #[must_use]
    pub fn session(&self, key: &SessionKey) -> Option<&SessionRecord> {
        self.sessions.iter().find(|s| s.key == *key)
    }

    /// The session record whose live handle is `id`.
    #[must_use]
    pub fn session_by_live(&self, id: SessionId) -> Option<&SessionRecord> {
        self.sessions.iter().find(|s| s.live == Some(id))
    }

    /// The resource record `key` names.
    #[must_use]
    pub fn resource(&self, key: &ResourceKey) -> Option<&ResourceRecord> {
        self.resources.iter().find(|r| r.key == *key)
    }

    /// The resource record executing as `id` here.
    #[must_use]
    pub fn resource_by_live(&self, id: &ResourceId) -> Option<&ResourceRecord> {
        self.handles.key(id).and_then(|key| self.resource(&key))
    }

    /// The members of `session`, in record order.
    pub fn members<'a>(
        &'a self,
        session: &'a SessionKey,
    ) -> impl Iterator<Item = &'a ResourceRecord> + 'a {
        self.resources.iter().filter(move |r| r.session == *session)
    }

    /// The key-to-handle map of every resource that executes here.
    #[must_use]
    pub const fn handles(&self) -> &HandleMap {
        &self.handles
    }

    /// The stored arrangement `key` names, when the replica fetched it.
    #[must_use]
    pub fn arrangement(&self, key: &ArrangementKey) -> Option<&ArrangementValue> {
        self.arrangements.get(key)
    }

    /// `session`'s arrangement in handle space, with what writing it back
    /// needs, when the replica holds it.
    #[must_use]
    pub fn arrangement_document(
        &self,
        session: &SessionKey,
    ) -> Option<(ArrangementKey, Revision, HandleDoc, EntryBinding)> {
        let key = self.session(session)?.arrangement?;
        let value = self.arrangements.get(&key)?;
        let (document, binding) = from_protocol(&value.document, &self.handles);
        Some((key, value.revision, document, binding))
    }

    /// Legacy alias for [`Self::intake_view`].
    #[must_use]
    pub fn inbox_view(&self) -> InboxView {
        self.intake_view()
    }

    /// The owner-local Intake: designated session and imported groups with members.
    #[must_use]
    pub fn intake_view(&self) -> IntakeView {
        let mut sessions: Vec<SessionRecord> = Vec::new();
        if let Some(intake) = self.intake.and_then(|key| self.session(&key)) {
            sessions.push(intake.clone());
        }
        sessions.extend(
            self.sessions
                .iter()
                .filter(|s| s.awaiting_organization && Some(s.key) != self.intake)
                .cloned(),
        );
        let keys: HashSet<SessionKey> = sessions.iter().map(|s| s.key).collect();
        IntakeView {
            inbox: self.intake,
            resources: self
                .resources
                .iter()
                .filter(|r| keys.contains(&r.session))
                .cloned()
                .collect(),
            sessions,
        }
    }

    /// Fill session live handles from a `GET_STATE` or `ATTACHED` snapshot,
    /// whose sessions carry their keys.
    pub fn observe_snapshot_keys(
        &mut self,
        pairs: impl IntoIterator<Item = (SessionKey, SessionId)>,
    ) {
        for (key, live) in pairs {
            if let Some(session) = self.sessions.iter_mut().find(|s| s.key == key) {
                session.live = Some(live);
            }
        }
    }
}

/// Whether `event` is one the organization replica follows.
#[must_use]
pub const fn is_organization_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::SessionCreated { .. }
            | AgentEvent::SessionRenamed { .. }
            | AgentEvent::SessionProjectSet { .. }
            | AgentEvent::SessionDeleted { .. }
            | AgentEvent::SessionLifetimeChanged { .. }
            | AgentEvent::MembershipChanged { .. }
            | AgentEvent::ResourceCreated { .. }
            | AgentEvent::ResourceRemoved { .. }
            | AgentEvent::ActivationChanged { .. }
            | AgentEvent::ArrangementChanged { .. }
            | AgentEvent::ResourceRenamed { .. }
            | AgentEvent::LeaseChanged { .. }
            | AgentEvent::IntakeChanged { .. }
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod tests;
