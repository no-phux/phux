//! Per-connection negotiated state, QUIC stream events, and releasing a consumer's state on detach, revocation, or close.

use super::{
    BootstrapLimits, BootstrapProfile, CancellationToken, ClientCapabilities, ClientId,
    ClientPlumbing, Compression, ConsumerDetachRequest, HookEvent, QuicStreamEvent, ServerFeature,
    ServerFeatureSet, ServerState, SharedState, WireResourceId, debug, handle_stream_event,
    oneshot, trace, warn,
};

pub(super) async fn dispatch_stream_event(
    event: Option<QuicStreamEvent>,
    stream_events: &mut Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>>,
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    negotiated: Option<&NegotiatedConnection>,
    token: &CancellationToken,
) {
    let Some(event) = event else {
        // Disable the select arm permanently. Polling a closed receiver is
        // immediately ready and would starve control EOF.
        *stream_events = None;
        return;
    };
    handle_stream_event(state, client_id, event, plumbing, negotiated, token).await;
}

pub(super) async fn wait_initial_hello(
    deadline: std::pin::Pin<&mut tokio::time::Sleep>,
    waiting: bool,
) {
    if waiting {
        deadline.await;
    } else {
        core::future::pending::<()>().await;
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct NegotiatedConnection {
    pub(super) client_caps: ClientCapabilities,
    pub(super) profile: BootstrapProfile,
    pub(super) limits: BootstrapLimits,
    pub(super) server_features: ServerFeatureSet,
    /// The frame compression HELLO selected (`docs/spec/proto.md` §6.4).
    /// [`Compression::None`] for every consumer that offered nothing, which
    /// is every local one.
    pub(super) compression: Compression,
}

impl NegotiatedConnection {
    /// QUIC multi-stream: Terminal content rides per-Terminal streams, and
    /// subscriptions start at `STREAM_BIND`.
    pub(super) const fn quic_streams(self) -> bool {
        self.server_features.contains(ServerFeature::QuicStreams)
    }

    pub(super) const fn accepts_terminal_reply(self) -> bool {
        self.server_features.contains(ServerFeature::TerminalReply)
    }
}

pub(super) const fn runtime_server_features() -> ServerFeatureSet {
    // QUIC_STREAMS is transport-gated at HELLO (see negotiate_hello); every
    // other known bit is advertised on every connection.
    ServerFeatureSet::all().without(ServerFeature::QuicStreams)
}

/// Attachment teardown: free the per-consumer state every subscribed pane
/// holds for this client (ADR-0018), release its leases and relay state,
/// and detach it. The connection itself stays open; HELLO-negotiated state
/// is left to [`release_connection_state`]. A dropped `consumer_detach`
/// is self-healing: the actor reaps the entry once the mailbox closes.
pub(crate) fn detach_and_release_consumer_state(state: &SharedState, client_id: ClientId) {
    // Captured before teardown, for the `client-detached` hook.
    let attached_session = state.with(|s| attached_session_name(s, client_id));
    state.with(|s| release_actor_consumers(s, client_id));
    // The `Released` transitions name this client as their actor.
    state.with(|s| {
        announce_lease_releases(s, client_id, Some(crate::runtime::wire_client(client_id)));
    });
    state.with(|s| release_relay_state(s, client_id));
    state.with_mut(|s| s.detach(client_id));
    fire_client_detached(state, client_id, attached_session);
}

/// `workload-auth.md` §7 step 2: [`detach_and_release_consumer_state`] in
/// the caller's lock, so nothing outlives the revocation. Returns the
/// attached session for [`fire_client_detached`], run off-lock.
pub(crate) fn release_revoked_consumer_state(
    s: &mut ServerState,
    client_id: ClientId,
) -> Option<DetachedFrom> {
    let attached_session = attached_session_name(s, client_id);
    // A revoked connection's held actions are withdrawn in the same
    // critical section: nothing it held can be approved (ADR-0128).
    let _ = s.withdraw_approvals(client_id);
    release_actor_consumers(s, client_id);
    announce_lease_releases(s, client_id, None);
    release_relay_state(s, client_id);
    s.detach(client_id);
    attached_session
}

/// Fire the `client-detached` hook for a client that was attached.
pub(crate) fn fire_client_detached(
    state: &SharedState,
    client_id: ClientId,
    detached_from: Option<DetachedFrom>,
) {
    if let Some(from) = detached_from {
        crate::hooks::fire_hook(
            state,
            HookEvent::client_detached(client_id, from.session_name.as_deref()),
        );
    }
}

/// The session an attached client was attached to, captured before its
/// teardown for the `client-detached` hook.
pub(crate) struct DetachedFrom {
    /// The session's name; `None` once the session itself is gone.
    pub(super) session_name: Option<String>,
}

/// Where the client is attached, or `None` when it never attached.
pub(super) fn attached_session_name(s: &ServerState, client_id: ClientId) -> Option<DetachedFrom> {
    s.attached().get(&client_id).map(|client| DetachedFrom {
        session_name: s
            .registry()
            .session(client.session)
            .map(|session| session.name.clone()),
    })
}

/// Free the per-consumer state-sync entries every pane this client
/// subscribes to allocated for it. `try_send` is non-blocking and
/// best-effort, so this is safe under the state lock.
pub(super) fn release_actor_consumers(s: &ServerState, client_id: ClientId) {
    let wire_client_id = crate::runtime::wire_client(client_id);
    for handle in s.subscribed_resource_handles(client_id) {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Ok(terminal) = handle.terminal() {
            let _ = terminal
                .native_release
                .try_send(crate::terminal_actor::NativeReleaseRequest { owner: client_id.0 });
        }
        let (reply_tx, _reply_rx) = oneshot::channel();
        match handle.consumer_detach.try_send(ConsumerDetachRequest {
            client_id: wire_client_id,
            reply: reply_tx,
        }) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                trace!(
                    ?client_id,
                    "consumer_detach mailbox full; entry reaped by tick_emit when its mailbox closes",
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                trace!(
                    ?client_id,
                    "consumer_detach: pane actor gone; nothing to free"
                );
            }
        }
    }
}

/// Announce the `Released` transition for every input lease this client
/// holds (ADR-0033). `actor` is `None` when the server took it back.
/// Event-only: `detach` clears the lease state regardless.
pub(super) fn announce_lease_releases(
    s: &ServerState,
    client_id: ClientId,
    actor: Option<phux_protocol::ids::ClientId>,
) {
    for pane in s.leases_held_by(client_id) {
        let Some(handle) = s.resource_handle(pane) else {
            continue;
        };
        let _ = handle
            .control
            .try_send(crate::terminal_actor::ControlRequest::LeaseChanged {
                input_holder: None,
                action: phux_protocol::wire::frame::ControlAction::Released,
                actor,
            });
    }
}

/// Drop this client's federation relay state. Empty (no-op) on a non-hub
/// server.
pub(super) fn release_relay_state(s: &ServerState, client_id: ClientId) {
    // Rides an unbounded channel, so a saturated relay cannot keep a stale
    // subscriber alive.
    for relay in s.hub_relays_all() {
        relay.unsubscribe_client(client_id);
    }
    // Relay a RELEASE_INPUT per satellite lease this client held.
    for (host, terminal) in s.satellite_leases_held_by(client_id) {
        if let Some(relay) = s.hub_relay(&host) {
            relay.command_detached(phux_protocol::wire::frame::Command::ReleaseInput {
                terminal_id: WireResourceId::local(terminal),
            });
        }
    }
}

/// Transport-close teardown: [`detach_and_release_consumer_state`] plus the
/// connection-scoped state HELLO negotiated. Only the accept loop calls it:
/// a `DETACH` keeps the connection, its layers, and its peer identity.
pub(crate) fn release_connection_state(state: &SharedState, client_id: ClientId) {
    // A closing connection withdraws the actions it holds (ADR-0128).
    crate::runtime::approvals::withdraw(state, client_id);
    detach_and_release_consumer_state(state, client_id);
    state.with_mut(|s| s.forget_connection(client_id));
}

/// A listener's live-connection cap ([`Incoming::max_connections`]), counted
/// on the accept loop's thread.
pub(super) struct ConnectionCap {
    pub(super) cap: Option<usize>,
    pub(super) live: std::rc::Rc<std::cell::Cell<usize>>,
    pub(super) refusals: crate::transport::RefusalWarnings,
}

/// One connection's place under a [`ConnectionCap`], returned when its task
/// ends.
pub(super) struct LiveConnection(std::rc::Rc<std::cell::Cell<usize>>);

impl Drop for LiveConnection {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

impl ConnectionCap {
    pub(super) fn new(cap: Option<usize>) -> Self {
        Self {
            cap,
            live: std::rc::Rc::default(),
            refusals: crate::transport::RefusalWarnings::new(),
        }
    }

    /// A place for one more connection, or `None` (logged, at a bounded
    /// rate) when the listener is at its cap.
    pub(super) fn admit(&self, transport: &'static str) -> Option<LiveConnection> {
        if let Some(cap) = self.cap
            && self.live.get() >= cap
        {
            if let Some(suppressed) = self.refusals.due() {
                warn!(
                    transport,
                    cap, suppressed, "connection refused: listener at its connection cap"
                );
            } else {
                debug!(
                    transport,
                    cap, "connection refused: listener at its connection cap"
                );
            }
            return None;
        }
        self.live.set(self.live.get() + 1);
        Some(LiveConnection(std::rc::Rc::clone(&self.live)))
    }
}
