use super::*;

/// `defaults.history-bytes` reaches libghostty and binds (ADR-0094).
#[test]
fn configured_history_bytes_decides_retained_scrollback() {
    fn retained_rows(bytes: u32) -> usize {
        let bundle = TerminalActor::build_with_token(
            200,
            50,
            None,
            phux_config::ScrollbackLimits::new(500_000, bytes),
            CancellationToken::new(),
        )
        .expect("actor");
        {
            let mut terminal = bundle.actor.terminal.borrow_mut();
            for row in 0..40_000 {
                terminal.vt_write(format!("scrollback row {row}\r\n").as_bytes());
            }
        }
        let canonical = bundle.actor.terminal.borrow();
        let rows = canonical
            .try_terminal()
            .expect("no capture in flight")
            .scrollback_rows()
            .expect("retained scrollback rows");
        drop(canonical);
        bundle.token.cancel();
        rows
    }

    let shallow = retained_rows(2 * 1024 * 1024);
    let shipped = retained_rows(phux_config::DEFAULT_HISTORY_BYTES);
    assert!(
        shallow > 0,
        "a 2 MiB byte bound must still retain history, saw {shallow}",
    );
    assert!(
        shipped > shallow * 4,
        "the shipped 10 MiB bound (5x) must retain proportionally more history: \
         {shallow} -> {shipped}",
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn actual_server_wire_kernel_history_reaches_finish_with_zero_and_discarded_rows() {
    use phux_client_core::history::{HistoryCacheConfig, HistoryLoadState};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (empty, empty_units, empty_rows) =
                run_native_server_kernel_flow(0, HistoryCacheConfig::default(), false).await;
            assert_eq!(empty.state, HistoryLoadState::Complete);
            assert_eq!(empty_units, 1, "empty history still authenticates FINISH");
            assert_eq!(empty_rows, 0);

            let tiny = HistoryCacheConfig {
                max_materialized_rows: 1,
                prefetch_rows: 2,
                request_max_rows: 1024,
                ..HistoryCacheConfig::default()
            };
            let (history, units, authenticated_rows) =
                run_native_server_kernel_flow(3_000, tiny, true).await;
            assert_eq!(history.state, HistoryLoadState::Complete);
            assert!(units >= 3, "two history pages plus FINISH are required");
            assert!(authenticated_rows > history.materialized_rows);
            assert!(history.materialized_rows <= 1);
        })
        .await;
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn saturated_history_busy_hint_clamps_rows_on_the_public_wire() {
    let bundle = TerminalActor::new(20, 5).expect("actor");
    let mut actor = bundle.actor;
    let (outbound, _outbound_rx) = mpsc::channel(MAX_NATIVE_HISTORY_CLIENTS + 1);
    let mut saturated = None;
    for owner in 0..=MAX_NATIVE_HISTORY_CLIENTS {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history permit");
        let (reply, response) = oneshot::channel();
        actor.handle_native_history(NativeHistoryRequest {
            permit,
            owner: owner as u64,
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
            cursor: Bytes::from_static(b"not-a-valid-native-cursor"),
            max_bytes: u32::MAX,
            max_rows: u32::MAX,
            limits: phux_protocol::caps::BootstrapLimits::default(),
            reply,
        });
        if owner == MAX_NATIVE_HISTORY_CLIENTS {
            saturated = Some(response);
        }
    }
    let frame = saturated
        .expect("saturated reply")
        .await
        .expect("busy response")
        .result
        .expect("busy frame");
    let mut encoded = (&[][..]).into();
    frame.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode(&encoded).expect("public wire decode");
    assert!(tail.is_empty());
    assert!(matches!(
        decoded,
        FrameKind::HistoryRejected {
            reason: phux_protocol::wire::frame::HistoryRejectionReason::Busy,
            required_rows: phux_protocol::MAX_HISTORY_PAGE_ROWS,
            ..
        }
    ));
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn releasing_an_owner_answers_pending_history_and_promotes_the_backlog() {
    use phux_protocol::wire::frame::HistoryTombstoneReason;
    for mode in ["release", "reattach", "expire"] {
        let bundle = TerminalActor::new(20, 5).expect("actor");
        let mut actor = bundle.actor;
        let (outbound, _outbound_rx) = mpsc::channel(2);
        let mut replies = Vec::new();
        for owner in [7, 8] {
            let permit = outbound
                .clone()
                .reserve_owned()
                .await
                .expect("history permit");
            let (reply, response) = oneshot::channel();
            actor.handle_native_history(NativeHistoryRequest {
                permit,
                owner,
                terminal_id: phux_protocol::ids::ResourceId::local(1),
                stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
                cursor: Bytes::from_static(b"pending-cursor"),
                max_bytes: 1024,
                max_rows: 64,
                limits: phux_protocol::caps::BootstrapLimits::default(),
                reply,
            });
            replies.push(response);
        }

        let expected_reason = if mode == "expire" {
            actor.native_cursor_owners.insert(
                NativeCursorKey::new(7, phux_protocol::ids::StreamId::new(1).expect("stream")),
                NativeCursorOwner {
                    cursor: [0; 32],
                    record_index: 0,
                    touched: tokio::time::Instant::now() - NATIVE_HISTORY_TTL,
                    next_page_seq: 1,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(1).expect("stream"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap"),
                },
            );
            let mut controls = actor.core.output_tx.subscribe();
            actor.expire_native_cursors();
            assert!(matches!(
                controls.try_recv().expect("expiry control"),
                PaneOutput::Control {
                    frame: FrameKind::HistoryTombstone {
                        reason: HistoryTombstoneReason::Expired,
                        ..
                    },
                    ..
                }
            ));
            HistoryTombstoneReason::Expired
        } else if mode == "reattach" {
            actor.invalidate_native_owner(
                7,
                phux_protocol::ids::StreamId::new(1).expect("stream"),
                phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
            );
            HistoryTombstoneReason::Released
        } else {
            actor.release_native_owner(7);
            HistoryTombstoneReason::Released
        };
        let released = replies
            .remove(0)
            .try_recv()
            .expect("released reply without a later actor turn")
            .result;
        assert!(matches!(
            released,
            Ok(FrameKind::HistoryTombstone {
                reason,
                ..
            }) if reason == expected_reason
        ));
        assert_eq!(
            actor
                .pending_native_history
                .as_ref()
                .map(|pending| pending.request.owner),
            Some(8)
        );
        let _ = actor
            .invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
        assert!(actor.pending_native_history.is_none());
        assert!(actor.native_history_backlog.is_empty());
        assert!(matches!(
            replies
                .remove(0)
                .try_recv()
                .expect("resize answers parked request")
                .result,
            Ok(FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Released,
                ..
            })
        ));
    }
}

/// What the next native host allocation does in place of allocating.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[derive(Clone, Copy)]
enum AllocFault {
    Fail,
    Panic,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "the test keeps fault injection and the full cursor-continuation proof in one LocalSet lifecycle"
)]
async fn capture_host_allocation_failures_release_state_and_history_still_pages() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            async fn request_prefix(
                handle: &ResourceHandle,
            ) -> Result<NativeBootstrapReply, crate::native_state::NativeStateError> {
                let (reply, replied) = oneshot::channel();
                handle
                    .terminal()
                    .expect("terminal facet")
                    .native_bootstrap
                    .send(NativeBootstrapRequest {
                        owner: 11,
                        terminal_id: phux_protocol::ids::ResourceId::local(2),
                        stream_id: phux_protocol::ids::StreamId::new(2).expect("stream id"),
                        bootstrap_id: phux_protocol::ids::BootstrapId::new(4)
                            .expect("bootstrap id"),
                        limits: phux_protocol::caps::BootstrapLimits::default(),
                        max_bytes: crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                        max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
                        reply,
                    })
                    .await
                    .expect("send bootstrap");
                replied.await.expect("bootstrap reply")
            }

            async fn request_history_unit(
                handle: &ResourceHandle,
                outbound: &mpsc::Sender<Outbound>,
                cursor: Bytes,
            ) -> FrameKind {
                let permit = outbound
                    .clone()
                    .reserve_owned()
                    .await
                    .expect("history request permit");
                let (reply, response) = oneshot::channel();
                handle
                    .terminal()
                    .expect("terminal facet")
                    .native_history
                    .send(NativeHistoryRequest {
                        permit,
                        owner: 11,
                        terminal_id: phux_protocol::ids::ResourceId::local(2),
                        stream_id: phux_protocol::ids::StreamId::new(2).expect("stream id"),
                        bootstrap_id: phux_protocol::ids::BootstrapId::new(4)
                            .expect("bootstrap id"),
                        cursor,
                        max_bytes: phux_protocol::caps::BootstrapLimits::default()
                            .max_history_page_bytes(),
                        max_rows: 128,
                        limits: phux_protocol::caps::BootstrapLimits::default(),
                        reply,
                    })
                    .await
                    .expect("send history request");
                response
                    .await
                    .expect("history reply")
                    .result
                    .expect("history host")
            }

            let fault = Rc::new(Cell::new(None));
            let mut bundle = TerminalActor::new(20, 5).expect("new actor");
            bundle.actor.native_alloc = {
                let fault = Rc::clone(&fault);
                Box::new(move |capacity| match fault.take() {
                    Some(AllocFault::Fail) => {
                        Err(crate::native_state::NativeStateError::OutOfMemory)
                    }
                    Some(AllocFault::Panic) => panic!("injected native host allocation panic"),
                    None => try_native_bytes(capacity),
                })
            };
            let handle = bundle.handle.clone();
            let token = bundle.token.clone();
            let (outbound, _outbound_rx) = mpsc::channel(2);
            let run = tokio::task::spawn_local(bundle.actor.run());

            fault.set(Some(AllocFault::Fail));
            assert!(matches!(
                request_prefix(&handle).await,
                Err(crate::native_state::NativeStateError::OutOfMemory)
            ));
            fault.set(Some(AllocFault::Panic));
            assert!(matches!(
                request_prefix(&handle).await,
                Err(crate::native_state::NativeStateError::OutOfMemory)
            ));

            let prefix = request_prefix(&handle).await.expect("bootstrap capture");
            let cursor = prefix
                .frames
                .into_iter()
                .find_map(|frame| match frame {
                    FrameKind::BootstrapReady {
                        history_cursor: Some(cursor),
                        ..
                    } => Some(cursor),
                    _ => None,
                })
                .expect("bootstrap ready cursor");

            let result = request_history_unit(&handle, &outbound, cursor.clone()).await;
            let FrameKind::HistoryPage {
                page_seq,
                cursor: echoed,
                next_cursor,
                rows,
                ..
            } = result
            else {
                panic!("expected history page");
            };
            assert_eq!(page_seq, 1);
            assert!(rows <= 128);
            assert_eq!(echoed, cursor);

            let mut next_cursor = next_cursor;
            let mut expected_page_seq = 2_u64;
            while let Some(request_cursor) = next_cursor {
                assert_eq!(request_cursor, cursor, "cursor is stable and opaque");
                let frame = request_history_unit(&handle, &outbound, request_cursor).await;
                let FrameKind::HistoryPage {
                    page_seq,
                    cursor: echoed,
                    next_cursor: following,
                    rows,
                    ..
                } = frame
                else {
                    panic!("continuation must end through authenticated FINISH");
                };
                assert_eq!(page_seq, expected_page_seq);
                assert_eq!(echoed, cursor);
                assert!(rows <= 128);
                next_cursor = following;
                expected_page_seq = expected_page_seq.checked_add(1).expect("bounded sequence");
                assert!(expected_page_seq <= 4_099, "bounded continuation");
            }
            token.cancel();
            run.await.expect("actor run");
        })
        .await;
}
