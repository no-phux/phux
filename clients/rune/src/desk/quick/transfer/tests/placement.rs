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
    assert_eq!(
        desk.connection.unplaced_splits.len(),
        1,
        "A arrangement is withheld"
    );
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    install_arrangement(&client, 2);
    outbound(&client);
    desk.place_pending_splits(&client);
    no_layout_write(&client);
    assert!(!desk.splits_waiting());
    assert!(
        desk.connection.wanted.is_none(),
        "no fallback may focus the A terminal from B"
    );
    assert!(crate::test_support::notice(&desk).contains("selected session changed"));
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
    assert_eq!(desk.connection.unplaced_splits.len(), 1);
    install_arrangement(&client, 1);
    desk.place_pending_splits(&client);
    layout_write(&client);
    assert!(desk.connection.unplaced_splits.is_empty());
    assert!(desk.connection.split_write.is_some());
}

#[test]
fn ordinary_queued_tabs_and_splits_also_cancel_instead_of_following_selection() {
    use phux_client_core::layout::SplitDir;
    for place in [None, Some((terminal(), SplitDir::Horizontal))] {
        let (client, mut desk) = connected();
        desk.place_split(&client, &terminal(), place, Some(session(1)));
        assert_eq!(desk.connection.unplaced_splits.len(), 1);
        client.attach_session(AttachTarget::ById(SessionId::new(2)));
        install_arrangement(&client, 2);
        outbound(&client);
        desk.place_pending_splits(&client);
        no_layout_write(&client);
        assert!(!desk.splits_waiting());
        assert!(desk.connection.wanted.is_none());
        assert!(crate::test_support::notice(&desk).contains("selected session changed"));
    }
}

#[test]
fn tab_split_and_mirror_spawns_keep_the_destination_from_before_the_spawn_receipt() {
    use crate::desk::resources::Source;
    use phux_client_core::layout::SplitDir;
    use phux_protocol::wire::frame::SpawnResult;
    for action in 0..3 {
        let (client, mut desk) = connected();
        desk.connection.shown = Some(Shown {
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
        assert!(desk.connection.pending_splits.contains_key(&request_id));
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
        assert!(desk.connection.pending_splits.is_empty());
        assert!(!desk.splits_waiting());
        assert!(desk.connection.wanted.is_none());
        assert!(crate::test_support::notice(&desk).contains("selected session changed"));
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
        desk.connection.wanted.is_none(),
        "neither retry nor fallback focuses A in B"
    );
    assert!(crate::test_support::notice(&desk).contains("selected session changed"));
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
        assert!(!desk.connection.layout_ops.contains(&operation));
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
        assert!(desk.connection.wanted.is_none());
        assert!(crate::test_support::notice(&desk).contains("selected session changed"));
    }
}

/// The size a split's spawn asks for, with the focused pane's grid `area`.
fn split_spawn_size(
    dir: phux_client_core::layout::SplitDir,
    area: [f32; 2],
    zoomed: bool,
) -> (Option<(u16, u16)>, Desk) {
    let (client, mut desk) = connected();
    desk.connection.shown = Some(Shown {
        resource: terminal(),
        view: None,
    });
    desk.connection.areas.insert(terminal(), area);
    if zoomed {
        desk.connection.zoomed = Some(terminal());
    }
    desk.split(dir);
    let size = outbound(&client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::SpawnResource { initial_size, .. } => Some(initial_size),
            _ => None,
        })
        .expect("a split spawns");
    (size, desk)
}

#[test]
fn a_split_spawns_its_terminal_at_the_half_it_will_fill() {
    use crate::ui::PAD;
    use phux_client_core::layout::SplitDir;
    use phux_design::tokens::SPACE;
    let area = [800.0, 400.0];
    let (side, desk) = split_spawn_size(SplitDir::Horizontal, area, false);
    let metrics = desk.metrics();
    let half = |grid: f32| (grid + 2.0 * PAD - SPACE.card_gap) / 2.0 - 2.0 * PAD;
    assert_eq!(side, Some(metrics.fit([half(800.0), 400.0])));
    let whole = metrics.fit(area);
    let (cols, rows) = side.unwrap();
    assert!(
        cols < whole.0 / 2 && rows == whole.1,
        "{side:?} of {whole:?}"
    );
    let (below, _) = split_spawn_size(SplitDir::Vertical, area, false);
    assert_eq!(below, Some(metrics.fit([800.0, half(400.0)])));
    // A zoomed pane's area is not the one it splits: no guess from it.
    let (zoomed, _) = split_spawn_size(SplitDir::Horizontal, area, true);
    assert_ne!(zoomed, side);
}

/// A split of `terminal()` written into session 1 and not yet answered.
fn split_in_flight() -> (Arc<Client>, Desk, u32, OperationId) {
    use phux_client_core::layout::SplitDir;
    let (client, mut desk) = connected();
    install_arrangement(&client, 1);
    desk.place_split(
        &client,
        &ResourceId::local(8),
        Some((terminal(), SplitDir::Horizontal)),
        Some(session(1)),
    );
    let (request, operation) = layout_write(&client);
    (client, desk, request, operation)
}

#[test]
fn a_split_shows_and_takes_the_keyboard_when_written_not_when_answered() {
    use phux_client_core::layout::leaves;
    let (client, mut desk, request, operation) = split_in_flight();
    let tree = desk
        .split_preview(&client)
        .and_then(|doc| doc.active_group()?.tree.clone())
        .expect("the written split shows before its answer");
    assert_eq!(leaves(&tree), vec![terminal(), ResourceId::local(8)]);
    assert_eq!(
        desk.connection.wanted.as_ref().map(|(id, _)| id),
        Some(&ResourceId::local(8))
    );
    // The person moves on before the server answers; the answer and the
    // replica catching up must not pull the keyboard back.
    desk.connection.wanted = None;
    reply(
        &mut desk,
        &client,
        request,
        CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
            operation,
            Revision::new(2),
        )))),
    );
    desk.place_pending_splits(&client);
    assert!(desk.connection.wanted.is_none());
}

#[test]
fn a_refused_split_shows_the_replica_until_it_is_written_again() {
    let (client, mut desk, request, _) = split_in_flight();
    reply(
        &mut desk,
        &client,
        request,
        CommandResult::Refused(Refusal::Conflict {
            current: Revision::new(2),
        }),
    );
    assert!(desk.split_preview(&client).is_none());
}

#[test]
fn a_cancelled_split_in_flight_gives_back_the_keyboard_it_asked_for() {
    let (client, mut desk, _, _) = split_in_flight();
    assert!(desk.connection.wanted.is_some());
    client.attach_session(AttachTarget::ById(SessionId::new(2)));
    install_arrangement(&client, 2);
    desk.place_pending_splits(&client);
    assert!(!desk.splits_waiting());
    assert!(desk.connection.wanted.is_none());
    assert!(desk.split_preview(&client).is_none());
}
