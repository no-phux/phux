//! Grid revisions, multi-pane snapshots, scrollback search, and the session catalog.

use super::*;

#[test]
#[allow(clippy::too_many_lines)]
fn stale_tombstone_preserves_newer_grid_revision() {
    let terminal_id = phux_protocol::ResourceId::local(7);
    let c_terminal_id = PhuxResourceId {
        kind: 0,
        id: 7,
        host: PhuxBytes::default(),
    };
    let stream_id = phux_protocol::StreamId::new(1).expect("stream");
    let first_bootstrap = phux_protocol::BootstrapId::new(1).expect("bootstrap");
    let second_bootstrap = phux_protocol::BootstrapId::new(2).expect("bootstrap");
    let session_id = SessionId::new(1);
    let window_id = phux_protocol::WindowId::new(1);
    let snapshot =
        phux_protocol::wire::info::SessionSnapshot::new(session_id, window_id, terminal_id.clone())
            .with_sessions(vec![phux_protocol::wire::info::SessionInfo::new(
                session_id, "working",
            )])
            .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
                window_id, session_id, "working",
            )])
            .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
                terminal_id.clone(),
                window_id,
                80,
                24,
            )]);
    let client = attaching(&[], 7);
    assert_eq!(
        feed(
            client,
            &FrameKind::Attached {
                attach_id: 7,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(9),
            },
        ),
        PhuxClientResult::Ok
    );
    for bootstrap_id in [first_bootstrap, second_bootstrap] {
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"$ "),
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            assert_eq!(feed(client, &frame), PhuxClientResult::Ok);
        }
    }
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok
    );
    assert_eq!(
        feed(
            client,
            &FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id: first_bootstrap,
                reason: phux_protocol::wire::frame::TombstoneReason::OutboundGap,
                last_valid_seq: 0,
            },
        ),
        PhuxClientResult::Ok
    );

    client::RENDER_CACHE_BUILDS.set(0);
    let mut view = PhuxTerminalGridView::default();
    assert_eq!(
        unsafe { phux_client_terminal_grid(client, &raw const c_terminal_id, &raw mut view,) },
        PhuxClientResult::Ok
    );
    assert_eq!(view.stream_id, stream_id.get());
    assert_eq!(view.bootstrap_id, second_bootstrap.get());
    assert_eq!(client::RENDER_CACHE_BUILDS.get(), 1);
    for _ in 0..4 {
        assert_eq!(
            unsafe {
                phux_client_anchor_release(client, &raw const c_terminal_id, view.top_anchor)
            },
            PhuxClientResult::Ok
        );
        assert_eq!(
            unsafe { phux_client_terminal_grid(client, &raw const c_terminal_id, &raw mut view) },
            PhuxClientResult::Ok
        );
        assert_eq!(view.cell_count, 80 * 24);
    }
    assert_eq!(
        client::RENDER_CACHE_BUILDS.get(),
        1,
        "cache hits must not construct libghostty render state or iterators"
    );
    if view.top_anchor.opaque_id != 0 {
        assert_eq!(
            unsafe {
                phux_client_anchor_release(client, &raw const c_terminal_id, view.top_anchor)
            },
            PhuxClientResult::Ok
        );
    }
    unsafe { phux_client_free(client) };
}

#[test]
fn attached_snapshot_scopes_participants_to_the_focused_session() {
    let client = attaching(&[], 7);
    let focused_session = SessionId::new(2);
    let other_session = SessionId::new(1);
    let focused_window = phux_protocol::WindowId::new(20);
    let other_window = phux_protocol::WindowId::new(10);
    let focused_terminal = phux_protocol::ResourceId::local(30);
    let other_terminal = phux_protocol::ResourceId::local(40);
    let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
        focused_session,
        focused_window,
        focused_terminal.clone(),
    )
    .with_sessions(vec![
        phux_protocol::wire::info::SessionInfo::new(other_session, "other"),
        phux_protocol::wire::info::SessionInfo::new(focused_session, "focused"),
    ])
    .with_windows(vec![
        phux_protocol::wire::info::WindowInfo::new(other_window, other_session, "other".to_owned()),
        phux_protocol::wire::info::WindowInfo::new(
            focused_window,
            focused_session,
            "focused".to_owned(),
        ),
    ])
    .with_resources(vec![
        phux_protocol::wire::info::ResourceInfo::new(other_terminal.clone(), other_window, 80, 24),
        phux_protocol::wire::info::ResourceInfo::new(
            focused_terminal.clone(),
            focused_window,
            80,
            24,
        ),
    ]);

    assert_eq!(
        feed(
            client,
            &FrameKind::Attached {
                attach_id: 7,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(9),
            },
        ),
        PhuxClientResult::Ok
    );
    assert!(unsafe { (*client).inner.active_attach_contains(&focused_terminal) });
    assert!(
        !unsafe { (*client).inner.active_attach_contains(&other_terminal) },
        "a terminal in another session is never bootstrapped by this attach"
    );
    unsafe { phux_client_free(client) };
}

fn three_pane_snapshot(
    seed: &phux_protocol::ResourceId,
    horizontal: &phux_protocol::ResourceId,
    vertical: &phux_protocol::ResourceId,
) -> phux_protocol::wire::info::SessionSnapshot {
    let catalog_session = SessionId::new(1);
    let working_session = SessionId::new(2);
    let catalog_window = phux_protocol::WindowId::new(10);
    let working_window = phux_protocol::WindowId::new(20);
    phux_protocol::wire::info::SessionSnapshot::new(working_session, working_window, seed.clone())
        .with_sessions(vec![
            phux_protocol::wire::info::SessionInfo::new(catalog_session, "catalog"),
            phux_protocol::wire::info::SessionInfo::new(working_session, "working"),
        ])
        .with_windows(vec![
            phux_protocol::wire::info::WindowInfo::new(catalog_window, catalog_session, "catalog"),
            phux_protocol::wire::info::WindowInfo::new(working_window, working_session, "working")
                .with_active_resource(Some(seed.clone())),
        ])
        .with_resources(vec![
            phux_protocol::wire::info::ResourceInfo::new(
                phux_protocol::ResourceId::local(1),
                catalog_window,
                80,
                24,
            ),
            phux_protocol::wire::info::ResourceInfo::new(seed.clone(), working_window, 80, 24),
            phux_protocol::wire::info::ResourceInfo::new(
                horizontal.clone(),
                working_window,
                40,
                24,
            ),
            phux_protocol::wire::info::ResourceInfo::new(vertical.clone(), working_window, 40, 12),
        ])
}

#[test]
fn search_pin_moves_rendered_viewport_and_follow_live_restores_tail() {
    let client = client_with_searchable_scrollback();
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    let mut view = PhuxTerminalGridView::default();
    let mut results = ptr::null();
    let mut count = 0;
    // SAFETY: all spans and output pointers belong to this test; borrowed
    // grid/search data is consumed before the next mutable client call.
    unsafe {
        assert_eq!(
            phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
            PhuxClientResult::Ok
        );
        let tail_offset = view.history_viewport_offset;
        assert!(tail_offset > 1);
        let text = std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap()).unwrap();
        assert!(text.contains("LIVE TAIL"));
        assert!(!text.contains("OFFSCREEN MATCH"));
        assert_eq!(
            phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_client_search(
                client,
                &raw const terminal,
                bytes_out(b"offscreen match"),
                false,
                &raw mut results,
                &raw mut count
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(count, 1);
        let matched = *results;
        assert_eq!(
            phux_client_history_viewport_pin(client, &raw const terminal, matched.start),
            PhuxClientResult::Ok
        );
        // The viewport must own its pin independently of the transient results.
        assert_eq!(
            phux_client_search_results_release(client),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
            PhuxClientResult::Ok
        );
        assert_eq!(
            view.history_viewport_offset, 1,
            "search must move the actual rendered viewport"
        );
        let text = std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap()).unwrap();
        assert!(text.starts_with("OFFSCREEN MATCH"));
        assert!(!text.contains("LIVE TAIL"));
        assert_eq!(
            phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_client_history_follow_live(client, &raw const terminal),
            PhuxClientResult::Ok
        );
        assert_eq!(
            phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
            PhuxClientResult::Ok
        );
        assert_eq!(
            view.history_viewport_offset, tail_offset,
            "follow-live must scroll the engine back to the tail"
        );
        let text = std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap()).unwrap();
        assert!(text.contains("LIVE TAIL"));
        assert_eq!(
            phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
            PhuxClientResult::Ok
        );
        phux_client_free(client);
    }
}

#[test]
fn scroll_kind_four_jumps_between_shell_prompts() {
    let client = attaching(&[], 7);
    let terminal_id = phux_protocol::ResourceId::local(1);
    let snapshot = single_terminal_snapshot(terminal_id.clone(), 40, 12);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok,
    );
    let mut bytes = b"\x1b]133;A\x07$ first\r\n".to_vec();
    for row in 0..20 {
        bytes.extend_from_slice(format!("out {row}\r\n").as_bytes());
    }
    bytes.extend_from_slice(b"\x1b]133;A\x07$ live");
    feed_complete_bootstrap(client, &terminal_id, &bytes);
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok,
    );
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    let mut view = PhuxTerminalGridView::default();
    // SAFETY: all spans and output pointers belong to this test; borrowed
    // grid data is consumed before the next mutable client call.
    unsafe {
        let mut top = |client| {
            assert_eq!(
                phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
                PhuxClientResult::Ok
            );
            let text = std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap())
                .unwrap()
                .to_owned();
            assert_eq!(
                phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
                PhuxClientResult::Ok
            );
            (view.history_viewport_offset, text)
        };
        let (tail, _) = top(client);
        assert_eq!(
            phux_client_scroll_viewport(client, &raw const terminal, 4, -1),
            PhuxClientResult::Ok
        );
        let (offset, text) = top(client);
        assert_eq!(offset, 0);
        assert!(text.starts_with("$ first"));
        assert_eq!(
            phux_client_scroll_viewport(client, &raw const terminal, 4, 1),
            PhuxClientResult::Ok
        );
        assert_eq!(top(client).0, tail, "a live prompt follows the tail");
        assert_eq!(
            phux_client_scroll_viewport(client, &raw const terminal, 5, 0),
            PhuxClientResult::InvalidArgument
        );
        phux_client_free(client);
    }
}

#[test]
fn three_pane_attach_resolves_unbootstrapped_seed_by_closure() {
    let client = attaching(&[], 7);
    let seed = phux_protocol::ResourceId::local(2);
    let horizontal = phux_protocol::ResourceId::local(3);
    let vertical = phux_protocol::ResourceId::local(4);
    let snapshot = three_pane_snapshot(&seed, &horizontal, &vertical);
    assert_eq!(
        feed(
            client,
            &FrameKind::Attached {
                attach_id: 7,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(9),
            },
        ),
        PhuxClientResult::Ok,
    );
    feed_complete_bootstrap(client, &horizontal, b"horizontal");
    feed_complete_bootstrap(client, &vertical, b"vertical");
    assert_eq!(
        feed(
            client,
            &FrameKind::ResourceClosed {
                terminal_id: seed.clone(),
                exit_status: None,
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            },
        ),
        PhuxClientResult::Ok,
    );
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok,
    );
    assert!(unsafe { (*client).inner.attached });
    assert_eq!(unsafe { (*client).inner.sessions.len() }, 2);
    assert!(!unsafe { (*client).inner.active_attach_contains(&seed) });
    assert!(unsafe { (*client).inner.active_attach_contains(&horizontal) });
    assert!(unsafe { (*client).inner.active_attach_contains(&vertical) });
    unsafe { phux_client_free(client) };
}

#[test]
fn attached_snapshot_exposes_the_server_session_catalog() {
    let client = attaching(&[], 7);
    let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
        SessionId::new(2),
        phux_protocol::WindowId::new(20),
        phux_protocol::ResourceId::local(30),
    )
    .with_sessions(vec![
        phux_protocol::wire::info::SessionInfo::new(SessionId::new(1), "other")
            .with_created_at_unix_secs(100)
            .with_window_count(2),
        phux_protocol::wire::info::SessionInfo::new(SessionId::new(2), "focused")
            .with_created_at_unix_secs(200)
            .with_window_count(3)
            .with_attached_client_count(4),
    ]);
    assert_eq!(
        feed(
            client,
            &FrameKind::Attached {
                attach_id: 7,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(9),
            },
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(unsafe { phux_client_session_count(client) }, 2);

    let mut session = PhuxSessionInfo::default();
    assert_eq!(
        unsafe { phux_client_session_get(client, 1, &raw mut session) },
        PhuxClientResult::Ok
    );
    assert_eq!(session.session_id, 2);
    assert_eq!(
        unsafe { bytes_in(session.name.data, session.name.len) }.unwrap(),
        b"focused"
    );
    assert_eq!(session.created_at_unix_secs, 200);
    assert_eq!(session.window_count, 3);
    assert_eq!(session.attached_client_count, 4);
    assert!(session.focused);

    assert_eq!(
        unsafe { phux_client_session_get(client, 2, &raw mut session) },
        PhuxClientResult::NoValue
    );
    assert!(session.name.data.is_null());
    unsafe { phux_client_free(client) };
}
