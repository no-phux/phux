//! Viewport resize, attached input, replies, and frame acks.
#![allow(
    clippy::wildcard_imports,
    reason = "the split command modules share the parent command namespace"
)]

use super::*;
use crate::runtime::connections::resource_frames::attached_input_resource;
use phux_terminal_api::actor::TerminalResource as _;

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
        refit_client_terminals(s, client_id, Some(*viewport), "VIEWPORT_RESIZE");
    });
}

/// `client_id` used one of its panes. Under the `active` window-size policy
/// a view that wasn't already the last one used takes its panes back to
/// its own size; otherwise nothing happens.
pub(crate) fn handle_client_activity(state: &SharedState, client_id: ClientId) {
    state.with_mut(|s| {
        if s.note_client_activity(client_id) {
            refit_client_terminals(s, client_id, None, "ACTIVE_VIEW");
        }
    });
}

/// Recompute every Terminal `client_id` sizes under the window-size policy;
/// `latest` is the viewport that just changed, for the `latest` policy.
fn refit_client_terminals(
    s: &mut crate::ServerState,
    client_id: ClientId,
    latest: Option<ViewportInfo>,
    label: &'static str,
) {
    for terminal_id in s.viewport_terminals(client_id) {
        if s.retained_exit(terminal_id).is_some() {
            continue;
        }
        let Some((cols, rows)) = s.resolve_terminal_geometry(terminal_id, latest) else {
            continue;
        };
        let cell_px = s.resolve_terminal_cell_px(terminal_id);
        let Some(Ok(terminal)) = s
            .resource_handle(terminal_id)
            .map(|handle| handle.terminal())
        else {
            continue;
        };
        if try_resize(terminal, (cols, rows), cell_px, label, client_id)
            && let Some(pane) = s.registry_mut().terminal_mut(terminal_id)
        {
            pane.dims = (cols, rows);
        }
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
        // The host checked before dispatch; the input lane may have waited.
        // Recheck the same authority under the lock held through delivery.
        let (pane, resource) = attached_input_resource(s, client_id, wire_terminal_id)?;
        let touched_session = s.attached().get(&client_id).map(|c| c.session);
        if let Some(session) = touched_session {
            s.touch_session(session);
        }
        // R14: the member that last took human input.
        if let Some(session) = s.registry().session_of(pane) {
            let _ = s.registry_mut().set_recent(session, Some(pane));
        }
        let handle = match resource.terminal() {
            Ok(terminal) => terminal.clone(),
            Err(error) => {
                warn!(?client_id, ?wire_terminal_id, frame_label, %error, "dropping input");
                // docs/protocol.md#input: no reply frame, so an uncorrelated ERROR.
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
    // A routed focus-gained fires `focus-changed` (docs/hooks.md) after the lock.
    let is_focus_gained = matches!(
        input,
        TerminalInput::Focus(phux_protocol::input::focus::FocusEvent::Gained)
    );
    let Some(routed) = with_attached_input_destination(
        state,
        client_id,
        wire_terminal_id,
        frame_label,
        |destination| {
            dispatch_input(
                &destination,
                input,
                client_id,
                wire_terminal_id,
                frame_label,
            )
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

fn dispatch_input(
    destination: &InputDestination,
    input: TerminalInput,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    frame_label: &'static str,
) -> bool {
    match destination.handle.input.try_send(input) {
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
    }
}

/// Discard a client terminal-engine reply (docs/protocol.md#terminal-queries): the canonical
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
/// non-subscriber, or a full mailbox just drops it. The host relays satellites.
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
            ResolvedOwned::Remote(_) | ResolvedOwned::Unknown => {
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
        let Ok(terminal) = local.handle.terminal() else {
            return;
        };
        let dispatched = terminal.consumer_ack.try_send(ConsumerAckRequest {
            client_id: crate::runtime::wire_client(client_id),
            stream_id,
            bootstrap_id,
            seq,
        });
        log_frame_ack_dispatch(&dispatched, client_id, wire_terminal_id, seq);
    });
}

/// Whether `client_id` subscribes to `pane` and so may ack it.
fn frame_ack_subscribed(
    s: &crate::state::ServerState,
    client_id: ClientId,
    wire_terminal_id: &phux_protocol::ids::ResourceId,
    pane: phux_core::ids::ResourceId,
    seq: u64,
) -> bool {
    if s.subscribers_for_resource(pane).contains(&client_id) {
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
