//! The organization replica: sessions, resources, arrangements, and the
//! Inbox as one authority reports them (`docs/spec/L1.md` §5.7, CONTRACTS
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

/// What changed in the records, for the consumer's own bookkeeping (a kill
/// workflow, an Inbox view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordChange {
    /// A resource's execution state changed.
    Activation {
        /// The resource.
        resource: ResourceKey,
        /// Its new activation.
        activation: ActivationInfo,
    },
    /// A resource record was removed.
    ResourceRemoved(ResourceKey),
    /// A session record was deleted.
    SessionDeleted(SessionKey),
    /// Any other change.
    Other,
}

/// The Inbox as this owner sees it: the designated Inbox session and every
/// session awaiting organization, with their members.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InboxView {
    /// The designated Inbox session, when the caller observes it.
    pub inbox: Option<SessionKey>,
    /// Sessions in the Inbox: the designated one first, then those awaiting
    /// organization, in record order.
    pub sessions: Vec<SessionRecord>,
    /// Their members, in record order.
    pub resources: Vec<ResourceRecord>,
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
    inbox: Option<SessionKey>,
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

    /// The designated Inbox session, when the caller observes it.
    #[must_use]
    pub const fn inbox(&self) -> Option<SessionKey> {
        self.inbox
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

    /// The owner-local Inbox: the designated Inbox session and every
    /// session awaiting organization, with their members.
    #[must_use]
    pub fn inbox_view(&self) -> InboxView {
        let mut sessions: Vec<SessionRecord> = Vec::new();
        if let Some(inbox) = self.inbox.and_then(|key| self.session(&key)) {
            sessions.push(inbox.clone());
        }
        sessions.extend(
            self.sessions
                .iter()
                .filter(|s| s.awaiting_organization && Some(s.key) != self.inbox)
                .cloned(),
        );
        let keys: HashSet<SessionKey> = sessions.iter().map(|s| s.key).collect();
        InboxView {
            inbox: self.inbox,
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

    /// Forget everything: a different server, or one without
    /// `ORGANIZATION`.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Start a seed of `filter`: the replica buffers events until it ends.
    /// Returns the first page request.
    pub fn begin_seed(&mut self, filter: OrgFilter) -> Command {
        self.state = ReplicaState::Seeding;
        self.seed = Some(Seed {
            filter,
            page: None,
            sessions: Vec::new(),
            resources: Vec::new(),
        });
        Command::GetOrganization {
            filter,
            page_size: ORG_PAGE_SIZE,
            cursor: None,
        }
    }

    /// Whether a seed is in flight.
    #[must_use]
    pub const fn seeding(&self) -> bool {
        self.seed.is_some()
    }

    /// Feed one page of the seed. Returns the next page request, or, on the
    /// last page, installs the records and replays buffered events past the
    /// cut.
    pub fn apply_page(&mut self, page: OrganizationPage) -> ReplicaAction {
        let Some(seed) = self.seed.as_mut() else {
            return ReplicaAction::None;
        };
        seed.sessions.extend(page.sessions.iter().cloned());
        seed.resources.extend(page.resources.iter().cloned());
        let next = page.next_cursor.clone();
        if seed.page.is_none() {
            seed.page = Some(page);
        }
        if let Some(cursor) = next {
            return ReplicaAction::NextPage(Command::GetOrganization {
                filter: seed.filter,
                page_size: ORG_PAGE_SIZE,
                cursor: Some(cursor),
            });
        }
        let Some(seed) = self.seed.take() else {
            return ReplicaAction::None;
        };
        let Some(first) = seed.page else {
            return ReplicaAction::None;
        };
        self.install(seed.filter, &first, seed.sessions, seed.resources);
        let buffered = std::mem::take(&mut self.buffered);
        for (seq, event) in buffered {
            // A reseed requested while replaying wins; the rest is moot.
            if matches!(self.apply_event(seq, &event).0, ReplicaAction::Reseed) {
                return ReplicaAction::Reseed;
            }
        }
        ReplicaAction::Seeded
    }

    /// The seed was refused or failed: drop it. The replica keeps what it
    /// had and reports itself stale.
    pub fn abandon_seed(&mut self) {
        self.seed = None;
        self.buffered.clear();
        self.state = ReplicaState::Stale;
    }

    fn install(
        &mut self,
        filter: OrgFilter,
        first: &OrganizationPage,
        sessions: Vec<SessionRecord>,
        resources: Vec<ResourceRecord>,
    ) {
        self.authority = Some(first.authority);
        if let OrgFilter::Session(key) = filter {
            self.sessions.retain(|s| s.key != key);
            self.sessions.extend(sessions);
            self.forget_members(key);
            self.resources.extend(resources);
        } else {
            self.inbox = first.inbox;
            self.snapshot_revision = first.snapshot_revision;
            self.journal_seq = first.journal_seq;
            self.sessions = sessions;
            self.resources = resources;
            self.handles = HandleMap::new();
            let known: HashSet<ArrangementKey> =
                self.sessions.iter().filter_map(|s| s.arrangement).collect();
            self.arrangements.retain(|key, _| known.contains(key));
        }
        for resource in &self.resources {
            if let Some(live) = &resource.live {
                self.handles.insert(resource.key, live.clone());
            }
        }
        if self.state != ReplicaState::Stale || matches!(filter, OrgFilter::All) {
            self.state = ReplicaState::Live;
        }
    }

    /// Store an arrangement fetched with `ARRANGEMENT_GET`.
    pub fn apply_arrangement(&mut self, value: ArrangementValue) {
        if let ArrangementOwner::Session(owner) = value.owner
            && let Some(session) = self.sessions.iter_mut().find(|s| s.key == owner)
        {
            session.arrangement = Some(value.key);
        }
        self.arrangements.insert(value.key, value);
    }

    /// Apply one journal event, stamped `seq` when the server journals
    /// events. Returns what the control plane must do, and what changed.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per organization event, each a few record updates"
    )]
    pub fn apply_event(
        &mut self,
        seq: Option<u64>,
        event: &AgentEvent,
    ) -> (ReplicaAction, RecordChange) {
        if matches!(event, AgentEvent::JournalGap { .. }) {
            if self.state != ReplicaState::Empty {
                self.state = ReplicaState::Stale;
                return (ReplicaAction::Reseed, RecordChange::Other);
            }
            return (ReplicaAction::None, RecordChange::Other);
        }
        if !is_organization_event(event) {
            return (ReplicaAction::None, RecordChange::Other);
        }
        if self.seed.is_some() {
            if self.buffered.len() >= MAX_BUFFERED_EVENTS {
                self.buffered.clear();
                return (ReplicaAction::Reseed, RecordChange::Other);
            }
            self.buffered.push((seq, event.clone()));
            return (ReplicaAction::None, RecordChange::Other);
        }
        if self.state != ReplicaState::Live {
            return (ReplicaAction::None, RecordChange::Other);
        }
        if let Some(seq) = seq {
            if seq <= self.journal_seq {
                // Already part of the seed's cut.
                return (ReplicaAction::None, RecordChange::Other);
            }
            self.journal_seq = seq;
        }
        match event {
            AgentEvent::SessionCreated {
                session,
                name,
                lifetime,
                revision,
            } => {
                if self.session(session).is_none() {
                    self.sessions.push(
                        SessionRecord::new(*session, name.clone(), *revision)
                            .with_lifetime(*lifetime),
                    );
                }
                // The live handle arrives with the records.
                (ReplicaAction::RefetchSession(*session), RecordChange::Other)
            }
            AgentEvent::SessionRenamed {
                session,
                name,
                revision,
            } => {
                self.with_session(session, |s| {
                    s.name.clone_from(name);
                    s.revision = *revision;
                });
                (ReplicaAction::Changed, RecordChange::Other)
            }
            AgentEvent::SessionDeleted { session, .. } => {
                if let Some(arrangement) = self.session(session).and_then(|s| s.arrangement) {
                    self.arrangements.remove(&arrangement);
                }
                self.sessions.retain(|s| s.key != *session);
                self.forget_members(*session);
                (
                    ReplicaAction::Changed,
                    RecordChange::SessionDeleted(*session),
                )
            }
            AgentEvent::SessionLifetimeChanged {
                session,
                lifetime,
                revision,
            } => {
                self.with_session(session, |s| {
                    s.lifetime = *lifetime;
                    s.revision = *revision;
                });
                (ReplicaAction::Changed, RecordChange::Other)
            }
            AgentEvent::IntakeChanged {
                session,
                awaiting_organization,
                revision,
            } => {
                self.with_session(session, |s| {
                    s.awaiting_organization = *awaiting_organization;
                    s.revision = *revision;
                });
                (ReplicaAction::Changed, RecordChange::Other)
            }
            AgentEvent::MembershipChanged {
                resource,
                from,
                to,
                from_revision,
                to_revision,
            } => {
                if let Some(record) = self.resources.iter_mut().find(|r| r.key == *resource) {
                    record.session = *to;
                }
                self.with_session(from, |s| {
                    s.revision = *from_revision;
                    s.members = s.members.saturating_sub(1);
                });
                self.with_session(to, |s| {
                    s.revision = *to_revision;
                    s.members = s.members.saturating_add(1);
                });
                if self.resource(resource).is_none() {
                    return (ReplicaAction::RefetchSession(*to), RecordChange::Other);
                }
                (ReplicaAction::Changed, RecordChange::Other)
            }
            AgentEvent::ResourceCreated {
                resource,
                session,
                kind,
            } => {
                if self.resource(resource).is_none() {
                    self.resources.push(
                        ResourceRecord::new(*resource, *session, Revision::default())
                            .with_kind(*kind),
                    );
                    self.with_session(session, |s| s.members = s.members.saturating_add(1));
                }
                // Its revision, launch, and live handle arrive with the
                // records.
                (ReplicaAction::RefetchSession(*session), RecordChange::Other)
            }
            AgentEvent::ResourceRemoved {
                resource, session, ..
            } => {
                let existed = self.resource(resource).is_some();
                self.resources.retain(|r| r.key != *resource);
                self.handles.remove_key(resource);
                if existed {
                    self.with_session(session, |s| s.members = s.members.saturating_sub(1));
                }
                (
                    ReplicaAction::Changed,
                    RecordChange::ResourceRemoved(*resource),
                )
            }
            AgentEvent::ActivationChanged {
                resource,
                activation,
                state,
                outcome,
            } => {
                let Some(record) = self.resources.iter_mut().find(|r| r.key == *resource) else {
                    return (ReplicaAction::None, RecordChange::Other);
                };
                let instance = record.activation.map(|a| a.instance);
                let fresh = record.activation.is_none_or(|a| a.id != *activation);
                let info = ActivationInfo::new(
                    *activation,
                    instance.unwrap_or_else(|| phux_protocol::ids::ServerInstance::new([0; 16])),
                    *state,
                )
                .with_outcome(*outcome);
                record.activation = Some(info);
                let session = record.session;
                let change = RecordChange::Activation {
                    resource: *resource,
                    activation: info,
                };
                // A new activation runs under a new live handle and maybe a
                // new server instance: read them.
                if (fresh && !is_inactive(Some(*state))) || instance.is_none() {
                    return (ReplicaAction::RefetchSession(session), change);
                }
                (ReplicaAction::Changed, change)
            }
            AgentEvent::ResourceRenamed {
                resource,
                name,
                revision,
            } => {
                if let Some(record) = self.resources.iter_mut().find(|r| r.key == *resource) {
                    record.name.clone_from(name);
                    record.revision = *revision;
                }
                (ReplicaAction::Changed, RecordChange::Other)
            }
            AgentEvent::ArrangementChanged {
                arrangement,
                owner,
                revision,
            } => {
                let ArrangementOwner::Session(owner) = *owner else {
                    return (ReplicaAction::None, RecordChange::Other);
                };
                match revision {
                    None => {
                        self.arrangements.remove(arrangement);
                        self.with_session(&owner, |s| {
                            if s.arrangement == Some(*arrangement) {
                                s.arrangement = None;
                            }
                        });
                        (ReplicaAction::Changed, RecordChange::Other)
                    }
                    Some(revision) => {
                        self.with_session(&owner, |s| s.arrangement = Some(*arrangement));
                        let stale = self
                            .arrangements
                            .get(arrangement)
                            .is_some_and(|held| held.revision < *revision);
                        if stale {
                            (
                                ReplicaAction::RefetchArrangement(*arrangement),
                                RecordChange::Other,
                            )
                        } else {
                            (ReplicaAction::Changed, RecordChange::Other)
                        }
                    }
                }
            }
            _ => (ReplicaAction::None, RecordChange::Other),
        }
    }

    /// Drop `session`'s resource records and their handles.
    fn forget_members(&mut self, session: SessionKey) {
        for gone in self.resources.iter().filter(|r| r.session == session) {
            self.handles.remove_key(&gone.key);
        }
        self.resources.retain(|r| r.session != session);
    }

    fn with_session(&mut self, key: &SessionKey, edit: impl FnOnce(&mut SessionRecord)) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.key == *key) {
            edit(session);
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
mod tests {
    use super::*;
    use phux_protocol::ids::{ActivationId, ResourceKind, ServerInstance};
    use phux_protocol::wire::frame::{ExecState, Outcome};

    fn authority() -> AuthorityId {
        AuthorityId::new([1; 16]).unwrap()
    }

    fn skey(n: u8) -> SessionKey {
        SessionKey::new([n; 16]).unwrap()
    }

    fn rkey(n: u8) -> ResourceKey {
        ResourceKey::new([0x40 | n; 16]).unwrap()
    }

    fn running(n: u8, session: SessionKey) -> ResourceRecord {
        ResourceRecord::new(rkey(n), session, Revision::new(1))
            .with_activation(Some(ActivationInfo::new(
                ActivationId::new(1),
                ServerInstance::new([9; 16]),
                ExecState::Running,
            )))
            .with_live(Some(ResourceId::local(u32::from(n))))
    }

    fn page(
        seq: u64,
        sessions: Vec<SessionRecord>,
        resources: Vec<ResourceRecord>,
    ) -> OrganizationPage {
        OrganizationPage::new(authority(), Revision::new(10), seq)
            .with_sessions(sessions)
            .with_resources(resources)
    }

    fn seeded() -> OrganizationReplica {
        let mut replica = OrganizationReplica::new();
        replica.begin_seed(OrgFilter::All);
        let work = SessionRecord::new(skey(1), "work", Revision::new(2)).with_members(1);
        assert_eq!(
            replica.apply_page(page(5, vec![work], vec![running(1, skey(1))])),
            ReplicaAction::Seeded
        );
        replica
    }

    #[test]
    fn a_paged_seed_installs_records_and_the_handle_map() {
        let mut replica = OrganizationReplica::new();
        assert_eq!(replica.state(), ReplicaState::Empty);
        replica.begin_seed(OrgFilter::All);
        let first = page(
            5,
            vec![SessionRecord::new(skey(1), "work", Revision::new(2))],
            vec![],
        )
        .with_next_cursor(Some(vec![1]));
        let ReplicaAction::NextPage(Command::GetOrganization { cursor, .. }) =
            replica.apply_page(first)
        else {
            panic!("expected the next page");
        };
        assert_eq!(cursor, Some(vec![1]));
        assert!(replica.seeding());
        assert_eq!(
            replica.apply_page(page(99, vec![], vec![running(1, skey(1))])),
            ReplicaAction::Seeded
        );
        assert!(replica.is_live());
        assert_eq!(replica.journal_seq(), 5, "the first page's cut");
        assert_eq!(replica.authority(), Some(authority()));
        assert_eq!(
            replica
                .resource_by_live(&ResourceId::local(1))
                .map(|r| r.key),
            Some(rkey(1))
        );
    }

    #[test]
    fn events_apply_in_journal_order_and_old_ones_are_skipped() {
        let mut replica = seeded();
        let rename = |name: &str, revision| AgentEvent::SessionRenamed {
            session: skey(1),
            name: name.to_owned(),
            revision: Revision::new(revision),
        };
        assert_eq!(
            replica.apply_event(Some(4), &rename("old", 1)).0,
            ReplicaAction::None,
            "before the cut"
        );
        assert_eq!(replica.session(&skey(1)).unwrap().name, "work");
        assert_eq!(
            replica.apply_event(Some(6), &rename("notes", 3)).0,
            ReplicaAction::Changed
        );
        assert_eq!(replica.session(&skey(1)).unwrap().name, "notes");
        assert_eq!(replica.journal_seq(), 6);
        replica.apply_event(
            Some(7),
            &AgentEvent::ActivationChanged {
                resource: rkey(1),
                activation: ActivationId::new(1),
                state: ExecState::Completed,
                outcome: Some(Outcome::Exited(0)),
            },
        );
        assert_eq!(
            replica
                .resource(&rkey(1))
                .unwrap()
                .activation
                .unwrap()
                .state,
            ExecState::Completed
        );
        let (_, change) = replica.apply_event(
            Some(8),
            &AgentEvent::ResourceRemoved {
                resource: rkey(1),
                session: skey(1),
                kind: ResourceKind::Terminal,
            },
        );
        assert_eq!(change, RecordChange::ResourceRemoved(rkey(1)));
        assert!(replica.handles().is_empty());
        assert_eq!(replica.session(&skey(1)).unwrap().members, 0);
    }

    #[test]
    fn events_during_a_seed_are_buffered_and_replayed_past_the_cut() {
        let mut replica = OrganizationReplica::new();
        replica.begin_seed(OrgFilter::All);
        let renamed = |seq_name: &str| AgentEvent::SessionRenamed {
            session: skey(1),
            name: seq_name.to_owned(),
            revision: Revision::new(9),
        };
        replica.apply_event(Some(4), &renamed("before-cut"));
        replica.apply_event(Some(6), &renamed("after-cut"));
        replica.apply_page(page(
            5,
            vec![SessionRecord::new(skey(1), "work", Revision::new(2))],
            vec![],
        ));
        assert_eq!(replica.session(&skey(1)).unwrap().name, "after-cut");
    }

    #[test]
    fn a_journal_gap_asks_for_a_reseed() {
        let mut replica = seeded();
        let (action, _) = replica.apply_event(
            None,
            &AgentEvent::JournalGap {
                first_missing: 6,
                last_missing: 9,
            },
        );
        assert_eq!(action, ReplicaAction::Reseed);
        assert_eq!(replica.state(), ReplicaState::Stale);
        replica.begin_seed(OrgFilter::All);
        replica.apply_page(page(20, vec![], vec![]));
        assert!(replica.is_live());
        assert!(replica.sessions().is_empty());
    }

    #[test]
    fn incomplete_events_ask_for_a_session_refetch() {
        let mut replica = seeded();
        let (action, _) = replica.apply_event(
            Some(6),
            &AgentEvent::ResourceCreated {
                resource: rkey(2),
                session: skey(1),
                kind: ResourceKind::Terminal,
            },
        );
        assert_eq!(action, ReplicaAction::RefetchSession(skey(1)));
        assert_eq!(replica.members(&skey(1)).count(), 2);
        // The session refetch replaces only that session's records.
        replica.begin_seed(OrgFilter::Session(skey(1)));
        replica.apply_page(page(
            7,
            vec![SessionRecord::new(skey(1), "work", Revision::new(3)).with_members(2)],
            vec![running(1, skey(1)), running(2, skey(1))],
        ));
        assert_eq!(
            replica.journal_seq(),
            6,
            "a session refetch does not move the cursor"
        );
        assert_eq!(
            replica
                .resource_by_live(&ResourceId::local(2))
                .map(|r| r.key),
            Some(rkey(2))
        );
    }

    #[test]
    fn membership_moves_and_the_inbox_projection() {
        let mut replica = OrganizationReplica::new();
        replica.begin_seed(OrgFilter::All);
        let first = OrganizationPage::new(authority(), Revision::new(1), 1)
            .with_inbox(Some(skey(9)))
            .with_sessions(vec![
                SessionRecord::new(skey(9), "inbox", Revision::new(1)),
                SessionRecord::new(skey(2), "imported", Revision::new(1))
                    .with_awaiting_organization(true)
                    .with_members(1),
                SessionRecord::new(skey(1), "work", Revision::new(1)),
            ])
            .with_resources(vec![running(1, skey(2))]);
        replica.apply_page(first);
        let view = replica.inbox_view();
        assert_eq!(
            view.sessions.iter().map(|s| s.key).collect::<Vec<_>>(),
            vec![skey(9), skey(2)]
        );
        assert_eq!(view.resources.len(), 1);
        replica.apply_event(
            Some(2),
            &AgentEvent::MembershipChanged {
                resource: rkey(1),
                from: skey(2),
                to: skey(1),
                from_revision: Revision::new(2),
                to_revision: Revision::new(2),
            },
        );
        replica.apply_event(
            Some(3),
            &AgentEvent::IntakeChanged {
                session: skey(2),
                awaiting_organization: false,
                revision: Revision::new(3),
            },
        );
        let view = replica.inbox_view();
        assert_eq!(view.sessions.len(), 1);
        assert!(view.resources.is_empty());
        assert_eq!(replica.members(&skey(1)).count(), 1);
        assert_eq!(replica.session(&skey(1)).unwrap().members, 1);
    }
}
