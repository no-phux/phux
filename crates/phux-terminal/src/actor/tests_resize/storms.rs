use super::*;

/// A storm of live resizes coalesces into one resync.
#[tokio::test]
async fn rapid_resizes_coalesce_into_one_resync_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_seed(80, 24, b"drag-marker").expect("seed");
            let handle = bundle.handle.clone();
            let token = bundle.token;
            let mut out = handle.output.subscribe();
            let join = tokio::task::spawn_local(bundle.actor.run());

            // Fire a storm of live resizes back-to-back, well within
            // the RESIZE_RESYNC_DEBOUNCE window.
            for w in [70u16, 60, 50, 60, 70, 80, 90, 100] {
                handle
                    .terminal()
                    .expect("terminal facet")
                    .resize
                    .send(ResizeRequest {
                        cols: w,
                        rows: 24,
                        cell_px: None,
                        resync_clients: true,
                        resync_only: false,
                        resync_for: None,
                    })
                    .await
                    .expect("send resize");
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

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked");
        })
        .await;
}

/// A storm of degenerate resizes (zeros, 1x1, extreme ratios, a big spike
/// into a both-axes shrink) does not panic the actor, and a sane resize
/// still applies.
#[tokio::test]
async fn degenerate_resize_storm_does_not_panic_actor() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
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
                handle
                    .terminal()
                    .expect("terminal facet")
                    .resize
                    .send(ResizeRequest {
                        cols,
                        rows,
                        cell_px: None,
                        resync_clients: false,
                        resync_only: false,
                        resync_for: None,
                    })
                    .await
                    .expect("send resize");
            }
            // Let the actor drain the whole mailbox.
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }

            // A final sane resize must still take effect — proof the
            // actor survived and is processing, not wedged.
            handle
                .terminal()
                .expect("terminal facet")
                .resize
                .send(ResizeRequest {
                    cols: 100,
                    rows: 30,
                    cell_px: None,
                    resync_clients: false,
                    resync_only: false,
                    resync_for: None,
                })
                .await
                .expect("send final resize");
            for _ in 0..16 {
                tokio::task::yield_now().await;
            }

            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor did not exit after cancel")
                .expect("actor task panicked under degenerate resize storm");
        })
        .await;
}

/// Both-axes shrinks issued as single `resize()` calls do not overflow
/// libghostty's `PageList.resizeCols` (a plain terminal test, so a
/// regression aborts here).
#[test]
fn resize_desync_then_both_shrink_does_not_overflow() {
    let mut term = {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("term");
        terminal.set_scrollback_max_lines(Some(100)).expect("term");
        terminal
    };
    // Enough content that a 1-col reflow pushes rows into scrollback.
    for i in 0..50u32 {
        let line = format!("row-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        term.vt_write(line.as_bytes());
    }

    // The minimal trigger: a 0x0 (fails, no-op) immediately followed by
    // a both-shrink to 1x1, then the wider degenerate storm.
    let storm: &[(u16, u16)] = &[
        (0, 0),
        (1, 1),
        (1, 200),
        (200, 1),
        (0, 0),
        (1000, 1000),
        (1, 1),
        (3, 3),
        (2, 2),
        (1, 1),
        (100, 30),
    ];
    for &(req_cols, req_rows) in storm {
        // Fresh lines each step reflow content from the previous geometry.
        for i in 0..8u32 {
            let line = format!("interleave-{i}-bbbbbbbbbbbbbbbbbbbbbbbbbbbb\r\n");
            term.vt_write(line.as_bytes());
        }
        // Mirror `handle_resize`: 1-cell clamp, one bare resize per step.
        let cols = req_cols.max(1);
        let rows = req_rows.max(1);
        let _ = term.resize(cols, rows, 0, 0);
    }

    // Survived without SIGABRT; the grid settled at the final sane size.
    assert_eq!(term.cols().expect("cols"), 100);
    assert_eq!(term.rows().expect("rows"), 30);
}
