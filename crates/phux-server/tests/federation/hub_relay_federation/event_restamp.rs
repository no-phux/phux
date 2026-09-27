//! A relayed satellite `EVENT` is journaled by the hub (ADR-0123,
//! `docs/spec/L1.md` §7.3): stamped with the hub's `seq`, in hub order,
//! between the hub's local events.

use super::*;

fn ask(terminal: &ResourceId, id: &str) -> Command {
    Command::ReportAsked {
        terminal_id: terminal.clone(),
        id: id.to_owned(),
        question: format!("{id}?"),
        suggestions: Vec::new(),
        elapsed_seconds: None,
    }
}

/// Report an `asked` with `id` and return its stamped `EVENT`.
async fn asked_event(
    hub: &mut UnixStream,
    request_id: u32,
    terminal: &ResourceId,
    id: &str,
) -> FrameKind {
    let is_it = |frame: &FrameKind| match frame {
        FrameKind::Event {
            event: AgentEvent::Asked { id: got, .. },
            ..
        } => got == id,
        _ => false,
    };
    let (result, frames) = command_via_hub(hub, request_id, ask(terminal, id)).await;
    assert_eq!(result, CommandResult::Ok);
    match frames.into_iter().find(is_it) {
        Some(frame) => frame,
        None => next_event(hub, is_it).await,
    }
}

fn seq_of(frame: &FrameKind) -> u64 {
    let FrameKind::Event {
        stamp: Some(stamp), ..
    } = frame
    else {
        panic!("a stamped event, got {frame:?}");
    };
    stamp.seq
}

#[test]
fn relayed_satellite_events_are_restamped_in_hub_order() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(None, Some("local")).await;
        let sat_id = fed.sat_id();
        let mut hub = fed.linked_hub().await;
        send_frame(&mut hub, &phux_server_testkit::attach_by_name("local")).await;
        let local_pane = phux_server_testkit::recv_until(&mut hub, |_, frame| match frame {
            FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
            _ => None,
        })
        .await;
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

        let before = asked_event(&mut hub, 1, &local_pane, "local-before").await;
        let relayed = asked_event(&mut hub, 2, &sat_id, "relayed").await;
        let after = asked_event(&mut hub, 3, &local_pane, "local-after").await;
        let (a, b, c) = (seq_of(&before), seq_of(&relayed), seq_of(&after));
        assert!(
            a < b && b < c,
            "relayed event takes the hub's next seq: {a} < {b} < {c}"
        );
        let FrameKind::Event {
            terminal,
            stamp: Some(stamp),
            ..
        } = &relayed
        else {
            unreachable!()
        };
        assert!(stamp.ts_ms > 0, "the satellite's timestamp crosses the hub");
        assert_eq!(
            terminal.as_ref(),
            Some(&sat_id),
            "still re-tagged Satellite"
        );

        drop(hub);
        fed.shutdown().await;
    });
}
