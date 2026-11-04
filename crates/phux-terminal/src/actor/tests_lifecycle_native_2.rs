use super::*;

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn combined_native_pty_ingress_never_parks_on_a_silent_source() {
    let (_bootstrap_tx, bootstrap) = mpsc::channel(1);
    let (_history_tx, history) = mpsc::channel(1);
    let (_publication_tx, publication) = mpsc::channel(1);
    let (release_tx, release) = mpsc::channel(4);
    let mut native = NativeRequestReceivers {
        bootstrap,
        publication,
        history,
        release,
    };
    let (pty_tx, mut pty) = mpsc::channel(4);

    release_tx
        .send(NativeReleaseRequest { owner: 1 })
        .await
        .expect("first native request");
    release_tx
        .send(NativeReleaseRequest { owner: 2 })
        .await
        .expect("second native request");
    for owner in [1, 2] {
        let ingress = tokio::time::timeout(
            ACTOR_EXIT_DEADLINE,
            recv_native_or_pty(&mut native, Some(&mut pty), false),
        )
        .await
        .expect("silent PTY must not park native control");
        assert!(matches!(
            ingress,
            NativeOrPty::Native(NativeActorRequest::Release(NativeReleaseRequest {
                owner: actual
            })) if actual == owner
        ));
    }

    release_tx
        .send(NativeReleaseRequest { owner: 3 })
        .await
        .expect("ready native request");
    pty_tx
        .try_send(PtyEvent::Bytes {
            chunk: Bytes::from_static(b"x"),
            read_at: std::time::Instant::now(),
        })
        .expect("ready PTY event");
    assert!(matches!(
        recv_native_or_pty(&mut native, Some(&mut pty), false).await,
        NativeOrPty::Pty(Some(PtyEvent::Bytes { chunk: bytes, .. })) if bytes.as_ref() == b"x"
    ));
    assert!(matches!(
        recv_native_or_pty(&mut native, Some(&mut pty), true).await,
        NativeOrPty::Native(NativeActorRequest::Release(NativeReleaseRequest {
            owner: 3
        }))
    ));
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[test]
fn resize_tombstone_is_ordered_after_every_queued_live_sequence() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let mut output = bundle.handle.output.subscribe();
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id");
    let cursor: crate::native_state::OpaqueHistoryCursor = [1; crate::native_state::TOKEN_LEN];
    actor.native_cursor_owners.insert(
        NativeCursorKey::new(7, stream_id),
        NativeCursorOwner {
            cursor,
            record_index: 0,
            touched: tokio::time::Instant::now(),
            next_page_seq: 1,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
        },
    );

    actor.core.seq = 5;
    actor
        .core
        .output_tx
        .send(ResourceOutput::Live {
            seq: 5,
            bytes: Bytes::from_static(b"prior"),
            at: std::time::Instant::now(),
        })
        .expect("queue prior live output");
    actor.invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);

    assert!(matches!(
        output.try_recv(),
        Ok(ResourceOutput::Live { seq: 5, .. })
    ));
    let Ok(ResourceOutput::Control { owner: 7, frame }) = output.try_recv() else {
        panic!("ordered generation tombstone");
    };
    assert!(matches!(
        frame,
        FrameKind::BootstrapTombstone {
            terminal_id: actual_terminal,
            stream_id: actual_stream,
            bootstrap_id: actual_bootstrap,
            reason: phux_protocol::wire::frame::TombstoneReason::Resize,
            last_valid_seq: 5,
        } if actual_terminal == terminal_id
            && actual_stream == stream_id
            && actual_bootstrap == bootstrap_id
    ));
}

/// An attach-time reflow owes a resync to exactly the pumps it tombstoned.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn an_attach_time_reflow_owes_a_resync_to_the_native_pumps_it_tombstoned() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let stream_id = phux_protocol::ids::StreamId::new(3).expect("stream id");
    actor.native_cursor_owners.insert(
        NativeCursorKey::new(7, stream_id),
        NativeCursorOwner {
            cursor: [1; crate::native_state::TOKEN_LEN],
            record_index: 0,
            touched: tokio::time::Instant::now(),
            next_page_seq: 1,
            terminal_id: phux_protocol::ids::ResourceId::local(1),
            stream_id,
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
        },
    );
    let attach_time_reflow = |cols, rows| ResizeRequest {
        cols,
        rows,
        cell_px: None,
        resync_clients: false,
        resync_only: false,
        resync_for: None,
    };

    let owed = actor.apply_resize_request(attach_time_reflow(30, 8));
    assert_eq!(
        owed,
        vec![super::super::run_loop::OwedResync {
            reason: phux_resource::ResyncReason::Resize,
            target: Some(phux_resource::ResyncTarget {
                owner: 7,
                stream_id,
                bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id"),
            }),
        }],
        "the tombstoned native pump is owed a resync addressed to it",
    );

    let owed = actor.apply_resize_request(attach_time_reflow(40, 9));
    assert!(
        owed.is_empty(),
        "with nothing native left to tombstone, an attach-time reflow owes nothing",
    );
}

/// A `HISTORY_REQUEST` racing a resize that drained its binding gets a
/// per-replica `HISTORY_TOMBSTONE`, never a connection-level error.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn a_cursor_invalidated_by_resize_is_tombstoned_never_faulted() {
    let bundle = TerminalActor::new(20, 5).expect("new actor");
    let mut actor = bundle.actor;
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream id");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap id");
    let cursor: crate::native_state::OpaqueHistoryCursor = [1; crate::native_state::TOKEN_LEN];
    let wire_cursor = Bytes::copy_from_slice(&cursor);
    let binding = || NativeCursorOwner {
        cursor,
        record_index: 0,
        touched: tokio::time::Instant::now(),
        next_page_seq: 1,
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
    };
    let (outbound, _outbound_rx) = dummy_outbound();

    let request = async |actor: &mut TerminalActor, bootstrap_id| {
        let permit = outbound
            .clone()
            .reserve_owned()
            .await
            .expect("history request permit");
        let (reply, answered) = oneshot::channel();
        actor.handle_native_history(NativeHistoryRequest {
            permit,
            owner: 7,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: wire_cursor.clone(),
            max_bytes: phux_protocol::caps::BootstrapLimits::default().max_history_page_bytes(),
            max_rows: 128,
            limits: phux_protocol::caps::BootstrapLimits::default(),
            reply,
        });
        actor.cooperative_native_step();
        answered
            .await
            .expect("history reply")
            .result
            .expect("an unusable cursor is answered, never faulted")
    };

    // The binding is gone entirely: the mid-attach resize drained it.
    actor
        .native_cursor_owners
        .insert(NativeCursorKey::new(7, stream_id), binding());
    actor.invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Resize);
    assert!(actor.native_cursor_owners.is_empty());
    let frame = request(&mut actor, bootstrap_id).await;
    assert!(
        matches!(
            &frame,
            FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Stale,
                cursor: echoed,
                ..
            } if *echoed == wire_cursor
        ),
        "a drained binding tombstones the cursor: {frame:?}"
    );

    // The binding names the generation the resize replaced.
    actor
        .native_cursor_owners
        .insert(NativeCursorKey::new(7, stream_id), binding());
    let superseded = phux_protocol::ids::BootstrapId::new(2).expect("bootstrap id");
    let frame = request(&mut actor, superseded).await;
    assert!(
        matches!(
            &frame,
            FrameKind::HistoryTombstone {
                reason: phux_protocol::wire::frame::HistoryTombstoneReason::Stale,
                ..
            }
        ),
        "a superseded generation tombstones the cursor: {frame:?}"
    );
    assert!(
        actor
            .native_cursor_owners
            .contains_key(&NativeCursorKey::new(7, stream_id)),
        "answering a stale request must not release the live binding"
    );
}

/// EOF is not a new incarnation: a completed checkpoint blocked on a slow
/// consumer must still activate and restore its final viewport.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn ready_native_publication_survives_eof_before_activation() {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
    use phux_client_core::engine::ghostty::GhosttyAdapter;
    use phux_client_core::session::{EffectBuffer, SessionKernel};
    use phux_protocol::caps::{BootstrapProfile, EngineCodec, EngineFeatureSet};

    let mut actor = TerminalActor::new(40, 8).expect("native actor").actor;
    actor.vt_write_for_test(b"final-before-eof");
    let stream_id = phux_protocol::ids::StreamId::new(1).expect("stream");
    let bootstrap_id = phux_protocol::ids::BootstrapId::new(1).expect("bootstrap");
    let capture = capture_native_for_pump(&mut actor, 7, stream_id, bootstrap_id);

    // The wire publisher holds this READY while the producer ends.
    actor.handle_pty_eof();
    actor.flush_final_gap_resync(&mut super::super::run_loop::ResyncDebounce::idle());
    activate_native_for_pump(
        &mut actor,
        7,
        stream_id,
        bootstrap_id,
        capture.publication_cursor,
    )
    .expect("EOF must preserve the completed publication fence");

    let limits = phux_protocol::caps::BootstrapLimits::default();
    let mut kernel = SessionKernel::new(
        GhosttyAdapter::new(limits),
        BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            features: EngineFeatureSet::required_native(),
        },
    );
    let mut effects = EffectBuffer::new();
    for frame in &capture.frames {
        apply_native_wire_frame(&mut kernel, frame, &mut effects);
    }
    let replica = kernel
        .published_engine(&phux_protocol::ids::ResourceId::local(1))
        .expect("consumer committed the final viewport");
    let mut formatter = Formatter::new(
        replica.terminal().expect("native terminal"),
        FormatterOptions::new()
            .with_format(Format::Plain)
            .with_trim(true),
    )
    .expect("viewport formatter");
    let mut text = [0; 512];
    let len = formatter.format_buf(&mut text).expect("bounded viewport");
    assert_eq!(
        std::str::from_utf8(&text[..len])
            .expect("viewport text")
            .trim_end(),
        "final-before-eof"
    );
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#[tokio::test(flavor = "current_thread")]
async fn eof_returns_a_loan_before_final_output_and_resumes_queued_capture() {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};

    tokio::task::LocalSet::new()
        .run_until(async {
            let mut bundle = TerminalActor::new(40, 8).expect("native actor");
            let exited = bundle.exit_notify.take().expect("exit notification");
            let token = bundle.token.clone();
            let mut output = bundle.handle.output.subscribe();
            let mut actor = bundle.actor;
            actor.vt_write_for_test(b"before\r\n");
            let (first_reply, first) = oneshot::channel();
            let (queued_reply, queued) = oneshot::channel();
            // Different admitted budgets put the second consumer in the
            // backlog instead of joining the first consumer's frozen cut.
            for (owner, max_bytes, reply) in [
                (
                    31,
                    crate::native_state::MAX_NATIVE_PREFIX_BYTES,
                    first_reply,
                ),
                (
                    32,
                    crate::native_state::MAX_NATIVE_PREFIX_BYTES - 1,
                    queued_reply,
                ),
            ] {
                actor.handle_native_bootstrap(NativeBootstrapRequest {
                    owner,
                    terminal_id: phux_protocol::ids::ResourceId::local(1),
                    stream_id: phux_protocol::ids::StreamId::new(owner).expect("stream"),
                    bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap"),
                    limits: phux_protocol::caps::BootstrapLimits::default(),
                    max_bytes,
                    max_frames: crate::native_state::MAX_NATIVE_BOOTSTRAP_FRAMES,
                    reply,
                });
            }
            let (pty, _writer) = actor.install_test_pty_channels();
            pty.send(PtyEvent::Bytes {
                chunk: Bytes::from_static(b"final-tail"),
                read_at: std::time::Instant::now(),
            })
            .await
            .expect("queue final output behind the loan");
            pty.send(PtyEvent::Eof).await.expect("queue EOF");
            let run = tokio::task::spawn_local(actor.run());

            assert!(matches!(
                first.await.expect("interrupted capture is answered"),
                Err(crate::native_state::NativeStateError::Resize)
            ));
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, exited)
                .await
                .expect("EOF is announced")
                .expect("exit outcome");
            let mut final_snapshot = None;
            while let Ok(event) = output.try_recv() {
                if let ResourceOutput::Resync {
                    reason: super::super::ResyncReason::Exit,
                    bytes,
                    ..
                } = event
                {
                    final_snapshot = Some(bytes);
                }
            }
            let mut mirror = libghostty_vt::Terminal::new(40, 8).expect("consumer mirror");
            mirror.vt_write(&final_snapshot.expect("EOF publishes the final grid"));
            let mut formatter = Formatter::new(
                &mirror,
                FormatterOptions::new()
                    .with_format(Format::Plain)
                    .with_trim(true),
            )
            .expect("viewport formatter");
            let mut text = [0; 512];
            let len = formatter.format_buf(&mut text).expect("bounded viewport");
            assert_eq!(
                std::str::from_utf8(&text[..len])
                    .expect("viewport text")
                    .trim_end(),
                "before\nfinal-tail"
            );
            tokio::time::timeout(ACTOR_EXIT_DEADLINE, queued)
                .await
                .expect("queued capture resumes")
                .expect("queued consumer is answered")
                .expect("queued consumer captures the EOF grid");
            token.cancel();
            run.await.expect("actor stops");
        })
        .await;
}
