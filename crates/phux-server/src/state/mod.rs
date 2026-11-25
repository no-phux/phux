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

mod app_slots;
mod approvals;
mod bindings;
mod client;
mod client_table;
mod conditional_kill;
mod config;
mod construct;
mod controller;
pub(crate) mod credentials;
mod cwd;
mod delegations;
mod events;
mod federation;
mod hook_dispatch;
mod id_space;
mod journal;
mod layout_translation;
mod lease_table;
mod leases;
mod legacy_snapshot;

static ACTIVATION_REPORT_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

pub(crate) fn notify_activation_reports() {
    ACTIVATION_REPORT_WAKE.notify_one();
}

pub(crate) async fn activation_reports_woken() {
    ACTIVATION_REPORT_WAKE.notified().await;
}
mod lifecycle_state;
mod metadata;
mod org;
mod plugin_startup;
mod policy;
mod reap;
mod registration;
mod remote_listeners;
mod resolve;
mod resource_table;
mod retained;
mod roles;
mod session_table;
mod sessions;
mod snapshot;
pub(crate) mod unseen;
pub(crate) mod upgrade_blob;
mod viewport;

pub(crate) use app_slots::AppSlot;
pub(crate) use org::launch::execute_terminal as execute_admitted_terminal;
pub(crate) use org::operation as org_operation;
pub use approvals::{Decision, HoldRefusal, OpenedApproval, PendingApproval};
pub(crate) use bindings::ClosePlan;
pub use client::{AttachError, AttachSnapshotPane, AttachedClient, ClientId};
use client_table::ClientTable;
pub use conditional_kill::KillIfRefusal;
pub(crate) use config::StateConfig;
pub use delegations::UnrestoredDelegation;
pub use events::{
    EventFilter, EventPump, EventScope, EventSubscription, PumpStep, SatelliteScopeChange,
    journal_gap_frame,
};
pub(crate) use federation::{Federation, SatelliteLease};
pub use id_space::IdSpace;
pub use journal::EventRecord;
pub(crate) use journal::now_unix_ms;
pub(crate) use retained::Retention;
pub use retained::{RetainPolicy, close_reason_name, process_exit};
pub use roles::RoleEffects;
// The mailbox payloads live in `crate::mailbox` so `state` and the actor
// share them without depending on each other.
pub use crate::mailbox::{DEFAULT_CLIENT_MAILBOX, Outbound};
use lease_table::LeaseTable;
use lifecycle_state::Lifecycle;
pub use metadata::{
    KeepEmptyOutcome, MetadataSetOutcome, MetadataStore, RenameOutcome, ServerInterceptedKey,
};
pub(crate) use org::ensure_arrangements;
pub use org::{
    ActivationToken, OrgCommit, OsKeys, Reconciled, TerminalTarget, fresh_org, now_stamp,
};
pub use registration::ResourceRegistrationError;
pub(crate) use resolve::{RelayRoute, Resolved, ResolvedOwned};
use resource_table::ResourceTable;
use session_table::SessionTable;
pub use upgrade_blob::RebuildError;

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
    /// The kill's `operation_id`, when it carried one.
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
    /// The local input-lease ledger (ADR-0033), released together with the
    /// satellite ledger in [`Federation`] on detach; see [`lease_table`].
    leases: LeaseTable,
    /// Live delegated agent credentials (ADR-0163 §5); see
    /// [`crate::delegation`].
    delegations: crate::delegation::DelegationTable,
    /// Every core-id ↔ wire-id mapping and the wire-id allocators (see
    /// [`IdSpace`]).
    pub idspace: IdSpace,
    /// Per-scope L3 metadata store; values are opaque.
    metadata: MetadataStore,
    /// Boot-time configuration, built before the state and never changed;
    /// see [`config`].
    config: StateConfig,
    /// Hub-only state (ADR-0007): satellite table, relays, links, spawn
    /// ledger, and satellite leases; empty on a non-hub server. See
    /// [`federation`].
    federation: Federation,
    /// Satellite uplinks this server keeps to hubs (ADR-0182).
    uplinks: crate::uplink::Uplinks,
    /// The ssh-agent broker (`docs/ssh.md#ssh-agent`).
    forward_agent: crate::runtime::forward::agent::Broker,
    /// Links panes asked to open, waiting on a client's answer.
    open_urls: crate::runtime::open_url::OpenUrls,
    /// Event-hook dispatcher; `None` when no hooks are configured (firing
    /// is then a no-op).
    hook_dispatcher: Option<crate::hooks::HookDispatcher>,
    /// `[[startup]]` supervisor. `None` until the accept loop's neighbor
    /// task is installed, and on servers that were not given a catalog.
    plugin_startup: Option<crate::plugin_startup::StartupHandle>,
    /// Rung on a config reload to re-read `[[mirrors]]`; `None` until the
    /// standing-mirror supervisor is installed.
    standing_mirrors: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// Who wrote each `phux.push/v1/<device>` grant, and what was last
    /// pushed to it (ADR-0155).
    push_grants: crate::push::PushGrants,
    /// Process-scoped state: incarnation, connection count and idle clock
    /// (ADR-0063), self-exit arming, viewport stamps, upgrade context. See
    /// [`lifecycle_state`].
    lifecycle: Lifecycle,
    /// Each close's reason and attribution, from admission to reap.
    pending_closes: std::collections::HashMap<phux_core::ids::ResourceId, bindings::PendingClose>,
    /// Every resource whose exit watcher still runs, with its wire id once
    /// a kill reaped it ahead of its process (`docs/protocol.md#commands-events-resources`); see
    /// [`bindings`].
    exit_watches: std::collections::HashMap<
        (phux_core::ids::ResourceId, ActivationToken),
        Option<phux_protocol::ids::ResourceId>,
    >,
    /// Retain-on-exit bookkeeping (ADR-0124); see [`retained`].
    retained: retained::RetainedTable,
    /// Remote listener bind outcomes for `GET_STATE` and `phux doctor`.
    remote_listeners: phux_protocol::wire::RemoteListenersReport,
    /// The configured QUIC listener's failed replies, read into
    /// [`Self::remote_listeners`].
    quic_replies: Option<std::sync::Arc<crate::transport::quic::ReplyFailures>>,
    /// The operation dedupe record (ADR-0053, ADR-0126), with its own lock
    /// so the input lane need not take this one.
    operation_dedupe: crate::runtime::operation_dedupe::OperationDedupe,
    /// Held `SIGNAL` actions awaiting a decision (ADR-0128).
    approvals: approvals::ApprovalTable,
    pub(crate) credentials: credentials::CredentialBroker,
    /// Each app's own state, which the host holds without knowing its
    /// type; see [`app_slots`].
    app_slots: app_slots::AppSlots,
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerState {
    /// The push-grant registry (ADR-0155).
    pub(crate) const fn push_grants(&self) -> &crate::push::PushGrants {
        &self.push_grants
    }

    /// Mutable push-grant registry (ADR-0155).
    pub(crate) const fn push_grants_mut(&mut self) -> &mut crate::push::PushGrants {
        &mut self.push_grants
    }

    /// The boot-time configuration this state was built with.
    #[must_use]
    pub(crate) const fn config(&self) -> &StateConfig {
        &self.config
    }

    /// Mutable configuration, for tests that vary it after construction.
    #[cfg(test)]
    pub(crate) const fn config_mut(&mut self) -> &mut StateConfig {
        &mut self.config
    }

    /// Hub-only state; empty on a non-hub server.
    #[must_use]
    pub(crate) const fn federation(&self) -> &Federation {
        &self.federation
    }

    /// Mutable [`Self::federation`].
    pub(crate) const fn federation_mut(&mut self) -> &mut Federation {
        &mut self.federation
    }

    /// The uplinks this server keeps to hubs as a satellite (ADR-0182).
    #[must_use]
    pub(crate) const fn uplinks(&self) -> &crate::uplink::Uplinks {
        &self.uplinks
    }

    /// Mutable [`Self::uplinks`], to arm, start, and stop them.
    pub(crate) const fn uplinks_mut(&mut self) -> &mut crate::uplink::Uplinks {
        &mut self.uplinks
    }

    /// The ssh-agent broker.
    pub(crate) const fn forward_agent(&self) -> &crate::runtime::forward::agent::Broker {
        &self.forward_agent
    }

    /// Mutable [`Self::forward_agent`].
    pub(crate) const fn forward_agent_mut(
        &mut self,
    ) -> &mut crate::runtime::forward::agent::Broker {
        &mut self.forward_agent
    }

    /// The links waiting on a client's answer.
    pub(crate) const fn open_urls_mut(&mut self) -> &mut crate::runtime::open_url::OpenUrls {
        &mut self.open_urls
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

    /// Wrap a fresh [`ServerState`] built with `config`.
    #[must_use]
    pub(crate) fn with_config(config: StateConfig) -> Self {
        #[allow(
            clippy::arc_with_non_send_sync,
            reason = "single-threaded current-thread runtime; Mutex+Arc safety not required"
        )]
        let state = Arc::new(Mutex::new(ServerState::with_config(config)));
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
