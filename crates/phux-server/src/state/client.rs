use std::collections::HashMap;

use phux_core::ids::{SessionId, TerminalId};
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, ColorSupport, LayerSet,
};
use phux_protocol::ids::TerminalId as WireTerminalId;
use thiserror::Error;
use tokio::sync::mpsc;

use super::ServerState;
use super::input_log::Outbound;
use crate::terminal_actor::TerminalHandle;

/// Server-assigned identifier for an attached client.
///
/// Distinct from [`phux_protocol::ids::ClientId`] (which is the wire-level
/// identity carried in protocol messages): this one is allocated by the
/// server, monotonic from `1`, and used purely for routing inside
/// [`super::ServerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

/// An attached client: routing identity plus outbound mailbox.
#[derive(Debug)]
pub struct AttachedClient {
    /// Server-assigned client id.
    pub id: ClientId,
    /// The session this client is observing.
    pub session: SessionId,
    /// Outbound mailbox; the per-client write task drains this and writes to
    /// the socket.
    pub tx: mpsc::Sender<Outbound>,
    /// The client's advertised capabilities (SPEC §6.2). The server MUST
    /// downsample outbound terminal bytes to this set before fanout — see
    /// [`crate::downsample::rewrite_bytes_with_caps`] for the helper the
    /// fanout layer plugs into.
    ///
    /// Populated from the [`phux_protocol::caps::ClientCapabilities`] the
    /// client advertised in HELLO (SPEC §6.1) and forwarded into
    /// [`super::ServerState::attach`]. Test scaffolding that never observed a
    /// HELLO calls [`super::ServerState::attach_default_caps`] which falls back
    /// to [`ClientCapabilities::default`] (most-permissive — never silently
    /// downgrades).
    pub client_caps: ClientCapabilities,
    /// Immutable bootstrap profile selected for this HELLO.
    pub bootstrap_profile: BootstrapProfile,
    /// Immutable payload limits selected for this HELLO.
    pub bootstrap_limits: BootstrapLimits,
    /// This client's current outer viewport (`phux-nk07`).
    ///
    /// Set from the `ATTACH` viewport and updated on every `VIEWPORT_RESIZE`.
    /// The server resolves a shared Terminal's authoritative PTY geometry by
    /// applying the `defaults.window-size` policy across the viewports of
    /// every client subscribed to that Terminal — replacing the old
    /// last-writer-wins resize, where two differently-sized clients thrashed
    /// each other's grid. `None` until the client announces a viewport.
    pub viewport: Option<phux_protocol::wire::frame::ViewportInfo>,
    /// Stamp from [`super::ServerState`]'s viewport clock, taken when
    /// `viewport` was last set. Orders viewport announcements across
    /// clients so cell-pixel resolution
    /// ([`super::ServerState::resolve_terminal_cell_px`]) can prefer the
    /// most recent usable pixel report. `0` until the client announces
    /// a viewport.
    pub viewport_seq: u64,
}

/// One pane target in an ATTACH snapshot pass.
///
/// Bridges the protocol-facing attach flow (`runtime.rs`) to the
/// state-internal registry topology without exposing `Session`/`Window`
/// traversal details outside this module.
#[derive(Debug, Clone)]
pub struct AttachSnapshotPane {
    /// Core pane identifier.
    pub terminal_id: TerminalId,
    /// Cross-task handle for snapshot/input/resize requests.
    pub handle: TerminalHandle,
    /// Stable wire id to use in `TERMINAL_SNAPSHOT` / `TERMINAL_OUTPUT`.
    pub wire_terminal_id: WireTerminalId,
}

/// Errors returned by [`super::ServerState::attach`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AttachError {
    /// No session with that name was found in the registry.
    #[error("unknown session: {0}")]
    UnknownSession(String),
    /// The given [`ClientId`] is already attached.
    #[error("client {0:?} is already attached")]
    AlreadyAttached(ClientId),
    /// The session cannot fit the bounded aggregate attach preflight.
    #[error("session exceeds aggregate attach resource limits")]
    ResourceLimit,
}

impl ServerState {
    /// Borrow the currently attached clients, keyed by server-assigned id.
    ///
    /// Read-only by construction: every write to the map goes through
    /// [`Self::attach`], [`Self::detach`], the capability setters, or
    /// [`Self::set_client_viewport`], all of which live in `state`.
    #[must_use]
    pub const fn attached(&self) -> &HashMap<ClientId, AttachedClient> {
        &self.clients.attached
    }

    /// Record the layer set advertised by `client_id` in HELLO. Called
    /// from the runtime's HELLO handler. Re-set is idempotent (the
    /// most recent HELLO wins, matching `ColorSupport`).
    pub fn set_client_layers(&mut self, client_id: ClientId, layers: LayerSet) {
        self.clients.set_layers(client_id, layers);
    }

    /// Look up the layer set advertised by `client_id`. Defaults to
    /// [`LayerSet::all`] for clients we never saw a HELLO from — the
    /// permissive default matches test scaffolding that skips HELLO.
    #[must_use]
    pub fn client_layers(&self, client_id: ClientId) -> LayerSet {
        self.clients.layers(client_id)
    }

    /// `true` iff `client_id` has L3 in its negotiated `HELLO.layers`.
    /// Gates emission of `METADATA_CHANGED` per SPEC §16.4.
    #[must_use]
    pub fn client_speaks_l3(&self, client_id: ClientId) -> bool {
        self.clients.speaks_l3(client_id)
    }

    /// Allocate the next monotonic [`ClientId`].
    ///
    /// Ids are never reused. `0` is intentionally skipped so log entries
    /// printing `client=0` are obviously a placeholder, not a real client.
    pub const fn new_client_id(&mut self) -> ClientId {
        self.clients.new_client_id()
    }

    /// Attach a client to the session with `session_name`.
    ///
    /// On success the client is recorded in `attached` and subscribed to the
    /// session's currently active pane (if any). Returns a borrow of the
    /// [`phux_core::Session`] for callers that want to build an `ATTACHED`
    /// snapshot.
    ///
    /// `client_caps` are the capabilities the client advertised in HELLO
    /// (SPEC §6.1/§6.2).
    /// Callers that never observed a HELLO (test scaffolding) MAY pass
    /// [`ClientCapabilities::default`]; the convenience wrapper
    /// [`Self::attach_default_caps`] does that for them.
    pub fn attach(
        &mut self,
        client_id: ClientId,
        session_name: &str,
        tx: mpsc::Sender<Outbound>,
        client_caps: ClientCapabilities,
        bootstrap_profile: BootstrapProfile,
        bootstrap_limits: BootstrapLimits,
    ) -> Result<SessionId, AttachError> {
        // Resolve the session BEFORE the already-attached check: a second
        // attach naming the SAME session is a re-bootstrap (the client is
        // renegotiating its profile, or recovering), not an error. Only a
        // client trying to switch sessions on one connection is refused. The
        // check therefore needs the resolved id, so an unknown session name
        // still reports UnknownSession rather than AlreadyAttached.
        let session_id = self
            .find_session_by_name(session_name)
            .ok_or_else(|| AttachError::UnknownSession(session_name.to_owned()))?;
        if let Some(existing) = self.clients.attached.get(&client_id) {
            if existing.session != session_id {
                return Err(AttachError::AlreadyAttached(client_id));
            }
            // Same session: keep the live record (and its identity) and fall
            // through to the subscription sweep below, which is idempotent and
            // picks up panes created since the first attach.
        } else {
            self.clients.attached.insert(
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
        }

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
            .sessions
            .registry
            .session(session_id)
            .map(|s| s.windows.clone())
            .unwrap_or_default()
            .into_iter()
            .flat_map(|wid| {
                self.sessions
                    .registry
                    .window(wid)
                    .map(|w| w.panes.clone())
                    .unwrap_or_default()
            })
            .collect();
        for pane in session_panes {
            self.terminal_table.subscribe(client_id, pane);
        }
        Ok(session_id)
    }

    /// Convenience wrapper around [`Self::attach`] that passes
    /// [`ClientCapabilities::default`] plus the baseline bootstrap contract.
    /// Intended for test scaffolding and in-tree call sites that do not carry
    /// a HELLO-derived capability value.
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

    /// Update the recorded [`ClientCapabilities`] for an already-attached
    /// client. Returns `false` if the client is not in [`Self::attached`].
    ///
    /// Used by the HELLO handler if a HELLO arrives after ATTACH (out of
    /// spec, but tolerated for forward-compat — the alternative is a
    /// protocol-error close that gives the operator no breadcrumbs).
    pub fn set_client_capabilities(
        &mut self,
        client_id: ClientId,
        client_caps: ClientCapabilities,
    ) -> bool {
        self.clients.set_capabilities(client_id, client_caps)
    }

    /// Compatibility wrapper for tests that still update color only.
    pub fn set_client_color_support(
        &mut self,
        client_id: ClientId,
        color_support: ColorSupport,
    ) -> bool {
        self.clients.set_color_support(client_id, color_support)
    }

    /// Detach `client_id`, removing it from `attached` and from every
    /// `terminal_subscribers` list it appears in.
    ///
    /// Silent no-op if the client is not currently attached — detach must be
    /// idempotent for the EOF cleanup path in `handle_client`.
    pub fn detach(&mut self, client_id: ClientId) {
        self.clients.attached.remove(&client_id);
        // Release any input leases this client held (ADR-0033) so a
        // disconnect never strands the wheel, local and hub-side satellite
        // (phux-v45.7) in one step. The runtime broadcasts the `Released`
        // events (via `leases_held_by`) and relays the detached
        // RELEASE_INPUT (via `satellite_leases_held_by`) before calling
        // detach; this clears both ledgers regardless of those paths
        // running.
        self.leases.release_all_for(client_id);
        // Cancel every ATTACH_TERMINAL output pump this client owns
        // (phux-v45.7) so no task keeps streaming into a dead mailbox, then
        // drop it from every subscriber list (empty lists are GC'd so the
        // map doesn't grow unboundedly across attach/detach churn).
        self.terminal_table.cancel_pumps_for_client(client_id);
        self.terminal_table.drop_client_subscriptions(client_id);
        // Drop any L3 metadata subscriptions this client owned (SPEC §7.4
        // says subscriptions are connection-scoped) plus its cached layer
        // negotiation. Keeps the maps bounded across attach churn.
        self.metadata.drop_client(client_id);
        if let Some(keys) = self.clients.session_create_results.remove(&client_id) {
            for key in keys {
                let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, &key);
            }
        }
        self.clients.layers.remove(&client_id);
        // Agent-event subscriptions are connection-scoped (SPEC §7.5),
        // same as L3 metadata subscriptions above. Drop them so the map
        // stays bounded across attach churn.
        self.clients.event_subscriptions.remove(&client_id);
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
        self.clients
            .attached
            .values()
            .filter(|c| target_session.is_none_or(|sid| c.session == sid))
            .map(|c| (c.id, c.tx.clone()))
            .collect()
    }

    /// Collect session-attached clients observing `session` by its stable id.
    ///
    /// Unlike [`Self::attached_clients_to_detach`], this remains usable after
    /// the registry has reaped the session and its name can no longer resolve.
    /// Per-terminal `ATTACH_TERMINAL` consumers are not in [`Self::attached`]
    /// and are deliberately excluded.
    #[must_use]
    pub fn attached_clients_in_session(
        &self,
        session: SessionId,
    ) -> Vec<(ClientId, mpsc::Sender<Outbound>)> {
        self.clients.attached_in_session(session)
    }
}
