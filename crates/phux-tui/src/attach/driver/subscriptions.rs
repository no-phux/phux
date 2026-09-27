//! Metadata subscription sweeps: the per-pane `phux.agent/v1` watches and
//! the peer-session layout/agent caches behind the picker and the fleet.

use std::collections::HashMap;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use phux_client::agent_meta::{
    AgentRecord, RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY, parse_agent_record,
};
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

/// GET and SUBSCRIBE each peer session's layout key (every session but
/// `focused`), correlated through `pending`, for the picker's one-step rows.
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
        // Subscribe even if the GET answers `None`: a peer's first write is
        // the one that matters. Send-once, since L3 has no unsubscribe.
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

/// Fold a peer layout GET reply; `None` or garbage clears the entry.
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

/// GET/SUBSCRIBE `phux.agent/v1` for each terminal in `targets`. Satellite
/// terminals also GET the asked flag (ADR-0136), correlated through
/// `asked_pending` since its value is not a record.
pub(super) async fn sync_foreign_agent_ids(
    conn: &mut Connection,
    targets: Vec<ResourceId>,
    next_request_id: &mut u32,
    pending: &mut HashMap<u32, ResourceId>,
    subscribed: &mut std::collections::HashSet<ResourceId>,
    asked_pending: &mut HashMap<u32, ResourceId>,
) -> Result<(), AttachError> {
    let in_flight: std::collections::HashSet<&ResourceId> = pending.values().collect();
    // Dedup while preserving order; skip in-flight GETs.
    let mut seen = std::collections::HashSet::new();
    let targets: Vec<ResourceId> = targets
        .into_iter()
        .filter(|id| !in_flight.contains(id))
        .filter(|id| seen.insert(id.clone()))
        .collect();
    for id in targets {
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
        if subscribed.insert(id.clone()) {
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Resource(id.clone()),
                key: RESOURCE_AGENT_KEY.to_owned(),
            })
            .await?;
            if !id.is_local() {
                let asked_id = *next_request_id;
                *next_request_id = next_request_id.wrapping_add(1);
                asked_pending.insert(asked_id, id.clone());
                conn.send(&FrameKind::GetMetadata {
                    request_id: asked_id,
                    scope: Scope::Resource(id.clone()),
                    key: RESOURCE_ASKED_KEY.to_owned(),
                })
                .await?;
                conn.send(&FrameKind::SubscribeMetadata {
                    scope: Scope::Resource(id),
                    key: RESOURCE_ASKED_KEY.to_owned(),
                })
                .await?;
            }
        }
    }
    Ok(())
}

/// Fold a peer agent-record GET reply (`None` or garbage clears); returns
/// whether the cache moved.
pub(super) fn apply_foreign_agent_reply(
    cache: &mut HashMap<ResourceId, AgentRecord>,
    id: ResourceId,
    value: Option<&[u8]>,
) -> bool {
    match value.and_then(parse_agent_record) {
        Some(record) => cache.insert(id, record.clone()) != Some(record),
        None => cache.remove(&id).is_some(),
    }
}

/// Drop peer agent records (and their send-once subscription markers, so a
/// returning id re-subscribes) for panes no longer in `live`.
pub(super) fn prune_foreign_agents(
    cache: &mut HashMap<ResourceId, AgentRecord>,
    subscribed: &mut std::collections::HashSet<ResourceId>,
    live: &std::collections::HashSet<ResourceId>,
) {
    cache.retain(|id, _| live.contains(id));
    subscribed.retain(|id| live.contains(id));
}

/// ADR-0040: GET + SUBSCRIBE `phux.agent/v1` for every pane not yet
/// watched, and prune closed panes from the side tables. Idempotent.
pub(super) async fn sync_agent_meta_subscriptions(
    conn: &mut Connection,
    // Owned ids: a `PaneSlot` reference across sends would make this `!Send`.
    pane_ids: Vec<ResourceId>,
    agent_meta: &mut AgentMetaIndex,
    next_request_id: &mut u32,
) -> Result<(), AttachError> {
    agent_meta.subscribed.retain(|id| pane_ids.contains(id));
    agent_meta.records.retain(|id, _| pane_ids.contains(id));
    agent_meta.pending.retain(|_, id| pane_ids.contains(id));
    agent_meta
        .asked_pending
        .retain(|_, id| pane_ids.contains(id));
    // Same hygiene for the attention ladder's clock: a closed pane must not
    // leave a timestamp behind for a recycled ResourceId to inherit.
    agent_meta.change_at.retain(|id, _| pane_ids.contains(id));
    for id in &pane_ids {
        if agent_meta.subscribed.contains(id) {
            continue;
        }
        let request_id = *next_request_id;
        *next_request_id = next_request_id.wrapping_add(1);
        agent_meta.pending.insert(request_id, id.clone());
        conn.send(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
        // ADR-0136: a satellite pane's asked flag is metadata, not an event
        // this client is guaranteed to see. The GET must not share `pending`.
        if !id.is_local() {
            let asked_id = *next_request_id;
            *next_request_id = next_request_id.wrapping_add(1);
            agent_meta.asked_pending.insert(asked_id, id.clone());
            conn.send(&FrameKind::GetMetadata {
                request_id: asked_id,
                scope: Scope::Resource(id.clone()),
                key: RESOURCE_ASKED_KEY.to_owned(),
            })
            .await?;
            conn.send(&FrameKind::SubscribeMetadata {
                scope: Scope::Resource(id.clone()),
                key: RESOURCE_ASKED_KEY.to_owned(),
            })
            .await?;
        }
        agent_meta.subscribed.insert(id.clone());
    }
    Ok(())
}
