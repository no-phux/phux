//! The pane exit lifecycle: the exit watcher, retention and reaping, cascading
//! closes to children, and the `RESOURCE_CLOSED` broadcast.

use crate::runtime::apps::terminal::agent_state::drain_retract;

use super::detach_clients_of_killed_session;
use crate::hooks::HookEvent;
use phux_core::ids::ResourceId as CoreResourceId;
use phux_core::process::ExitOutcome;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::Scope;
use tokio_util::sync::CancellationToken;

use phux_protocol::wire::frame::{AgentEvent, CloseReason, FrameKind};
use tokio::sync::{mpsc, oneshot};

use tracing::{debug, info};

use crate::state::{ClientId, Outbound, ServerState, SharedState};

use super::pane_events::{
    PaneEvents, journal_drained_event, journal_pending_events, journal_source_gap,
    journal_until_exit, spawn_pane_event_drain,
};

/// Spawn the per-pane exit watcher: on PTY EOF it journals and broadcasts
/// `RESOURCE_CLOSED` to every subscriber, then reaps the pane. Detaching is
/// the consumer's decision (ADR-0015). A dropped `exit_notify` sender is
/// treated as an exit with unknown status. The watcher is also the pane's
/// event drain (ADR-0123), so every event is journaled before `pane_closed`.
pub(crate) fn spawn_resource_exit_watcher(
    state: SharedState,
    pane: CoreResourceId,
    exit_notify: Option<oneshot::Receiver<ExitOutcome>>,
    root_token: CancellationToken,
    events: Option<PaneEvents>,
) {
    let Some(rx) = exit_notify else {
        if let Some(events) = events {
            spawn_pane_event_drain(state, events);
        }
        return;
    };
    state.with_mut(|s| s.begin_exit_watch(pane));
    tokio::task::spawn_local(async move {
        let (exit, mut events) = journal_until_exit(&state, rx, events).await;
        crate::runtime::apps::resource_exited(&state, pane).await;
        // ADR-0124: a retained pane stays `Exited` until purged or expired,
        // then takes the ordinary close path below.
        let retained = state.with_mut(|s| retain_exited_pane(s, pane, exit, events.as_mut()));
        let exit_hook_owed = retained.is_none();
        if let Some(RetainedExit {
            retention,
            wire_terminal_id,
            agent_hook,
            control,
        }) = retained
        {
            // Awaited so a full mailbox delays the retire instead of dropping it.
            if let Some(control) = control {
                let _ = control.send(crate::resource::ControlRequest::Retire).await;
            }
            fire_retained_exit_hooks(&state, &wire_terminal_id, agent_hook, exit);
            events = hold_until_purge(&state, &retention, events).await;
        }
        let reap = state.with_mut(|s| reap_exited_pane(s, pane, exit, events.as_mut()));
        let targets = if let Some(reap) = reap {
            announce_close(&state, reap, exit_hook_owed).await
        } else {
            fire_exit_hook_after_kill(&state, pane, exit, exit_hook_owed);
            Vec::new()
        };
        end_exit_watch(&state, pane, &root_token, &targets).await;
    });
}

/// A kill reaped `pane` in its own commit (L1 §5.2) while the process ran
/// its hangup grace: fire the `pane-exit` its exit still owes, with the
/// status the process actually ended with. Nothing when another closer
/// reaped it, as before.
fn fire_exit_hook_after_kill(
    state: &SharedState,
    pane: CoreResourceId,
    exit: ExitOutcome,
    exit_hook_owed: bool,
) {
    let killed = state.with(|s| s.closed_before_exit(pane));
    if let Some(wire_terminal_id) = killed.filter(|_| exit_hook_owed) {
        crate::hooks::fire_hook(state, HookEvent::pane_exit(&wire_terminal_id, exit.status));
    }
}

/// End `pane`'s exit watch, and self-exit once nothing runs and no client
/// is connected (R13) and no killed process is still in its hangup grace,
/// but only after serving a client (a fresh auto-spawned server's launcher
/// is still connecting) and not during an ordinary shutdown.
async fn end_exit_watch(
    state: &SharedState,
    pane: CoreResourceId,
    root_token: &CancellationToken,
    targets: &[mpsc::Sender<Outbound>],
) {
    let due = state.with_mut(|s| {
        s.end_exit_watch(pane);
        s.self_exit_due()
    });
    if !due || root_token.is_cancelled() {
        return;
    }
    // Let writers flush `RESOURCE_CLOSED` before the root cancel closes
    // their transports.
    yield_until_close_frames_taken(targets).await;
    info!("nothing left running after serving clients; server self-exit");
    root_token.cancel();
}

/// A pane kept as `Exited` after its process exited (ADR-0124), as its exit
/// watcher holds it until the purge.
struct RetainedExit {
    /// What a purge cancels, and when retention expires.
    retention: crate::state::Retention,
    /// The pane's wire id, for the `pane-exit` hook.
    wire_terminal_id: WireResourceId,
    /// The `agent-state-changed` hook the withdrawn agent record owes.
    agent_hook: Option<HookEvent>,
    /// The engine's control mailbox, for the `Retire` sent off the lock.
    control: Option<tokio::sync::mpsc::Sender<crate::resource::ControlRequest>>,
}

/// Keep `pane` as `Exited` when it asked to be retained and nothing is
/// already closing it (ADR-0124 §2), all in one lock: record its exit facet,
/// journal what it already queued and then `terminal_control { Exited }`,
/// withdraw its `phux.agent/v1` state to `unknown` (L3 §3.7), and tell its
/// engine to stop the detector and refuse input. No `RESOURCE_CLOSED`: the
/// resource is still here. `None` leaves the pane to the close path.
fn retain_exited_pane(
    s: &mut ServerState,
    pane: CoreResourceId,
    exit: ExitOutcome,
    events: Option<&mut PaneEvents>,
) -> Option<RetainedExit> {
    let retention = s.retain_exited(pane, exit, crate::state::now_unix_ms())?;
    // The record keeps its outcome (`ExitPolicy::Retain`).
    s.activation_ended(pane, exit, false);
    let wire_terminal_id = s.intern_resource_wire(pane);
    if let Some(events) = events {
        journal_pending_events(s, events);
    }
    let input_holder = s.input_lease_holder(pane).map(crate::runtime::wire_client);
    let exited = AgentEvent::TerminalControl {
        lifecycle: phux_protocol::wire::frame::ResourceLifecycle::Exited,
        exit_status: exit.status,
        input_holder,
        action: phux_protocol::wire::frame::ControlAction::Exited,
        actor: None,
    };
    let _ = s.record_and_fanout(crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        exited,
    ));
    let control = s.resource_handle(pane).map(|handle| handle.control.clone());
    let scope = Scope::Resource(wire_terminal_id.clone());
    let hooks_live = s.hook_dispatcher().is_some();
    let agent_hook = drain_retract(s, &wire_terminal_id, &scope, hooks_live);
    Some(RetainedExit {
        retention,
        wire_terminal_id,
        agent_hook,
        control,
    })
}

/// The hooks a retained exit owes, fired off-lock: `pane-exit` now, because
/// the process is what exited (its purge fires none), and the withdrawn agent
/// record's `agent-state-changed`.
fn fire_retained_exit_hooks(
    state: &SharedState,
    wire_terminal_id: &WireResourceId,
    agent_hook: Option<HookEvent>,
    exit: ExitOutcome,
) {
    crate::hooks::fire_hook(state, HookEvent::pane_exit(wire_terminal_id, exit.status));
    if let Some(event) = agent_hook {
        crate::hooks::fire_hook(state, event);
    }
}

/// Hold a retained pane until its purge (ADR-0124 §4): a kill, an eviction,
/// or a shutdown cancels its engine token, or its retention expires. Its
/// events are still journaled meanwhile (a lease change on an exited pane is
/// still an event). Returns the event source for the close to drain.
async fn hold_until_purge(
    state: &SharedState,
    retention: &crate::state::Retention,
    mut events: Option<PaneEvents>,
) -> Option<PaneEvents> {
    let expiry = tokio::time::sleep(retention.hold);
    tokio::pin!(expiry);
    loop {
        tokio::select! {
            biased;
            () = retention.token.cancelled() => return events,
            () = &mut expiry => return events,
            event = next_pane_event(&mut events) => journal_held_event(state, &mut events, event),
        }
    }
}

/// The next event a pane's engine emits; pending forever once the source is
/// gone, so a held pane waits only on its purge.
async fn next_pane_event(
    events: &mut Option<PaneEvents>,
) -> Option<crate::resource::event_sink::Emitted> {
    match events.as_mut() {
        Some(events) => events.source.recv().await,
        None => std::future::pending().await,
    }
}

/// Journal one event a held pane emitted; a closed source journals its last
/// loss and is dropped.
fn journal_held_event(
    state: &SharedState,
    events: &mut Option<PaneEvents>,
    event: Option<crate::resource::event_sink::Emitted>,
) {
    let Some(pane_events) = events.as_mut() else {
        return;
    };
    let dropped = pane_events.source.take_dropped();
    let wire = pane_events.wire.clone();
    if let Some(event) = event {
        state.with_mut(|s| journal_drained_event(s, &wire, event, dropped));
        return;
    }
    state.with_mut(|s| journal_source_gap(s, &wire, dropped));
    *events = None;
}

/// Close a pane whose process exited (or whose retention ended) in one
/// critical section that also gathers who must be told, so no ATTACH can
/// interleave between gathering subscribers and the reap. `None` when
/// another closer already reaped the pane.
fn reap_exited_pane(
    s: &mut ServerState,
    pane: CoreResourceId,
    exit: ExitOutcome,
    events: Option<&mut PaneEvents>,
) -> Option<ReapAndNotify> {
    // ADR-0104 §2: a cascading parent may already have reaped this child.
    let reason = s.begin_resource_close(pane)?;
    // ADR-0124: a retained pane's close reports the exit it was kept with.
    let exit = s.retained_outcome(pane).unwrap_or(exit);
    // Interned before the cascade and the reap, which retires the wire id.
    let wire_terminal_id = s.intern_resource_wire(pane);
    if let Some(events) = events {
        journal_pending_events(s, events);
    }
    let parent = s
        .resource_parent(pane)
        .map(|parent| s.intern_resource_wire(parent));
    let cascaded = cascade_children(s, pane, &wire_terminal_id);
    // Every subscriber, including `ATTACH_RESOURCE`-only ones (L1 §3.1).
    let targets: Vec<mpsc::Sender<Outbound>> = s.terminal_fanout_targets(pane);
    let drains = s.begin_terminal_output_drain(pane, wire_terminal_id.clone());
    // ADR-0123: `pane_closed` is journaled in the lock that removes the pane.
    let attribution = s.take_close_attribution(pane);
    journal_pane_closed(
        s,
        &wire_terminal_id,
        parent.as_ref(),
        exit.status,
        attribution,
    );
    let preserve_record = matches!(reason, CloseReason::Exited | CloseReason::ServerShutdown)
        && s.record_survives_exit(pane);
    let (_, actor_token) = s.reap_terminal_deferring_actor_cancel(pane, exit, preserve_record);
    // ADR-0105: clients of a session a group kill released are detached.
    let killed_clients: Vec<_> = s
        .take_killed_sessions()
        .into_iter()
        .flat_map(|session| s.attached_clients_in_session(session))
        .collect();
    Some(ReapAndNotify {
        core_terminal_id: pane,
        wire_terminal_id,
        reason,
        exit,
        cascaded,
        targets,
        drains,
        killed_clients,
        actor_token,
    })
}

/// Close every descendant bound to `pane` (ADR-0104 §2) in the parent's
/// lock, before the parent is reaped, so no client observes a child whose
/// parent has left. Each descendant's frame is emitted by the parent's
/// watcher; its own watcher later finds it already claimed.
fn cascade_children(
    s: &mut ServerState,
    pane: CoreResourceId,
    wire_terminal_id: &WireResourceId,
) -> Vec<CascadedClose> {
    // Deepest first, so no descendant leaves the registry unjournaled.
    let descendants = s.resource_descendants(pane);
    let mut cascaded = Vec::new();
    for child in descendants.into_iter().rev() {
        let Some(recorded) = s.begin_resource_close(child) else {
            continue;
        };
        // An unmarked child leaves because its parent is; one named in the
        // same kill keeps that kill's reason.
        let child_reason = if recorded == CloseReason::Exited {
            CloseReason::ParentClosed
        } else {
            recorded
        };
        let wire_child_id = s.intern_resource_wire(child);
        let child_targets = s.terminal_fanout_targets(child);
        let child_exit = s.retained_outcome(child).unwrap_or(ExitOutcome::UNKNOWN);
        let attribution = s.take_close_attribution(child);
        journal_pane_closed(
            s,
            &wire_child_id,
            Some(wire_terminal_id),
            child_exit.status,
            attribution,
        );
        s.cancel_terminal_pumps(child);
        s.reap_resource(child);
        cascaded.push(CascadedClose {
            wire_terminal_id: wire_child_id,
            targets: child_targets,
            reason: child_reason,
            exit: child_exit,
        });
    }
    cascaded
}

/// The off-lock half of a close: the `pane-exit` hook when the exit was not
/// already announced, `RESOURCE_CLOSED` (children first), and the detach of
/// a released keep-empty session's clients. Returns every mailbox a close
/// went to, for the phux-60s self-exit to wait on.
async fn announce_close(
    state: &SharedState,
    reap: ReapAndNotify,
    exit_hook_owed: bool,
) -> Vec<mpsc::Sender<Outbound>> {
    let ReapAndNotify {
        core_terminal_id,
        wire_terminal_id,
        reason,
        exit,
        cascaded,
        mut targets,
        drains,
        killed_clients,
        actor_token,
    } = reap;
    // A retained pane already fired `pane-exit` when its process exited.
    if exit_hook_owed {
        crate::hooks::fire_hook(state, HookEvent::pane_exit(&wire_terminal_id, exit.status));
    }

    // Children first: the order the tree came apart in.
    for child in &cascaded {
        broadcast_resource_closed(
            &child.wire_terminal_id,
            &child.targets,
            child.exit,
            child.reason,
        )
        .await;
    }
    broadcast_terminal_closed_after_drain(&wire_terminal_id, &targets, exit, reason, &drains).await;
    state.with_mut(|s| s.finish_terminal_output_drain(core_terminal_id));
    if let Some(token) = actor_token {
        token.cancel();
    }

    // ADR-0105: after the closes, so a client sees its last pane go before
    // the session that held it.
    detach_clients_of_killed_session(state, killed_clients);

    for child in cascaded {
        targets.extend(child.targets);
    }
    targets
}

/// Yield until each client writer has taken the just-queued close frames,
/// or a small turn budget expires.
async fn yield_until_close_frames_taken(targets: &[mpsc::Sender<Outbound>]) {
    for _ in 0..256 {
        if targets
            .iter()
            .all(|tx| tx.is_closed() || tx.capacity() == tx.max_capacity())
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    // A few more turns for the write and flush.
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// Everything the exit watcher captures under the reap lock for the
/// off-lock `RESOURCE_CLOSED` fanout.
struct ReapAndNotify {
    core_terminal_id: CoreResourceId,
    /// The pane's wire id, interned before the reap retired it.
    wire_terminal_id: WireResourceId,
    /// Why this pane is closing (ADR-0104 §4).
    reason: CloseReason,
    exit: ExitOutcome,
    /// Children already reaped, waiting only for their frames.
    cascaded: Vec<CascadedClose>,
    /// Every client subscribed to the pane at reap time.
    targets: Vec<mpsc::Sender<Outbound>>,
    /// Each terminal pump finishes its final publication before its client closes.
    drains: Vec<(mpsc::Sender<Outbound>, Vec<CancellationToken>)>,
    /// ADR-0105: clients of a released session, detached with `SESSION_KILLED`.
    killed_clients: Vec<(ClientId, mpsc::Sender<Outbound>)>,
    /// Cancelled after `RESOURCE_CLOSED` is queued so a fenced pump can
    /// publish the last screen first.
    actor_token: Option<CancellationToken>,
}

/// One resource closed because its parent did (ADR-0104 §2), captured
/// under the parent's lock and broadcast off it.
struct CascadedClose {
    wire_terminal_id: WireResourceId,
    targets: Vec<mpsc::Sender<Outbound>>,
    reason: CloseReason,
    /// Unknown unless the child is a retained exited pane.
    exit: ExitOutcome,
}

/// Send `RESOURCE_CLOSED` to every mailbox in `targets`, gathered by the
/// caller under the reap lock. Best-effort: closed mailboxes are skipped.
pub(crate) async fn broadcast_resource_closed(
    wire_terminal_id: &WireResourceId,
    targets: &[mpsc::Sender<Outbound>],
    exit: ExitOutcome,
    reason: phux_protocol::wire::frame::CloseReason,
) {
    broadcast_terminal_closed_after_drain(wire_terminal_id, targets, exit, reason, &[]).await;
}

async fn broadcast_terminal_closed_after_drain(
    wire_terminal_id: &WireResourceId,
    targets: &[mpsc::Sender<Outbound>],
    exit: ExitOutcome,
    reason: CloseReason,
    drains: &[(mpsc::Sender<Outbound>, Vec<CancellationToken>)],
) {
    if targets.is_empty() {
        debug!("RESOURCE_CLOSED: no L1-subscribed clients to notify");
    } else {
        debug!(
            count = targets.len(),
            ?exit,
            "RESOURCE_CLOSED: broadcasting to subscribed clients",
        );
        // Concurrent, so one stalled subscriber cannot starve the rest.
        let sends = targets.iter().map(|tx| {
            let tx = tx.clone();
            let frame = Outbound::Frame(FrameKind::ResourceClosed {
                terminal_id: wire_terminal_id.clone(),
                exit_status: exit.status,
                reason,
                signal: exit.signal,
            });
            async move {
                for (_, done) in drains
                    .iter()
                    .filter(|(mailbox, _)| mailbox.same_channel(&tx))
                {
                    for token in done {
                        token.cancelled().await;
                    }
                }
                let _ = tx.send(frame).await;
            }
        });
        futures_util::future::join_all(sends).await;
    }
}

/// Journal a resource's `pane_closed` (ADR-0123) under the lock that reaps
/// it. A child's close names its parent (ADR-0104 §2).
pub(in crate::runtime) fn journal_pane_closed(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    parent: Option<&WireResourceId>,
    exit_status: Option<i32>,
    attribution: crate::state::CloseAttribution,
) {
    // Nothing follows a close (L1 §7): withdraw held actions first (ADR-0128).
    s.withdraw_approvals_naming(wire_terminal_id);
    let record = crate::state::EventRecord::new(
        Some(wire_terminal_id.clone()),
        AgentEvent::ResourceClosed { exit_status },
    )
    .with_parent(parent.cloned())
    .with_actor(attribution.actor)
    .with_operation_id(attribution.operation_id);
    let _ = s.record_and_fanout(record);
}

#[cfg(test)]
mod cascade_close_tests;

#[cfg(test)]
mod natural_close_tests;
