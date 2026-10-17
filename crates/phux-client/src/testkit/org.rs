//! The organization half of the scripted server: an in-memory model of
//! sessions, resources, arrangements, revisions, and receipts that answers
//! every organization command (`docs/spec/L1.md` §5.7) the way the reference
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
    CommandValue, Committed, EntryTarget, ExecState, ExitPolicy, LaunchMode, LiveTarget, OrgFilter,
    OrganizationPage, Outcome, RecordRevision, Refusal, ResourceRecord, SessionLifetime,
    SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

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

    /// Answer one organization command.
    pub(super) fn answer(&mut self, command: &Command) -> CommandResult {
        if let Some(operation) = command.idempotency_key().copied() {
            if let Some(receipt) = self.receipts.iter().find(|r| r.operation == operation) {
                return if receipt.command == *command {
                    receipt.result.clone()
                } else {
                    CommandResult::Refused(Refusal::DuplicateOperation)
                };
            }
            let name = command_name(command);
            if let Some(position) = self.refusals.iter().position(|(n, _)| *n == name) {
                let (_, refusal) = self.refusals.remove(position).expect("present");
                return CommandResult::Refused(refusal);
            }
            let (result, inverse) = match self.mutate(command) {
                Ok((committed, inverse)) => (committed, inverse),
                Err(refusal) => (CommandResult::Refused(refusal), None),
            };
            self.receipts.push(Receipt {
                operation,
                command: command.clone(),
                result: result.clone(),
                inverse,
            });
            return result;
        }
        match command {
            Command::GetOrganization {
                filter,
                page_size,
                cursor,
            } => self.page(*filter, *page_size, cursor.as_deref()),
            Command::ArrangementGet { arrangement } => self
                .arrangement_ref(arrangement)
                .map_or(CommandResult::Refused(Refusal::NotFound), |value| {
                    CommandResult::OkWith(CommandValue::Arrangement(Box::new(value.clone())))
                }),
            _ => CommandResult::Refused(Refusal::Unsupported {
                capability: "scripted".to_owned(),
            }),
        }
    }

    fn page(&self, filter: OrgFilter, page_size: u32, cursor: Option<&[u8]>) -> CommandResult {
        let sessions: Vec<&SessionRecord> = self
            .sessions
            .iter()
            .filter(|s| match filter {
                OrgFilter::Session(key) => s.key == key,
                OrgFilter::Intake => s.awaiting_organization || Some(s.key) == self.inbox,
                _ => true,
            })
            .collect();
        let resources: Vec<&ResourceRecord> = self
            .resources
            .iter()
            .filter(|r| sessions.iter().any(|s| s.key == r.session))
            .collect();
        let total = sessions.len() + resources.len();
        let start = cursor
            .and_then(|c| <[u8; 4]>::try_from(c).ok())
            .map_or(0, |c| u32::from_be_bytes(c) as usize);
        if start > total {
            return CommandResult::Refused(Refusal::InvalidArgument {
                field: "cursor".to_owned(),
                reason: "expired".to_owned(),
            });
        }
        let size = self.page_size.unwrap_or(if page_size == 0 {
            usize::MAX
        } else {
            page_size as usize
        });
        let end = start.saturating_add(size.max(1)).min(total);
        let mut page_sessions = Vec::new();
        let mut page_resources = Vec::new();
        for index in start..end {
            if index < sessions.len() {
                page_sessions.push(sessions[index].clone());
            } else {
                page_resources.push(resources[index - sessions.len()].clone());
            }
        }
        let intake = u32::try_from(sessions.iter().filter(|s| s.awaiting_organization).count())
            .unwrap_or(u32::MAX);
        let page = OrganizationPage::new(self.authority, Revision::new(self.revision), 0)
            .with_inbox(self.inbox)
            .with_sessions(page_sessions)
            .with_resources(page_resources)
            .with_totals(
                u32::try_from(sessions.len()).unwrap_or(u32::MAX),
                u32::try_from(resources.len()).unwrap_or(u32::MAX),
                intake,
            )
            .with_next_cursor(
                (end < total).then(|| u32::try_from(end).unwrap_or(0).to_be_bytes().to_vec()),
            );
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page)))
    }

    /// An injected conflict: the record's revision moves and the caller's
    /// expectation no longer holds.
    fn conflict(&mut self, record: RecordKey) -> Result<(), Refusal> {
        if self.conflicts == 0 {
            return Ok(());
        }
        self.conflicts -= 1;
        let revision = self.bump();
        match record {
            RecordKey::Session(key) => {
                if let Some(s) = self.session_mut(&key) {
                    s.revision = revision;
                }
            }
            RecordKey::Resource(key) => {
                if let Some(r) = self.resource_mut(&key) {
                    r.revision = revision;
                }
            }
            RecordKey::Arrangement(key) => {
                if let Some(a) = self.arrangements.iter_mut().find(|a| a.key == key) {
                    a.revision = revision;
                }
            }
        }
        Err(Refusal::Conflict { current: revision })
    }

    fn check_session(&mut self, key: SessionKey, expected: Revision) -> Result<(), Refusal> {
        let current = self.session_ref(&key).ok_or(Refusal::NotFound)?.revision;
        self.conflict(RecordKey::Session(key))?;
        if current == expected {
            Ok(())
        } else {
            Err(Refusal::Conflict { current })
        }
    }

    fn check_resource(&mut self, key: ResourceKey, expected: Revision) -> Result<(), Refusal> {
        let current = self.resource_ref(&key).ok_or(Refusal::NotFound)?.revision;
        self.conflict(RecordKey::Resource(key))?;
        if current == expected {
            Ok(())
        } else {
            Err(Refusal::Conflict { current })
        }
    }

    fn require_inactive(&self, key: &ResourceKey) -> Result<(), Refusal> {
        let state = self
            .resource_ref(key)
            .and_then(|r| r.activation.map(|a| a.state));
        match state {
            Some(ExecState::Lost) => Err(Refusal::ExecutionUncertain),
            s if !phux_client_core::org_ops::is_inactive(s) => Err(Refusal::InvalidArgument {
                field: "resource".to_owned(),
                reason: "execution is current; terminate it first".to_owned(),
            }),
            _ => Ok(()),
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per organization command")]
    fn mutate(&mut self, command: &Command) -> Result<(CommandResult, Option<Inverse>), Refusal> {
        let ok = |committed: Committed| {
            CommandResult::OkWith(CommandValue::Committed(Box::new(committed)))
        };
        match command {
            Command::SessionCreate {
                operation,
                name,
                lifetime,
                launch,
                ..
            } => {
                if self.sessions.iter().any(|s| s.name == *name) {
                    return Err(Refusal::NameTaken);
                }
                if *lifetime == SessionLifetime::Ephemeral && launch.is_none() {
                    return Err(Refusal::InvalidArgument {
                        field: "launch".to_owned(),
                        reason: "an ephemeral session needs a launch".to_owned(),
                    });
                }
                let session = self.session(name, *lifetime);
                let revision = self
                    .session_ref(&session)
                    .map(|record| record.revision)
                    .unwrap_or_default();
                let mut committed =
                    Committed::new(*operation, revision).with_session(Some(session));
                let mut groups = Vec::new();
                if launch.is_some() {
                    let resource = self.shell(session);
                    let record = self.resource_ref(&resource).expect("made");
                    committed = committed
                        .with_resource(Some(resource))
                        .with_target(phux_client_core::org_ops::live_target(record));
                    groups.push(phux_protocol::wire::frame::ArrangementGroup {
                        id: self.fresh(),
                        name: None,
                        tree: phux_protocol::wire::frame::ArrangementNode::Leaf(
                            phux_protocol::wire::frame::EntryRef {
                                entry: self.fresh(),
                                target: EntryTarget::Member(resource),
                            },
                        ),
                    });
                }
                // Every session has an arrangement (SPEC L2 §2.2): the seed
                // member's one group, or none.
                self.arrangement(session, ArrangementDoc { groups });
                Ok((ok(committed), None))
            }
            Command::SessionRename {
                operation,
                session,
                name,
            } => {
                self.session_ref(session).ok_or(Refusal::NotFound)?;
                if self
                    .sessions
                    .iter()
                    .any(|s| s.name == *name && s.key != *session)
                {
                    return Err(Refusal::NameTaken);
                }
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                record.name.clone_from(name);
                record.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionSetLifetime {
                operation,
                session,
                lifetime,
            } => {
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                record.lifetime = *lifetime;
                record.revision = revision;
                if *lifetime == SessionLifetime::Ephemeral && record.members == 0 {
                    self.delete_session(session);
                }
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionDelete { operation, session } => {
                let record = self.session_ref(session).ok_or(Refusal::NotFound)?;
                if Some(*session) == self.inbox {
                    return Err(Refusal::InvalidArgument {
                        field: "session".to_owned(),
                        reason: "the designated intake session cannot be deleted".to_owned(),
                    });
                }
                if record.members > 0 {
                    return Err(Refusal::NotEmpty {
                        members: record.members,
                    });
                }
                self.delete_session(session);
                let revision = self.bump();
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::SessionKill { operation, session } => {
                if Some(*session) == self.inbox {
                    return Err(Refusal::InvalidArgument {
                        field: "session".to_owned(),
                        reason: "the designated intake session cannot be deleted".to_owned(),
                    });
                }
                let members: Vec<ResourceKey> = self
                    .resources
                    .iter()
                    .filter(|r| r.session == *session)
                    .map(|r| r.key)
                    .collect();
                if self.session_ref(session).is_none() {
                    return Err(Refusal::NotFound);
                }
                for member in &members {
                    self.drop_resource(member);
                }
                self.delete_session(session);
                let committed = Committed::new(*operation, Revision::default())
                    .with_members(Some(u32::try_from(members.len()).unwrap_or(u32::MAX)));
                Ok((ok(committed), None))
            }
            Command::ResourceCreate {
                operation,
                session,
                on_exit,
                launch_mode,
                ..
            } => {
                if self.session_ref(session).is_none() {
                    return Err(Refusal::NotFound);
                }
                let resource = match launch_mode {
                    LaunchMode::Deferred => self.member(*session, None, *on_exit, None),
                    LaunchMode::Immediate => self.shell(*session),
                };
                let record = self.resource_ref(&resource).expect("made");
                let committed = Committed::new(*operation, record.revision)
                    .with_resource(Some(resource))
                    .with_target(phux_client_core::org_ops::live_target(record))
                    .with_revisions(self.pin_session(session).into_iter().collect());
                Ok((ok(committed), None))
            }
            Command::ResourceMove {
                operation,
                resource,
                from,
                from_expected,
                to,
                to_expected,
            } => {
                let current = self
                    .resource_ref(resource)
                    .ok_or(Refusal::NotFound)?
                    .session;
                if current != *from {
                    return Err(Refusal::InvalidArgument {
                        field: "from".to_owned(),
                        reason: "the resource is not in that session".to_owned(),
                    });
                }
                self.check_session(*from, *from_expected)?;
                self.check_session(*to, *to_expected)?;
                let revision = self.move_one(resource, *to);
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&[*from, *to]));
                Ok((
                    ok(committed),
                    Some(Inverse::Moves(vec![(*resource, *from)])),
                ))
            }
            Command::ResourceMoveMany {
                operation,
                items,
                to,
                to_expected,
            } => {
                for item in items {
                    let current = self.resource_ref(&item.resource).ok_or(Refusal::NotFound)?;
                    if current.session != item.from {
                        return Err(Refusal::InvalidArgument {
                            field: "move_item".to_owned(),
                            reason: "a resource is not in its named session".to_owned(),
                        });
                    }
                    let source = self.session_ref(&item.from).ok_or(Refusal::NotFound)?;
                    if source.revision != item.from_expected {
                        return Err(Refusal::Conflict {
                            current: source.revision,
                        });
                    }
                }
                self.check_session(*to, *to_expected)?;
                let mut inverse = Vec::new();
                let mut touched = vec![*to];
                let mut revision = Revision::default();
                for item in items {
                    revision = self.move_one(&item.resource, *to);
                    inverse.push((item.resource, item.from));
                    touched.push(item.from);
                }
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&touched));
                Ok((ok(committed), Some(Inverse::Moves(inverse))))
            }
            Command::ResourceRename {
                operation,
                resource,
                name,
            } => {
                self.resource_ref(resource).ok_or(Refusal::NotFound)?;
                let revision = self.bump();
                let record = self.resource_mut(resource).ok_or(Refusal::NotFound)?;
                record.name.clone_from(name);
                record.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            Command::ResourceTerminate {
                operation, target, ..
            } => {
                let key = self.terminate_target(target)?;
                if !self.terminate_holds {
                    self.end_activation(&key, Outcome::Signaled(15));
                }
                Ok((
                    CommandResult::Accepted {
                        operation: *operation,
                    },
                    None,
                ))
            }
            Command::ResourceRemove {
                operation,
                resource,
            } => {
                self.resource_ref(resource).ok_or(Refusal::NotFound)?;
                self.require_inactive(resource)?;
                let session = self.drop_resource(resource).ok_or(Refusal::NotFound)?;
                let revision = self.bump();
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&[session]));
                Ok((ok(committed), None))
            }
            Command::ResourceRelaunch {
                operation,
                resource,
                expected,
            } => {
                self.check_resource(*resource, *expected)?;
                self.require_inactive(resource)?;
                let live = ResourceId::local(self.next_live());
                let revision = self.bump();
                let instance = self.instance;
                let record = self.resource_mut(resource).ok_or(Refusal::NotFound)?;
                let id = record
                    .activation
                    .map_or(ActivationId::new(1), |a| ActivationId::new(a.id.get() + 1));
                record.activation = Some(ActivationInfo::new(id, instance, ExecState::Running));
                record.live = Some(live.clone());
                record.revision = revision;
                let committed =
                    Committed::new(*operation, revision).with_target(Some(LiveTarget {
                        resource: live,
                        instance,
                        activation: id,
                    }));
                Ok((ok(committed), None))
            }
            Command::InboxResolve {
                operation,
                session,
                expected,
            } => {
                self.check_session(*session, *expected)?;
                let revision = self.bump();
                let record = self.session_mut(session).ok_or(Refusal::NotFound)?;
                let prior = record.awaiting_organization;
                record.awaiting_organization = false;
                record.revision = revision;
                let committed = Committed::new(*operation, revision)
                    .with_prior_awaiting(Some(prior))
                    .with_revisions(self.pins(&[*session]));
                Ok((ok(committed), Some(Inverse::Intake(*session))))
            }
            Command::OrganizationUndo {
                operation,
                undo,
                expected,
            } => {
                let receipt = self
                    .receipts
                    .iter()
                    .find(|r| r.operation == *undo)
                    .cloned()
                    .ok_or(Refusal::NotFound)?;
                let inverse = receipt.inverse.ok_or_else(|| Refusal::Unsupported {
                    capability: "undo of this operation".to_owned(),
                })?;
                for pin in expected {
                    let current = self
                        .sessions
                        .iter()
                        .find(|s| s.key.as_bytes() == &pin.key)
                        .map(|s| s.revision);
                    if let Some(current) = current
                        && current != pin.revision
                    {
                        return Err(Refusal::Conflict { current });
                    }
                }
                let (revision, touched, redo) = match inverse {
                    Inverse::Moves(moves) => {
                        let mut revision = Revision::default();
                        let mut touched = Vec::new();
                        let mut redo = Vec::new();
                        for (resource, session) in moves {
                            let from = self.resource_ref(&resource).map(|r| r.session);
                            revision = self.move_one(&resource, session);
                            touched.push(session);
                            if let Some(from) = from {
                                touched.push(from);
                                redo.push((resource, from));
                            }
                        }
                        (revision, touched, Inverse::Moves(redo))
                    }
                    Inverse::Intake(session) => {
                        let revision = self.bump();
                        let record = self.session_mut(&session).ok_or(Refusal::NotFound)?;
                        record.awaiting_organization = true;
                        record.revision = revision;
                        (revision, vec![session], Inverse::Intake(session))
                    }
                };
                let committed =
                    Committed::new(*operation, revision).with_revisions(self.pins(&touched));
                Ok((ok(committed), Some(redo)))
            }
            Command::ArrangementReplace {
                operation,
                arrangement,
                expected,
                document,
            } => {
                let value = self.arrangement_ref(arrangement).ok_or(Refusal::NotFound)?;
                let (current, owner) = (value.revision, value.owner);
                self.conflict(RecordKey::Arrangement(*arrangement))?;
                if current != *expected {
                    return Err(Refusal::Conflict { current });
                }
                if let ArrangementOwner::Session(session) = owner {
                    for group in &document.groups {
                        for entry in phux_client_core::layout::wire::wire_leaves(&group.tree) {
                            if let EntryTarget::Member(key) = entry.target
                                && self.resource_ref(&key).is_none_or(|r| r.session != session)
                            {
                                return Err(Refusal::InvalidArgument {
                                    field: "document".to_owned(),
                                    reason: "an entry names a resource that is not a member"
                                        .to_owned(),
                                });
                            }
                        }
                    }
                }
                let revision = self.bump();
                let value = self
                    .arrangements
                    .iter_mut()
                    .find(|a| a.key == *arrangement)
                    .ok_or(Refusal::NotFound)?;
                value.document = document.clone();
                value.revision = revision;
                Ok((ok(Committed::new(*operation, revision)), None))
            }
            _ => Err(Refusal::Unsupported {
                capability: "scripted".to_owned(),
            }),
        }
    }

    fn pins(&self, sessions: &[SessionKey]) -> Vec<RecordRevision> {
        let mut pins: Vec<RecordRevision> = Vec::new();
        for session in sessions {
            if let Some(pin) = self.pin_session(session)
                && !pins.iter().any(|p| p.key == pin.key)
            {
                pins.push(pin);
            }
        }
        pins
    }

    fn move_one(&mut self, resource: &ResourceKey, to: SessionKey) -> Revision {
        let from = self.resource_ref(resource).map(|r| r.session);
        if let Some(from) = from {
            self.prune_arrangement(&from, resource);
        }
        let revision = self.bump();
        if let Some(record) = self.resource_mut(resource) {
            record.session = to;
            record.revision = revision;
        }
        if let Some(from) = from {
            self.touch_session(&from);
        }
        self.touch_session(&to);
        revision
    }

    fn terminate_target(&self, target: &LiveTarget) -> Result<ResourceKey, Refusal> {
        let record = self
            .resources
            .iter()
            .find(|r| r.live.as_ref() == Some(&target.resource))
            .ok_or(Refusal::NotFound)?;
        let current = record.activation.map(|a| a.id);
        if current != Some(target.activation) {
            return Err(Refusal::Stale { current });
        }
        Ok(record.key)
    }

    /// End `resource`'s activation with `outcome`, as its process exiting
    /// would: an exit policy of Remove drops the record.
    pub fn end_activation(&mut self, resource: &ResourceKey, outcome: Outcome) {
        let Some(record) = self.resource_ref(resource) else {
            return;
        };
        if record.on_exit == ExitPolicy::Remove {
            self.drop_resource(resource);
            return;
        }
        let revision = self.bump();
        if let Some(record) = self.resource_mut(resource) {
            if let Some(activation) = record.activation.as_mut() {
                activation.state = ExecState::Completed;
                activation.outcome = Some(outcome);
            }
            record.live = None;
            record.revision = revision;
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum RecordKey {
    Session(SessionKey),
    Resource(ResourceKey),
    Arrangement(ArrangementKey),
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

/// The lower-snake-case name [`OrgFixture::refuse_next`] keys on.
const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::SessionCreate { .. } => "session_create",
        Command::SessionRename { .. } => "session_rename",
        Command::SessionSetLifetime { .. } => "session_set_lifetime",
        Command::SessionDelete { .. } => "session_delete",
        Command::SessionKill { .. } => "session_kill",
        Command::ResourceCreate { .. } => "resource_create",
        Command::ResourceMove { .. } => "resource_move",
        Command::ResourceMoveMany { .. } => "resource_move_many",
        Command::ResourceRename { .. } => "resource_rename",
        Command::ResourceTerminate { .. } => "resource_terminate",
        Command::ResourceRemove { .. } => "resource_remove",
        Command::ResourceRelaunch { .. } => "resource_relaunch",
        Command::InboxResolve { .. } => "inbox_resolve",
        Command::OrganizationUndo { .. } => "organization_undo",
        Command::ArrangementReplace { .. } => "arrangement_replace",
        _ => "other",
    }
}
