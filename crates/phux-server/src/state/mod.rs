//! Server-side state shared by the listener loop and per-client tasks.
//!
//! [`ServerState`] groups the session registry, attached clients, per-pane
//! subscribers, the event journal, metadata, leases, and process-level
//! bookkeeping into internal tables (each file below documents its own).
//! Client input does not pass through here; it goes to the pane actors.
//!
//! The server is one current-thread runtime with a `LocalSet` (ADR-0003,
//! ADR-0014). `ServerState` sits behind `Arc<Mutex<_>>` because it holds
//! `Send` handles and is used in a few sync contexts; critical sections are
//! short and never `.await` with the lock held.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by sibling runtime/state modules"
)]

use std::cell::Cell;
use std::sync::{Arc, Mutex, MutexGuard};

use phux_protocol::ids::GroupId;

mod agent;
mod agent_tracking;
mod approvals;
mod bindings;
mod client;
mod client_table;
mod conditional_kill;
mod config;
mod construct;
mod cwd;
mod defaults;
mod delegations;
mod events;
mod hook_dispatch;
mod hub;
mod hub_state;
mod id_space;
mod journal;
mod layout_translation;
mod lease_table;
mod leases;
mod legacy_snapshot;
mod lifecycle;

static ACTIVATION_REPORT_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub(crate) fn notify_activation_reports() {
    ACTIVATION_REPORT_WAKE.notify_one();
}

pub(crate) async fn activation_reports_woken() {
    ACTIVATION_REPORT_WAKE.notified().await;
}
mod lifecycle_state;
mod metadata;
pub(crate) mod native_agent;
mod org;
mod plugin_startup;
mod policy;
mod reap;
mod remote_listeners;
mod resolve;
mod resource_table;
mod retained;
mod roles;
mod satellite_spawns;
mod session_table;
mod sessions;
mod snapshot;
mod terminals;
mod upgrade_blob;
mod viewport;
mod wire_ids;

use agent_tracking::AgentState;
pub use approvals::{Decision, HoldRefusal, OpenedApproval, PendingApproval};
pub use client::{AttachError, AttachSnapshotPane, AttachedClient, ClientId};
use client_table::ClientTable;
pub use conditional_kill::KillIfRefusal;
use config::ServerConfig;
pub use delegations::UnrestoredDelegation;
pub use events::{
    EventFilter, EventPump, EventScope, EventSubscription, PumpStep, SatelliteScopeChange,
    journal_gap_frame,
};
use hub_state::HubState;
pub use id_space::IdSpace;
pub use journal::EventRecord;
pub(crate) use journal::now_unix_ms;
pub use leases::{LeaseEvidence, LeaseInterval};
pub(crate) use retained::Retention;
pub use retained::{RetainPolicy, close_reason_name, process_exit};
pub use roles::RoleEffects;
// The mailbox payloads live in `crate::mailbox` so `state` and the actor
// share them without depending on each other.
pub use crate::mailbox::{DEFAULT_CLIENT_MAILBOX, Outbound, TerminalInput};
use lease_table::LeaseTable;
pub(crate) use lease_table::SatelliteLease;
use lifecycle_state::Lifecycle;
pub use metadata::{
    KeepEmptyOutcome, MetadataSetOutcome, MetadataStore, RenameOutcome, ServerInterceptedKey,
};
pub(crate) use org::ensure_arrangements;
pub use org::{OrgCommit, OsKeys, Reconciled, TerminalTarget, fresh_org, now_stamp};
pub(crate) use resolve::{RelayRoute, Resolved, ResolvedOwned};
use resource_table::ResourceTable;
use session_table::SessionTable;
pub use upgrade_blob::RebuildError;
pub(crate) use upgrade_blob::{UpgradeHandoffs, request_agent_session_cut};

/// The single static Group every `Scope::Group` metadata op lands in; the
/// TUI's layout key needs one (ADR-0019).
pub const DEFAULT_GROUP_ID: GroupId = GroupId::new(1);

/// Terminal size when no attached view contributes a viewport (seeds, and
/// automatic size policies after the last view leaves).
pub(crate) const HEADLESS_TERMINAL_DIMS: (u16, u16) = (80, 24);

/// Opaque process-incarnation id advertised in `HELLO_OK`; `Debug` is
/// redacted so traces do not leak a stable correlation token.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerIncarnation([u8; 16]);

impl ServerIncarnation {
    #[allow(
        clippy::expect_used,
        reason = "a server cannot safely start without its OS-generated incarnation id"
    )]
    fn random() -> Self {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable for server incarnation");
        Self(bytes)
    }

    /// Borrow the opaque bytes for `HELLO_OK.server_id`.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl core::fmt::Debug for ServerIncarnation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ServerIncarnation(<redacted>)")
    }
}

/// The connection and keyed operation behind a kill, stamped on the
/// `pane_closed` of every resource it closes (`docs/protocol.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloseAttribution {
    /// The connection that sent the kill.
    pub actor: Option<ClientId>,
    /// The kill's `operation_id`, when it carried one (L1 §5.1.1).
    pub operation_id: Option<phux_protocol::ids::IdempotencyKey>,
}

/// Single owner of all server-side state; share it as [`SharedState`].
#[derive(Debug)]
pub struct ServerState {
    /// The organization registry plus per-session ledgers (last touch,
    /// frozen roots, last cwd); see [`session_table`].
    sessions: SessionTable,
    /// The organization store behind [`ServerState::commit`]; see [`org`].
    org: org::OrgSide,
    /// Everything keyed on a connected client; see [`client_table`].
    clients: ClientTable,
    /// The event journal (ADR-0123): one `seq` for every event and the
    /// replay ring, written only under this lock so order and snapshot cuts
    /// agree.
    journal: journal::Journal,
    /// Everything keyed on a live pane (handles, tokens, actor `JoinSet`,
    /// subscriptions, output pumps); see [`resource_table`].
    resources: ResourceTable,
    /// Local and satellite input-lease ledgers (ADR-0033, L1 §9.1),
    /// released together on detach; see [`lease_table`].
    leases: LeaseTable,
    /// Live delegated agent credentials (ADR-0163 §5); see
    /// [`crate::delegation`].
    delegations: crate::delegation::DelegationTable,
    /// On a hub, who spawned each satellite resource and whether another
    /// consumer attached it since (ADR-0109).
    satellite_spawns: satellite_spawns::SatelliteSpawnLedger,
    /// Every core-id ↔ wire-id mapping and the wire-id allocators (see
    /// [`IdSpace`]).
    pub idspace: IdSpace,
    /// Per-scope L3 metadata store (SPEC §7.4); values are opaque.
    metadata: MetadataStore,
    /// Agent tracking: the asked detector (ADR-0046 §D) and the record
    /// arbiter (§E); see [`agent_tracking`].
    agent: AgentState,
    /// Boot-time configuration, set before the accept loops start and never
    /// changed; see [`config`].
    config: ServerConfig,
    /// Hub handles (ADR-0007); all `None` on a non-hub server. See
    /// [`hub_state`].
    hub: HubState,
    /// Satellite uplinks this server keeps to hubs (ADR-0182).
    uplinks: crate::uplink::Uplinks,
    /// Event-hook dispatcher; `None` when no hooks are configured (firing
    /// is then a no-op).
    hook_dispatcher: Option<crate::hooks::HookDispatcher>,
    /// `[[startup]]` supervisor. `None` until the accept loop's neighbor
    /// task is installed, and on servers that were not given a catalog.
    plugin_startup: Option<crate::plugin_startup::StartupHandle>,
    /// Who wrote each `phux.push/v1/<device>` grant, and what was last
    /// pushed to it (ADR-0155).
    push_grants: crate::push::PushGrants,
    /// Process-scoped state: incarnation, connection count and idle clock
    /// (ADR-0063), self-exit arming, viewport stamps, upgrade context. See
    /// [`lifecycle_state`].
    lifecycle: Lifecycle,
    /// Why each closing resource is closing (ADR-0104 §4), from the
    /// decision to its `RESOURCE_CLOSED`; see [`bindings`] for the
    /// one-closer rule.
    close_reasons: std::collections::HashMap<
        phux_core::ids::ResourceId,
        phux_protocol::wire::frame::CloseReason,
    >,
    /// Who and which keyed operation closed a resource, for `pane_closed`
    /// (`docs/protocol.md#receipts-retries-and-backpressure`); lives from kill to reap.
    close_attributions: std::collections::HashMap<phux_core::ids::ResourceId, CloseAttribution>,
    /// Every resource whose exit watcher still runs, with its wire id once
    /// a kill reaped it ahead of its process (`docs/protocol.md#commands-events-resources`); see
    /// [`bindings`].
    exit_watches: std::collections::HashMap<
        phux_core::ids::ResourceId,
        Option<phux_protocol::ids::ResourceId>,
    >,
    /// Retain-on-exit bookkeeping (ADR-0124); see [`retained`].
    retained: retained::RetainedTable,
    /// Remote listener bind outcomes for `GET_STATE` and `phux doctor`.
    remote_listeners: phux_protocol::wire::RemoteListenersReport,
    /// The operation dedupe record (ADR-0053, ADR-0126), with its own lock
    /// so the input lane need not take this one.
    operation_dedupe: crate::runtime::operation_dedupe::OperationDedupe,
    /// Held `SIGNAL` actions awaiting a decision (ADR-0128).
    approvals: approvals::ApprovalTable,
    /// Dedicated controller state and exclusive SDK stream authority.
    native_agents: native_agent::NativeAgents,
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerState {
    /// Return this state's stable, redaction-safe process incarnation.
    #[must_use]
    pub const fn server_incarnation(&self) -> ServerIncarnation {
        self.lifecycle.server_incarnation()
    }

    /// The push-grant registry (ADR-0155).
    pub(crate) const fn push_grants(&self) -> &crate::push::PushGrants {
        &self.push_grants
    }

    /// Mutable push-grant registry (ADR-0155).
    pub(crate) const fn push_grants_mut(&mut self) -> &mut crate::push::PushGrants {
        &mut self.push_grants
    }

    /// The shared operation dedupe record. Cloning the handle shares it.
    pub(crate) const fn operation_dedupe(
        &self,
    ) -> &crate::runtime::operation_dedupe::OperationDedupe {
        &self.operation_dedupe
    }
}

/// `Arc<Mutex<ServerState>>`, cloned by per-client tasks. Never `.await`
/// while holding the guard; a poisoned lock panics by design.
#[derive(Debug, Clone, Default)]
pub struct SharedState(Arc<Mutex<ServerState>>);

/// The `ServerState` this thread's [`SharedState::with`] is borrowing, so a
/// nested `with` on the same state does not lock again.
#[derive(Clone, Copy)]
struct BorrowedState {
    state: *const ServerState,
    arc: usize,
}

thread_local! {
    static BORROWED_STATE: Cell<BorrowedState> = const {
        Cell::new(BorrowedState {
            state: std::ptr::null(),
            arc: 0,
        })
    };
}

/// Restores the previous [`BORROWED_STATE`] cell, including on unwind.
struct ClearBorrowed(BorrowedState);

impl Drop for ClearBorrowed {
    fn drop(&mut self) {
        BORROWED_STATE.with(|cell| cell.set(self.0));
    }
}

impl SharedState {
    /// Wrap a fresh [`ServerState`].
    #[must_use]
    pub fn new() -> Self {
        #[allow(
            clippy::arc_with_non_send_sync,
            reason = "single-threaded current-thread runtime; Mutex+Arc safety not required"
        )]
        let state = Arc::new(Mutex::new(ServerState::new()));
        Self(state)
    }

    /// Lock the state (prefer [`Self::with`] / [`Self::with_mut`]).
    ///
    /// # Panics
    ///
    /// If a previous holder panicked mid-mutation; continuing with
    /// possibly inconsistent state is worse.
    #[allow(clippy::expect_used, reason = "poison panic is the intended behavior")]
    pub fn lock(&self) -> MutexGuard<'_, ServerState> {
        self.0.lock().expect("ServerState mutex poisoned")
    }

    /// Scoped immutable access. Reentrant on this thread for the same state:
    /// a nested [`Self::with`] reuses the guard instead of locking again.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard backs the borrowed-state pointer until `f` returns"
    )]
    pub fn with<R>(&self, f: impl FnOnce(&ServerState) -> R) -> R {
        let arc = Arc::as_ptr(&self.0) as usize;
        let current = BORROWED_STATE.with(Cell::get);
        if current.arc == arc && !current.state.is_null() {
            // SAFETY: this thread's outer `with` stored `current.state` and
            // holds that mutex guard until it restores the previous cell.
            // The Arc address matches, so the pointer is this state, and the
            // outer borrow is shared.
            return f(unsafe { &*current.state });
        }
        let guard = self.lock();
        BORROWED_STATE.with(|cell| {
            cell.set(BorrowedState {
                state: std::ptr::from_ref(&*guard),
                arc,
            });
        });
        let restore = ClearBorrowed(current);
        let result = f(&guard);
        drop(restore);
        result
    }

    /// Scoped mutable access.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut ServerState) -> R) -> R {
        f(&mut self.lock())
    }
}

#[cfg(test)]
#[allow(
    clippy::match_same_arms,
    clippy::single_match_else,
    clippy::assertions_on_constants,
    clippy::bool_assert_comparison,
    clippy::eq_op
)]
mod tests;
