//! Held actions awaiting approval (ADR-0128, `docs/spec/workload-auth.md`
//! §6.1): the pending table, its server-owned `phux.approval/v1/<id>`
//! records, and the two journaled events.
//!
//! Opening, deciding, expiring, and withdrawing an approval each run in one
//! critical section of the state lock, so the table, the record, and the
//! event always agree. Whichever of them takes an id from the table first is
//! the only one that ends it: that is what makes an approval single-use.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use phux_protocol::ids::{ApprovalId, ResourceId as WireResourceId};
use phux_protocol::kinds::{self, Carrier};
use phux_protocol::wire::frame::{
    ActorRef, AgentEvent, ApprovalOutcome, Command, Scope, TerminalSignal,
};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;

use super::{ClientId, EventRecord, ServerState};

/// A decision on one held action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run the held command once, as the requester.
    Approve,
    /// Refuse it.
    Deny,
    /// The Terminal it names was reaped: refuse it ("terminal gone").
    TerminalGone,
    /// Nobody decided within the TTL: refuse it ("approval expired").
    Expired,
}

impl Decision {
    /// The journaled outcome this decision ends an approval with.
    #[must_use]
    pub const fn outcome(self) -> ApprovalOutcome {
        match self {
            Self::Approve => ApprovalOutcome::Approved,
            Self::Deny => ApprovalOutcome::Denied,
            Self::TerminalGone => ApprovalOutcome::Withdrawn,
            Self::Expired => ApprovalOutcome::Expired,
        }
    }
}

/// One held command awaiting a decision.
#[derive(Debug)]
pub struct PendingApproval {
    /// The connection whose command is held.
    pub requester: ClientId,
    /// The held command, verbatim.
    pub command: Command,
    /// The waiters that run or refuse the command: the one that held it,
    /// then any identical keyed request that joined it.
    waiters: Vec<oneshot::Sender<Decision>>,
}

impl PendingApproval {
    /// Hand `decision` to every waiter. A waiter that is gone (its
    /// connection closed) is owed nothing.
    pub fn deliver(self, decision: Decision) {
        for waiter in self.waiters {
            let _ = waiter.send(decision);
        }
    }
}

/// A newly held action, as the requester's waiter needs it.
#[derive(Debug)]
pub struct OpenedApproval {
    /// The approval's id.
    pub id: ApprovalId,
    /// Where the decision arrives. Closed without a value when the
    /// approval is withdrawn.
    pub decision: oneshot::Receiver<Decision>,
    /// How long the approval waits before it expires.
    pub ttl: Duration,
}

/// Why an action could not be held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldRefusal {
    /// The requester already holds `defaults.approval-max-pending` actions.
    TooManyPending,
    /// The server already holds `defaults.approval-max-pending-total`.
    ServerFull,
    /// The operating system's CSPRNG could not mint an id.
    NoRandomness,
}

/// The pending approvals and their bounds.
#[derive(Debug)]
pub(super) struct ApprovalTable {
    pending: HashMap<ApprovalId, PendingApproval>,
    /// How many of `pending` each requester holds, kept beside the table so
    /// the per-connection bound costs no scan.
    held: HashMap<ClientId, u32>,
    ttl: Duration,
    max_pending: u32,
    max_total: u32,
}

impl Default for ApprovalTable {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            held: HashMap::new(),
            ttl: Duration::from_secs(u64::from(phux_config::DEFAULT_APPROVAL_TTL_SECS)),
            max_pending: phux_config::DEFAULT_APPROVAL_MAX_PENDING,
            max_total: phux_config::DEFAULT_APPROVAL_MAX_PENDING_TOTAL,
        }
    }
}

impl ServerState {
    /// Set the approval bounds (`defaults.approval-ttl-secs`,
    /// `defaults.approval-max-pending`, `defaults.approval-max-pending-total`).
    /// Called once at startup.
    pub fn set_approval_limits(&mut self, ttl: Duration, max_pending: u32, max_total: u32) {
        self.approvals.ttl = ttl;
        self.approvals.max_pending = max_pending;
        self.approvals.max_total = max_total;
    }

    /// The held action `id` names, while it awaits a decision.
    #[must_use]
    pub fn pending_approval(&self, id: ApprovalId) -> Option<&PendingApproval> {
        self.approvals.pending.get(&id)
    }

    /// Every pending approval's id, in no particular order.
    #[must_use]
    pub fn pending_approval_ids(&self) -> Vec<ApprovalId> {
        self.approvals.pending.keys().copied().collect()
    }

    /// Hold `command` for `requester`: mint an id, write its record, and
    /// journal `approval_requested` with the requester as actor.
    ///
    /// # Errors
    ///
    /// [`HoldRefusal::TooManyPending`] at the per-connection bound, and
    /// [`HoldRefusal::NoRandomness`] when no id could be minted.
    pub fn open_approval(
        &mut self,
        requester: ClientId,
        command: &Command,
    ) -> Result<OpenedApproval, HoldRefusal> {
        if self.approvals.pending.len() >= self.approvals.max_total as usize {
            return Err(HoldRefusal::ServerFull);
        }
        if self.held_by(requester) >= self.approvals.max_pending {
            return Err(HoldRefusal::TooManyPending);
        }
        let id = self.mint_approval_id()?;
        let ttl = self.approvals.ttl;
        let record = approval_record(&self.clients.actor_ref(requester), id, command, ttl);
        let _ = self.metadata_set(&Scope::Global, &id.record_key(), record);
        let (sender, decision) = oneshot::channel();
        self.approvals.pending.insert(
            id,
            PendingApproval {
                requester,
                command: command.clone(),
                waiters: vec![sender],
            },
        );
        *self.approvals.held.entry(requester).or_default() += 1;
        self.record_and_fanout(
            EventRecord::new(sole_terminal(command), AgentEvent::ApprovalRequested { id })
                .with_actor(Some(requester)),
        );
        Ok(OpenedApproval { id, decision, ttl })
    }

    /// Join an identical keyed request to the pending hold it repeats
    /// (ADR-0128): the same requester, the same command, the same
    /// `operation_id`. One approval, one execution: the joined waiter hears
    /// the same decision, and L20's dedupe answers it the first run's
    /// result. `Ok(None)` for an unkeyed command or one no hold matches.
    ///
    /// A joined waiter counts against the requester's
    /// `defaults.approval-max-pending` like a hold, so repeats cannot pile up
    /// waiters; its count is released by [`Self::release_joined`] when the
    /// waiter ends, however it ends.
    ///
    /// # Errors
    ///
    /// [`HoldRefusal::TooManyPending`] at the per-connection bound.
    pub fn join_approval(
        &mut self,
        requester: ClientId,
        command: &Command,
    ) -> Result<Option<oneshot::Receiver<Decision>>, HoldRefusal> {
        if command.idempotency_key().is_none() {
            return Ok(None);
        }
        let held = self.held_by(requester);
        let max_pending = self.approvals.max_pending;
        let Some(pending) = self
            .approvals
            .pending
            .values_mut()
            .find(|pending| pending.requester == requester && pending.command == *command)
        else {
            return Ok(None);
        };
        if held >= max_pending {
            return Err(HoldRefusal::TooManyPending);
        }
        let (sender, decision) = oneshot::channel();
        pending.waiters.push(sender);
        *self.approvals.held.entry(requester).or_default() += 1;
        Ok(Some(decision))
    }

    /// Release a joined waiter's count (see [`Self::join_approval`]).
    pub fn release_joined(&mut self, requester: ClientId) {
        self.forget_hold(requester);
    }

    /// How many holds and joined waiters `requester` counts against its
    /// per-connection bound.
    #[must_use]
    pub fn approvals_held_by(&self, requester: ClientId) -> u32 {
        self.held_by(requester)
    }

    /// End approval `id` with `outcome`: remove it, delete its record, and
    /// journal `approval_decided` with `decider` as actor. `None` when it was
    /// no longer pending, so only the first caller ends it.
    pub fn close_approval(
        &mut self,
        id: ApprovalId,
        outcome: ApprovalOutcome,
        decider: Option<ClientId>,
    ) -> Option<PendingApproval> {
        let pending = self.approvals.pending.remove(&id)?;
        self.forget_hold(pending.requester);
        let _ = self.metadata_delete(&Scope::Global, &id.record_key());
        self.record_and_fanout(
            EventRecord::new(
                sole_terminal(&pending.command),
                AgentEvent::ApprovalDecided { id, outcome },
            )
            .with_actor(decider),
        );
        Some(pending)
    }

    /// Withdraw every action `requester` holds: its connection closed, so
    /// nothing runs and nobody is owed a result. Returns how many.
    pub fn withdraw_approvals(&mut self, requester: ClientId) -> usize {
        let ids: Vec<ApprovalId> = self
            .approvals
            .pending
            .iter()
            .filter(|(_, pending)| pending.requester == requester)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            let _ = self.close_approval(*id, ApprovalOutcome::Withdrawn, None);
        }
        ids.len()
    }

    fn held_by(&self, requester: ClientId) -> u32 {
        self.approvals.held.get(&requester).copied().unwrap_or(0)
    }

    fn forget_hold(&mut self, requester: ClientId) {
        let Some(count) = self.approvals.held.get_mut(&requester) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.approvals.held.remove(&requester);
        }
    }

    /// Withdraw every action that names `terminal`, which is being reaped
    /// (ADR-0128): each ends `withdrawn`, journaled before the Terminal's
    /// close, and its requester is refused ("terminal gone"). A batch that
    /// names it is withdrawn whole.
    pub fn withdraw_approvals_naming(&mut self, terminal: &WireResourceId) {
        let ids: Vec<ApprovalId> = self
            .approvals
            .pending
            .iter()
            .filter(|(_, pending)| {
                crate::policy::held_terminals(&pending.command).any(|held| held == terminal)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(pending) = self.close_approval(id, ApprovalOutcome::Withdrawn, None) {
                pending.deliver(Decision::TerminalGone);
            }
        }
    }

    /// A fresh id from the OS CSPRNG. The id is not an authority, but an
    /// unguessable one keeps a refused decider from learning which ids
    /// exist by probing.
    fn mint_approval_id(&self) -> Result<ApprovalId, HoldRefusal> {
        for _ in 0..4 {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| HoldRefusal::NoRandomness)?;
            let fresh =
                ApprovalId::new(bytes).filter(|id| !self.approvals.pending.contains_key(id));
            if let Some(id) = fresh {
                return Ok(id);
            }
        }
        Err(HoldRefusal::NoRandomness)
    }
}

/// The record `phux.approval/v1/<id>` holds (`docs/spec/L3.md` §3.10).
fn approval_record(
    requester: &ActorRef,
    id: ApprovalId,
    command: &Command,
    ttl: Duration,
) -> Vec<u8> {
    let requested_at_ms = now_ms();
    let ttl_ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX);
    let mut record = json!({
        "schema_version": 1,
        "id": id.to_string(),
        "requester": actor_json(requester),
        "method": method_name(command),
        "subjects": subjects(command),
        "requested_at_ms": requested_at_ms,
        "expires_at_ms": requested_at_ms.saturating_add(ttl_ms),
    });
    if let Command::SignalTerminal { signal, .. } = command {
        record["signal"] = Value::from(signal_name(*signal));
    }
    serde_json::to_vec(&record).unwrap_or_default()
}

fn actor_json(actor: &ActorRef) -> Value {
    let mut out = Map::new();
    out.insert("client".to_owned(), Value::from(actor.client.get()));
    if let Some(credential) = &actor.credential_id {
        out.insert("credential_id".to_owned(), Value::from(credential.clone()));
    }
    if let Some(name) = &actor.client_name {
        out.insert("client_name".to_owned(), Value::from(name.clone()));
    }
    Value::Object(out)
}

/// The catalog name of the held command, e.g. `KILL_RESOURCE`.
fn method_name(command: &Command) -> &'static str {
    let rule = kinds::command_rule(command);
    kinds::methods()
        .filter(|method| matches!(method.carrier, Carrier::Command(_)))
        .find(|method| method.rules.iter().any(|row| std::ptr::eq(*row, rule)))
        .map_or("COMMAND", |method| method.name)
}

/// The subjects the held command names, in the registry selector grammar
/// (`terminal:3`, `terminal:<host>/3`, `global`), or `session:<name>` for a
/// forced detach of one session.
fn subjects(command: &Command) -> Vec<String> {
    if let Command::DetachClients {
        session: Some(name),
    } = command
    {
        return vec![format!("session:{name}")];
    }
    let terminals: Vec<String> = crate::policy::held_terminals(command)
        .map(terminal_selector)
        .collect();
    if terminals.is_empty() {
        vec!["global".to_owned()]
    } else {
        terminals
    }
}

/// The one Terminal a held command names, if it names exactly one.
fn sole_terminal(command: &Command) -> Option<WireResourceId> {
    let mut named = crate::policy::held_terminals(command);
    let first = named.next()?;
    named.next().is_none().then(|| first.clone())
}

fn terminal_selector(terminal: &WireResourceId) -> String {
    match terminal {
        WireResourceId::Local { id } => format!("terminal:{id}"),
        WireResourceId::Satellite { host, id } => format!("terminal:{}/{id}", host.as_str()),
    }
}

const fn signal_name(signal: TerminalSignal) -> &'static str {
    match signal {
        TerminalSignal::Interrupt => "interrupt",
        TerminalSignal::Freeze => "freeze",
        TerminalSignal::Resume => "resume",
        TerminalSignal::Terminate => "terminate",
        TerminalSignal::Kill => "kill",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
