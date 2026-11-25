//! Popup creation, receipts and lifecycle events cross the actual Desk entrypoints.

use super::*;
use crate::test_support::{self, terminal::Replica};
use phux_client_runtime::control::Event;
use phux_protocol::{
    caps::{ServerCapabilities, ServerFeature, ServerFeatureSet},
    wire::frame::{CloseReason, Command, ErrorCode, FrameKind, SpawnResult},
};

fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}
fn replica() -> Replica {
    Replica::with_capabilities(
        80,
        24,
        2,
        ServerCapabilities::new()
            .with_features(ServerFeatureSet::with(&[ServerFeature::CloseTabResources])),
    )
}
fn window(replica: &Replica) -> rune::Runtime<Desk> {
    let mut desk = replica.window();
    desk.show(id(1), "budget");
    test_support::runtime(
        desk,
        crate::ui::view,
        test_support::SIZE,
        rune::Env::new(true, 1.0),
    )
}
fn open(rt: &mut rune::Runtime<Desk>, line: &str) -> u32 {
    rt.update(|desk, _| {
        desk.open_popup(line);
        desk.popup_laid_out([600.0, 400.0]);
    });
    rt.state.connection.popup.request.unwrap()
}
fn spawned(client: &Client, request: u32) {
    client
        .feed(FrameKind::ResourceSpawned {
            request_id: request,
            result: SpawnResult::Ok(id(2)),
        })
        .unwrap();
}
fn observe(rt: &mut rune::Runtime<Desk>, client: &Client) {
    let observation = client.take_activity();
    rt.update(|desk, tx| {
        desk.observe(client, observation, tx);
    });
}
fn assert_no_termination_or_detach(client: &Client) {
    assert!(
        !client.take_outbound().iter().any(|bytes| matches!(
            FrameKind::decode(bytes).unwrap().0,
            FrameKind::Command {
                command: Command::CloseTabResources { .. }
                    | Command::KillResource { .. }
                    | Command::ResourceTerminate { .. }
                    | Command::DetachResource { .. },
                ..
            }
        )),
        "retiring UI interest cannot kill execution or remove a sibling subscription"
    );
}

#[test]
fn a_cancelled_old_popup_receipt_cannot_cancel_or_replace_the_latest_popup() {
    for failure in [false, true] {
        let replica = replica();
        let client = &replica.client;
        let mut rt = window(&replica);
        let old = open(&mut rt, "old command");
        rt.update(|desk, _| desk.close_popup(client, false));
        let latest = open(&mut rt, "latest command");
        assert_ne!(old, latest);
        if failure {
            crate::wire_fixture::refuse(client, old, ErrorCode::InvalidCommand, "old failure");
        } else {
            spawned(client, old);
        }
        observe(&mut rt, client);
        assert_eq!(rt.state.connection.popup.command(), Some("latest command"));
        assert_eq!(rt.state.connection.popup.request, Some(latest));
        assert!(rt.state.connection.popup.terminal().is_none());
        spawned(client, latest);
        observe(&mut rt, client);
        assert!(rt.state.connection.popup.view_of(&id(2)).is_some());
        assert_eq!(rt.state.connection.popup.command(), Some("latest command"));
        assert!(client.acquire_view(replica.views[1]).is_some());
        assert_no_termination_or_detach(client);
    }
}

#[test]
fn old_popup_receipts_retire_before_reused_id_stream_and_bootstrap_can_be_admitted() {
    for server in [1, 2] {
        let replica = replica();
        let client = &replica.client;
        let mut rt = window(&replica);
        let request = open(&mut rt, "old command");
        spawned(client, request);
        replica.replace_server(server);
        let source = client.view_source(&id(2), Some(&[server; 16]), 2).unwrap();
        let sibling = client.create_view(&source).unwrap();
        let _ = client.take_outbound();
        observe(&mut rt, client);
        assert!(rt.state.connection.popup.command().is_none());
        assert!(rt.state.connection.popup.request.is_none());
        assert!(rt.state.connection.popup.terminal().is_none());
        assert!(client.acquire_view(sibling).is_some());
        assert_no_termination_or_detach(client);
        client.destroy_view(sibling).unwrap();
    }
}

#[test]
fn delayed_popup_layout_never_spawns_on_a_replaced_server_or_runtime_owner() {
    for replace_runtime in [false, true] {
        let original = replica();
        let mut rt = window(&original);
        rt.update(|desk, _| desk.open_popup("must not be redirected"));
        let replacement = replica();
        let client = if replace_runtime {
            rt.state.connection.client = Some(replacement.client.clone());
            &replacement.client
        } else {
            original.replace_server(2);
            &original.client
        };
        let _ = client.take_outbound();
        rt.update(|desk, _| desk.popup_laid_out([600.0, 400.0]));
        assert!(rt.state.connection.popup.command().is_none());
        assert!(
            client.take_outbound().is_empty(),
            "the stale layout cannot submit a spawn"
        );
    }
}

#[test]
fn a_captured_popup_source_is_reused_offline_but_never_upgraded_after_generation_replacement() {
    for replace_generation in [false, true] {
        let replica = replica();
        let client = &replica.client;
        let mut rt = window(&replica);
        let request = open(&mut rt, "command");
        spawned(client, request);
        observe(&mut rt, client);
        let old_view = rt.state.connection.popup.view_of(&id(2)).unwrap();
        client.destroy_view(old_view).unwrap();
        if replace_generation {
            crate::wire_fixture::bootstrap(client, &id(2), [9; 2], [80, 24], b"new generation");
        } else {
            client.with_control(|control| control.connection_lost(None));
        }
        let sibling_source = client.view_source(&id(2), Some(&[1; 16]), 1).unwrap();
        let sibling = client.create_view(&sibling_source).unwrap();
        let _ = client.take_outbound();
        rt.update(|desk, tx| {
            desk.tend_popup(client, tx);
        });
        assert_eq!(
            rt.state.connection.popup.command().is_none(),
            replace_generation
        );
        if !replace_generation {
            let reopened = rt.state.connection.popup.view_of(&id(2)).unwrap();
            assert_ne!(reopened, old_view);
            rt.update(|desk, _| desk.close_popup(client, false));
            assert!(client.view_generation(reopened).is_none());
        }
        assert!(client.acquire_view(sibling).is_some());
        assert_no_termination_or_detach(client);
        client.destroy_view(sibling).unwrap();
    }
}

#[test]
fn exit_notification_releases_only_the_popup_view_not_execution_or_sibling_views() {
    let replica = replica();
    let client = &replica.client;
    let mut rt = window(&replica);
    let request = open(&mut rt, "command");
    spawned(client, request);
    observe(&mut rt, client);
    let popup_view = rt.state.connection.popup.view_of(&id(2)).unwrap();
    let _ = client.take_outbound();
    let mut observation = client.take_activity();
    observation.events.push(Event::Exited {
        terminal_id: id(2),
        exit_status: Some(0),
        signal: None,
        reason: CloseReason::Unknown,
    });
    rt.update(|desk, tx| {
        desk.observe(client, observation, tx);
    });
    assert!(rt.state.connection.popup.command().is_none());
    assert!(client.view_generation(popup_view).is_none());
    assert!(client.acquire_view(replica.views[1]).is_some());
    assert_no_termination_or_detach(client);
}

#[test]
fn lifecycle_events_before_opened_cannot_dismiss_the_newest_popup_with_the_same_id() {
    let replica = replica();
    let client = &replica.client;
    let mut rt = window(&replica);
    let old = open(&mut rt, "old command");
    spawned(client, old);
    observe(&mut rt, client);
    client
        .feed(FrameKind::ResourceClosed {
            terminal_id: id(2),
            exit_status: Some(0),
            signal: None,
            reason: CloseReason::Unknown,
        })
        .unwrap();
    replica.replace_server(2);
    let source = client.view_source(&id(2), Some(&[2; 16]), 2).unwrap();
    rt.update(|desk, _| {
        desk.close_popup(client, false);
        // A qualified fresh observation establishes the new intent while the
        // old lifecycle event is still queued before ConnectionOpened.
        desk.connection.seed_view_sources(vec![2; 16], &[source]);
    });
    let latest = open(&mut rt, "latest command");
    spawned(client, latest);
    let mut observation = client.take_activity();
    observation.events.insert(
        0,
        Event::Exited {
            terminal_id: id(2),
            exit_status: Some(0),
            signal: None,
            reason: CloseReason::Unknown,
        },
    );
    assert!(
        observation
            .events
            .iter()
            .any(|event| matches!(event, Event::ConnectionOpened { .. }))
    );
    let _ = client.take_outbound();
    rt.update(|desk, tx| {
        desk.observe(client, observation, tx);
    });
    assert_eq!(rt.state.connection.popup.command(), Some("latest command"));
    assert!(rt.state.connection.popup.view_of(&id(2)).is_some());
    assert_no_termination_or_detach(client);
}

#[test]
fn an_old_runtime_receipt_with_reused_request_and_resource_ids_cannot_touch_the_new_popup() {
    let original = replica();
    let replacement = replica();
    let mut rt = window(&original);
    let old = open(&mut rt, "old command");
    rt.update(|desk, _| {
        desk.replace_connection(crate::desk::link::Replacement::Retry);
        desk.connection.client = Some(replacement.client.clone());
        desk.connection.status = replacement.client.status();
        desk.connection.topology = replacement.client.topology();
        desk.connection
            .seed_view_sources(vec![1; 16], &replacement.sources);
        desk.show(id(1), "budget");
    });
    let latest = open(&mut rt, "latest command");
    assert_eq!(
        old, latest,
        "the fixture deliberately reuses the request namespace"
    );
    spawned(&original.client, old);
    observe(&mut rt, &original.client);
    assert_eq!(rt.state.connection.popup.request, Some(latest));
    assert_eq!(rt.state.connection.popup.command(), Some("latest command"));
    assert!(
        !rt.state
            .popup_spawned(&original.client, old, Some(&id(2)), None)
    );
    spawned(&replacement.client, latest);
    observe(&mut rt, &replacement.client);
    assert!(rt.state.connection.popup.view_of(&id(2)).is_some());
    assert_no_termination_or_detach(&replacement.client);
}

#[test]
fn confirmed_popup_close_cannot_terminate_reused_id_before_the_ui_drains_new_hello() {
    let replica = replica();
    let client = &replica.client;
    let mut rt = window(&replica);
    let request = open(&mut rt, "original command");
    spawned(client, request);
    observe(&mut rt, client);
    let _ = client.take_outbound();

    // The confirmation still belongs to the displayed old popup. The backend
    // already knows a new server with all the same short ids.
    replica.replace_server(2);
    let source = client
        .view_source(&id(2), Some(&[2; 16]), client.connection_epoch())
        .unwrap();
    let independent = client.create_view(&source).unwrap();
    let _ = client.take_outbound();
    rt.update(|desk, _| desk.close_now(&id(2)));
    assert!(rt.state.connection.popup.command().is_none());
    assert!(client.acquire_view(independent).is_some());
    assert!(client.has_projection(&id(2)));
    assert_no_termination_or_detach(client);
}
