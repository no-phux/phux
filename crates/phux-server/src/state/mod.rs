#![allow(clippy::nursery)]
//! Server-side state shared by the listener loop and per-client tasks
//! (`phux-byc.4`).
//!
//! This module owns:
//!
//! * The [`Registry`](phux_core::registry::Registry) of sessions, windows,
//!   and panes (the canonical domain state from
//!   `phux-byc.1`/`phux-byc.2`), grouped with its per-session ledgers in
//!   `state::session_table` and reached through [`ServerState::registry`].
//! * The set of currently attached clients ([`AttachedClient`]) keyed by a
//!   server-assigned monotonic [`ClientId`].
//! * The list of subscribers per pane — used to fan diffs out to every client
//!   that is currently observing a pane.
//!
//! Client input is not buffered here: [`TerminalInput`] events flow directly
//! onto the per-pane [`crate::terminal_actor::TerminalActor`]'s input mailbox,
//! which encodes them to PTY bytes (see `runtime::commands`).
//!
//! # Concurrency model
//!
//! The server runs on a `tokio::runtime::Builder::new_current_thread`
//! executor (see `runtime.rs`, ADR-0003 "one server per user, one event
//! loop"). Per-client tasks are spawned via `tokio::task::spawn_local`
//! onto a [`tokio::task::LocalSet`] (per ADR-0014), so per-client
//! futures are `!Send` and can hold `Rc<RefCell<_>>` if desired.
//!
//! [`ServerState`] itself stays behind `Arc<Mutex<_>>` because the
//! [`crate::terminal_actor::TerminalHandle`] held inside `panes` is `Send` and
//! the surrounding [`SharedState`] is used in a few sync contexts
//! (pre-seed before `LocalSet` entry, test scaffolding). Critical sections
//! are short (microseconds: a few `HashMap` ops), so atomic contention
//! is not a concern in steady state. The `std::sync::Mutex` avoids
//! `tokio::sync::Mutex`'s async-friendly futures-park machinery because
//! every section in this module is sync and finite — we never `.await`
//! while holding it.

use std::sync::{Arc, Mutex, MutexGuard};

use phux_protocol::ids::GroupId;

mod agent;
mod agent_tracking;
mod client;
mod client_table;
mod config;
mod construct;
mod cwd;
mod defaults;
mod events;
mod hook_dispatch;
mod hub;
mod hub_state;
mod id_space;
mod input_log;
mod lease_table;
mod leases;
mod lifecycle;
mod lifecycle_state;
mod metadata;
mod policy;
mod reap;
mod session_table;
mod sessions;
mod snapshot;
mod terminal_table;
mod terminals;
mod upgrade_blob;
mod viewport;
mod wire_ids;

use agent_tracking::AgentState;
pub use client::{AttachError, AttachSnapshotPane, AttachedClient, ClientId};
use client_table::ClientTable;
use config::ServerConfig;
pub use events::{EventScope, EventSubscription};
use hub_state::HubState;
pub use id_space::IdSpace;
pub use input_log::{DEFAULT_CLIENT_MAILBOX, Outbound, TerminalInput};
use lease_table::LeaseTable;
pub(crate) use lease_table::SatelliteLease;
use lifecycle_state::Lifecycle;
pub use metadata::{MetadataSetOutcome, MetadataStore, RenameOutcome};
use session_table::SessionTable;
use terminal_table::TerminalTable;
pub use upgrade_blob::RebuildError;

/// Default Group identifier exposed by v0.1 servers.
///
/// The grouping tier is not a wire lifecycle (SPEC §7.3); the server
/// exposes a single static Group that every L3 metadata operation
/// targeting `Scope::Group` lands in. This is load-bearing for the
/// reference TUI's `phux.tui.layout/v1` key — ADR-0019 ties layout
/// persistence to a Group scope and the TUI needs a Group to write into.
pub const DEFAULT_GROUP_ID: GroupId = GroupId::new(1);

/// Opaque process-incarnation identifier advertised during `HELLO_OK`.
///
/// Debug output is redacted so traces cannot accidentally expose a stable
/// cross-connection correlation token.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerIncarnation([u8; 16]);

impl ServerIncarnation {
    #[allow(
        clippy::expect_used,
        reason = "a server cannot safely start without its OS-generated incarnation id"
    )]
    fn random() -> Self {
        let mut bytes = [0; 16];
        getrandom::getrandom(&mut bytes).expect("OS CSPRNG unavailable for server incarnation");
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

/// Single owner of all server-side state.
///
/// See the module-level doc for the concurrency model. Wrap this in
/// [`SharedState`] before sharing with per-client tasks.
#[derive(Debug)]
pub struct ServerState {
    /// The canonical session/window/pane
    /// [`Registry`](phux_core::registry::Registry) plus the per-session
    /// and per-window ledgers keyed on it: last-touch ordering
    /// (`AttachTarget::Last`), frozen session roots, and per-window last
    /// CWDs (the two halves of `defaults.cwd-inheritance`, phux-nyx).
    ///
    /// Accessors stay on this type (see `state::sessions`, `state::cwd`);
    /// the table is an internal grouping, so nothing outside `state` names
    /// it. The registry alone is reachable from outside, through
    /// [`Self::registry`] / [`Self::registry_mut`]. See [`session_table`]
    /// for the per-field documentation.
    sessions: SessionTable,
    /// Everything keyed on a connected client's identity: the attached-client
    /// records, the monotonic [`ClientId`] allocator, per-client negotiated
    /// layers, agent-event subscriptions, peer identities, and the unread
    /// one-shot session-create results.
    ///
    /// Accessors stay on this type (see `state::client`, `state::events`,
    /// `state::policy`, `state::metadata`); the table is an internal
    /// grouping, so nothing outside `state` names it. The attached-client
    /// map alone is readable from outside, through [`Self::attached`]. See
    /// [`client_table`] for the per-field documentation.
    clients: ClientTable,
    /// Everything keyed on a live pane's identity: actor handles, shutdown
    /// tokens, the pane-actor `JoinSet`, per-pane client subscriptions, and
    /// the `ATTACH_TERMINAL` output pumps.
    ///
    /// Accessors stay on this type (see `state::terminals`); the table is
    /// an internal grouping, so nothing outside `state` names it. See
    /// [`terminal_table`] for the per-field documentation, including the
    /// ADR-0014 drop-safety contract on the `JoinSet`.
    terminal_table: TerminalTable,
    /// Both input-lease ledgers (ADR-0033, "take the wheel"): the local
    /// per-pane leases and, on a federation hub, the per-satellite-pane
    /// leases that tell hub consumers apart behind the link's single client
    /// identity (phux-v45.7, L1 §9.1).
    ///
    /// Accessors stay on this type (see `state::leases`); the table is an
    /// internal grouping, so nothing outside `state` names it. Both ledgers
    /// are released together for a departing client by
    /// `LeaseTable::release_all_for`, called from [`Self::detach`]. See
    /// [`lease_table`] for the per-field documentation.
    leases: LeaseTable,
    /// Every core-id ↔ wire-id mapping the server owns (sessions,
    /// terminals, windows) plus the allocators that mint fresh wire ids.
    /// Lives in this crate (and only this crate) because `phux-core` and
    /// `phux-protocol` must not depend on each other — see [`IdSpace`] and
    /// [`crate::id_bridge`] for the allocation contract.
    pub idspace: IdSpace,
    /// Per-scope K/V store backing SPEC §7.4 / §11.L3 metadata.
    ///
    /// Three independently-keyed maps mirror the three `Scope` variants
    /// on the wire. Values are opaque `Vec<u8>`; the server enforces
    /// nothing beyond per-key size limits (currently un-enforced; the
    /// SPEC §11.L3 recommended 256 KiB cap is a follow-up).
    metadata: MetadataStore,
    /// What the server knows about the agents running inside its panes: the
    /// pending-question detector (`phux.agent.asked/v1`, ADR-0046 §D) and
    /// the `phux.agent/v1` record arbiter (ADR-0046 §E).
    ///
    /// Accessors stay on this type (see `state::agent`); the grouping is
    /// internal, so nothing outside `state` names it. Both ledgers are
    /// keyed on a pane and both are cleared in `state::reap`'s cascade. See
    /// [`agent_tracking`] for the per-field documentation.
    agent: AgentState,
    /// Boot-time configuration mirrored from [`crate::runtime::ServerConfig`]
    /// (scrollback cap, cwd-inheritance policy, `TERM`, default shell,
    /// socket path, window-size policy, policy bundle).
    ///
    /// Every field is written once by [`ServerConfig::default`] and once by
    /// the matching `set_*` method during `ServerRuntime::run_async`, before
    /// the accept loops start; none of them changes while the server is
    /// serving. See [`config`] for the per-field documentation.
    config: ServerConfig,
    /// The federation-hub handles this server holds while acting as a hub
    /// (phux-v45, ADR-0007): the validated satellite table, the
    /// per-satellite link statuses, and the per-satellite frame relays.
    ///
    /// Accessors stay on this type (see `state::hub`); the grouping is
    /// internal, so nothing outside `state` names it. All three handles are
    /// `None` on a non-hub server — that is the mode gate, not a
    /// not-yet-initialized marker. See [`hub_state`] for the per-field
    /// documentation.
    hub: HubState,
    /// Server-side event-hook dispatcher handle (`docs/consumers/tui.md`
    /// §9, phux-r82.1). `None` until the runtime spawns the dispatcher
    /// (it does so only when the hook catalog is non-empty), which is
    /// also the default for every test that never configures hooks —
    /// firing an event is then a no-op. Set once at startup via
    /// [`Self::set_hook_dispatcher`].
    hook_dispatcher: Option<crate::hooks::HookDispatcher>,
    /// Everything scoped to this server *process* rather than to anything
    /// it serves: its [`ServerIncarnation`], the open-connection count and
    /// idle clock that drive `--exit-after-idle` (ADR-0063), the
    /// last-session self-exit arming (phux-60s), the monotonic viewport
    /// stamp source, and the graceful-upgrade context (ADR-0032).
    ///
    /// Accessors stay on this type (see `state::lifecycle`,
    /// `state::upgrade_blob`, and [`Self::server_incarnation`]); the
    /// grouping is internal, so nothing outside `state` names it. See
    /// [`lifecycle_state`] for the per-field documentation, including why
    /// the connection count and the idle clock may only be written
    /// together.
    lifecycle: Lifecycle,
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
}

/// Convenience newtype: `Arc<Mutex<ServerState>>`. This is the type
/// per-client tasks clone and hold.
///
/// Usage rules:
/// * Lock for as short as possible — never `.await` while the guard is
///   held. Every section in this crate is sync and finite.
/// * Use [`Self::with`] / [`Self::with_mut`] for scoped access; they
///   panic if the mutex is poisoned (i.e. a previous holder panicked),
///   which is the bug-finding behavior we want at this stage.
#[derive(Debug, Clone, Default)]
pub struct SharedState(Arc<Mutex<ServerState>>);

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

    /// Lock the state. Prefer [`Self::with`] / [`Self::with_mut`] when
    /// possible.
    ///
    /// # Panics
    ///
    /// Panics if the mutex was poisoned (a previous holder panicked while
    /// holding the lock). In a current-thread tokio server that means a
    /// per-client task crashed mid-mutation; the conservative response is
    /// to crash the server rather than continue with potentially
    /// inconsistent state.
    #[allow(clippy::expect_used, reason = "poison panic is the intended behavior")]
    pub fn lock(&self) -> MutexGuard<'_, ServerState> {
        self.0.lock().expect("ServerState mutex poisoned")
    }

    /// Scoped immutable access.
    pub fn with<R>(&self, f: impl FnOnce(&ServerState) -> R) -> R {
        f(&self.lock())
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
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::terminal_actor::TerminalHandle;
    use phux_core::ids::TerminalId;
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, ClientCapabilities, ColorSupport, LayerSet,
    };

    use phux_protocol::wire::frame::{FrameKind, Scope};

    #[test]
    fn server_incarnation_is_stable_per_state_and_distinct_between_states() {
        let first = ServerState::new();
        let stable = first.server_incarnation();
        assert_eq!(first.server_incarnation(), stable);
        assert_eq!(format!("{stable:?}"), "ServerIncarnation(<redacted>)");

        let second = ServerState::new();
        assert_ne!(stable, second.server_incarnation());
    }
    use tokio::sync::{broadcast, mpsc};
    use tokio_util::sync::CancellationToken;

    fn mk_tx() -> mpsc::Sender<Outbound> {
        let (tx, _rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        tx
    }
    fn mk_handle() -> TerminalHandle {
        let (input_tx, _input_rx) = mpsc::channel(8);
        let (snapshot_tx, _snapshot_rx) = mpsc::channel(8);
        let (screen_tx, _screen_rx) = mpsc::channel(8);
        let (upgrade_tx, _upgrade_rx) = mpsc::channel(8);
        let (pwd_tx, _pwd_rx) = mpsc::channel(8);
        let (output_tx, _output_rx_seed) =
            broadcast::channel::<crate::terminal_actor::PaneOutput>(8);
        let (resize_tx, _resize_rx) = mpsc::channel(8);
        let (consumer_attach_tx, _consumer_attach_rx) = mpsc::channel(8);
        let (consumer_detach_tx, _consumer_detach_rx) = mpsc::channel(8);
        let (consumer_ack_tx, _consumer_ack_rx) = mpsc::channel(8);
        let (subscribe_to_events_tx, _subscribe_to_events_rx) = mpsc::channel(8);
        let (unsubscribe_from_events_tx, _unsubscribe_from_events_rx) = mpsc::channel(8);
        TerminalHandle {
            input: input_tx,
            encoded_input: mpsc::channel(8).0,
            input_snapshot: tokio::sync::watch::channel(
                crate::input::InputEncoderSnapshot::default(),
            )
            .1,
            snapshot: snapshot_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: mpsc::channel(8).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication: mpsc::channel(8).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_history: mpsc::channel(8).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_release: mpsc::channel(8).0,
            set_default_colors: mpsc::channel(8).0,
            screen: screen_tx,
            upgrade: upgrade_tx,
            pwd: pwd_tx,
            output: output_tx,
            resize: resize_tx,
            consumer_attach: consumer_attach_tx,
            consumer_detach: consumer_detach_tx,
            consumer_ack: consumer_ack_tx,
            subscribe_to_events: subscribe_to_events_tx,
            unsubscribe_from_events: unsubscribe_from_events_tx,
            control: mpsc::channel(8).0,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn new_client_id_is_monotonic_from_one() {
        let mut s = ServerState::new();
        assert_eq!(s.new_client_id(), ClientId(1));
        assert_eq!(s.new_client_id(), ClientId(2));
        assert_eq!(s.new_client_id(), ClientId(3));
    }

    #[test]
    fn one_shot_session_create_results_are_bounded_and_connection_scoped() {
        let mut state = ServerState::new();
        let client_id = state.new_client_id();
        let scope = Scope::Global;
        for index in 0..257 {
            let key = format!("phux.session.created/v1/{index}");
            let _ = state.metadata_set(&scope, &key, vec![b'x']);
            state.track_session_create_result(client_id, key);
        }

        assert!(
            state
                .metadata()
                .get(&scope, "phux.session.created/v1/0")
                .is_none(),
            "the oldest unread result must be evicted at the cap",
        );
        assert_eq!(
            state
                .clients
                .session_create_results
                .get(&client_id)
                .map(std::collections::VecDeque::len),
            Some(256),
        );

        state.consume_session_create_result("phux.session.created/v1/1");
        assert!(
            state
                .metadata()
                .get(&scope, "phux.session.created/v1/1")
                .is_none(),
        );
        state.detach(client_id);
        assert!(
            state
                .metadata()
                .get(&scope, "phux.session.created/v1/256")
                .is_none(),
            "disconnect cleanup must remove abandoned one-shot results",
        );
        assert!(
            !state
                .clients
                .session_create_results
                .contains_key(&client_id)
        );
    }

    #[test]
    fn attach_unknown_session_returns_error() {
        let mut s = ServerState::new();
        let cid = s.new_client_id();
        let err = s.attach_default_caps(cid, "ghost", mk_tx()).unwrap_err();
        assert_eq!(err, AttachError::UnknownSession("ghost".to_owned()));
    }

    #[test]
    fn attach_records_client_and_subscribes_to_active_pane() {
        let mut s = ServerState::new();
        let (sid, _wid, pid) = s.seed_session("default");
        let cid = s.new_client_id();
        let returned_sid = s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        assert_eq!(returned_sid, sid);
        assert!(s.attached().contains_key(&cid));
        assert_eq!(s.subscribers_for_terminal(pid), &[cid]);
    }

    #[test]
    fn attach_subscribes_to_every_pane_not_just_the_active_one() {
        // phux-fysb.2: a multi-pane client must be subscribed to ALL its panes
        // or the input gate drops keystrokes to non-active panes — the
        // "can't type after re-attach" bug. Before the fix only the active
        // pane was subscribed.
        let mut s = ServerState::new();
        let (sid, _wid, pid1) = s.seed_session("default");
        let pid2 = s
            .add_pane_to_session(sid)
            .expect("add a second pane to the session");
        assert_ne!(pid1, pid2);
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        assert!(
            s.subscribers_for_terminal(pid1).contains(&cid),
            "client not subscribed to first pane"
        );
        assert!(
            s.subscribers_for_terminal(pid2).contains(&cid),
            "client not subscribed to second pane (the regression)"
        );
    }

    // ADR-0033 input-lease state machine (the gate's backing store).

    #[test]
    fn input_open_by_default_blocks_nobody() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let a = s.new_client_id();
        assert_eq!(s.input_lease_holder(pid), None);
        assert!(!s.input_blocked(pid, a), "an Open pane blocks no one");
    }

    #[test]
    fn acquired_lease_blocks_others_not_holder() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let a = s.new_client_id();
        let b = s.new_client_id();
        assert_eq!(
            s.set_input_lease(pid, a),
            None,
            "first acquire has no prior"
        );
        assert_eq!(s.input_lease_holder(pid), Some(a));
        assert!(!s.input_blocked(pid, a), "the holder is never blocked");
        assert!(s.input_blocked(pid, b), "a non-holder is blocked");
    }

    #[test]
    fn seize_returns_prior_holder() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let a = s.new_client_id();
        let b = s.new_client_id();
        s.set_input_lease(pid, a);
        assert_eq!(
            s.set_input_lease(pid, b),
            Some(a),
            "seizing returns the preempted holder"
        );
        assert_eq!(s.input_lease_holder(pid), Some(b));
        assert!(
            s.input_blocked(pid, a),
            "the preempted client is now blocked"
        );
    }

    #[test]
    fn release_is_holder_scoped_and_idempotent() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let a = s.new_client_id();
        let b = s.new_client_id();
        s.set_input_lease(pid, a);
        assert!(!s.release_input_lease(pid, b), "non-holder cannot release");
        assert_eq!(s.input_lease_holder(pid), Some(a));
        assert!(
            s.release_input_lease(pid, a),
            "holder releases its own lease"
        );
        assert_eq!(s.input_lease_holder(pid), None);
        assert!(!s.release_input_lease(pid, a), "double release is a no-op");
    }

    #[test]
    fn detach_releases_the_wheel() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let a = s.new_client_id();
        s.set_input_lease(pid, a);
        assert_eq!(s.leases_held_by(a), vec![pid]);
        s.detach(a);
        assert_eq!(
            s.input_lease_holder(pid),
            None,
            "a disconnect must never strand the wheel"
        );
        assert!(s.leases_held_by(a).is_empty());
    }

    #[test]
    fn same_client_may_rebootstrap_same_session_but_not_switch_sessions() {
        // A second ATTACH naming the SAME session is a re-bootstrap, not an
        // error: the client is renegotiating its profile or recovering. Only
        // switching sessions on one connection is refused.
        let mut s = ServerState::new();
        let (default, window, original) = s.seed_session("default");
        let _ = s.seed_session("other");
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        let added = s
            .registry_mut()
            .new_terminal(window)
            .expect("pane added after initial attach");
        assert_eq!(
            s.attach_default_caps(cid, "default", mk_tx()).unwrap(),
            default,
            "same-connection recovery reuses the live session identity"
        );
        assert_eq!(
            s.subscribers_for_terminal(original),
            &[cid],
            "reattach must not duplicate an existing subscription",
        );
        assert_eq!(
            s.subscribers_for_terminal(added),
            &[cid],
            "reattach must subscribe panes created after the first attach",
        );
        let err = s.attach_default_caps(cid, "other", mk_tx()).unwrap_err();
        assert_eq!(err, AttachError::AlreadyAttached(cid));
    }

    #[test]
    fn attach_stores_hello_selected_bootstrap_contract_unchanged() {
        let mut s = ServerState::new();
        let _ = s.seed_session("default");
        let cid = s.new_client_id();
        let profile = BootstrapProfile::SynthesizedVtStateSync;
        let limits = BootstrapLimits::new(64 * 1024, 128 * 1024).unwrap();
        s.attach(
            cid,
            "default",
            mk_tx(),
            ClientCapabilities::default(),
            profile,
            limits,
        )
        .unwrap();
        let attached = &s.attached()[&cid];
        assert_eq!(attached.bootstrap_profile, profile);
        assert_eq!(attached.bootstrap_limits, limits);
    }

    #[test]
    fn two_clients_attach_same_session_see_same_active_pane() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let a = s.new_client_id();
        let b = s.new_client_id();
        s.attach_default_caps(a, "default", mk_tx()).unwrap();
        s.attach_default_caps(b, "default", mk_tx()).unwrap();
        let subs = s.subscribers_for_terminal(pid);
        assert!(subs.contains(&a) && subs.contains(&b));
        assert_eq!(subs.len(), 2);
    }

    #[test]
    fn resolve_geometry_applies_window_size_policy_across_subscribers() {
        use phux_config::WindowSize;
        use phux_protocol::wire::frame::ViewportInfo;

        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let big = s.new_client_id();
        let small = s.new_client_id();
        s.attach_default_caps(big, "default", mk_tx()).unwrap();
        s.attach_default_caps(small, "default", mk_tx()).unwrap();
        s.set_client_viewport(big, ViewportInfo::new(120, 48));
        s.set_client_viewport(small, ViewportInfo::new(80, 24));

        // smallest: per-axis min — nothing cropped.
        s.set_window_size(WindowSize::Smallest);
        assert_eq!(s.resolve_terminal_geometry(pid, None), Some((80, 24)));

        // largest: per-axis max.
        s.set_window_size(WindowSize::Largest);
        assert_eq!(s.resolve_terminal_geometry(pid, None), Some((120, 48)));

        // latest: the resizing client's viewport (the `latest` hint), not a
        // min/max across subscribers.
        s.set_window_size(WindowSize::Latest);
        assert_eq!(
            s.resolve_terminal_geometry(pid, Some(ViewportInfo::new(100, 30))),
            Some((100, 30)),
        );

        // manual: geometry is never derived from views.
        s.set_window_size(WindowSize::Manual);
        assert_eq!(
            s.resolve_terminal_geometry(pid, Some(ViewportInfo::new(100, 30))),
            None
        );

        // A zero-dimension viewport is ignored, so it can't collapse the grid.
        s.set_window_size(WindowSize::Smallest);
        s.set_client_viewport(small, ViewportInfo::new(0, 0));
        assert_eq!(s.resolve_terminal_geometry(pid, None), Some((120, 48)));
    }

    #[test]
    fn resolve_cell_px_prefers_most_recent_usable_pixel_report() {
        use phux_protocol::wire::frame::ViewportInfo;

        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let retina = s.new_client_id();
        let lodpi = s.new_client_id();
        s.attach_default_caps(retina, "default", mk_tx()).unwrap();
        s.attach_default_caps(lodpi, "default", mk_tx()).unwrap();

        // No viewports yet: no pixel truth.
        assert_eq!(s.resolve_terminal_cell_px(pid), None);

        // A viewport without pixel metrics contributes nothing.
        s.set_client_viewport(retina, ViewportInfo::new(120, 48));
        assert_eq!(s.resolve_terminal_cell_px(pid), None);

        // 120x48 cells over 1920x1440 px -> 16x30 px cells.
        s.set_client_viewport(
            retina,
            ViewportInfo::new(120, 48).with_pixels(Some(1920), Some(1440)),
        );
        assert_eq!(s.resolve_terminal_cell_px(pid), Some((16, 30)));

        // A later report from another display wins on recency...
        s.set_client_viewport(
            lodpi,
            ViewportInfo::new(80, 24).with_pixels(Some(640), Some(384)),
        );
        assert_eq!(s.resolve_terminal_cell_px(pid), Some((8, 16)));

        // ...but a later report WITHOUT usable pixels does not erase the
        // best available truth: degenerate (sub-pixel cell) and absent
        // metrics are both skipped, falling back to the retina report.
        s.set_client_viewport(
            lodpi,
            ViewportInfo::new(80, 24).with_pixels(Some(79), Some(23)),
        );
        assert_eq!(s.resolve_terminal_cell_px(pid), Some((16, 30)));
        s.set_client_viewport(lodpi, ViewportInfo::new(80, 24));
        assert_eq!(s.resolve_terminal_cell_px(pid), Some((16, 30)));

        // Detach drops the donor's report with it.
        s.detach(retina);
        assert_eq!(s.resolve_terminal_cell_px(pid), None);
    }

    #[test]
    fn detach_removes_client_and_drops_empty_subscriber_lists() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        assert!(!s.subscribers_for_terminal(pid).is_empty());
        s.detach(cid);
        assert!(!s.attached().contains_key(&cid));
        assert!(s.subscribers_for_terminal(pid).is_empty());
        assert!(
            s.terminal_table.subscriber_map_is_empty(),
            "empty lists should be GC'd"
        );
    }

    #[test]
    fn detach_is_idempotent() {
        let mut s = ServerState::new();
        let cid = ClientId(99);
        // Not attached at all — must not panic.
        s.detach(cid);
        s.detach(cid);
    }

    #[test]
    fn move_that_empties_source_window_reaps_it_and_its_session() {
        // ADR-0056: the registry re-parent plus the shared empty-window
        // cascade — a cross-session move of a solo pane must reap the
        // emptied source window AND its now-window-less session, exactly
        // as pane death would, while the moved pane survives untouched.
        let mut s = ServerState::new();
        let (sid_a, wid_a, pid_a) = s.seed_session("a");
        let (sid_b, wid_b, _pid_b) = s.seed_session("b");
        let attached = s.new_client_id();
        s.attach_default_caps(attached, "a", mk_tx()).unwrap();

        s.registry_mut()
            .move_terminal(pid_a, wid_b)
            .expect("cross-session move succeeds");
        s.reap_window_if_empty(wid_a);

        assert!(
            s.registry().session(sid_a).is_none(),
            "emptied session reaped"
        );
        assert_eq!(
            s.attached_clients_in_session(sid_a)
                .iter()
                .map(|(client, _)| *client)
                .collect::<Vec<_>>(),
            vec![attached],
            "session-attached clients remain discoverable by stable id after reap"
        );
        assert!(s.registry().session(sid_b).is_some());
        assert_eq!(
            s.registry().terminal(pid_a).expect("pane survives").window,
            wid_b
        );

        // A move that leaves the source window populated reaps nothing.
        let (sid_c, wid_c, pid_c) = s.seed_session("c");
        let pid_c2 = s.registry_mut().new_terminal(wid_c).unwrap();
        s.registry_mut()
            .move_terminal(pid_c2, wid_b)
            .expect("move succeeds");
        s.reap_window_if_empty(wid_c);
        assert!(
            s.registry().session(sid_c).is_some(),
            "populated source kept"
        );
        assert!(s.registry().terminal(pid_c).is_some());
    }

    #[test]
    fn reap_last_pane_empties_server() {
        let mut s = ServerState::new();
        let (sid, _wid, pid) = s.seed_session("default");
        assert_eq!(s.registry().session_count(), 1);

        let server_empty = s.reap_terminal(pid);

        assert!(server_empty, "reaping the only pane must empty the server");
        assert_eq!(s.registry().session_count(), 0);
        assert!(s.registry().session(sid).is_none(), "session cascaded away");
        assert!(s.registry().terminal(pid).is_none());
    }

    #[test]
    fn reap_one_of_two_sessions_keeps_server_alive() {
        let mut s = ServerState::new();
        let (sid_a, _wa, pid_a) = s.seed_session("a");
        let (sid_b, _wb, _pb) = s.seed_session("b");

        let server_empty = s.reap_terminal(pid_a);

        assert!(!server_empty, "a second session is still live");
        assert_eq!(s.registry().session_count(), 1);
        assert!(s.registry().session(sid_a).is_none(), "session a reaped");
        assert!(s.registry().session(sid_b).is_some(), "session b untouched");
    }

    #[test]
    fn reap_pane_in_multipane_window_keeps_session() {
        let mut s = ServerState::new();
        let (sid, wid, pid1) = s.seed_session("default");
        // Add a second pane to the same window so reaping one does not
        // empty the window.
        let pid2 = s.registry_mut().new_terminal(wid).unwrap();

        let server_empty = s.reap_terminal(pid1);

        assert!(!server_empty);
        assert_eq!(s.registry().session_count(), 1);
        assert!(s.registry().session(sid).is_some());
        assert!(s.registry().terminal(pid1).is_none(), "reaped pane gone");
        assert!(
            s.registry().terminal(pid2).is_some(),
            "sibling pane survives"
        );
        assert_eq!(
            s.registry().window(wid).map(|w| w.panes.len()),
            Some(1),
            "window keeps the surviving pane",
        );
    }

    #[test]
    fn reap_is_idempotent_on_unknown_pane() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");

        assert!(s.reap_terminal(pid), "first reap empties the server");
        // Second reap of the now-unknown pane must not panic and must
        // report the server is (still) empty.
        assert!(s.reap_terminal(pid));
        assert_eq!(s.registry().session_count(), 0);
    }

    #[test]
    fn reap_clears_pane_bookkeeping() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        let wire = s.intern_terminal_wire(pid);
        assert!(!s.subscribers_for_terminal(pid).is_empty());
        assert_eq!(s.terminal_from_wire(&wire), Some(pid));

        s.reap_terminal(pid);

        assert!(s.subscribers_for_terminal(pid).is_empty());
        assert!(
            s.terminal_from_wire(&wire).is_none(),
            "wire id retired on reap",
        );
    }

    #[test]
    fn reap_clears_agent_asked_state() {
        let mut s = ServerState::new();
        let (_sid, _wid, pid) = s.seed_session("default");
        s.report_agent_asked(
            pid,
            crate::agent_asked::AskedSource::Hook,
            crate::agent_asked::AskedPayload {
                id: "hook".to_owned(),
                question: "Approve?".to_owned(),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            },
        );
        assert!(s.current_agent_asked(pid).is_some());

        s.reap_terminal(pid);

        assert!(s.current_agent_asked(pid).is_none());
    }

    #[test]
    fn attached_client_color_support_defaults_to_truecolor() {
        // `attach_default_caps` keeps the most-permissive tier — used by
        // tests and any call site that doesn't have HELLO-derived caps
        // in hand.
        let mut s = ServerState::new();
        let _ = s.seed_session("default");
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        let client = s.attached().get(&cid).unwrap();
        assert_eq!(client.client_caps.color_support, ColorSupport::TrueColor);
    }

    #[test]
    fn attach_records_advertised_color_support() {
        // Production path: HELLO advertised a tier, ATTACH consumes it.
        let mut s = ServerState::new();
        let _ = s.seed_session("default");
        let cid = s.new_client_id();
        s.attach(
            cid,
            "default",
            mk_tx(),
            ClientCapabilities::new().with_color_support(ColorSupport::Indexed16),
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
        )
        .unwrap();
        let client = s.attached().get(&cid).unwrap();
        assert_eq!(client.client_caps.color_support, ColorSupport::Indexed16);
    }

    #[test]
    fn set_client_color_support_updates_live_attached_client() {
        // Out-of-order HELLO after ATTACH (out of spec, but tolerated):
        // the setter patches the live record so downsample picks up the
        // newer tier.
        let mut s = ServerState::new();
        let _ = s.seed_session("default");
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        assert!(s.set_client_color_support(cid, ColorSupport::Indexed256));
        let client = s.attached().get(&cid).unwrap();
        assert_eq!(client.client_caps.color_support, ColorSupport::Indexed256);
    }

    #[test]
    fn set_client_color_support_returns_false_for_unknown_client() {
        let mut s = ServerState::new();
        assert!(!s.set_client_color_support(ClientId(999), ColorSupport::Indexed16));
    }

    #[test]
    fn attach_snapshot_panes_collects_live_handles_for_session_tree() {
        let mut s = ServerState::new();
        let (sid, wid, pid_a) = s.seed_session("default");
        let pid_b = s
            .registry_mut()
            .new_terminal(wid)
            .expect("same window second pane");
        let wid_2 = s.registry_mut().new_window(sid).expect("second window");
        let pid_c = s
            .registry_mut()
            .new_terminal(wid_2)
            .expect("pane in second window");

        let _ = s.register_terminal_handle(pid_a, mk_handle(), CancellationToken::new());
        let _ = s.register_terminal_handle(pid_c, mk_handle(), CancellationToken::new());
        // pid_b intentionally has no handle and must be excluded.

        let panes = s.attach_snapshot_panes(sid);
        let ids: HashSet<TerminalId> = panes.iter().map(|p| p.terminal_id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&pid_a));
        assert!(ids.contains(&pid_c));
        assert!(!ids.contains(&pid_b));
        for pane in panes {
            assert_eq!(
                s.terminal_from_wire(&pane.wire_terminal_id),
                Some(pane.terminal_id),
                "wire id should resolve back to the same pane",
            );
        }
    }

    #[test]
    fn most_recently_touched_session_starts_none_and_tracks_touch_order() {
        let mut s = ServerState::new();
        assert!(
            s.most_recently_touched_session().is_none(),
            "fresh state has no prior activity memory",
        );
        let (sid, _wid, _pid) = s.seed_session("default");
        s.touch_session(sid);
        assert_eq!(s.most_recently_touched_session(), Some(sid));

        // Later touches win, regardless of attach order.
        let (sid2, _w, _p) = s.seed_session("other");
        s.touch_session(sid2);
        assert_eq!(s.most_recently_touched_session(), Some(sid2));
        s.touch_session(sid);
        assert_eq!(s.most_recently_touched_session(), Some(sid));
    }

    #[test]
    fn shared_state_with_and_with_mut_round_trip() {
        let shared = SharedState::new();
        let (_sid, _wid, pid) = shared.with_mut(|s| s.seed_session("default"));
        let count = shared.with(|s| s.subscribers_for_terminal(pid).len());
        assert_eq!(count, 0);
    }

    // -------------------------------------------------------------------------
    // L3 metadata tests — SPEC §7.4 / §11.L3 (phux-4li.2).
    //
    // Cover: SUBSCRIBE → SET → broadcast fanout, scope isolation (Terminal
    // vs Group vs Global), non-L3 consumer filtering (§16.4), DELETE
    // tombstone semantics, and the `Unchanged` SET shortcut.
    // -------------------------------------------------------------------------

    fn attach_l3_client(s: &mut ServerState) -> (ClientId, mpsc::Receiver<Outbound>) {
        let _ = s.seed_session("default");
        let cid = s.new_client_id();
        let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        s.attach_default_caps(cid, "default", tx).unwrap();
        s.set_client_layers(cid, LayerSet::all());
        (cid, rx)
    }

    fn attach_l1_only_client(s: &mut ServerState) -> (ClientId, mpsc::Receiver<Outbound>) {
        let cid = s.new_client_id();
        let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        s.attach_default_caps(cid, "default", tx).unwrap();
        s.set_client_layers(cid, LayerSet::new());
        (cid, rx)
    }

    /// Pull every queued frame off `rx` and return the inner frames.
    fn drain_frames(rx: &mut mpsc::Receiver<Outbound>) -> Vec<FrameKind> {
        let mut out = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            let Outbound::Frame(f) = msg else {
                panic!("unexpected terminal outbound sentinel")
            };
            out.push(f);
        }
        out
    }

    #[test]
    fn metadata_subscribe_then_set_broadcasts_matching_key() {
        let mut s = ServerState::new();
        let (cid, mut rx) = attach_l3_client(&mut s);
        let scope = Scope::Group(DEFAULT_GROUP_ID);

        s.metadata_subscribe(cid, scope.clone(), "phux.tui.layout/v1".to_owned());
        let delivered = s.metadata_set(&scope, "phux.tui.layout/v1", b"value-1".to_vec());

        assert_eq!(delivered, vec![cid]);
        let frames = drain_frames(&mut rx);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            FrameKind::MetadataChanged {
                scope: s2,
                key,
                value,
            } => {
                assert_eq!(s2, &scope);
                assert_eq!(key, "phux.tui.layout/v1");
                assert_eq!(value.as_deref(), Some(b"value-1".as_slice()));
            }
            other => panic!("expected MetadataChanged, got {other:?}"),
        }
    }

    #[test]
    fn metadata_set_on_different_key_does_not_fan_to_subscriber() {
        let mut s = ServerState::new();
        let (cid, mut rx) = attach_l3_client(&mut s);
        let scope = Scope::Group(DEFAULT_GROUP_ID);

        s.metadata_subscribe(cid, scope.clone(), "phux.a/v1".to_owned());
        let delivered = s.metadata_set(&scope, "phux.b/v1", b"x".to_vec());

        assert!(delivered.is_empty(), "no subscriber for the b/v1 key");
        assert!(drain_frames(&mut rx).is_empty());
    }

    #[test]
    fn metadata_scope_isolation_terminal_vs_group_vs_global() {
        let mut s = ServerState::new();
        let (cid, mut rx) = attach_l3_client(&mut s);
        let key = "phux.same/v1";
        let t_scope = Scope::Terminal(phux_protocol::ids::TerminalId::local(7));
        let c_scope = Scope::Group(DEFAULT_GROUP_ID);
        let g_scope = Scope::Global;

        // Only subscribe to Group.
        s.metadata_subscribe(cid, c_scope.clone(), key.to_owned());

        // Writes to Terminal and Global must NOT fire the subscriber.
        assert!(s.metadata_set(&t_scope, key, b"t".to_vec()).is_empty());
        assert!(s.metadata_set(&g_scope, key, b"g".to_vec()).is_empty());

        // Write to Group MUST fire it.
        let delivered = s.metadata_set(&c_scope, key, b"c".to_vec());
        assert_eq!(delivered, vec![cid]);

        // And the receiver MUST see exactly one frame (for Group).
        let frames = drain_frames(&mut rx);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            FrameKind::MetadataChanged { scope, value, .. } => {
                assert_eq!(scope, &c_scope);
                assert_eq!(value.as_deref(), Some(b"c".as_slice()));
            }
            other => panic!("expected MetadataChanged, got {other:?}"),
        }
    }

    #[test]
    fn metadata_delete_emits_tombstone_only_if_key_existed() {
        let mut s = ServerState::new();
        let (cid, mut rx) = attach_l3_client(&mut s);
        let scope = Scope::Global;

        s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned());

        // Deleting a missing key is idempotent and silent.
        let delivered = s.metadata_delete(&scope, "phux.k/v1");
        assert!(delivered.is_empty());
        assert!(drain_frames(&mut rx).is_empty());

        // After a SET, DELETE fires the tombstone.
        s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
        drain_frames(&mut rx); // consume the SET broadcast

        let delivered = s.metadata_delete(&scope, "phux.k/v1");
        assert_eq!(delivered, vec![cid]);
        let frames = drain_frames(&mut rx);
        assert_eq!(frames.len(), 1);
        match &frames[0] {
            FrameKind::MetadataChanged {
                value: None,
                key,
                scope: s2,
            } => {
                assert_eq!(key, "phux.k/v1");
                assert_eq!(s2, &scope);
            }
            other => panic!("expected tombstone MetadataChanged, got {other:?}"),
        }
    }

    #[test]
    fn metadata_set_unchanged_value_does_not_broadcast() {
        let mut s = ServerState::new();
        let (cid, mut rx) = attach_l3_client(&mut s);
        let scope = Scope::Global;
        s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned());

        let first = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
        assert_eq!(first, vec![cid]);
        drain_frames(&mut rx);

        let second = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
        assert!(second.is_empty(), "no broadcast on identical write");
        assert!(drain_frames(&mut rx).is_empty());
    }

    #[test]
    fn non_l3_consumer_does_not_receive_metadata_changed() {
        // SPEC §16.4: a non-L3 client (agent / recorder) MUST NOT see any
        // L3 frames. The fanout layer filters by `client_speaks_l3`.
        let mut s = ServerState::new();
        let (l3_cid, mut l3_rx) = attach_l3_client(&mut s);
        let (l1_cid, mut l1_rx) = attach_l1_only_client(&mut s);
        let scope = Scope::Global;

        s.metadata_subscribe(l3_cid, scope.clone(), "phux.k/v1".to_owned());
        // L1-only consumer might still TRY to subscribe via misbehaving
        // client; the dispatch in runtime.rs refuses it. Simulate that by
        // not subscribing through the gated path.
        s.metadata_subscribe(l1_cid, scope.clone(), "phux.k/v1".to_owned());

        let delivered = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
        // Only the L3 client makes it through.
        assert_eq!(delivered, vec![l3_cid]);
        assert_eq!(drain_frames(&mut l3_rx).len(), 1);
        assert!(drain_frames(&mut l1_rx).is_empty());
    }

    #[test]
    fn detach_drops_metadata_subscriptions() {
        let mut s = ServerState::new();
        let (cid, mut rx) = attach_l3_client(&mut s);
        let scope = Scope::Global;
        s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned());

        s.detach(cid);

        let delivered = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
        assert!(delivered.is_empty());
        // Channel returns Err(Disconnected) eventually; just confirm no
        // frame arrived before detach cleanup.
        assert!(drain_frames(&mut rx).is_empty());
    }

    #[test]
    fn metadata_list_returns_keys_sorted_and_scope_isolated() {
        let mut s = ServerState::new();
        let scope_a = Scope::Group(DEFAULT_GROUP_ID);
        let scope_b = Scope::Global;

        s.metadata_set(&scope_a, "zeta", b"z".to_vec());
        s.metadata_set(&scope_a, "alpha", b"a".to_vec());
        s.metadata_set(&scope_a, "mu", b"m".to_vec());
        s.metadata_set(&scope_b, "global-only", b"g".to_vec());

        let keys_a = s.metadata().list(&scope_a);
        assert_eq!(keys_a, vec!["alpha", "mu", "zeta"]);
        let keys_b = s.metadata().list(&scope_b);
        assert_eq!(keys_b, vec!["global-only"]);
    }

    #[test]
    fn metadata_get_returns_stored_value_or_none() {
        let mut s = ServerState::new();
        let scope = Scope::Group(DEFAULT_GROUP_ID);
        s.metadata_set(&scope, "k", b"v".to_vec());
        assert_eq!(s.metadata().get(&scope, "k"), Some(b"v".to_vec()));
        assert_eq!(s.metadata().get(&scope, "missing"), None);
        // Wrong scope: same key returns None.
        assert_eq!(s.metadata().get(&Scope::Global, "k"), None);
    }
}
