//! Viewport resize, attached input, replies, and frame acks.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;

/// A `SessionSnapshot` describing a server with no sessions: empty lists,
/// sentinel focus ids. Used by `GET_STATE` when the registry is empty.
pub(crate) const fn empty_session_snapshot() -> phux_protocol::wire::info::SessionSnapshot {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    phux_protocol::wire::info::SessionSnapshot::new(
        SessionId::new(0),
        WindowId::new(0),
        ResourceId::local(0),
    )
}

/// Record a session-attached client's viewport vote and recompute every
/// subscribed Terminal in that session under the server's geometry policy.
pub(crate) fn handle_viewport_resize(
    state: &SharedState,
    client_id: ClientId,
    viewport: &ViewportInfo,
) {
    state.with_mut(|s| {
        if !s.attached().contains_key(&client_id) {
            debug!(
                ?client_id,
                "VIEWPORT_RESIZE from non-attached client; ignoring"
            );
            return;
        }
        s.set_client_viewport(client_id, *viewport);
        // A degenerate report withdraws the vote, but must not itself resize
        // a pane from another subscriber's vote or a headless fallback.
        if viewport.cols == 0 || viewport.rows == 0 {
            return;
        }
        for terminal_id in s.viewport_terminals(client_id) {
            if s.retained_exit(terminal_id).is_some() {
                continue;
            }
            let Some((cols, rows)) = s.resolve_terminal_geometry(terminal_id, Some(*viewport))
            else {
                continue;
            };
            let cell_px = s.resolve_terminal_cell_px(terminal_id);
            let Some(Ok(terminal)) = s.resource_handle(terminal_id).map(ResourceHandle::terminal)
            else {
                continue;
            };
            if try_resize(
                terminal,
                (cols, rows),
                cell_px,
                "VIEWPORT_RESIZE",
                client_id,
            ) && let Some(pane) = s.registry_mut().terminal_mut(terminal_id)
            {
                pane.dims = (cols, rows);
            }
        }
    });
}

/// The satellite branch of [`handle_terminal_input`]: gate on the caller's
/// proxy attach and the hub-side lease, then forward the frame with a
/// satellite-local id; warn-drop without a route.
fn relay_satellite_input(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    route: &RelayRoute,
    input: TerminalInput,
    frame_label: &'static str,
) {
    if !state.with(|s| s.has_satellite_proxy_attach(client_id, &route.host, route.id)) {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "satellite-routed input requires this client's ATTACH_RESOURCE proxy; dropping",
        );
        return;
    }
    // L1 §9.1: the satellite cannot tell hub consumers apart.
    if state.with(|s| {
        s.satellite_lease_holder(&route.host, route.id)
            .is_some_and(|holder| holder != client_id)
    }) {
        trace!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "satellite-routed input dropped: another hub consumer holds the input lease",
        );
        return;
    }
    let relayed =
        relay_satellite_frame(
            client_id,
            wire_terminal_id,
            route,
            frame_label,
            |id| match input {
                TerminalInput::Key(event) => FrameKind::InputKey {
                    terminal_id: id,
                    event,
                },
                TerminalInput::Mouse(event) => FrameKind::InputMouse {
                    terminal_id: id,
                    event,
                },
                TerminalInput::Focus(event) => FrameKind::InputFocus {
                    terminal_id: id,
                    event,
                },
                TerminalInput::Paste(event) => FrameKind::InputPaste {
                    terminal_id: id,
                    event,
                },
            },
        );
    if !relayed {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            frame_label,
            "input frame carried a SATELLITE ResourceId on a non-federation-hub server; dropping",
        );
    }
}

/// Apply subscription, lease, and activity gates for attached local input,
/// then run `action` while the authority lock is held. Callers relay
/// satellite ids before reaching here.
pub(crate) fn with_attached_input_destination<R>(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    frame_label: &'static str,
    action: impl FnOnce(InputDestination) -> R,
) -> Option<R> {
    state.with_mut(|s| {
        // workload-auth §7 step 2.
        if s.connection_revoked(client_id) {
            trace!(
                ?client_id,
                frame_label, "input from a revoked connection dropped"
            );
            return None;
        }
        let local = match s.resolve_resource(wire_terminal_id).into_owned() {
            ResolvedOwned::Local(local) => local,
            ResolvedOwned::Remote(_) | ResolvedOwned::Unknown => {
                warn!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "input frame for unknown pane; dropping"
                );
                return None;
            }
        };
        let pane = local.id;
        if s.native_owned_terminal(pane) {
            let error = Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InputNotWritten,
                message: "SDK-owned guardian refuses terminal input; use native agent commands"
                    .to_owned(),
            });
            if let Some(out) = s.client_mailbox(client_id) {
                let _ = out.try_send(error);
            }
            return None;
        }
        if s.retained_exit(pane).is_some() {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "input dropped: the pane's process exited (retained read-only)"
            );
            return None;
        }
        if !s.subscribers_for_terminal(pane).contains(&client_id) {
            warn!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "client not subscribed to pane (no ATTACH or ATTACH_RESOURCE); dropping input"
            );
            return None;
        }
        if s.input_blocked(pane, client_id) {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                frame_label,
                "input dropped: another client holds the input lease"
            );
            return None;
        }
        let touched_session = s.attached().get(&client_id).map(|c| c.session);
        if let Some(session) = touched_session {
            s.touch_session(session);
        }
        // R14: the member that last took human input.
        if let Some(session) = s.registry().session_of(pane) {
            let _ = s.registry_mut().set_recent(session, Some(pane));
        }
        let handle = match local.handle.terminal() {
            Ok(terminal) => terminal.clone(),
            Err(error) => {
                warn!(?client_id, ?wire_terminal_id, frame_label, %error, "dropping input");
                // input.md §9: no reply frame, so an uncorrelated ERROR.
                send_wrong_kind_error(s, client_id, error);
                return None;
            }
        };
        Some(action(InputDestination { pane, handle }))
    })
}

pub(crate) fn handle_terminal_input(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    input: TerminalInput,
    frame_label: &'static str,
) {
    if let ResolvedOwned::Remote(route) =
        state.with(|s| s.resolve_resource(wire_terminal_id).into_owned())
    {
        relay_satellite_input(
            state,
            client_id,
            wire_terminal_id,
            &route,
            input,
            frame_label,
        );
        return;
    }
    // A routed focus-gained fires `focus-changed` (tui.md §9) after the lock.
    let is_focus_gained = matches!(
        input,
        TerminalInput::Focus(phux_protocol::input::focus::FocusEvent::Gained)
    );
    let Some(routed) = with_attached_input_destination(
        state,
        client_id,
        wire_terminal_id,
        frame_label,
        |destination| match destination.handle.input.try_send(input) {
            Ok(()) => {
                trace!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "input routed to TerminalActor"
                );
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "pane input mailbox full; dropping (fire-and-forget per SPEC §9)"
                );
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                debug!(
                    ?client_id,
                    ?wire_terminal_id,
                    frame_label,
                    "pane actor gone; dropping input"
                );
                false
            }
        },
    ) else {
        return;
    };
    if routed && is_focus_gained {
        crate::hooks::fire_hook(
            state,
            crate::hooks::HookEvent::focus_changed(wire_terminal_id, client_id),
        );
    }
}

/// Discard a client terminal-engine reply (input.md §6): the canonical
/// terminal already answered the query once, and a second answer reaches
/// the child as typed input.
pub(crate) fn handle_terminal_reply(
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    bytes: &[u8],
) {
    trace!(
        ?client_id,
        ?wire_terminal_id,
        bytes = bytes.len(),
        "terminal reply discarded: the canonical terminal answers queries",
    );
}

/// Route an inbound `FRAME_ACK` to the pane's actor for per-consumer cache
/// eviction (ADR-0018). Acks are hints, so an unknown pane, a
/// non-subscriber, or a full mailbox just drops it; satellite acks relay.
pub(crate) fn handle_frame_ack(
    state: &SharedState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    seq: u64,
) {
    state.with_mut(|s| {
        let local = match s.resolve_resource(wire_terminal_id).into_owned() {
            ResolvedOwned::Remote(route) => {
                relay_frame_ack(
                    client_id,
                    wire_terminal_id,
                    &route,
                    stream_id,
                    bootstrap_id,
                    seq,
                );
                return;
            }
            ResolvedOwned::Unknown => {
                warn!(
                    ?client_id,
                    ?wire_terminal_id,
                    seq,
                    "FRAME_ACK for unknown pane; dropping",
                );
                return;
            }
            ResolvedOwned::Local(local) => local,
        };
        if !frame_ack_subscribed(s, client_id, wire_terminal_id, local.id, seq) {
            return;
        }
        let dispatched = local.handle.consumer_ack.try_send(ConsumerAckRequest {
            client_id: super::super::wire_client(client_id),
            stream_id,
            bootstrap_id,
            seq,
        });
        log_frame_ack_dispatch(&dispatched, client_id, wire_terminal_id, seq);
    });
}

/// Forward a satellite-routed `FRAME_ACK` over the hub link, warn-dropping it
/// on a server that is not a federation hub for that host.
fn relay_frame_ack(
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    route: &RelayRoute,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    seq: u64,
) {
    let relayed = relay_satellite_frame(client_id, wire_terminal_id, route, "FRAME_ACK", |id| {
        FrameKind::FrameAck {
            terminal_id: id,
            stream_id,
            bootstrap_id,
            seq,
        }
    });
    if !relayed {
        warn!(
            ?client_id,
            ?wire_terminal_id,
            seq,
            "FRAME_ACK carried a SATELLITE ResourceId on a non-federation-hub server; dropping",
        );
    }
}

/// Whether `client_id` subscribes to `pane` and so may ack it.
fn frame_ack_subscribed(
    s: &crate::state::ServerState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    pane: phux_core::ids::ResourceId,
    seq: u64,
) -> bool {
    if s.subscribers_for_terminal(pane).contains(&client_id) {
        return true;
    }
    warn!(
        ?client_id,
        ?wire_terminal_id,
        seq,
        "FRAME_ACK from client not subscribed to pane; dropping",
    );
    false
}

/// Log the outcome of routing one `FRAME_ACK` to its pane actor.
fn log_frame_ack_dispatch(
    dispatched: &Result<(), tokio::sync::mpsc::error::TrySendError<ConsumerAckRequest>>,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    seq: u64,
) {
    match dispatched {
        Ok(()) => {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK routed to TerminalActor"
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            trace!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK mailbox full; dropping (ADR-0018: next ack catches up)",
            );
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            debug!(
                ?client_id,
                ?wire_terminal_id,
                seq,
                "FRAME_ACK: pane actor gone; dropping",
            );
        }
    }
}
