use super::dispatch::handle_client;
use crate::hooks::HookEvent;
use crate::runtime::ServerError;
use crate::runtime::input_lane::InputLaneHandle;
use crate::state::{ClientId, Outbound, ServerState, SharedState};
use crate::terminal_actor::ConsumerDetachRequest;
use crate::transport::{AcceptErrorDisposition, Incoming, WS_REJECTION_WARN_INTERVAL};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ids::ResourceId as WireResourceId;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

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

/// `docs/remote.md#managing-credentials`: [`detach_and_release_consumer_state`] in
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
    session_name: Option<String>,
}

/// Where the client is attached, or `None` when it never attached.
fn attached_session_name(s: &ServerState, client_id: ClientId) -> Option<DetachedFrom> {
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
fn release_actor_consumers(s: &ServerState, client_id: ClientId) {
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
fn announce_lease_releases(
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
fn release_relay_state(s: &ServerState, client_id: ClientId) {
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
fn release_connection_state(state: &SharedState, client_id: ClientId) {
    // A closing connection withdraws the actions it holds (ADR-0128).
    crate::runtime::approvals::withdraw(state, client_id);
    detach_and_release_consumer_state(state, client_id);
    state.with_mut(|s| s.forget_connection(client_id));
}

/// A listener's live-connection cap ([`Incoming::max_connections`]), counted
/// on the accept loop's thread.
struct ConnectionCap {
    cap: Option<usize>,
    live: std::rc::Rc<std::cell::Cell<usize>>,
    refusals: crate::transport::RefusalWarnings,
}

/// One connection's place under a [`ConnectionCap`], returned when its task
/// ends.
struct LiveConnection(std::rc::Rc<std::cell::Cell<usize>>);

impl Drop for LiveConnection {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

impl ConnectionCap {
    fn new(cap: Option<usize>) -> Self {
        Self {
            cap,
            live: std::rc::Rc::default(),
            refusals: crate::transport::RefusalWarnings::new(),
        }
    }

    /// A place for one more connection, or `None` (logged, at a bounded
    /// rate) when the listener is at its cap.
    fn admit(&self, transport: &'static str) -> Option<LiveConnection> {
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

/// Accept connections and spawn a local client task for each (ADR-0014).
/// Root cancellation stops admission, then waits (bounded) for every client
/// task to flush its shutdown `DETACHED`.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: the server runs on a LocalSet; per-connection transports (L::Reader/Writer) are !Send by design"
)]
pub(crate) async fn accept_loop<L: Incoming>(
    listener: &L,
    state: SharedState,
    root_token: CancellationToken,
    // ADR-0044 input lane; `None` routes input inline.
    input_lane: Option<InputLaneHandle>,
) -> Result<(), ServerError> {
    let mut clients: JoinSet<()> = JoinSet::new();
    let cap = ConnectionCap::new(listener.max_connections());
    loop {
        tokio::select! {
            () = root_token.cancelled() => {
                info!("root cancellation token fired; draining client tasks");
                if tokio::time::timeout(CLIENT_SHUTDOWN_DRAIN_TIMEOUT, async {
                    while clients.join_next().await.is_some() {}
                })
                .await
                .is_err()
                {
                    warn!("client shutdown drain timed out; aborting remaining tasks");
                    clients.shutdown().await;
                }
                return Ok(());
            }
            // Reap finished connections, or the set keeps one entry per
            // connection for the server's lifetime.
            Some(_) = clients.join_next(), if !clients.is_empty() => {}
            accept = listener.accept() => {
                match accept {
                    Ok((reader, writer, connection_identity)) => {
                        // Over the cap: dropping the transport closes it.
                        let Some(live) = cap.admit(listener.kind()) else {
                            continue;
                        };
                        debug!(transport = listener.kind(), "client connected");
                        // Counted before the task exists so the idle watchdog
                        // never sees an unattended gap.
                        state.with_mut(ServerState::note_connection_opened);
                        let client_id = state.with_mut(ServerState::new_client_id);
                        state.with_mut(|s| s.set_connection_identity(client_id, connection_identity));
                        let task_state = state.clone();
                        let client_token = root_token.child_token();
                        let task_root_token = root_token.clone();
                        let task_input_lane = input_lane.clone();
                        let task_transport = listener.transport_type();
                        let task_supports_quic_streams = listener.supports_quic_streams();
                        clients.spawn_local(async move {
                            let _live = live;
                            if let Err(err) = handle_client(reader, writer, task_state.clone(), client_id, client_token, task_root_token.clone(), task_input_lane, task_transport, task_supports_quic_streams).await {
                                warn!(error = %err, "client task ended with error");
                            }
                            // The one site that forgets connection state.
                            release_connection_state(&task_state, client_id);
                            // R13: the last connection closing on a server
                            // that runs nothing ends it.
                            let due = task_state.with_mut(|s| {
                                s.note_connection_closed();
                                s.self_exit_due()
                            });
                            if due && !task_root_token.is_cancelled() {
                                info!("last client left a server running nothing; server self-exit");
                                task_root_token.cancel();
                            }
                        });
                    }
                    Err(err) => {
                        if listener.accept_errors_are_fatal() {
                            return Err(err.into());
                        }
                        match listener.accept_error_disposition(&err) {
                            AcceptErrorDisposition::Default => {
                                // Typically transient (EMFILE, ECONNABORTED).
                                error!(error = %err, "accept failed");
                            }
                            AcceptErrorDisposition::PeerRejected {
                                stage,
                                source_ip,
                                warn_suppressed,
                            } => {
                                // WARN is rate-limited so a peer cannot flood logs.
                                debug!(
                                    transport = listener.kind(),
                                    stage,
                                    %source_ip,
                                    "peer connection rejected"
                                );
                                if let Some(suppressed_count) = warn_suppressed {
                                    warn!(
                                        transport = listener.kind(),
                                        stage,
                                        %source_ip,
                                        suppressed_count,
                                        interval_seconds = WS_REJECTION_WARN_INTERVAL.as_secs(),
                                        "peer connection rejections observed"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The major is the compatibility line (protocol 1.0 on): any client of the
/// server's major interoperates, whatever its minor and patch, because
/// everything a minor adds is negotiated or ignorable. Only a major
/// mismatch is refused.
pub(super) const fn protocol_is_compatible(client_major: u16) -> bool {
    client_major == PROTOCOL_VERSION.major
}

pub(super) fn incompatible_protocol_message(
    client_major: u16,
    client_minor: u16,
    client_patch: u16,
) -> String {
    let client = (client_major, client_minor, client_patch);
    let server = (
        PROTOCOL_VERSION.major,
        PROTOCOL_VERSION.minor,
        PROTOCOL_VERSION.patch,
    );
    let remediation = if client < server {
        "update the phux app/client"
    } else {
        "update the phux server"
    };
    format!(
        "incompatible protocol: client offered {client_major}.{client_minor}.{client_patch}, \
         server requires {}.x; {remediation} so the protocol majors match",
        PROTOCOL_VERSION.major,
    )
}

pub(super) const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
/// Bound the server-wide wait for clients to flush their shutdown ending.
const CLIENT_SHUTDOWN_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub(super) async fn close_client_writer(
    out_tx: mpsc::Sender<Outbound>,
    writer_close: &tokio::sync::watch::Sender<bool>,
    sibling_tasks: &mut JoinSet<()>,
) {
    // The writer drains every frame ordered before this, then closes.
    let _ = writer_close.send(true);
    drop(out_tx);
    if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, async {
        while sibling_tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        sibling_tasks.abort_all();
        while sibling_tasks.join_next().await.is_some() {}
    }
}
