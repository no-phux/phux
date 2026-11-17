use super::*;

/// A resize with cell pixel metrics sets the kernel winsize pixels
/// (`cells x cell size`); a later pixel-less resize keeps them.
#[tokio::test(flavor = "current_thread")]
async fn resize_with_cell_px_updates_pty_winsize_pixels() {
    on_local!(async {
        let cmd = CommandBuilder::new("/bin/cat");
        let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
        let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
        let handle = bundle.handle.clone();
        let token = bundle.token.clone();
        let join = tokio::task::spawn_local(bundle.actor.run());

        // 100x40 cells at 9x18 px per cell: 900x720 px text area.
        resize(
            &handle,
            ResizeRequest {
                cell_px: Some((9, 18)),
                resync_clients: false,
                ..grid_resize(100, 40)
            },
        )
        .await;
        wait_for_winsize(&master, (100, 40, 900, 720)).await;

        // Pixel-less resize: grid changes, cell size sticks.
        resize(
            &handle,
            ResizeRequest {
                resync_clients: false,
                ..grid_resize(90, 30)
            },
        )
        .await;
        wait_for_winsize(&master, (90, 30, 810, 540)).await;

        stop(&token, join).await;
    })
    .await;
}

/// Without client pixel metrics the winsize still carries nonzero pixels
/// from [`DEFAULT_CELL_PX`], at spawn and after a pixel-less resize.
#[tokio::test(flavor = "current_thread")]
async fn winsize_pixels_default_when_no_client_reports_metrics() {
    on_local!(async {
        let cmd = CommandBuilder::new("/bin/cat");
        let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
        let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
        let handle = bundle.handle.clone();
        let token = bundle.token.clone();
        let join = tokio::task::spawn_local(bundle.actor.run());

        let (cell_w, cell_h) = DEFAULT_CELL_PX;

        // Spawn-time winsize: derived from the fallback cell size,
        // never zero. 80x24 cells at 8x16 px -> 640x384 px.
        let (_, _, pixel_width, pixel_height) = winsize(&master);
        assert_eq!(
            (pixel_width, pixel_height),
            (80 * cell_w, 24 * cell_h),
            "spawn-time winsize must carry nonzero default pixel dims",
        );

        // A pixel-less resize keeps deriving pixels from the default
        // cell size: 100x40 cells at 8x16 px -> 800x640 px.
        resize(
            &handle,
            ResizeRequest {
                resync_clients: false,
                ..grid_resize(100, 40)
            },
        )
        .await;
        wait_for_winsize(&master, (100, 40, 100 * cell_w, 40 * cell_h)).await;

        stop(&token, join).await;
    })
    .await;
}

/// XTWINOPS end to end: a PTY child's `CSI 14 t` / `CSI 18 t` get the
/// latest geometry back through `on_size` and `on_pty_write` (seen via tty
/// echo).
#[tokio::test(flavor = "current_thread")]
async fn xtwinops_size_queries_answered_from_resized_geometry() {
    on_local!(async {
        let mut cmd = CommandBuilder::new("/bin/sh");
        // One query pair per input line, so the test can re-trigger
        // if an early line raced the resize.
        cmd.args(["-c", r"while read _; do printf '\033[14t\033[18t'; done"]);
        let bundle = TerminalActor::new_with_command(cmd, 80, 24).expect("spawn");
        let pty_in = bundle.actor.pty_tx.clone().expect("pty writer");
        let handle = bundle.handle.clone();
        let token = bundle.token;
        let mut out = handle.output.subscribe();
        let join = tokio::task::spawn_local(bundle.actor.run());

        resize(
            &handle,
            ResizeRequest {
                cell_px: Some((9, 18)),
                resync_clients: false,
                ..grid_resize(100, 40)
            },
        )
        .await;
        // Let the actor drain the resize before the first query.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }

        // Replies: `ESC [ 4 ; h ; w t` and `ESC [ 8 ; rows ; cols t`,
        // echoed with ESC possibly rendered as `^[`.
        let seen = |acc: &[u8], tail: &[u8]| {
            contains_subslice(acc, &[b"\x1b[", tail].concat())
                || contains_subslice(acc, &[b"^[[", tail].concat())
        };
        let mut acc: Vec<u8> = Vec::new();
        let mut found = false;
        let deadline = tokio::time::Instant::now() + ACTOR_EXIT_DEADLINE;
        let mut round = 0_usize;
        while tokio::time::Instant::now() < deadline {
            // Re-poke in case the first `go` beat the child's startup.
            if round.is_multiple_of(16) {
                pty_in
                    .try_send(EncodedInputRequest::legacy(b"go\n".to_vec()))
                    .expect("pty write");
            }
            round += 1;
            match tokio::time::timeout(DRAIN_POLL_TICK, out.recv()).await {
                Ok(Ok(ResourceOutput::Live { bytes, .. })) => acc.extend_from_slice(&bytes),
                Ok(Ok(ResourceOutput::Resync { bytes, .. })) => {
                    acc.extend_from_slice(&bytes);
                }
                Ok(Ok(ResourceOutput::Control { .. })) | Err(_) => {}
                Ok(Err(_)) => break, // channel closed
            }
            if seen(&acc, b"4;720;900t") && seen(&acc, b"8;40;100t") {
                found = true;
                break;
            }
        }
        assert!(
            found,
            "XTWINOPS replies never observed; output so far: {:?}",
            String::from_utf8_lossy(&acc),
        );

        // The writer bridge exits on channel close; `shutdown_pty`
        // joins it, so the test's sender clone must drop first.
        drop(pty_in);
        stop(&token, join).await;
    })
    .await;
}
