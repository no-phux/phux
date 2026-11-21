use super::*;

/// A storm of live resizes coalesces into one resync.
#[tokio::test]
async fn rapid_resizes_coalesce_into_one_resync_snapshot() {
    on_local!(async {
        let bundle = TerminalActor::new_with_seed(80, 24, b"drag-marker").expect("seed");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let mut out = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        // Fire a storm of live resizes back-to-back, well within
        // the RESIZE_RESYNC_DEBOUNCE window.
        for w in [70u16, 60, 50, 60, 70, 80, 90, 100] {
            resize(&handle, grid_resize(w, 24)).await;
        }

        // Wait comfortably past the debounce so the single
        // coalesced snapshot has fired.
        tokio::time::sleep(RESIZE_RESYNC_DEBOUNCE * 4).await;

        // Each resync is one `ResourceOutput::Resync`.
        let mut snapshots = 0usize;
        loop {
            match out.try_recv() {
                Ok(ResourceOutput::Resync { bytes, .. }) => {
                    debug_assert!(contains_subslice(&bytes, b"\x1b[!p"));
                    snapshots += 1;
                }
                // Live output and a lagged drop are both "not a
                // resync" — skip and keep draining.
                Ok(ResourceOutput::Live { .. } | ResourceOutput::Control { .. })
                | Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
        assert_eq!(
            snapshots, 1,
            "a resize storm must coalesce into exactly one resync snapshot, got {snapshots}",
        );

        stop(&token, join).await;
    })
    .await;
}

/// A storm of degenerate resizes (zeros, 1x1, extreme ratios, a big spike
/// into a both-axes shrink) does not panic the actor, and a sane resize
/// still applies.
#[tokio::test]
async fn degenerate_resize_storm_does_not_panic_actor() {
    on_local!(async {
        let bundle = TerminalActor::new_with_seed(80, 24, b"crash-hunt").expect("seed");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let join = tokio::task::spawn_local(bundle.actor.run());

        // A 300x300 spike exercises the actor like the unit repro's
        // 1000x1000 at a fraction of the cost.
        let storm: &[(u16, u16)] = &[
            (0, 0),
            (1, 1),
            (1, 200),
            (200, 1),
            (0, 0),
            (300, 300),
            (1, 1),
            (3, 3),
            (2, 2),
            (1, 1),
            (5, 1),
            (1, 5),
            (1, 1),
        ];
        for &(cols, rows) in storm {
            resize(
                &handle,
                ResizeRequest {
                    resync_clients: false,
                    ..grid_resize(cols, rows)
                },
            )
            .await;
        }
        // Let the actor drain the whole mailbox.
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }

        // A final sane resize must still take effect — proof the
        // actor survived and is processing, not wedged.
        resize(
            &handle,
            ResizeRequest {
                resync_clients: false,
                ..grid_resize(100, 30)
            },
        )
        .await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        stop(&token, join).await;
    })
    .await;
}
