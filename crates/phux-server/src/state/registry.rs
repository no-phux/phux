use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;

use phux_core::ids::{SessionId, TerminalId, WindowId};
use phux_core::registry::Registry;
use phux_core::session::Session;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ClientCapabilities, Layer, LayerSet};
use phux_protocol::ids::{TerminalId as WireTerminalId, WindowId as WireWindowId};
use phux_protocol::wire::frame::{FrameKind, Scope};
use portable_pty::CommandBuilder;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::{
    AttachError, AttachSnapshotPane, AttachedClient, ClientId, EventScope, EventSubscription,
    MetadataStore, Outbound, RenameOutcome, SatelliteLease, ServerState,
};
use crate::agent_asked::{AskedPayload, AskedSource, AskedTransition};
use crate::id_bridge::IdBridge;
use crate::terminal_actor::TerminalHandle;

/// Derive the per-cell pixel size implied by one client's viewport report:
/// `pixel / cells`, floored. `None` when the report carries no pixel metrics
/// or they are degenerate — zero cells, or a pixel field smaller than the
/// cell count (a sub-pixel cell is a bogus report, not a tiny font).
fn viewport_cell_px(v: &phux_protocol::wire::frame::ViewportInfo) -> Option<(u16, u16)> {
    if v.cols == 0 || v.rows == 0 {
        return None;
    }
    let w = v.pixel_w? / v.cols;
    let h = v.pixel_h? / v.rows;
    (w > 0 && h > 0).then_some((w, h))
}

impl ServerState {
    /// Build an empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            server_incarnation: super::ServerIncarnation::random(),
            registry: Registry::new(),
            attached: HashMap::new(),
            viewport_clock: 0,
            terminal_subscribers: HashMap::new(),
            input_leases: HashMap::new(),
            satellite_leases: std::collections::BTreeMap::new(),
            attach_terminal_pumps: HashMap::new(),
            session_id_bridge: IdBridge::new(),
            terminals: HashMap::new(),
            terminal_tokens: HashMap::new(),
            terminal_tasks: JoinSet::new(),
            terminal_wire_forward: HashMap::new(),
            terminal_wire_reverse: HashMap::new(),
            next_terminal_wire_id: 1,
            window_wire_forward: HashMap::new(),
            window_wire_reverse: HashMap::new(),
            next_window_wire_id: 1,
            next_client_id: 1,
            session_last_touched: HashMap::new(),
            next_touch_timestamp: 1,
            metadata: MetadataStore::default(),
            session_create_results: HashMap::new(),
            client_layers: HashMap::new(),
            event_subscriptions: HashMap::new(),
            agent_asked: crate::agent_asked::AskedDetector::default(),
            agent_records: crate::agent_state::AgentRecordArbiter::default(),
            attach_create_seeds_pty: false,
            attach_create_seed_command: None,
            history_limit: phux_config::DefaultsCfg::default().history_limit,
            cwd_inheritance: phux_config::CwdInheritance::default(),
            term: phux_config::DefaultsCfg::default().term,
            shell: crate::terminal_actor::resolve_shell(None),
            server_socket_path: None,
            window_size: phux_config::WindowSize::default(),
            session_root: HashMap::new(),
            window_last_cwd: HashMap::new(),
            has_served_client: false,
            policy_bundle: crate::policy::PolicyBundle::default(),
            peer_identities: HashMap::new(),
            upgrade_ctx: None,
            hub_table: None,
            hub_link_statuses: None,
            hub_relays: None,
            hook_dispatcher: None,
            live_connections: 0,
            // "Idle since startup": a server nobody ever dialed is the
            // leak shape `--exit-after-idle` exists for, so the clock is
            // already running before the first accept.
            idle_since: Some(std::time::Instant::now()),
        }
    }

    /// Record that a client connection was accepted (any transport).
    ///
    /// Disarms the idle clock: while a connection is open the server is by
    /// definition not idle, however long that connection sits quiet. "Idle"
    /// here means *unattended*, not *silent* — a human parked in an attached
    /// pane reading a log for an hour must never be reaped.
    pub fn note_connection_opened(&mut self) {
        self.live_connections = self.live_connections.saturating_add(1);
        self.idle_since = None;
    }

    /// Record that a client connection ended (clean EOF, error, or drop).
    ///
    /// Re-arms the idle clock when this was the last one. `saturating_sub`
    /// rather than `-= 1`: a decrement without a matching increment is a
    /// bookkeeping bug, and underflowing to `u32::MAX` would silently make
    /// the server immortal — the exact failure this whole feature exists to
    /// prevent. Saturating instead pins the count at zero, which fails
    /// *toward* exiting and is therefore the safe direction.
    pub fn note_connection_closed(&mut self) {
        self.live_connections = self.live_connections.saturating_sub(1);
        if self.live_connections == 0 {
            self.idle_since = Some(std::time::Instant::now());
        }
    }

    /// Instant the server became unattended, or `None` while a client
    /// connection is open. See [`Self::note_connection_opened`].
    #[must_use]
    pub const fn idle_since(&self) -> Option<std::time::Instant> {
        self.idle_since
    }

    /// Install the validated hub satellite table (phux-v45.1). Called once
    /// at server startup, only in hub mode, after
    /// [`crate::hub::resolve_hub_table`] succeeds.
    pub fn set_hub_table(&mut self, table: crate::hub::HubTable) {
        self.hub_table = Some(table);
    }

    /// Read the hub satellite table set by [`Self::set_hub_table`].
    /// `None` on a non-hub server.
    #[must_use]
    pub const fn hub_table(&self) -> Option<&crate::hub::HubTable> {
        self.hub_table.as_ref()
    }

    /// Install the shared per-satellite link-status handle (phux-v45.3).
    /// Called once at hub startup, alongside spawning the link
    /// supervisors that publish into it.
    pub fn set_hub_link_statuses(&mut self, statuses: crate::hub::link::HubLinkStatuses) {
        self.hub_link_statuses = Some(statuses);
    }

    /// Read the per-satellite link statuses set by
    /// [`Self::set_hub_link_statuses`]. `None` on a non-hub server.
    #[must_use]
    pub const fn hub_link_statuses(&self) -> Option<&crate::hub::link::HubLinkStatuses> {
        self.hub_link_statuses.as_ref()
    }

    /// Install the shared per-satellite frame-relay registry
    /// (phux-v45.4). Called once at hub startup, alongside spawning the
    /// link supervisors that drain its mailboxes.
    pub(crate) fn set_hub_relays(&mut self, relays: crate::hub::relay::HubRelays) {
        self.hub_relays = Some(relays);
    }

    /// The relay handle for satellite `host`, or `None` when this server
    /// is not a hub or `host` is not in its table — the caller's
    /// `UnsupportedSatelliteRoute` signal.
    #[must_use]
    pub(crate) fn hub_relay(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
    ) -> Option<crate::hub::relay::RelayHandle> {
        self.hub_relays.as_ref().and_then(|relays| relays.get(host))
    }

    /// Every satellite relay handle (detach fan-out); empty off-hub.
    #[must_use]
    pub(crate) fn hub_relays_all(&self) -> Vec<crate::hub::relay::RelayHandle> {
        self.hub_relays
            .as_ref()
            .map(crate::hub::relay::HubRelays::all)
            .unwrap_or_default()
    }

    /// Install the event-hook dispatcher handle (phux-r82.1). Called once
    /// at server startup, after [`crate::hooks::spawn_hook_dispatcher`].
    pub fn set_hook_dispatcher(&mut self, dispatcher: crate::hooks::HookDispatcher) {
        self.hook_dispatcher = Some(dispatcher);
    }

    /// The installed event-hook dispatcher handle, if any. `None` means no
    /// hooks are configured and firing events is a no-op.
    #[must_use]
    pub const fn hook_dispatcher(&self) -> Option<&crate::hooks::HookDispatcher> {
        self.hook_dispatcher.as_ref()
    }

    /// Set the policy extension bundle. Called once at server startup.
    pub fn set_policy_bundle(&mut self, bundle: crate::policy::PolicyBundle) {
        self.policy_bundle = bundle;
    }

    /// Read the policy extension bundle.
    #[must_use]
    pub fn policy_bundle(&self) -> &crate::policy::PolicyBundle {
        &self.policy_bundle
    }

    /// Store a peer identity for a client.
    pub fn set_peer_identity(
        &mut self,
        client_id: ClientId,
        identity: phux_protocol::policy::PeerIdentity,
    ) {
        self.peer_identities.insert(client_id, identity);
    }

    /// Look up a peer identity by client id.
    #[must_use]
    pub fn peer_identity(
        &self,
        client_id: ClientId,
    ) -> Option<&phux_protocol::policy::PeerIdentity> {
        self.peer_identities.get(&client_id)
    }

    /// Remove a peer identity when a client disconnects.
    pub fn remove_peer_identity(&mut self, client_id: ClientId) {
        self.peer_identities.remove(&client_id);
    }

    /// Configure the PTY mode and seed command used by
    /// `crate::runtime::handle_attach`'s
    /// `AttachTarget::CreateIfMissing` branch (phux-k61.3).
    ///
    /// Called once at server startup to mirror
    /// [`crate::runtime::ServerConfig::seed_with_pty`] /
    /// [`crate::runtime::ServerConfig::seed_command`] into state, so the
    /// attach-time creation path can read them without an extra channel
    /// to the runtime.
    ///
    /// When `with_pty` is `false`, `cmd` is ignored — the create path
    /// spawns a no-PTY actor instead. Setting `cmd = None` with
    /// `with_pty = true` falls back to
    /// [`crate::terminal_actor::default_shell_command`] at create time.
    pub fn set_attach_create_pty(&mut self, with_pty: bool, cmd: Option<CommandBuilder>) {
        self.attach_create_seeds_pty = with_pty;
        self.attach_create_seed_command = cmd;
    }

    /// Read the PTY-mode flag set by [`Self::set_attach_create_pty`].
    #[must_use]
    pub const fn attach_create_seeds_pty(&self) -> bool {
        self.attach_create_seeds_pty
    }

    /// Clone the optional pre-built seed command. Used by the create
    /// path inside `handle_attach`: each `AttachTarget::CreateIfMissing`
    /// that fires gets a fresh clone, so the slot stays populated for
    /// future creates. `CommandBuilder` is `Clone` (per portable-pty
    /// 0.8), so this is cheap.
    #[must_use]
    pub fn attach_create_seed_command(&self) -> Option<CommandBuilder> {
        self.attach_create_seed_command.clone()
    }

    /// Set the per-pane scrollback cap (`defaults.history-limit`) used
    /// by the attach-time creation path and `SPAWN_TERMINAL`. Called
    /// once at server startup to mirror
    /// [`crate::runtime::ServerConfig::history_limit`] into state.
    pub const fn set_history_limit(&mut self, history_limit: u32) {
        self.history_limit = history_limit;
    }

    /// Read the per-pane scrollback cap set by [`Self::set_history_limit`].
    #[must_use]
    pub const fn history_limit(&self) -> u32 {
        self.history_limit
    }

    /// Set the working-directory inheritance policy
    /// (`defaults.cwd-inheritance`) used by `SPAWN_TERMINAL`. Called once
    /// at server startup to mirror
    /// [`crate::runtime::ServerConfig::cwd_inheritance`] into state.
    pub const fn set_cwd_inheritance(&mut self, mode: phux_config::CwdInheritance) {
        self.cwd_inheritance = mode;
    }

    /// Read the working-directory inheritance policy set by
    /// [`Self::set_cwd_inheritance`].
    #[must_use]
    pub const fn cwd_inheritance(&self) -> phux_config::CwdInheritance {
        self.cwd_inheritance
    }

    /// Set the default `TERM` (`defaults.term`) advertised to
    /// server-spawned panes. Called once at server startup to mirror
    /// [`crate::runtime::ServerConfig::term`] into state.
    pub fn set_term(&mut self, term: String) {
        self.term = term;
    }

    /// Read the default `TERM` set by [`Self::set_term`]. A per-spawn
    /// `SPAWN_TERMINAL.env` entry for `TERM` overrides this baseline.
    #[must_use]
    pub fn term(&self) -> &str {
        &self.term
    }

    /// Set the resolved default shell (`defaults.shell` → `$SHELL` →
    /// `/bin/sh`, phux-i0e8.4.1) server-spawned panes run when no wire
    /// `command` names a program. Called once at server startup to
    /// mirror [`crate::runtime::ServerConfig::shell`] into state.
    pub fn set_shell(&mut self, shell: String) {
        self.shell = shell;
    }

    /// Read the resolved default shell set by [`Self::set_shell`].
    #[must_use]
    pub fn shell(&self) -> &str {
        &self.shell
    }

    /// Set the UDS path this server listens on. Called once at server
    /// startup to mirror [`crate::runtime::ServerConfig::socket_path`]
    /// into state so every pane spawn site can inject it as
    /// `PHUX_SOCKET` (phux-cufw).
    pub fn set_server_socket_path(&mut self, path: std::path::PathBuf) {
        self.server_socket_path = Some(path);
    }

    /// Read the socket path set by [`Self::set_server_socket_path`].
    /// `None` until the runtime mirrors it (e.g. in state-only tests).
    #[must_use]
    pub fn server_socket_path(&self) -> Option<&std::path::Path> {
        self.server_socket_path.as_deref()
    }

    /// Set the multi-client window-size policy (`defaults.window-size`,
    /// phux-nk07). Called once at server startup to mirror
    /// [`crate::runtime::ServerConfig::window_size`] into state.
    pub const fn set_window_size(&mut self, window_size: phux_config::WindowSize) {
        self.window_size = window_size;
    }

    /// Read the window-size policy set by [`Self::set_window_size`].
    #[must_use]
    pub const fn window_size(&self) -> phux_config::WindowSize {
        self.window_size
    }

    /// Record `client`'s current outer viewport (`phux-nk07`), as carried by
    /// `ATTACH` or a live `VIEWPORT_RESIZE`. No-op for an unattached client.
    pub fn set_client_viewport(
        &mut self,
        client: ClientId,
        viewport: phux_protocol::wire::frame::ViewportInfo,
    ) {
        if let Some(c) = self.attached.get_mut(&client) {
            self.viewport_clock += 1;
            c.viewport = Some(viewport);
            c.viewport_seq = self.viewport_clock;
        }
    }

    /// Resolve the one authoritative `(cols, rows)` a Terminal's PTY should
    /// take, given the viewports of every client subscribed to it and the
    /// active `window-size` policy (`phux-nk07`).
    ///
    /// Returns `None` when the policy is `Manual` (geometry is fixed
    /// externally, never derived from views) or when no subscriber has
    /// announced a usable (non-zero) viewport yet — in both cases the caller
    /// leaves the PTY size unchanged. `latest` is the viewport of the client
    /// that just resized, used only by the `Latest` policy.
    ///
    /// Degenerate `0`-dimension viewports are ignored in the min/max so a
    /// transient resize-to-zero (a detaching client, a probe) can't collapse
    /// the shared grid.
    #[must_use]
    pub fn resolve_terminal_geometry(
        &self,
        terminal: TerminalId,
        latest: Option<phux_protocol::wire::frame::ViewportInfo>,
    ) -> Option<(u16, u16)> {
        use phux_config::WindowSize;
        match self.window_size {
            WindowSize::Manual => None,
            WindowSize::Latest => latest
                .filter(|v| v.cols > 0 && v.rows > 0)
                .map(|v| (v.cols, v.rows)),
            WindowSize::Smallest | WindowSize::Largest => {
                let viewports = self
                    .subscribers_for_terminal(terminal)
                    .iter()
                    .filter_map(|cid| self.attached.get(cid).and_then(|c| c.viewport))
                    .filter(|v| v.cols > 0 && v.rows > 0);
                let mut acc: Option<(u16, u16)> = None;
                for v in viewports {
                    acc = Some(match (acc, self.window_size) {
                        (None, _) => (v.cols, v.rows),
                        (Some((c, r)), WindowSize::Smallest) => (c.min(v.cols), r.min(v.rows)),
                        (Some((c, r)), _) => (c.max(v.cols), r.max(v.rows)),
                    });
                }
                acc
            }
        }
    }

    /// Resolve the per-cell pixel size a Terminal should report — via the
    /// PTY `winsize` pixel fields and XTWINOPS size replies — from the most
    /// recent usable pixel report among the Terminal's subscribers.
    ///
    /// The resolved unit is *cell* size, not total pixels: the authoritative
    /// grid from [`Self::resolve_terminal_geometry`] may match no single
    /// client's viewport, so the Terminal's pixel size is `cells x cell size`
    /// computed at the point of use. That keeps the kernel-reported geometry
    /// self-consistent (`ws_xpixel / ws_col` is exactly the cell width —
    /// the division `kitten icat`-style preflights perform).
    ///
    /// Recency — not the `window-size` policy — picks the donor viewport:
    /// cell pixel size is a property of one physical display, and min/max
    /// over mixed-DPI viewports would synthesize a cell belonging to no real
    /// screen. `None` until some subscriber announces a viewport with usable
    /// pixel metrics; callers then leave the Terminal's pixel state alone.
    #[must_use]
    pub fn resolve_terminal_cell_px(&self, terminal: TerminalId) -> Option<(u16, u16)> {
        self.subscribers_for_terminal(terminal)
            .iter()
            .filter_map(|cid| self.attached.get(cid))
            .filter_map(|c| Some((c.viewport_seq, viewport_cell_px(c.viewport.as_ref()?)?)))
            .max_by_key(|&(seq, _)| seq)
            .map(|(_, cell)| cell)
    }

    /// Read the frozen session-creation directory recorded for `session`
    /// under the `session-root` cwd-inheritance policy (phux-nyx), if one
    /// has been captured.
    #[must_use]
    pub fn session_root(&self, session: SessionId) -> Option<&PathBuf> {
        self.session_root.get(&session)
    }

    /// Freeze `root` as `session`'s creation directory the first time it is
    /// observed; later calls are no-ops so a `cd` in the seed pane cannot
    /// move an already-recorded root (phux-nyx, `session-root`). Returns the
    /// effective recorded root.
    pub fn record_session_root(&mut self, session: SessionId, root: PathBuf) -> &PathBuf {
        self.session_root.entry(session).or_insert(root)
    }

    /// Read the most-recent working directory recorded for `window` under
    /// the `last-cwd-per-window` cwd-inheritance policy (phux-nyx), if any.
    #[must_use]
    pub fn window_last_cwd(&self, window: WindowId) -> Option<&PathBuf> {
        self.window_last_cwd.get(&window)
    }

    /// Record `cwd` as `window`'s most-recent working directory, overwriting
    /// any prior value (phux-nyx, `last-cwd-per-window`).
    pub fn record_window_last_cwd(&mut self, window: WindowId, cwd: PathBuf) {
        self.window_last_cwd.insert(window, cwd);
    }

    /// Resolve the window that owns `session`'s active pane, if any. The
    /// `last-cwd-per-window` policy keys its ledger on this window.
    #[must_use]
    pub fn active_window_of_session(&self, session: SessionId) -> Option<WindowId> {
        self.registry.session(session)?.active
    }

    /// Resolve the seed (oldest) pane of `session` — the first pane of its
    /// first window. The `session-root` policy reads this pane's CWD to
    /// establish the session's creation directory.
    #[must_use]
    pub fn seed_pane_of_session(&self, session: SessionId) -> Option<TerminalId> {
        let session = self.registry.session(session)?;
        let window_id = *session.windows.first()?;
        let window = self.registry.window(window_id)?;
        window.panes.first().copied()
    }

    /// Borrow the L3 metadata store.
    #[must_use]
    pub const fn metadata(&self) -> &MetadataStore {
        &self.metadata
    }

    /// Record the layer set advertised by `client_id` in HELLO. Called
    /// from the runtime's HELLO handler. Re-set is idempotent (the
    /// most recent HELLO wins, matching `ColorSupport`).
    pub fn set_client_layers(&mut self, client_id: ClientId, layers: LayerSet) {
        self.client_layers.insert(client_id, layers);
    }

    /// Look up the layer set advertised by `client_id`. Defaults to
    /// [`LayerSet::all`] for clients we never saw a HELLO from — the
    /// permissive default matches test scaffolding that skips HELLO.
    #[must_use]
    pub fn client_layers(&self, client_id: ClientId) -> LayerSet {
        self.client_layers
            .get(&client_id)
            .copied()
            .unwrap_or_else(LayerSet::all)
    }

    /// `true` iff `client_id` has L3 in its negotiated `HELLO.layers`.
    /// Gates emission of `METADATA_CHANGED` per SPEC §16.4.
    #[must_use]
    pub fn client_speaks_l3(&self, client_id: ClientId) -> bool {
        self.client_layers(client_id).contains(Layer::L3)
    }

    /// Atomic SET + broadcast: store `value` at `(scope, key)`, then
    /// enqueue a `MetadataChanged` to every L3-capable subscriber
    /// whose subscription matches `(scope, key)`. Silently skips
    /// subscribers that have been detached or whose mailbox is full
    /// (`try_send` semantics — backpressure is a flow-control concern
    /// SPEC §12 doesn't yet cover for L3).
    ///
    /// Returns the set of clients the broadcast was attempted against
    /// (after L3-capability filtering) so callers can assert fanout
    /// shape in tests.
    pub fn metadata_set(&mut self, scope: &Scope, key: &str, value: Vec<u8>) -> Vec<ClientId> {
        // Broadcast first so the borrow of `value` is finished by the time
        // the K/V store consumes it on `set`. The "set before broadcast"
        // ordering is preserved by checking the prior value: if the new
        // bytes equal what's already stored we return early *before*
        // mutating, so subscribers never observe a fake notification.
        let unchanged = self
            .metadata
            .get(scope, key)
            .is_some_and(|prev| prev == value);
        if unchanged {
            return Vec::new();
        }
        let subscribers = self.metadata.subscribers_for(scope, key);
        let delivered = self.broadcast_metadata_changed(&subscribers, scope, key, Some(&value));
        // Commit the write last; `MetadataSetOutcome` is now redundant
        // here but kept on the lower-level API for direct callers.
        let _ = self.metadata.set(scope, key, value);
        delivered
    }

    /// Atomic DELETE + tombstone broadcast. Idempotent: deleting a
    /// missing key returns an empty broadcast set.
    pub fn metadata_delete(&mut self, scope: &Scope, key: &str) -> Vec<ClientId> {
        let existed = self.metadata.delete(scope, key);
        if !existed {
            return Vec::new();
        }
        let subscribers = self.metadata.subscribers_for(scope, key);
        self.broadcast_metadata_changed(&subscribers, scope, key, None)
    }

    /// Publish ownership of a one-shot session-create result.
    ///
    /// The metadata value must already have been written. At most 256 unread
    /// results are retained per connection; the oldest is evicted first.
    pub fn track_session_create_result(&mut self, client_id: ClientId, key: String) {
        const MAX_PENDING_PER_CLIENT: usize = 256;
        let evicted = {
            let keys = self.session_create_results.entry(client_id).or_default();
            let evicted = (keys.len() >= MAX_PENDING_PER_CLIENT)
                .then(|| keys.pop_front())
                .flatten();
            keys.push_back(key);
            evicted
        };
        if let Some(key) = evicted {
            let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, &key);
        }
    }

    /// Whether any live connection owns an unread result at `key`.
    #[must_use]
    pub fn session_create_result_is_pending(&self, key: &str) -> bool {
        self.session_create_results
            .values()
            .any(|keys| keys.iter().any(|candidate| candidate == key))
    }

    /// Whether `client_id` owns the unread nonce-bearing result at `key`.
    #[must_use]
    pub fn owns_session_create_result(&self, client_id: ClientId, key: &str) -> bool {
        self.session_create_results
            .get(&client_id)
            .is_some_and(|keys| keys.iter().any(|candidate| candidate == key))
    }

    /// Consume a one-shot session-create result and forget its owner.
    pub fn consume_session_create_result(&mut self, key: &str) {
        let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, key);
        for keys in self.session_create_results.values_mut() {
            keys.retain(|candidate| candidate != key);
        }
        self.session_create_results
            .retain(|_, keys| !keys.is_empty());
    }

    /// Register a subscription for `client_id`. The client MUST be
    /// L3-capable (call sites in the runtime gate on
    /// [`Self::client_speaks_l3`] before invoking this).
    pub fn metadata_subscribe(&mut self, client_id: ClientId, scope: Scope, key: String) {
        self.metadata.subscribe(client_id, scope, key);
    }

    /// Helper: fan a `MetadataChanged` frame out to every subscriber in
    /// `subscribers` that is (a) still attached, (b) L3-capable, and
    /// (c) drainable (mailbox not closed). Returns the actually-targeted
    /// client list.
    fn broadcast_metadata_changed(
        &self,
        subscribers: &[ClientId],
        scope: &Scope,
        key: &str,
        value: Option<&[u8]>,
    ) -> Vec<ClientId> {
        let mut delivered = Vec::with_capacity(subscribers.len());
        for client_id in subscribers {
            if !self.client_speaks_l3(*client_id) {
                continue;
            }
            let Some(client) = self.attached.get(client_id) else {
                continue;
            };
            let frame = FrameKind::MetadataChanged {
                scope: scope.clone(),
                key: key.to_owned(),
                value: value.map(<[u8]>::to_vec),
            };
            // `try_send`: the mailbox is bounded (DEFAULT_CLIENT_MAILBOX)
            // and we hold the state mutex synchronously; awaiting on a
            // full mailbox would deadlock the per-client read loop. A
            // dropped notification is acceptable per SPEC §7.4 — the
            // subscriber can re-`GET_METADATA` on next attach.
            if client.tx.try_send(Outbound::Frame(frame)).is_ok() {
                delivered.push(*client_id);
            }
        }
        delivered
    }

    /// Most-recently-touched live session, if any. Resolves
    /// `AttachTarget::Last`.
    #[must_use]
    pub fn most_recently_touched_session(&self) -> Option<SessionId> {
        self.session_last_touched
            .iter()
            .filter(|(sid, _)| self.registry.session(**sid).is_some())
            .max_by_key(|(_, touched_at)| *touched_at)
            .map(|(sid, _)| *sid)
    }

    /// Mark `session` as touched by attach/input/focus activity.
    pub fn touch_session(&mut self, session: SessionId) {
        let touched_at = self.next_touch_timestamp;
        self.next_touch_timestamp = self.next_touch_timestamp.saturating_add(1);
        self.session_last_touched.insert(session, touched_at);
    }

    /// Allocate the next monotonic [`ClientId`].
    ///
    /// Ids are never reused. `0` is intentionally skipped so log entries
    /// printing `client=0` are obviously a placeholder, not a real client.
    pub const fn new_client_id(&mut self) -> ClientId {
        let id = ClientId(self.next_client_id);
        self.next_client_id = self.next_client_id.saturating_add(1);
        id
    }

    /// Attach a client to the session with `session_name`.
    ///
    /// On success the client is recorded in `attached` and subscribed to the
    /// session's currently active pane (if any). Returns a borrow of the
    /// [`Session`] for callers that want to build an `ATTACHED` snapshot.
    ///
    /// `client_caps`, `bootstrap_profile`, and `bootstrap_limits` are the
    /// immutable values selected while processing this connection's HELLO.
    /// Callers without a negotiated connection should use
    /// [`Self::attach_default_caps`] only for test scaffolding.
    pub fn attach(
        &mut self,
        client_id: ClientId,
        session_name: &str,
        tx: mpsc::Sender<Outbound>,
        client_caps: ClientCapabilities,
        bootstrap_profile: BootstrapProfile,
        bootstrap_limits: BootstrapLimits,
    ) -> Result<SessionId, AttachError> {
        let session_id = self
            .find_session_by_name(session_name)
            .ok_or_else(|| AttachError::UnknownSession(session_name.to_owned()))?;
        if let Some(existing) = self.attached.get(&client_id) {
            if existing.session == session_id {
                return Ok(session_id);
            }
            return Err(AttachError::AlreadyAttached(client_id));
        }

        self.attached.insert(
            client_id,
            AttachedClient {
                id: client_id,
                session: session_id,
                tx,
                client_caps,
                bootstrap_profile,
                bootstrap_limits,
                viewport: None,
                viewport_seq: 0,
            },
        );
        // Attaching arms tmux-model last-session self-exit (phux-60s).
        self.arm_self_exit();

        // Subscribe to EVERY pane in the session, across all its windows —
        // not just the active one (phux-fysb.2). A multi-pane client renders
        // all panes (it receives a TERMINAL_SNAPSHOT for each via
        // `attach_snapshot_panes`) and must be able to route input to whichever
        // it focuses. The input gate in `handle_terminal_input` DROPS keystrokes
        // to panes the client isn't subscribed to, so the old active-pane-only
        // subscription left every other pane unable to receive input on
        // (re-)attach — the user could see the prompts but not type into them,
        // while a freshly spawned pane worked because `handle_spawn_terminal`
        // auto-subscribes it. Subscribing every pane also lets the per-pane
        // actor fan out live output to this client (terminal_actor's
        // subscriber loop), so non-focused panes stay live too.
        let session_panes: Vec<TerminalId> = self
            .registry
            .session(session_id)
            .map(|s| s.windows.clone())
            .unwrap_or_default()
            .into_iter()
            .flat_map(|wid| {
                self.registry
                    .window(wid)
                    .map(|w| w.panes.clone())
                    .unwrap_or_default()
            })
            .collect();
        for pane in session_panes {
            let subs = self.terminal_subscribers.entry(pane).or_default();
            if !subs.contains(&client_id) {
                subs.push(client_id);
            }
        }
        Ok(session_id)
    }

    /// Convenience wrapper around [`Self::attach`] that passes
    /// [`ColorSupport::default`] for the client tier. Intended for test
    /// scaffolding and in-tree call sites that don't carry a HELLO-derived
    /// capability value.
    pub fn attach_default_caps(
        &mut self,
        client_id: ClientId,
        session_name: &str,
        tx: mpsc::Sender<Outbound>,
    ) -> Result<SessionId, AttachError> {
        self.attach(
            client_id,
            session_name,
            tx,
            ClientCapabilities::default(),
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
        )
    }

    /// Detach `client_id`, removing it from `attached` and from every
    /// `terminal_subscribers` list it appears in.
    ///
    /// Silent no-op if the client is not currently attached — detach must be
    /// idempotent for the EOF cleanup path in `handle_client`.
    pub fn detach(&mut self, client_id: ClientId) {
        self.attached.remove(&client_id);
        // Release any input leases this client held (ADR-0033) so a
        // disconnect never strands the wheel. The runtime broadcasts the
        // `Released` events (via `leases_held_by`) before calling detach;
        // this clears the state regardless of that path running.
        self.input_leases.retain(|_, holder| *holder != client_id);
        // Hub-side satellite leases follow the same rule (phux-v45.7): the
        // runtime relays the detached RELEASE_INPUT before calling detach;
        // this clears the ledger regardless of that path running.
        self.satellite_leases
            .retain(|_, lease| lease.holder != client_id);
        // Cancel every ATTACH_TERMINAL output pump this client owns
        // (phux-v45.7) so no task keeps streaming into a dead mailbox.
        self.attach_terminal_pumps.retain(|(owner, _), token| {
            if *owner == client_id {
                token.cancel();
                false
            } else {
                true
            }
        });
        for subs in self.terminal_subscribers.values_mut() {
            subs.retain(|c| *c != client_id);
        }
        // Drop entries that became empty so the map doesn't grow unboundedly
        // across attach/detach churn.
        self.terminal_subscribers.retain(|_, subs| !subs.is_empty());
        // Drop any L3 metadata subscriptions this client owned (SPEC §7.4
        // says subscriptions are connection-scoped) plus its cached layer
        // negotiation. Keeps the maps bounded across attach churn.
        self.metadata.drop_client(client_id);
        if let Some(keys) = self.session_create_results.remove(&client_id) {
            for key in keys {
                let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, &key);
            }
        }
        self.client_layers.remove(&client_id);
        // Agent-event subscriptions are connection-scoped (SPEC §7.5),
        // same as L3 metadata subscriptions above. Drop them so the map
        // stays bounded across attach churn.
        self.event_subscriptions.remove(&client_id);
    }

    /// Collect the `(client, outbound mailbox)` pairs to force-detach for the
    /// `phux detach` verb (`DETACH_CLIENTS`).
    ///
    /// `session = Some(name)` selects only clients attached to that session
    /// (empty when the name is unknown); `None` selects every attached client.
    /// Each mailbox is cloned so the caller can push a `DETACHED` frame before
    /// running the normal per-client detach teardown, which re-locks state and
    /// so must run off this borrow.
    #[must_use]
    pub fn attached_clients_to_detach(
        &self,
        session: Option<&str>,
    ) -> Vec<(ClientId, mpsc::Sender<Outbound>)> {
        let target_session = match session {
            Some(name) => match self.find_session_by_name(name) {
                Some(id) => Some(id),
                None => return Vec::new(),
            },
            None => None,
        };
        self.attached
            .values()
            .filter(|c| target_session.is_none_or(|sid| c.session == sid))
            .map(|c| (c.id, c.tx.clone()))
            .collect()
    }

    /// Record an agent-event subscription for `client_id` at `scope`
    /// (SPEC §7.5, phux-y2t). Idempotent: re-subscribing the same scope
    /// is a no-op (the per-client scope set absorbs the duplicate). A
    /// `terminal: None` `SUBSCRIBE_EVENTS` maps to [`EventScope::Server`];
    /// a `Some(id)` maps to [`EventScope::Terminal`].
    ///
    /// `tx` is the client's outbound mailbox, captured here so event
    /// fanout reaches a pure `watch` client that never attached. A
    /// re-subscribe leaves the stored mailbox in place (the connection's
    /// tx is stable, so this is a no-op in practice).
    pub fn subscribe_events(
        &mut self,
        client_id: ClientId,
        terminal: Option<WireTerminalId>,
        tx: mpsc::Sender<Outbound>,
    ) {
        let scope = terminal.map_or(EventScope::Server, EventScope::Terminal);
        let entry = self
            .event_subscriptions
            .entry(client_id)
            .or_insert_with(|| EventSubscription {
                tx,
                scopes: HashSet::new(),
            });
        entry.scopes.insert(scope);
    }

    /// Collect the outbound mailbox of every client subscribed to an agent
    /// event scoped to `terminal` (SPEC §7.5, phux-y2t).
    ///
    /// A client receives the event when it subscribed [`EventScope::Server`]
    /// (server-wide) OR, when `terminal` is `Some(id)`, it subscribed
    /// [`EventScope::Terminal`] for that same id. A server-scoped event
    /// (`terminal == None`) reaches only the server-wide subscribers — it
    /// has no single owning Terminal to match a per-pane subscription.
    /// Order is unspecified; callers MUST NOT rely on it. Resolves the
    /// mailbox from the subscription registry, NOT from
    /// [`Self::attached`], so a pure `watch` client (subscribed without an
    /// attach) is still reached.
    #[must_use]
    pub fn event_targets(&self, terminal: Option<&WireTerminalId>) -> Vec<mpsc::Sender<Outbound>> {
        self.event_subscriptions
            .values()
            .filter(|sub| {
                sub.scopes.contains(&EventScope::Server)
                    || terminal
                        .is_some_and(|tid| sub.scopes.contains(&EventScope::Terminal(tid.clone())))
            })
            .map(|sub| sub.tx.clone())
            .collect()
    }

    pub(crate) fn report_agent_asked(
        &mut self,
        terminal: TerminalId,
        source: AskedSource,
        payload: AskedPayload,
    ) -> AskedTransition {
        self.agent_asked.report(terminal, source, payload)
    }

    #[cfg(test)]
    pub(crate) fn current_agent_asked(&self, terminal: TerminalId) -> Option<&AskedPayload> {
        self.agent_asked.current(terminal)
    }

    /// Read the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(crate) const fn agent_records(&self) -> &crate::agent_state::AgentRecordArbiter {
        &self.agent_records
    }

    /// Mutate the `phux.agent/v1` record arbiter (ADR-0046 §E).
    pub(crate) const fn agent_records_mut(
        &mut self,
    ) -> &mut crate::agent_state::AgentRecordArbiter {
        &mut self.agent_records
    }

    /// Arm tmux-model last-session self-exit.
    pub(crate) const fn arm_self_exit(&mut self) {
        self.has_served_client = true;
    }

    /// Whether last-session self-exit has been armed.
    #[must_use]
    pub const fn has_served_client(&self) -> bool {
        self.has_served_client
    }

    /// Reap a pane whose actor has exited, cascading the removal up the
    /// `pane → window → session` tree (phux-60s, the tmux server-lifecycle
    /// model). When the pane's window has no panes left the window is
    /// removed; when that window's session has no windows left the session
    /// is removed.
    ///
    /// Returns `true` iff the server now holds zero sessions — the signal
    /// the runtime uses to self-exit (nothing left to serve). Idempotent on
    /// an unknown or already-reaped pane: it touches nothing and reports the
    /// current emptiness.
    ///
    /// This is the structural counterpart to the `on_terminal_exited`
    /// path in `runtime.rs`: that path detaches clients focused on the
    /// dead pane; this one frees the domain entities and their server-side
    /// bookkeeping (actor handle, token, input log, subscribers, wire-id
    /// interning, and per-Terminal L3 metadata).
    pub fn reap_terminal(&mut self, pane: TerminalId) -> bool {
        // Resolve the parent window before the registry drops the pane.
        let window_id = self.registry.terminal(pane).map(|t| t.window);
        if self.registry.remove_terminal(pane).is_some() {
            self.forget_terminal_bookkeeping(pane);
        }
        let Some(window_id) = window_id else {
            return self.registry.session_count() == 0;
        };

        // Cascade up only while the parent has been emptied.
        let window_empty = self
            .registry
            .window(window_id)
            .is_some_and(|w| w.panes.is_empty());
        if window_empty {
            let session_id = self.registry.window(window_id).map(|w| w.session);
            if self.registry.remove_window(window_id).is_some() {
                self.forget_window_bookkeeping(window_id);
            }
            if let Some(session_id) = session_id {
                let session_empty = self
                    .registry
                    .session(session_id)
                    .is_some_and(|s| s.windows.is_empty());
                if session_empty && self.registry.remove_session(session_id).is_some() {
                    self.forget_session_bookkeeping(session_id);
                }
            }
        }

        self.registry.session_count() == 0
    }

    /// Drop every server-side map entry keyed on a now-removed pane.
    ///
    /// Cancels the actor token defensively (the actor has usually already
    /// exited by the time we reap, but a still-live token is cleanly
    /// resolved by the cancel) and retires the wire id without reuse.
    fn forget_terminal_bookkeeping(&mut self, pane: TerminalId) {
        self.terminals.remove(&pane);
        if let Some(token) = self.terminal_tokens.remove(&pane) {
            token.cancel();
        }
        self.terminal_subscribers.remove(&pane);
        // Cancel the pane's ATTACH_TERMINAL pumps (phux-v45.7): the
        // broadcast channel is closing anyway, but the cancel keeps the
        // token map bounded and the teardown prompt.
        self.attach_terminal_pumps.retain(|(_, terminal), token| {
            if *terminal == pane {
                token.cancel();
                false
            } else {
                true
            }
        });
        self.agent_asked.clear_terminal(pane);
        if let Some(wire) = self.terminal_wire_forward.remove(&pane) {
            self.terminal_wire_reverse.remove(&wire);
            self.metadata.forget_terminal(&wire);
            // The record died with the per-Terminal metadata scope; the
            // arbiter's bookkeeping about who owned it must not outlive it,
            // or a recycled wire id would inherit a stale declaration.
            self.agent_records.forget(&wire);
        }
    }

    /// Retire a removed window's wire-id mapping (no reuse).
    fn forget_window_bookkeeping(&mut self, window: WindowId) {
        if let Some(wire) = self.window_wire_forward.remove(&window) {
            self.window_wire_reverse.remove(&wire);
        }
        // Drop the last-cwd-per-window ledger entry (phux-nyx) so a reused
        // window id can never inherit a dead window's directory.
        self.window_last_cwd.remove(&window);
    }

    /// Forget a removed session's wire id and last-touch ordering entry.
    fn forget_session_bookkeeping(&mut self, session: SessionId) {
        self.session_id_bridge.forget(session);
        self.session_last_touched.remove(&session);
        // Drop the frozen session-root entry (phux-nyx) alongside the rest
        // of the session's bookkeeping.
        self.session_root.remove(&session);
    }

    /// Subscribers (snapshot) for `pane`. Returns an empty slice if no
    /// clients are currently observing the pane.
    #[must_use]
    pub fn subscribers_for_terminal(&self, terminal: TerminalId) -> &[ClientId] {
        self.terminal_subscribers
            .get(&terminal)
            .map_or(&[], Vec::as_slice)
    }

    /// Clone the [`TerminalHandle`] of every pane `client_id` currently
    /// subscribes to (phux-0q8). The runtime uses this at DETACH /
    /// disconnect / EOF time to send a
    /// [`ConsumerDetachRequest`](crate::terminal_actor::ConsumerDetachRequest) to each
    /// pane actor so the per-consumer `RenderState` cache (ADR-0018) is
    /// freed, mirroring the `register_consumer` calls the ATTACH path
    /// made. Gathered under-lock; the sends happen off-lock in the
    /// runtime to avoid awaiting inside `with_mut`.
    #[must_use]
    pub fn subscribed_terminal_handles(&self, client_id: ClientId) -> Vec<TerminalHandle> {
        self.terminal_subscribers
            .iter()
            .filter(|(_, subs)| subs.contains(&client_id))
            .filter_map(|(terminal, _)| self.terminal_handle(*terminal).cloned())
            .collect()
    }

    /// Look up the active pane of the active window of `session`, if any.
    #[must_use]
    pub fn active_pane_of_session(&self, session: SessionId) -> Option<TerminalId> {
        let session = self.registry.session(session)?;
        let window_id = session.active?;
        let window = self.registry.window(window_id)?;
        window.active
    }

    /// Borrow the session named `name`, if it exists.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<&Session> {
        let id = self.find_session_by_name(name)?;
        self.registry.session(id)
    }

    /// Look up the [`SessionId`] for a name by scanning the registry.
    ///
    /// Uses [`Registry::sessions`] directly — no side ledger required.
    pub(crate) fn find_session_by_name(&self, name: &str) -> Option<SessionId> {
        self.registry
            .sessions()
            .find(|(_, s)| s.name == name)
            .map(|(id, _)| id)
    }

    /// Rename the session named `current` to `new_name`, in place.
    ///
    /// Mirrors `CREATE_SESSION`'s uniqueness rule: names are unique within
    /// the registry, so a `new_name` already in use is rejected. Resolution
    /// uses the same registry scan as every other name lookup (no side
    /// ledger, per [`Registry::sessions`]), so there is nothing else to keep
    /// in sync. The server is authoritative once this returns
    /// [`RenameOutcome::Renamed`]; the next `ATTACHED` snapshot each client
    /// builds carries the new name.
    ///
    /// Returns a [`RenameOutcome`] distinguishing the two refusal cases the
    /// wire surfaces (`SESSION_NOT_FOUND` vs `INVALID_COMMAND`) from success.
    pub fn rename_session(&mut self, current: &str, new_name: &str) -> RenameOutcome {
        let Some(id) = self.find_session_by_name(current) else {
            return RenameOutcome::NotFound;
        };
        // A no-op rename (current == new_name) resolves to the same session,
        // so the duplicate check would otherwise reject it. Treat it as
        // success: the name already is what was asked for.
        if current != new_name && self.find_session_by_name(new_name).is_some() {
            return RenameOutcome::NameTaken;
        }
        // Resolution above guarantees the id is live, so `session_mut` is
        // `Some`; the rename is a single field write.
        if let Some(session) = self.registry.session_mut(id) {
            new_name.clone_into(&mut session.name);
        }
        RenameOutcome::Renamed
    }

    /// Seed a session+window+pane. Returns the new
    /// `(SessionId, WindowId, TerminalId)`.
    ///
    /// This is the entry point `ServerConfig::pre_seeded_session` uses to
    /// pre-populate the registry before clients connect.
    ///
    /// # Panics
    ///
    /// Panics if the registry rejects the freshly-allocated session or
    /// window ids — both branches are unreachable because the parent
    /// entity was created on the line above. A panic here indicates a
    /// `phux-core::Registry` regression.
    #[allow(clippy::expect_used, reason = "unreachable: parent just created")]
    pub fn seed_session(
        &mut self,
        name: &str,
    ) -> (SessionId, phux_core::ids::WindowId, TerminalId) {
        let sid = self.registry.new_session(name.to_owned());
        let wid = self.registry.new_window(sid).expect("session just created");
        let pid = self
            .registry
            .new_terminal(wid)
            .expect("window just created");
        (sid, wid, pid)
    }

    /// Add a new pane (Terminal) to `session`'s first window — the spawn
    /// counterpart to [`Self::seed_session`] that does NOT create a new
    /// session.
    ///
    /// A TUI split lands here (phux-i9zl): the new L1 Terminal joins the
    /// current session's window so `phux ls` keeps showing one session, and
    /// a reattach to that session resolves every split pane. Targets the
    /// session's first window — v0.1 sessions are single-window, so that is
    /// the window the client is viewing; multi-window targeting (the client's
    /// active window) is future work.
    ///
    /// Returns `None` if `session` is unknown or has no window — unreachable
    /// for a seeded session, which always has at least one window.
    #[must_use]
    pub fn add_pane_to_session(&mut self, session: SessionId) -> Option<TerminalId> {
        let wid = self.registry.session(session)?.windows.first().copied()?;
        self.registry.new_terminal(wid).ok()
    }

    /// Add a pane to the exact window that owns `owner`.
    ///
    /// Used by headless spawn ownership targeting: the caller names a known
    /// Terminal so the request cannot drift into another session or window.
    /// Layout geometry remains client-owned L3 metadata.
    #[must_use]
    pub fn add_pane_to_terminal_owner(&mut self, owner: &WireTerminalId) -> Option<TerminalId> {
        let owner = self.terminal_from_wire(owner)?;
        let window = self.registry.terminal(owner)?.window;
        self.registry.new_terminal(window).ok()
    }

    /// Record a freshly-spawned [`TerminalHandle`] against `pane` and
    /// allocate its wire id.
    ///
    /// Called by the runtime after `TerminalActor::new` /
    /// `build_with_token`. Subsequent attaches use
    /// [`Self::terminal_handle`] to look the handle up.
    ///
    /// `token` is stashed in `terminal_tokens`; cancelling it (e.g. via
    /// [`Self::detach_terminal_actor`]) fires the actor's shutdown branch.
    ///
    /// This method does NOT spawn the actor — pair it with
    /// [`Self::spawn_terminal_actor`] when you also want the actor task
    /// registered against the per-server `JoinSet`.
    ///
    /// Idempotent on the wire-id allocation (a second call for the
    /// same `pane` returns the same wire id) but overwrites the
    /// `TerminalHandle` / token. In practice the runtime calls this
    /// exactly once per pane lifetime.
    pub fn register_terminal_handle(
        &mut self,
        terminal: TerminalId,
        handle: TerminalHandle,
        token: CancellationToken,
    ) -> WireTerminalId {
        let wire = self.intern_terminal_wire(terminal);
        self.terminals.insert(terminal, handle);
        self.terminal_tokens.insert(terminal, token);
        wire
    }

    /// One-shot helper: register `handle`/`token` AND spawn
    /// `actor_future` onto the per-server pane `JoinSet`. Must be
    /// called from inside a `LocalSet` (per ADR-0014; pane actors
    /// own `!Send` `Terminal`s and are spawned via
    /// `JoinSet::spawn_local`).
    ///
    /// Returns the wire pane id, matching [`Self::register_terminal_handle`].
    pub fn spawn_terminal_actor<F>(
        &mut self,
        terminal: TerminalId,
        handle: TerminalHandle,
        token: CancellationToken,
        actor_future: F,
    ) -> WireTerminalId
    where
        F: Future<Output = ()> + 'static,
    {
        let wire = self.register_terminal_handle(terminal, handle, token);
        self.terminal_tasks.spawn_local(actor_future);
        wire
    }

    /// Cancel `pane`'s actor token, signalling the `TerminalActor` to
    /// exit, and forget the token. Idempotent. Used by future
    /// pane-close lifecycle paths; not exercised by `phux-byc.8`.
    ///
    /// The actor task itself is drained from the per-server `JoinSet`
    /// when it returns from `run`; we don't need to touch
    /// `terminal_tasks` here.
    pub fn detach_terminal_actor(&mut self, terminal: TerminalId) {
        if let Some(token) = self.terminal_tokens.remove(&terminal) {
            token.cancel();
        }
    }

    /// Look up the [`TerminalHandle`] for `pane`, if registered.
    #[must_use]
    pub fn terminal_handle(&self, terminal: TerminalId) -> Option<&TerminalHandle> {
        self.terminals.get(&terminal)
    }

    /// The client currently holding `pane`'s input lease (ADR-0033), or
    /// `None` if the pane is `Open`.
    #[must_use]
    pub fn input_lease_holder(&self, terminal: TerminalId) -> Option<ClientId> {
        self.input_leases.get(&terminal).copied()
    }

    /// Whether `client`'s input to `pane` is blocked by another client's
    /// lease (ADR-0033). `false` when the pane is `Open` or `client` is the
    /// holder. The gate calls this before forwarding input to the actor.
    #[must_use]
    pub fn input_blocked(&self, terminal: TerminalId, client: ClientId) -> bool {
        self.input_leases
            .get(&terminal)
            .is_some_and(|holder| *holder != client)
    }

    /// Grant `pane`'s input lease to `client` (ADR-0033), returning the prior
    /// holder if the lease was already held (a `Seize` preemption).
    pub fn set_input_lease(&mut self, terminal: TerminalId, client: ClientId) -> Option<ClientId> {
        self.input_leases.insert(terminal, client)
    }

    /// Release `pane`'s input lease if `client` holds it (ADR-0033). Returns
    /// `true` if a lease was actually released. A no-op (returns `false`) if
    /// the pane is `Open` or held by someone else.
    pub fn release_input_lease(&mut self, terminal: TerminalId, client: ClientId) -> bool {
        if self.input_leases.get(&terminal) == Some(&client) {
            self.input_leases.remove(&terminal);
            true
        } else {
            false
        }
    }

    /// Every pane whose input lease `client` currently holds (ADR-0033). The
    /// runtime reads this at disconnect time to broadcast `Released` events
    /// before [`Self::detach`] clears the leases.
    #[must_use]
    pub fn leases_held_by(&self, client: ClientId) -> Vec<TerminalId> {
        self.input_leases
            .iter()
            .filter_map(|(pane, holder)| (*holder == client).then_some(*pane))
            .collect()
    }

    /// The hub consumer currently holding the input lease over satellite
    /// terminal `(host, id)` (phux-v45.7, L1 §9.1), or `None` when free.
    /// See the `satellite_leases` field doc for why this ledger exists.
    #[must_use]
    pub fn satellite_lease_holder(
        &self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
    ) -> Option<ClientId> {
        self.satellite_leases
            .get(&(host.clone(), terminal))
            .map(|lease| lease.holder)
    }

    /// Record `client` (with its outbound mailbox `out_tx`) as the
    /// hub-side holder of the satellite lease, after the satellite acked
    /// the relayed `ACQUIRE_INPUT`.
    ///
    /// Returns the **evicted** prior lease when this acquire preempted a
    /// *different* hub consumer (a SEIZE takeover, phux-v45.13): the caller
    /// notifies that holder it lost the wheel. A re-acquire by the same
    /// holder (idempotent cooperative acquire) or a grant over a free lease
    /// returns `None` — nobody was evicted.
    pub(crate) fn set_satellite_lease(
        &mut self,
        host: phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
        out_tx: mpsc::Sender<Outbound>,
    ) -> Option<SatelliteLease> {
        let prior = self.satellite_leases.insert(
            (host, terminal),
            SatelliteLease {
                holder: client,
                out_tx,
            },
        );
        prior.filter(|lease| lease.holder != client)
    }

    /// Release the hub-side satellite lease over `(host, terminal)` if
    /// `client` holds it. Returns `true` when an entry was removed.
    pub fn release_satellite_lease(
        &mut self,
        host: &phux_protocol::ids::SatelliteHost,
        terminal: u32,
        client: ClientId,
    ) -> bool {
        let key = (host.clone(), terminal);
        if self.satellite_leases.get(&key).map(|lease| lease.holder) == Some(client) {
            self.satellite_leases.remove(&key);
            true
        } else {
            false
        }
    }

    /// Every satellite lease `client` currently holds. Read at disconnect
    /// time so the runtime can relay a detached `RELEASE_INPUT` per entry
    /// before [`Self::detach`] clears the ledger.
    #[must_use]
    pub fn satellite_leases_held_by(
        &self,
        client: ClientId,
    ) -> Vec<(phux_protocol::ids::SatelliteHost, u32)> {
        self.satellite_leases
            .iter()
            .filter(|(_, lease)| lease.holder == client)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Register (and return the token for) an `ATTACH_TERMINAL` output
    /// pump for `(client, terminal)` (phux-v45.7). Returns `None` when a
    /// pump is already live for the pair — the idempotent re-attach must
    /// not double-stream.
    pub fn register_attach_terminal_pump(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
    ) -> Option<tokio_util::sync::CancellationToken> {
        use std::collections::hash_map::Entry;
        match self.attach_terminal_pumps.entry((client, terminal)) {
            Entry::Occupied(_) => None,
            Entry::Vacant(slot) => {
                let token = tokio_util::sync::CancellationToken::new();
                slot.insert(token.clone());
                Some(token)
            }
        }
    }

    /// Cancel and forget the `ATTACH_TERMINAL` pump for `(client,
    /// terminal)`, if one is live. Idempotent.
    pub fn cancel_attach_terminal_pump(&mut self, client: ClientId, terminal: TerminalId) {
        if let Some(token) = self.attach_terminal_pumps.remove(&(client, terminal)) {
            token.cancel();
        }
    }

    /// Remove `client` from `terminal`'s subscriber list (the
    /// `DETACH_TERMINAL` counterpart of the attach-time registration).
    pub fn unsubscribe_terminal(&mut self, client: ClientId, terminal: TerminalId) {
        if let Some(subs) = self.terminal_subscribers.get_mut(&terminal) {
            subs.retain(|c| *c != client);
            if subs.is_empty() {
                self.terminal_subscribers.remove(&terminal);
            }
        }
    }

    /// Drop `client`'s per-terminal agent-event subscription for `wire`
    /// (`DETACH_TERMINAL`, phux-v45.7). Server-wide subscriptions and
    /// other terminals' scopes are untouched; an empty scope set drops
    /// the whole entry so the map stays bounded.
    pub fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireTerminalId) {
        if let Some(sub) = self.event_subscriptions.get_mut(&client) {
            sub.scopes.remove(&EventScope::Terminal(wire.clone()));
            if sub.scopes.is_empty() {
                self.event_subscriptions.remove(&client);
            }
        }
    }

    /// Wire pane id for `pane`, allocating one if needed.
    ///
    /// Mirrors [`IdBridge::intern`] but inline here for pane ids — see
    /// the field-level note on `terminal_wire_forward` for why a second
    /// general-purpose `IdBridge` is deferred.
    pub fn intern_terminal_wire(&mut self, terminal: TerminalId) -> WireTerminalId {
        if let Some(w) = self.terminal_wire_forward.get(&terminal) {
            return w.clone();
        }
        let raw = self.next_terminal_wire_id;
        self.next_terminal_wire_id = self.next_terminal_wire_id.saturating_add(1);
        let wire = WireTerminalId::local(raw);
        self.terminal_wire_forward.insert(terminal, wire.clone());
        self.terminal_wire_reverse.insert(wire.clone(), terminal);
        wire
    }

    /// Reverse lookup: which core pane id (if any) does `wire`
    /// resolve to?
    #[must_use]
    pub fn terminal_from_wire(&self, wire: &WireTerminalId) -> Option<TerminalId> {
        self.terminal_wire_reverse.get(wire).copied()
    }

    /// Wire window id for `window`, allocating one if needed.
    pub fn intern_window_wire(&mut self, window: WindowId) -> WireWindowId {
        if let Some(w) = self.window_wire_forward.get(&window) {
            return *w;
        }
        let raw = self.next_window_wire_id;
        self.next_window_wire_id = self.next_window_wire_id.saturating_add(1);
        let wire = WireWindowId(raw);
        self.window_wire_forward.insert(window, wire);
        self.window_wire_reverse.insert(wire, window);
        wire
    }

    /// Build a [`phux_protocol::wire::info::SessionSnapshot`] describing
    /// the entire registry plus the attaching client's initial focus.
    ///
    /// Used by the ATTACH handler in [`crate::runtime`] to populate the
    /// `ATTACHED` frame per SPEC §13. Allocates wire ids on demand so
    /// every entity in the registry gets one before this returns.
    ///
    /// `focus_session` is the resolved target of the ATTACH request;
    /// the attaching client's focused window/pane fall back to the
    /// session's `active` / window's `active` (tmux semantics).
    /// Returns `None` if `focus_session` has no active window or pane,
    /// since `SessionSnapshot::focused_window` / `focused_pane` are
    /// required fields on the wire.
    pub fn build_session_snapshot(
        &mut self,
        focus_session: SessionId,
    ) -> Option<phux_protocol::wire::info::SessionSnapshot> {
        use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo, WindowInfo};

        let attached_counts: HashMap<SessionId, u16> = {
            let mut counts: HashMap<SessionId, u16> = HashMap::new();
            for c in self.attached.values() {
                *counts.entry(c.session).or_insert(0) = counts
                    .get(&c.session)
                    .copied()
                    .unwrap_or(0)
                    .saturating_add(1);
            }
            counts
        };

        let session_pairs: Vec<(SessionId, Session)> = self
            .registry
            .sessions()
            .map(|(id, s)| (id, s.clone()))
            .collect();

        let mut sessions = Vec::with_capacity(session_pairs.len());
        let mut windows = Vec::new();
        let mut panes = Vec::new();

        for (sid, session) in &session_pairs {
            let session_wire = self.session_id_bridge.intern(*sid);
            // Pre-intern the active window so `active_window` round-trips.
            let active_window_wire = session.active.map(|w| self.intern_window_wire(w));

            let created_at_unix_secs = session
                .created_at
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            sessions.push(
                SessionInfo::new(session_wire, session.name.clone())
                    .with_active_window(active_window_wire)
                    .with_created_at_unix_secs(created_at_unix_secs)
                    .with_window_count(u16::try_from(session.windows.len()).unwrap_or(u16::MAX))
                    .with_attached_client_count(attached_counts.get(sid).copied().unwrap_or(0)),
            );

            for (index, wid) in session.windows.iter().enumerate() {
                let Some(window) = self.registry.window(*wid).cloned() else {
                    continue;
                };
                let window_wire = self.intern_window_wire(*wid);
                let active_pane_wire = window.active.map(|p| self.intern_terminal_wire(p));

                // Layout-on-the-wire mirroring is its own concern;
                // for phux-byc.8 we ship `None` and let later tickets
                // translate `phux_core::LayoutNode` →
                // `phux_protocol::wire::info::LayoutNode`.
                windows.push(
                    WindowInfo::new(window_wire, session_wire, format!("window-{index}"))
                        .with_index(u16::try_from(index).unwrap_or(u16::MAX))
                        .with_active_pane(active_pane_wire),
                );

                for pid in &window.panes {
                    let Some(terminal) = self.registry.terminal(*pid).cloned() else {
                        continue;
                    };
                    let terminal_wire = self.intern_terminal_wire(*pid);
                    let cwd =
                        Some(terminal.cwd.to_string_lossy().into_owned()).filter(|s| !s.is_empty());
                    panes.push(
                        TerminalInfo::new(
                            terminal_wire,
                            window_wire,
                            terminal.dims.0,
                            terminal.dims.1,
                        )
                        .with_title(terminal.title.clone())
                        .with_cwd(cwd),
                    );
                }
            }
        }

        let session = self.registry.session(focus_session)?;
        let focused_window = session.active?;
        let focused_pane = self.registry.window(focused_window)?.active?;

        let focused_session_wire = self.session_id_bridge.intern(focus_session);
        let focused_window_wire = self.intern_window_wire(focused_window);
        let focused_pane_wire = self.intern_terminal_wire(focused_pane);

        Some(
            SessionSnapshot::new(focused_session_wire, focused_window_wire, focused_pane_wire)
                .with_sessions(sessions)
                .with_windows(windows)
                .with_panes(panes),
        )
    }

    /// Collect panes in `session` that have live actor handles, with wire ids.
    ///
    /// Protocol dispatch (`runtime::handle_attach`) uses this to drive per-pane
    /// snapshot/output setup without touching `Session`/`Window` internals.
    #[must_use]
    pub fn attach_snapshot_panes(&mut self, session: SessionId) -> Vec<AttachSnapshotPane> {
        let window_ids = self
            .registry
            .session(session)
            .map(|s| s.windows.clone())
            .unwrap_or_default();
        let mut panes = Vec::new();
        for wid in window_ids {
            let window_panes = self
                .registry
                .window(wid)
                .map(|w| w.panes.clone())
                .unwrap_or_default();
            for pid in window_panes {
                if let Some(handle) = self.terminal_handle(pid).cloned() {
                    panes.push(AttachSnapshotPane {
                        terminal_id: pid,
                        handle,
                        wire_terminal_id: self.intern_terminal_wire(pid),
                    });
                }
            }
        }
        panes
    }
}
