/// `edge/@9`, the satellite pane a parked window spawned.
fn spawned_edge_pane() -> ResourceId {
    ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9)
}

/// A bootstrapped loop over a socket pair, with a window parked on the
/// attach of the satellite pane it just spawned (request 900).
async fn loop_with_spawned_window() -> (SessionLoop, Connection, Connection, Vec<u8>) {
    Box::pin(loop_with_window_on(ServerFeatureSet::new(), None)).await
}

/// [`loop_with_spawned_window`] on a hub advertising `features`, the pane's
/// spawn bound to `instance` when that is `Some`.
async fn loop_with_window_on(
    features: ServerFeatureSet,
    instance: Option<phux_protocol::ids::ServerInstance>,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (mut state, client, server, out) = Box::pin(bootstrapped_loop_with(features)).await;
    state.mirror.pending_windows.insert(
        900,
        PendingWindow {
            name: "2".to_owned(),
            adopt: Some(crate::attach::actions::Adopt::Spawned(
                crate::attach::actions::SpawnedPane {
                    id: spawned_edge_pane(),
                    instance,
                },
            )),
        },
    );
    (state, client, server, out)
}

/// A loop over a socket pair on a server advertising `features`, with the
/// ATTACHED frame replayed but the recv-arm drain not yet run.
async fn loop_before_drain(
    features: ServerFeatureSet,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    Box::pin(loop_before_drain_ext(
        features,
        phux_protocol::ServerFeatureExtSet::new(),
    ))
    .await
}

/// [`loop_before_drain`] with an extended-feature word as well.
async fn loop_before_drain_ext(
    features: ServerFeatureSet,
    features_ext: phux_protocol::ServerFeatureExtSet,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Connection::from_stream(a);
    let server = Connection::from_stream(b);
    let negotiated = NegotiatedBootstrap {
        profile: BootstrapProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
        server_features: features,
        server_features_ext: features_ext,
    };
    let mut state = SessionLoop::new(
        negotiated,
        PredictiveConfig::disabled(),
        false,
        None,
        EntryPick::default(),
        None,
    )
    .unwrap();
    let mut out = Vec::new();
    state
        .bootstrap(&mut client, &mut out, initial_attached(), None)
        .await
        .unwrap();
    (state, client, server, out)
}

/// [`loop_before_drain`] with the deferred bootstrap outbound sent.
async fn bootstrapped_loop_with(
    features: ServerFeatureSet,
) -> (SessionLoop, Connection, Connection, Vec<u8>) {
    let (mut state, mut client, server, out) = Box::pin(loop_before_drain(features)).await;
    state
        .emit_deferred_bootstrap_outbound(&mut client)
        .await
        .unwrap();
    (state, client, server, out)
}

#[tokio::test(flavor = "current_thread")]
async fn host_path_picker_requires_the_negotiated_extension() {
    use phux_protocol::{ServerFeatureExt, ServerFeatureExtSet};

    for (features_ext, expected) in [
        (ServerFeatureExtSet::new(), false),
        (
            ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]),
            true,
        ),
    ] {
        let negotiated = NegotiatedBootstrap {
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features: ServerFeatureSet::new(),
            server_features_ext: features_ext,
        };
        let state = SessionLoop::new(
            negotiated,
            PredictiveConfig::disabled(),
            false,
            None,
            EntryPick::default(),
            None,
        )
        .unwrap();
        assert_eq!(state.path.supported, expected);
    }
}

/// A last-pane `RESOURCE_CLOSED` on the first recv-arm burst must
/// end the attach without sending the deferred bootstrap subscriptions.
#[tokio::test(flavor = "current_thread")]
async fn last_pane_close_on_first_burst_skips_deferred_bootstrap_writes() {
    let (mut state, mut client, mut server, mut out) =
        loop_before_drain(ServerFeatureSet::new()).await;
    let closed = FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(1),
        exit_status: Some(7),
        reason: phux_protocol::wire::frame::CloseReason::Exited,
        signal: None,
    };
    let step = state
        .handle_frame_burst(&mut client, &mut out, None, closed)
        .await
        .unwrap();
    match step {
        Step::Exit(LoopExit::Detached {
            end: AttachEnd::LastPaneClosed {
                exit_status: Some(7),
            },
            ..
        }) => {}
        Step::Continue => panic!("expected LastPaneClosed(7), got Continue"),
        Step::Exit(other) => panic!("expected LastPaneClosed(7), got {other:?}"),
    }
    assert!(
        server.try_recv().unwrap().is_none(),
        "last-pane close must not send Subscribe* / GetMetadata into a session that already ended",
    );
    drop(client);
    drop(server);
}

#[tokio::test(flavor = "current_thread")]
async fn attached_generation_discards_delayed_stream_bind_reply() {
    let (mut state, mut client, _server, _out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    state.pending_stream_binds.insert(41, ResourceId::local(2));
    state
        .coordinate_multistream_frame(&mut client, &initial_attached())
        .await
        .unwrap();
    assert!(state.pending_stream_binds.is_empty());

    state
        .coordinate_multistream_frame(
            &mut client,
            &FrameKind::CommandResult {
                request_id: 41,
                result: phux_protocol::wire::frame::CommandResult::Ok,
            },
        )
        .await
        .unwrap();
    assert!(state.pending_stream_binds.is_empty());

    // A correlated ERROR releases its pending bind too.
    state.pending_stream_binds.insert(42, ResourceId::local(2));
    state
        .coordinate_multistream_frame(
            &mut client,
            &FrameKind::Error {
                request_id: Some(42),
                code: phux_protocol::wire::frame::ErrorCode::ResourceExhausted,
                message: "refused".to_owned(),
            },
        )
        .await
        .unwrap();
    assert!(state.pending_stream_binds.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn pending_stream_bind_tracking_matches_connection_cap() {
    let (mut state, _client, _server, _out) = bootstrapped_loop_with(ServerFeatureSet::new()).await;
    for request_id in 0..u32::try_from(MAX_PENDING_STREAM_BINDS).unwrap() {
        state
            .track_pending_stream_bind(request_id, ResourceId::local(request_id + 1))
            .unwrap();
    }
    assert!(matches!(
        state.track_pending_stream_bind(u32::MAX, ResourceId::local(u32::MAX)),
        Err(AttachError::Protocol(message)) if message.contains("stream cap exceeded")
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn late_bound_reflow_uses_current_zoom_and_chrome_once() {
    let (mut state, mut client, mut server, _) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let _ = frames_sent(&mut client, &mut server).await;
    let first = ResourceId::local(1);
    let bound = ResourceId::local(2);
    let offscreen = ResourceId::local(3);
    let workspace = &mut state.mirror.workspace;
    workspace.active_group_mut().unwrap().tree = Some(
        crate::layout::split_at(
            &crate::layout::LayoutNode::Leaf(first.clone()),
            &first,
            &bound,
            crate::layout::SplitDir::Horizontal,
            0.5,
        )
        .unwrap(),
    );
    workspace.add_group("offscreen".into(), offscreen.clone());
    workspace.select(0);
    state.mirror.zoomed = Some(bound.clone());
    state.viewport_dims = (120, 40);
    let sidebar = Some(wide_sidebar());
    let content = state.content(sidebar);
    // A layout reply and a late binding may settle in the same drain. Neither
    // may restore the old unzoomed size or leave a second reflow armed.
    state.bind_reflow_owed = true;
    state
        .emit_outcome_requests(
            &mut client,
            &mut FrameOutcome {
                layout_get_answered: true,
                ..FrameOutcome::default()
            },
            sidebar,
            None,
        )
        .await
        .unwrap();
    let resized: HashMap<_, _> = frames_sent(&mut client, &mut server)
        .await
        .into_iter()
        .map(|frame| match frame {
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                ..
            } => (terminal_id, (cols, rows)),
            other => panic!("unexpected late-bind frame: {other:?}"),
        })
        .collect();
    assert_eq!(
        resized,
        [
            (bound, (content.w, content.h)),
            (offscreen, (content.w, content.h)),
        ]
        .into_iter()
        .collect()
    );
    state
        .emit_outcome_requests(&mut client, &mut FrameOutcome::default(), sidebar, None)
        .await
        .unwrap();
    assert!(frames_sent(&mut client, &mut server).await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn sigwinch_reasserts_pane_targets_when_host_cells_are_unchanged() {
    let (mut state, mut client, mut server, mut out) =
        bootstrapped_loop_with(ServerFeatureSet::new()).await;
    let _ = frames_sent(&mut client, &mut server).await;
    let host = current_viewport_or_default();
    state.viewport_dims = (host.cols.max(1), host.rows.max(1));
    out.clear();
    state.on_resize(&mut client, &mut out, None).await.unwrap();
    assert!(
        !out.windows(4).any(|seq| seq == b"\x1b[2J"),
        "a same-grid resize must keep the screen; the server sends no resync to refill it: {out:?}"
    );
    let content = state.content(None);
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        matches!(
            sent.as_slice(),
            [
                FrameKind::ViewportResize { .. },
                FrameKind::ResizeTerminal { terminal_id, cols, rows, .. },
            ] if terminal_id == &ResourceId::local(1)
                && (*cols, *rows) == (content.w, content.h)
        ),
        "a session vote must be followed by the explicit chrome-inset target: {sent:?}"
    );
}

/// ADR-0145: against a server that applies per-pane cell size, an outer
/// resize sends no window-size vote, only each pane's exact tile, so no pane
/// is ever sized to the full outer window on the way to its tile.
#[tokio::test(flavor = "current_thread")]
async fn sigwinch_sends_only_pane_targets_when_the_server_applies_cell_px() {
    use phux_protocol::{ServerFeatureExt, ServerFeatureExtSet};
    let (mut state, mut client, mut server, mut out) = Box::pin(loop_before_drain_ext(
        ServerFeatureSet::new(),
        ServerFeatureExtSet::with(&[ServerFeatureExt::ResizeCellPx]),
    ))
    .await;
    state
        .emit_deferred_bootstrap_outbound(&mut client)
        .await
        .unwrap();
    let _ = frames_sent(&mut client, &mut server).await;
    state.on_resize(&mut client, &mut out, None).await.unwrap();
    let content = state.content(None);
    let sent = frames_sent(&mut client, &mut server).await;
    assert!(
        matches!(
            sent.as_slice(),
            [FrameKind::ResizeTerminal { terminal_id, cols, rows, .. }]
                if terminal_id == &ResourceId::local(1)
                    && (*cols, *rows) == (content.w, content.h)
        ),
        "no VIEWPORT_RESIZE vote, only the chrome-inset tile: {sent:?}"
    );
}

/// Drain sent frames through a FIFO barrier, with no timing-based idle guess.
async fn frames_sent(client: &mut Connection, server: &mut Connection) -> Vec<FrameKind> {
    client
        .send(&FrameKind::GetMetadata {
            request_id: u32::MAX,
            scope: Scope::Global,
            key: "test.barrier".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut frames = Vec::new();
        loop {
            let frame = server.recv().await.unwrap();
            if matches!(
                frame,
                FrameKind::GetMetadata {
                    request_id: u32::MAX,
                    ..
                }
            ) {
                return frames;
            }
            frames.push(frame);
        }
    })
    .await
    .unwrap()
}

fn painted_sidebar_text(bytes: &[u8]) -> String {
    use crate::attach::render::ReplicaWalk;
    let mut probe = PaneSlot::new_with_size(100, 24).unwrap();
    probe.terminal.vt_write(bytes);
    let mut frame = phux_core::screen::RenderedFrame::blank(100, 24);
    probe
        .renderer
        .render_at_cells(
            ReplicaWalk::for_test(&probe.terminal),
            &mut frame,
            (0, 0),
            (100, 24),
        )
        .unwrap();
    frame
        .cells
        .chunks(100)
        .map(|row| {
            row[..32]
                .iter()
                .map(|c| c.grapheme.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
