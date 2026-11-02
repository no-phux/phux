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

use std::collections::HashSet;

use phux_core::ids::{ResourceId, SessionId};
use phux_core::{
    Actor, AgentLaunch, Applied, Command, Committed, ExecState, ExitOutcome, ExitPolicy, FreshKeys,
    InstanceId, LaunchSpec, NewResource, Op, OperationId, Org, Outcome, PrincipalId, ProcIdentity,
    Refusal, ResourceKey, ResourceKind, SessionLifetime, Stamp, TerminalLaunch,
};

use super::{ClientId, ServerState};
use crate::org_store::{MemOrgStore, OrgStore, RECEIPT_CAP, RECEIPT_HORIZON_MS, StoreError};

mod activation;
mod create;
mod journal;

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
    Stamp(super::now_unix_ms())
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
    /// Members a `SESSION_KILL` has already deleted in the store. Reaping
    /// their engines must not write another transition: memory still has the
    /// records, and that write would put the rows back.
    retirement_suppressed: HashSet<ResourceKey>,
}

impl OrgSide {
    pub(super) fn memory() -> Self {
        Self {
            store: Box::new(MemOrgStore::default()),
            keys: OsKeys,
            shutting_down: false,
            loaded: false,
            terminating: std::collections::HashSet::new(),
            retirement_suppressed: HashSet::new(),
        }
    }
}

/// A command whose store write has been decided, before memory mirrors it.
#[must_use]
pub(crate) struct PersistedCommit {
    applied: Applied,
    now: Stamp,
    /// `false` for a replay: the store was not written and memory must stay
    /// as it is.
    stored: bool,
}

impl PersistedCommit {
    /// Whether the store accepted a new transition. A replay is `false`.
    pub(crate) const fn stored(&self) -> bool {
        self.stored
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
    Refusal::StoreFailure {
        reason: error.to_string(),
    }
}

/// A fresh organization for a server that has no store yet: a new
/// authority and its empty Intake, with its empty arrangement.
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

/// Give every session of `org` that has none an empty arrangement: a first
/// start and an import, before the records reach the store. A session a
/// command creates gets its arrangement from core in the same transition.
pub(crate) fn ensure_arrangements(org: &mut Org) {
    org.ensure_arrangements(&mut OsKeys);
}

impl ServerState {
    /// The organization records.
    #[must_use]
    pub const fn org(&self) -> &Org {
        self.sessions.registry.org()
    }

    /// The server run this state stamps on new activations: the bytes of
    /// the wire instance token (ADR-0109), so both name the same run.
    #[must_use]
    pub const fn instance_id(&self) -> InstanceId {
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
    /// (a first start): the authority, its Intake, and any imported tree.
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

    /// Swap in another store, keeping the records, to inject write failures.
    #[cfg(test)]
    pub fn set_org_store(&mut self, store: Box<dyn OrgStore>) {
        self.org.store = store;
    }

    /// Mark that shutdown has begun; later teardown keeps records as they
    /// are for the next start to reconcile.
    pub const fn begin_shutdown(&mut self) {
        self.org.shutting_down = true;
    }

    /// Whether shutdown has begun.
    #[must_use]
    pub const fn shutting_down(&self) -> bool {
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
        let persisted = self.persist_command_from(cmd, actor, 0)?;
        Ok(self.mirror_persisted(persisted, actor, client))
    }

    /// Apply `cmd` and commit it to the store, without mirroring it into
    /// memory. `leading_journal` is how many journal records the caller will
    /// write before this transition's own events (a `SESSION_KILL` journals
    /// each closed engine's `pane_closed` first); the stored position
    /// includes them.
    ///
    /// # Errors
    ///
    /// As [`Self::commit`]. A store failure leaves memory unchanged.
    pub(crate) fn persist_command_from(
        &mut self,
        cmd: &Command,
        actor: &Actor,
        leading_journal: u64,
    ) -> Result<PersistedCommit, Refusal> {
        let now = now_stamp();
        let applied = phux_core::apply(
            self.sessions.registry.org(),
            cmd,
            actor,
            now,
            &mut self.org.keys,
        )?;
        if applied.replayed {
            return Ok(PersistedCommit {
                applied,
                now,
                stored: false,
            });
        }
        // Incremental server intentions must obey the same bounds as decoded
        // whole-document writes, before anything is stored or launched.
        for delta in &applied.transition.deltas {
            if let phux_core::Delta::PutArrangement(arrangement) = delta {
                crate::org_wire::document(&arrangement.groups)
                    .validate_wire()
                    .map_err(|error| Refusal::InvalidArgument {
                        field: "arrangement",
                        reason: error.to_string(),
                    })?;
            }
        }
        // The journal position after this commit's events are recorded,
        // counting records the caller journals ahead of them.
        let journaled = applied
            .transition
            .events
            .iter()
            .filter(|e| crate::org_wire::event(e, self.sessions.registry.org()).is_some())
            .count();
        let journal_seq = self
            .journal
            .head()
            .saturating_add(u64::try_from(journaled).unwrap_or(u64::MAX))
            .saturating_add(leading_journal);
        if let Err(error) = self.org.store.commit(&applied.transition, journal_seq, now) {
            tracing::error!(%error, operation = %cmd.operation, "org store commit failed; refused");
            return Err(store_refusal(&error));
        }
        Ok(PersistedCommit {
            applied,
            now,
            stored: true,
        })
    }

    /// Mirror a [`Self::persist_command_from`] result into the registry and
    /// the journal. A replay changes nothing.
    pub(crate) fn mirror_persisted(
        &mut self,
        persisted: PersistedCommit,
        actor: &Actor,
        client: Option<ClientId>,
    ) -> OrgCommit {
        if !persisted.stored {
            return OrgCommit {
                applied: persisted.applied,
                handles: Committed::default(),
            };
        }
        let PersistedCommit { applied, now, .. } = persisted;
        let handles = self.sessions.registry.commit(&applied.transition);
        if applied.transition.receipt.is_some() {
            let floor = Stamp(now.as_millis().saturating_sub(RECEIPT_HORIZON_MS));
            let _ = self.sessions.registry.prune_receipts(floor, RECEIPT_CAP);
        }
        self.after_commit(&applied.transition, &handles);
        self.journal_org_events(&applied, actor, client);
        OrgCommit { applied, handles }
    }

    /// Hold `retire_record` off these keys until
    /// [`Self::clear_retirement_suppression`].
    pub(crate) fn suppress_retirement(&mut self, keys: &[ResourceKey]) {
        self.org.retirement_suppressed.clear();
        self.org.retirement_suppressed.extend(keys.iter().copied());
    }

    /// Let [`Self::retire_record`] write again.
    pub(crate) fn clear_retirement_suppression(&mut self) {
        self.org.retirement_suppressed.clear();
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
            if let Some(token) = self.forget_resource_bookkeeping(*resource) {
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
}

/// What [`ServerState::reconcile`] recorded.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Activations whose process outlived the previous run.
    pub lost: u32,
    /// Activations that ended unobserved.
    pub unobserved: u32,
    /// Activations whose launch intent committed and never started a process.
    pub never_launched: u32,
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
mod creation_bounds;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
