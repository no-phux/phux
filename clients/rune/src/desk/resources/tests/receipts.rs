//! Correlated outcomes must settle even when one drain spans reconnects.

use super::*;
use phux_client_runtime::control::Event;
use phux_protocol::ids::OperationId;
use phux_protocol::wire::frame::{ErrorCode, SpawnResult};

fn home() -> (Arc<Client>, Runtime<Desk>) {
    fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    )
}

fn start(rt: &mut Runtime<Desk>, client: &Client, mirror: bool) -> u32 {
    rt.state.launch.phux = Some("/dev/phux".into());
    rt.state.launch.socket = Ok("/dev/socket".into());
    outbound(client);
    rt.update(|d, _| {
        if mirror {
            d.ask_mirror(Source::Page, "first".to_owned());
            d.confirm_pending();
        } else {
            d.new_tab();
        }
    });
    outbound(client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::SpawnResource { request_id, .. } => Some(request_id),
            _ => None,
        })
        .expect("a real spawn was submitted")
}

fn answer_spawn(client: &Client, request_id: u32) {
    client
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(id(2)),
        })
        .unwrap();
}

fn reattach(client: &Client, server: u8) {
    outbound(client);
    client.feed(hello(server)).unwrap();
    // The same numeric handle deliberately names a different resource after
    // replacement. Neither a receipt nor an old split may mutate that pane.
    let terminal =
        record(if server == 1 { 2 } else { 9 }, ResourceKind::Terminal).with_live(Some(id(2)));
    let records = [record(1, ResourceKind::AccessibilityTree), terminal];
    let attach_id = seed(client, &records).unwrap();
    install_arrangement(client, &records, &doc(LayoutNode::Leaf(id(1))));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_resources(
            records
                .iter()
                .map(|r| {
                    ResourceInfo::resource(r.live.clone().unwrap(), r.kind)
                        .with_session(Some(SessionId::new(1)))
                })
                .collect(),
        );
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    publish(client, 2, ResourceKind::Terminal, b"spawned terminal");
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    outbound(client);
}

fn advertise_spawn(client: &Client) {
    let request_id = client.refresh_organization().unwrap();
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(2), 0)
        .with_sessions(vec![
            SessionRecord::new(session(), "work", Revision::new(1))
                .with_live(Some(SessionId::new(1)))
                .with_arrangement(Some(arrangement())),
        ])
        .with_resources(vec![
            record(1, ResourceKind::AccessibilityTree),
            record(2, ResourceKind::Terminal),
        ]);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
}

fn pending_layout(client: &Client, rt: &mut Runtime<Desk>) -> (OperationId, u32) {
    let request = start(rt, client, false);
    answer_spawn(client, request);
    advertise_spawn(client);
    step(rt);
    let operation = *rt
        .state
        .layout_ops
        .iter()
        .next()
        .expect("split write in flight");
    let request = outbound(client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::ArrangementReplace { .. },
            } => Some(request_id),
            _ => None,
        })
        .unwrap();
    (operation, request)
}

fn drain(rt: &mut Runtime<Desk>, client: &Client) {
    let observation = client.take_activity();
    let opened = observation
        .events
        .iter()
        .rposition(|e| matches!(e, Event::ConnectionOpened { .. }))
        .unwrap();
    assert!(
        observation.events[..opened].iter().any(|e| matches!(
            e,
            Event::TerminalSpawned { .. } | Event::OrganizationOperation { .. }
        )),
        "receipt really precedes Opened in ONE observation"
    );
    rt.update(|d, tx| {
        d.observe(client, observation, tx);
    });
}

fn cancelled(rt: &Runtime<Desk>, client: &Client) {
    assert!(rt.state.pending_splits.is_empty());
    assert!(rt.state.pending_spawns.is_empty());
    assert!(rt.state.resources.starting.is_empty());
    assert!(rt.state.resources.producers.is_empty());
    assert!(rt.state.layout_ops.is_empty());
    assert!(!rt.state.splits_waiting());
    assert!(
        rt.state
            .wanted
            .as_ref()
            .is_none_or(|(target, _)| *target != id(2))
    );
    assert!(
        rt.state
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("Check the server’s panes")
    );
    assert!(
        !outbound(client).iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::ArrangementReplace { .. },
                ..
            }
        )),
        "never arrange a reused handle from another incarnation"
    );
}

#[test]
fn nonreplayable_mirror_failure_before_opened_settles_without_hello() {
    let (client, mut rt) = home();
    let request = start(&mut rt, &client, true);
    client.with_control(ControlPlane::connection_opened);
    drain(&mut rt, &client);
    assert!(!rt.state.pending_splits.contains_key(&request));
    assert!(!rt.state.resources.starting.contains_key(&request));
    let text = &rt.state.notice.as_ref().unwrap().text;
    assert!(text.contains("Mirror not started"));
    assert!(text.contains("connection replaced"));
    assert!(
        text.contains("localhost:9222"),
        "correlation retains producer prerequisites"
    );
    assert!(!rt.state.splits_waiting());
}

#[test]
fn spawn_failure_receipts_settle_on_same_and_replaced_servers() {
    for server in [1, 2] {
        let (client, mut rt) = home();
        let request = start(&mut rt, &client, false);
        client
            .feed(FrameKind::CommandResult {
                request_id: request,
                result: CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    message: "spawn denied".to_owned(),
                },
            })
            .unwrap();
        client.with_control(ControlPlane::connection_opened);
        reattach(&client, server);
        drain(&mut rt, &client);
        assert!(rt.state.pending_splits.is_empty());
        assert!(!rt.state.splits_waiting());
        assert!(
            rt.state
                .notice
                .as_ref()
                .unwrap()
                .text
                .contains("spawn denied")
        );
    }
}

#[test]
fn successful_spawn_receipts_place_only_on_the_exact_same_server() {
    for mirror in [false, true] {
        for server in [1, 2] {
            let (client, mut rt) = home();
            let request = start(&mut rt, &client, mirror);
            answer_spawn(&client, request);
            client.with_control(ControlPlane::connection_opened);
            reattach(&client, server);
            drain(&mut rt, &client);
            assert!(!rt.state.pending_splits.contains_key(&request));
            if server == 2 {
                cancelled(&rt, &client);
                continue;
            }
            assert!(
                rt.state.split_write.is_some(),
                "new tab gets its arrangement write"
            );
            assert_eq!(rt.state.layout_ops.len(), 1);
            assert_eq!(rt.state.resources.producers.contains_key(&id(2)), mirror);
            assert!(outbound(&client).iter().any(|frame| matches!(
                frame,
                FrameKind::Command {
                    command: Command::ArrangementReplace { .. },
                    ..
                }
            )));
        }
    }
}

#[test]
fn old_success_waits_for_fresh_hello_not_the_cached_server_identity() {
    for server in [1, 2] {
        let (client, mut rt) = home();
        let request = start(&mut rt, &client, false);
        answer_spawn(&client, request);
        client.with_control(ControlPlane::connection_opened);
        drain(&mut rt, &client);
        assert_eq!(
            client.server().unwrap().id,
            vec![1; 16],
            "runtime caches prior HELLO"
        );
        assert!(rt.state.pending_splits.contains_key(&request));
        assert!(rt.state.split_write.is_none());
        rt.update(|d, _| d.place_pending_splits(&client));
        outbound(&client);
        reattach(&client, server);
        step(&mut rt);
        if server == 1 {
            assert!(rt.state.split_write.is_some());
            assert!(!rt.state.pending_splits.contains_key(&request));
        } else {
            cancelled(&rt, &client);
        }
    }
}

#[test]
fn plain_terminal_spawn_receipt_cannot_focus_a_reused_handle() {
    for server in [1, 2] {
        let (client, mut rt) = home();
        outbound(&client);
        rt.update(|d, _| d.new_terminal());
        let request = rt.state.pending_spawns[0];
        answer_spawn(&client, request);
        client.with_control(ControlPlane::connection_opened);
        reattach(&client, server);
        drain(&mut rt, &client);
        assert!(rt.state.pending_spawns.is_empty());
        if server == 1 {
            assert_eq!(
                rt.state.wanted.as_ref().map(|(id, _)| id.clone()),
                Some(id(2))
            );
        } else {
            cancelled(&rt, &client);
        }
    }
}

#[test]
fn organization_outcomes_before_opened_settle_or_cancel_layout_correlations() {
    for server in [1, 2] {
        for success in [false, true] {
            let (client, mut rt) = home();
            let (operation, request_id) = pending_layout(&client, &mut rt);
            let result = if success {
                CommandResult::Accepted { operation }
            } else {
                CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    message: "layout denied".to_owned(),
                }
            };
            client
                .feed(FrameKind::CommandResult { request_id, result })
                .unwrap();
            client.with_control(ControlPlane::connection_opened);
            reattach(&client, server);
            drain(&mut rt, &client);
            assert!(
                !rt.state.layout_ops.contains(&operation),
                "old outcome is not dropped"
            );
            if server == 2 {
                cancelled(&rt, &client);
            } else {
                assert!(
                    rt.state.split_write.is_some(),
                    "settled write waits for replica revision"
                );
                assert!(
                    outbound(&client).iter().any(|frame| matches!(
                        frame,
                        FrameKind::Command {
                            command: Command::ArrangementGet { .. },
                            ..
                        }
                    )),
                    "receipt requested replica reconciliation"
                );
            }
        }
    }
}

#[test]
fn an_unobserved_intermediate_socket_does_not_borrow_the_previous_hello_identity() {
    let (client, mut rt) = home();
    client.with_control(ControlPlane::connection_opened);
    step(&mut rt); // no HELLO yet; the remembered server identity is stale.
    reattach(&client, 2); // the UI never observes this server's identity.
    let request = client.spawn_terminal(phux_client_runtime::control::SpawnRequest::default());
    rt.state
        .pending_splits
        .insert(request, (Some(session()), None));
    answer_spawn(&client, request);
    client.with_control(ControlPlane::connection_opened);
    reattach(&client, 1); // back to the original server, reusing @2.
    drain(&mut rt, &client);
    cancelled(&rt, &client);
}

#[test]
fn unknown_organization_outcome_does_not_masquerade_as_a_success_or_retry() {
    let (client, mut rt) = home();
    let (operation, request_id) = pending_layout(&client, &mut rt);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::Unknown(65535),
                message: "unreadable layout reply".to_owned(),
            },
        })
        .unwrap();
    client.with_control(ControlPlane::connection_opened);
    reattach(&client, 1);
    drain(&mut rt, &client);
    assert!(!rt.state.layout_ops.contains(&operation));
    assert!(rt.state.split_write.is_none());
    assert!(
        rt.state
            .wanted
            .as_ref()
            .is_none_or(|(target, _)| *target != id(2))
    );
    assert!(
        rt.state
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("unreadable layout reply")
    );
    assert!(
        outbound(&client).is_empty(),
        "unknown receipt never generates a new layout operation"
    );
}

#[test]
fn failed_organization_outcome_settles_before_the_new_hello_without_retrying() {
    let (client, mut rt) = home();
    let (operation, request_id) = pending_layout(&client, &mut rt);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "layout denied before reconnect".to_owned(),
            },
        })
        .unwrap();
    client.with_control(ControlPlane::connection_opened);
    drain(&mut rt, &client);
    assert!(!rt.state.layout_ops.contains(&operation));
    assert!(!rt.state.splits_waiting());
    assert!(
        rt.state
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("layout denied before reconnect")
    );
    assert!(!outbound(&client).iter().any(|frame| matches!(
        frame,
        FrameKind::Command {
            command: Command::ArrangementReplace { .. },
            ..
        }
    )));
}
