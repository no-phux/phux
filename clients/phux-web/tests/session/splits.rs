//! Splits and kills: bootstrap waits, refusals, sizing, and quarantined frames.

use super::*;

#[wasm_bindgen_test]
async fn split_waits_for_bootstrap_and_ack_then_isolates_content_and_input() {
    let mut session = path_session(false, false).await;
    let first = ResourceId::local(101);
    let second = ResourceId::local(202);
    session.on_frame(Gen::on(first.clone(), 1).output(1, &b"original"[..]));
    let spawn = requested_spawn(&mut session, "vertical");
    let attach = announce_split(&mut session, spawn, second.clone());
    assert_eq!(session.pane_rects().len(), 1);
    bootstrap_split(&mut session, second.clone(), 2, b"second");
    assert!(session.pane_pending());
    assert_eq!(session.focused_pane(), Some(first.clone()));
    let committed = session.on_frame(reply(attach, CommandResult::Ok));
    assert!(committed.panes);
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 2);
    assert_eq!(session.focused_pane(), Some(second.clone()));
    let rects = session.pane_rects();
    assert_eq!((rects[0].1.x, rects[0].1.cols), (0, 9));
    assert_eq!((rects[1].1.x, rects[1].1.cols), (10, 10));
    let text = |id| grid_text(session.pane_terminal(id).unwrap());
    assert!(text(&first).starts_with("original"));
    assert!(text(&second).starts_with("second"));
    assert!(matches!(decode_one(&session.key_frame(key()).unwrap()),
        FrameKind::InputKey { terminal_id, .. } if terminal_id == second));
    session.focus_next_pane().unwrap();
    assert!(matches!(decode_one(&session.key_frame(key()).unwrap()),
        FrameKind::InputKey { terminal_id, .. } if terminal_id == first));
    let output = session.on_frame(Gen::on(second.clone(), 2).output(1, &b"!"[..]));
    assert!(output.render, "unfocused output still repaints");
    let text = |id| grid_text(session.pane_terminal(id).unwrap());
    assert!(text(&second).contains('!'));
    assert!(!text(&first).contains('!'));
}

#[wasm_bindgen_test]
async fn refused_split_and_attach_leave_the_original_usable() {
    use phux_protocol::wire::frame::SpawnError;
    let mut session = path_session(false, false).await;
    let original = ResourceId::local(101);
    let request_id = requested_spawn(&mut session, "vertical");
    let refused = session.on_frame(FrameKind::ResourceSpawned {
        request_id,
        result: SpawnResult::Err(SpawnError::SpawnFailed("quota reached".to_owned())),
    });
    assert!(refused.panes);
    assert!(session.pane_error().unwrap().contains("quota reached"));
    assert!(!session.pane_pending());
    assert_eq!(session.focused_pane(), Some(original.clone()));
    let request_id = requested_spawn(&mut session, "vertical");
    let spawned = ResourceId::local(202);
    let request_id = announce_split(&mut session, request_id, spawned.clone());
    let refused = session.on_frame(error_reply(
        request_id,
        ErrorCode::InvalidCommand,
        "attach refused",
    ));
    assert!(matches!(decode_one(&refused.send[0]), FrameKind::Command {
        command: Command::KillResource { terminal_id, .. }, ..
    } if terminal_id == spawned));
    assert_eq!(session.pane_rects().len(), 1);
    assert!(!session.is_failed());
    assert!(matches!(decode_one(&session.key_frame(key()).unwrap()),
        FrameKind::InputKey { terminal_id, .. } if terminal_id == original));
}

#[wasm_bindgen_test]
async fn killing_a_pane_waits_for_resource_closed_and_restores_sibling_size() {
    let mut session = path_session(false, false).await;
    assert!(
        session
            .kill_pane_frame()
            .unwrap_err()
            .contains("Release Session")
    );
    let original = ResourceId::local(101);
    let second = ResourceId::local(202);
    let spawn = requested_spawn(&mut session, "vertical");
    let attach = announce_split(&mut session, spawn, second.clone());
    bootstrap_split(&mut session, second.clone(), 2, b"independent");
    session.on_frame(reply(attach, CommandResult::Ok));
    let resized = session.resize_panes(60, 15);
    let sizes: Vec<_> = resized
        .iter()
        .map(|frame| match decode_one(frame) {
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                ..
            } => (terminal_id, cols, rows),
            other => panic!("expected addressed resize, got {other:?}"),
        })
        .collect();
    assert_eq!(
        sizes,
        vec![(original.clone(), 29, 15), (second.clone(), 30, 15)]
    );
    let request_id = match decode_one(&session.kill_pane_frame().unwrap()) {
        FrameKind::Command {
            request_id,
            command: Command::KillResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, second);
            request_id
        }
        other => panic!("expected kill, got {other:?}"),
    };
    session.on_frame(reply(request_id, CommandResult::Ok));
    assert_eq!(session.pane_rects().len(), 2);
    assert!(session.pane_pending());
    let closed = session.on_frame(closed(second, CloseReason::Unknown));
    assert!(!session.pane_pending());
    assert_eq!(session.focused_pane(), Some(original.clone()));
    assert_eq!(session.pane_rects()[0].1.cols, 60);
    assert!(closed.send.iter().any(|frame| matches!(decode_one(frame),
        FrameKind::ResizeTerminal { terminal_id, cols: 60, rows: 15, .. } if terminal_id == original)));
    assert!(session.kill_pane_frame().is_err());
    assert!(session.key_frame(key()).is_some());
}

#[wasm_bindgen_test]
async fn panes_stop_at_four_and_survive_a_refused_kill() {
    let mut session = path_session(false, false).await;
    session.resize_panes(120, 40);
    for (serial, axis) in [(2, "vertical"), (3, "horizontal"), (4, "vertical")] {
        commit_split(
            &mut session,
            axis,
            ResourceId::local(200 + serial as u32),
            serial,
        );
    }
    assert_eq!(session.pane_rects().len(), 4);
    assert!(
        session
            .split_pane_frame("vertical")
            .unwrap_err()
            .contains("Four")
    );
    let focused = session.focused_pane();
    let request_id = match decode_one(&session.kill_pane_frame().unwrap()) {
        FrameKind::Command { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    session.on_frame(error_reply(
        request_id,
        ErrorCode::InvalidCommand,
        "not permitted",
    ));
    assert_eq!(session.pane_rects().len(), 4);
    assert_eq!(session.focused_pane(), focused);
    assert!(!session.pane_pending());
    assert!(session.key_frame(key()).is_some());
}

#[wasm_bindgen_test]
async fn pane_resize_restores_size_before_earlier_bootstrap_arrives() {
    let mut session = path_session(false, false).await;
    let sent = session.resize_panes(40, 3);
    assert!(matches!(
        decode_one(&sent[0]),
        FrameKind::ResizeTerminal {
            cols: 40,
            rows: 3,
            ..
        }
    ));
    let restored = session.resize_panes(20, 3);
    assert_eq!(
        restored.len(),
        1,
        "the outstanding 40-column request must be superseded"
    );
    assert!(matches!(
        decode_one(&restored[0]),
        FrameKind::ResizeTerminal {
            cols: 20,
            rows: 3,
            ..
        }
    ));
}

#[wasm_bindgen_test]
async fn failed_split_quarantines_queued_frames_without_weakening_other_streams() {
    let mut session = path_session(false, false).await;
    let spawned = ResourceId::local(202);
    let request = requested_spawn(&mut session, "vertical");
    announce_split(&mut session, request, spawned.clone());
    let split = Gen::on(spawned.clone(), 2);
    session.on_frame(split.begin(RAW, 9, 3, 0));
    let too_long = session.bootstrap_limits().unwrap().max_chunk_bytes() as usize + 1;
    let refused = session.on_frame(split.chunk(0, vec![b'x'; too_long]));
    assert!(refused.fatal.is_none());
    assert!(session.pane_error().is_some());
    let late = session.on_frame(split.ready());
    assert!(
        late.fatal.is_none(),
        "a failed split must not close its healthy sibling"
    );
    session.on_frame(generation(101, 1, 1).output(1, &b"healthy"[..]));
    let text: String = session.grid().cells.iter().map(|cell| cell.ch).collect();
    assert!(text.starts_with("healthy"));
    assert!(session.key_frame(key()).is_some());
    let closed = session.on_frame(closed(spawned, CloseReason::Unknown));
    assert!(closed.fatal.is_none());
    let unrelated = session.on_frame(generation(999, 9, 9).ready());
    assert!(
        unrelated.fatal.is_some(),
        "unknown streams remain a protocol fault"
    );
}
