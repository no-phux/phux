//! A hub journals the events it relays in its own journal (ADR-0123,
//! `docs/spec/L1.md` §7.3): a satellite's `EVENT` reaches a hub consumer
//! stamped with the hub's `seq`, in the hub's order, beside the hub's local
//! events, so the consumer's one cursor is hub-scoped.

use super::*;

/// Every `EVENT` and the reply to `request_id`, until both the reply and an
/// `asked` with `asked_id` have arrived.
async fn reply_and_asked(
    hub: &mut UnixStream,
    request_id: u32,
    asked_id: &str,
) -> (CommandResult, FrameKind) {
    let deadline = Instant::now() + STEP_DEADLINE;
    let mut result = None;
    let mut event = None;
    while result.is_none() || event.is_none() {
        assert!(
            Instant::now() < deadline,
            "no reply and asked for {asked_id}"
        );
        let (_, frame) = recv_typed(hub).await;
        match frame {
            FrameKind::CommandResult {
                request_id: got,
                result: got_result,
            } if got == request_id => result = Some(got_result),
            FrameKind::Event {
                event: AgentEvent::Asked { ref id, .. },
                ..
            } if id == asked_id => event = Some(frame),
            _ => {}
        }
    }
    (result.unwrap(), event.unwrap())
}

fn stamp_of(frame: &FrameKind) -> &phux_protocol::wire::frame::EventStamp {
    let FrameKind::Event {
        stamp: Some(stamp), ..
    } = frame
    else {
        panic!("a stamped event, got {frame:?}");
    };
    stamp
}

fn ask(terminal: &ResourceId, id: &str) -> Command {
    Command::ReportAsked {
        terminal_id: terminal.clone(),
        id: id.to_owned(),
        question: format!("{id}?"),
        suggestions: Vec::new(),
        elapsed_seconds: None,
    }
}

#[test]
fn relayed_satellite_events_are_restamped_in_hub_order() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"));
        let sat_pane = discover_satellite_pane(ws_port).await;
        let sat_id = ResourceId::satellite("sat", sat_pane);
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) = spawn_hub_with_session(
            hub_path.clone(),
            vec![satellite_entry("sat", ws_port)],
            Some("local"),
        );
        let mut hub = wait_for_socket(&hub_path, STEP_DEADLINE).await;
        send_frame(&mut hub, &phux_server_testkit::attach_by_name("local")).await;
        let local_pane = loop {
            let (_, frame) = recv_typed(&mut hub).await;
            if let FrameKind::Attached { snapshot, .. } = frame {
                break snapshot.focused_resource;
            }
        };
        // The link must be up before a satellite scope can be subscribed.
        let _ = get_screen_until_ok(&mut hub, sat_pane).await;

        for terminal in [None, Some(sat_id.clone())] {
            send_frame(
                &mut hub,
                &FrameKind::SubscribeEvents {
                    terminal,
                    after_seq: None,
                },
            )
            .await;
        }

        send_frame(
            &mut hub,
            &FrameKind::Command {
                request_id: 1,
                command: ask(&local_pane, "local-before"),
            },
        )
        .await;
        let (result, before) = reply_and_asked(&mut hub, 1, "local-before").await;
        assert_eq!(result, CommandResult::Ok);

        send_frame(
            &mut hub,
            &FrameKind::Command {
                request_id: 2,
                command: ask(&sat_id, "relayed"),
            },
        )
        .await;
        let (result, relayed) = reply_and_asked(&mut hub, 2, "relayed").await;
        assert_eq!(result, CommandResult::Ok);

        send_frame(
            &mut hub,
            &FrameKind::Command {
                request_id: 3,
                command: ask(&local_pane, "local-after"),
            },
        )
        .await;
        let (result, after) = reply_and_asked(&mut hub, 3, "local-after").await;
        assert_eq!(result, CommandResult::Ok);

        let (a, b, c) = (stamp_of(&before), stamp_of(&relayed), stamp_of(&after));
        assert!(
            a.seq < b.seq && b.seq < c.seq,
            "the relayed event takes the hub's next seq between its local neighbours: \
             {} < {} < {}",
            a.seq,
            b.seq,
            c.seq
        );
        assert!(b.ts_ms > 0, "the satellite's timestamp crosses the hub");
        let FrameKind::Event { terminal, .. } = &relayed else {
            unreachable!()
        };
        assert_eq!(
            terminal.as_ref(),
            Some(&sat_id),
            "still re-tagged Satellite"
        );

        drop(hub);
        let _ = hub_shutdown.send(());
        let _ = sat_shutdown.send(());
        let _ = tokio::time::timeout(STEP_DEADLINE, hub_task).await;
        let _ = tokio::time::timeout(STEP_DEADLINE, sat_task).await;
    });
}
