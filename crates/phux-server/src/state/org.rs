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
}

impl OrgSide {
    pub(super) fn memory() -> Self {
        Self {
            store: Box::new(MemOrgStore::default()),
            keys: OsKeys,
            shutting_down: false,
            loaded: false,
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
/// authority and its empty Inbox.
#[must_use]
pub fn fresh_org(instance: InstanceId, now: Stamp) -> Org {
    let mut keys = OsKeys;
    Org::new(
        phux_core::AuthorityId::from_bytes(keys.fresh()),
        phux_core::SessionKey::from_bytes(keys.fresh()),
        instance,
        now,
    )
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

    /// The actor a connection's writes are attributed to: the owner,
    /// through the client name it gave at `HELLO`.
    #[must_use]
    pub fn actor_for(&self, client: ClientId) -> Actor {
        Actor {
            principal: PrincipalId::Owner,
            client: self.clients.client_names.get(&client).cloned(),
            on_behalf_of: None,
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
        let now = now_stamp();
        let applied = phux_core::apply(
            self.sessions.registry.org(),
            cmd,
            actor,
            now,
            &mut self.org.keys,
        )?;
        if applied.replayed {
            return Ok(OrgCommit {
                applied,
                handles: Committed::default(),
            });
        }
        let journal_seq = self.journal.head();
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
        Ok(OrgCommit { applied, handles })
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

    #[test]
    fn shutdown_leaves_records_for_the_next_start() {
        let mut state = ServerState::new();
        let (_, terminal) = state.seed_session("work");
        state.begin_shutdown();
        state.retire_record(terminal, ExitOutcome::exited(0));
        assert!(state.registry().record(terminal).is_some());
    }
}
