use super::*;

/// A cell-pixel change at the settled grid updates winsize and the input
/// encoder, and publishes no resync. Text did not reflow; a snapshot would
/// replace a scrolled client with the live screen and retire history cursors.
#[tokio::test(flavor = "current_thread")]
async fn pixel_only_resize_updates_winsize_without_resync() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_command(CommandBuilder::new("/bin/cat"), 80, 24)
                .expect("spawn");
            let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
            let handle = bundle.handle.clone();
            let mut out = handle.output.subscribe();
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());

            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 80,
                    rows: 24,
                    cell_px: Some((9, 18)),
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send pixel-only resize");

            let want = (80, 24, 720, 432);
            let read = || {
                let got = master
                    .lock()
                    .expect("master lock")
                    .get_size()
                    .expect("size");
                (got.cols, got.rows, got.pixel_width, got.pixel_height)
            };
            for _ in 0..200 {
                if read() == want {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(
                read(),
                want,
                "pixel-only resize must reach the kernel winsize"
            );
            assert_eq!(
                handle
                    .terminal()
                    .expect("terminal facet")
                    .input_snapshot
                    .borrow()
                    .cell_px,
                (9, 18),
                "mouse encoding must see the new cell size",
            );
            tokio::task::yield_now().await;
            tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;
            assert_eq!(
                drain_resync_dims(&mut out),
                Vec::new(),
                "a cell-size change at the same grid must not resync clients",
            );

            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 40,
                    rows: 10,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send real resize");
            tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;
            assert_eq!(
                drain_resync_dims(&mut out),
                vec![(40, 10)],
                "a resize that changes the grid must still resync",
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// A resize repeating the settled geometry publishes no resync (it would
/// rotate the bootstrap generation for nothing); a real one still does.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_no_op_resize_publishes_no_resync_for_phux_a5xj() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"a5xj-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            // Subscribe before the actor runs so no broadcast is missed.
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // A coalesced excursion that never reached the actor is still
            // a no-op, not a reason to replace an established generation.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .try_send(ResizeRequest {
                    cols: 100,
                    rows: 40,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .expect("transient geometry");
            // Exactly what the reflow emits for a pane the spawn already
            // sized: the geometry it is already at.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 80,
                    rows: 24,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send no-op resize");
            settle_past_resync_debounce().await;
            assert_eq!(
                drain_resync_dims(&mut out),
                Vec::new(),
                "a resize to the settled geometry must not rotate the generation",
            );

            // A real reflow still resyncs with the new dims.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 40,
                    rows: 10,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send real resize");
            settle_past_resync_debounce().await;
            assert_eq!(
                drain_resync_dims(&mut out),
                vec![(40, 10)],
                "a resize that actually reflowed must still resync exactly once",
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// Two pumps falling behind in one window get one snapshot addressed to
/// both and nobody else, and the grid does not move.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn targeted_gap_resyncs_coalesce_into_one_snapshot_for_exactly_their_pumps() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"auqy-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            let pump = |owner| ResyncTarget {
                owner,
                stream_id: phux_protocol::ids::StreamId::new(1).expect("stream id"),
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
            };
            for owner in [4, 9, 4] {
                handle
                    .terminal()
                    .expect("terminal facet")
                    .resize
                    .send(ResizeRequest {
                        cols: 0,
                        rows: 0,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: true,
                        resync_for: Some(pump(owner)),
                    })
                    .await
                    .expect("send targeted resync_only");
            }
            settle_past_resync_debounce().await;

            let mut resyncs = Vec::new();
            loop {
                match out.try_recv() {
                    Ok(PaneOutput::Resync {
                        cols,
                        rows,
                        reason,
                        audience,
                        ..
                    }) => resyncs.push((cols, rows, reason, audience)),
                    Ok(PaneOutput::Live { .. } | PaneOutput::Control { .. })
                    | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
            assert_eq!(
                resyncs,
                vec![(
                    80,
                    24,
                    ResyncReason::OutboundGap,
                    ResyncAudience::Only(vec![pump(4), pump(9)].into()),
                )],
                "one synthesis, addressed to exactly the pumps that asked",
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// A `resync_only` request re-broadcasts the grid without resizing it.
#[tokio::test]
async fn resync_only_request_rebroadcasts_snapshot_without_resizing() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"resync-only-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // resync_only: geometry fields are ignored, so the bogus 0x0
            // must NOT become the grid size.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 0,
                    rows: 0,
                    cell_px: None,
                    resync_clients: true,
                    resync_only: true,
                    resync_for: None,
                })
                .await
                .expect("send resync_only");

            let mut acc: Vec<u8> = Vec::new();
            let mut resync_dims: Option<(u16, u16)> = None;
            let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
            while tokio::time::Instant::now() < deadline {
                match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                    Ok(Ok(PaneOutput::Resync {
                        cols, rows, bytes, ..
                    })) => {
                        resync_dims = Some((cols, rows));
                        acc.extend_from_slice(&bytes);
                        if contains_subslice(&acc, b"\x1b[!p")
                            && contains_subslice(&acc, b"resync-only-marker")
                        {
                            break;
                        }
                    }
                    Ok(Ok(PaneOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
                    Ok(Ok(PaneOutput::Control { .. })) => {}
                    Ok(Err(_)) => break,
                    Err(_) => tokio::task::yield_now().await,
                }
            }

            assert_eq!(
                resync_dims,
                Some((80, 24)),
                "resync_only must keep the grid size, not adopt the ignored 0x0",
            );
            assert!(
                contains_subslice(&acc, b"\x1b[!p"),
                "resync_only broadcast missing DECSTR snapshot preamble; got {:?}",
                String::from_utf8_lossy(&acc),
            );
            assert!(
                contains_subslice(&acc, b"resync-only-marker"),
                "resync_only broadcast did not re-send grid content; got {:?}",
                String::from_utf8_lossy(&acc),
            );

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}
