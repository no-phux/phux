use super::*;
use phux_protocol::wire::frame::{Command, FrameKind};

fn moved(client: &Client, desk: &mut Desk) {
    let (request, operation, result) = begin(desk, client);
    reply(
        desk,
        client,
        request,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(2),
        )))),
    );
    assert_eq!(result.try_recv().unwrap(), Ok(()));
}

fn no_layout_write(client: &Client) {
    assert!(
        !outbound(client).iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::ArrangementReplace { .. },
                ..
            }
        )),
        "a cancelled placement must never write the newly selected session"
    );
}

fn layout_write(client: &Client) -> (u32, OperationId) {
    outbound(client)
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command:
                    Command::ArrangementReplace {
                        operation,
                        arrangement: target,
                        ..
                    },
            } => {
                assert_eq!(*target, arrangement(1), "only A owns this placement");
                Some((*request_id, *operation))
            }
            _ => None,
        })
        .expect("A arrangement write")
}

#[test]
fn switching_after_the_move_receipt_cancels_the_wait_for_its_original_arrangement() {
    let (client, mut desk) = connected();
    moved(&client, &mut desk);
    assert_eq!(desk.unplaced_splits.len(), 1, "A arrangement is withheld");
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    install_arrangement(&client, 2);
    outbound(&client);
    desk.place_pending_splits(&client);
    no_layout_write(&client);
    assert!(!desk.splits_waiting());
    assert!(
        desk.wanted.is_none(),
        "no fallback may focus the A terminal from B"
    );
    assert!(
        desk.notice
            .as_ref()
            .unwrap()
            .text
            .contains("selected session changed")
    );
    install_arrangement(&client, 1);
    desk.place_pending_splits(&client);
    no_layout_write(&client);
    assert_eq!(
        client.selected_session(),
        Some(2),
        "cancellation does not steal selection"
    );
}

#[test]
fn the_deferred_move_still_writes_a_tab_into_a_when_its_arrangement_arrives() {
    let (client, mut desk) = connected();
    moved(&client, &mut desk);
    assert_eq!(desk.unplaced_splits.len(), 1);
    install_arrangement(&client, 1);
    desk.place_pending_splits(&client);
    layout_write(&client);
    assert!(desk.unplaced_splits.is_empty());
    assert!(desk.split_write.is_some());
}

#[test]
fn ordinary_queued_tabs_and_splits_also_cancel_instead_of_following_selection() {
    use phux_client_core::layout::SplitDir;
    for place in [None, Some((terminal(), SplitDir::Horizontal))] {
        let (client, mut desk) = connected();
        desk.place_split(&client, &terminal(), place, Some(session(1)));
        assert_eq!(desk.unplaced_splits.len(), 1);
        client.attach_session(AttachTarget::ById(SessionId::new(2)));
        install_arrangement(&client, 2);
        outbound(&client);
        desk.place_pending_splits(&client);
        no_layout_write(&client);
        assert!(!desk.splits_waiting());
        assert!(desk.wanted.is_none());
        assert!(
            desk.notice
                .as_ref()
                .unwrap()
                .text
                .contains("selected session changed")
        );
    }
}

#[test]
fn tab_split_and_mirror_spawns_keep_the_destination_from_before_the_spawn_receipt() {
    use crate::desk::resources::Source;
    use phux_client_core::layout::SplitDir;
    use phux_protocol::wire::frame::SpawnResult;
    for action in 0..3 {
        let (client, mut desk) = connected();
        desk.shown = Some(Shown {
            resource: terminal(),
            view: None,
        });
        desk.launch.phux = Some("/dev/phux".into());
        desk.launch.socket = Ok("/dev/socket".into());
        match action {
            0 => desk.new_tab(),
            1 => desk.split(SplitDir::Horizontal),
            _ => desk.start_mirror(Source::Page, "first"),
        }
        let request_id = outbound(&client)
            .iter()
            .find_map(|frame| match frame {
                FrameKind::SpawnResource { request_id, .. } => Some(*request_id),
                _ => None,
            })
            .expect("embedded spawn request");
        assert!(desk.pending_splits.contains_key(&request_id));
        client.attach_session(AttachTarget::ById(SessionId::new(2)));
        install_arrangement(&client, 2);
        client
            .feed(FrameKind::ResourceSpawned {
                request_id,
                result: SpawnResult::Ok(ResourceId::local(8)),
            })
            .unwrap();
        for event in client.take_activity().events {
            desk.apply(&client, event, &mut rune::Tx::default());
        }
        no_layout_write(&client);
        assert!(desk.pending_splits.is_empty());
        assert!(!desk.splits_waiting());
        assert!(desk.wanted.is_none());
        assert!(
            desk.notice
                .as_ref()
                .unwrap()
                .text
                .contains("selected session changed")
        );
    }
}

#[test]
fn a_refused_in_flight_a_write_never_retries_or_falls_back_in_b() {
    let (client, mut desk) = connected();
    install_arrangement(&client, 1);
    moved(&client, &mut desk);
    let (request, _) = layout_write(&client);
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    install_arrangement(&client, 2);
    outbound(&client);
    reply(
        &mut desk,
        &client,
        request,
        CommandResult::Refused(Refusal::Conflict {
            current: Revision::new(2),
        }),
    );
    no_layout_write(&client);
    assert!(!desk.splits_waiting());
    assert!(
        desk.wanted.is_none(),
        "neither retry nor fallback focuses A in B"
    );
    assert!(
        desk.notice
            .as_ref()
            .unwrap()
            .text
            .contains("selected session changed")
    );
}

#[test]
fn a_cancelled_in_flight_write_ignores_late_success_and_refusal_receipts() {
    for success in [false, true] {
        let (client, mut desk) = connected();
        install_arrangement(&client, 1);
        moved(&client, &mut desk);
        let (request_id, operation) = layout_write(&client);
        client.attach_session(AttachTarget::ById(SessionId::new(2)));
        install_arrangement(&client, 2);
        outbound(&client);
        desk.place_pending_splits(&client);
        assert!(!desk.layout_ops.contains(&operation));
        assert!(!desk.splits_waiting());
        let result = if success {
            CommandResult::Accepted { operation }
        } else {
            CommandResult::Refused(Refusal::NotFound)
        };
        client
            .feed(FrameKind::CommandResult { request_id, result })
            .unwrap();
        for event in client.take_activity().events {
            if let Event::OrganizationOperation { operation, outcome } = event {
                assert!(!desk.apply(
                    &client,
                    Event::OrganizationOperation { operation, outcome },
                    &mut rune::Tx::default()
                ));
            }
        }
        desk.place_pending_splits(&client);
        no_layout_write(&client);
        assert!(desk.wanted.is_none());
        assert!(
            desk.notice
                .as_ref()
                .unwrap()
                .text
                .contains("selected session changed")
        );
    }
}
