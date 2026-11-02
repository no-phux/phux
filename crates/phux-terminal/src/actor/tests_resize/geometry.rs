use super::*;

#[test]
fn kitty_snapshot_keeps_natural_extent_after_pixel_metrics_change() {
    let seed = format!(
        "\x1b_Ga=T,f=24,s=1,v=97,i=7,p=9,C=1,q=2;{}\x1b\\",
        "/wAA".repeat(97)
    );
    let actor = TerminalActor::new_with_seed(10, 5, seed.as_bytes())
        .unwrap()
        .actor;
    let snapshot = actor.synthesize_with_scrollback(None).unwrap();
    let mut replica = GhosttyTerminal::new(10, 5).unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut replica).unwrap();
    replica.resize(10, 5, 8, 16).unwrap();
    replica.vt_write(&snapshot.bytes);
    // More pixels fit without changing the text grid or requesting a new snapshot.
    replica.resize(10, 5, 11, 19).unwrap();
    let graphics = replica.kitty_graphics().unwrap();
    let image = graphics.image(7).unwrap();
    let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    let mut placements = iter.update(&graphics).unwrap();
    let placement = placements.next().expect("natural image");
    let size = placement.pixel_size(&image, &replica).unwrap();
    assert_eq!((size.width, size.height), (1, 97));
}

#[test]
fn kitty_bootstrap_uses_initial_and_pixel_only_geometry() {
    let mut actor = TerminalActor::new_with_seed(
        10,
        5,
        b"\x1b[2;2H\x1b_Ga=T,f=24,s=1,v=1,i=7,p=9,c=2,r=2,C=1,q=2;/wAA\x1b\\",
    )
    .unwrap()
    .actor;
    for cell in [DEFAULT_CELL_PX, (11, 19)] {
        assert!(!actor.handle_resize(10, 5, Some(cell)));
        {
            let canonical = actor.terminal.borrow();
            let terminal = canonical.try_terminal().unwrap();
            assert_eq!(
                (terminal.width_px().unwrap(), terminal.height_px().unwrap()),
                (10 * u32::from(cell.0), 5 * u32::from(cell.1))
            );
        }
        let snapshot = actor.synthesize_with_scrollback(None).unwrap();
        let mut replica = GhosttyTerminal::new(10, 5).unwrap();
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut replica).unwrap();
        replica
            .resize(10, 5, u32::from(cell.0), u32::from(cell.1))
            .unwrap();
        replica.vt_write(&snapshot.bytes);
        let graphics = replica.kitty_graphics().unwrap();
        let image = graphics.image(7).expect("resident image");
        assert_eq!(image.data().unwrap().unwrap(), &[255, 0, 0]);
        let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
        let mut items = iter.update(&graphics).unwrap();
        let placement = items.next().expect("image visible on first attach");
        assert_eq!(
            (
                placement.image_id().unwrap(),
                placement.placement_id().unwrap()
            ),
            (7, 9)
        );
        let size = placement.pixel_size(&image, &replica).unwrap();
        assert_eq!(
            (size.width, size.height),
            (2 * u32::from(cell.0), 2 * u32::from(cell.1))
        );
        assert!(items.next().is_none());
    }
}

/// More geometry updates than the bounded mailbox can hold still apply
/// the final grid and PTY size before the attach snapshot is captured.
#[tokio::test(flavor = "current_thread")]
async fn saturated_geometry_applies_final_size_before_snapshot() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let bundle = TerminalActor::new_with_command(CommandBuilder::new("/bin/cat"), 80, 24)
                .expect("spawn");
            let master = std::sync::Arc::clone(&bundle.actor.pty.as_ref().expect("pty").master);
            let terminal = bundle.handle.terminal().expect("terminal").clone();
            let mut out = bundle.handle.output.subscribe();
            let request = ResizeRequest {
                cols: 90,
                rows: 30,
                cell_px: Some((9, 18)),
                resync_clients: true,
                resync_only: false,
                resync_for: None,
            };
            terminal.resize.try_send(request).expect("pixel donor");
            for cols in 1..=512 {
                terminal
                    .resize
                    .try_send(ResizeRequest {
                        cols,
                        rows: 30,
                        cell_px: None,
                        resync_clients: false,
                        ..request
                    })
                    .expect("geometry cannot fill the mailbox");
            }
            terminal
                .resize
                .try_send(ResizeRequest {
                    cols: 137,
                    rows: 53,
                    cell_px: None,
                    resync_clients: false,
                    ..request
                })
                .expect("final geometry");
            let (reply, snapshot) = tokio::sync::oneshot::channel();
            terminal
                .snapshot
                .try_send(SnapshotRequest {
                    image_protocols: phux_protocol::caps::ImageProtocolSet::all(),
                    scrollback: None,
                    max_bytes: 1024 * 1024,
                    max_frames: 64,
                    chunk_bytes: 64 * 1024,
                    reply,
                })
                .expect("queue attach capture before actor starts");
            let token = bundle.token;
            let join = tokio::task::spawn_local(bundle.actor.run());
            let (snapshot, _) = tokio::time::timeout(ACTOR_EXIT_DEADLINE, snapshot)
                .await
                .expect("capture deadline")
                .expect("actor reply")
                .expect("snapshot");
            assert_eq!((snapshot.cols, snapshot.rows), (137, 53));
            let size = master.lock().expect("master").get_size().expect("winsize");
            assert_eq!(
                (size.cols, size.rows, size.pixel_width, size.pixel_height),
                (137, 53, 1233, 954)
            );
            let resync = tokio::time::timeout(ACTOR_EXIT_DEADLINE, out.recv())
                .await
                .expect("resync deadline")
                .expect("resync");
            assert!(
                matches!(
                    resync,
                    ResourceOutput::Resync {
                        cols: 137,
                        rows: 53,
                        reason: ResyncReason::Resize,
                        audience: ResyncAudience::Everyone,
                        ..
                    }
                ),
                "an attach-time update cannot erase an owed live resync: {resync:?}"
            );
            token.cancel();
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, join)
                .await
                .expect("actor exit")
                .expect("actor task");
        })
        .await;
}

#[test]
fn geometry_pressure_preserves_targeted_recovery_and_closed_delivery() {
    let (tx, mut rx) = ResizeSender::channel(1);
    let target = ResyncTarget {
        owner: 7,
        stream_id: phux_protocol::ids::StreamId::new(3).unwrap(),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(2).unwrap(),
    };
    let recovery = ResizeRequest {
        cols: 0,
        rows: 0,
        cell_px: None,
        resync_clients: true,
        resync_only: true,
        resync_for: Some(target),
    };
    tx.try_send(recovery).expect("fill recovery queue");
    assert!(matches!(
        tx.try_send(recovery),
        Err(mpsc::error::TrySendError::Full(_))
    ));
    for cols in 1..=512 {
        tx.try_send(ResizeRequest {
            cols,
            rows: 30,
            resync_only: false,
            resync_for: None,
            ..recovery
        })
        .expect("recovery pressure cannot drop geometry");
    }
    let final_size = rx.try_recv().expect("latest geometry");
    assert_eq!((final_size.cols, final_size.rows), (512, 30));
    let owed = rx.try_recv().expect("targeted recovery survives");
    assert!(owed.resync_only);
    assert_eq!(owed.resync_for, Some(target));
    drop(rx);
    assert!(matches!(
        tx.try_send(final_size),
        Err(mpsc::error::TrySendError::Closed(_))
    ));
}

#[test]
fn failed_engine_resize_does_not_settle_or_publish_the_requested_geometry() {
    let mut bundle = TerminalActor::new(80, 24).expect("terminal");
    let saved = std::mem::replace(
        bundle.actor.terminal.get_mut(),
        CanonicalTerminal::Plain(None),
    );
    let request = ResizeRequest {
        cols: 100,
        rows: 40,
        cell_px: Some((9, 18)),
        resync_clients: true,
        resync_only: false,
        resync_for: None,
    };
    assert!(
        bundle.actor.apply_resize_request(request).is_empty(),
        "a refused engine resize cannot publish replacement geometry"
    );
    *bundle.actor.terminal.get_mut() = saved;
    assert_eq!(
        bundle.actor.apply_resize_request(request).len(),
        1,
        "a successful retry owes the replacement that failure refused"
    );
    let snapshot = bundle
        .actor
        .synthesize_with_scrollback(None)
        .expect("snapshot after retry");
    assert_eq!(
        (snapshot.cols, snapshot.rows),
        (100, 40),
        "the same request must remain retryable after engine refusal"
    );
}
