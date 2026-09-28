use std::collections::HashMap;

use phux_core::ids::{ResourceId, SessionId};
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, ColorSupport, LayerSet,
};
use phux_protocol::ids::ResourceId as WireResourceId;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::ServerState;
use crate::mailbox::Outbound;
use crate::resource::ResourceHandle;

/// Server-assigned client id, monotonic from 1, used only for routing
/// inside [`super::ServerState`] (not the wire `ClientId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

/// An attached client: routing identity plus outbound mailbox.
#[derive(Debug)]
pub struct AttachedClient {
    /// Server-assigned client id.
    pub id: ClientId,
    /// The session this client is observing.
    pub session: SessionId,
    /// Outbound mailbox, drained by the per-client write task.
    pub tx: mpsc::Sender<Outbound>,
    /// Capabilities from HELLO (SPEC §6.2); outbound bytes are downsampled
    /// to them. Without a HELLO, the permissive default.
    pub client_caps: ClientCapabilities,
    /// Immutable bootstrap profile selected for this HELLO.
    pub bootstrap_profile: BootstrapProfile,
    /// Immutable payload limits selected for this HELLO.
    pub bootstrap_limits: BootstrapLimits,
    /// Current outer viewport; shared-Terminal geometry applies the
    /// `defaults.window-size` policy across subscribers' viewports.
    pub viewport: Option<phux_protocol::wire::frame::ViewportInfo>,
    /// Viewport-clock stamp of the last `viewport` set, so cell-pixel
    /// resolution can prefer the newest report.
    pub viewport_seq: u64,
    /// The session attach declared `VIEWER` (ADR-0127); panes it spawns are
    /// observe-only too.
    pub viewer: bool,
}

/// One pane target in an ATTACH snapshot pass.
#[derive(Debug, Clone)]
pub struct AttachSnapshotPane {
    /// Core pane identifier.
    pub terminal_id: ResourceId,
    /// Cross-task handle to the pane's engine.
    pub handle: ResourceHandle,
    /// Stable wire id to use in `TERMINAL_SNAPSHOT` / `RESOURCE_OUTPUT`.
    pub wire_terminal_id: WireResourceId,
}

/// Errors from [`super::ServerState::attach`] (not the client-side
/// `AttachError`).
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
    /// The attached clients, read-only (writes stay in `state`).
    #[must_use]
    pub const fn attached(&self) -> &HashMap<ClientId, AttachedClient> {
        &self.clients.attached
    }

    /// Record the HELLO layer set (latest wins).
    pub fn set_client_layers(&mut self, client_id: ClientId, layers: LayerSet) {
        self.clients.set_layers(client_id, layers);
    }

    /// The HELLO layer set; [`LayerSet::all`] if none was seen.
    #[must_use]
    pub fn client_layers(&self, client_id: ClientId) -> LayerSet {
        self.clients.layers(client_id)
    }

    /// Whether `client_id` negotiated L3 (SPEC §16.4).
    #[must_use]
    pub fn client_speaks_l3(&self, client_id: ClientId) -> bool {
        self.clients.speaks_l3(client_id)
    }

    /// Allocate the next monotonic [`ClientId`].
    pub const fn new_client_id(&mut self) -> ClientId {
        self.clients.new_client_id()
    }

    /// Register the transport's cancellation root (the relay uses it to end
    /// a connection other sender clones keep alive).
    pub fn set_client_connection_cancellation(
        &mut self,
        client_id: ClientId,
        token: CancellationToken,
    ) {
        self.clients
            .connection_cancellations
            .insert(client_id, token);
    }

    /// Clone the cancellation root for one live client transport.
    #[must_use]
    pub fn client_connection_cancellation(&self, client_id: ClientId) -> Option<CancellationToken> {
        self.clients
            .connection_cancellations
            .get(&client_id)
            .cloned()
    }

    /// Attach a client to `session_name`, subscribing it to every pane in
    /// the session. `client_caps` come from HELLO; see
    /// [`Self::attach_default_caps`] otherwise.
    pub fn attach(
        &mut self,
        client_id: ClientId,
        session_name: &str,
        tx: mpsc::Sender<Outbound>,
        client_caps: ClientCapabilities,
        bootstrap_profile: BootstrapProfile,
        bootstrap_limits: BootstrapLimits,
    ) -> Result<SessionId, AttachError> {
        // Resolve first: re-attaching the same session is a re-bootstrap;
        // switching sessions is refused; an unknown name stays
        // UnknownSession.
        let session_id = self
            .find_session_by_name(session_name)
            .ok_or_else(|| AttachError::UnknownSession(session_name.to_owned()))?;
        if let Some(existing) = self.clients.attached.get(&client_id) {
            if existing.session != session_id {
                return Err(AttachError::AlreadyAttached(client_id));
            }
            // Same session: keep the record; the sweep below picks up new
            // panes.
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
                    viewer: false,
                },
            );
            // Attaching arms tmux-model last-session self-exit (phux-60s).
            self.arm_self_exit();
        }

        // Subscribe every pane, not just the active one: the input gate
        // drops keys to unsubscribed panes, and every pane renders live.
        let session_panes: Vec<ResourceId> = self
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
                    .map(|w| w.slots.clone())
                    .unwrap_or_default()
            })
            .collect();
        for pane in session_panes {
            self.resources.subscribe(client_id, pane);
        }
        Ok(session_id)
    }

    /// [`Self::attach`] with default capabilities and bootstrap contract.
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

    /// Update an attached client's capabilities (a late HELLO is tolerated);
    /// `false` if not attached.
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

    /// Detach `client_id`: attachment teardown (idempotent).
    ///
    /// Clears attachment-scoped state (leases, pumps, subscriptions, L3 and
    /// event subscriptions per `docs/spec/L3.md` §1.2). Connection-scoped
    /// state (layers, peer identity) survives until
    /// [`Self::forget_connection`].
    pub fn detach(&mut self, client_id: ClientId) {
        let detached_session = self
            .clients
            .attached
            .remove(&client_id)
            .map(|client| client.session);
        // Drop local and satellite leases; the runtime already broadcast and
        // relayed the releases.
        self.leases.release_all_for(client_id);
        // Cancel owned pumps, then drop the client from subscriber lists.
        self.resources.cancel_pumps_for_client(client_id);
        self.resources.drop_client_subscriptions(client_id);
        // L3 subscriptions are attachment-scoped (L3.md §1.2).
        self.metadata.drop_client(client_id);
        self.clients.metadata_mailboxes.remove(&client_id);
        // The mailbox half of each `ATTACH_RESOURCE` registration.
        self.clients.terminal_mailboxes.remove(&client_id);
        if let Some(keys) = self.clients.session_create_results.remove(&client_id) {
            for key in keys {
                let _ = self.metadata_delete(&phux_protocol::wire::frame::Scope::Global, &key);
            }
        }
        // Event subscriptions follow the same lifecycle (SPEC §7.5).
        if let Some(sub) = self.clients.event_subscriptions.remove(&client_id) {
            sub.retire();
        }
        if let Some(session) = detached_session {
            self.restore_session_geometry_after_detach(session);
        }
    }

    /// Forget `client_id`'s connection-scoped state after [`Self::detach`];
    /// only when the transport goes away. Clearing the layers early would
    /// fail open (unknown clients default to all layers), and clearing the
    /// peer identity would break the local `SHUTDOWN` gate. Idempotent.
    pub fn forget_connection(&mut self, client_id: ClientId) {
        self.detach(client_id);
        // `VIEWER` marks last until the connection ends (ADR-0127).
        self.clients.viewers.remove(&client_id);
        self.clients.layers.remove(&client_id);
        self.clients.client_names.remove(&client_id);
        self.clients.connection_cancellations.remove(&client_id);
        self.clients.revocation_signals.remove(&client_id);
        self.remove_peer_identity(client_id);
    }

    /// `(client, mailbox)` pairs to force-detach for `DETACH_CLIENTS`:
    /// clients of `session`, or all when `None`. Mailboxes are cloned so the
    /// caller can send `DETACHED` before the teardown re-locks.
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

    /// Session-attached clients of `session` by stable id (works after the
    /// session was reaped; per-terminal consumers excluded).
    #[must_use]
    pub fn attached_clients_in_session(
        &self,
        session: SessionId,
    ) -> Vec<(ClientId, mpsc::Sender<Outbound>)> {
        self.clients.attached_in_session(session)
    }

    /// Whether `client_id` holds an agent-event subscription.
    #[cfg(test)]
    pub(crate) fn has_event_subscription(&self, client_id: ClientId) -> bool {
        self.clients.event_subscriptions.contains_key(&client_id)
    }

    /// Whether `client_id` holds an L3 metadata subscription on `(scope, key)`.
    #[cfg(test)]
    pub(crate) fn has_metadata_subscription(
        &self,
        client_id: ClientId,
        scope: &phux_protocol::wire::frame::Scope,
        key: &str,
    ) -> bool {
        self.metadata
            .subscribers_for(scope, key)
            .contains(&client_id)
    }
}

#[cfg(test)]
mod connection_cancellation_tests {
    use super::*;

    #[test]
    fn connection_cancellation_survives_detach_and_is_removed_on_forget() {
        let mut state = ServerState::new();
        let client = state.new_client_id();
        let token = CancellationToken::new();
        state.set_client_connection_cancellation(client, token.clone());

        state.detach(client);
        let registered = state
            .client_connection_cancellation(client)
            .expect("detach preserves connection-scoped cancellation");
        registered.cancel();
        assert!(token.is_cancelled());

        state.forget_connection(client);
        assert!(state.client_connection_cancellation(client).is_none());
    }
}
