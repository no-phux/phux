//! Submodule for runtime internals.

use std::collections::HashSet;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::Path;

use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, LayerSet, ServerCapabilities,
    ServerFeature, ServerFeatureSet, select_bootstrap_profile,
};
use phux_protocol::policy::ConsumerId as PolicyConsumerId;
use phux_protocol::wire::frame::{AgentEvent, ErrorCode, FrameKind, TERMINAL_AGENT_KEY};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::{InputLaneHandle, RoutedInput};
use super::{
    STALE_PROBE_TIMEOUT, ServerError, SpawnRequest, handle_attach, handle_command,
    handle_frame_ack, handle_move_terminal, handle_spawn_terminal, handle_terminal_input,
    handle_terminal_reply, handle_terminal_resize, handle_viewport_resize,
};
use crate::state::{ClientId, DEFAULT_CLIENT_MAILBOX, Outbound, SharedState, TerminalInput};
use crate::terminal_actor::ConsumerDetachRequest;
use crate::transport::{FrameReader, FrameWriter, Incoming};

#[derive(Debug, Clone, Copy)]
struct NegotiatedConnection {
    client_caps: ClientCapabilities,
    profile: BootstrapProfile,
    limits: BootstrapLimits,
    server_features: ServerFeatureSet,
}

impl NegotiatedConnection {
    const fn accepts_terminal_reply(self) -> bool {
        self.server_features.contains(ServerFeature::TerminalReply)
    }
}
const fn runtime_server_features() -> ServerFeatureSet {
    ServerFeatureSet::with(&[
        ServerFeature::AcknowledgedInput,
        ServerFeature::FileUpload,
        ServerFeature::MoveTerminal,
        ServerFeature::TerminalReply,
    ])
}

#[cfg(test)]
mod negotiated_feature_tests {
    use super::*;

    fn connection(server_features: ServerFeatureSet) -> NegotiatedConnection {
        NegotiatedConnection {
            client_caps: ClientCapabilities::default(),
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features,
        }
    }

    #[test]
    fn old_07_without_terminal_reply_bit_discards_reply_without_routing() {
        assert!(!connection(ServerFeatureSet::new()).accepts_terminal_reply());
    }

    #[test]
    fn current_07_advertisement_enables_installed_terminal_reply_route() {
        let advertised = runtime_server_features();
        assert!(advertised.contains(ServerFeature::TerminalReply));
        assert!(connection(advertised).accepts_terminal_reply());
    }
}

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
            // Resolved under the lock, fired outside it: `fire_hook` re-takes
            // the state lock to clone the dispatcher handle, so firing from
            // inside `with_mut` would deadlock. `None` means nothing actually
            // changed and no hook is owed.
            let hook = state.with_mut(|s| {
                let scope = Scope::Terminal(wire_terminal_id.clone());
                // No dispatcher means no hook can run, so skip the work
                // entirely: reading the prior record costs a metadata lookup
                // and a JSON decode on every published transition, and a
                // server with no `[[hooks.agent-state-changed]]` entry must
                // not pay for a notification nobody asked for.
                let hooks_live = s.hook_dispatcher().is_some();
                match event {
                    AgentDetectEvent::Retract => {
                        // Only ever delete a record we authored. A human's
                        // declaration is not ours to retract.
                        if !s.agent_records().detector_owns(&wire_terminal_id) {
                            return None;
                        }
                        let from = hooks_live
                            .then(|| {
                                crate::agent_state::stored_state(
                                    s.metadata().get(&scope, TERMINAL_AGENT_KEY).as_deref(),
                                )
                            })
                            .flatten();
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
                                return hooks_live
                                    .then(|| retract_hook(&wire_terminal_id, from.as_deref()))
                                    .flatten();
                            }
                        }
                        s.metadata_delete(&scope, TERMINAL_AGENT_KEY);
                        s.agent_records_mut()
                            .note_detector_retract(&wire_terminal_id);
                        hooks_live
                            .then(|| retract_hook(&wire_terminal_id, from.as_deref()))
                            .flatten()
                    }
                    AgentDetectEvent::State(report) => {
                        // ADR-0046 §E: an explicit SET_METADATA that supplied
                        // a `state` outranks the detector entirely.
                        if s.agent_records().is_declared(&wire_terminal_id) {
                            return None;
                        }
                        let existing = s.metadata().get(&scope, TERMINAL_AGENT_KEY);
                        let from = hooks_live
                            .then(|| crate::agent_state::stored_state(existing.as_deref()))
                            .flatten();
                        let to = report.state.as_str();
                        let bytes = crate::agent_state::compose(
                            existing.as_deref(),
                            &report.kind,
                            &report.name,
                            to,
                        );
                        s.metadata_set(&scope, TERMINAL_AGENT_KEY, bytes);
                        s.agent_records_mut().note_detector_write(&wire_terminal_id);
                        hooks_live
                            .then(|| {
                                state_change_hook(
                                    &wire_terminal_id,
                                    &report.kind,
                                    &report.name,
                                    from.as_deref(),
                                    to,
                                )
                            })
                            .flatten()
                    }
                }
            });
            if let Some(event) = hook {
                crate::hooks::fire_hook(&state, event);
            }
        }
    });
}

/// The `agent-state-changed` event for a detector write, unless the store
/// already held that state.
///
/// The detector's edge filter models its OWN emissions, not the store, so a
/// republish after someone else wrote the record can land on the state that
/// is already there. Comparing against the store keeps the hook a true edge —
/// a notifier that fires on a non-change is a notifier the operator turns off.
fn state_change_hook(
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    kind: &str,
    name: &str,
    from: Option<&str>,
    to: &str,
) -> Option<crate::hooks::HookEvent> {
    if from == Some(to) {
        return None;
    }
    Some(crate::hooks::HookEvent::agent_state_changed(
        wire_terminal_id,
        kind,
        name,
        from,
        to,
    ))
}

/// The `agent-state-changed` event for a withdrawn record, unless the record
/// was already `unknown` (a retract that changes nothing owes no hook).
fn retract_hook(
    wire_terminal_id: &phux_protocol::ids::TerminalId,
    from: Option<&str>,
) -> Option<crate::hooks::HookEvent> {
    if from == Some(crate::hooks::AGENT_STATE_UNKNOWN) {
        return None;
    }
    Some(crate::hooks::HookEvent::agent_state_changed(
        wire_terminal_id,
        "",
        "",
        from,
        crate::hooks::AGENT_STATE_UNKNOWN,
    ))
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
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let _ = handle
            .native_release
            .try_send(crate::terminal_actor::NativeReleaseRequest { owner: client_id.0 });
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

/// Prepare and validate the parent directory of `socket_path` with mode `0o700`.
pub(crate) fn prepare_socket_dir(socket_path: &Path) -> Result<(), ServerError> {
    let Some(parent) = socket_path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let fail = |source| ServerError::PrepareDir {
        path: parent.to_path_buf(),
        source,
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(parent).map_err(fail)?;
    let metadata = std::fs::symlink_metadata(parent).map_err(fail)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(fail(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket parent must be a real directory, not a symlink",
        )));
    }
    let expected_uid = rustix::process::geteuid().as_raw();
    if metadata.uid() != expected_uid {
        return Err(fail(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket parent is owned by another user",
        )));
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(fail)?;
    let secured = std::fs::symlink_metadata(parent).map_err(fail)?;
    if secured.uid() != expected_uid || secured.mode() & 0o777 != 0o700 {
        return Err(fail(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket parent ownership or permissions changed during setup",
        )));
    }
    Ok(())
}

/// Restrict a freshly bound UDS to its owning user.
pub(crate) fn secure_socket_file(socket_path: &Path) -> Result<(), ServerError> {
    let metadata = std::fs::symlink_metadata(socket_path)?;
    let expected_uid = rustix::process::geteuid().as_raw();
    if !metadata.file_type().is_socket() || metadata.uid() != expected_uid {
        return Err(ServerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bound socket is not an owner-controlled Unix socket",
        )));
    }
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    let secured = std::fs::symlink_metadata(socket_path)?;
    if secured.uid() != expected_uid || secured.mode() & 0o777 != 0o600 {
        return Err(ServerError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "bound socket ownership or permissions changed during setup",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod socket_security_tests {
    use super::*;

    #[test]
    fn existing_permissive_socket_directory_is_restricted() {
        let root = tempfile::tempdir().expect("tempdir");
        let parent = root.path().join("runtime");
        std::fs::create_dir(&parent).expect("runtime dir");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777))
            .expect("permissive mode");

        prepare_socket_dir(&parent.join("phux.sock")).expect("secure existing directory");

        let mode = std::fs::symlink_metadata(parent)
            .expect("secured metadata")
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn socket_parent_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("tempdir");
        let target = root.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let parent = root.path().join("runtime-link");
        symlink(&target, &parent).expect("symlink");
        assert!(matches!(
            prepare_socket_dir(&parent.join("phux.sock")),
            Err(ServerError::PrepareDir { source, .. })
                if source.kind() == io::ErrorKind::InvalidInput
        ));
    }
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
                        // phux-n6rv: count the live connection before the task
                        // exists, so the idle-exit watchdog can never observe a
                        // window where the server looks unattended between
                        // `accept` returning and the client task being polled.
                        // Paired with `note_connection_closed` at the end of
                        // that task; every transport funnels through here, so
                        // this is the one place the pair has to hold.
                        state.with_mut(crate::state::ServerState::note_connection_opened);
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
                            // phux-n6rv: re-arm the idle clock if this was the
                            // last connection. Runs after the detach above so
                            // "unattended" and "detached" become true in the
                            // same tick. A task ABORTED by shutdown never gets
                            // here, which is harmless: the only reader of the
                            // clock is the watchdog, and shutdown is already
                            // underway.
                            task_state.with_mut(crate::state::ServerState::note_connection_closed);
                        });
                    }
                    Err(err) => {
                        if listener.accept_errors_are_fatal() {
                            return Err(err.into());
                        }
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

/// The wire carries one concrete version rather than a range. A patch release
/// is editorial/behavior-preserving, while a major or minor change may alter
/// the wire contract; therefore only `major.minor` must match.
const fn protocol_is_compatible(client_major: u16, client_minor: u16) -> bool {
    client_major == PROTOCOL_VERSION.major && client_minor == PROTOCOL_VERSION.minor
}

fn incompatible_protocol_message(
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
         server requires {}.{}.x; {remediation} so protocol major.minor match",
        PROTOCOL_VERSION.major, PROTOCOL_VERSION.minor,
    )
}

const WRITER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

async fn close_client_writer(
    out_tx: tokio::sync::mpsc::Sender<Outbound>,
    writer_close: &tokio::sync::watch::Sender<bool>,
    sibling_tasks: &mut JoinSet<()>,
) {
    // The close command is independent of sender liveness. The writer closes
    // its receiver, drains every frame already ordered before this command,
    // then calls FrameWriter::close.
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

/// Per-client task. Reads frames in a loop and dispatches each one.
///
/// Outbound messages are routed through a per-client `mpsc` channel
/// drained by a sibling writer task (also `spawn_local`'d). This gives
/// us one place to back-pressure on slow clients without entangling
/// the read side, and matches the `tx: mpsc::Sender<Outbound>` shape
///
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
    let (writer_close_tx, writer_close_rx) = tokio::sync::watch::channel(false);
    // Per-client `JoinSet` for sibling tasks (today: just the writer).
    // Held in this scope so it drops with `handle_client` and the
    // writer aborts if it hasn't already exited via its own
    // close-on-EOF path. Keeps lifecycle plumbing local.
    let mut sibling_tasks: JoinSet<()> = JoinSet::new();
    sibling_tasks.spawn_local(writer_task(writer, out_rx, writer_close_rx, client_id));

    // Per-attach raw-output pumps. These are deliberately separate from
    // `sibling_tasks`: DETACH/session switch must abort pane output pumps
    // without killing the writer, because the writer still needs to emit
    // DETACHED and may serve a later ATTACH on the same connection.
    let mut output_pumps: JoinSet<()> = JoinSet::new();
    // An attach id names one immutable aggregate generation for the life of
    // this connection. Reuse would collide with a completed stream/bootstrap
    // key even when the replacement otherwise followed the right barriers.
    let mut used_attach_ids = HashSet::new();

    // Exact per-connection bootstrap state selected by HELLO. `None` is the
    // pre-negotiation state; successful selection writes it exactly once and
    // duplicate HELLO is fatal, so an attached connection can never mutate it.
    let mut negotiated: Option<NegotiatedConnection> = None;

    loop {
        // Pull the next complete frame from the transport — length-prefixed on
        // UDS, one binary message on WebSocket (see `transport.rs`). EOF ends
        // the session cleanly; cancellation preempts a slow read via the biased
        // select so a server-wide shutdown isn't blocked behind it.
        let framed = tokio::select! {
            biased;
            () = token.cancelled() => {
                debug!(?client_id, "client task cancelled");
                abort_output_pumps(&mut output_pumps, client_id, "connection cancellation").await;
                detach_and_release_consumer_state(&state, client_id);
                close_client_writer(out_tx, &writer_close_tx, &mut sibling_tasks).await;
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

        let decoded = negotiated.as_ref().map_or_else(
            || FrameKind::decode(&framed),
            |selection| FrameKind::decode_with_limits(&framed, selection.limits),
        );
        let frame = match decoded {
            Ok((frame, _rest)) => frame,
            Err(err) => {
                warn!(error = ?err, "client sent undecodable frame; closing");
                return Ok(());
            }
        };

        // PING is exempt: a stateless, version-insensitive liveness probe
        // (the connector's consumer health check is exactly that), and the
        // spec's close-before-processing clause targets "ATTACH or other
        // stateful frames".
        if negotiated.is_none()
            && !matches!(frame, FrameKind::Hello { .. } | FrameKind::Ping { .. })
        {
            warn!(?client_id, "stateful frame before HELLO; closing");
            let _ = out_tx
                .send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::VersionIncompatible,
                    message: "HELLO required before any stateful frame".to_owned(),
                }))
                .await;
            // Flush the refusal before closing the transport.
            drop(out_tx);
            let _ = sibling_tasks.join_next().await;
            return Ok(());
        }

        match frame {
            FrameKind::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => {
                if negotiated.is_some() {
                    warn!(?client_id, "duplicate HELLO; closing");
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::InvalidCommand,
                            message: "HELLO already completed on this connection".to_owned(),
                        }))
                        .await;
                    // Never patch a live client's capabilities. Tear down any
                    // attached profile so its mailbox sender cannot keep the
                    // writer alive, flush the protocol-order error, then close.
                    abort_output_pumps(&mut output_pumps, client_id, "duplicate HELLO").await;
                    detach_and_release_consumer_state(&state, client_id);
                    drop(out_tx);
                    let _ = sibling_tasks.join_next().await;
                    return Ok(());
                }
                debug!(
                    ?client_id,
                    %client_name,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    color_support = ?client_caps.color_support,
                    "HELLO",
                );
                if !protocol_is_compatible(protocol_major, protocol_minor) {
                    let message = incompatible_protocol_message(
                        protocol_major,
                        protocol_minor,
                        protocol_patch,
                    );
                    warn!(?client_id, %message, "HELLO protocol mismatch");
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::VersionIncompatible,
                            message,
                        }))
                        .await;
                    // Closing the last sender lets the writer flush the ERROR
                    // before this task closes the transport.
                    drop(out_tx);
                    let _ = sibling_tasks.join_next().await;
                    return Ok(());
                }
                // Policy check: authorize HELLO only against the identity
                // authenticated by the accepting transport. A missing registry
                // entry is never equivalent to a local root peer.
                let Some(peer) = state.with(|s| s.peer_identity(client_id).cloned()) else {
                    warn!(
                        ?client_id,
                        "HELLO denied: authenticated peer identity missing"
                    );
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::PermissionDenied,
                            message: "authenticated peer identity missing".to_owned(),
                        }))
                        .await;
                    drop(out_tx);
                    let _ = sibling_tasks.join_next().await;
                    return Ok(());
                };
                let policy_ok = {
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
                    // Do not abort the writer before the policy refusal is on
                    // the transport.
                    drop(out_tx);
                    let _ = sibling_tasks.join_next().await;
                    return Ok(());
                }
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                let server_bootstrap = crate::native_state::native_bootstrap_capabilities();
                #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
                let server_bootstrap = BootstrapCapabilities::new();
                let Ok((selected_profile, bootstrap_limits)) =
                    select_bootstrap_profile(&client_caps, &server_bootstrap)
                else {
                    let message = format!(
                        "no common protocol-0.7 bootstrap profile: client profiles=0x{:02x} native_codecs=0x{:016x} native_features=0x{:08x}; server profiles=0x{:02x} native_codecs=0x{:016x} native_features=0x{:08x}. NativeState requires an exact common codec and every required engine feature; advertise SynthesizedVtRaw/SynthesizedVtStateSync or update the incompatible peer",
                        client_caps.bootstrap.profiles.as_wire(),
                        client_caps.bootstrap.native_codecs.as_wire(),
                        client_caps.bootstrap.native_features.as_wire(),
                        server_bootstrap.profiles.as_wire(),
                        server_bootstrap.native_codecs.as_wire(),
                        server_bootstrap.native_features.as_wire(),
                    );
                    warn!(?client_id, %message, "HELLO codec unavailable");
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::CodecUnavailable,
                            message,
                        }))
                        .await;
                    drop(out_tx);
                    let _ = sibling_tasks.join_next().await;
                    return Ok(());
                };
                let mut effective_client_caps = client_caps;
                effective_client_caps.output_mode =
                    if matches!(selected_profile, BootstrapProfile::SynthesizedVtStateSync) {
                        phux_protocol::caps::OutputMode::StateSync
                    } else {
                        // NativeState and SynthesizedVtRaw both carry raw live PTY
                        // output regardless of the client's compatibility
                        // preference field.
                        phux_protocol::caps::OutputMode::Raw
                    };
                let server_features = runtime_server_features();

                // Cache all negotiated state exactly once before any stateful
                // frame can be processed. Subsequent decoding immediately uses
                // these bounds, rejecting oversized borrowed payloads before
                // the protocol decoder copies them into owned storage.
                negotiated = Some(NegotiatedConnection {
                    client_caps: effective_client_caps,
                    profile: selected_profile,
                    limits: bootstrap_limits,
                    server_features,
                });
                state.with_mut(|s| {
                    // SPEC §6.2: cache the negotiated layer set. The L3
                    // dispatch arms gate METADATA_CHANGED on this value.
                    s.set_client_layers(client_id, client_caps.layers);
                });
                let hello_ok = FrameKind::HelloOk {
                    protocol_major: PROTOCOL_VERSION.major,
                    protocol_minor: PROTOCOL_VERSION.minor,
                    protocol_patch: PROTOCOL_VERSION.patch,
                    server_caps: ServerCapabilities::new()
                        .with_layers(LayerSet::all())
                        .with_features(server_features),
                    server_id: state.with(|server| server.server_incarnation().as_bytes().to_vec()),
                    selected_profile,
                    bootstrap_limits,
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
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
            } => {
                if attach_id == 0 {
                    warn!(?client_id, "ATTACH used reserved zero attach_id; closing");
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::MalformedMessage,
                            message: "ATTACH attach_id must be nonzero".to_owned(),
                        }))
                        .await;
                    abort_output_pumps(&mut output_pumps, client_id, "zero ATTACH id").await;
                    detach_and_release_consumer_state(&state, client_id);
                    drop(out_tx);
                    let _ = sibling_tasks.join_next().await;
                    return Ok(());
                }
                if !used_attach_ids.insert(attach_id) {
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::MalformedMessage,
                            message: format!(
                                "ATTACH attach_id {attach_id} was already used on this connection"
                            ),
                        }))
                        .await;
                    continue;
                }
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                debug!(
                    ?client_id,
                    attach_id,
                    profile = ?selection.profile,
                    chunk_limit = selection.limits.max_chunk_bytes(),
                    history_page_limit = selection.limits.max_history_page_bytes(),
                    "ATTACH with immutable bootstrap selection",
                );
                handle_attach(
                    &state,
                    client_id,
                    attach_id,
                    target,
                    viewport,
                    request_scrollback,
                    scrollback_limit_lines,
                    &out_tx,
                    selection.client_caps,
                    selection.profile,
                    selection.limits,
                    &root_token,
                    &mut output_pumps,
                    &token,
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
            FrameKind::InputTerminalReply { terminal_id, bytes } => {
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                if !selection.accepts_terminal_reply() {
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::UnknownMessageType,
                            message: "INPUT_TERMINAL_REPLY was not advertised for this connection"
                                .to_owned(),
                        }))
                        .await;
                    // The frame is additive within protocol 0.7: report the
                    // unadvertised type without killing an otherwise valid
                    // connection, and never pass its bytes to the PTY.
                    continue;
                }
                handle_terminal_reply(&state, client_id, &terminal_id, bytes);
            }
            FrameKind::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => {
                handle_frame_ack(
                    &state,
                    client_id,
                    &terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                );
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            FrameKind::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => {
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                if !matches!(
                    selection.profile,
                    BootstrapProfile::NativeState {
                        codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
                        ..
                    }
                ) {
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::CodecUnavailable,
                            message: "HISTORY_REQUEST requires negotiated native checkpoint v2"
                                .to_owned(),
                        }))
                        .await;
                    continue;
                }
                let handle = state.with(|server| {
                    server
                        .terminal_from_wire(&terminal_id)
                        .and_then(|pane| server.terminal_handle(pane).cloned())
                });
                let Some(handle) = handle else {
                    let _ = out_tx
                        .send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code: ErrorCode::TerminalNotFound,
                            message: format!("no such terminal: {terminal_id:?}"),
                        }))
                        .await;
                    continue;
                };
                let Ok(permit) = out_tx.clone().reserve_owned().await else {
                    continue;
                };
                let (reply_tx, reply_rx) = oneshot::channel();
                if handle
                    .native_history
                    .send(crate::terminal_actor::NativeHistoryRequest {
                        permit,
                        owner: client_id.0,
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor,
                        max_bytes,
                        max_rows,
                        limits: selection.limits,
                        reply: reply_tx,
                    })
                    .await
                    .is_err()
                {
                    continue;
                }
                let Ok(reply) = reply_rx.await else {
                    continue;
                };
                match reply.result {
                    Ok(frame) => {
                        reply.permit.send(Outbound::Frame(frame));
                    }
                    Err(error) => {
                        let code = match error {
                            crate::native_state::NativeStateError::OutOfMemory
                            | crate::native_state::NativeStateError::OutOfSpace { .. }
                            | crate::native_state::NativeStateError::LimitExceeded => {
                                ErrorCode::ResourceExhausted
                            }
                            _ => ErrorCode::InternalError,
                        };
                        reply.permit.send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code,
                            message: format!("native history request failed: {error}"),
                        }));
                    }
                }
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
                agent_session,
            } => {
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
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
                        agent_session,
                    },
                    &out_tx,
                    selection.profile,
                    selection.limits,
                    &root_token,
                    &token,
                    &mut output_pumps,
                )
                .await;
            }
            FrameKind::MoveTerminal {
                request_id,
                terminal,
                owner_terminal,
            } => {
                handle_move_terminal(
                    &state,
                    client_id,
                    request_id,
                    terminal,
                    owner_terminal,
                    &out_tx,
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
                let Some(selection) = negotiated.as_ref() else {
                    continue;
                };
                handle_command(
                    &state,
                    client_id,
                    request_id,
                    command,
                    &out_tx,
                    selection.client_caps,
                    selection.profile,
                    selection.limits,
                    input_lane.as_ref(),
                    &token,
                )
                .await;
            }
            other => {
                warn!(?client_id, kind = ?other, "direction-invalid client frame; closing");
                let _ = out_tx
                    .send(Outbound::Frame(FrameKind::Error {
                        request_id: None,
                        code: ErrorCode::InvalidCommand,
                        message: format!(
                            "frame is not valid from a client in the negotiated phase: {other:?}"
                        ),
                    }))
                    .await;
                abort_output_pumps(&mut output_pumps, client_id, "direction-invalid frame").await;
                detach_and_release_consumer_state(&state, client_id);
                drop(out_tx);
                let _ = sibling_tasks.join_next().await;
                return Ok(());
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
    let nonce_result = is_reserved_session_create_result(scope, key);
    let (value, speaks_l3) = state.with(|s| {
        let authorized = !nonce_result || s.owns_session_create_result(client_id, key);
        (
            authorized.then(|| s.metadata().get(scope, key)).flatten(),
            s.client_speaks_l3(client_id),
        )
    });
    let one_shot = value.is_some() && nonce_result;
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
    } else if one_shot {
        state.with_mut(|s| s.consume_session_create_result(key));
    }
}

#[derive(serde::Deserialize)]
struct SessionCreateRequest {
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    agent_session: Option<Vec<u8>>,
    #[serde(default)]
    request_token: Option<String>,
}

/// Parse the typed JSON body of a `SESSION_CREATE_KEY` write.
fn parse_session_create_request(value: &[u8]) -> Option<SessionCreateRequest> {
    serde_json::from_slice(value).ok()
}

fn valid_session_create_token(token: &str) -> bool {
    token.len() == 36
        && token.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn is_reserved_session_create_result(scope: &phux_protocol::wire::frame::Scope, key: &str) -> bool {
    matches!(scope, phux_protocol::wire::frame::Scope::Global)
        && key.starts_with(phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX)
}

fn handle_session_create_metadata(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    value: &[u8],
    root_token: &tokio_util::sync::CancellationToken,
) {
    use phux_protocol::wire::frame::{
        MAX_AGENT_SESSION_RECORD_BYTES, SESSION_CREATE_RESULT_KEY,
        SESSION_CREATE_RESULT_KEY_PREFIX, Scope,
    };

    let Some(SessionCreateRequest {
        agent_session,
        name,
        command,
        cwd,
        env,
        request_token,
    }) = parse_session_create_request(value)
    else {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-create): malformed JSON value (want {{name, command?, cwd?, env?, request_token?, agent_session?}}); ignoring",
        );
        return;
    };
    if agent_session
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        warn!(
            ?client_id,
            request_id, "SET_METADATA(session-create): invalid agent session record size; ignoring"
        );
        return;
    }
    if request_token
        .as_deref()
        .is_some_and(|token| !valid_session_create_token(token))
    {
        warn!(
            ?client_id,
            request_id, "SET_METADATA(session-create): invalid request token; ignoring"
        );
        return;
    }
    let result_key = request_token.as_ref().map_or_else(
        || SESSION_CREATE_RESULT_KEY.to_owned(),
        |token| format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}"),
    );
    if request_token.is_some() && state.with(|s| s.session_create_result_is_pending(&result_key)) {
        warn!(
            ?client_id,
            request_id,
            "SET_METADATA(session-create): request token already has a pending result; ignoring"
        );
        return;
    }
    let outcome = crate::runtime::commands::create_named_session(
        state,
        &name,
        command,
        cwd.as_deref(),
        env,
        agent_session,
        root_token,
    );
    if let Ok(wire) = &outcome {
        // A nonce-bearing client gets a one-shot, request-specific result key.
        // Legacy requests retain the original global key.
        let payload = serde_json::json!({
            "name": name,
            "terminal_id": wire.local_id(),
            "request_token": request_token,
        });
        if let Ok(bytes) = serde_json::to_vec(&payload) {
            // `result_key` was reserved above before the synchronous create.
            state.with_mut(|s| {
                let _ = s.metadata_set(&Scope::Global, &result_key, bytes);
                if request_token.is_some() {
                    s.track_session_create_result(client_id, result_key);
                }
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
/// Reject writes into a local Terminal namespace after its owner is gone.
///
/// Satellite scopes stay hub-owned metadata until that pre-existing routing
/// contract is migrated.
fn reject_unknown_local_terminal_scope(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    scope: &phux_protocol::wire::frame::Scope,
    key: &str,
) -> bool {
    use phux_protocol::wire::frame::Scope;

    let Scope::Terminal(terminal @ phux_protocol::ids::TerminalId::Local { .. }) = scope else {
        return false;
    };
    if state.with(|s| s.terminal_from_wire(terminal)).is_some() {
        return false;
    }
    warn!(
        ?client_id,
        request_id,
        ?terminal,
        %key,
        "SET_METADATA: unknown terminal scope; ignoring",
    );
    true
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
    use phux_protocol::wire::frame::{
        MAX_AGENT_SESSION_RECORD_BYTES, SESSION_CREATE_KEY, Scope, TERMINAL_AGENT_SESSION_KEY,
    };
    debug!(?client_id, request_id, ?scope, %key, "SET_METADATA");
    if is_reserved_session_create_result(scope, key) {
        warn!(
            ?client_id,
            request_id, "SET_METADATA: reserved session-create result key; ignoring"
        );
        return;
    }
    // Terminal scope is an ownership address, not an arbitrary namespace.
    if reject_unknown_local_terminal_scope(state, client_id, request_id, scope, key) {
        return;
    }
    if key == TERMINAL_AGENT_SESSION_KEY
        && (!matches!(
            scope,
            Scope::Terminal(phux_protocol::ids::TerminalId::Local { .. })
        ) || value.is_empty()
            || value.len() > MAX_AGENT_SESSION_RECORD_BYTES)
    {
        warn!(
            ?client_id,
            request_id,
            ?scope,
            value_len = value.len(),
            "SET_METADATA(agent-session): want a local Terminal scope and 1..=4096 bytes; ignoring",
        );
        return;
    }
    // v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027): a create-without-
    // attach is a `SET_METADATA` write of the conventional
    // `SESSION_CREATE_KEY` under `Scope::Global`, replacing the removed
    // `CREATE_SESSION` verb. Its UTF-8 JSON object may carry
    // `{ name, command?, cwd?, env?, request_token?, agent_session? }`.
    // The server seeds the session + pane; a nonce-bearing caller reads its exact
    // key because SET_METADATA has no reply frame. A malformed value or a
    // duplicate name is a silent no-op (logged), matching the fire-and-forget
    // shape of metadata writes.
    if key == SESSION_CREATE_KEY && matches!(scope, Scope::Global) {
        handle_session_create_metadata(state, client_id, request_id, &value, root_token);
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
    if is_reserved_session_create_result(scope, key) {
        warn!(
            ?client_id,
            request_id, "DELETE_METADATA: reserved session-create result key; ignoring"
        );
        return;
    }
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
    let (mut keys, speaks_l3) =
        state.with(|s| (s.metadata().list(scope), s.client_speaks_l3(client_id)));
    if matches!(scope, phux_protocol::wire::frame::Scope::Global) {
        keys.retain(|key| {
            !key.starts_with(phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX)
        });
    }
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
    if is_reserved_session_create_result(&scope, &key) {
        warn!(
            ?client_id,
            "SUBSCRIBE_METADATA: reserved session-create result key; ignoring"
        );
        return;
    }
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
                    bootstrap_profile: None,
                    bootstrap_limits: None,
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
    mut close: tokio::sync::watch::Receiver<bool>,
    client_id: ClientId,
) {
    let mut buf = BytesMut::with_capacity(1024);
    let mut close_control_open = true;
    loop {
        let message = if close_control_open {
            tokio::select! {
                biased;
                changed = close.changed() => {
                    match changed {
                        Ok(()) if *close.borrow_and_update() => {
                            rx.close();
                            close_control_open = false;
                        }
                        _ => close_control_open = false,
                    }
                    continue;
                }
                message = rx.recv() => message,
            }
        } else {
            rx.recv().await
        };
        let Some(message) = message else {
            break;
        };
        let (frame, terminal) = match message {
            Outbound::Frame(frame) => (frame, false),
            Outbound::TerminalError {
                request_id,
                code,
                message,
            } => (
                FrameKind::Error {
                    request_id,
                    code,
                    message,
                },
                true,
            ),
        };
        buf.clear();
        frame.encode(&mut buf);
        if let Err(err) = writer.write_frame(&buf).await {
            debug!(?client_id, error = %err, "writer error on frame; client task ending");
            let _ = writer.close().await;
            return;
        }
        if terminal {
            let _ = writer.close().await;
            return;
        }
    }
    if let Err(err) = writer.close().await {
        debug!(?client_id, error = %err, "writer close failed");
    }
    debug!(?client_id, "writer task exiting (channel closed)");
}

#[cfg(test)]
mod writer_close_tests {
    use std::cell::RefCell;
    use std::io;
    use std::rc::Rc;

    use phux_protocol::wire::frame::{ErrorCode, FrameKind};

    use super::writer_task;
    use crate::state::{ClientId, Outbound};
    use crate::transport::FrameWriter;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum WriterEvent {
        Frame,
        Error,
        Close,
    }

    struct RecordingWriter(Rc<RefCell<Vec<WriterEvent>>>);

    impl FrameWriter for RecordingWriter {
        async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
            let event = match FrameKind::decode(frame).expect("encoded frame").0 {
                FrameKind::Error { .. } => WriterEvent::Error,
                _ => WriterEvent::Frame,
            };
            self.0.borrow_mut().push(event);
            Ok(())
        }

        async fn close(&mut self) -> io::Result<()> {
            self.0.borrow_mut().push(WriterEvent::Close);
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn terminal_error_is_written_before_transport_close() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let writer = RecordingWriter(Rc::clone(&events));
        let (tx, rx) = tokio::sync::mpsc::channel(3);
        tx.send(Outbound::Frame(FrameKind::Pong { nonce: 1 }))
            .await
            .expect("queue earlier frame");
        tx.send(Outbound::TerminalError {
            request_id: None,
            code: ErrorCode::CodecUnavailable,
            message: "fatal native stream failure".to_owned(),
        })
        .await
        .expect("queue terminal error");
        tx.send(Outbound::Frame(FrameKind::Pong { nonce: 2 }))
            .await
            .expect("racing producer queues after sentinel");

        let (_close_tx, close_rx) = tokio::sync::watch::channel(false);
        writer_task(writer, rx, close_rx, ClientId(7)).await;
        assert_eq!(
            events.borrow().as_slice(),
            [WriterEvent::Frame, WriterEvent::Error, WriterEvent::Close],
            "the terminal ERROR is the last decoded frame before transport close",
        );
    }
}
#[cfg(test)]
#[cfg(all(test, feature = "native-engine", not(target_arch = "wasm32")))]
mod fatal_preflight_close_tests {
    use std::collections::VecDeque;
    use std::io;

    use bytes::BytesMut;
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapCapabilities, ClientCapabilities, EngineCodec, EngineFeatureSet,
    };
    use phux_protocol::policy::{PeerIdentity, TransportType};
    use phux_protocol::wire::frame::{AttachTarget, ErrorCode, FrameKind, ViewportInfo};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::{broadcast, mpsc};
    use tokio::task::LocalSet;
    use tokio_util::sync::CancellationToken;

    use super::handle_client;
    use crate::state::SharedState;
    use crate::terminal_actor::{ConsumerAttachOutcome, PaneOutput, TerminalHandle};
    use crate::transport::{FrameReader, FrameWriter};

    struct ScriptReader {
        frames: VecDeque<BytesMut>,
    }

    impl ScriptReader {
        fn new(frames: impl IntoIterator<Item = FrameKind>) -> Self {
            Self {
                frames: frames
                    .into_iter()
                    .map(|frame| {
                        let mut encoded = BytesMut::new();
                        frame.encode(&mut encoded);
                        encoded
                    })
                    .collect(),
            }
        }
    }

    impl FrameReader for ScriptReader {
        async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Some(frame));
            }
            std::future::pending().await
        }
    }

    struct DuplexWriter(tokio::io::WriteHalf<tokio::io::DuplexStream>);

    impl FrameWriter for DuplexWriter {
        async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
            self.0.write_all(frame).await
        }

        async fn close(&mut self) -> io::Result<()> {
            self.0.shutdown().await
        }
    }

    async fn read_frame(
        reader: &mut tokio::io::ReadHalf<tokio::io::DuplexStream>,
    ) -> io::Result<Option<FrameKind>> {
        let mut header = [0_u8; 4];
        match reader.read_exact(&mut header).await {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let body_len = u32::from_be_bytes(header) as usize;
        let mut framed = BytesMut::with_capacity(4 + body_len);
        framed.extend_from_slice(&header);
        framed.resize(4 + body_len, 0);
        reader.read_exact(&mut framed[4..]).await?;
        FrameKind::decode(&framed)
            .map(|(frame, _)| Some(frame))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, format!("{error:?}")))
    }

    fn native_failure_handle() -> (
        TerminalHandle,
        mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
        mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
    ) {
        let (output, _output_seed) = broadcast::channel::<PaneOutput>(8);
        let (consumer_attach, consumer_attach_rx) = mpsc::channel(8);
        let (native_bootstrap, native_bootstrap_rx) = mpsc::channel(8);
        let (native_release, _native_release_rx) = mpsc::channel(8);
        let (consumer_detach, _consumer_detach_rx) = mpsc::channel(8);
        (
            TerminalHandle {
                input: mpsc::channel(8).0,
                encoded_input: mpsc::channel(8).0,
                input_snapshot: tokio::sync::watch::channel(
                    crate::input::InputEncoderSnapshot::default(),
                )
                .1,
                snapshot: mpsc::channel(8).0,
                native_bootstrap,
                native_publication: mpsc::channel(8).0,
                native_history: mpsc::channel(8).0,
                native_release,
                set_default_colors: mpsc::channel(8).0,
                screen: mpsc::channel(8).0,
                pwd: mpsc::channel(8).0,
                output,
                resize: mpsc::channel(8).0,
                consumer_attach,
                consumer_detach,
                consumer_ack: mpsc::channel(8).0,
                subscribe_to_events: mpsc::channel(8).0,
                unsubscribe_from_events: mpsc::channel(8).0,
                upgrade: mpsc::channel(8).0,
                control: mpsc::channel(8).0,
                cols: 80,
                rows: 24,
            },
            consumer_attach_rx,
            native_bootstrap_rx,
        )
    }

    #[allow(clippy::too_many_lines)]
    #[tokio::test(flavor = "current_thread")]
    async fn native_preflight_failure_flushes_error_then_duplex_eof() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_session, _window, terminal) =
                    state.with_mut(|server| server.seed_session("fatal-duplex"));
                let (handle, mut consumer_attach_rx, mut native_bootstrap_rx) =
                    native_failure_handle();
                state.with_mut(|server| {
                    let _ =
                        server.register_terminal_handle(terminal, handle, CancellationToken::new());
                });
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                state.with_mut(|server| {
                    server.set_peer_identity(
                        client_id,
                        PeerIdentity {
                            uid: 0,
                            pid: None,
                            exe_path: None,
                            mcp_host_key: None,
                            transport: TransportType::UnixSocket,
                            source_addr: None,
                        },
                    );
                });

                let native = BootstrapCapabilities::new().with_native(
                    EngineCodec::LibghosttyCheckpointV2,
                    EngineFeatureSet::required_native(),
                );
                let reader = ScriptReader::new([
                    FrameKind::Hello {
                        client_name: "fatal-duplex-test".to_owned(),
                        protocol_major: PROTOCOL_VERSION.major,
                        protocol_minor: PROTOCOL_VERSION.minor,
                        protocol_patch: PROTOCOL_VERSION.patch,
                        client_caps: ClientCapabilities::new().with_bootstrap(native),
                    },
                    FrameKind::Attach {
                        attach_id: 1,
                        target: AttachTarget::ByName("fatal-duplex".to_owned()),
                        viewport: ViewportInfo::new(80, 24),
                        request_scrollback: false,
                        scrollback_limit_lines: 0,
                    },
                ]);
                let (server_io, peer_io) = tokio::io::duplex(64 * 1024);
                let (server_read, server_write) = tokio::io::split(server_io);
                drop(server_read);
                let (mut client_read, client_write) = tokio::io::split(peer_io);
                drop(client_write);
                let connection_token = CancellationToken::new();
                let task = tokio::task::spawn_local(handle_client(
                    reader,
                    DuplexWriter(server_write),
                    state,
                    client_id,
                    connection_token,
                    CancellationToken::new(),
                    None,
                ));
                let actor = tokio::task::spawn_local(async move {
                    let registration = consumer_attach_rx
                        .recv()
                        .await
                        .expect("consumer registration");
                    registration
                        .reply
                        .send(Ok(ConsumerAttachOutcome {
                            tick_managed: false,
                            state_sync_bootstrap: None,
                        }))
                        .expect("registration reply");
                    native_bootstrap_rx
                        .recv()
                        .await
                        .expect("native preflight")
                        .reply
                        .send(Err(crate::native_state::NativeStateError::OutOfMemory))
                        .expect("native failure reply");
                });

                let hello = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    read_frame(&mut client_read),
                )
                .await
                .expect("HELLO_OK timed out")
                .expect("read HELLO_OK")
                .expect("HELLO_OK frame");
                assert!(matches!(
                    hello,
                    FrameKind::HelloOk {
                        selected_profile: phux_protocol::caps::BootstrapProfile::NativeState {
                            codec: EngineCodec::LibghosttyCheckpointV2,
                            ..
                        },
                        ..
                    }
                ));
                let error = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    read_frame(&mut client_read),
                )
                .await
                .expect("terminal ERROR timed out")
                .expect("read terminal ERROR")
                .expect("terminal ERROR frame");
                assert!(matches!(
                    error,
                    FrameKind::Error {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    }
                ));
                assert!(
                    tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        read_frame(&mut client_read),
                    )
                    .await
                    .expect("duplex EOF timed out")
                    .expect("read duplex EOF")
                    .is_none(),
                    "transport closes only after flushing terminal ERROR"
                );
                actor.await.expect("actor task");
                task.await
                    .expect("client task")
                    .expect("client task result");
            })
            .await;
    }
}
#[cfg(test)]
mod terminal_metadata_scope_tests {
    use phux_protocol::ids::TerminalId as WireTerminalId;
    use phux_protocol::wire::frame::Scope;
    use tokio_util::sync::CancellationToken;

    use super::handle_set_metadata;
    use crate::state::{ClientId, SharedState};

    #[test]
    fn terminal_metadata_cannot_outlive_or_target_a_missing_terminal() {
        let state = SharedState::new();
        let (_session, _window, pane) = state.with_mut(|s| s.seed_session("scope-test"));
        let wire = state.with_mut(|s| s.intern_terminal_wire(pane));
        let scope = Scope::Terminal(wire);
        let token = CancellationToken::new();

        handle_set_metadata(
            &state,
            ClientId(1),
            1,
            &scope,
            "phux.test/v1",
            b"live".to_vec(),
            &token,
        );
        assert_eq!(
            state.with(|s| s.metadata().get(&scope, "phux.test/v1")),
            Some(b"live".to_vec())
        );

        for (request_id, invalid) in [(10, Vec::new()), (11, vec![b'x'; 4097])] {
            handle_set_metadata(
                &state,
                ClientId(1),
                request_id,
                &scope,
                phux_protocol::wire::frame::TERMINAL_AGENT_SESSION_KEY,
                invalid,
                &token,
            );
            assert!(
                state
                    .with(|s| {
                        s.metadata().get(
                            &scope,
                            phux_protocol::wire::frame::TERMINAL_AGENT_SESSION_KEY,
                        )
                    })
                    .is_none(),
                "invalid reserved agent-session metadata must not be stored",
            );
        }

        state.with_mut(|s| s.reap_terminal(pane));
        handle_set_metadata(
            &state,
            ClientId(1),
            2,
            &scope,
            "phux.test/v1",
            b"orphan".to_vec(),
            &token,
        );
        assert!(
            state
                .with(|s| s.metadata().get(&scope, "phux.test/v1"))
                .is_none(),
            "reaped Terminal metadata is deleted and a stale id cannot recreate it"
        );

        let missing = Scope::Terminal(WireTerminalId::local(u32::MAX));
        handle_set_metadata(
            &state,
            ClientId(1),
            3,
            &missing,
            "phux.test/v1",
            b"missing".to_vec(),
            &token,
        );
        assert!(
            state
                .with(|s| s.metadata().get(&missing, "phux.test/v1"))
                .is_none()
        );
    }

    #[test]
    fn ordinary_metadata_cannot_write_the_owner_only_create_result_namespace() {
        let state = SharedState::new();
        let token = CancellationToken::new();
        let reserved_key = format!(
            "{}11111111-1111-4111-8111-111111111111",
            phux_protocol::wire::frame::SESSION_CREATE_RESULT_KEY_PREFIX,
        );
        handle_set_metadata(
            &state,
            ClientId(2),
            4,
            &Scope::Global,
            &reserved_key,
            b"forged".to_vec(),
            &token,
        );
        assert!(
            state
                .with(|s| s.metadata().get(&Scope::Global, &reserved_key))
                .is_none(),
            "the owner-only result namespace must reject ordinary metadata writes",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pending_session_create_token_cannot_be_reused_by_another_connection() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let state = SharedState::new();
                let root_token = CancellationToken::new();
                let request_token = "11111111-1111-4111-8111-111111111111";
                for (client_id, request_id, name) in
                    [(ClientId(1), 1, "first"), (ClientId(2), 2, "collision")]
                {
                    let value = serde_json::to_vec(&serde_json::json!({
                        "name": name,
                        "request_token": request_token,
                    }))
                    .expect("request JSON");
                    handle_set_metadata(
                        &state,
                        client_id,
                        request_id,
                        &Scope::Global,
                        phux_protocol::wire::frame::SESSION_CREATE_KEY,
                        value,
                        &root_token,
                    );
                }

                assert!(state.with(|s| s.session_by_name("first").is_some()));
                assert!(
                    state.with(|s| s.session_by_name("collision").is_none()),
                    "a duplicate pending nonce must not create a session whose result cannot be correlated",
                );
                root_token.cancel();
            })
            .await;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod agent_drain_tests {
    use phux_protocol::ids::TerminalId as WireTerminalId;
    use phux_protocol::wire::frame::{Scope, TERMINAL_AGENT_KEY};

    use super::{retract_hook, spawn_agent_state_drain, state_change_hook};
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

    // --- agent-state-changed hook (the notification seam) -------------------

    fn terminal() -> WireTerminalId {
        WireTerminalId::local(1)
    }

    fn ctx(event: &crate::hooks::HookEvent, key: &str) -> Option<String> {
        event.context.get(key).cloned()
    }

    /// A first sighting has no `from`: "we have never seen this pane" is a
    /// different fact from "it was idle", and a notifier that conflates them
    /// announces every agent launch as a transition.
    #[test]
    fn first_sighting_reports_no_prior_state() {
        let event = state_change_hook(&terminal(), "claude", "claude", None, "working")
            .expect("a first sighting is an edge");
        assert_eq!(event.name, crate::hooks::AGENT_STATE_CHANGED);
        assert_eq!(ctx(&event, "from"), None);
        assert_eq!(ctx(&event, "to").as_deref(), Some("working"));
        assert_eq!(ctx(&event, "agent-kind").as_deref(), Some("claude"));
    }

    /// The transition the whole feature exists for: an agent that stopped and
    /// wants a human. The hook must carry both ends so a `when` clause can
    /// fire on `blocked` alone.
    #[test]
    fn working_to_blocked_carries_both_ends() {
        let event = state_change_hook(&terminal(), "claude", "rev", Some("working"), "blocked")
            .expect("working -> blocked is an edge");
        assert_eq!(ctx(&event, "from").as_deref(), Some("working"));
        assert_eq!(ctx(&event, "to").as_deref(), Some("blocked"));
        assert_eq!(ctx(&event, "agent-name").as_deref(), Some("rev"));
    }

    /// The detector's edge filter models its own emissions, not the store, so
    /// a republish can land on the state already recorded. That is not an
    /// edge, and firing there is how a notifier earns being turned off.
    #[test]
    fn republishing_the_stored_state_fires_nothing() {
        assert!(
            state_change_hook(&terminal(), "claude", "claude", Some("idle"), "idle").is_none(),
            "idle -> idle is not a transition"
        );
    }

    /// An anonymous record must not export an empty `agent-name`: a hook
    /// child reading `PHUX_AGENT_NAME=""` cannot tell "unnamed" from "unset".
    #[test]
    fn empty_agent_name_is_omitted_rather_than_blank() {
        let event = state_change_hook(&terminal(), "codex", "", None, "working")
            .expect("a first sighting is an edge");
        assert_eq!(ctx(&event, "agent-name"), None);
    }

    /// A withdrawn record is an edge too — the agent exited, and a fleet view
    /// that never hears about it keeps showing a dead pane as working.
    #[test]
    fn retract_reports_the_unknown_landing_state() {
        let event = retract_hook(&terminal(), Some("working")).expect("a retract is an edge");
        assert_eq!(ctx(&event, "from").as_deref(), Some("working"));
        assert_eq!(
            ctx(&event, "to").as_deref(),
            Some(crate::hooks::AGENT_STATE_UNKNOWN)
        );
    }

    /// Retracting an already-withdrawn record changes nothing and owes no
    /// hook, or a flapping detector becomes a stream of duplicate alerts.
    #[test]
    fn retracting_an_already_unknown_record_fires_nothing() {
        assert!(
            retract_hook(&terminal(), Some(crate::hooks::AGENT_STATE_UNKNOWN)).is_none(),
            "unknown -> unknown is not a transition"
        );
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
