//! Metadata subscription sweeps: the per-pane `phux.agent/v1` watches and
//! the peer-session layout/agent caches behind the picker and the fleet.

use std::collections::HashMap;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::agent_meta::{AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record};
use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

/// phux-foz.8: fetch each peer session's persisted layout — one
/// `GET_METADATA` on the per-session layout key per session other than
/// `focused` — so the window picker can render one-step cross-session
/// window rows. Correlation is via `pending` (request id -> session id);
/// replies drain through the driver's recv arm into the foreign-layout
/// cache. Best-effort: a peer with nothing persisted replies `value: None`
/// (dropped by [`apply_foreign_layout_reply`]) and keeps its fallback
/// "switch to this session" row.
pub(super) async fn sync_foreign_layout_subscriptions(
    conn: &mut Connection,
    sessions: &[phux_protocol::wire::info::SessionInfo],
    focused: Option<phux_protocol::ids::SessionId>,
    next_request_id: &mut u32,
    pending: &mut HashMap<u32, phux_protocol::ids::SessionId>,
    subscribed: &mut std::collections::HashSet<phux_protocol::ids::SessionId>,
) -> Result<(), AttachError> {
    for s in sessions.iter().filter(|s| Some(s.id) != focused) {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        pending.insert(request_id, s.id);
        let key = layout_key(s.id);
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Group(DEFAULT_GROUP_ID),
            key: key.clone(),
        })
        .await?;
        // phux-k0cw: the GET is the level; this is the edge. Sent even when
        // the GET will answer `None` — a peer that has not persisted a layout
        // yet is precisely the one whose FIRST write matters, and without the
        // subscription that write is invisible until the next attach.
        //
        // Send-once bookkeeping rather than teardown: L3 has no
        // UNSUBSCRIBE_METADATA verb (docs/spec/L3.md), so a subscription ends
        // with the connection.
        if subscribed.insert(s.id) {
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Group(DEFAULT_GROUP_ID),
                key,
            })
            .await?;
        }
    }
    Ok(())
}

/// phux-foz.8: fold one foreign-session layout GET reply into the picker's
/// cache. `value: None` (nothing persisted) or an undecodable envelope
/// clears the entry, so the picker falls back to the plain
/// "switch to this session" row rather than showing stale windows.
pub(super) fn apply_foreign_layout_reply(
    cache: &mut HashMap<phux_protocol::ids::SessionId, Workspace>,
    session: phux_protocol::ids::SessionId,
    value: Option<&[u8]>,
) {
    match value {
        Some(bytes) => match Workspace::decode_cbor(bytes) {
            Ok(ws) => {
                cache.insert(session, ws);
            }
            Err(err) => {
                tracing::debug!(
                    session = session.get(),
                    error = %err,
                    "foreign layout decode failed; window picker keeps the fallback row",
                );
                cache.remove(&session);
            }
        },
        None => {
            cache.remove(&session);
        }
    }
}

/// phux-jpqd: fetch the `phux.agent/v1` record of every pane in one peer
/// session's just-loaded `workspace` — one `GET_METADATA` per `TerminalId`
/// leaf on the pane's agent key — so the agent-fleet dashboard's foreign
/// rows show its agent glyph/state without attaching there. Correlated
/// through `pending` (request id -> terminal id); replies fold via
/// [`apply_foreign_agent_reply`]. Skips leaves with a GET already in flight
/// so a re-fold (session-graph refresh re-requests the layout) does not
/// duplicate traffic. One-shot reads, no subscription — the same lazy-query
/// shape as [`request_foreign_layouts`] (ADR-0018 / ADR-0030).
pub(super) async fn sync_foreign_agent_subscriptions(
    conn: &mut Connection,
    workspace: &Workspace,
    next_request_id: &mut u32,
    pending: &mut HashMap<u32, TerminalId>,
    subscribed: &mut std::collections::HashSet<TerminalId>,
) -> Result<(), AttachError> {
    // Collect the leaf ids first so the immutable borrow of `pending` (for
    // the in-flight check) is released before we mutate it in the send loop.
    let targets: Vec<TerminalId> = {
        let in_flight: std::collections::HashSet<&TerminalId> = pending.values().collect();
        let mut targets: Vec<TerminalId> = Vec::new();
        for window in &workspace.windows {
            if let Some(tree) = window.state.tree.as_ref() {
                for id in crate::layout::leaves(tree) {
                    // phux-k0cw: a satellite Terminal's metadata scope is
                    // normatively refused (docs/spec/L3.md), so subscribing
                    // would earn one UNSUPPORTED_SATELLITE_ROUTE per sweep —
                    // errors the correlated-refusal intercept swallows
                    // silently, which is the worst kind of noise. The same
                    // skip `sync_agent_meta_subscriptions` already applies.
                    if !id.is_local() {
                        continue;
                    }
                    if !in_flight.contains(&id) && !targets.contains(&id) {
                        targets.push(id);
                    }
                }
            }
        }
        targets
    };
    for id in targets {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Terminal(id.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
        })
        .await?;
        // The level, then the edge — same shape as the layout sweep.
        if subscribed.insert(id.clone()) {
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Terminal(id),
                key: TERMINAL_AGENT_KEY.to_owned(),
            })
            .await?;
        }
    }
    Ok(())
}

/// phux-jpqd: fold one foreign-pane agent-record GET reply into the fleet's
/// cache. `value: None` (no record) or an unparseable record clears the
/// entry, so the fleet row falls back to `?` / "no agent" rather than
/// showing stale identity — the same clear-on-empty policy as
/// [`apply_foreign_layout_reply`].
pub(super) fn apply_foreign_agent_reply(
    cache: &mut HashMap<TerminalId, AgentRecord>,
    id: TerminalId,
    value: Option<&[u8]>,
) {
    match value.and_then(parse_agent_record) {
        Some(record) => {
            cache.insert(id, record);
        }
        None => {
            cache.remove(&id);
        }
    }
}

/// phux-jpqd: drop foreign agent records for panes no longer present in any
/// cached foreign layout (a peer closed a pane, or a session left the
/// graph), keeping the cache bounded to the live foreign pane set. Called
/// on each foreign-layout fold, before re-requesting the surviving panes.
///
/// phux-k0cw: the send-once subscription bookkeeping is pruned with it. A
/// pane that leaves and later returns under the same id must be re-subscribed
/// — leaving it in the `subscribed` set would suppress the re-subscribe and
/// the row would go permanently silent.
pub(super) fn prune_foreign_agents(
    cache: &mut HashMap<TerminalId, AgentRecord>,
    subscribed: &mut std::collections::HashSet<TerminalId>,
    foreign_layouts: &HashMap<phux_protocol::ids::SessionId, Workspace>,
) {
    let live: std::collections::HashSet<TerminalId> = foreign_layouts
        .values()
        .flat_map(|ws| ws.windows.iter())
        .filter_map(|w| w.state.tree.as_ref())
        .flat_map(crate::layout::leaves)
        .collect();
    cache.retain(|id, _| live.contains(id));
    subscribed.retain(|id| live.contains(id));
}

/// ADR-0040 (phux-3ert): reconcile the agent-metadata index with the live
/// pane set.
///
/// For every pane that has no live `phux.agent/v1` watch yet, send a
/// one-shot `GET_METADATA` (the read-back for a record set before we
/// attached; the reply is correlated through `AgentMetaIndex::pending`) plus
/// a `SUBSCRIBE_METADATA` (the push path for later `SET`/`DELETE`
/// broadcasts). Panes that closed are pruned from every side table — the
/// server already dropped their per-Terminal store and our subscription
/// with the Terminal, so pruning is purely local hygiene. Idempotent: a
/// pane already in `subscribed` is skipped, so callers can re-run the sweep
/// on every pane-set change (bootstrap, split, new window, layout
/// broadcast) without duplicate wire traffic.
pub(super) async fn sync_agent_meta_subscriptions(
    conn: &mut Connection,
    // Owned id list (not `&HashMap<_, PaneSlot>`): `PaneSlot` holds a
    // libghostty mirror that is not `Send`, and holding a reference to it
    // across the sends would make this future `!Send` (clippy
    // `future_not_send`). Callers pass `panes.keys().cloned().collect()`.
    pane_ids: Vec<TerminalId>,
    agent_meta: &mut AgentMetaIndex,
    next_request_id: &mut u32,
) -> Result<(), AttachError> {
    agent_meta.subscribed.retain(|id| pane_ids.contains(id));
    agent_meta.records.retain(|id, _| pane_ids.contains(id));
    agent_meta.pending.retain(|_, id| pane_ids.contains(id));
    // Same hygiene for the attention ladder's clock: a closed pane must not
    // leave a timestamp behind for a recycled TerminalId to inherit.
    agent_meta.change_at.retain(|id, _| pane_ids.contains(id));
    for id in &pane_ids {
        if agent_meta.subscribed.contains(id) {
            continue;
        }
        // phux-w7z2.57: `phux.agent/v1` does not federate. A hub's metadata
        // store holds nothing for a satellite pane, so the `GET` can only
        // answer "unset" and the server now refuses the `SUBSCRIBE` outright
        // with `ERROR { UNSUPPORTED_SATELLITE_ROUTE }`. Sending them anyway
        // would spend two frames per remote pane to earn a warning notice in
        // the status area on every pane-set change. Deliberately not recorded
        // in `subscribed`: that set means "a live watch exists", and none
        // does — the re-skip costs nothing because no frame is sent either way.
        if !id.is_local() {
            continue;
        }
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        agent_meta.pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Terminal(id.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
        })
        .await?;
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Terminal(id.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
        })
        .await?;
        agent_meta.subscribed.insert(id.clone());
    }
    Ok(())
}
