use crate::testkit::{EndOfScript, ScriptSpec, serve_one};
use crate::watch::{WatchItem, watch_events};
use phux_protocol::ids::{ResourceKind, WindowId};
use phux_protocol::wire::frame::AgentEvent;
use phux_protocol::wire::info::ResourceInfo;

use super::{FrameKind, RESOURCE_AGENT_KEY, ResourceId, Scope, agent_record, snapshot};

/// A server-wide watch subscribes each local Terminal's record, excluding
/// satellite Terminals and non-Terminal resources.
#[tokio::test]
async fn a_server_wide_watch_streams_multiple_local_agents_and_filters_resources() {
    let first = ResourceId::local(7);
    let second = ResourceId::local(8);
    let child = ResourceId::local(9);
    let satellite = ResourceId::satellite("edge", 10);
    let dir = tempfile::tempdir().expect("temp dir");
    let spec = ScriptSpec::new()
        .state(snapshot(vec![
            ResourceInfo::new(first.clone(), WindowId::new(1), 80, 24),
            ResourceInfo::new(second.clone(), WindowId::new(1), 80, 24),
            ResourceInfo::resource(child.clone(), ResourceKind::AgentSession),
            ResourceInfo::new(satellite.clone(), WindowId::new(1), 80, 24),
        ]))
        .push(FrameKind::MetadataChanged {
            scope: Scope::Resource(first.clone()),
            key: "phux.other/v1".to_owned(),
            value: Some(b"not an agent record".to_vec()),
            actor: None,
        })
        .push_after_subscribe(
            Scope::Resource(first.clone()),
            RESOURCE_AGENT_KEY,
            vec![agent_record(
                &first,
                r#"{"name":"first","state":"working"}"#,
            )],
        )
        .push_after_subscribe(
            Scope::Resource(second.clone()),
            RESOURCE_AGENT_KEY,
            vec![agent_record(
                &second,
                r#"{"name":"second","state":"blocked"}"#,
            )],
        )
        .end(EndOfScript::ServeUntilDetach);
    let (socket, server) = serve_one(dir.path(), spec);
    let mut items = Vec::new();
    watch_events(&socket, None, |item| {
        items.push(item);
        items
            .iter()
            .filter(|item| matches!(item, WatchItem::AgentState(_)))
            .count()
            < 2
    })
    .await
    .expect("fleet stream");
    let seen = server.await.expect("scripted server task");

    let updates: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            WatchItem::AgentState(update) => Some(update),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 2, "the foreign key is filtered: {items:?}");
    assert!(
        updates
            .iter()
            .any(|update| update.terminal.as_ref() == Some(&first))
    );
    assert!(
        updates
            .iter()
            .any(|update| update.terminal.as_ref() == Some(&second))
    );
    for excluded in [child, satellite] {
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::SubscribeMetadata { scope: Scope::Resource(id), .. }
                    if *id == excluded
            )),
            "only local Terminal resources are subscribed; sent {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_server_wide_watch_follows_a_new_terminal_without_a_wire_wildcard() {
    let first = ResourceId::local(7);
    let spawned = ResourceId::local(8);
    let dir = tempfile::tempdir().expect("temp dir");
    let update = agent_record(&spawned, r#"{"name":"new-agent","state":"working"}"#);
    let spawned_event = FrameKind::Event {
        terminal: Some(spawned.clone()),
        event: AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            parent: None,
        },
        stamp: None,
    };
    let (socket, server) = serve_one(
        dir.path(),
        ScriptSpec::new()
            .state(snapshot(vec![ResourceInfo::new(
                first,
                WindowId::new(1),
                80,
                24,
            )]))
            .push(spawned_event)
            .push_after_subscribe(
                Scope::Resource(spawned.clone()),
                RESOURCE_AGENT_KEY,
                vec![update],
            )
            .end(EndOfScript::ServeUntilDetach),
    );
    let mut items = Vec::new();
    watch_events(&socket, None, |item| {
        let stop = matches!(
            &item,
            WatchItem::AgentState(update)
                if update.terminal.as_ref() == Some(&ResourceId::local(8))
        );
        items.push(item);
        !stop
    })
    .await
    .expect("fleet stream");
    let seen = server.await.expect("scripted server task");

    assert!(
        seen.iter()
            .any(|f| matches!(f, FrameKind::SubscribeEvents { terminal: None, .. })),
        "sent {seen:?}"
    );
    assert!(
        seen.iter().any(|f| matches!(
            f,
            FrameKind::SubscribeMetadata { scope: Scope::Resource(id), key }
                if *id == ResourceId::local(8) && key == RESOURCE_AGENT_KEY
        )),
        "the spawn event must extend the L3 subscription set; sent {seen:?}"
    );
    assert!(items.iter().any(|item| matches!(
        item,
        WatchItem::AgentState(update)
            if update.terminal.as_ref() == Some(&ResourceId::local(8))
    )));
}
