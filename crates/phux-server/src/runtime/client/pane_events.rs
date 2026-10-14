//! Pane event drains: journaling a pane's engine events (ADR-0123) and
//! folding its agent-state reports into the `phux.agent/v1` record.

use phux_core::ids::ResourceId as CoreResourceId;
use phux_core::process::ExitOutcome;
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::Scope;
use phux_protocol::wire::frame::{AgentEvent, RESOURCE_AGENT_KEY};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::hooks::HookEvent;
use crate::state::{ClientId, Outbound, ServerState, SharedState};

use super::broadcast_event;
use super::pane_exit::journal_pane_closed;

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
/// `pane_closed`. Returns what [`ServerState::reap_terminal`]
/// returns; `false` without journaling when the pane is already gone.
pub(crate) fn reap_pane_journaling_close(s: &mut ServerState, pane: CoreResourceId) -> bool {
    if s.registry().resource(pane).is_none() {
        return false;
    }
    let wire = s.intern_terminal_wire(pane);
    let parent = s
        .resource_parent(pane)
        .map(|parent| s.intern_terminal_wire(parent));
    journal_pane_closed(
        s,
        &wire,
        parent.as_ref(),
        None,
        crate::state::CloseAttribution::default(),
    );
    s.reap_terminal(pane)
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
    s.terminal_from_wire(wire_terminal_id)
        .is_some_and(|pane| s.registry().resource(pane).is_some())
}

/// Whether the pane `wire_terminal_id` names is retained after its process
/// exited (ADR-0124).
fn pane_exited(s: &ServerState, wire_terminal_id: &WireResourceId) -> bool {
    s.terminal_from_wire(wire_terminal_id)
        .is_some_and(|pane| s.retained_exit(pane).is_some())
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

/// Spawn the per-pane detector metadata drain (ADR-0046): the actor emits
/// edge-filtered [`AgentDetectEvent`](crate::agent_detect::AgentDetectEvent)s
/// and this task, which can reach `ServerState`, performs the authority check
/// and the metadata write.
pub(crate) fn spawn_agent_state_drain(
    state: SharedState,
    wire_terminal_id: WireResourceId,
    mut rx: tokio::sync::mpsc::Receiver<crate::agent_detect::AgentDetectEvent>,
) {
    use crate::agent_detect::AgentDetectEvent;

    tokio::task::spawn_local(async move {
        while let Some(event) = rx.recv().await {
            let _publish_span = tracing::debug_span!("agent_publish", ?wire_terminal_id,).entered();
            let _timer = crate::perf::AGENT_PUBLISH.timer();
            // Both the ask broadcast and the hook re-take the state lock, so
            // they are resolved under it and fired after it is released.
            let mut asked = None;
            let hook = state.with_mut(|s| {
                // ADR-0124: an exited pane's record was withdrawn; a report
                // queued before the detector stopped must not reassert it.
                if pane_exited(s, &wire_terminal_id) {
                    return None;
                }
                let scope = Scope::Resource(wire_terminal_id.clone());
                // Skip reading the prior record when no hook can run.
                let hooks_live = s.hook_dispatcher().is_some();
                match event {
                    AgentDetectEvent::Occupant(occupant) => {
                        if let Ok(bytes) = serde_json::to_vec(&occupant) {
                            s.metadata_set(
                                &scope,
                                phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY,
                                bytes,
                            );
                        }
                        None
                    }
                    AgentDetectEvent::Retract => {
                        drain_retract(s, &wire_terminal_id, &scope, hooks_live)
                    }
                    AgentDetectEvent::Reidentified { kind, name } => {
                        drain_reidentified(s, &wire_terminal_id, &scope, hooks_live, &kind, &name)
                    }
                    AgentDetectEvent::State(report) => {
                        drain_state(s, &wire_terminal_id, &scope, hooks_live, &report)
                    }
                    AgentDetectEvent::AskSentinel(ask) => {
                        asked = drain_ask_sentinel(s, &wire_terminal_id, ask);
                        None
                    }
                }
            });
            if let Some(payload) = asked {
                broadcast_event(&state, Some(&wire_terminal_id), &payload.into_event());
            }
            if let Some(event) = hook {
                crate::hooks::fire_hook(&state, event);
            }
        }
    });
}

/// The drain's `AskSentinel` arm: the pane's `phux-ask` title changed
/// (ADR-0036 tier 2). Runs the edge through the same arbiter `REPORT_ASKED`
/// uses and returns the payload to broadcast, if any. A cleared marker
/// retracts only a sentinel-owned ask and broadcasts nothing.
fn drain_ask_sentinel(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    ask: Option<crate::agent_asked::AskedPayload>,
) -> Option<crate::agent_asked::AskedPayload> {
    use crate::agent_asked::AskedSource;

    let terminal = s.terminal_from_wire(wire_terminal_id)?;
    let emitted = if let Some(payload) = ask {
        s.report_agent_asked(terminal, AskedSource::Sentinel, payload)
            .emit_payload()
    } else {
        s.retract_agent_asked(terminal, AskedSource::Sentinel);
        None
    };
    // ADR-0136: the metadata flag is how a consumer sees a clear.
    crate::hub::metadata_mirror::publish_asked_flag(
        s,
        wire_terminal_id,
        s.agent_is_asked(terminal),
    );
    emitted
}

/// The drain's `Retract` arm: the pane's agent is confirmed gone. A declared
/// record (L3 §3.7), or a detector record carrying a human's identity, is
/// withdrawn to `unknown` with its other fields kept; a record the detector
/// wrote alone is deleted; anything else is not ours.
pub(super) fn drain_retract(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    hooks_live: bool,
) -> Option<HookEvent> {
    if s.metadata().contains(
        scope,
        phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_KEY,
    ) {
        return None;
    }
    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    let prior = prior_state(hooks_live, existing.as_deref());
    if s.agent_records().is_declared(wire_terminal_id) {
        withdraw_declared(s, wire_terminal_id, scope, existing.as_deref())?;
        return retract_hook(wire_terminal_id, prior);
    }
    if !s.agent_records().detector_owns(wire_terminal_id) {
        return None;
    }
    let withdrawn = if s.agent_records().has_explicit_identity(wire_terminal_id) {
        crate::agent_state::withdraw_state(existing.as_deref())
    } else {
        None
    };
    match withdrawn {
        Some(bytes) => {
            s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
        }
        None => {
            s.metadata_delete(scope, RESOURCE_AGENT_KEY);
        }
    }
    s.agent_records_mut()
        .note_detector_retract(wire_terminal_id);
    retract_hook(wire_terminal_id, prior)
}

/// Withdraw a declared record's state to `unknown`, keeping its writer's
/// other fields. `None` when there is nothing to withdraw.
fn withdraw_declared(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    existing: Option<&[u8]>,
) -> Option<()> {
    let bytes = crate::agent_state::withdraw_state(existing)?;
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    s.agent_records_mut()
        .note_declaration_withdrawn(wire_terminal_id);
    Some(())
}

/// The drain's `Reidentified` arm: a different occupant now owns the pane.
/// One write landing on `unknown` (invariant I2 in `crate::agent_state`); a
/// declared record is withdrawn rather than corrected, and a pane with no
/// record is left without one.
fn drain_reidentified(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    hooks_live: bool,
    kind: &str,
    name: &str,
) -> Option<HookEvent> {
    use crate::hooks::AGENT_STATE_UNKNOWN;
    if s.metadata().contains(
        scope,
        phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_KEY,
    ) {
        return None;
    }

    let existing = s.metadata().get(scope, RESOURCE_AGENT_KEY)?;
    let prior = prior_state(hooks_live, Some(&existing));
    if s.agent_records().is_declared(wire_terminal_id) {
        withdraw_declared(s, wire_terminal_id, scope, Some(&existing))?;
        return retract_hook(wire_terminal_id, prior);
    }
    let owned = s.agent_records().identity_ownership(wire_terminal_id);
    let bytes =
        crate::agent_state::compose(Some(&existing), kind, name, AGENT_STATE_UNKNOWN, owned);
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    s.agent_records_mut().note_detector_write(wire_terminal_id);
    state_change_hook(wire_terminal_id, kind, name, prior, AGENT_STATE_UNKNOWN)
}

/// The drain's `State` arm: the detector derived a state for this pane.
///
/// A declared record outranks the detector (ADR-0046 §E). `kind`, `name` and
/// `state` are composed from one read under one lock (invariant I1). Where an
/// explicit writer's `kind` contradicts the detector, the `kind` is kept and
/// the state is withdrawn to `unknown` (invariant I2).
fn drain_state(
    s: &mut ServerState,
    wire_terminal_id: &WireResourceId,
    scope: &Scope,
    hooks_live: bool,
    report: &crate::agent_detect::AgentReport,
) -> Option<HookEvent> {
    let program_status = s.metadata().contains(
        scope,
        phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_KEY,
    );
    if program_status
        && !crate::agent_detect::live_session::server_has_live_session(s, wire_terminal_id)
    {
        return None;
    }
    if s.agent_records().is_declared(wire_terminal_id) {
        return None;
    }
    let mut existing = s.metadata().get(scope, RESOURCE_AGENT_KEY);
    let prior = prior_state(hooks_live, existing.as_deref());
    let owned = s.agent_records().identity_ownership(wire_terminal_id);
    // The OSC error badge is derived attention, not a human declaration.
    if program_status
        && !s.agent_records().has_explicit_identity(wire_terminal_id)
        && let Some(mut record) = existing
            .as_deref()
            .and_then(crate::agent_detect::record::AgentRecordJson::decode)
    {
        record.attention = None;
        existing = Some(record.encode());
    }
    let contradicted = owned.kind
        && crate::agent_state::explicit_kind_is_contradicted(
            existing.as_deref(),
            &report.kind,
            &crate::agent_detect::rules::global(),
        );
    let (to, bytes) = if contradicted {
        let bytes = crate::agent_state::withdraw_state(existing.as_deref())?;
        (crate::hooks::AGENT_STATE_UNKNOWN, bytes)
    } else {
        let to = report.state.as_str();
        let bytes =
            crate::agent_state::compose(existing.as_deref(), &report.kind, &report.name, to, owned);
        (to, bytes)
    };
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    if !contradicted {
        // Withdrawing is not authoring: the detector must not gain the right
        // to delete an explicit writer's record.
        s.agent_records_mut().note_detector_write(wire_terminal_id);
    }
    state_change_hook(wire_terminal_id, &report.kind, &report.name, prior, to)
}

/// The stored state before a detector write, if any.
struct Prior(Option<String>);

/// The prior state, read only when a hook could fire: `None` means no hook
/// is owed at all.
fn prior_state(hooks_live: bool, existing: Option<&[u8]>) -> Option<Prior> {
    hooks_live.then(|| Prior(crate::agent_state::stored_state(existing)))
}

/// The `agent-state-changed` event for a detector write, unless hooks are
/// off or the store already held that state.
fn state_change_hook(
    wire_terminal_id: &WireResourceId,
    kind: &str,
    name: &str,
    prior: Option<Prior>,
    to: &str,
) -> Option<HookEvent> {
    let Prior(from) = prior?;
    (from.as_deref() != Some(to))
        .then(|| HookEvent::agent_state_changed(wire_terminal_id, kind, name, from.as_deref(), to))
}

/// The `agent-state-changed` event for a withdrawn record: a write to
/// `unknown` with no identity.
fn retract_hook(wire_terminal_id: &WireResourceId, prior: Option<Prior>) -> Option<HookEvent> {
    state_change_hook(
        wire_terminal_id,
        "",
        "",
        prior,
        crate::hooks::AGENT_STATE_UNKNOWN,
    )
}

/// Re-arm the pane detector's edge filter after someone else wrote its
/// `phux.agent/v1` record (ADR-0046 §E); otherwise the filter keeps modelling
/// a store that no longer exists and suppresses the next write. A full or
/// closed control mailbox is benign (the actor is wedged or gone).
pub(super) fn invalidate_agent_detector(state: &SharedState, scope: &Scope, key: &str) {
    if key != RESOURCE_AGENT_KEY {
        return;
    }
    let Scope::Resource(wire) = scope else {
        return;
    };
    let handle = state.with(|s| {
        s.terminal_from_wire(wire)
            .and_then(|pane| s.resource_handle(pane).cloned())
    });
    if let Some(handle) = handle {
        let _ = handle
            .control
            .try_send(crate::terminal_actor::ControlRequest::AgentRecordInvalidated);
    }
}

#[cfg(test)]
mod pane_event_drain_tests {
    use super::*;

    /// A pane reaped by a failed publication already has its `pane_closed`;
    /// an event its drain reads afterwards is not journaled.
    #[test]
    fn a_reaped_pane_journals_nothing_after_its_close() {
        let mut s = crate::state::ServerState::new();
        let (_session, pane) = s.seed_session("drain");
        let wire = s.intern_terminal_wire(pane);
        // The seed's organization events are journaled too.
        let seeded = s.journal_head();
        journal_drained_event(&mut s, &wire, AgentEvent::Bell.into(), 0);
        let live = s.journal_head();
        assert_eq!(live, seeded + 1, "a live pane's event is journaled");
        let _ = reap_pane_journaling_close(&mut s, pane);
        let closed = s.journal_head();
        assert!(closed > live, "the close, and the record's removal");
        journal_drained_event(&mut s, &wire, AgentEvent::Bell.into(), 2);
        assert_eq!(s.journal_head(), closed, "nothing follows pane_closed");
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod agent_drain_tests {
    use phux_protocol::ids::ResourceId as WireResourceId;
    use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

    use super::{Prior, retract_hook, spawn_agent_state_drain, state_change_hook};
    use crate::agent_asked::AskedPayload;
    use crate::agent_detect::record::AgentRecordJson;
    use crate::agent_detect::{AgentDetectEvent, AgentReport, DetectedState};
    use crate::hooks::AGENT_STATE_UNKNOWN;
    use crate::state::SharedState;

    fn report_of(kind: &str, state: DetectedState) -> AgentDetectEvent {
        AgentDetectEvent::State(AgentReport {
            kind: kind.to_owned(),
            name: kind.to_owned(),
            state,
        })
    }

    fn claude(state: DetectedState) -> AgentDetectEvent {
        report_of("claude", state)
    }

    fn reidentified(kind: &str) -> AgentDetectEvent {
        AgentDetectEvent::Reidentified {
            kind: kind.to_owned(),
            name: kind.to_owned(),
        }
    }

    /// A pane at wire id 1, optionally carrying a record its human wrote.
    fn pane(declared: Option<&[u8]>) -> (SharedState, WireResourceId) {
        let state = SharedState::new();
        let terminal = WireResourceId::new(1);
        if let Some(bytes) = declared {
            state.with_mut(|s| {
                s.agent_records_mut().note_explicit_set(&terminal, bytes);
                s.metadata_set(
                    &Scope::Resource(terminal.clone()),
                    RESOURCE_AGENT_KEY,
                    bytes.to_vec(),
                );
            });
        }
        (state, terminal)
    }

    /// Drive the real drain task to quiescence over `events`.
    async fn drain(state: &SharedState, terminal: &WireResourceId, events: Vec<AgentDetectEvent>) {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        spawn_agent_state_drain(state.clone(), terminal.clone(), rx);
        for event in events {
            tx.send(event).await.expect("drain is alive");
        }
        drop(tx);
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    fn raw(state: &SharedState, terminal: &WireResourceId) -> Option<Vec<u8>> {
        let scope = Scope::Resource(terminal.clone());
        state.with(|s| s.metadata().get(&scope, RESOURCE_AGENT_KEY))
    }

    fn stored(state: &SharedState, terminal: &WireResourceId) -> Option<AgentRecordJson> {
        raw(state, terminal).and_then(|bytes| AgentRecordJson::decode(&bytes))
    }

    async fn local(test: impl std::future::Future<Output = ()>) {
        tokio::task::LocalSet::new().run_until(test).await;
    }

    #[test]
    fn hooks_fire_only_on_real_edges() {
        let terminal = WireResourceId::local(1);
        let ctx = |event: &crate::hooks::HookEvent, key: &str| event.context.get(key).cloned();
        let was = |state: &str| Some(Prior(Some(state.to_owned())));

        assert!(
            state_change_hook(&terminal, "claude", "", None, "working").is_none(),
            "no hook is owed while hooks are off"
        );
        let first =
            state_change_hook(&terminal, "claude", "", Some(Prior(None)), "working").expect("edge");
        assert_eq!(first.name, crate::hooks::AGENT_STATE_CHANGED);
        assert_eq!(
            ctx(&first, "from"),
            None,
            "a first sighting has no prior state"
        );
        assert_eq!(ctx(&first, "agent-name"), None, "an empty name is omitted");
        let blocked =
            state_change_hook(&terminal, "claude", "rev", was("working"), "blocked").expect("edge");
        assert_eq!(ctx(&blocked, "from").as_deref(), Some("working"));
        assert_eq!(ctx(&blocked, "to").as_deref(), Some("blocked"));
        assert!(state_change_hook(&terminal, "claude", "claude", was("idle"), "idle").is_none());

        let retract = retract_hook(&terminal, was("working")).expect("a retract is an edge");
        assert_eq!(ctx(&retract, "to").as_deref(), Some(AGENT_STATE_UNKNOWN));
        assert!(retract_hook(&terminal, was(AGENT_STATE_UNKNOWN)).is_none());
    }

    /// One drain run per row over a pane that may carry a human's record:
    /// the resulting `(kind, name, state, session)`, or `None` when no record
    /// remains.
    ///
    /// - An identity-only record is not a declaration: the detector fills in
    ///   `state`, and a retract withdraws it without deleting the name.
    /// - A record the detector wrote alone is deleted by a retract.
    /// - I2: a new occupant lands on `unknown` in one write; a declaration is
    ///   withdrawn rather than corrected; no record means nothing is written.
    /// - I1: a dropped `Reidentified` is healed by the next `State` write.
    /// - A custom kind the detector cannot derive still gets its state, and
    ///   reasserting a kind never drags a human's name with it.
    #[tokio::test(flavor = "current_thread")]
    async fn detector_writes_respect_human_records() {
        type Expect<'a> = Option<(Option<&'a str>, &'a str, &'a str, Option<&'a str>)>;
        type Case<'a> = (Option<&'a [u8]>, Vec<AgentDetectEvent>, Expect<'a>);
        let identity = br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#.as_slice();
        let declared = br#"{"name":"me","kind":"claude","state":"working"}"#.as_slice();
        let custom = br#"{"name":"reviewer","kind":"my-agent","session":"fleet-7"}"#.as_slice();
        let nameless = br#"{"name":"reviewer","session":"fleet-7"}"#.as_slice();
        let cases: Vec<Case<'_>> = vec![
            (
                Some(identity),
                vec![claude(DetectedState::Working), AgentDetectEvent::Retract],
                Some((
                    Some("claude"),
                    "reviewer",
                    AGENT_STATE_UNKNOWN,
                    Some("fleet-7"),
                )),
            ),
            (
                None,
                vec![claude(DetectedState::Working), AgentDetectEvent::Retract],
                None,
            ),
            (
                None,
                vec![claude(DetectedState::Working), reidentified("codex")],
                Some((Some("codex"), "codex", AGENT_STATE_UNKNOWN, None)),
            ),
            (
                None,
                vec![
                    claude(DetectedState::Working),
                    report_of("codex", DetectedState::Blocked),
                ],
                Some((Some("codex"), "codex", "blocked", None)),
            ),
            (
                Some(declared),
                vec![reidentified("codex")],
                Some((Some("claude"), "me", AGENT_STATE_UNKNOWN, None)),
            ),
            (None, vec![reidentified("codex")], None),
            (
                Some(custom),
                vec![claude(DetectedState::Blocked)],
                Some((Some("my-agent"), "reviewer", "blocked", Some("fleet-7"))),
            ),
            (
                Some(nameless),
                vec![
                    claude(DetectedState::Working),
                    report_of("codex", DetectedState::Idle),
                ],
                Some((Some("codex"), "reviewer", "idle", Some("fleet-7"))),
            ),
        ];
        for (row, (record, events, expect)) in cases.into_iter().enumerate() {
            local(async {
                let (state, terminal) = pane(record);
                drain(&state, &terminal, events).await;
                let got = stored(&state, &terminal);
                let got = got.as_ref().map(|r| {
                    (
                        r.kind.as_deref(),
                        r.name.as_str(),
                        r.state.as_str(),
                        r.session.as_deref(),
                    )
                });
                assert_eq!(got, expect, "row {row}");
            })
            .await;
        }
    }

    /// A declared state outranks derivations (L3 §3.7), but a confirmed
    /// departure withdraws it to `unknown`, never deletes it; afterwards the
    /// detector's writes land again (phux-w7z2.13: a `kill -9` must not pin
    /// the pane at `working`).
    #[tokio::test(flavor = "current_thread")]
    async fn a_retract_withdraws_a_declared_state_but_never_deletes_it() {
        local(async {
            let (state, terminal) = pane(Some(
                br#"{"name":"me","kind":"claude","state":"working","attention":"high"}"#,
            ));
            drain(&state, &terminal, vec![claude(DetectedState::Idle)]).await;
            assert_eq!(
                stored(&state, &terminal).expect("declared").state,
                "working"
            );

            drain(&state, &terminal, vec![AgentDetectEvent::Retract]).await;
            let record = stored(&state, &terminal).expect("withdrawing is not deleting");
            assert_eq!(record.state, AGENT_STATE_UNKNOWN);
            assert_eq!(record.name, "me");
            assert_eq!(record.kind.as_deref(), Some("claude"));
            assert_eq!(record.attention, None);

            drain(&state, &terminal, vec![claude(DetectedState::Working)]).await;
            let after = stored(&state, &terminal).expect("still there");
            assert_eq!(after.state, "working", "the detector resumed");
            assert_eq!(after.name, "me");
        })
        .await;
    }

    /// phux-w7z2.45: a shim pane declared `kind: claude` now running codex
    /// keeps the declared kind (L3 §3.7) and withholds codex's state, and
    /// every later contradicted tick writes nothing.
    #[tokio::test(flavor = "current_thread")]
    async fn a_shim_pane_never_takes_a_state_from_an_occupant_its_kind_denies() {
        local(async {
            let (state, terminal) = pane(Some(br#"{"name":"claude","kind":"claude"}"#));
            drain(
                &state,
                &terminal,
                vec![
                    claude(DetectedState::Working),
                    reidentified("codex"),
                    report_of("codex", DetectedState::Working),
                ],
            )
            .await;
            let record = stored(&state, &terminal).expect("still there");
            assert_eq!(record.kind.as_deref(), Some("claude"));
            assert_eq!(record.name, "claude");
            assert_eq!(record.state, AGENT_STATE_UNKNOWN);

            let first = raw(&state, &terminal);
            let churn = [
                DetectedState::Blocked,
                DetectedState::Idle,
                DetectedState::Working,
                DetectedState::Done,
            ];
            let ticks = (0..20)
                .map(|i| report_of("codex", churn[i % churn.len()]))
                .collect();
            drain(&state, &terminal, ticks).await;
            assert_eq!(raw(&state, &terminal), first, "withholding is free");
        })
        .await;
    }

    /// Level-triggered: the state resumes once the pane runs the declared
    /// kind again, and clearing the declaration hands the kind back.
    #[tokio::test(flavor = "current_thread")]
    async fn a_contradiction_heals_on_the_declared_kind_or_a_cleared_declaration() {
        local(async {
            let (state, terminal) = pane(Some(br#"{"name":"claude","kind":"claude"}"#));
            drain(
                &state,
                &terminal,
                vec![report_of("codex", DetectedState::Working)],
            )
            .await;
            assert_eq!(
                stored(&state, &terminal).expect("written").state,
                AGENT_STATE_UNKNOWN
            );
            drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
            assert_eq!(stored(&state, &terminal).expect("written").state, "blocked");

            drain(
                &state,
                &terminal,
                vec![report_of("codex", DetectedState::Working)],
            )
            .await;
            let scope = Scope::Resource(terminal.clone());
            state.with_mut(|s| {
                s.agent_records_mut().note_explicit_delete(&terminal);
                s.metadata_delete(&scope, RESOURCE_AGENT_KEY);
            });
            drain(
                &state,
                &terminal,
                vec![report_of("codex", DetectedState::Working)],
            )
            .await;
            let record = stored(&state, &terminal).expect("rewritten");
            assert_eq!(record.kind.as_deref(), Some("codex"));
            assert_eq!(record.state, "working");
        })
        .await;
    }

    /// phux-uaon: an identity-only SET that reached the store but not the
    /// arbiter still survives the next detector write.
    #[tokio::test(flavor = "current_thread")]
    async fn a_subsequent_detector_write_merges_over_an_identity_only_set() {
        local(async {
            let (state, terminal) = pane(None);
            drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
            let scope = Scope::Resource(terminal.clone());
            state.with_mut(|s| {
                s.metadata_set(
                    &scope,
                    RESOURCE_AGENT_KEY,
                    br#"{"name":"reviewer","session":"fleet-7"}"#.to_vec(),
                );
            });
            drain(&state, &terminal, vec![claude(DetectedState::Blocked)]).await;
            let record = stored(&state, &terminal).expect("merged");
            assert_eq!(record.name, "reviewer");
            assert_eq!(record.session.as_deref(), Some("fleet-7"));
            assert_eq!(record.state, "blocked");
            assert_eq!(record.kind.as_deref(), Some("claude"));
        })
        .await;
    }

    /// ADR-0036 tier 2: a `phux-ask` marker lands in the same ledger a
    /// `REPORT_ASKED` hook writes to, and clearing it retracts the ask.
    #[tokio::test(flavor = "current_thread")]
    async fn an_ask_sentinel_lands_in_the_ledger_the_hook_shares() {
        local(async {
            let state = SharedState::new();
            let (pane, terminal) = state.with_mut(|s| {
                let (_session, pane) = s.seed_session("demo");
                (pane, s.intern_terminal_wire(pane))
            });
            let ask = AskedPayload {
                id: "q1".to_owned(),
                question: "Deploy to prod?".to_owned(),
                suggestions: vec!["Yes".to_owned(), "No".to_owned()],
                elapsed_seconds: None,
            };
            drain(
                &state,
                &terminal,
                vec![AgentDetectEvent::AskSentinel(Some(ask))],
            )
            .await;
            assert_eq!(
                state.with(|s| s.current_agent_asked(pane).map(|p| p.id.clone())),
                Some("q1".to_owned()),
            );
            drain(&state, &terminal, vec![AgentDetectEvent::AskSentinel(None)]).await;
            assert!(state.with(|s| s.current_agent_asked(pane).is_none()));
        })
        .await;
    }
}
