use super::pane_events::ensure_event_pump;
use super::session_create::is_reserved_session_create_result;
use crate::state::{ClientId, Outbound, ServerState, SharedState};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{AgentEvent, ErrorCode, FrameKind, Scope};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// A satellite Terminal scope. Client writes of one are ignored (ADR-0136):
/// the mirror is read-only, and every other key stays on the satellite.
pub(super) const fn is_satellite_terminal_scope(scope: &Scope) -> bool {
    matches!(scope, Scope::Resource(WireResourceId::Satellite { .. }))
}

/// Start the read-only agent-metadata mirror when `scope` names a satellite
/// this hub routes and `key` is allowlisted. `false` otherwise.
pub(super) fn kick_satellite_metadata_mirror(
    state: &ServerState,
    scope: &Scope,
    key: &str,
) -> bool {
    let Scope::Resource(terminal) = scope else {
        return false;
    };
    if !crate::hub::metadata_mirror::is_mirrored_key(key) {
        return false;
    }
    let Some((host, id)) = crate::hub::relay::satellite_route(terminal) else {
        return false;
    };
    let Some(relay) = state.hub_relay(&host) else {
        return false;
    };
    relay.mirror_terminal(id);
    true
}

/// Refuse an L3 metadata subscription whose `Terminal` scope names a
/// satellite pane with an uncorrelated `UNSUPPORTED_SATELLITE_ROUTE` push,
/// so the consumer does not wait on a change that can never come. Mirrored
/// agent keys on a routing hub are handled before this (ADR-0136). Returns
/// `true` when the caller must abandon the subscription.
fn refuse_satellite_metadata_scope(
    client_id: ClientId,
    scope: &Scope,
    key: &str,
    out_tx: &mpsc::Sender<Outbound>,
) -> bool {
    let Scope::Resource(terminal) = scope else {
        return false;
    };
    let Some((host, id)) = crate::hub::relay::satellite_route(terminal) else {
        return false;
    };
    warn!(
        ?client_id,
        satellite = %host,
        %key,
        "SUBSCRIBE_METADATA refused: L3 metadata has no satellite route"
    );
    push_error(
        out_tx,
        ErrorCode::UnsupportedSatelliteRoute,
        format!(
            "L3 metadata does not federate: no subscription to key '{key}' on \
             {host}/@{id}. The record lives on that satellite's own server; run \
             the command there.",
            host = host.as_str(),
        ),
    );
    true
}

/// Record an L3 metadata subscription for `client_id` (SPEC §7.4). The
/// mailbox is captured so a consumer that never attached still receives
/// `METADATA_CHANGED`.
pub(super) fn handle_subscribe_metadata(
    state: &SharedState,
    client_id: ClientId,
    scope: Scope,
    key: String,
    out_tx: &mpsc::Sender<Outbound>,
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
            debug!(?client_id, ?scope, %key, "SUBSCRIBE_METADATA refused (non-L3)");
            return;
        }
        if kick_satellite_metadata_mirror(s, &scope, &key) {
            // Fall through: the subscription is on the hub's retagged scope.
        } else if refuse_satellite_metadata_scope(client_id, &scope, &key, out_tx) {
            return;
        }
        let log_scope = scope.clone();
        let log_key = key.clone();
        if s.metadata_subscribe(client_id, scope, key, out_tx.clone()) {
            debug!(?client_id, ?log_scope, %log_key, "SUBSCRIBE_METADATA");
        } else {
            // Per-connection cap; refuse rather than evict a working one.
            warn!(
                ?client_id,
                ?log_scope,
                %log_key,
                "SUBSCRIBE_METADATA refused: per-connection subscription cap reached"
            );
        }
    });
}

/// Record an agent-event subscription for `client_id` (SPEC §7.5,
/// ADR-0123): server-wide or per-pane, not tier-gated. With `after_seq` it
/// replays from the journal first (L1 §7.3); what the mailbox cannot take
/// is owed as a `journal_gap` and delivered by the event pump.
pub(super) fn handle_subscribe_events(
    state: &SharedState,
    client_id: ClientId,
    terminal: Option<WireResourceId>,
    after_seq: Option<u64>,
    out_tx: &mpsc::Sender<Outbound>,
) {
    debug!(?client_id, ?terminal, ?after_seq, "SUBSCRIBE_EVENTS");
    if let Some(wire_id) = &terminal
        && let Some(route) = crate::hub::relay::satellite_route(wire_id)
    {
        subscribe_via_hub(state, client_id, wire_id, route, after_seq, out_tx);
        return;
    }
    state.with_mut(|s| match after_seq {
        None => s.subscribe_events(client_id, terminal, out_tx.clone()),
        Some(after_seq) => s.subscribe_events_after(client_id, terminal, after_seq, out_tx.clone()),
    });
    ensure_event_pump(state, client_id);
}

/// Satellite-scoped `SUBSCRIBE_EVENTS`: register a hub-side proxy subscriber
/// and forward the frame over the owning link. A missing route is a typed
/// `ERROR` push. Relayed events take this hub's `seq` and are not retained,
/// so a cursor on a satellite scope is owed a `journal_gap`.
fn subscribe_via_hub(
    state: &SharedState,
    client_id: ClientId,
    wire_id: &WireResourceId,
    (host, id): (phux_protocol::ids::SatelliteHost, u32),
    after_seq: Option<u64>,
    out_tx: &mpsc::Sender<Outbound>,
) {
    let Some(relay) = state.with(|s| s.hub_relay(&host)) else {
        warn!(
            ?client_id,
            satellite = %host,
            "SUBSCRIBE_EVENTS: no route to satellite; refusing subscription"
        );
        push_error(
            out_tx,
            ErrorCode::UnsupportedSatelliteRoute,
            format!(
                "no satellite route to {host:?}: this server is not a federation hub for that host"
            ),
        );
        return;
    };
    let Some(consumer_cancel) = state.with(|s| s.client_connection_cancellation(client_id)) else {
        push_error(
            out_tx,
            ErrorCode::InternalError,
            "client connection cancellation is unavailable",
        );
        return;
    };
    let change = state.with_mut(|s| {
        s.subscribe_satellite_events(
            client_id,
            wire_id.clone(),
            crate::state::EventFilter::all(),
            after_seq,
            out_tx.clone(),
        )
    });
    // Either both registrations happen or the consumer gets an error push.
    let forwarded = relay.subscribe(
        crate::hub::relay::ProxySubscription {
            terminal: id,
            client: client_id,
            out_tx: out_tx.clone(),
            consumer_cancel,
            // Stamped by `subscribe` at enqueue.
            seq: 0,
            awaits_snapshot: false,
            resource_kind: None,
            bootstrap_profile: None,
            bootstrap_limits: None,
        },
        FrameKind::SubscribeEvents {
            terminal: Some(WireResourceId::local(id)),
            after_seq: None,
        },
    );
    if !forwarded {
        // Put the registry scope back as it was.
        state.with_mut(|s| s.restore_satellite_scope(client_id, change));
        return;
    }
    ensure_event_pump(state, client_id);
}

/// Push an uncorrelated `ERROR` without waiting; a full mailbox drops it.
fn push_error(out_tx: &mpsc::Sender<Outbound>, code: ErrorCode, message: impl Into<String>) {
    let _ = out_tx.try_send(Outbound::Frame(FrameKind::Error {
        request_id: None,
        code,
        message: message.into(),
    }));
}

/// Journal an unattributed server-driven [`AgentEvent`] about `terminal`
/// (`None` for server-scoped) and fan it out (ADR-0123).
pub(crate) fn broadcast_event(
    state: &SharedState,
    terminal: Option<&WireResourceId>,
    event: &AgentEvent,
) {
    // ADR-0155: every ask source funnels through here, so this is where an
    // absent phone hears about it.
    if let (Some(pane), AgentEvent::Asked { id, .. }) = (terminal, event) {
        crate::push::on_asked(state, pane, id);
    }
    journal_event(
        state,
        crate::state::EventRecord::new(terminal.cloned(), event.clone()),
    );
}

/// Journal `record` and offer it to every subscription (ADR-0123) in its
/// own short lock. A full mailbox is owed a `journal_gap`.
fn journal_event(state: &SharedState, record: crate::state::EventRecord) {
    trace!(terminal = ?record.terminal, event = ?record.event, "EVENT: journaling");
    let _ = state.with_mut(|s| s.record_and_fanout(record));
}
