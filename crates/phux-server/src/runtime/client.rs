//! Submodule for runtime internals.

use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    ClientCapabilities, LayerSet, ServerCapabilities, ServerFeature, ServerFeatureSet,
};
use phux_protocol::policy::{ConsumerId as PolicyConsumerId, PeerIdentity};
use phux_protocol::wire::frame::{AgentEvent, ErrorCode, FrameKind, TERMINAL_AGENT_KEY};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::{InputLaneHandle, RoutedInput};
use super::{
    STALE_PROBE_TIMEOUT, ServerError, SpawnRequest, handle_attach, handle_command,
    handle_frame_ack, handle_spawn_terminal, handle_terminal_input, handle_terminal_resize,
    handle_viewport_resize,
};
use crate::state::{ClientId, DEFAULT_CLIENT_MAILBOX, Outbound, SharedState, TerminalInput};
use crate::terminal_actor::ConsumerDetachRequest;
use crate::transport::{FrameReader, FrameWriter, Incoming};

pub(crate) fn spawn_pane_event_drain(
    state: SharedState,
    wire_terminal_id: phux_protocol::ids::TerminalId,
    mut event_rx: tokio::sync::mpsc::Receiver<AgentEvent>,
) {
    tokio::task::spawn_local(async move {
        while let Some(event) = event_rx.recv().await {
            broadcast_event(&state, Some(&wire_terminal_id), &event);
        }
    });
}

/// Spawn the per-pane agent-state drain (ADR-0046).
///
/// The `TerminalActor` derives the state — it owns the grid and the PTY — but
/// it cannot write it: `ServerState` (and therefore the metadata store, the
/// L3 subscriber set, and the arbiter) lives out here. So the actor emits an
/// edge-filtered [`AgentDetectEvent`] and this task performs the authority
/// check and the write.
///
/// The write rides the shipped `SET_METADATA` / `METADATA_CHANGED` path for
/// `phux.agent/v1`. There is no new wire surface, no new frame, and no
/// `PROTOCOL_VERSION` bump: the detector is simply another *writer* of a key
/// the protocol already carries.
///
/// `metadata_set` suppresses a broadcast when the bytes are unchanged, which
/// — together with the detector's own edge filter — is what makes a `working`
/// agent that streams output for ten minutes cost zero writes and zero events.
pub(crate) fn spawn_agent_state_drain(
    state: SharedState,
    wire_terminal_id: phux_protocol::ids::TerminalId,
    mut rx: tokio::sync::mpsc::Receiver<crate::agent_detect::AgentDetectEvent>,
) {
    use crate::agent_detect::AgentDetectEvent;
    use phux_protocol::wire::frame::{Scope, TERMINAL_AGENT_KEY};

    tokio::task::spawn_local(async move {
        while let Some(event) = rx.recv().await {
            state.with_mut(|s| {
                let scope = Scope::Terminal(wire_terminal_id.clone());
                match event {
                    AgentDetectEvent::Retract => {
                        // Only ever delete a record we authored. A human's
                        // declaration is not ours to retract.
                        if !s.agent_records().detector_owns(&wire_terminal_id) {
                            return;
                        }
                        // ... and "we authored it" is not the same as "all of
                        // it is ours". After `phux agent set --name reviewer`
                        // the detector keeps filling `state` in, and that write
                        // re-acquires ownership — of a record whose NAME the
                        // human chose. Deleting the key on retract would take
                        // their name, session and attention with it. Withdraw
                        // only the field we own.
                        if s.agent_records().has_explicit_identity(&wire_terminal_id) {
                            let existing = s.metadata().get(&scope, TERMINAL_AGENT_KEY);
                            if let Some(bytes) =
                                crate::agent_state::withdraw_state(existing.as_deref())
                            {
                                s.metadata_set(&scope, TERMINAL_AGENT_KEY, bytes);
                                s.agent_records_mut()
                                    .note_detector_retract(&wire_terminal_id);
                                return;
                            }
                        }
                        s.metadata_delete(&scope, TERMINAL_AGENT_KEY);
                        s.agent_records_mut()
                            .note_detector_retract(&wire_terminal_id);
                    }
                    AgentDetectEvent::State(report) => {
                        // ADR-0046 §E: an explicit SET_METADATA that supplied
                        // a `state` outranks the detector entirely.
                        if s.agent_records().is_declared(&wire_terminal_id) {
                            return;
                        }
                        let existing = s.metadata().get(&scope, TERMINAL_AGENT_KEY);
                        let bytes = crate::agent_state::compose(
                            existing.as_deref(),
                            &report.kind,
                            &report.name,
                            report.state.as_str(),
                        );
                        s.metadata_set(&scope, TERMINAL_AGENT_KEY, bytes);
                        s.agent_records_mut().note_detector_write(&wire_terminal_id);
                    }
                }
            });
        }
    });
}

/// Re-arm the pane detector's edge filter after someone ELSE wrote its
/// `phux.agent/v1` record (ADR-0046 §E).
///
/// `AgentDetector::published` is a model of the detector's own emissions, so
/// an explicit `SET_METADATA` / `DELETE_METADATA` leaves it modelling a store
/// that no longer exists. The detector then derives the same tuple, its edge
/// filter suppresses it, and nothing is written — so a `DELETE` on an idle
/// agent's record does not mean "the detector resumes", it means "the pane has
/// no agent until the agent's state next changes", which for an agent waiting
/// on a human is never. Same for the identity-only `SET` that is supposed to
/// leave the detector filling `state` in.
///
/// So the store tells the detector. Resolved under the state lock, sent off
/// it, on the same actor control mailbox the ADR-0033 lease broadcasts ride. A
/// saturated or closed mailbox is benign: the actor is wedged or gone, and a
/// gone actor has no detector to re-arm. A no-op for a non-agent key, a
/// non-Terminal scope, and a Terminal with no local actor (a satellite pane's
/// record is written where its actor lives).
fn invalidate_agent_detector(
    state: &SharedState,
    scope: &phux_protocol::wire::frame::Scope,
    key: &str,
) {
    use phux_protocol::wire::frame::Scope;

    if key != TERMINAL_AGENT_KEY {
        return;
    }
    let Scope::Terminal(wire) = scope else {
        return;
    };
    let handle = state.with(|s| {
        s.terminal_from_wire(wire)
            .and_then(|pane| s.terminal_handle(pane).cloned())
    });
    if let Some(handle) = handle {
        let _ = handle
            .control
            .try_send(crate::terminal_actor::ControlRequest::AgentRecordInvalidated);
    }
}

/// Spawn the per-pane EOF watcher task (phux-it8, reshaped by phux-4r1).
///
/// Awaits the `TerminalActor`'s `exit_notify` oneshot. When the actor
/// observes PTY EOF (the child process has exited — typically the
/// shell typed `exit`), this watcher broadcasts the L1 lifecycle event
/// `FrameKind::TerminalClosed { terminal_id, exit_status }` to every
/// client subscribed to the now-dead pane, then reaps the pane's
/// server-side state.
///
/// The watcher does NOT decide whether any client should detach:
/// "no Terminals left in my attached collection ⇒ detach" is a
/// *consumer* policy (ADR-0015 L1: lifecycle events are facts, detach
/// is interpretation), now owned by the TUI's `attach::driver`
/// main loop, which folds the closed pane out of its layout and
/// detaches itself when the last pane closes. The server stops
/// sending `FrameKind::Detached` on EOF.
///
/// The watcher is `spawn_local` because `SharedState` is `Send` but
/// we want the task to live on the same `LocalSet` that owns the
/// pane actor — co-locating the lifecycle keeps the cancellation
/// story tidy (root-token cascade still applies via `JoinSet` drop
/// when the runtime exits).
///
/// No-op when `exit_notify` is `None` (the bundle's receiver was
/// already taken) or when the actor exits without ever firing EOF
/// (cancellation via the root token, for example). Errors on the
/// oneshot recv side are treated identically to "EOF observed":
/// they only happen if the sender was dropped without firing, which
/// in current code means the actor was dropped without going through
/// the EOF branch — i.e. the pane is going away too. Broadcasting
/// `TERMINAL_CLOSED` is still the right response.
pub(crate) fn spawn_terminal_exit_watcher(
    state: SharedState,
    pane: phux_core::ids::TerminalId,
    exit_notify: Option<oneshot::Receiver<Option<i32>>>,
    root_token: CancellationToken,
) {
    let Some(rx) = exit_notify else {
        return;
    };
    tokio::task::spawn_local(async move {
        // Recv error (sender dropped without firing) is treated the
        // same as a fired EOF with unknown exit status: in both cases
        // the pane is dead and every subscribed client must be told.
        let exit_status = rx.await.unwrap_or(None);
        // phux-emdv: gather the broadcast subscriber set AND reap the
        // dead pane in ONE critical section, BEFORE the awaited
        // TERMINAL_CLOSED sends. This closes the TOCTOU window that left
        // a late attacher frozen on a dead pane: previously subscribers
        // were gathered in one lock, the sends were awaited, and the reap
        // happened in a SECOND lock — a client whose ATTACH landed in the
        // gap subscribed to a pane that had already hit EOF, was never in
        // the broadcast set, and never learned the shell exited. Reaping
        // up-front removes the pane (and, if last, its session) from the
        // registry, so any ATTACH that interleaves now either subscribes
        // to the surviving panes (the dead one is gone from
        // `attach_snapshot_panes`) or gets `SessionNotFound` — never a
        // silent subscription to a doomed pane.
        //
        // `reap_terminal` clears `terminal_subscribers` for the pane
        // (via `forget_terminal_bookkeeping`) and retires its wire id, so
        // both MUST be captured in the same lock before the reap runs.
        let ReapAndNotify {
            wire_terminal_id,
            targets,
            server_empty,
            served,
        } = state.with_mut(|s| {
            let wire_terminal_id = s.intern_terminal_wire(pane);
            let targets: Vec<tokio::sync::mpsc::Sender<Outbound>> = s
                .subscribers_for_terminal(pane)
                .iter()
                .filter_map(|cid| s.attached.get(cid).map(|c| c.tx.clone()))
                .collect();
            // phux-60s: reap the dead pane, cascading to its window and
            // session when they empty. Done here (inside the same lock
            // that gathered subscribers) so no ATTACH can interleave
            // between "gather" and "reap".
            let server_empty = s.reap_terminal(pane);
            let served = s.has_served_client();
            ReapAndNotify {
                wire_terminal_id,
                targets,
                server_empty,
                served,
            }
        });

        // docs/consumers/tui.md §9 (phux-r82.1): the inner process exited —
        // the `pane-exit` hook point. Fired off-lock (the hook helper
        // re-takes the state lock briefly to clone the dispatcher handle);
        // `fire` itself is a non-blocking try_send.
        crate::hooks::fire_hook(
            &state,
            crate::hooks::HookEvent::pane_exit(&wire_terminal_id, exit_status),
        );

        // phux-4li.11 / phux-4r1: broadcast the L1 lifecycle event
        // TERMINAL_CLOSED to every client that was subscribed to the
        // dying pane at reap time. The server's job ends here — it
        // reports the fact. The detach policy ("no Terminals left in my
        // collection ⇒ detach") is the consumer's (the TUI driver folds
        // the pane out of its layout and detaches itself when the last
        // pane closes); the server no longer sends `Detached` on EOF
        // (ADR-0015 L1). The sends are awaited off-lock — `with_mut` is
        // synchronous and must not hold the state borrow across an await.
        broadcast_terminal_closed(&state, &wire_terminal_id, &targets, exit_status).await;

        // phux-60s: when the last session is gone the server has nothing
        // left to serve, so fire the root token — the tmux server-exit
        // model. Without this the server lingers forever after every
        // shell exits.
        //
        // Two guards keep this from misfiring:
        //   * `has_served_client`: a freshly auto-spawned server whose
        //     seed pane dies before anyone attaches must NOT vanish — the
        //     launching `phux` is still racing to connect and will
        //     repopulate it via `CreateIfMissing`. Only self-exit once
        //     we've actually served someone.
        //   * `!root_token.is_cancelled()`: a Ctrl-C shutdown cancels the
        //     pane actor too, routing through here; don't log a spurious
        //     "self-exit" or double-cancel during normal teardown.
        if server_empty && served && !root_token.is_cancelled() {
            info!("last session reaped after serving clients; server self-exit");
            root_token.cancel();
        }
    });
}

/// Everything the EOF watcher captures under one state lock before it
/// performs the off-lock, awaited `TERMINAL_CLOSED` fanout (phux-emdv).
///
/// Gathering the subscriber mailboxes, interning the wire id, and reaping
/// the pane in a single critical section is what closes the TOCTOU race:
/// no ATTACH can observe a "still alive in the registry but already
/// EOF'd" pane between the gather and the reap.
struct ReapAndNotify {
    /// The pane's wire id, interned before the reap retired it. Reused
    /// for both the L1 `TERMINAL_CLOSED` fanout and the `PaneClosed`
    /// agent event so they carry the id the client saw on spawn/snapshot.
    wire_terminal_id: phux_protocol::ids::TerminalId,
    /// Outbound mailboxes of every client subscribed to the pane at reap
    /// time. The L1 `TERMINAL_CLOSED` fanout targets exactly this set.
    targets: Vec<tokio::sync::mpsc::Sender<Outbound>>,
    /// `true` iff the reap emptied the last session — the server self-exit
    /// signal (phux-60s).
    server_empty: bool,
    /// Whether any client has ever attached (arms the phux-60s self-exit).
    served: bool,
}

/// Emit `TERMINAL_CLOSED { terminal_id, exit_status }` to every client
/// in `targets` (phux-4li.11, SPEC §7.2 / §10.1).
///
/// The subscriber set and `wire_terminal_id` are gathered by the caller
/// ([`spawn_terminal_exit_watcher`]) in the SAME state lock that reaps the
/// pane, so they reflect exactly the clients subscribed at reap time. This
/// function only performs the off-lock work: the awaited L1 fanout and the
/// `PaneClosed` agent-event broadcast. Both are done off-lock because
/// `with_mut` is synchronous and the borrow must not be held across an
/// await (phux-emdv).
///
/// The `wire_terminal_id` is the one the client saw on `TERMINAL_SPAWNED`
/// / `TERMINAL_SNAPSHOT`; the caller interned it before the reap retired
/// it. The send is best-effort: a client whose mailbox has closed (it
/// dropped the socket) is silently skipped — `reap_terminal` (already run
/// by the caller) handled server-side state cleanup.
pub(crate) async fn broadcast_terminal_closed(
    state: &SharedState,
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    targets: &[tokio::sync::mpsc::Sender<Outbound>],
    exit_status: Option<i32>,
) {
    if targets.is_empty() {
        debug!("TERMINAL_CLOSED: no L1-subscribed clients to notify");
    } else {
        debug!(
            count = targets.len(),
            ?exit_status,
            "TERMINAL_CLOSED: broadcasting to subscribed clients",
        );
        for tx in targets {
            let _ = tx
                .send(Outbound::Frame(FrameKind::TerminalClosed {
                    terminal_id: wire_terminal_id.clone(),
                    exit_status,
                }))
                .await;
        }
    }
    // phux-y2t: fan a `pane_closed` agent event to event-stream
    // subscribers (SPEC §7.5) regardless of L1 subscribers — a
    // `watch`-only client that never attached must still learn the pane
    // died, so this MUST run even when the L1 fanout above was empty.
    broadcast_event(
        state,
        Some(wire_terminal_id),
        &AgentEvent::PaneClosed { exit_status },
    );
}

/// Free the per-consumer state-sync entries (ADR-0018, phux-0q8) this
/// client holds across every pane it subscribes to, then remove the
/// client from `ServerState`.
///
/// Counterpart to the `consumer_attach` registration the ATTACH path
/// performs per pane. Run at every client-teardown site (explicit
/// DETACH, transport disconnect, PTY EOF) so the per-consumer
/// `RenderState` cache the actor allocated at attach is dropped rather
/// than leaked until pane teardown.
///
/// Handles are gathered under-lock (`subscribed_terminal_handles`); the
/// `consumer_detach` sends happen off-lock to avoid awaiting inside
/// `with_mut`. `try_send` is non-blocking and best-effort: a full or
/// closed mailbox just means the actor is gone or saturated. A dropped
/// detach on a *live* actor is no longer a leak — `state.detach` below
/// drops the client's outbound receiver, so the actor's `tick_emit`
/// observes the mailbox as `Closed` on its next tick and reaps the
/// orphaned per-consumer entry itself (phux-ddg, the self-healing path).
pub(crate) fn detach_and_release_consumer_state(state: &SharedState, client_id: ClientId) {
    // docs/consumers/tui.md §9 (phux-r82.1): capture whether this client
    // was actually attached (and to which session, if it still exists)
    // BEFORE tearing anything down. Runs for every connection teardown,
    // but the `client-detached` hook fires only for attached clients —
    // a connection that never attached never "detaches".
    let attached_session: Option<Option<String>> = state.with(|s| {
        s.attached.get(&client_id).map(|client| {
            s.registry
                .session(client.session)
                .map(|session| session.name.clone())
        })
    });
    state.with_mut(|s| s.remove_peer_identity(client_id));
    let wire_client_id =
        phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX));
    let handles = state.with(|s| s.subscribed_terminal_handles(client_id));
    for handle in handles {
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
    // Release any input leases this client held (ADR-0033) and broadcast the
    // `Released` transition so other clients stop showing it as the holder.
    // Gathered under-lock; the `control` sends happen off-lock. `detach`
    // (below) clears the lease state regardless, so this is purely the
    // observable-event half — a saturated/closed mailbox is benign.
    let released: Vec<crate::terminal_actor::TerminalHandle> = state.with(|s| {
        s.leases_held_by(client_id)
            .into_iter()
            .filter_map(|pane| s.terminal_handle(pane).cloned())
            .collect()
    });
    for handle in released {
        let _ = handle
            .control
            .try_send(crate::terminal_actor::ControlRequest::LeaseChanged {
                input_holder: None,
                action: phux_protocol::wire::frame::ControlAction::Released,
                actor: wire_client_id,
            });
    }
    // Federation relay (phux-v45.4): drop every hub-side proxy
    // subscription this client holds on any satellite link — the
    // counterpart to the registrations the satellite-scoped
    // SUBSCRIBE_EVENTS / SUBSCRIBE_TERMINAL_EVENTS / ATTACH_TERMINAL
    // paths performed. Empty (no-op) on a non-hub server. Undroppable
    // (phux-v45.11 finding 1): rides the unbounded unsubscribe channel,
    // so a saturated relay mailbox can never leave a stale subscriber
    // that outlives its consumer.
    for relay in state.with(crate::state::ServerState::hub_relays_all) {
        relay.unsubscribe_client(client_id);
    }
    // Release any hub-side satellite input leases this client held
    // (phux-v45.7, the federation mirror of the ADR-0033 release above):
    // relay a detached RELEASE_INPUT per lease so the satellite-side
    // lease (held by the link identity) follows the hub-side ledger,
    // which `detach` below clears regardless.
    for (host, terminal) in state.with(|s| s.satellite_leases_held_by(client_id)) {
        if let Some(relay) = state.with(|s| s.hub_relay(&host)) {
            relay.command_detached(phux_protocol::wire::frame::Command::ReleaseInput {
                terminal_id: phux_protocol::ids::TerminalId::local(terminal),
            });
        }
    }
    state.with_mut(|s| s.detach(client_id));
    // docs/consumers/tui.md §9 (phux-r82.1): the client is fully detached —
    // the `client-detached` hook point (any reason: explicit DETACH,
    // transport drop, EOF). Skipped for connections that never attached.
    if let Some(session_name) = attached_session {
        crate::hooks::fire_hook(
            state,
            crate::hooks::HookEvent::client_detached(client_id, session_name.as_deref()),
        );
    }
}

/// Prepare the parent directory of `socket_path` with mode `0o700`.
pub(crate) fn prepare_socket_dir(socket_path: &Path) -> Result<(), ServerError> {
    let Some(parent) = socket_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(parent)
        .map_err(|source| ServerError::PrepareDir {
            path: parent.to_path_buf(),
            source,
        })
}

/// Handle the case where `socket_path` already exists. If something accepts a
/// connection on it within the probe timeout, treat it as live and refuse to
/// start. Otherwise unlink the stale entry so `bind` can succeed.
pub(crate) async fn handle_existing_socket(socket_path: &Path) -> Result<(), ServerError> {
    let metadata = match std::fs::symlink_metadata(socket_path) {
        Ok(m) => m,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(ServerError::Io(err)),
    };
    // Anything sitting in the way — socket, file, symlink — gets probed and
    // either rejected or removed.
    let connect = tokio::time::timeout(STALE_PROBE_TIMEOUT, UnixStream::connect(socket_path)).await;
    if let Ok(Ok(_stream)) = connect {
        return Err(ServerError::SocketBusy(socket_path.to_path_buf()));
    }
    debug!(
        path = %socket_path.display(),
        file_type = ?metadata.file_type(),
        "removing stale socket entry",
    );
    std::fs::remove_file(socket_path).map_err(ServerError::Io)?;
    Ok(())
}

/// Core accept loop. Pulled out to keep `run_async` flat.
///
/// Per ADR-0014, every per-client task spawns via
/// [`tokio::task::JoinSet::spawn_local`]; the futures we hand it are
/// `!Send` because they call into pane actors that own `!Send`
/// `Terminal`s.
///
/// `root_token` is the per-server root cancellation token. Cancellation
/// drives a clean return from this loop (the `JoinSet` of per-client
/// tasks then drops, aborting any in-flight client tasks).
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: the server runs on a LocalSet; per-connection transports (L::Reader/Writer) are !Send by design"
)]
pub(crate) async fn accept_loop<L: Incoming>(
    listener: &L,
    state: SharedState,
    root_token: CancellationToken,
    // Dedicated input lane (phux-51n6.2, ADR-0044). `Some` in production so
    // each client task routes `INPUT_*` off the main runtime; `None` in the
    // direct-drive tests that never spawn the lane, which fall back to inline
    // routing (identical behavior, on-thread).
    input_lane: Option<InputLaneHandle>,
) -> Result<(), ServerError> {
    // JoinSet of per-client tasks. Dropping this set on loop exit
    // aborts every still-running client task in one step — much
    // shorter than waiting for each task's own `select!` to observe
    // its child token's cancellation.
    let mut clients: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            () = root_token.cancelled() => {
                info!("root cancellation token fired; accept loop exiting");
                return Ok(());
            }
            accept = listener.accept() => {
                match accept {
                    Ok((reader, writer, peer_identity)) => {
                        debug!(transport = listener.kind(), "client connected");
                        // Allocate the per-client routing id up-front so the
                        // task can detach itself cleanly on EOF.
                        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                        state.with_mut(|s| s.set_peer_identity(client_id, peer_identity));
                        let task_state = state.clone();
                        let client_token = root_token.child_token();
                        let task_root_token = root_token.clone();
                        let task_input_lane = input_lane.clone();
                        clients.spawn_local(async move {
                            if let Err(err) = handle_client(reader, writer, task_state.clone(), client_id, client_token, task_root_token, task_input_lane).await {
                                warn!(error = %err, "client task ended with error");
                            }
                            // Implicit detach on EOF / error path — matches
                            // the explicit `DETACH` semantics for the wire
                            // path that will land alongside the protocol
                            // variants.
                            detach_and_release_consumer_state(&task_state, client_id);
                        });
                    }
                    Err(err) => {
                        // Accept errors are typically transient (EMFILE,
                        // ECONNABORTED). Log and continue rather than killing
                        // the server.
                        error!(error = %err, "accept failed");
                    }
                }
            }
        }
    }
}

/// Per-client task. Reads frames in a loop and dispatches each one.
///
/// Outbound messages are routed through a per-client `mpsc` channel
/// drained by a sibling writer task (also `spawn_local`'d). This gives
/// us one place to back-pressure on slow clients without entangling
/// the read side, and matches the `tx: mpsc::Sender<Outbound>` shape
/// `ServerState::attach` already wants. The channel carries
/// [`Outbound`] so every typed [`FrameKind`] send shares one ordering
/// domain.
///
/// `phux-byc.8`: implements the ATTACH path. Resolves the target,
/// builds a [`SessionSnapshot`](phux_protocol::wire::info::SessionSnapshot)
/// from the registry, requests a snapshot from each pane's
/// [`TerminalActor`](crate::terminal_actor::TerminalActor), and emits
/// `ATTACHED` + `TERMINAL_SNAPSHOT` frames per SPEC §13. On unknown
/// session, emits an `ERROR` frame with `SessionNotFound` (SPEC §14).
#[allow(
    clippy::too_many_lines,
    reason = "single per-client dispatch loop; each frame arm is small and the catalog grows linearly. Extracting arms hides the wire→state seam without simplifying it."
)]
#[allow(
    clippy::cognitive_complexity,
    reason = "see `clippy::too_many_lines` rationale above: the dispatch shape is one match arm per wire frame variant, where each arm is small and self-contained. Splitting on the arm boundary fragments the wire→state seam; merging arms across variants is what generated the complexity score in the first place."
)]
pub(crate) async fn handle_client<R, W>(
    mut reader: R,
    writer: W,
    state: SharedState,
    client_id: ClientId,
    token: CancellationToken,
    root_token: CancellationToken,
    input_lane: Option<InputLaneHandle>,
) -> io::Result<()>
where
    R: FrameReader + 'static,
    W: FrameWriter + 'static,
{
    debug!(?client_id, "client task started");

    // Allocate the per-client outbound mailbox + spawn the writer task.
    // The writer drains one `Outbound` channel; closure of this one
    // channel is the unambiguous signal for the writer to exit.
    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    // Per-client `JoinSet` for sibling tasks (today: just the writer).
    // Held in this scope so it drops with `handle_client` and the
    // writer aborts if it hasn't already exited via its own
    // close-on-EOF path. Keeps lifecycle plumbing local.
    let mut sibling_tasks: JoinSet<()> = JoinSet::new();
    sibling_tasks.spawn_local(writer_task(writer, out_rx, client_id));

    // Per-attach raw-output pumps. These are deliberately separate from
    // `sibling_tasks`: DETACH/session switch must abort pane output pumps
    // without killing the writer, because the writer still needs to emit
    // DETACHED and may serve a later ATTACH on the same connection.
    let mut output_pumps: JoinSet<()> = JoinSet::new();

    // Per-connection cache of the most-recently-advertised
    // [`ClientCapabilities`] (SPEC §6.2). HELLO populates this; ATTACH
    // consumes it when constructing the `AttachedClient`. Pre-HELLO it
    // defaults to [`ClientCapabilities::default`] (most-permissive) so a
    // client that skips HELLO (out of spec, but tolerated for
    // forward-compat) still attaches with sensible bytes-on-wire behavior.
    let mut negotiated_client_caps = ClientCapabilities::default();

    loop {
        // Pull the next complete frame from the transport — length-prefixed on
        // UDS, one binary message on WebSocket (see `transport.rs`). EOF ends
        // the session cleanly; cancellation preempts a slow read via the biased
        // select so a server-wide shutdown isn't blocked behind it.
        let framed = tokio::select! {
            biased;
            () = token.cancelled() => {
                debug!(?client_id, "client task cancelled by root token");
                return Ok(());
            }
            res = reader.read_frame() => match res {
                Ok(Some(framed)) => framed,
                Ok(None) => {
                    debug!("client disconnected (eof)");
                    return Ok(());
                }
                Err(err) => {
                    debug!(error = %err, "client read error; closing");
                    return Ok(());
                }
            },
        };

        let frame = match FrameKind::decode(&framed) {
            Ok((frame, _rest)) => frame,
            Err(err) => {
                warn!(error = ?err, "client sent undecodable frame; closing");
                return Ok(());
            }
        };

        match frame {
            FrameKind::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => {
                debug!(
                    ?client_id,
                    %client_name,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    color_support = ?client_caps.color_support,
                    "HELLO",
                );
                // Policy check: authorize HELLO before proceeding.
                let policy_ok = {
                    let peer = state
                        .with(|s| s.peer_identity(client_id).cloned())
                        .unwrap_or(PeerIdentity {
                            uid: 0,
                            pid: None,
                            exe_path: None,
                            mcp_host_key: None,
                            transport: phux_protocol::policy::TransportType::UnixSocket,
                            source_addr: None,
                        });
                    let bundle = state.with(|s| s.policy_bundle().clone());
                    let _consumer = PolicyConsumerId(client_id.0.to_string());
                    // Build a capability list from the advertised layers.
                    let requested_caps = vec![phux_protocol::policy::Capability {
                        layer: phux_protocol::caps::Layer::L1,
                        ops: vec![],
                        terminals: None,
                        groups: None,
                        expires_at: None,
                    }];
                    match bundle.engine.authorize_hello(&peer, requested_caps).await {
                        Ok(_granted) => true,
                        Err(err) => {
                            warn!(?client_id, error = %err, "HELLO denied by policy");
                            let _ = out_tx
                                .send(Outbound::Frame(FrameKind::Error {
                                    request_id: None,
                                    code: ErrorCode::PermissionDenied,
                                    message: format!("policy denied: {err}"),
                                }))
                                .await;
                            false
                        }
                    }
                };
                if !policy_ok {
                    return Ok(());
                }
                // SPEC §6.1: HELLO arrives before ATTACH. Cache the
                // advertised tier on the per-task stack; the ATTACH
                // branch consumes it when building the `AttachedClient`.
                // If a client (mis-)sends HELLO post-ATTACH we also
                // patch the live `AttachedClient` so downsample picks
                // up the change — the alternative (protocol error
                // close) gives the operator nothing to debug.
                negotiated_client_caps = client_caps;
                state.with_mut(|s| {
                    s.set_client_capabilities(client_id, client_caps);
                    // SPEC §6.2: cache the negotiated layer set. The L3
                    // dispatch arms (METADATA_*) gate emission of
                    // `METADATA_CHANGED` on `client_speaks_l3` so non-L3
                    // consumers never see L3 frames (SPEC §16.4).
                    s.set_client_layers(client_id, client_caps.layers);
                });
                // SPEC §6.1: server replies with HELLO_OK before ATTACH
                // is processed on this connection. The single-version
                // reference server echoes its own PROTOCOL_VERSION as the
                // selected version (no `VERSION_INCOMPATIBLE` negotiation
                // yet) and advertises the full tier set it mounts (L1+L2+L3);
                // the negotiated set is the intersection with the client's
                // `layers`. `server_id` is the opaque process identity.
                let hello_ok = FrameKind::HelloOk {
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new()
                        .with_layers(LayerSet::all())
                        .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
                    server_id: state.with(|server| server.server_incarnation().as_bytes().to_vec()),
                };
                if out_tx.send(Outbound::Frame(hello_ok)).await.is_err() {
                    trace!(?client_id, "HELLO_OK send dropped: writer gone");
                }
            }
            FrameKind::Ping { nonce } => {
                // SPEC §7.4: echo nonce in PONG.
                debug!(nonce, "PING -> PONG");
                if out_tx
                    .send(Outbound::Frame(FrameKind::Pong { nonce }))
                    .await
                    .is_err()
                {
                    trace!(?client_id, nonce, "PONG send dropped: writer gone");
                }
            }
            FrameKind::Attach {
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
            } => {
                handle_attach(
                    &state,
                    client_id,
                    target,
                    viewport,
                    request_scrollback,
                    scrollback_limit_lines,
                    &out_tx,
                    negotiated_client_caps,
                    &root_token,
                    &mut output_pumps,
                )
                .await;
            }
            FrameKind::Detach => {
                // Lifecycle event at info so it shows under the default
                // capture filter — DETACH is a per-client lifecycle edge a
                // trace reader wants to see without enabling debug.
                info!(?client_id, "DETACH");
                // SPEC §7.3: server responds with DETACHED, then closes.
                // For byc.8 we emit DETACHED and let the read loop
                // continue — actual transport close lands when the
                // client drops, which is the path the existing
                // socket-lifecycle tests exercise.
                // Intentionally silent on send failure: we are about
                // to `detach()` this client on the next line, so the
                // writer being gone is the next thing to happen
                // anyway. Logging here would be pure noise.
                abort_output_pumps(&mut output_pumps, client_id, "DETACH").await;
                let _ = out_tx.send(Outbound::Frame(FrameKind::Detached)).await;
                detach_and_release_consumer_state(&state, client_id);
            }
            FrameKind::ViewportResize { viewport } => {
                debug!(
                    ?client_id,
                    cols = viewport.cols,
                    rows = viewport.rows,
                    "VIEWPORT_RESIZE"
                );
                handle_viewport_resize(&state, client_id, &viewport);
            }
            FrameKind::InputKey { terminal_id, event } => {
                route_client_input(
                    &state,
                    input_lane.as_ref(),
                    client_id,
                    terminal_id,
                    TerminalInput::Key(event),
                    "INPUT_KEY",
                );
            }
            FrameKind::InputMouse { terminal_id, event } => {
                route_client_input(
                    &state,
                    input_lane.as_ref(),
                    client_id,
                    terminal_id,
                    TerminalInput::Mouse(event),
                    "INPUT_MOUSE",
                );
            }
            FrameKind::InputFocus { terminal_id, event } => {
                route_client_input(
                    &state,
                    input_lane.as_ref(),
                    client_id,
                    terminal_id,
                    TerminalInput::Focus(event),
                    "INPUT_FOCUS",
                );
            }
            FrameKind::InputPaste { terminal_id, event } => {
                // Same dispatch as the sibling INPUT_* frames; the terminal
                // actor's per-pane paste encoder applies the trust policy and
                // DEC 2004 bracketing (SPEC §9.4). Until this arm existed the
                // frame fell into the unhandled-type debug arm and pastes
                // from projection clients silently vanished.
                route_client_input(
                    &state,
                    input_lane.as_ref(),
                    client_id,
                    terminal_id,
                    TerminalInput::Paste(event),
                    "INPUT_PASTE",
                );
            }
            FrameKind::FrameAck { terminal_id, seq } => {
                handle_frame_ack(&state, client_id, &terminal_id, seq);
            }
            FrameKind::GetMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_get_metadata(&state, client_id, request_id, &scope, &key, &out_tx).await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                handle_set_metadata(
                    &state,
                    client_id,
                    request_id,
                    &scope,
                    &key,
                    value,
                    &root_token,
                );
            }
            FrameKind::DeleteMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_delete_metadata(&state, client_id, request_id, &scope, &key);
            }
            FrameKind::ListMetadata { request_id, scope } => {
                handle_list_metadata(&state, client_id, request_id, &scope, &out_tx).await;
            }
            FrameKind::SubscribeMetadata { scope, key } => {
                handle_subscribe_metadata(&state, client_id, scope, key);
            }
            FrameKind::SubscribeEvents { terminal } => {
                handle_subscribe_events(&state, client_id, terminal, &out_tx);
            }
            FrameKind::SpawnTerminal {
                request_id,
                group,
                command,
                cwd,
                env,
                term,
                satellite,
                owner_terminal,
            } => {
                handle_spawn_terminal(
                    &state,
                    client_id,
                    request_id,
                    SpawnRequest {
                        group,
                        command,
                        cwd,
                        env,
                        term,
                        satellite,
                        owner_terminal,
                    },
                    &out_tx,
                    &root_token,
                )
                .await;
            }
            FrameKind::TerminalResize {
                terminal_id,
                cols,
                rows,
            } => {
                handle_terminal_resize(&state, client_id, &terminal_id, cols, rows);
            }
            FrameKind::Command {
                request_id,
                command,
            } => {
                handle_command(
                    &state,
                    client_id,
                    request_id,
                    command,
                    &out_tx,
                    input_lane.as_ref(),
                )
                .await;
            }
            other => {
                debug!(kind = ?other, "unhandled message type (INPUT_* / etc.)");
            }
        }
    }
}

/// Route one decoded `INPUT_*` event, preferring the dedicated input lane
/// (phux-51n6.2, ADR-0044).
///
/// A **local** pane id with a live lane is handed to the lane thread, which
/// runs lease/subscription gating, snapshot-driven encode, and bounded
/// encoded-byte delivery off the main runtime. Everything else falls back to the inline
/// [`handle_terminal_input`]: satellite-tagged ids (their delivery is a
/// hub-link relay, not a mailbox `try_send`, so it stays on the main thread)
/// and the no-lane path used by direct-drive tests. Both share the same
/// destination-resolution gates, so lease and subscription semantics match.
fn route_client_input(
    state: &SharedState,
    input_lane: Option<&InputLaneHandle>,
    client_id: ClientId,
    terminal_id: phux_protocol::ids::TerminalId,
    input: TerminalInput,
    frame_label: &'static str,
) {
    if let Some(lane) = input_lane
        && terminal_id.is_local()
    {
        lane.route(RoutedInput::attached(
            client_id,
            terminal_id,
            input,
            frame_label,
        ));
        return;
    }
    handle_terminal_input(state, client_id, &terminal_id, input, frame_label);
}

pub(crate) async fn abort_output_pumps(
    output_pumps: &mut JoinSet<()>,
    client_id: ClientId,
    reason: &'static str,
) {
    if output_pumps.is_empty() {
        return;
    }
    debug!(
        ?client_id,
        pump_count = output_pumps.len(),
        reason,
        "aborting per-attach output pumps",
    );
    output_pumps.abort_all();
    while output_pumps.join_next().await.is_some() {}
}

// -----------------------------------------------------------------------------
// L3 metadata dispatch — SPEC §7.4 / §11.L3 (phux-4li.2 / phux-4li.8).
//
// GET / LIST replies ride dedicated `METADATA_VALUE` / `METADATA_KEYS`
// S→C frames (allocated by phux-4li.8) correlated to the originating
// request by `request_id`. Reply emission, like `METADATA_CHANGED`
// fan-out, is gated on `client_speaks_l3` (SPEC §16.4): a non-L3
// consumer that nevertheless ships an L3 request gets silence.
// -----------------------------------------------------------------------------

pub(crate) async fn handle_get_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &phux_protocol::wire::frame::Scope,
    key: &str,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let (value, speaks_l3) =
        state.with(|s| (s.metadata().get(scope, key), s.client_speaks_l3(client_id)));
    debug!(
        ?client_id,
        request_id,
        ?scope,
        %key,
        present = value.is_some(),
        speaks_l3,
        "GET_METADATA",
    );
    if !speaks_l3 {
        // SPEC §16.4: out-of-tier traffic from a non-L3 consumer is
        // dropped silently, matching the SUBSCRIBE_METADATA arm above.
        // A future ticket may switch to ERROR { OUT_OF_TIER } once the
        // error code lands.
        return;
    }
    if out_tx
        .send(Outbound::Frame(FrameKind::MetadataValue {
            request_id,
            value,
        }))
        .await
        .is_err()
    {
        trace!(
            ?client_id,
            request_id, "METADATA_VALUE send dropped: writer gone"
        );
    }
}

/// Parse the JSON body of a `SESSION_CREATE_KEY` write into
/// `(name, command, cwd)`. Returns `None` if the bytes are not valid JSON,
/// the top level is not an object, or `name` is missing/non-string. `command`
/// (an array of strings) and `cwd` (a string) are optional; a malformed
/// optional field is treated as absent rather than failing the whole parse.
fn parse_session_create_request(
    value: &[u8],
) -> Option<(String, Option<Vec<String>>, Option<String>)> {
    let v: serde_json::Value = serde_json::from_slice(value).ok()?;
    let obj = v.as_object()?;
    let name = obj.get("name")?.as_str()?.to_owned();
    let command = obj.get("command").and_then(|c| c.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|e| e.as_str().map(str::to_owned))
            .collect::<Vec<String>>()
    });
    let cwd = obj.get("cwd").and_then(|c| c.as_str()).map(str::to_owned);
    Some((name, command, cwd))
}

pub(crate) fn handle_set_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &phux_protocol::wire::frame::Scope,
    key: &str,
    value: Vec<u8>,
    root_token: &tokio_util::sync::CancellationToken,
) {
    use phux_protocol::wire::frame::{SESSION_CREATE_KEY, SESSION_CREATE_RESULT_KEY, Scope};
    debug!(?client_id, request_id, ?scope, %key, "SET_METADATA");
    // v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027): a create-without-
    // attach is a `SET_METADATA` write of the conventional
    // `SESSION_CREATE_KEY` under `Scope::Global`, replacing the removed
    // `CREATE_SESSION` verb. The value is a UTF-8 JSON object
    // `{ name, command?, cwd? }`. The server seeds the session + pane; the
    // caller reads the seed-pane id back via `GET_STATE` (SET_METADATA has
    // no reply frame). A malformed value or a duplicate name is a silent
    // no-op (logged), matching the fire-and-forget shape of metadata writes.
    if key == SESSION_CREATE_KEY && matches!(scope, Scope::Global) {
        match parse_session_create_request(&value) {
            Some((name, command, cwd)) => {
                let outcome = crate::runtime::commands::create_named_session(
                    state,
                    &name,
                    command,
                    cwd.as_deref(),
                    root_token,
                );
                // Publish the result under the conventional result key so the
                // caller can read the seed-pane id back (SET_METADATA has no
                // reply frame). On failure we leave the result key untouched;
                // the client surfaces "did not register" from its snapshot.
                if let Ok(wire) = &outcome {
                    let payload = serde_json::json!({
                        "name": name,
                        "terminal_id": wire.local_id(),
                    });
                    if let Ok(bytes) = serde_json::to_vec(&payload) {
                        let _ = state.with_mut(|s| {
                            s.metadata_set(&Scope::Global, SESSION_CREATE_RESULT_KEY, bytes)
                        });
                    }
                }
                debug!(
                    ?client_id,
                    request_id,
                    %name,
                    ok = outcome.is_ok(),
                    "SET_METADATA(session-create): create attempted",
                );
            }
            None => {
                warn!(
                    ?client_id,
                    request_id,
                    "SET_METADATA(session-create): malformed JSON value (want {{name, command?, cwd?}}); ignoring",
                );
            }
        }
        return;
    }
    // v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027): a session rename is a
    // `SET_METADATA` write of the conventional `SESSION_NAME_KEY` under
    // `Scope::Global`, replacing the removed `RENAME_SESSION` verb. The
    // value is `current_name\0new_name` (NUL-separated UTF-8). The server is
    // authoritative for session names (they drive `ls` / `attach`), so it
    // intercepts the write and applies the registry rename rather than
    // storing it as an opaque blob. A malformed value or unknown session is
    // a silent no-op — `SET_METADATA` has no reply frame to carry an error,
    // matching the fire-and-forget shape of every other metadata write.
    if key == phux_protocol::wire::frame::SESSION_NAME_KEY && matches!(scope, Scope::Global) {
        match std::str::from_utf8(&value).ok().and_then(|s| {
            s.split_once('\0')
                .map(|(cur, new)| (cur.to_owned(), new.to_owned()))
        }) {
            Some((current, new_name)) => {
                let outcome = state.with_mut(|s| s.rename_session(&current, &new_name));
                debug!(
                    ?client_id,
                    request_id,
                    %current,
                    %new_name,
                    ?outcome,
                    "SET_METADATA(session-name): applied registry rename",
                );
            }
            None => {
                warn!(
                    ?client_id,
                    request_id,
                    "SET_METADATA(session-name): malformed value (want current\\0new); ignoring",
                );
            }
        }
        return;
    }
    // ADR-0046 §E. This is the ONLY entry point an *explicit* agent-record
    // write passes through — the detector's own drain calls `metadata_set`
    // directly — which is precisely what makes the arbiter's bookkeeping
    // honest. It cannot be reconstructed from the stored bytes: the client's
    // `AgentMetaState` decodes an absent `state` and an unrecognized one both
    // to `Unknown`, and the detector's writes carry a `state` too, so "was
    // this declared by a human?" is not a question the value can answer.
    let declared_agent_record = matches!(scope, phux_protocol::wire::frame::Scope::Terminal(_))
        && key == TERMINAL_AGENT_KEY;
    let agent_value = declared_agent_record.then(|| value.clone());

    let delivered = state.with_mut(|s| {
        if let (Some(bytes), phux_protocol::wire::frame::Scope::Terminal(terminal)) =
            (agent_value.as_deref(), scope)
        {
            s.agent_records_mut().note_explicit_set(terminal, bytes);
        }
        s.metadata_set(scope, key, value)
    });
    // The store just changed under the detector's edge filter.
    invalidate_agent_detector(state, scope, key);
    trace!(
        ?client_id,
        request_id,
        subscriber_count = delivered.len(),
        "SET_METADATA delivered"
    );
}

pub(crate) fn handle_delete_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &phux_protocol::wire::frame::Scope,
    key: &str,
) {
    debug!(?client_id, request_id, ?scope, %key, "DELETE_METADATA");
    let delivered = state.with_mut(|s| {
        // ADR-0046 §E: deleting the record withdraws any human declaration,
        // so the detector resumes ownership of this Terminal.
        if let phux_protocol::wire::frame::Scope::Terminal(terminal) = scope
            && key == TERMINAL_AGENT_KEY
        {
            s.agent_records_mut().note_explicit_delete(terminal);
        }
        s.metadata_delete(scope, key)
    });
    // ADR-0046 §E's "the detector resumes" is only true if the detector is
    // told: its edge filter still holds the state it derived before the
    // delete, and would silently suppress the republish.
    invalidate_agent_detector(state, scope, key);
    trace!(
        ?client_id,
        request_id,
        subscriber_count = delivered.len(),
        "DELETE_METADATA delivered"
    );
}

pub(crate) async fn handle_list_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &phux_protocol::wire::frame::Scope,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let (keys, speaks_l3) =
        state.with(|s| (s.metadata().list(scope), s.client_speaks_l3(client_id)));
    debug!(
        ?client_id,
        request_id,
        ?scope,
        key_count = keys.len(),
        speaks_l3,
        "LIST_METADATA",
    );
    if !speaks_l3 {
        // SPEC §16.4: same out-of-tier gating as `handle_get_metadata`.
        return;
    }
    if out_tx
        .send(Outbound::Frame(FrameKind::MetadataKeys {
            request_id,
            keys,
        }))
        .await
        .is_err()
    {
        trace!(
            ?client_id,
            request_id, "METADATA_KEYS send dropped: writer gone"
        );
    }
}

pub(crate) fn handle_subscribe_metadata(
    state: &SharedState,
    client_id: ClientId,
    scope: phux_protocol::wire::frame::Scope,
    key: String,
) {
    state.with_mut(|s| {
        if !s.client_speaks_l3(client_id) {
            // SPEC §16.4: out-of-tier traffic from a non-L3 consumer.
            // The L3 dispatch is best-effort: we drop the subscribe
            // rather than tear the connection down, on the theory that
            // a misbehaving client should learn from silence faster
            // than from a protocol error. A future ticket may swap
            // this for an explicit `ERROR { OUT_OF_TIER }` once the
            // error code lands.
            debug!(?client_id, ?scope, %key, "SUBSCRIBE_METADATA refused (non-L3)");
            return;
        }
        debug!(?client_id, ?scope, %key, "SUBSCRIBE_METADATA");
        s.metadata_subscribe(client_id, scope, key);
    });
}

/// Record an agent-event subscription for `client_id` (SPEC §7.5,
/// phux-y2t). `terminal = None` subscribes server-wide; `Some(id)`
/// subscribes per-pane. Idempotent (the per-client scope set absorbs
/// duplicates) and connection-scoped (cleared on detach). Unlike the L3
/// metadata path this is not tier-gated — the event stream is part of L1
/// and any consumer may opt in.
pub(crate) fn handle_subscribe_events(
    state: &SharedState,
    client_id: ClientId,
    terminal: Option<phux_protocol::ids::TerminalId>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    debug!(?client_id, ?terminal, "SUBSCRIBE_EVENTS");
    // Satellite-scoped subscription (phux-v45.4): register the caller as
    // a hub-side proxy subscriber on the owning link and forward the
    // SUBSCRIBE_EVENTS frame (id rewritten satellite-local) so the
    // satellite starts pushing EVENT frames back over the link; the relay
    // re-tags them `Local -> Satellite { host, .. }` on the way to this
    // consumer. SUBSCRIBE_EVENTS has no reply frame, so a missing route
    // (non-hub server / unknown host) surfaces as a typed ERROR push
    // rather than silence.
    if let Some(wire_id) = &terminal
        && let Some((host, id)) = crate::hub::relay::satellite_route(wire_id)
    {
        if let Some(relay) = state.with(|s| s.hub_relay(&host)) {
            // Atomic register-and-forward (phux-v45.11 finding 2): the
            // hub-side registration and the satellite-side SUBSCRIBE_EVENTS
            // either both happen or the consumer gets a typed error push.
            relay.subscribe(
                crate::hub::relay::ProxySubscription {
                    terminal: id,
                    client: client_id,
                    out_tx: out_tx.clone(),
                    // Stamped with the issue-order token by `subscribe`
                    // at enqueue.
                    seq: 0,
                    // An event subscription carries no snapshot; its EVENT
                    // deltas must flow immediately, so it is not gated
                    // (phux-v45.14).
                    awaits_snapshot: false,
                },
                FrameKind::SubscribeEvents {
                    terminal: Some(phux_protocol::ids::TerminalId::local(id)),
                },
            );
        } else {
            warn!(
                ?client_id,
                satellite = %host,
                "SUBSCRIBE_EVENTS: no route to satellite; refusing subscription"
            );
            let _ = out_tx.try_send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::UnsupportedSatelliteRoute,
                message: format!(
                    "no satellite route to {host:?}: this server is not a federation hub \
                     for that host"
                ),
            }));
        }
        return;
    }
    // Capture the client's mailbox in the subscription so event fanout
    // reaches it even without an ATTACH (a pure `watch` client never
    // attaches).
    state.with_mut(|s| s.subscribe_events(client_id, terminal, out_tx.clone()));
}

/// Push an [`AgentEvent`] to every client subscribed to events scoped to
/// `terminal` (SPEC §7.5, phux-y2t).
///
/// `terminal` is the wire id the event concerns, or `None` for a
/// server-scoped event with no owning Terminal. Fan-out uses
/// [`crate::state::ServerState::event_targets`], which matches server-wide
/// subscribers
/// plus (when `terminal` is `Some`) per-pane subscribers for that id.
/// Best-effort: a client whose mailbox is full or closed is silently
/// skipped — the event stream is an accelerator, never a guarantee
/// (a dropped event just means the consumer falls back to the poll floor).
///
/// Synchronous: fanout uses non-blocking `try_send`, so there is nothing
/// to await — the caller need not be in an async context to push an event.
pub(crate) fn broadcast_event(
    state: &SharedState,
    terminal: Option<&phux_protocol::ids::TerminalId>,
    event: &AgentEvent,
) {
    let targets = state.with(|s| s.event_targets(terminal));
    if targets.is_empty() {
        return;
    }
    trace!(
        ?terminal,
        ?event,
        count = targets.len(),
        "EVENT: broadcasting"
    );
    for tx in targets {
        // `try_send` is non-blocking: a full mailbox drops the event
        // rather than stalling the emitter. The accelerator contract
        // tolerates loss (the CLI poll floor still converges).
        let _ = tx.try_send(Outbound::Frame(FrameKind::Event {
            terminal: terminal.cloned(),
            event: event.clone(),
        }));
    }
}

/// Writer task: drain the per-client outbound channel and write each
/// message to the socket. Encodes [`Outbound::Frame`] via
/// `FrameKind::encode`.
///
/// Exits when the channel closes — i.e. the client task drops its
/// sender.
pub(crate) async fn writer_task<W: FrameWriter>(
    mut writer: W,
    mut rx: tokio::sync::mpsc::Receiver<Outbound>,
    client_id: ClientId,
) {
    let mut buf = BytesMut::with_capacity(1024);
    while let Some(msg) = rx.recv().await {
        let Outbound::Frame(frame) = msg;
        buf.clear();
        frame.encode(&mut buf);
        if let Err(err) = writer.write_frame(&buf).await {
            debug!(?client_id, error = %err, "writer error on frame; client task ending");
            return;
        }
    }
    debug!(?client_id, "writer task exiting (channel closed)");
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod agent_drain_tests {
    use phux_protocol::ids::TerminalId as WireTerminalId;
    use phux_protocol::wire::frame::{Scope, TERMINAL_AGENT_KEY};

    use super::spawn_agent_state_drain;
    use crate::agent_detect::record::AgentRecordJson;
    use crate::agent_detect::{AgentDetectEvent, AgentReport, DetectedState};
    use crate::state::SharedState;

    fn report(state: DetectedState) -> AgentReport {
        AgentReport {
            kind: "claude".to_owned(),
            name: "claude".to_owned(),
            state,
        }
    }

    /// Drive the real drain task to quiescence over `events`, and hand back the
    /// stored `phux.agent/v1` bytes.
    async fn drain(state: &SharedState, terminal: &WireTerminalId, events: Vec<AgentDetectEvent>) {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        spawn_agent_state_drain(state.clone(), terminal.clone(), rx);
        for event in events {
            tx.send(event).await.expect("drain is alive");
        }
        drop(tx);
        // The drain is a `spawn_local` task; yield until it has consumed the
        // channel and closed.
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn stored(state: &SharedState, terminal: &WireTerminalId) -> Option<AgentRecordJson> {
        let scope = Scope::Terminal(terminal.clone());
        state
            .with(|s| s.metadata().get(&scope, TERMINAL_AGENT_KEY))
            .and_then(|bytes| AgentRecordJson::decode(&bytes))
    }

    /// THE label-eater, end to end through the real drain.
    ///
    /// A human runs `phux agent set --name reviewer --session fleet-7`. That is
    /// identity only, so it is NOT a declaration: the detector keeps running and
    /// fills `state` in around them — and that write re-acquires `detector_owned`.
    /// When the agent exits back to the shell, the retract used to `DELETE` the
    /// whole key on the strength of that bit alone, destroying the name and the
    /// session the human chose.
    #[tokio::test(flavor = "current_thread")]
    async fn a_retract_does_not_delete_a_humans_name_from_the_record() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let terminal = WireTerminalId::new(1);
                let scope = Scope::Terminal(terminal.clone());

                // The human names the pane.
                let declared = br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#;
                state.with_mut(|s| {
                    s.agent_records_mut().note_explicit_set(&terminal, declared);
                    s.metadata_set(&scope, TERMINAL_AGENT_KEY, declared.to_vec());
                });

                // The agent works, then exits back to the shell.
                drain(
                    &state,
                    &terminal,
                    vec![
                        AgentDetectEvent::State(report(DetectedState::Working)),
                        AgentDetectEvent::Retract,
                    ],
                )
                .await;

                let record = stored(&state, &terminal).expect(
                    "the record must SURVIVE the agent's exit: the human authored its identity, \
                     and the detector only ever owned `state`",
                );
                assert_eq!(record.name, "reviewer", "the human's name survives");
                assert_eq!(
                    record.session.as_deref(),
                    Some("fleet-7"),
                    "and their label"
                );
                assert_eq!(
                    record.state, "unknown",
                    "but a dead agent must not leave a `working` badge spinning",
                );
            })
            .await;
    }

    /// The other half: a record the detector authored ENTIRELY is its to delete.
    /// Otherwise every pane that ever ran an agent keeps a tombstone record
    /// forever.
    #[tokio::test(flavor = "current_thread")]
    async fn a_retract_deletes_a_record_the_detector_wrote_alone() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let terminal = WireTerminalId::new(1);

                drain(
                    &state,
                    &terminal,
                    vec![
                        AgentDetectEvent::State(report(DetectedState::Working)),
                        AgentDetectEvent::Retract,
                    ],
                )
                .await;

                assert!(
                    stored(&state, &terminal).is_none(),
                    "a purely detector-authored record is deleted on retract",
                );
            })
            .await;
    }

    /// A human who DECLARED a state stands the detector down entirely: it makes
    /// no writes at all, retract included.
    #[tokio::test(flavor = "current_thread")]
    async fn a_retract_never_touches_a_declared_record() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let terminal = WireTerminalId::new(1);
                let scope = Scope::Terminal(terminal.clone());

                let declared = br#"{"name":"me","kind":"claude","state":"done"}"#;
                state.with_mut(|s| {
                    s.agent_records_mut().note_explicit_set(&terminal, declared);
                    s.metadata_set(&scope, TERMINAL_AGENT_KEY, declared.to_vec());
                });

                drain(
                    &state,
                    &terminal,
                    vec![
                        AgentDetectEvent::State(report(DetectedState::Working)),
                        AgentDetectEvent::Retract,
                    ],
                )
                .await;

                let record = stored(&state, &terminal).expect("the declaration stands");
                assert_eq!(record.state, "done", "the detector never wrote over it");
                assert_eq!(record.name, "me");
            })
            .await;
    }

    /// The efficiency contract at the store: a `working` agent whose detector
    /// re-emits the same tuple produces ZERO broadcasts after the first. The
    /// detector's edge filter normally means the drain never even sees these —
    /// this pins the store-side backstop that makes the invariant hold anyway.
    #[tokio::test(flavor = "current_thread")]
    async fn re_emitting_an_unchanged_state_writes_nothing() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let terminal = WireTerminalId::new(1);
                let scope = Scope::Terminal(terminal.clone());

                drain(
                    &state,
                    &terminal,
                    vec![AgentDetectEvent::State(report(DetectedState::Working))],
                )
                .await;
                let first = state
                    .with(|s| s.metadata().get(&scope, TERMINAL_AGENT_KEY))
                    .expect("written once");

                // Nine more identical emissions.
                let repeats = (0..9)
                    .map(|_| AgentDetectEvent::State(report(DetectedState::Working)))
                    .collect();
                drain(&state, &terminal, repeats).await;

                let after = state
                    .with(|s| s.metadata().get(&scope, TERMINAL_AGENT_KEY))
                    .expect("still there");
                assert_eq!(
                    first, after,
                    "byte-identical: metadata_set dedups the write"
                );
            })
            .await;
    }
}
