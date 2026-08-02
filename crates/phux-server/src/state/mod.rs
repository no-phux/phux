#![allow(clippy::nursery)]
//! Server-side state shared by the listener loop and per-client tasks
//! (`phux-byc.4`).
//!
//! This module owns:
//!
//! * The [`Registry`] of sessions, windows, and panes (the canonical
//!   domain state from `phux-byc.1`/`phux-byc.2`).
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::agent_asked::AskedDetector;
use crate::agent_state::AgentRecordArbiter;
use crate::id_bridge::IdBridge;
use crate::terminal_actor::TerminalHandle;
use phux_core::ids::{SessionId, TerminalId, WindowId};
use phux_core::registry::Registry;
use phux_protocol::caps::LayerSet;
use phux_protocol::ids::{GroupId, TerminalId as WireTerminalId, WindowId as WireWindowId};
use portable_pty::CommandBuilder;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

mod client;
mod events;
mod input_log;
mod metadata;
mod registry;
mod upgrade_blob;

pub use client::{AttachError, AttachSnapshotPane, AttachedClient, ClientId};
pub use events::{EventScope, EventSubscription};
pub use input_log::{DEFAULT_CLIENT_MAILBOX, Outbound, TerminalInput};
pub use metadata::{MetadataSetOutcome, MetadataStore, RenameOutcome};
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

/// One hub-side satellite input lease (phux-v45.7, phux-v45.13).
///
/// Records which hub consumer holds the relayed ADR-0033 lease over a
/// satellite terminal **and** that consumer's outbound mailbox. The
/// mailbox is what lets a SEIZE takeover by a *different* hub consumer
/// notify the evicted prior holder directly (a hub-synthesized
/// `TerminalControl(Seized)` event, mirroring the local takeover
/// broadcast) — the satellite cannot do it, because every hub consumer
/// reaches it through the link's single client identity, so its own lease
/// change reads as a same-identity re-acquire.
#[derive(Debug, Clone)]
pub(crate) struct SatelliteLease {
    /// The hub consumer that holds the lease.
    pub(crate) holder: ClientId,
    /// The holder's outbound mailbox, for the eviction notification.
    pub(crate) out_tx: tokio::sync::mpsc::Sender<Outbound>,
}

/// Single owner of all server-side state.
///
/// See the module-level doc for the concurrency model. Wrap this in
/// [`SharedState`] before sharing with per-client tasks.
#[derive(Debug)]
pub struct ServerState {
    /// Random identity for this in-memory state incarnation.
    server_incarnation: ServerIncarnation,
    /// Canonical domain state.
    pub registry: Registry,
    /// Currently attached clients, keyed by server-assigned id.
    pub attached: HashMap<ClientId, AttachedClient>,
    /// For each pane, the clients currently observing it (and thus eligible
    /// to receive `TERMINAL_OUTPUT` frames for it).
    pub terminal_subscribers: HashMap<TerminalId, Vec<ClientId>>,
    /// Per-pane input lease (ADR-0033, "take the wheel"). When a pane has an
    /// entry, only that `ClientId`'s input reaches the PTY; everyone else's
    /// `INPUT_*` / `ROUTE_INPUT` is dropped at the gate (still acked, per the
    /// fire-and-forget input invariant). Absent = `Open`: any subscriber's
    /// input passes (the back-compat default). Released automatically when the
    /// holder detaches or its connection drops.
    input_leases: HashMap<TerminalId, ClientId>,
    /// Hub-side ledger of which **hub consumer** owns the input lease over
    /// a satellite terminal (phux-v45.7). All hub consumers share the
    /// link's single client identity on the satellite, so the satellite's
    /// own lease map cannot tell them apart: without this ledger, consumer
    /// A's `ACQUIRE_INPUT` over a satellite terminal would not exclude
    /// consumer B's relayed input, and B's `RELEASE_INPUT` would release
    /// A's lease. The hub therefore gates relayed `ACQUIRE_INPUT` /
    /// `RELEASE_INPUT` / `ROUTE_INPUT` / `INPUT_*` on this map *before*
    /// forwarding, and the satellite-side lease (held by the link
    /// identity) keeps excluding the satellite's own local clients.
    /// Entries are keyed `(host, satellite-local id)` and cleared when the
    /// holder detaches (with a detached `RELEASE_INPUT` relayed so the
    /// satellite-side lease follows). Each entry carries the holder's
    /// outbound mailbox so a SEIZE takeover by another hub consumer can
    /// notify the evicted prior holder directly (phux-v45.13) — the
    /// satellite cannot, since it sees only the shared link identity. See
    /// L1 §9.1.
    satellite_leases:
        std::collections::BTreeMap<(phux_protocol::ids::SatelliteHost, u32), SatelliteLease>,
    /// Per-`(client, terminal)` cancellation for `ATTACH_TERMINAL` output
    /// pumps (phux-v45.7). `DETACH_TERMINAL` cancels one entry; client
    /// detach / disconnect cancels all of the client's entries; pane reap
    /// cancels the pane's entries. Without the token the pump task (which
    /// holds the client's outbound sender) would keep streaming until the
    /// connection died.
    attach_terminal_pumps: HashMap<(ClientId, TerminalId), tokio_util::sync::CancellationToken>,
    /// Bridge between core slotmap [`SessionId`]s and wire-level
    /// `phux_protocol::ids::SessionId` (u32). Lives in this crate (and only
    /// this crate) because `phux-core` and `phux-protocol` must not depend
    /// on each other — see [`crate::id_bridge`] module docs.
    pub session_id_bridge: IdBridge,
    /// Per-pane actor handles, keyed by core [`TerminalId`]. The
    /// `TerminalHandle` is `Send`; the underlying `TerminalActor` (which owns
    /// the `!Send` `Terminal`) lives on the `LocalSet` — see ADR-0014.
    ///
    /// Populated by [`Self::register_terminal_handle`] after the actor is
    /// spawned. Looked up by the ATTACH handler to request snapshots
    /// and by future PTY-input branches to forward keystrokes.
    pub terminals: HashMap<TerminalId, TerminalHandle>,
    /// Per-pane cancellation tokens. Cancelling a token fires the
    /// matching `TerminalActor`'s shutdown branch (see
    /// `TerminalActor::run`'s `select!`). Typically a child of the
    /// per-server root token, so a root cancel cascades to every
    /// pane in one step.
    ///
    /// Distinct from the prior `oneshot::Sender<()>` shutdown channel:
    /// dropping the token does NOT cancel — cancellation must be
    /// explicit (see [`Self::detach_terminal_actor`]).
    terminal_tokens: HashMap<TerminalId, CancellationToken>,
    /// `JoinSet` collecting the `TerminalActor::run` futures spawned via
    /// [`Self::spawn_terminal_actor`]. Owned at this scope so cancellation
    /// of the per-server root token (or drop of `ServerState`) aborts
    /// every still-running pane actor in one go.
    ///
    /// **Drop-safety note:** `JoinSet<()>` is `Send`, but the futures it
    /// holds are `!Send` (pane actors own a `!Send` `Terminal` per
    /// ADR-0014). They were spawned via `JoinSet::spawn_local`, which
    /// is only legal inside a `LocalSet`. `ServerState` is dropped at
    /// the tail of `runtime::ServerRuntime::run_async` on the same
    /// thread that ran the `LocalSet`, so this `JoinSet`'s `Drop` is
    /// always on the spawning thread — no cross-thread poll of
    /// `!Send` futures occurs.
    terminal_tasks: JoinSet<()>,
    /// Wire-side identifier for each core pane id. Allocated
    /// monotonically from `1` in [`Self::register_terminal_handle`]. Mirrors
    /// the `IdBridge` shape used for session ids — kept inline because
    /// adding a second `IdBridge` generic over an arbitrary id pair is
    /// out of scope for `phux-byc.8` (the session bridge has its own
    /// reverse-lookup story; pane reverse lookup is needed too for
    /// future `INPUT_KEY` routing).
    terminal_wire_forward: HashMap<TerminalId, WireTerminalId>,
    terminal_wire_reverse: HashMap<WireTerminalId, TerminalId>,
    next_terminal_wire_id: u32,
    /// Wire-side identifier for each core window id. Same shape as
    /// the pane bridge above; used to populate
    /// [`phux_protocol::wire::info::WindowInfo::id`] in
    /// the `ATTACHED` snapshot.
    window_wire_forward: HashMap<WindowId, WireWindowId>,
    window_wire_reverse: HashMap<WireWindowId, WindowId>,
    next_window_wire_id: u32,
    next_client_id: u64,
    /// Per-session last-touch order used to resolve
    /// [`phux_protocol::wire::frame::AttachTarget::Last`].
    ///
    /// Updated by the runtime after successful attach and after valid
    /// input/focus dispatch. The value is a server-local monotonic
    /// timestamp: ordering is the only observable contract.
    session_last_touched: HashMap<SessionId, u64>,
    next_touch_timestamp: u64,
    /// Per-scope K/V store backing SPEC §7.4 / §11.L3 metadata.
    ///
    /// Three independently-keyed maps mirror the three `Scope` variants
    /// on the wire. Values are opaque `Vec<u8>`; the server enforces
    /// nothing beyond per-key size limits (currently un-enforced; the
    /// SPEC §11.L3 recommended 256 KiB cap is a follow-up).
    metadata: MetadataStore,
    /// Per-client cache of the negotiated [`LayerSet`] from HELLO (SPEC
    /// §6.2). The dispatcher consults this before emitting any L3
    /// frame; non-L3 consumers MUST NOT see `METADATA_CHANGED` (SPEC
    /// §16.4). Default for a client that never sent HELLO (test
    /// scaffolding) is [`LayerSet::all`] — the most-permissive default
    /// keeps test setups simple; production clients always advertise.
    client_layers: HashMap<ClientId, LayerSet>,
    /// Per-client agent-event subscriptions (SPEC §7.5, phux-y2t). Each
    /// subscribed client maps to its outbound mailbox plus the set of
    /// scopes it watches: `EventScope::Server` (every event) or one or
    /// more `EventScope::Terminal(id)` (per-pane). The push half of the
    /// agent surface; an additive accelerator of the CLI poll-floor
    /// `wait`. Cleared on detach, matching the L3 metadata subscription
    /// lifecycle.
    ///
    /// The mailbox is stored here (rather than resolved through
    /// [`Self::attached`]) because a `watch` client subscribes WITHOUT
    /// attaching — it connects, sends `SUBSCRIBE_EVENTS`, and streams. So
    /// event fanout must not depend on an `attached` entry that a pure
    /// watcher never creates.
    event_subscriptions: HashMap<ClientId, EventSubscription>,
    agent_asked: AskedDetector,
    /// Who owns each Terminal's `phux.agent/v1` record: a human's explicit
    /// `SET_METADATA`, or the server-side detector (ADR-0046 §E). An explicit
    /// declaration of `state` outranks the detector, which stands down until
    /// the record is deleted.
    agent_records: AgentRecordArbiter,
    /// Whether `AttachTarget::CreateIfMissing` (phux-k61.3) should spawn
    /// a real PTY-backed actor for the newly created session's seed
    /// pane. Mirrors [`crate::runtime::ServerConfig::seed_with_pty`] so
    /// the attach-time creation path matches the server's startup
    /// configuration. Set by the runtime via
    /// [`Self::set_attach_create_pty`] right after `SharedState::new`.
    ///
    /// Defaults to `false` so tests that never call the setter exercise
    /// the cheaper no-PTY actor (matches every existing integration
    /// test that uses `spawn_server`).
    attach_create_seeds_pty: bool,
    /// Optional pre-built `CommandBuilder` used when
    /// [`Self::attach_create_seeds_pty`] is `true` and `CreateIfMissing`
    /// fires. `None` falls back to
    /// [`crate::terminal_actor::default_shell_command`] over the resolved
    /// default shell ([`Self::shell`]: `defaults.shell`, `$SHELL`, or
    /// `/bin/sh`).
    attach_create_seed_command: Option<CommandBuilder>,
    /// Lines of scrollback retained per pane (`defaults.history-limit`).
    /// Mirrors [`crate::runtime::ServerConfig::history_limit`] so the
    /// attach-time creation path (`AttachTarget::CreateIfMissing`) and
    /// `SPAWN_TERMINAL` build their `TerminalActor`s with the configured
    /// cap without an extra channel to the runtime. Set by the runtime
    /// via [`Self::set_history_limit`] right after `SharedState::new`.
    ///
    /// Defaults to the `phux_config` schema default so tests that never
    /// call the setter still get a sane bound.
    history_limit: u32,
    /// How a freshly-spawned pane chooses its working directory
    /// (`defaults.cwd-inheritance`). Mirrors
    /// [`crate::runtime::ServerConfig::cwd_inheritance`] so the
    /// `SPAWN_TERMINAL` handler resolves the new pane's CWD without an
    /// extra channel to the runtime. Set by the runtime via
    /// [`Self::set_cwd_inheritance`] right after `SharedState::new`.
    ///
    /// Defaults to the `phux_config` schema default
    /// ([`phux_config::CwdInheritance::InheritFocused`]) so tests that
    /// never call the setter exercise the tmux-default behavior.
    cwd_inheritance: phux_config::CwdInheritance,
    /// `TERM` advertised to the inner program of every server-spawned pane
    /// (`defaults.term`, phux-ign). Mirrors
    /// [`crate::runtime::ServerConfig::term`] so the attach-time creation
    /// path and `SPAWN_TERMINAL` apply it as the PTY's `TERM` baseline
    /// without an extra channel to the runtime. A per-spawn
    /// `SPAWN_TERMINAL.env` entry for `TERM` overrides it. Set by the
    /// runtime via [`Self::set_term`] right after `SharedState::new`.
    ///
    /// Defaults to the `phux_config` schema default (`xterm-256color`) so
    /// tests that never call the setter get the safe baseline.
    term: String,
    /// Resolved default shell for server-spawned panes (phux-i0e8.4.1):
    /// `defaults.shell` → `$SHELL` → `/bin/sh`, resolved once by the
    /// binary from its single config load. Mirrors
    /// [`crate::runtime::ServerConfig::shell`] so the attach-time
    /// creation path, `SESSION_CREATE_KEY`, and a command-less
    /// `SPAWN_TERMINAL` spawn the configured shell without an extra
    /// channel to the runtime. A wire `command` always wins over this
    /// default. Set by the runtime via [`Self::set_shell`] right after
    /// `SharedState::new`.
    ///
    /// Defaults to `resolve_shell(None)` (`$SHELL`, else `/bin/sh`) so
    /// state-only tests keep the pre-config behavior.
    shell: String,
    /// The UDS path this server listens on. Mirrors
    /// [`crate::runtime::ServerConfig::socket_path`] so every pane spawn
    /// site can inject it into the child environment as `PHUX_SOCKET`
    /// (phux-cufw) without an extra channel to the runtime. Set by the
    /// runtime via [`Self::set_server_socket_path`] right after
    /// `SharedState::new`.
    ///
    /// `None` when the runtime never mirrors it (state-only tests):
    /// spawned panes then carry no `PHUX_SOCKET` and an in-pane `phux`
    /// falls back to the default-socket resolution.
    server_socket_path: Option<std::path::PathBuf>,
    /// How a Terminal viewed by clients of differing sizes resolves its one
    /// authoritative PTY geometry (`defaults.window-size`, phux-nk07). Mirrors
    /// [`crate::runtime::ServerConfig::window_size`] so `handle_viewport_resize`
    /// can apply the policy across every subscriber's viewport instead of
    /// last-writer-wins. Set by the runtime via [`Self::set_window_size`] right
    /// after `SharedState::new`; defaults to the `phux_config` schema default
    /// ([`phux_config::WindowSize::Smallest`] — never crops).
    window_size: phux_config::WindowSize,
    /// Frozen session-creation directory per session (phux-nyx,
    /// `defaults.cwd-inheritance = session-root`). Captured the first time
    /// a `session-root` spawn resolves a session's seed-pane CWD and reused
    /// thereafter, so later `cd`s in the seed pane do not move the root.
    /// Cleared with the rest of a session's bookkeeping on teardown.
    session_root: HashMap<SessionId, PathBuf>,
    /// Most-recent observed working directory per window (phux-nyx,
    /// `defaults.cwd-inheritance = last-cwd-per-window`). Updated whenever a
    /// `last-cwd-per-window` spawn resolves the window's active-pane CWD;
    /// new panes in that window inherit the latest value. Cleared with the
    /// window's bookkeeping on teardown.
    window_last_cwd: HashMap<WindowId, PathBuf>,
    /// Whether last-session self-exit has been armed.
    ///
    /// A client attach or successful headless session create arms the
    /// tmux-model self-exit (phux-60s). A freshly auto-spawned server whose
    /// seed pane dies before either interaction stays alive instead of
    /// vanishing mid-handshake — the launching `phux` can repopulate it via
    /// `CreateIfMissing`.
    has_served_client: bool,
    /// Monotonic stamp handed out on every viewport announcement
    /// ([`Self::set_client_viewport`]). Orders announcements across
    /// clients so [`Self::resolve_terminal_cell_px`] can pick the most
    /// recent usable pixel report deterministically.
    viewport_clock: u64,
    /// Optional policy extension bundle. Defaults to permissive.
    policy_bundle: crate::policy::PolicyBundle,
    /// Per-client peer identities, keyed by server-assigned client id.
    peer_identities: HashMap<ClientId, phux_protocol::policy::PeerIdentity>,
    /// Graceful-upgrade context (ADR-0032): the listening socket's raw fd,
    /// its path, and the server's effective runtime flags (phux-v45.10),
    /// captured at startup. `handle_upgrade` reads these to build the handoff
    /// blob and to re-pass `--socket` / `--listen` / `--quic` / `--hub` to
    /// the re-exec'd image. `None` until [`Self::set_upgrade_context`] runs
    /// (i.e. before serving).
    upgrade_ctx: Option<(
        std::os::fd::RawFd,
        std::path::PathBuf,
        crate::runtime::RuntimeFlags,
    )>,
    /// Validated satellite table for a federation hub (phux-v45.1,
    /// ADR-0007). `None` on every non-hub server — the registry is never
    /// read outside hub mode. Set once at startup by the runtime via
    /// [`Self::set_hub_table`] after `crate::hub::resolve_hub_table`
    /// succeeds. Held for the upcoming dial (phux-v45.3) and route
    /// (phux-v45.4) beads; nothing consumes it for I/O yet.
    hub_table: Option<crate::hub::HubTable>,
    /// Per-satellite link statuses published by the hub's outbound link
    /// supervisors (phux-v45.3). `None` on every non-hub server. Set once
    /// at startup via [`Self::set_hub_link_statuses`] alongside the link
    /// spawn; the handle is the read surface a future `LIST` aggregation
    /// (phux-v45.5) consumes.
    hub_link_statuses: Option<crate::hub::link::HubLinkStatuses>,
    /// Per-satellite frame-relay handles (phux-v45.4, ADR-0007 §4).
    /// `None` on every non-hub server. Set once at hub startup via
    /// [`Self::set_hub_relays`] alongside the link spawn; command and
    /// input dispatch resolve `TerminalId::Satellite { host, .. }`
    /// through it to the owning link's relay mailbox.
    hub_relays: Option<crate::hub::relay::HubRelays>,
    /// Server-side event-hook dispatcher handle (`docs/consumers/tui.md`
    /// §9, phux-r82.1). `None` until the runtime spawns the dispatcher
    /// (it does so only when the hook catalog is non-empty), which is
    /// also the default for every test that never configures hooks —
    /// firing an event is then a no-op. Set once at startup via
    /// [`Self::set_hook_dispatcher`].
    hook_dispatcher: Option<crate::hooks::HookDispatcher>,
    /// Number of client connections currently open across every transport
    /// (UDS, WebSocket, QUIC, WebTransport). Incremented the instant an
    /// accept succeeds and decremented when that connection's task ends —
    /// see [`Self::note_connection_opened`] / [`Self::note_connection_closed`].
    ///
    /// This counts *connections*, not attached clients: a one-shot control
    /// verb (`phux ls`, `phux send-keys`, `phux new --json`) connects,
    /// issues a `COMMAND`, and leaves without ever appearing in
    /// [`Self::attached`]. Only the connection count sees it.
    live_connections: u32,
    /// When the server last had zero open connections, or `None` while at
    /// least one is open.
    ///
    /// Seeded to "now" at construction so a server that has *never* been
    /// connected to is idle from birth — the leak shape that motivated
    /// `--exit-after-idle` is a bootstrapped daemon whose harness died
    /// before it ever dialed in. Read by the runtime's idle-exit watchdog;
    /// nothing else consumes it, and it is inert unless that watchdog was
    /// spawned.
    idle_since: Option<std::time::Instant>,
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
        self.server_incarnation
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
    use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};

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
        assert!(s.attached.contains_key(&cid));
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
    fn second_attach_for_same_client_returns_already_attached() {
        let mut s = ServerState::new();
        let _ = s.seed_session("default");
        let cid = s.new_client_id();
        s.attach_default_caps(cid, "default", mk_tx()).unwrap();
        let err = s.attach_default_caps(cid, "default", mk_tx()).unwrap_err();
        assert_eq!(err, AttachError::AlreadyAttached(cid));
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
        assert!(!s.attached.contains_key(&cid));
        assert!(s.subscribers_for_terminal(pid).is_empty());
        assert!(
            s.terminal_subscribers.is_empty(),
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
    fn reap_last_pane_empties_server() {
        let mut s = ServerState::new();
        let (sid, _wid, pid) = s.seed_session("default");
        assert_eq!(s.registry.session_count(), 1);

        let server_empty = s.reap_terminal(pid);

        assert!(server_empty, "reaping the only pane must empty the server");
        assert_eq!(s.registry.session_count(), 0);
        assert!(s.registry.session(sid).is_none(), "session cascaded away");
        assert!(s.registry.terminal(pid).is_none());
    }

    #[test]
    fn reap_one_of_two_sessions_keeps_server_alive() {
        let mut s = ServerState::new();
        let (sid_a, _wa, pid_a) = s.seed_session("a");
        let (sid_b, _wb, _pb) = s.seed_session("b");

        let server_empty = s.reap_terminal(pid_a);

        assert!(!server_empty, "a second session is still live");
        assert_eq!(s.registry.session_count(), 1);
        assert!(s.registry.session(sid_a).is_none(), "session a reaped");
        assert!(s.registry.session(sid_b).is_some(), "session b untouched");
    }

    #[test]
    fn reap_pane_in_multipane_window_keeps_session() {
        let mut s = ServerState::new();
        let (sid, wid, pid1) = s.seed_session("default");
        // Add a second pane to the same window so reaping one does not
        // empty the window.
        let pid2 = s.registry.new_terminal(wid).unwrap();

        let server_empty = s.reap_terminal(pid1);

        assert!(!server_empty);
        assert_eq!(s.registry.session_count(), 1);
        assert!(s.registry.session(sid).is_some());
        assert!(s.registry.terminal(pid1).is_none(), "reaped pane gone");
        assert!(s.registry.terminal(pid2).is_some(), "sibling pane survives");
        assert_eq!(
            s.registry.window(wid).map(|w| w.panes.len()),
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
        assert_eq!(s.registry.session_count(), 0);
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
        let client = s.attached.get(&cid).unwrap();
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
        )
        .unwrap();
        let client = s.attached.get(&cid).unwrap();
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
        let client = s.attached.get(&cid).unwrap();
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
            .registry
            .new_terminal(wid)
            .expect("same window second pane");
        let wid_2 = s.registry.new_window(sid).expect("second window");
        let pid_c = s
            .registry
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
            let Outbound::Frame(f) = msg;
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
