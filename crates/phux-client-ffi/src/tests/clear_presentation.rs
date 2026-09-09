use super::*;

/// Copy the borrowed grid immediately and release its tracked top anchor.
fn grid(client: *mut PhuxClient) -> (PhuxTerminalGridView, String) {
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    let mut view = PhuxTerminalGridView::default();
    // SAFETY: this fixture owns the live client and every output span.
    unsafe {
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
        (view, text)
    }
}

fn output(client: *mut PhuxClient, seq: u64, payload: &'static [u8]) {
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::ResourceOutput {
                terminal_id: phux_protocol::ResourceId::local(1),
                stream_id: phux_protocol::StreamId::new(7).unwrap(),
                bootstrap_id: phux_protocol::BootstrapId::new(1).unwrap(),
                seq,
                bytes: bytes::Bytes::from_static(payload),
            }
        ),
        PhuxClientResult::Ok
    );
}

#[test]
fn clear_erases_only_local_presentation_and_keeps_next_output_live() {
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    let client = client_with_searchable_scrollback();
    let peer = client_with_searchable_scrollback();
    output(client, 1, b"\x1b]2;durable title\x07\x1b[?2004h");
    let (before, text) = grid(client);
    assert!(text.contains("LIVE TAIL"));
    assert!(before.history_total_rows > u64::from(before.rows));
    // SAFETY: all handles belong to this fixture on this thread.
    unsafe {
        (*client)
            .inner
            .search(&phux_protocol::ResourceId::local(1), b"OFFSCREEN", true)
            .unwrap();
        let found = (&(*client).inner.search_results)[0];
        assert_eq!(
            phux_client_selection_set(client, &raw const terminal, found.start, found.end, false),
            PhuxClientResult::Ok
        );
        let outgoing = phux_client_outgoing_count(client);
        assert_eq!(
            phux_client_clear_presentation(client, &raw const terminal, 7, 1),
            PhuxClientResult::Ok
        );
        assert_eq!(phux_client_outgoing_count(client), outgoing);
        assert_eq!(
            phux_client_anchor_release(client, &raw const terminal, found.start),
            PhuxClientResult::InvalidState
        );
        assert!((*client).inner.search_results.is_empty());
        let mut selected = PhuxBytes::default();
        assert_eq!(
            phux_client_selection_text(client, &raw const terminal, &raw mut selected),
            PhuxClientResult::InvalidState
        );
        assert_eq!(
            (*client)
                .inner
                .terminal(&phux_protocol::ResourceId::local(1))
                .unwrap()
                .title()
                .unwrap(),
            "durable title"
        );
        assert!(
            (*client)
                .inner
                .terminal(&phux_protocol::ResourceId::local(1))
                .unwrap()
                .mode(libghostty_vt::terminal::Mode::BRACKETED_PASTE)
                .unwrap()
        );
    }
    let (after, text) = grid(client);
    assert!(
        text.trim().is_empty(),
        "Clear must erase rendered cells: {text:?}"
    );
    assert_eq!((after.cols, after.rows), (before.cols, before.rows));
    assert_eq!(
        (after.stream_id, after.bootstrap_id, after.last_seq),
        (before.stream_id, before.bootstrap_id, before.last_seq)
    );
    assert!(after.document_revision > before.document_revision);
    assert_eq!((after.cursor_col, after.cursor_row), (0, 0));
    assert_eq!(after.history_total_rows, u64::from(after.rows));
    assert_eq!(after.history_viewport_offset, 0);
    assert!(!after.history_loading && !after.history_has_more);
    assert!(
        grid(peer).1.contains("LIVE TAIL"),
        "another client must keep its presentation"
    );
    output(client, 2, b"AFTER CLEAR");
    let (after_output, text) = grid(client);
    assert_eq!(after_output.last_seq, 2);
    assert!(text.starts_with("AFTER CLEAR"));
    // SAFETY: release these fixture-owned clients once all views are consumed.
    unsafe {
        phux_client_free(client);
        phux_client_free(peer);
    }
}

#[test]
fn clear_refuses_stale_generations_and_disconnected_clients_without_mutation() {
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    let client = client_with_searchable_scrollback();
    let (before, text) = grid(client);
    // SAFETY: the fixture owns the live client; ids and outputs are valid.
    unsafe {
        assert_eq!(
            phux_client_clear_presentation(client, &raw const terminal, 8, 1),
            PhuxClientResult::InvalidState
        );
        assert_eq!(
            phux_client_clear_presentation(client, &raw const terminal, 7, 2),
            PhuxClientResult::InvalidState
        );
        let (after, after_text) = grid(client);
        assert_eq!(text, after_text);
        assert_eq!(before.document_revision, after.document_revision);
        assert_eq!(phux_client_disconnect(client), PhuxClientResult::Ok);
        assert_eq!(
            phux_client_clear_presentation(client, &raw const terminal, 7, 1),
            PhuxClientResult::InvalidState
        );
        phux_client_free(client);
    }
}

#[test]
fn clear_preserves_partial_csi_and_its_rendition() {
    let synthesized = client_with_searchable_scrollback();
    output(synthesized, 1, b"\x1b[3");
    // Native READY must preserve continuation captured on the source, too.
    let (bootstrap, _, _) = native_capture(b"\x1b[3");
    for (client, seq) in [(synthesized, 2), (native_client(&bootstrap), 1)] {
        clear_then_resume(client, seq, b"1mX", "X");
        // SAFETY: consume the cell style before freeing the fixture-owned client.
        unsafe {
            let engine = (*client)
                .inner
                .terminal(&phux_protocol::ResourceId::local(1))
                .unwrap();
            let cell = engine
                .grid_ref(libghostty_vt::terminal::Point::Active(
                    libghostty_vt::terminal::PointCoordinate { x: 0, y: 0 },
                ))
                .unwrap();
            assert_eq!(
                cell.style().unwrap().fg_color,
                libghostty_vt::style::StyleColor::Palette(libghostty_vt::style::PaletteIndex(1))
            );
            phux_client_free(client);
        }
    }
}

fn clear_then_resume(client: *mut PhuxClient, seq: u64, suffix: &'static [u8], text: &str) {
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    let (before, _) = grid(client);
    // SAFETY: this fixture owns the client and uses its exact live generation.
    unsafe {
        assert_eq!(phux_client_outgoing_clear(client), PhuxClientResult::Ok);
        assert_eq!(
            phux_client_clear_presentation(client, &raw const terminal, 7, 1),
            PhuxClientResult::Ok
        );
        assert_eq!(phux_client_outgoing_count(client), 0);
    }
    let (cleared, blank) = grid(client);
    assert!(
        blank.trim().is_empty(),
        "Clear did not blank presentation: {blank:?}"
    );
    assert_eq!(cleared.last_seq, before.last_seq);
    assert_eq!(cleared.history_total_rows, u64::from(cleared.rows));
    output(client, seq, suffix);
    assert_eq!(grid(client).1.trim(), text);
}

#[test]
fn clear_preserves_partial_osc_and_its_title() {
    let prefix = b"\x1b]2;pending ";
    let synthesized = client_with_searchable_scrollback();
    output(synthesized, 1, prefix);
    let (bootstrap, _, _) = native_capture(prefix);
    for (client, seq) in [(synthesized, 2), (native_client(&bootstrap), 1)] {
        clear_then_resume(client, seq, b"title\x07X", "X");
        // SAFETY: read engine state while this test owns the live client.
        unsafe {
            assert_eq!(
                (*client)
                    .inner
                    .terminal(&phux_protocol::ResourceId::local(1))
                    .unwrap()
                    .title()
                    .unwrap(),
                "pending title"
            );
            phux_client_free(client);
        }
    }
}

#[test]
fn clear_preserves_partial_dcs_and_utf8() {
    let cases: &[(&[u8], &[u8], &str)] = &[
        (b"\x1bP1;2|pending", b"body\x1b\\X", "X"),
        (b"\xe2", b"\x82\xac", "€"),
        (b"\xe2\x82", b"\xac", "€"),
        (b"\xf0", b"\x9f\x98\x80", "\u{1f600}"),
        (b"\xf0\x9f", b"\x98\x80", "\u{1f600}"),
        (b"\xf0\x9f\x98", b"\x80", "\u{1f600}"),
    ];
    for &(prefix, suffix, text) in cases {
        let synthesized = client_with_searchable_scrollback();
        output(synthesized, 1, prefix);
        let (bootstrap, _, _) = native_capture(prefix);
        for (client, seq) in [(synthesized, 2), (native_client(&bootstrap), 1)] {
            clear_then_resume(client, seq, suffix, text);
            // SAFETY: the borrowed grid was consumed before freeing this client.
            unsafe {
                phux_client_free(client);
            }
        }
    }
}

fn native_capture(pending: &[u8]) -> (Vec<u8>, Vec<u8>, u32) {
    use libghostty_vt::snapshot::incremental::{CaptureEventKind, CaptureOptions, Error};
    let mut source = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
        cols: 40,
        rows: 12,
        max_scrollback: 100,
    })
    .unwrap();
    for row in 0..40 {
        source.vt_write(format!("original row {row}\r\n").as_bytes());
    }
    let rows = u32::try_from(source.scrollback_rows().unwrap()).unwrap();
    source.vt_write(pending);
    let mut capture = source.capture(CaptureOptions::default()).unwrap();
    let mut bootstrap = Vec::new();
    let mut history = Vec::new();
    let mut ready = false;
    loop {
        let required = match capture.next(&mut []) {
            Err(Error::OutOfSpace { required_bytes, .. }) => required_bytes,
            other => panic!("capture size probe: {other:?}"),
        };
        let mut bytes = vec![0; required];
        let kind = capture.next(&mut bytes).unwrap().kind;
        if ready {
            history.extend(bytes);
        } else {
            bootstrap.extend(bytes);
        }
        match kind {
            CaptureEventKind::Ready { .. } => ready = true,
            CaptureEventKind::Finish => return (bootstrap, history, rows),
            _ => {}
        }
    }
}

fn feed_native_bootstrap(client: *mut PhuxClient, bootstrap_id: u64, bytes: &[u8]) {
    let terminal_id = phux_protocol::ResourceId::local(1);
    let stream_id = phux_protocol::StreamId::new(7).unwrap();
    let bootstrap_id = phux_protocol::BootstrapId::new(bootstrap_id).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::NativeState {
                codec: phux_protocol::EngineCodec::LibghosttyCheckpointV2,
            },
            cols: 40,
            rows: 12,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(bytes),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: Some(bytes::Bytes::from_static(b"older")),
        },
    ] {
        let result = feed_kind(client, &frame);
        // SAFETY: this fixture owns the client throughout bootstrap.
        let error = unsafe { String::from_utf8_lossy(&(*client).inner.last_error).into_owned() };
        assert_eq!(result, PhuxClientResult::Ok, "{frame:?}: {error}");
    }
}

fn native_client(bootstrap: &[u8]) -> *mut PhuxClient {
    let mut inner = Client::new(Limits {
        bootstrap_chunk: 256 * 1024,
        history_page: 256 * 1024,
        history_page_rows: 128,
        history_cache_bytes: 1024 * 1024,
        history_materialized_rows: 1024,
        history_prefetch_rows: 64,
    });
    inner.protocol_ready = true;
    inner.attach_queued = true;
    inner.expected_attach_id = Some(7);
    inner.selected_profile = Some(phux_protocol::BootstrapProfile::NativeState {
        codec: phux_protocol::EngineCodec::LibghosttyCheckpointV2,
        features: phux_protocol::EngineFeatureSet::required_native(),
    });
    inner.install_profile(
        inner.selected_profile.unwrap(),
        phux_protocol::BootstrapLimits::new(256 * 1024, 256 * 1024).unwrap(),
    );
    let client = Box::into_raw(Box::new(PhuxClient {
        inner,
        _not_send_sync: std::marker::PhantomData,
    }));
    let terminal = phux_protocol::ResourceId::local(1);
    let window = phux_protocol::WindowId::new(1);
    let session = SessionId::new(1);
    let snapshot =
        phux_protocol::wire::info::SessionSnapshot::new(session, window, terminal.clone())
            .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
                window, session, "native",
            )])
            .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
                terminal, window, 40, 12,
            )]);
    assert_eq!(
        feed_kind(
            client,
            &FrameKind::Attached {
                attach_id: 7,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(9),
            }
        ),
        PhuxClientResult::Ok
    );
    feed_native_bootstrap(client, 1, bootstrap);
    assert_eq!(
        feed_kind(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok
    );
    client
}

fn history_page(bootstrap_id: u64, bytes: &[u8], rows: u32) -> FrameKind {
    FrameKind::HistoryPage {
        terminal_id: phux_protocol::ResourceId::local(1),
        stream_id: phux_protocol::StreamId::new(7).unwrap(),
        bootstrap_id: phux_protocol::BootstrapId::new(bootstrap_id).unwrap(),
        cursor: bytes::Bytes::from_static(b"older"),
        page_seq: 1,
        next_cursor: None,
        rows,
        payload: bytes::Bytes::copy_from_slice(bytes),
    }
}

#[test]
fn clear_cancels_native_history_without_retiring_live_or_replacement_generations() {
    let (bootstrap, history, rows) = native_capture(b"");
    let client = native_client(&bootstrap);
    let terminal = PhuxResourceId {
        id: 1,
        ..PhuxResourceId::default()
    };
    assert!(grid(client).0.history_loading);
    // SAFETY: this test owns the live client and uses its exact generation.
    unsafe {
        assert_eq!(phux_client_outgoing_clear(client), PhuxClientResult::Ok);
        assert_eq!(
            phux_client_clear_presentation(client, &raw const terminal, 7, 1),
            PhuxClientResult::Ok
        );
        assert_eq!(phux_client_outgoing_count(client), 0);
    }
    assert_eq!(
        feed_kind(client, &history_page(1, &history, rows)),
        PhuxClientResult::Ok
    );
    let (cleared, text) = grid(client);
    assert!(text.trim().is_empty());
    assert!(!cleared.history_loading && !cleared.history_has_more);
    assert_eq!(cleared.history_pages_loaded, 0);
    output(client, 1, b"NEW LIVE OUTPUT");
    assert!(grid(client).1.starts_with("NEW LIVE OUTPUT"));
    feed_native_bootstrap(client, 2, &bootstrap);
    assert!(grid(client).0.history_loading);
    assert_eq!(
        feed_kind(client, &history_page(2, &history, rows)),
        PhuxClientResult::Ok
    );
    let (replacement, text) = grid(client);
    assert!(text.contains("original row"));
    assert!(replacement.history_total_rows > u64::from(replacement.rows));
    assert!(!replacement.history_loading);
    // SAFETY: all borrowed views were consumed before freeing this client.
    unsafe {
        phux_client_free(client);
    }
}
