//! The organization write path (ADR-0162): [`ServerState::commit`] is the
//! only way a session, resource, or arrangement record changes.
//!
//! Order, all under the state lock: `phux_core::apply` validates the
//! command against the in-memory [`Org`]; the transition, its receipt, and
//! the journal position commit to the [`OrgStore`] in one transaction; only
//! then does the [`phux_core::Registry`] mirror it. A failed store write
//! refuses the command and leaves memory exactly as it was.
//!
//! Executor reports (`ActivationStarted`, `ActivationEnded`,
//! `ActivationLost`) commit as [`Actor::system`]; core refuses them from
//! anyone else.

use phux_core::ids::{ResourceId, SessionId};
use phux_core::{
    Actor, AgentLaunch, Applied, Command, Committed, ExecState, ExitOutcome, ExitPolicy, FreshKeys,
    InstanceId, LaunchSpec, NewResource, Op, OperationId, Org, Outcome, PrincipalId, ProcIdentity,
    Refusal, ResourceKind, SessionLifetime, Stamp, TerminalLaunch,
};

use super::{ClientId, ServerState};
use crate::org_store::{MemOrgStore, OrgStore, RECEIPT_CAP, RECEIPT_HORIZON_MS, StoreError};

/// [`FreshKeys`] from the OS CSPRNG.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsKeys;

impl FreshKeys for OsKeys {
    #[allow(
        clippy::expect_used,
        reason = "a server cannot mint record keys without its OS CSPRNG"
    )]
    fn fresh(&mut self) -> [u8; 16] {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable for a record key");
        bytes
    }
}

/// Wall-clock now as a [`Stamp`]; `0` for a clock before the epoch.
#[must_use]
pub fn now_stamp() -> Stamp {
    Stamp(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|since| u64::try_from(since.as_millis()).ok())
            .unwrap_or(0),
    )
}

/// The store behind [`ServerState::commit`] and whether the server is
/// shutting down.
#[derive(Debug)]
pub(super) struct OrgSide {
    pub(super) store: Box<dyn OrgStore>,
    keys: OsKeys,
    /// Set once shutdown begins: resources torn down by shutdown keep their
    /// records as they are, so the next start reconciles them like a crash.
    shutting_down: bool,
    /// Whether the records came from a store that already held them; a
    /// resume then trusts the store over the blob's copies.
    pub(super) loaded: bool,
    /// Resources a `RESOURCE_TERMINATE` is ending: their reap records the
    /// outcome and leaves the record to its exit policy instead of removing
    /// it, as a legacy kill does.
    terminating: std::collections::HashSet<ResourceId>,
}

impl OrgSide {
    pub(super) fn memory() -> Self {
        Self {
            store: Box::new(MemOrgStore::default()),
            keys: OsKeys,
            shutting_down: false,
            loaded: false,
            terminating: std::collections::HashSet::new(),
        }
    }
}

/// A committed command: what core applied and which live handles the
/// registry allocated or released.
#[derive(Debug, Clone)]
pub struct OrgCommit {
    /// The transition, effects, and reply.
    pub applied: Applied,
    /// Handles gained and lost.
    pub handles: Committed,
}

/// The [`Refusal`] a failed store write turns into. Nothing changed.
fn store_refusal(error: &StoreError) -> Refusal {
    Refusal::Unsupported {
        capability: format!("durable organization store: {error}"),
    }
}

/// A fresh organization for a server that has no store yet: a new
/// authority and its empty Inbox, with its empty arrangement.
#[must_use]
pub fn fresh_org(instance: InstanceId, now: Stamp) -> Org {
    let mut keys = OsKeys;
    let mut org = Org::new(
        phux_core::AuthorityId::from_bytes(keys.fresh()),
        phux_core::SessionKey::from_bytes(keys.fresh()),
        instance,
        now,
    );
    ensure_arrangements(&mut org);
    org
}

/// The session `session` with an arrangement of its own at revision 1, so
/// `ARRANGEMENT_REPLACE` always has a key to name. `placed` members are laid
/// out in one unnamed group; with none, the arrangement has no groups.
fn arranged(
    mut session: phux_core::Session,
    placed: &[phux_core::ResourceKey],
    keys: &mut OsKeys,
) -> (phux_core::Session, phux_core::Arrangement) {
    let key = phux_core::ArrangementKey::from_bytes(keys.fresh());
    session.arrangement = Some(key);
    let arrangement = phux_core::Arrangement {
        key,
        owner: phux_core::ArrangementOwner::Session(session.key),
        groups: first_group(placed, keys).into_iter().collect(),
        revision: phux_core::Revision::FIRST,
    };
    (session, arrangement)
}

/// One unnamed group showing `members` side by side, every leaf an even
/// share; `None` for no members.
fn first_group(members: &[phux_core::ResourceKey], keys: &mut OsKeys) -> Option<phux_core::Group> {
    let mut leaves = members
        .iter()
        .map(|member| phux_core::EntryRef::member(keys.fresh(), *member));
    let mut tree = phux_core::LayoutNode::Leaf(leaves.next()?);
    for (placed, leaf) in (1_u16..).zip(leaves) {
        // The `placed` leaves already in `tree` keep `placed / (placed + 1)`.
        let ratio = f32::from(placed) / (f32::from(placed) + 1.0);
        tree = phux_core::LayoutNode::Split {
            dir: phux_core::SplitDir::Horizontal,
            ratio,
            left: Box::new(tree),
            right: Box::new(phux_core::LayoutNode::Leaf(leaf)),
        };
    }
    Some(phux_core::Group {
        id: keys.fresh(),
        name: None,
        tree,
    })
}

/// Give every session of `org` that has none an empty arrangement: a first
/// start and an import, before the records reach the store.
pub(crate) fn ensure_arrangements(org: &mut Org) {
    let mut keys = OsKeys;
    let mut deltas = Vec::new();
    for session in org.sessions().filter(|s| s.arrangement.is_none()) {
        let (session, arrangement) = arranged(session.clone(), &[], &mut keys);
        deltas.push(phux_core::Delta::PutArrangement(arrangement));
        deltas.push(phux_core::Delta::PutSession(session));
    }
    org.commit(&phux_core::Transition {
        deltas,
        events: Vec::new(),
        receipt: None,
    });
}

/// Give every session `transition` creates an arrangement in the same
/// transaction, with its `ArrangementChanged` event. A session created with
/// its first resource shows it in one group, so every client sees the
/// launched terminal placed; an empty session's arrangement has no groups.
/// Core creates sessions without one; the server adds it before the store
/// sees the transition.
fn arrange_new_sessions(org: &Org, transition: &mut phux_core::Transition, keys: &mut OsKeys) {
    let mut added = Vec::new();
    for delta in &mut transition.deltas {
        if let phux_core::Delta::PutSession(session) = delta
            && session.arrangement.is_none()
            && org.session(&session.key).is_none()
        {
            let members = session.members.clone();
            let (with, arrangement) = arranged(session.clone(), &members, keys);
            *session = with;
            added.push(arrangement);
        }
    }
    for arrangement in added {
        transition
            .events
            .push(phux_core::Event::ArrangementChanged {
                key: arrangement.key,
                owner: arrangement.owner,
                revision: Some(arrangement.revision),
            });
        transition
            .deltas
            .insert(0, phux_core::Delta::PutArrangement(arrangement));
    }
}

impl ServerState {
    /// The organization records.
    #[must_use]
    pub fn org(&self) -> &Org {
        self.sessions.registry.org()
    }

    /// The server run this state stamps on new activations: the bytes of
    /// the wire instance token (ADR-0109), so both name the same run.
    #[must_use]
    pub fn instance_id(&self) -> InstanceId {
        InstanceId::from_bytes(*self.idspace.instance().as_bytes())
    }

    /// Replace the organization and its store wholesale, at startup. Every
    /// record gets a fresh handle; wire-id bridges are the caller's.
    /// `loaded` says the records came from the store itself.
    pub fn install_org(&mut self, store: Box<dyn OrgStore>, mut org: Org, loaded: bool) {
        org.set_instance(self.instance_id());
        self.sessions.registry = phux_core::Registry::new(org);
        self.org.store = store;
        self.org.loaded = loaded;
    }

    /// Write the in-memory organization to a store that has never held one
    /// (a first start): the authority, its Inbox, and any imported tree.
    /// A no-op once the store holds the records.
    ///
    /// # Errors
    ///
    /// The store's write failure; the server must not start without it.
    pub fn persist_first_start(&mut self) -> Result<(), StoreError> {
        if self.org.loaded {
            return Ok(());
        }
        let org = self.sessions.registry.org().clone();
        self.org.store.import(&org, None, now_stamp())?;
        self.org.loaded = true;
        Ok(())
    }

    /// Swap in another store, keeping the records. Tests use it to inject
    /// write failures.
    pub fn set_org_store(&mut self, store: Box<dyn OrgStore>) {
        self.org.store = store;
    }

    /// Mark that shutdown has begun; later teardown keeps records as they
    /// are for the next start to reconcile.
    pub fn begin_shutdown(&mut self) {
        self.org.shutting_down = true;
    }

    /// Whether shutdown has begun.
    #[must_use]
    pub fn shutting_down(&self) -> bool {
        self.org.shutting_down
    }

    /// A fresh operation id for a command the server issues itself.
    pub(crate) fn mint_operation(&mut self) -> OperationId {
        OperationId::from_bytes(self.org.keys.fresh())
    }

    /// The actor a connection's writes are attributed to: the principal its
    /// `HELLO` grant authenticated (ADR-0163), through the client name it
    /// gave. A connection with no grant is the owner's in-process caller.
    #[must_use]
    pub fn actor_for(&self, client: ClientId) -> Actor {
        let (principal, on_behalf_of) = self
            .connection_grant(client)
            .map_or((PrincipalId::Owner, None), |grant| {
                (grant.principal().clone(), grant.on_behalf_of().cloned())
            });
        Actor {
            principal,
            client: self.clients.client_names.get(&client).cloned(),
            on_behalf_of,
        }
    }

    /// Commit `op` under a fresh operation id.
    ///
    /// # Errors
    ///
    /// The [`Refusal`] from core, or one for a failed store write; nothing
    /// changed either way.
    pub fn commit(&mut self, op: Op, actor: &Actor) -> Result<OrgCommit, Refusal> {
        let operation = self.mint_operation();
        self.commit_command(&Command { operation, op }, actor)
    }

    /// An executor report or reconciliation, committed as the server.
    ///
    /// # Errors
    ///
    /// As [`Self::commit`].
    pub fn commit_system(&mut self, op: Op) -> Result<OrgCommit, Refusal> {
        self.commit(op, &Actor::system())
    }

    /// The one mutation path: apply, store, mirror. A retried operation id
    /// with the same payload returns its stored reply and changes nothing.
    ///
    /// # Errors
    ///
    /// As [`Self::commit`].
    pub fn commit_command(&mut self, cmd: &Command, actor: &Actor) -> Result<OrgCommit, Refusal> {
        self.commit_command_from(cmd, actor, None)
    }

    /// [`Self::commit_command`] for a command `client` sent: its journaled
    /// events name that connection beside the actor's principal.
    ///
    /// # Errors
    ///
    /// As [`Self::commit`].
    pub fn commit_command_from(
        &mut self,
        cmd: &Command,
        actor: &Actor,
        client: Option<ClientId>,
    ) -> Result<OrgCommit, Refusal> {
        let now = now_stamp();
        let mut applied = phux_core::apply(
            self.sessions.registry.org(),
            cmd,
            actor,
            now,
            &mut self.org.keys,
        )?;
        arrange_new_sessions(
            self.sessions.registry.org(),
            &mut applied.transition,
            &mut self.org.keys,
        );
        if applied.replayed {
            return Ok(OrgCommit {
                applied,
                handles: Committed::default(),
            });
        }
        // The journal position after this commit's events are recorded.
        let journaled = applied
            .transition
            .events
            .iter()
            .filter(|e| crate::org_wire::event(e, self.sessions.registry.org()).is_some())
            .count();
        let journal_seq = self
            .journal
            .head()
            .saturating_add(u64::try_from(journaled).unwrap_or(u64::MAX));
        if let Err(error) = self.org.store.commit(&applied.transition, journal_seq, now) {
            tracing::error!(%error, operation = %cmd.operation, "org store commit failed; refused");
            return Err(store_refusal(&error));
        }
        let handles = self.sessions.registry.commit(&applied.transition);
        if applied.transition.receipt.is_some() {
            let floor = Stamp(now.as_millis().saturating_sub(RECEIPT_HORIZON_MS));
            let _ = self.sessions.registry.prune_receipts(floor, RECEIPT_CAP);
        }
        self.after_commit(&applied.transition, &handles);
        self.journal_org_events(&applied, actor, client);
        Ok(OrgCommit { applied, handles })
    }

    /// Journal a committed transition's events (ADR-0123), stamped with the
    /// operation and the actor: its principal, and the connection when one
    /// sent the command.
    fn journal_org_events(&mut self, applied: &Applied, actor: &Actor, client: Option<ClientId>) {
        let operation = crate::org_wire::operation(applied.operation);
        let mut stamp = client.map_or_else(
            || crate::org_wire::actor_ref(actor, None),
            |client| self.clients.actor_ref(client),
        );
        stamp = stamp
            .with_principal(crate::org_wire::principal_id(&actor.principal))
            .with_on_behalf_of(
                actor
                    .on_behalf_of
                    .as_ref()
                    .and_then(crate::org_wire::principal_id),
            );
        for event in &applied.transition.events {
            let Some(wire) = crate::org_wire::event(event, self.sessions.registry.org()) else {
                continue;
            };
            let record = super::EventRecord::new(None, wire)
                .with_actor(client)
                .with_actor_ref(Some(stamp.clone()))
                .with_operation_id(operation);
            let _ = self.record_and_fanout(record);
        }
    }

    /// Mark `resource` as ended by `RESOURCE_TERMINATE`: its reap keeps the
    /// record to its exit policy.
    pub(crate) fn mark_terminating(&mut self, resource: ResourceId) {
        self.org.terminating.insert(resource);
    }

    /// Bookkeeping that follows released handles: a removed resource loses
    /// its wire id and tables (cancelling an engine nobody else holds), and
    /// a deleted session loses its wire id, ledgers, and layout keys, and
    /// queues its attached clients for `SESSION_KILLED`.
    fn after_commit(&mut self, transition: &phux_core::Transition, handles: &Committed) {
        for (_, resource) in &handles.resources_removed {
            if let Some(token) = self.forget_terminal_bookkeeping(*resource) {
                token.cancel();
            }
        }
        // A session left with no members drops the layout keys naming it, so
        // a later attach does not adopt panes that no longer exist.
        let emptied: Vec<SessionId> = transition
            .deltas
            .iter()
            .filter_map(|delta| match delta {
                phux_core::Delta::PutSession(s) if s.is_empty() => {
                    self.sessions.registry.session_id(&s.key)
                }
                _ => None,
            })
            .collect();
        for session in emptied {
            let wire = self.idspace.session_wire(session);
            self.forget_layout_keys_of_session(wire);
        }
        for (_, session) in &handles.sessions_removed {
            let wire = self.idspace.session_wire(*session);
            self.sessions.note_removed(*session);
            self.forget_session_bookkeeping(*session);
            self.forget_layout_keys_of_session(wire);
        }
    }

    // ---- creation --------------------------------------------------------

    /// Create a Terminal record, and with `session: None` a new session
    /// named `new_session` around it, in one commit. Returns the session and
    /// the Terminal handles; the activation is `Starting` until
    /// [`Self::activation_started`].
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_terminal(
        &mut self,
        target: TerminalTarget<'_>,
        launch: TerminalLaunch,
        on_exit: ExitPolicy,
        actor: &Actor,
    ) -> Result<(SessionId, ResourceId), Refusal> {
        let resource = NewResource {
            kind: ResourceKind::Terminal,
            name: None,
            launch: Some(LaunchSpec::Terminal(launch)),
            on_exit,
            parent: None,
        };
        let op = match target {
            TerminalTarget::Session(session) => Op::ResourceCreate {
                session: Some(
                    self.sessions
                        .registry
                        .session_key(session)
                        .ok_or(Refusal::NotFound)?,
                ),
                resource,
            },
            TerminalTarget::NewSession { name, lifetime } => Op::SessionCreate {
                name: name.to_owned(),
                lifetime,
                attrs: phux_core::Attrs::default(),
                resource: Some(resource),
            },
        };
        let commit = self.commit(op, actor)?;
        let created = match commit.applied.reply {
            phux_core::Reply::Ok { created, .. } | phux_core::Reply::Accepted { created } => {
                created
            }
        };
        let resource = created
            .resource
            .and_then(|k| self.sessions.registry.resource_id(&k))
            .ok_or(Refusal::NotFound)?;
        let session = self
            .sessions
            .registry
            .session_of(resource)
            .ok_or(Refusal::NotFound)?;
        Ok((session, resource))
    }

    /// Create an empty session record.
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_empty_session(
        &mut self,
        name: &str,
        lifetime: SessionLifetime,
        actor: &Actor,
    ) -> Result<SessionId, Refusal> {
        let commit = self.commit(
            Op::SessionCreate {
                name: name.to_owned(),
                lifetime,
                attrs: phux_core::Attrs::default(),
                resource: None,
            },
            actor,
        )?;
        let phux_core::Reply::Ok { created, .. } = commit.applied.reply else {
            return Err(Refusal::NotFound);
        };
        created
            .session
            .and_then(|k| self.sessions.registry.session_id(&k))
            .ok_or(Refusal::NotFound)
    }

    /// Create an agent conversation bound to Terminal `parent`, in the
    /// parent's session, and record it running: its engine starts in the
    /// same lock.
    ///
    /// # Errors
    ///
    /// The [`Refusal`]; nothing was created.
    pub fn create_agent_session(
        &mut self,
        parent: ResourceId,
        facet: &phux_core::AgentFacet,
        actor: &Actor,
    ) -> Result<ResourceId, Refusal> {
        let parent_key = self
            .sessions
            .registry
            .resource_key(parent)
            .ok_or(Refusal::NotFound)?;
        let session = self
            .sessions
            .registry
            .record(parent)
            .map(|r| r.session)
            .ok_or(Refusal::NotFound)?;
        let commit = self.commit(
            Op::ResourceCreate {
                session: Some(session),
                resource: NewResource {
                    kind: ResourceKind::AgentSession,
                    name: None,
                    launch: Some(LaunchSpec::Agent(AgentLaunch {
                        provider: facet.provider.clone(),
                        native_id: facet.native_id.clone(),
                    })),
                    on_exit: ExitPolicy::Remove,
                    parent: Some(parent_key),
                },
            },
            actor,
        )?;
        let phux_core::Reply::Ok { created, .. } = commit.applied.reply else {
            return Err(Refusal::NotFound);
        };
        let id = created
            .resource
            .and_then(|k| self.sessions.registry.resource_id(&k))
            .ok_or(Refusal::NotFound)?;
        if let Some(agent) = self
            .sessions
            .registry
            .resource_mut(id)
            .and_then(|d| d.agent.as_mut())
        {
            agent.state.clone_from(&facet.state);
        }
        self.activation_started(id, None);
        Ok(id)
    }

    // ---- executor reports -------------------------------------------------

    /// Report that `resource`'s current activation holds a live handle.
    /// A no-op when it is not `Starting` (an already-running import).
    pub fn activation_started(&mut self, resource: ResourceId, proc: Option<ProcIdentity>) {
        let Some((key, activation)) =
            self.current_activation(resource, |s| s == ExecState::Starting)
        else {
            return;
        };
        if let Err(refusal) = self.commit_system(Op::ActivationStarted {
            resource: key,
            activation,
            proc,
        }) {
            tracing::warn!(?resource, %refusal, "activation start not recorded");
        }
    }

    /// Report that `resource`'s current activation ended with an observed
    /// `outcome`. An [`ExitPolicy::Remove`] record goes in the same commit.
    /// A no-op once nothing is current, or during shutdown.
    pub fn activation_ended(&mut self, resource: ResourceId, outcome: ExitOutcome, failed: bool) {
        if self.org.shutting_down {
            return;
        }
        let Some((key, activation)) =
            self.current_activation(resource, |s| s.is_current() || s == ExecState::Lost)
        else {
            return;
        };
        let state = if failed {
            ExecState::Failed
        } else {
            ExecState::Completed
        };
        if let Err(refusal) = self.commit_system(Op::ActivationEnded {
            resource: key,
            activation,
            state,
            outcome: Outcome::Exit(outcome),
        }) {
            tracing::warn!(?resource, %refusal, "activation end not recorded");
        }
    }

    /// End `resource`'s activation if it is still current, then remove its
    /// record: a reap, a kill, a purge. A no-op during shutdown, which
    /// leaves records for the next start to reconcile.
    pub(crate) fn retire_record(&mut self, resource: ResourceId, outcome: ExitOutcome) {
        if self.org.shutting_down {
            return;
        }
        self.activation_ended(resource, outcome, false);
        if self.org.terminating.remove(&resource) {
            return;
        }
        let Some(record) = self.sessions.registry.record(resource) else {
            return;
        };
        let (key, expected) = (record.key, record.revision);
        if let Err(refusal) = self.commit_system(Op::ResourceRemove {
            resource: key,
            expected,
        }) {
            tracing::warn!(?resource, %refusal, "record removal refused");
        }
    }

    fn current_activation(
        &self,
        resource: ResourceId,
        accept: impl Fn(ExecState) -> bool,
    ) -> Option<(phux_core::ResourceKey, phux_core::ActivationId)> {
        let record = self.sessions.registry.record(resource)?;
        let activation = record.activation.as_ref()?;
        accept(activation.state).then_some((record.key, activation.id))
    }

    /// Startup reconciliation (ADR-0162 §5): every activation a previous
    /// run left current or lost, and no engine in this process vouches for,
    /// becomes `Lost` when its process still runs on this boot with the
    /// recorded start time, and `Completed`/`Unobserved` otherwise. Nothing
    /// is relaunched. Idempotent.
    pub fn reconcile(&mut self) -> Reconciled {
        self.reconcile_with(crate::reconcile::boot_id(), crate::reconcile::observe)
    }

    /// [`Self::reconcile`] against a given boot identity and OS probe.
    pub fn reconcile_with(
        &mut self,
        boot: Option<[u8; 16]>,
        observe: impl Fn(i32) -> crate::reconcile::Observed,
    ) -> Reconciled {
        use crate::reconcile::{Verdict, decide};
        let candidates: Vec<_> = self
            .org()
            .resources()
            .filter_map(|r| {
                let a = r.activation.as_ref()?;
                Some((r.key, a.id, a.state, a.host_proc))
            })
            .collect();
        let mut out = Reconciled::default();
        for (key, activation, state, proc) in candidates {
            let vouched = self
                .sessions
                .registry
                .resource_id(&key)
                .is_some_and(|id| self.resource_handle(id).is_some());
            let op = match decide(state, vouched, proc, boot, &observe) {
                Verdict::Keep => continue,
                Verdict::Lost => {
                    out.lost += 1;
                    Op::ActivationLost {
                        resource: key,
                        activation,
                    }
                }
                Verdict::Unobserved => {
                    out.unobserved += 1;
                    Op::ActivationEnded {
                        resource: key,
                        activation,
                        state: ExecState::Completed,
                        outcome: Outcome::Unobserved,
                    }
                }
            };
            if let Err(refusal) = self.commit_system(op) {
                tracing::warn!(resource = %key, %refusal, "reconcile verdict not recorded");
            }
        }
        out
    }

    /// Whether any activation is current: the server still executes work
    /// (R13).
    #[must_use]
    pub fn has_live_activation(&self) -> bool {
        self.org()
            .resources()
            .any(|r| r.activation.as_ref().is_some_and(|a| a.state.is_current()))
    }
}

/// What [`ServerState::reconcile`] recorded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Activations whose process outlived the previous run.
    pub lost: u32,
    /// Activations that ended unobserved.
    pub unobserved: u32,
}

/// Where [`ServerState::create_terminal`] puts a new Terminal.
#[derive(Debug, Clone, Copy)]
pub enum TerminalTarget<'a> {
    /// An existing session.
    Session(SessionId),
    /// A new session, created in the same commit.
    NewSession {
        /// Its unique name.
        name: &'a str,
        /// Its lifetime.
        lifetime: SessionLifetime,
    },
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    /// The arrangement `session` names.
    fn arrangement_of(state: &ServerState, session: SessionId) -> phux_core::Arrangement {
        let key = state
            .registry()
            .session(session)
            .and_then(|s| s.arrangement)
            .expect("every session has an arrangement");
        state.org().arrangement(&key).cloned().expect("arrangement")
    }

    /// A session created with its first terminal shows it in one group from
    /// the creating commit on; an empty session's arrangement has no groups.
    #[test]
    fn a_launched_session_is_created_with_its_terminal_arranged() {
        let mut state = ServerState::new();
        let (session, terminal) = state.seed_session("work");
        let key = state.registry().record(terminal).expect("record").key;
        let arrangement = arrangement_of(&state, session);
        assert_eq!(arrangement.revision, phux_core::Revision::FIRST);
        assert_eq!(arrangement.groups.len(), 1);
        assert_eq!(arrangement.groups[0].name, None);
        assert_eq!(arrangement.resources(), vec![key]);

        let empty = state.seed_empty_session("empty").expect("empty session");
        assert!(arrangement_of(&state, empty).groups.is_empty());
    }

    /// More than one placed member shares the first group evenly.
    #[test]
    fn a_first_group_gives_every_member_an_even_share() {
        let members: Vec<_> = (1..=3_u8)
            .map(|n| phux_core::ResourceKey::from_bytes([n; 16]))
            .collect();
        let group = first_group(&members, &mut OsKeys).expect("a group");
        let phux_core::LayoutNode::Split {
            ratio, ref left, ..
        } = group.tree
        else {
            panic!("three members split: {:?}", group.tree);
        };
        assert!((ratio - 2.0 / 3.0).abs() < f32::EPSILON);
        assert!(matches!(
            **left,
            phux_core::LayoutNode::Split { ratio, .. } if (ratio - 0.5).abs() < f32::EPSILON
        ));
        let arrangement = phux_core::Arrangement {
            key: phux_core::ArrangementKey::from_bytes([9; 16]),
            owner: phux_core::ArrangementOwner::Session(phux_core::SessionKey::from_bytes([8; 16])),
            groups: vec![group],
            revision: phux_core::Revision::FIRST,
        };
        assert_eq!(arrangement.resources(), members);
        assert!(first_group(&[], &mut OsKeys).is_none());
    }

    #[test]
    fn a_failed_store_write_refuses_and_leaves_memory_unchanged() {
        let mut state = ServerState::new();
        let (session, terminal) = state.seed_session("work");
        let store = MemOrgStore::default();
        let fail = store.fail_commits();
        state.set_org_store(Box::new(store));
        let before = state.org().clone();
        let wire_before = state.idspace.session_wire(session);

        fail.store(true, Ordering::Relaxed);
        let refused = state.commit(
            Op::SessionCreate {
                name: "other".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: phux_core::Attrs::default(),
                resource: None,
            },
            &Actor::owner(),
        );
        assert!(
            matches!(refused, Err(Refusal::Unsupported { .. })),
            "{refused:?}"
        );
        state.retire_record(terminal, ExitOutcome::exited(0));
        assert_eq!(state.org(), &before, "no record moved");
        assert!(state.registry().record(terminal).is_some());
        assert_eq!(state.idspace.session_wire(session), wire_before);

        fail.store(false, Ordering::Relaxed);
        state.retire_record(terminal, ExitOutcome::exited(0));
        assert!(state.registry().record(terminal).is_none());
    }

    #[test]
    fn executor_reports_are_refused_from_a_client() {
        let mut state = ServerState::new();
        let (_, terminal) = state.seed_session("work");
        let record = state.registry().record(terminal).unwrap().clone();
        let refused = state.commit(
            Op::ActivationLost {
                resource: record.key,
                activation: phux_core::ActivationId::FIRST,
            },
            &Actor::owner(),
        );
        assert!(matches!(refused, Err(Refusal::Unauthorized { .. })));
    }

    #[test]
    fn a_remove_policy_exit_removes_the_record_and_an_ephemeral_session() {
        let mut state = ServerState::new();
        let (session, terminal) = state
            .create_terminal(
                TerminalTarget::NewSession {
                    name: "quick",
                    lifetime: SessionLifetime::Ephemeral,
                },
                TerminalLaunch::default(),
                ExitPolicy::Remove,
                &Actor::owner(),
            )
            .unwrap();
        state.activation_started(terminal, None);
        assert!(state.has_live_activation());
        state.activation_ended(terminal, ExitOutcome::exited(0), false);
        assert!(state.registry().record(terminal).is_none());
        assert!(state.registry().session(session).is_none());
        assert!(!state.has_live_activation());
    }

    #[test]
    fn a_retain_policy_exit_keeps_the_record_with_its_outcome() {
        let mut state = ServerState::new();
        let (_, terminal) = state
            .create_terminal(
                TerminalTarget::NewSession {
                    name: "kept",
                    lifetime: SessionLifetime::Persistent,
                },
                TerminalLaunch::default(),
                ExitPolicy::Retain,
                &Actor::owner(),
            )
            .unwrap();
        state.activation_ended(terminal, ExitOutcome::exited(3), false);
        let record = state.registry().record(terminal).expect("retained");
        let activation = record.activation.as_ref().unwrap();
        assert_eq!(activation.state, ExecState::Completed);
        assert_eq!(
            activation.outcome,
            Some(Outcome::Exit(ExitOutcome::exited(3)))
        );
    }

    /// ADR-0163 §8: every journaled organization event names a principal:
    /// the one the committing connection authenticated as, or the server.
    #[test]
    fn every_journaled_organization_event_names_its_principal() {
        use phux_protocol::wire::frame::AgentEvent;

        let mut state = ServerState::new();
        let (session, terminal) = state.seed_session("work");
        let owner = ClientId(1);
        state.set_connection_grant(owner, crate::policy::ConnectionGrant::owner());
        let workload = ClientId(2);
        let workload_id = format!("sha256:{}", "a".repeat(64));
        state.set_connection_grant(
            workload,
            crate::policy::ConnectionGrant::scoped(
                phux_protocol::scope::TerminalScopeSet::parse_all(&["*@global"]).unwrap(),
                Some(workload_id.clone()),
            )
            .unwrap(),
        );
        let first = state.journal.head() + 1;

        let key = state.registry().session_key(session).unwrap();
        let expected = state.org().session(&key).unwrap().revision;
        let rename = Command {
            operation: state.mint_operation(),
            op: Op::SessionRename {
                session: key,
                expected,
                name: "renamed".to_owned(),
            },
        };
        let as_owner = state.actor_for(owner);
        state
            .commit_command_from(&rename, &as_owner, Some(owner))
            .unwrap();
        let create = Command {
            operation: state.mint_operation(),
            op: Op::SessionCreate {
                name: "made-by-automation".to_owned(),
                lifetime: SessionLifetime::Persistent,
                attrs: phux_core::Attrs::default(),
                resource: None,
            },
        };
        let as_workload = state.actor_for(workload);
        assert_eq!(
            as_workload.principal,
            PrincipalId::Credential(workload_id.clone())
        );
        state
            .commit_command_from(&create, &as_workload, Some(workload))
            .unwrap();
        state.retire_record(terminal, ExitOutcome::exited(0));

        let principals: Vec<_> = state
            .journal
            .retained()
            .filter(|entry| entry.seq() >= first)
            .filter(|entry| !matches!(entry.event, AgentEvent::TerminalControl { .. }))
            .map(|entry| {
                entry
                    .stamp
                    .actor
                    .as_ref()
                    .and_then(phux_protocol::wire::frame::ActorRef::principal)
                    .unwrap_or_else(|| panic!("no principal on {:?}", entry.event))
            })
            .collect();
        let id = |principal: PrincipalId| crate::org_wire::principal_id(&principal).unwrap();
        assert!(principals.len() >= 3, "{principals:?}");
        assert!(principals.contains(&id(PrincipalId::Owner)));
        assert!(principals.contains(&id(PrincipalId::Credential(workload_id))));
        assert!(principals.contains(&id(PrincipalId::System)));
    }

    #[test]
    fn shutdown_leaves_records_for_the_next_start() {
        let mut state = ServerState::new();
        let (_, terminal) = state.seed_session("work");
        state.begin_shutdown();
        state.retire_record(terminal, ExitOutcome::exited(0));
        assert!(state.registry().record(terminal).is_some());
    }
}
