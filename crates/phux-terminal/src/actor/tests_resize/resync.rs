use super::*;

/// A cell-pixel change at the settled grid updates winsize and the input
/// encoder, and publishes no resync. Text did not reflow; a snapshot would
/// replace a scrolled client with the live screen and retire history cursors.
#[tokio::test(flavor = "current_thread")]
async fn pixel_only_resize_updates_winsize_without_resync() {
    on_local!(async {
        let bundle = TerminalActor::new_with_command(CommandBuilder::new("/bin/cat"), 80, 24)
            .expect("spawn");
        let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
        let handle = bundle.handle.clone();
        let mut out = handle.output.subscribe();
        let token = bundle.token;
        let input_snapshot = bundle.actor.input_snapshot_tx.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        resize(
            &handle,
            ResizeRequest {
                cell_px: Some((9, 18)),
                ..grid_resize(80, 24)
            },
        )
        .await;

        wait_for_winsize(&master, (80, 24, 720, 432)).await;
        assert_eq!(
            input_snapshot.borrow().cell_px,
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

        resize(&handle, grid_resize(40, 10)).await;
        tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;
        assert_eq!(
            drain_resync_dims(&mut out),
            vec![(40, 10)],
            "a resize that changes the grid must still resync",
        );

        stop(&token, join).await;
    })
    .await;
}

/// A resize repeating the settled geometry publishes no resync (it would
/// rotate the bootstrap generation for nothing); a real one still does.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_no_op_resize_publishes_no_resync_for_phux_a5xj() {
    on_local!(async {
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
        resize(&handle, grid_resize(80, 24)).await;
        settle_past_resync_debounce().await;
        assert_eq!(
            drain_resync_dims(&mut out),
            Vec::new(),
            "a resize to the settled geometry must not rotate the generation",
        );

        // A real reflow still resyncs with the new dims.
        resize(&handle, grid_resize(40, 10)).await;
        settle_past_resync_debounce().await;
        assert_eq!(
            drain_resync_dims(&mut out),
            vec![(40, 10)],
            "a resize that actually reflowed must still resync exactly once",
        );

        stop(&token, join).await;
    })
    .await;
}

/// Two pumps falling behind in one window get one snapshot addressed to
/// both and nobody else, and the grid does not move.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn targeted_gap_resyncs_coalesce_into_one_snapshot_for_exactly_their_pumps() {
    on_local!(async {
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
            resize(
                &handle,
                ResizeRequest {
                    resync_only: true,
                    resync_for: Some(pump(owner)),
                    ..grid_resize(0, 0)
                },
            )
            .await;
        }
        settle_past_resync_debounce().await;

        let mut resyncs = Vec::new();
        loop {
            match out.try_recv() {
                Ok(ResourceOutput::Resync {
                    cols,
                    rows,
                    reason,
                    audience,
                    ..
                }) => resyncs.push((cols, rows, reason, audience)),
                Ok(ResourceOutput::Live { .. } | ResourceOutput::Control { .. })
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

        stop(&token, join).await;
    })
    .await;
}

/// A `resync_only` request re-broadcasts the grid without resizing it.
#[tokio::test]
async fn resync_only_request_rebroadcasts_snapshot_without_resizing() {
    on_local!(async {
        let bundle = TerminalActor::new_with_seed(80, 24, b"resync-only-marker").expect("seed");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let mut out = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        // resync_only: geometry fields are ignored, so the bogus 0x0
        // must NOT become the grid size.
        resize(
            &handle,
            ResizeRequest {
                resync_only: true,
                ..grid_resize(0, 0)
            },
        )
        .await;

        let (dims, acc) = await_resync_with(&mut out, b"resync-only-marker").await;
        assert_eq!(
            dims,
            Some((80, 24)),
            "resync_only must keep the grid size, not adopt the ignored 0x0",
        );
        assert_resync_bytes(&acc, b"resync-only-marker");

        stop(&token, join).await;
    })
    .await;
}
