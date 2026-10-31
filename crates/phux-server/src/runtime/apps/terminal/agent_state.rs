//! Fold terminal detector reports into server-owned agent metadata and hooks.

use crate::hooks::HookEvent;
use crate::runtime::connections::metadata_subscriptions::broadcast_event;
use crate::state::{ServerState, SharedState};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

/// Whether the pane `wire_terminal_id` names is retained after its process
/// exited (ADR-0124).
fn pane_exited(s: &ServerState, wire_terminal_id: &WireResourceId) -> bool {
    s.resource_from_wire(wire_terminal_id)
        .is_some_and(|pane| s.retained_exit(pane).is_some())
}

/// Spawn the per-pane detector metadata drain (ADR-0046): the actor emits
/// edge-filtered [`AgentDetectEvent`](phux_terminal::agent_detect::AgentDetectEvent)s
/// and this task, which can reach `ServerState`, performs the authority check
/// and the metadata write.
pub(crate) fn spawn_agent_state_drain(
    state: SharedState,
    wire_terminal_id: WireResourceId,
    mut rx: tokio::sync::mpsc::Receiver<phux_terminal::agent_detect::AgentDetectEvent>,
) {
    use phux_terminal::agent_detect::AgentDetectEvent;

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
    ask: Option<phux_resource::agent_asked::AskedPayload>,
) -> Option<phux_resource::agent_asked::AskedPayload> {
    use phux_resource::agent_asked::AskedSource;

    let terminal = s.resource_from_wire(wire_terminal_id)?;
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
pub(in crate::runtime) fn drain_retract(
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
        phux_resource::agent_state::withdraw_state(existing.as_deref())
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
    let bytes = phux_resource::agent_state::withdraw_state(existing)?;
    s.metadata_set(scope, RESOURCE_AGENT_KEY, bytes);
    s.agent_records_mut()
        .note_declaration_withdrawn(wire_terminal_id);
    Some(())
}

/// The drain's `Reidentified` arm: a different occupant now owns the pane.
/// One write landing on `unknown` (invariant I2 in `phux_resource::agent_state`); a
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
    let bytes = phux_resource::agent_state::compose(
        Some(&existing),
        kind,
        name,
        AGENT_STATE_UNKNOWN,
        owned,
    );
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
    report: &phux_terminal::agent_detect::AgentReport,
) -> Option<HookEvent> {
    let program_status = s.metadata().contains(
        scope,
        phux_protocol::wire::frame::RESOURCE_PROGRAM_STATUS_KEY,
    );
    if program_status && !s.resource_has_live_session(wire_terminal_id) {
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
            .and_then(phux_resource::agent_record::AgentRecordJson::decode)
    {
        record.attention = None;
        existing = Some(record.encode());
    }
    let contradicted = owned.kind
        && phux_resource::agent_state::explicit_kind_is_contradicted(
            existing.as_deref(),
            &report.kind,
            &phux_terminal::agent_detect::rules::global(),
        );
    let (to, bytes) = if contradicted {
        let bytes = phux_resource::agent_state::withdraw_state(existing.as_deref())?;
        (crate::hooks::AGENT_STATE_UNKNOWN, bytes)
    } else {
        let to = report.state.as_str();
        let bytes = phux_resource::agent_state::compose(
            existing.as_deref(),
            &report.kind,
            &report.name,
            to,
            owned,
        );
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
    hooks_live.then(|| Prior(phux_resource::agent_state::stored_state(existing)))
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
pub(in crate::runtime) fn invalidate_agent_detector(state: &SharedState, scope: &Scope, key: &str) {
    if key != RESOURCE_AGENT_KEY {
        return;
    }
    let Scope::Resource(wire) = scope else {
        return;
    };
    let handle = state.with(|s| {
        s.resource_from_wire(wire)
            .and_then(|pane| s.resource_handle(pane).cloned())
    });
    if let Some(handle) = handle {
        let _ = handle
            .control
            .try_send(crate::resource::ControlRequest::AgentRecordInvalidated);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod agent_drain_tests;
