//! The organization half of the scripted server: an in-memory model of
//! sessions, resources, arrangements, revisions, and receipts that answers
//! every organization command (`docs/organization.md`) the way the reference
//! server does, with revision-conflict and refusal injection.
//!
//! Replies follow the spec's shapes: mutating commands answer
//! `OK_WITH(COMMITTED)` or `REFUSED`, `RESOURCE_TERMINATE` answers
//! `ACCEPTED`, reads answer `ORGANIZATION_PAGE` or `ARRANGEMENT`. A same-id
//! resend with the same payload returns the stored result; with another
//! payload it is refused `DUPLICATE_OPERATION`. Without explicit
//! `GET_STATE` payloads, the scripted server answers `GET_STATE` from this
//! model (one legacy window per non-empty session, as R06 synthesizes).

#![allow(
    clippy::missing_panics_doc,
    reason = "a test fixture's record builders panic only on the non-zero key \
              invariant its own counters guarantee"
)]

use std::collections::VecDeque;

use phux_protocol::ResourceId;
use phux_protocol::ids::{
    ActivationId, ArrangementKey, AuthorityId, OperationId, ResourceKey, Revision, ServerInstance,
    SessionId, SessionKey, WindowId,
};
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementDoc, ArrangementOwner, ArrangementValue, Command, CommandResult,
    EntryTarget, ExecState, ExitPolicy, Outcome, RecordRevision, Refusal, ResourceRecord,
    SessionLifetime, SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

mod answer;
mod mutate;

/// One stored receipt: the command, its result, and how to undo it.
#[derive(Debug, Clone)]
struct Receipt {
    operation: OperationId,
    command: Command,
    result: CommandResult,
    inverse: Option<Inverse>,
}

#[derive(Debug, Clone)]
enum Inverse {
    /// Move these resources back to their sessions.
    Moves(Vec<(ResourceKey, SessionKey)>),
    /// Restore a session's intake mark.
    Intake(SessionKey),
}

/// The in-memory organization a [`super::ScriptSpec`] serves. Build one with
/// [`OrgFixture::new`] and the record builders, then hand it to
/// [`super::ScriptSpec::organization`].
#[derive(Debug, Clone)]
pub struct OrgFixture {
    authority: AuthorityId,
    instance: ServerInstance,
    inbox: Option<SessionKey>,
    sessions: Vec<SessionRecord>,
    resources: Vec<ResourceRecord>,
    arrangements: Vec<ArrangementValue>,
    receipts: Vec<Receipt>,
    counter: u32,
    revision: u64,
    conflicts: u32,
    page_size: Option<usize>,
    refusals: VecDeque<(&'static str, Refusal)>,
    terminate_holds: bool,
}

impl Default for OrgFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl OrgFixture {
    /// An empty organization.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            authority: AuthorityId::new([0xa1; 16]).expect("non-zero"),
            instance: ServerInstance::new([0x15; 16]),
            inbox: None,
            sessions: Vec::new(),
            resources: Vec::new(),
            arrangements: Vec::new(),
            receipts: Vec::new(),
            counter: 0,
            revision: 1,
            conflicts: 0,
            page_size: None,
            refusals: VecDeque::new(),
            terminate_holds: false,
        }
    }

    /// The server instance live targets carry.
    #[must_use]
    pub const fn instance(&self) -> ServerInstance {
        self.instance
    }

    /// Serve pages of at most `size` records whatever the request asks.
    #[must_use]
    pub const fn page_size(mut self, size: usize) -> Self {
        self.page_size = Some(size);
        self
    }

    /// The next `n` revision-checked commands are refused `CONFLICT`, as if
    /// another writer committed first; the record's revision moves.
    #[must_use]
    pub const fn inject_conflicts(mut self, n: u32) -> Self {
        self.conflicts = n;
        self
    }

    /// Refuse the next command named `command` (its `CommandTag` name in
    /// lower snake case, e.g. `"session_delete"`) with `refusal`.
    #[must_use]
    pub fn refuse_next(mut self, command: &'static str, refusal: Refusal) -> Self {
        self.refusals.push_back((command, refusal));
        self
    }

    /// `RESOURCE_TERMINATE` is accepted but the activation never ends: a
    /// process that ignores the signal.
    #[must_use]
    pub const fn terminate_holds(mut self) -> Self {
        self.terminate_holds = true;
        self
    }

    const fn fresh(&mut self) -> [u8; 16] {
        self.counter += 1;
        let mut bytes = [0x5a; 16];
        let counter = self.counter.to_be_bytes();
        bytes[0] = counter[0];
        bytes[1] = counter[1];
        bytes[2] = counter[2];
        bytes[3] = counter[3];
        bytes
    }

    const fn next_live(&mut self) -> u32 {
        self.counter += 1;
        self.counter
    }

    const fn bump(&mut self) -> Revision {
        self.revision += 1;
        Revision::new(self.revision)
    }

    /// Add a session; returns its key. Its live handle is a fresh id.
    pub fn session(&mut self, name: &str, lifetime: SessionLifetime) -> SessionKey {
        let key = SessionKey::new(self.fresh()).expect("non-zero");
        let live = SessionId::new(self.next_live());
        let revision = self.bump();
        self.sessions.push(
            SessionRecord::new(key, name, revision)
                .with_lifetime(lifetime)
                .with_live(Some(live)),
        );
        key
    }

    /// Add the designated Intake session (legacy fixture method name).
    pub fn inbox_session(&mut self, name: &str) -> SessionKey {
        let key = self.session(name, SessionLifetime::Persistent);
        self.inbox = Some(key);
        if let Some(record) = self.session_mut(&key) {
            record.awaiting_organization = true;
        }
        key
    }

    /// Add a Terminal member of `session` that keeps its record when it
    /// exits, in `state` (`None` never launched). Executing states get a live
    /// handle.
    pub fn resource(&mut self, session: SessionKey, state: Option<ExecState>) -> ResourceKey {
        self.member(session, state, ExitPolicy::Retain, None)
    }

    /// Add an interactive shell member: running, removed when it exits.
    pub fn shell(&mut self, session: SessionKey) -> ResourceKey {
        self.member(session, Some(ExecState::Running), ExitPolicy::Remove, None)
    }

    /// Add a member whose activation a server restart ended (completed,
    /// outcome unobserved).
    pub fn ended_by_restart(&mut self, session: SessionKey) -> ResourceKey {
        self.member(
            session,
            Some(ExecState::Completed),
            ExitPolicy::Retain,
            Some(Outcome::Unobserved),
        )
    }

    fn member(
        &mut self,
        session: SessionKey,
        state: Option<ExecState>,
        on_exit: ExitPolicy,
        outcome: Option<Outcome>,
    ) -> ResourceKey {
        let key = ResourceKey::new(self.fresh()).expect("non-zero");
        let revision = self.bump();
        let instance = self.instance;
        let executing = state.is_some_and(|s| !phux_client_core::org_ops::is_inactive(Some(s)));
        let live = (executing && state != Some(ExecState::Lost))
            .then(|| ResourceId::local(self.next_live()));
        self.resources.push(
            ResourceRecord::new(key, session, revision)
                .with_on_exit(on_exit)
                .with_activation(state.map(|s| {
                    ActivationInfo::new(ActivationId::new(1), instance, s).with_outcome(outcome)
                }))
                .with_live(live),
        );
        self.touch_session(&session);
        key
    }

    /// Give `session` an arrangement holding `document`; returns its key.
    pub fn arrangement(&mut self, session: SessionKey, document: ArrangementDoc) -> ArrangementKey {
        let key = ArrangementKey::new(self.fresh()).expect("non-zero");
        let revision = self.bump();
        self.arrangements.push(ArrangementValue::new(
            key,
            ArrangementOwner::Session(session),
            revision,
            document,
        ));
        if let Some(record) = self.session_mut(&session) {
            record.arrangement = Some(key);
        }
        key
    }

    /// The live handle of a resource.
    #[must_use]
    pub fn live(&self, resource: &ResourceKey) -> Option<ResourceId> {
        self.resource_ref(resource).and_then(|r| r.live.clone())
    }

    /// The live handle of a session.
    #[must_use]
    pub fn live_session(&self, session: &SessionKey) -> Option<SessionId> {
        self.session_ref(session).and_then(|s| s.live)
    }

    /// Every session record, in creation order.
    #[must_use]
    pub fn sessions(&self) -> &[SessionRecord] {
        &self.sessions
    }

    /// Every resource record, in creation order.
    #[must_use]
    pub fn resources(&self) -> &[ResourceRecord] {
        &self.resources
    }

    /// A resource's record.
    #[must_use]
    pub fn resource_ref(&self, key: &ResourceKey) -> Option<&ResourceRecord> {
        self.resources.iter().find(|r| r.key == *key)
    }

    /// A session's record.
    #[must_use]
    pub fn session_ref(&self, key: &SessionKey) -> Option<&SessionRecord> {
        self.sessions.iter().find(|s| s.key == *key)
    }

    /// An arrangement.
    #[must_use]
    pub fn arrangement_ref(&self, key: &ArrangementKey) -> Option<&ArrangementValue> {
        self.arrangements.iter().find(|a| a.key == *key)
    }

    fn session_mut(&mut self, key: &SessionKey) -> Option<&mut SessionRecord> {
        self.sessions.iter_mut().find(|s| s.key == *key)
    }

    fn resource_mut(&mut self, key: &ResourceKey) -> Option<&mut ResourceRecord> {
        self.resources.iter_mut().find(|r| r.key == *key)
    }

    /// Recount a session's members and move its revision.
    fn touch_session(&mut self, key: &SessionKey) -> Option<Revision> {
        let members = u32::try_from(self.resources.iter().filter(|r| r.session == *key).count())
            .unwrap_or(u32::MAX);
        let revision = self.bump();
        let record = self.session_mut(key)?;
        record.members = members;
        record.revision = revision;
        Some(revision)
    }

    fn pin_session(&self, key: &SessionKey) -> Option<RecordRevision> {
        self.session_ref(key).map(|s| RecordRevision {
            key: *s.key.as_bytes(),
            revision: s.revision,
        })
    }

    /// Drop `resource` from the organization, pruning it from its session's
    /// arrangement and deleting an ephemeral session it leaves empty.
    fn drop_resource(&mut self, resource: &ResourceKey) -> Option<SessionKey> {
        let position = self.resources.iter().position(|r| r.key == *resource)?;
        let record = self.resources.remove(position);
        self.prune_arrangement(&record.session, resource);
        self.touch_session(&record.session);
        let empty_ephemeral = self
            .session_ref(&record.session)
            .is_some_and(|s| s.lifetime == SessionLifetime::Ephemeral && s.members == 0);
        if empty_ephemeral {
            self.delete_session(&record.session);
        }
        Some(record.session)
    }

    fn prune_arrangement(&mut self, session: &SessionKey, resource: &ResourceKey) {
        let Some(key) = self.session_ref(session).and_then(|s| s.arrangement) else {
            return;
        };
        let revision = self.bump();
        if let Some(arrangement) = self.arrangements.iter_mut().find(|a| a.key == key) {
            let groups = std::mem::take(&mut arrangement.document.groups);
            for mut group in groups {
                if let Some(tree) = prune_node(group.tree, resource) {
                    group.tree = tree;
                    arrangement.document.groups.push(group);
                }
            }
            arrangement.revision = revision;
        }
    }

    fn delete_session(&mut self, key: &SessionKey) {
        if let Some(arrangement) = self.session_ref(key).and_then(|s| s.arrangement) {
            self.arrangements.retain(|a| a.key != arrangement);
        }
        self.sessions.retain(|s| s.key != *key);
    }

    /// The `GET_STATE` snapshot this organization implies: one legacy
    /// window per non-empty session, each resource carrying its session.
    #[must_use]
    pub fn snapshot(&self) -> SessionSnapshot {
        let mut sessions = Vec::new();
        let mut windows = Vec::new();
        let mut resources = Vec::new();
        for record in &self.sessions {
            let Some(live) = record.live else { continue };
            let window = WindowId::new(live.get());
            let members: Vec<&ResourceRecord> = self
                .resources
                .iter()
                .filter(|r| r.session == record.key)
                .collect();
            let executing: Vec<ResourceId> =
                members.iter().filter_map(|r| r.live.clone()).collect();
            sessions.push(
                SessionInfo::new(live, record.name.clone())
                    .with_window_count(u16::from(!executing.is_empty()))
                    .with_lifetime(Some(record.lifetime))
                    .with_key(Some(record.key)),
            );
            if !executing.is_empty() {
                windows.push(
                    WindowInfo::new(window, live, "window-0")
                        .with_active_resource(executing.first().cloned()),
                );
            }
            for id in executing {
                resources.push(ResourceInfo::new(id, window, 80, 24).with_session(Some(live)));
            }
        }
        let focused_session = sessions.first().map_or(SessionId::new(0), |s| s.id);
        let focused_resource = resources
            .first()
            .map_or(ResourceId::local(0), |r: &ResourceInfo| r.id.clone());
        SessionSnapshot::new(
            focused_session,
            WindowId::new(focused_session.get()),
            focused_resource,
        )
        .with_sessions(sessions)
        .with_windows(windows)
        .with_resources(resources)
    }
}

fn prune_node(
    node: phux_protocol::wire::frame::ArrangementNode,
    resource: &ResourceKey,
) -> Option<phux_protocol::wire::frame::ArrangementNode> {
    use phux_protocol::wire::frame::ArrangementNode;
    match node {
        ArrangementNode::Leaf(entry) => match entry.target {
            EntryTarget::Member(key) if key == *resource => None,
            _ => Some(ArrangementNode::Leaf(entry)),
        },
        ArrangementNode::Split {
            dir,
            ratio,
            first,
            second,
        } => match (prune_node(*first, resource), prune_node(*second, resource)) {
            (Some(first), Some(second)) => Some(ArrangementNode::Split {
                dir,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}
