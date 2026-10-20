//! Native snapshots: publication, history import, corruption, and hard bounds.

use super::*;

/// The server-produced transcript `phux-vt-web` decodes; see its tests.
const SERVER_SNAPSHOT: &[u8] =
    include_bytes!("../../../phux-vt-web/tests/fixtures/server-snapshot-v1.bin");

/// History units in order, each with its PAGE rows; FINISH is last with
/// zero rows.
type HistoryUnits = Vec<(Vec<u8>, u32)>;

/// `(cols, rows, READY prefix, history units)`.
fn server_snapshot() -> (u16, u16, Vec<u8>, HistoryUnits) {
    let bytes = SERVER_SNAPSHOT;
    assert_eq!(&bytes[..8], b"PHUXSNP1");
    let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().expect("u32"));
    let cols = u16::from_le_bytes([bytes[8], bytes[9]]);
    let rows = u16::from_le_bytes([bytes[10], bytes[11]]);
    let mut ready = Vec::new();
    let mut history = Vec::new();
    let mut offset = 12;
    while offset < bytes.len() {
        let (kind, unit_rows, len) = (bytes[offset], u32_at(offset + 1), u32_at(offset + 5));
        let unit = bytes[offset + 9..offset + 9 + len as usize].to_vec();
        offset += 9 + len as usize;
        if kind == 0 {
            ready = unit;
        } else {
            history.push((unit, unit_rows));
        }
    }
    (cols, rows, ready, history)
}

fn native_stream() -> phux_protocol::caps::BootstrapStreamProfile {
    phux_protocol::caps::BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
    }
}

#[wasm_bindgen_test]
async fn native_snapshot_publishes_at_ready_and_imports_history_through_finish() {
    let (cols, rows, ready, history) = server_snapshot();
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, cols, rows);
    let terminal_id = ResourceId::local(7);
    let stream_id = stream(7);
    let bootstrap_id = bootstrap(7);
    let cursor = Bytes::from_static(b"history-lease");

    let hello = session.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    assert!(hello.fatal.is_none());
    attach_after_state(&mut session, &hello);
    assert!(
        session
            .on_frame(attached(terminal_id.clone(), cols, rows))
            .fatal
            .is_none()
    );
    let begun = session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        native_stream(),
        cols,
        rows,
        0,
    ));
    assert!(begun.fatal.is_none());
    // Cuts that split the envelope, record headers, and payloads.
    for (chunk_seq, chunk) in (0..).zip(ready.chunks(997)) {
        let outcome = session.on_frame(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: Bytes::copy_from_slice(chunk),
        });
        assert!(outcome.fatal.is_none(), "chunk {chunk_seq}");
        assert!(!outcome.render, "nothing renders before READY");
    }
    let ready_outcome = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: Some(cursor.clone()),
    });
    assert!(ready_outcome.fatal.is_none(), "{:?}", ready_outcome.fatal);
    assert!(matches!(
        decode_one(&ready_outcome.send[0]),
        FrameKind::HistoryRequest { .. }
    ));
    let attach_ready = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(attach_ready.render);
    assert!(session.render_visible());
    let terminal = session.terminal().expect("published native replica");
    assert!(
        terminal.rows_text().iter().any(|row| row == "live-tail"),
        "{:?}",
        terminal.rows_text()
    );
    assert_eq!(session.title(), "phux fixture");
    assert!(
        session.key_frame(key()).is_some(),
        "native replica takes input"
    );

    let last = history.len() - 1;
    for (index, (unit, unit_rows)) in history.iter().enumerate() {
        let finish = index == last;
        if index > 0 {
            // Past the first page, history loads on demand: nothing while
            // the live screen shows, one request once scrolled to the top.
            assert!(session.history_demand().is_empty(), "unit {index}");
            assert!(session.scroll_viewport(-100_000));
            let demand = session.history_demand();
            assert_eq!(demand.len(), 1, "unit {index} is requested");
            assert!(matches!(
                decode_one(&demand[0]),
                FrameKind::HistoryRequest { .. }
            ));
            assert!(
                session.history_demand().is_empty(),
                "an in-flight request is not repeated"
            );
            assert!(session.scroll_to_bottom());
        }
        let outcome = session.on_frame(FrameKind::HistoryPage {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            page_seq: index as u64 + 1,
            rows: *unit_rows,
            cursor: cursor.clone(),
            next_cursor: (!finish).then(|| cursor.clone()),
            payload: Bytes::copy_from_slice(unit),
        });
        assert!(outcome.fatal.is_none(), "unit {index}: {:?}", outcome.fatal);
        assert!(outcome.send.is_empty(), "a loaded page prefetches nothing");
    }
    let terminal = session.terminal().expect("native replica after FINISH");
    let screen = terminal.screen_rows();
    assert_eq!(screen.first().map(String::as_str), Some("fixture-0000"));

    // Live output continues the restored parser state: `ESC [ 3` + `1m`.
    let live = session.on_frame(FrameKind::ResourceOutput {
        terminal_id,
        stream_id,
        bootstrap_id,
        seq: 1,
        bytes: Bytes::from_static(b"1mX"),
    });
    assert!(live.fatal.is_none());
    assert!(live.send.is_empty(), "native raw output sends no FRAME_ACK");
    assert!(
        session
            .terminal()
            .expect("live terminal")
            .rows_text()
            .iter()
            .any(|row| row == "live-tail X")
    );
}

#[wasm_bindgen_test]
async fn corrupt_native_prefix_retires_the_generation_without_publishing() {
    let (cols, rows, mut ready, _) = server_snapshot();
    let middle = ready.len() / 2;
    ready[middle] ^= 0x40;
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, cols, rows);
    let terminal_id = ResourceId::local(8);
    let stream_id = stream(8);
    let bootstrap_id = bootstrap(8);
    let hello = session.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    attach_after_state(&mut session, &hello);
    session.on_frame(attached(terminal_id.clone(), cols, rows));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        native_stream(),
        cols,
        rows,
        0,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from(ready),
    });
    let outcome = session.on_frame(FrameKind::BootstrapReady {
        terminal_id,
        stream_id,
        bootstrap_id,
        history_cursor: Some(Bytes::from_static(b"history-lease")),
    });
    assert!(!outcome.render, "a corrupt prefix never publishes");
    assert!(session.terminal().is_none());
    assert!(session.key_frame(key()).is_none());
}

#[wasm_bindgen_test]
async fn engine_and_negotiated_memory_limits_are_hard_bounds() {
    let vt = Vt::load().await.expect("load engine");
    let terminal = vt.terminal(20, 3);
    terminal
        .set_history_budget(32 * 1024, 37)
        .expect("configure bounded scrollback");
    assert_eq!(
        terminal.history_budget().expect("read bounded scrollback"),
        (32 * 1024, 37)
    );

    let limits = BootstrapLimits::new(32, 64).expect("small valid negotiated bounds");
    let terminal_id = ResourceId::local(71);
    let stream_id = stream(71);
    let bootstrap_id = bootstrap(71);
    let mut session = Connection::new(&vt, 20, 3);
    assert!(
        session
            .on_frame(hello_ok(BootstrapProfile::SynthesizedVtRaw, limits))
            .fatal
            .is_none()
    );
    session.on_frame(attached(terminal_id.clone(), 20, 3));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        20,
        3,
        0,
    ));
    let oversized = session.on_frame(FrameKind::BootstrapChunk {
        terminal_id,
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from(vec![0; 33]),
    });
    assert!(oversized.fatal.is_some());
    assert!(session.is_failed());
}

#[wasm_bindgen_test]
async fn wire_round_trip_rejects_wrong_generation_without_duplicate_apply() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 10, 2);
    let terminal_id = ResourceId::local(5);
    let stream_id = stream(5);
    let bootstrap_id = bootstrap(6);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        0,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"once"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });

    let wrong = FrameKind::ResourceOutput {
        terminal_id,
        stream_id,
        bootstrap_id: bootstrap(999),
        seq: 1,
        bytes: Bytes::from_static(b"must-not-apply"),
    };
    let mut encoded = BytesMut::new();
    wrong.encode(&mut encoded);
    let (decoded, rest) = FrameKind::decode(&encoded).expect("decode output");
    assert!(rest.is_empty());
    let outcome = session.on_frame(decoded);
    assert!(outcome.fatal.is_some());
    assert!(session.is_failed());
    let row: String = session.grid().cells[..10]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(row.starts_with("once"));
    assert!(!row.contains("must-not-apply"));
}
