//! Foreign topology watches: cache folding, correlated reads/refusals,
//! send-once subscriptions, pruning, and inventory deadlines.

use super::*;

/// Foreign layout and agent GET replies cache on a decodable value and clear
/// on garbage or a tombstone; an identical agent record reports no change.
#[test]
fn foreign_replies_cache_clear_and_survive_garbage() {
    let sid = SessionId::new(7);
    let mut peers = PeerWatch::default();
    let mut ws = ArrangementDoc::single(ResourceId::local(1));
    ws.add_group("logs".to_owned(), ResourceId::local(2));
    let bytes = ws.encode_cbor().expect("encode");
    peers.apply_layout_reply(sid, Some(&bytes));
    assert_eq!(
        peers.foreign_layouts.get(&sid).map(|w| w.groups.len()),
        Some(2)
    );
    peers.apply_layout_reply(sid, Some(b"not cbor"));
    assert!(!peers.foreign_layouts.contains_key(&sid));
    peers.apply_layout_reply(sid, Some(&bytes));
    peers.apply_layout_reply(sid, None);
    assert!(!peers.foreign_layouts.contains_key(&sid));

    let id = ResourceId::local(3);
    let record = AgentRecord {
        name: "packer".to_owned(),
        kind: Some("codex".to_owned()),
        state: AgentMetaState::Working,
        ..AgentRecord::default()
    };
    assert!(peers.apply_agent_reply(id.clone(), Some(&record.encode())));
    assert_eq!(peers.foreign_agents[&id].name, "packer");
    assert!(!peers.apply_agent_reply(id.clone(), Some(&record.encode())));
    peers.apply_agent_reply(id.clone(), Some(b"not json"));
    assert!(!peers.foreign_agents.contains_key(&id));
    peers.apply_agent_reply(id.clone(), Some(&record.encode()));
    peers.apply_agent_reply(id.clone(), None);
    assert!(!peers.foreign_agents.contains_key(&id));
}

/// Peer layout keys are SUBSCRIBED once (there is no unsubscribe), still
/// GET each sweep, and our own session is never subscribed here.
#[tokio::test]
async fn peer_layout_keys_are_subscribed_not_just_read() {
    let (mut client, server) = pair();
    let mut peers = PeerWatch::default();
    peers.sessions = vec![
        phux_protocol::wire::info::SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
        phux_protocol::wire::info::SessionInfo::new(SessionId::new(2), "scratch")
            .with_window_count(1),
    ];
    peers.focused_session = Some(SessionId::new(1));
    let mut next = 1;
    for _ in 0..2 {
        peers
            .sweep_layouts(&mut client, &mut next)
            .await
            .expect("sweep sends");
    }
    let frames = drain(client, server).await;
    let (peer_key, own_key) = (layout_key(SessionId::new(2)), layout_key(SessionId::new(1)));
    let subscribe_count = |key: &str| {
        frames
            .iter()
            .filter(|f| {
                matches!(f, FrameKind::SubscribeMetadata { scope, key: k }
                if *scope == Scope::Group(DEFAULT_GROUP_ID) && k == key)
            })
            .count()
    };
    assert_eq!(subscribe_count(&peer_key), 1, "{frames:?}");
    assert_eq!(subscribe_count(&own_key), 0, "{frames:?}");
    assert!(
        frames
            .iter()
            .any(|f| matches!(f, FrameKind::GetMetadata { key, .. } if *key == peer_key))
    );
}

/// Foreign agent watches: each terminal (deduped) is fetched once and
/// subscribed, local or satellite; only satellites also read the asked
/// flag (a local ask arrives as an event).
#[tokio::test]
async fn foreign_agent_ids_are_fetched_and_subscribed_once() {
    let (mut client, server) = pair();
    let local = ResourceId::local(10);
    let satellite = ResourceId::satellite("prod-3", 2);
    let (mut peers, mut next) = (PeerWatch::default(), 1);
    peers
        .watch_agents(
            &mut client,
            vec![local.clone(), satellite.clone(), local.clone()],
            &mut next,
        )
        .await
        .expect("sweep sends");
    let frames = drain(client, server).await;
    let count = |id: &ResourceId, get: bool, key: &str| {
        frames
            .iter()
            .filter(|f| match f {
                FrameKind::GetMetadata { scope, key: k, .. } => {
                    get && *scope == Scope::Resource(id.clone()) && k == key
                }
                FrameKind::SubscribeMetadata { scope, key: k } => {
                    !get && *scope == Scope::Resource(id.clone()) && k == key
                }
                _ => false,
            })
            .count()
    };
    for id in [&local, &satellite] {
        assert_eq!(count(id, true, RESOURCE_AGENT_KEY), 1, "{id}: {frames:?}");
        assert_eq!(count(id, false, RESOURCE_AGENT_KEY), 1, "{id}: {frames:?}");
        assert!(peers.foreign_agent_subscribed.contains(id));
    }
    assert_eq!(count(&satellite, true, RESOURCE_ASKED_KEY), 1, "{frames:?}");
    assert_eq!(count(&local, true, RESOURCE_ASKED_KEY), 0, "{frames:?}");
}

/// Pruning keeps only live foreign panes' records and drops the send-once
/// subscription marker with them, so a re-spawned id re-subscribes.
#[test]
fn prune_foreign_agents_retains_only_live_foreign_panes() {
    let (live, stale) = (ResourceId::local(1), ResourceId::local(2));
    let mut peers = PeerWatch::default();
    peers.foreign_agents = [
        (live.clone(), AgentRecord::default()),
        (stale.clone(), AgentRecord::default()),
    ]
    .into();
    peers.foreign_agent_subscribed = [live.clone(), stale.clone()].into();
    peers.prune_agents(&HashSet::from([live.clone()]));
    let (cache, subscribed) = (&peers.foreign_agents, &peers.foreign_agent_subscribed);
    assert!(cache.contains_key(&live) && !cache.contains_key(&stale));
    assert!(subscribed.contains(&live) && !subscribed.contains(&stale));
    peers.prune_agents(&HashSet::new());
    let (cache, subscribed) = (&peers.foreign_agents, &peers.foreign_agent_subscribed);
    assert!(cache.is_empty() && subscribed.is_empty());
}

/// A refused agent GET frees only its correlation. Other pending replies
/// retain their own kind, and retry never duplicates the live subscription.
#[tokio::test]
async fn peer_watch_refusal_keeps_other_reads_correlated_and_subscribes_once() {
    use crate::attach::driver::subscriptions::{PeerRead, PeerReply};
    let (mut client, server) = pair();
    let mut peers = PeerWatch::default();
    let local = ResourceId::local(10);
    let satellite = ResourceId::satellite("edge", 7);
    let mut next = 100;
    peers
        .watch_agents(
            &mut client,
            vec![local.clone(), satellite.clone()],
            &mut next,
        )
        .await
        .expect("watch");
    let refusal = FrameKind::Error {
        request_id: Some(100),
        code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
        message: "refused".to_owned(),
    };
    assert!(matches!(
        peers.take_metadata_reply(refusal.clone()),
        Ok(PeerReply::Refused)
    ));
    assert!(
        peers.take_metadata_reply(refusal).is_err(),
        "a correlation is consumed once"
    );
    peers
        .watch_agents(
            &mut client,
            vec![local.clone(), satellite.clone()],
            &mut next,
        )
        .await
        .expect("retry");
    assert!(
        matches!(peers.take_metadata_reply(FrameKind::MetadataValue {
        request_id: 102, value: Some(b"1".to_vec()),
    }), Ok(PeerReply::Value(PeerRead::Asked(id), Some(value))) if id == satellite && value == b"1")
    );
    let frames = drain(client, server).await;
    let count = |id: &ResourceId, subscribe: bool| {
        frames
            .iter()
            .filter(|frame| match frame {
                FrameKind::GetMetadata { scope, key, .. } => {
                    !subscribe && *scope == Scope::Resource(id.clone()) && key == RESOURCE_AGENT_KEY
                }
                FrameKind::SubscribeMetadata { scope, key } => {
                    subscribe && *scope == Scope::Resource(id.clone()) && key == RESOURCE_AGENT_KEY
                }
                _ => false,
            })
            .count()
    };
    assert_eq!(count(&local, false), 2);
    assert_eq!(
        count(&satellite, false),
        1,
        "still-pending agent GET is not duplicated"
    );
    assert_eq!(count(&local, true), 1);
    assert_eq!(count(&satellite, true), 1);
}

/// Pruned panes cannot be resurrected by a late GET. A returning pane gets
/// both subscriptions again, including its satellite asked flag.
#[tokio::test]
async fn peer_watch_pruning_retires_stale_reads_and_rearms_returned_panes() {
    let (mut client, server) = pair();
    let mut peers = PeerWatch::default();
    let id = ResourceId::satellite("edge", 7);
    let mut next = 100;
    peers
        .watch_agents(&mut client, vec![id.clone()], &mut next)
        .await
        .expect("watch");
    peers.foreign_attention.insert(id.clone());
    peers.prune_agents(&HashSet::new());
    assert!(!peers.foreign_attention.contains(&id));
    for request_id in [100, 101] {
        assert!(
            peers
                .take_metadata_reply(FrameKind::MetadataValue {
                    request_id,
                    value: Some(b"late".to_vec()),
                })
                .is_err(),
            "the departed resource no longer owns this reply"
        );
    }
    peers
        .watch_agents(&mut client, vec![id.clone()], &mut next)
        .await
        .expect("return");
    let frames = drain(client, server).await;
    for key in [RESOURCE_AGENT_KEY, RESOURCE_ASKED_KEY] {
        assert_eq!(
            frames
                .iter()
                .filter(|frame| matches!(frame,
                    FrameKind::SubscribeMetadata { scope: Scope::Resource(target), key: actual }
                        if target == &id && actual == key
                ))
                .count(),
            2
        );
    }
}

/// Inventory refusal/expiry release held pushes and retire only the old
/// request. A late completion cannot settle a newly issued inventory read.
#[tokio::test]
async fn inventory_watch_refusal_and_expiry_retire_their_held_notices() {
    use crate::attach::driver::subscriptions::InventoryReply;
    let (mut client, server) = pair();
    let mut peers = PeerWatch::default();
    let mut next = 100;
    peers
        .request_inventory(&mut client, &mut next)
        .await
        .expect("request");
    peers
        .request_inventory(&mut client, &mut next)
        .await
        .expect("dedup");
    assert_eq!(next, 101);
    let push = |message: &str| FrameKind::Error {
        request_id: None,
        code: phux_protocol::wire::frame::ErrorCode::SatelliteUnreachable,
        message: message.to_owned(),
    };
    assert!(matches!(
        peers.take_inventory_reply(push("first")),
        Ok(InventoryReply::HeldUnreachable)
    ));
    assert!(matches!(peers.take_inventory_reply(FrameKind::Error {
        request_id: Some(100),
        code: phux_protocol::wire::frame::ErrorCode::Unsupported,
        message: "refused".to_owned(),
    }), Ok(InventoryReply::Refused(held)) if held == ["first"]));
    peers
        .request_inventory(&mut client, &mut next)
        .await
        .expect("second request");
    assert!(matches!(
        peers.take_inventory_reply(push("second")),
        Ok(InventoryReply::HeldUnreachable)
    ));
    let expired =
        peers.expire_inventory(std::time::Instant::now() + std::time::Duration::from_secs(60));
    assert_eq!(expired, Some(vec!["second".to_owned()]));
    peers
        .request_inventory(&mut client, &mut next)
        .await
        .expect("third request");
    assert!(
        peers
            .take_inventory_reply(FrameKind::CommandResult {
                request_id: 101,
                result: phux_protocol::wire::frame::CommandResult::Ok,
            })
            .is_err()
    );
    assert!(peers.inventory_in_flight());
    let frames = drain(client, server).await;
    assert_eq!(
        frames
            .iter()
            .filter(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: phux_protocol::wire::frame::Command::GetState { .. },
                    ..
                }
            ))
            .count(),
        3
    );
}
