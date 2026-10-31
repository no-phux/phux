//! Pane event drains: journaling a pane's engine events (ADR-0123) and
//! folding its agent-state reports into the `phux.agent/v1` record.

use super::pane_exit::journal_pane_closed;
use phux_core::ids::ResourceId as CoreResourceId;
use phux_core::process::ExitOutcome;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::AgentEvent;
use tokio_util::sync::CancellationToken;

use tokio::sync::{mpsc, oneshot};

use crate::state::{ClientId, Outbound, ServerState, SharedState};

/// A pane's event source and the wire id its events are journaled under
/// (ADR-0123). Handed to the pane's exit watcher, which is its drain.
///
/// The engine's sink is the one place an event can be lost before the
/// journal. Each drop is counted there, and the drain journals the count as
/// a `source_gap` scoped to this pane right after the event it read, so a
/// full sink is a typed loss rather than silence.
pub(crate) struct PaneEvents {
    /// The pane's wire id, interned at spawn.
    pub(crate) wire: WireResourceId,
    /// The runtime end of the pane's event sink.
    pub(crate) source: crate::resource::event_sink::EventSource,
}

/// Drain a pane's events into the journal for as long as its sink lives:
/// the fallback for a pane with no exit notification to watch.
pub(super) fn spawn_pane_event_drain(state: SharedState, mut events: PaneEvents) {
    tokio::task::spawn_local(async move {
        while let Some(event) = events.source.recv().await {
            let dropped = events.source.take_dropped();
            state.with_mut(|s| journal_drained_event(s, &events.wire, event, dropped));
        }
        let dropped = events.source.take_dropped();
        state.with_mut(|s| journal_source_gap(s, &events.wire, dropped));
    });
}

/// Journal a pane's events, in emission order, until its exit fires.
/// Returns the exit status and, when the sink is still open, the source,
/// whose already-queued events the reap lock journals before the close.
pub(super) async fn journal_until_exit(
    state: &SharedState,
    mut exit: oneshot::Receiver<ExitOutcome>,
    events: Option<PaneEvents>,
) -> (ExitOutcome, Option<PaneEvents>) {
    let Some(mut events) = events else {
        return (exit.await.unwrap_or_default(), None);
    };
    loop {
        tokio::select! {
            biased;
            outcome = &mut exit => return (outcome.unwrap_or_default(), Some(events)),
            event = events.source.recv() => {
                let Some(event) = event else { break };
                let dropped = events.source.take_dropped();
                state.with_mut(|s| journal_drained_event(s, &events.wire, event, dropped));
            }
        }
    }
    let dropped = events.source.take_dropped();
    state.with_mut(|s| journal_source_gap(s, &events.wire, dropped));
    (exit.await.unwrap_or_default(), None)
}

/// Start the event pump for `client_id`'s subscription, once (ADR-0123):
/// a task that waits for room in the connection's mailbox and hands the
/// subscription whatever it is owed, a `journal_gap` or the rest of a
/// cursor replay, a frame at a time. It never runs inside frame dispatch,
/// so a consumer that stops reading delays only itself, and it ends with
/// the connection or the subscription.
pub(crate) fn ensure_event_pump(state: &SharedState, client_id: ClientId) {
    let Some(pump) = state.with_mut(|s| s.claim_event_pump(client_id)) else {
        return;
    };
    let cancel = state
        .with(|s| s.client_connection_cancellation(client_id))
        .unwrap_or_default();
    let state = state.clone();
    tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = pump.wake.notified() => {}
            }
            if !pump_owed_frames(&state, (client_id, pump.epoch), &pump.tx, &cancel).await {
                return;
            }
        }
    });
}

/// Send subscription `epoch` of `client_id` its owed event frames as
/// mailbox room frees. `false` once the connection or that subscription is
/// gone, including when a re-subscribe replaced it.
async fn pump_owed_frames(
    state: &SharedState,
    (client_id, epoch): (ClientId, u64),
    tx: &mpsc::Sender<Outbound>,
    cancel: &CancellationToken,
) -> bool {
    loop {
        let permit = tokio::select! {
            () = cancel.cancelled() => return false,
            permit = tx.reserve() => match permit {
                Ok(permit) => permit,
                Err(_) => return false,
            },
        };
        match state.with_mut(|s| s.next_owed_event_frame(client_id, epoch)) {
            crate::state::PumpStep::Frame(frame) => permit.send(Outbound::Frame(frame)),
            crate::state::PumpStep::Idle => return true,
            crate::state::PumpStep::Gone => return false,
        }
    }
}

/// Reap a pane whose publication failed after its `pane_spawned` was
/// journaled (the spawner vanished, a preflight or send failed, its
/// generation was lost): journal its `pane_closed` in the same lock, once
/// (ADR-0123). The pane's exit watcher then finds it gone and journals
/// nothing more, so every `pane_spawned` is followed by exactly one
/// `pane_closed`. Returns what [`ServerState::reap_resource`]
/// returns; `false` without journaling when the pane is already gone.
pub(crate) fn reap_pane_journaling_close(s: &mut ServerState, pane: CoreResourceId) -> bool {
    if s.registry().resource(pane).is_none() {
        return false;
    }
    let wire = s.intern_resource_wire(pane);
    let parent = s
        .resource_parent(pane)
        .map(|parent| s.intern_resource_wire(parent));
    journal_pane_closed(
        s,
        &wire,
        parent.as_ref(),
        None,
        crate::state::CloseAttribution::default(),
    );
    s.reap_resource(pane)
}

/// Journal every event a closing pane already queued, and any loss its
/// sink counted, so all of them take a `seq` before its `pane_closed`.
pub(super) fn journal_pending_events(s: &mut ServerState, events: &mut PaneEvents) {
    while let Some(event) = events.source.try_recv() {
        journal_drained_event(s, &events.wire, event, 0);
    }
    journal_source_gap(s, &events.wire, events.source.take_dropped());
}

/// Journal one event a pane's engine emitted, then any loss its sink
/// counted since the previous one. Nothing once the pane is gone: a pane
/// reaped by a failed publication or its parent's cascade already has its
/// `pane_closed`, and nothing about it may follow.
pub(super) fn journal_drained_event(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    emitted: crate::resource::event_sink::Emitted,
    dropped: u64,
) {
    if !pane_is_live(s, wire_terminal_id) {
        return;
    }
    let crate::resource::event_sink::Emitted {
        event,
        operation_id,
    } = emitted;
    // The registry's cwd is what GET_STATE reports; a client that joins or
    // refreshes after this event must see the move, not the ATTACH-time dir.
    if let AgentEvent::CwdChanged { cwd } = &event
        && let Some(pane) = s.resource_from_wire(wire_terminal_id)
        && let Some(desc) = s.registry_mut().terminal_mut(pane)
    {
        desc.cwd = std::path::PathBuf::from(cwd);
    }
    let actor = control_actor(&event);
    let record = crate::state::EventRecord::new(Some(wire_terminal_id.clone()), event)
        .with_actor(actor)
        .with_operation_id(operation_id);
    let _ = s.record_and_fanout(record);
    journal_source_gap(s, wire_terminal_id, dropped);
}

/// Journal `source_gap { dropped }` for a pane, when anything was dropped.
pub(super) fn journal_source_gap(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    dropped: u64,
) {
    if dropped == 0 || !pane_is_live(s, wire_terminal_id) {
        return;
    }
    let gap = AgentEvent::SourceGap { dropped };
    let _ = s.record_and_fanout(crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        gap,
    ));
}

/// Whether the pane `wire_terminal_id` names is still registered. A reap
/// retires the wire id, so a reaped pane no longer resolves.
fn pane_is_live(s: &ServerState, wire_terminal_id: &WireResourceId) -> bool {
    s.resource_from_wire(wire_terminal_id)
        .is_some_and(|pane| s.registry().resource(pane).is_some())
}

/// The connection an engine-emitted event names as its cause: the `actor`
/// of a supervisory `terminal_control` (a take, a give, a signal). Every
/// other engine event is server-driven.
fn control_actor(event: &AgentEvent) -> Option<ClientId> {
    match event {
        AgentEvent::TerminalControl {
            actor: Some(actor), ..
        } => Some(ClientId(u64::from(actor.get()))),
        _ => None,
    }
}

#[cfg(test)]
mod pane_event_drain_tests;
