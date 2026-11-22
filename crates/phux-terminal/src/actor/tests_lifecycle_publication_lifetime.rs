//! Pending publication cleanup must not depend on an unfinished history lease.

use super::*;

fn populated_actor() -> TerminalActor {
    let mut actor = TerminalActor::new(200, 5).expect("actor").actor;
    for row in 0..3000 {
        actor.vt_write_for_test(format!("history-row-{row:04}\r\n").as_bytes());
    }
    actor
}

fn assert_no_native_retains(
    actor: &TerminalActor,
    cursor: &crate::native_state::OpaqueHistoryCursor,
) {
    assert!(
        !actor
            .terminal
            .borrow_mut()
            .native_manager()
            .expect("native manager")
            .has_generation(cursor),
        "history FINISH must actually release the engine generation"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn populated_shared_history_finishes_before_or_after_publication() {
    for same_owner in [false, true] {
        for activate_first in [false, true] {
            let mut actor = populated_actor();
            let pumps = pumps(same_owner);
            let receivers = begin_replacements(&mut actor, pumps);
            let captures = finish_captures(&mut actor, receivers);
            if activate_first {
                assert_eq!(
                    activate_replacements(&mut actor, pumps, &captures),
                    [None; 2]
                );
            }
            for i in 0..2 {
                assert!(
                    complete_history(&mut actor, pumps[i], &captures[i]).await >= 3,
                    "populated history must import multiple authenticated pages and FINISH"
                );
            }
            assert_no_native_retains(&actor, &captures[0].publication_cursor);
            if !activate_first {
                assert_eq!(
                    actor.native_cursor_owners.len(),
                    2,
                    "pending identities survive FINISH"
                );
                assert_eq!(
                    activate_replacements(&mut actor, pumps, &captures),
                    [None; 2]
                );
            }
            assert!(actor.native_publications.is_empty());
            assert!(actor.native_cursor_owners.is_empty());
            assert_eq!(
                activate_replacements(&mut actor, pumps, &captures),
                [Some(NativeStateError::InvalidHandle); 2],
                "exactly once"
            );
        }
    }
}

fn activation(
    actor: &mut TerminalActor,
    pump: Pump,
    capture: &NativeBootstrapReply,
    mutation: &str,
) -> Option<NativeStateError> {
    let (reply, mut response) = oneshot::channel();
    let mut req = NativePublicationRequest {
        owner: pump.0,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: pump.1,
        bootstrap_id: replacement_id(),
        cursor: capture.publication_cursor,
        reply,
    };
    match mutation {
        "owner" => req.owner = 99,
        "terminal" => req.terminal_id = phux_protocol::ids::ResourceId::local(99),
        "stream" => req.stream_id = StreamId::new(99).unwrap(),
        "bootstrap" => req.bootstrap_id = BootstrapId::new(99).unwrap(),
        "cursor" => req.cursor[0] ^= 0xff,
        "" => {}
        _ => panic!("known identity mutation"),
    }
    actor.handle_native_publication(req);
    response
        .try_recv()
        .expect("immediate activation reply")
        .err()
}

#[tokio::test(flavor = "current_thread")]
async fn only_the_complete_pending_identity_consumes_one_resize_receipt() {
    for finish_first in [false, true] {
        let mut actor = populated_actor();
        let pumps = pumps(false);
        let receivers = begin_replacements(&mut actor, pumps);
        let captures = finish_captures(&mut actor, receivers);
        if finish_first {
            for i in 0..2 {
                complete_history(&mut actor, pumps[i], &captures[i]).await;
            }
        }
        let mut controls = actor.core.output_tx.subscribe();
        let touched: Vec<_> = actor
            .native_cursor_owners
            .values()
            .map(|b| b.touched)
            .collect();
        actor.apply_resize_request(resize(30, 8));
        actor.apply_resize_request(resize(31, 9));
        assert_eq!(actor.native_cursor_owners.len(), 2);
        let after: Vec<_> = actor
            .native_cursor_owners
            .values()
            .map(|b| b.touched)
            .collect();
        assert!(
            after.iter().all(|t| touched.contains(t)),
            "resize never refreshes receipt TTL"
        );
        let mut tombstones = 0;
        while let Ok(event) = controls.try_recv() {
            if matches!(
                event,
                ResourceOutput::Control {
                    frame: FrameKind::BootstrapTombstone { .. },
                    ..
                }
            ) {
                tombstones += 1;
            }
        }
        assert_eq!(
            tombstones, 2,
            "repeat reflow does not re-retire either identity"
        );
        for pump_index in 0..2 {
            for mutation in ["owner", "terminal", "stream", "bootstrap", "cursor"] {
                assert_eq!(
                    activation(
                        &mut actor,
                        pumps[pump_index],
                        &captures[pump_index],
                        mutation
                    ),
                    Some(NativeStateError::InvalidHandle)
                );
            }
            assert_eq!(
                activation(&mut actor, pumps[pump_index], &captures[pump_index], ""),
                Some(NativeStateError::Resize)
            );
            assert_eq!(
                activation(&mut actor, pumps[pump_index], &captures[pump_index], ""),
                Some(NativeStateError::InvalidHandle)
            );
        }
        assert!(actor.native_cursor_owners.is_empty());
        assert!(actor.native_publications.is_empty());
        assert_no_native_retains(&actor, &captures[0].publication_cursor);
    }
}

fn cleanup(actor: &mut TerminalActor, pump: Pump, mode: &str) {
    match mode {
        "detach" => actor.release_native_owner(pump.0),
        "recapture" => {
            capture_native_for_pump(actor, pump.0, pump.1, BootstrapId::new(3).unwrap());
            actor.release_native_owner(pump.0);
        }
        "expiry" => {
            actor
                .native_cursor_owners
                .get_mut(&NativeCursorKey::new(pump.0, pump.1))
                .unwrap()
                .touched = tokio::time::Instant::now() - NATIVE_HISTORY_TTL;
            actor.expire_native_cursors();
        }
        "overflow" => {
            let bytes = Bytes::from(vec![b'x'; MAX_NATIVE_REPLAY_BYTES + 1]);
            actor.buffer_native_live_output(1, &bytes);
        }
        "shutdown" => actor.land_native_cuts(),
        _ => panic!("known cleanup"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn completed_history_and_wrong_identities_cannot_consume_or_refresh_pending_publication() {
    let mut actor = populated_actor();
    let pump = pumps(false)[0];
    let capture = capture_native_for_pump(&mut actor, pump.0, pump.1, replacement_id());
    complete_history(&mut actor, pump, &capture).await;
    let key = NativeCursorKey::new(pump.0, pump.1);
    let touched = actor.native_cursor_owners.get(&key).unwrap().touched;
    let (outbound, _mailbox) = mpsc::channel(1);
    let (reply, _response) = oneshot::channel();
    let request = NativeHistoryRequest {
        permit: outbound.reserve_owned().await.unwrap(),
        owner: pump.0,
        terminal_id: phux_protocol::ids::ResourceId::local(1),
        stream_id: pump.1,
        bootstrap_id: replacement_id(),
        cursor: Bytes::copy_from_slice(&capture.publication_cursor),
        max_bytes: 65536,
        max_rows: 1024,
        limits: phux_protocol::caps::BootstrapLimits::default(),
        reply,
    };
    assert!(
        matches!(
            actor.native_history_frame(&request).unwrap(),
            Some(FrameKind::HistoryTombstone { .. })
        ),
        "FINISH is not a valid history binding"
    );
    for mutation in ["owner", "terminal", "stream", "bootstrap", "cursor"] {
        assert_eq!(
            activation(&mut actor, pump, &capture, mutation),
            Some(NativeStateError::InvalidHandle)
        );
    }
    assert_eq!(
        actor.native_cursor_owners.get(&key).unwrap().touched,
        touched
    );
    assert_eq!(activation(&mut actor, pump, &capture, ""), None);
    assert_eq!(
        activation(&mut actor, pump, &capture, ""),
        Some(NativeStateError::InvalidHandle)
    );
    assert!(actor.native_publications.is_empty());
    assert!(actor.native_cursor_owners.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn every_retirement_reaches_publication_only_waiters_after_history_finish() {
    for mode in ["detach", "recapture", "expiry", "overflow", "shutdown"] {
        let mut actor = populated_actor();
        let pumps = pumps(false);
        let receivers = begin_replacements(&mut actor, pumps);
        let captures = finish_captures(&mut actor, receivers);
        for i in 0..2 {
            complete_history(&mut actor, pumps[i], &captures[i]).await;
        }
        assert_no_native_retains(&actor, &captures[0].publication_cursor);
        cleanup(&mut actor, pumps[0], mode);
        assert_eq!(
            activation(&mut actor, pumps[0], &captures[0], ""),
            Some(NativeStateError::InvalidHandle),
            "{mode}"
        );
        if ["detach", "recapture", "expiry"].contains(&mode) {
            assert_eq!(
                activation(&mut actor, pumps[1], &captures[1], ""),
                None,
                "sibling survives {mode}"
            );
        } else {
            assert_eq!(
                activation(&mut actor, pumps[1], &captures[1], ""),
                Some(NativeStateError::InvalidHandle)
            );
        }
        assert!(
            actor.native_cursor_owners.is_empty(),
            "{mode} retained identities"
        );
        assert!(
            actor.native_publications.is_empty(),
            "{mode} retained replay"
        );
    }
}

fn refused_capture(actor: &mut TerminalActor, owner: u64) {
    let (reply, mut response) = oneshot::channel();
    actor.handle_native_bootstrap(native_request(
        owner,
        phux_protocol::ids::ResourceId::local(1),
        (StreamId::new(1).unwrap(), replacement_id()),
        reply,
    ));
    for _ in 0..64 {
        match response.try_recv() {
            Ok(result) => {
                assert_eq!(result.err(), Some(NativeStateError::LimitExceeded));
                return;
            }
            Err(oneshot::error::TryRecvError::Empty) => actor.cooperative_native_step(),
            Err(oneshot::error::TryRecvError::Closed) => panic!("capture reply dropped"),
        }
    }
    panic!("capture refusal deadline");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn finished_and_superseded_waiters_share_the_existing_capacity_and_ttl() {
    let mut actor = TerminalActor::new(20, 5).expect("actor").actor;
    let mut captures = Vec::new();
    for owner in 1..=MAX_NATIVE_HISTORY_CLIENTS {
        let pump = (owner as u64, StreamId::new(1).unwrap());
        let capture = capture_native_for_pump(&mut actor, pump.0, pump.1, replacement_id());
        complete_history(&mut actor, pump, &capture).await;
        assert_no_native_retains(&actor, &capture.publication_cursor);
        captures.push((pump, capture));
    }
    assert_eq!(actor.native_cursor_owners.len(), MAX_NATIVE_HISTORY_CLIENTS);
    refused_capture(&mut actor, 99);
    assert_eq!(actor.native_cursor_owners.len(), MAX_NATIVE_HISTORY_CLIENTS);
    actor.apply_resize_request(resize(30, 8));
    refused_capture(&mut actor, 99);
    assert!(
        actor.native_publications.is_empty(),
        "resize discarded shared replay"
    );
    tokio::time::advance(NATIVE_HISTORY_TTL / 2).await;
    for (pump, capture) in &captures {
        assert_eq!(
            activation(&mut actor, *pump, capture, "cursor"),
            Some(NativeStateError::InvalidHandle)
        );
    }
    actor.apply_resize_request(resize(31, 9));
    tokio::time::advance(NATIVE_HISTORY_TTL / 2).await;
    for (pump, capture) in &captures {
        assert_eq!(
            activation(&mut actor, *pump, capture, ""),
            Some(NativeStateError::InvalidHandle),
            "invalid requests/repeated resize must not refresh TTL"
        );
    }
    assert!(actor.native_cursor_owners.is_empty());
    assert!(actor.native_publications.is_empty());
    let new = capture_native_for_pump(&mut actor, 99, StreamId::new(1).unwrap(), replacement_id());
    activate_native_for_pump(
        &mut actor,
        99,
        StreamId::new(1).unwrap(),
        replacement_id(),
        new.publication_cursor,
    )
    .unwrap();
    actor.release_native_owner(99);
    assert_no_native_retains(&actor, &new.publication_cursor);
}

#[allow(
    clippy::future_not_send,
    reason = "test fixture drives the LocalSet-only native actor"
)]
async fn write(actor: &mut TerminalActor, bytes: &'static [u8]) {
    actor.ingest_pty_for_test(Bytes::from_static(bytes)).await;
}

#[tokio::test(flavor = "current_thread")]
async fn history_finish_keeps_capture_ready_and_finish_replay_then_live_exactly_once() {
    let mut actor = populated_actor();
    let pump = pumps(false)[0];
    write(&mut actor, b"before-capture\r\n").await;
    let (reply, response) = oneshot::channel();
    actor.handle_native_bootstrap(native_request(
        pump.0,
        phux_protocol::ids::ResourceId::local(1),
        (pump.1, replacement_id()),
        reply,
    ));
    write(&mut actor, b"during-capture\r\n").await;
    let capture = finish_capture(&mut actor, response);
    assert_eq!(capture.base_seq, 1);
    write(&mut actor, b"after-READY\r\n").await;
    complete_history(&mut actor, pump, &capture).await;
    write(&mut actor, b"after-FINISH\r\n").await;
    let mut publication = activate_native_for_pump(
        &mut actor,
        pump.0,
        pump.1,
        replacement_id(),
        capture.publication_cursor,
    )
    .expect("handoff survives FINISH");
    assert_eq!(
        publication
            .replay
            .iter()
            .map(|(seq, _)| *seq)
            .collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert!(publication.replay.iter().all(|(_, b)| !b.is_empty()));
    write(&mut actor, b"live\r\n").await;
    assert!(matches!(
        publication.live.try_recv(),
        Ok(ResourceOutput::Live { seq: 5, .. })
    ));
    assert!(publication.live.try_recv().is_err());
    assert!(actor.native_publications.is_empty());
    assert!(actor.native_cursor_owners.is_empty());
}
