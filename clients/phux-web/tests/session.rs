//! Protocol-0.7 web session transcripts against the real wasm engine.

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, EngineCodec, EngineFeatureSet,
    ImageProtocolSet,
};
use phux_protocol::ids::{BootstrapId, ClientId, SessionId, StreamId, TerminalId, WindowId};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo};
use phux_vt_web::{NativeDecodeKind, Vt};
use phux_web::Session;
use wasm_bindgen_test::wasm_bindgen_test;

fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).expect("non-zero stream")
}

fn bootstrap(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).expect("non-zero bootstrap")
}

fn key() -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some("a".to_owned()),
        unshifted_codepoint: Some(u32::from(b'a')),
    }
}

fn hello_ok(profile: BootstrapProfile, limits: BootstrapLimits) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: phux_protocol::caps::ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: profile,
        bootstrap_limits: limits,
    }
}

fn attached(terminal_id: TerminalId, cols: u16, rows: u16) -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_panes(vec![TerminalInfo::new(
                terminal_id,
                WindowId::new(1),
                cols,
                rows,
            )]),
        initial_client_id: ClientId::new(1),
    }
}

fn begin(
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: phux_protocol::caps::BootstrapStreamProfile,
    cols: u16,
    rows: u16,
    base_seq: u64,
) -> FrameKind {
    FrameKind::BootstrapBegin {
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        cols,
        rows,
        base_seq,
    }
}

fn native_profile() -> BootstrapProfile {
    BootstrapProfile::NativeState {
        codec: EngineCodec::LibghosttyCheckpointV2,
        features: EngineFeatureSet::required_native(),
    }
}

fn native_stream_profile() -> phux_protocol::caps::BootstrapStreamProfile {
    phux_protocol::caps::BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttyCheckpointV2,
    }
}

fn checkpoint_fixture() -> Vec<u8> {
    include_bytes!("fixtures/history-multipage-v2.bin").to_vec()
}

fn ready_offset(vt: &std::rc::Rc<Vt>, bytes: &[u8], limit: usize) -> usize {
    let capabilities = vt
        .incremental_capabilities()
        .expect("checkpoint-capable wasm");
    let mut decoder = vt
        .native_decoder(limit, limit, capabilities.max_pages)
        .expect("bounded decoder");
    let mut offset = 0;
    loop {
        let end = bytes.len().min(offset + limit.min(4096));
        let event = decoder.push(&bytes[offset..end]).expect("decode fixture");
        assert!(event.consumed > 0, "decoder must advance before READY");
        offset += event.consumed;
        if event.kind == NativeDecodeKind::Ready {
            assert!(event.terminal.is_some());
            return offset;
        }
    }
}

#[wasm_bindgen_test]
async fn raw_transcript_waits_for_dual_and_global_ready_without_ack() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 20, 3);
    let terminal_id = TerminalId::local(1);
    let stream_id = stream(1);
    let bootstrap_id = bootstrap(1);

    let hello = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert!(hello.fatal.is_none());
    assert_eq!(hello.send.len(), 1);
    let (attach, _) = FrameKind::decode(&hello.send[0]).expect("decode attach");
    assert!(matches!(attach, FrameKind::Attach { attach_id: 1, .. }));

    assert!(
        session
            .on_frame(attached(terminal_id.clone(), 20, 3))
            .fatal
            .is_none()
    );
    assert!(
        !session
            .on_frame(begin(
                terminal_id.clone(),
                stream_id,
                bootstrap_id,
                phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
                20,
                3,
                6,
            ))
            .render
    );
    assert!(
        !session
            .on_frame(FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: Bytes::from_static(b"Hi "),
            })
            .render
    );
    assert!(
        !session
            .on_frame(FrameKind::BootstrapReady {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            })
            .render
    );
    assert!(!session.render_visible());
    assert!(session.key_frame(key()).is_none());
    assert!(
        !session.is_failed(),
        "input gate rejection is not protocol-fatal"
    );

    let output = session.on_frame(FrameKind::TerminalOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq: 7,
        bytes: Bytes::from_static(b"phux"),
    });
    assert!(
        !output.render,
        "global ATTACH_READY still gates first damage"
    );
    assert!(output.send.is_empty(), "raw profile never emits FRAME_ACK");

    let ready = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(ready.render);
    assert!(session.render_visible());
    assert!(session.key_frame(key()).is_some());
    let grid = session.grid();
    let row0: String = grid.cells[..usize::from(grid.cols)]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(row0.starts_with("Hi phux"), "row 0 = {row0:?}");
}

#[wasm_bindgen_test]
async fn state_sync_output_acks_the_exact_generation() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 10, 2);
    let terminal_id = TerminalId::local(2);
    let stream_id = stream(2);
    let bootstrap_id = bootstrap(2);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtStateSync,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtStateSync,
        10,
        2,
        40,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"base"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });

    let output = session.on_frame(FrameKind::TerminalOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq: 41,
        bytes: Bytes::from_static(b"+"),
    });
    assert!(output.render);
    assert_eq!(output.send.len(), 1);
    let (ack, rest) = FrameKind::decode(&output.send[0]).expect("decode ack");
    assert!(rest.is_empty());
    assert_eq!(
        ack,
        FrameKind::FrameAck {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq: 41,
        }
    );
}

#[wasm_bindgen_test]
async fn history_cursor_chain_is_echoed_and_bounded_after_ready() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 10, 2);
    let terminal_id = TerminalId::local(3);
    let stream_id = stream(3);
    let bootstrap_id = bootstrap(3);
    let limits = BootstrapLimits::new(1024, 77).expect("valid limits");
    session.on_frame(hello_ok(BootstrapProfile::SynthesizedVtRaw, limits));
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
        payload: Bytes::from_static(b"live"),
    });

    let first = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: Some(Bytes::from_static(b"cursor-1")),
    });
    assert_eq!(first.send.len(), 1);
    let (request, _) = FrameKind::decode(&first.send[0]).expect("decode history request");
    assert_eq!(
        request,
        FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: Bytes::from_static(b"cursor-1"),
            max_bytes: 77,
            max_rows: 1024,
        }
    );

    let second = session.on_frame(FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        page_seq: 1,
        rows: 1,
        cursor: Bytes::from_static(b"cursor-1"),
        next_cursor: Some(Bytes::from_static(b"cursor-2")),
        payload: Bytes::from_static(b"opaque-history-1"),
    });
    assert_eq!(second.send.len(), 1);
    let (request, _) = FrameKind::decode(&second.send[0]).expect("decode next request");
    assert_eq!(
        request,
        FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: Bytes::from_static(b"cursor-2"),
            max_bytes: 77,
            max_rows: 1024,
        }
    );

    let done = session.on_frame(FrameKind::HistoryPage {
        terminal_id,
        stream_id,
        bootstrap_id,
        page_seq: 1,
        rows: 1,
        cursor: Bytes::from_static(b"cursor-2"),
        next_cursor: None,
        payload: Bytes::from_static(b"opaque-history-2"),
    });
    assert!(done.send.is_empty());
    assert!(done.fatal.is_none());
}

#[wasm_bindgen_test]
async fn replacement_stages_without_touching_published_grid() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 10, 2);
    let terminal_id = TerminalId::local(4);
    let stream_id = stream(4);
    let first_bootstrap = bootstrap(4);
    let second_bootstrap = bootstrap(5);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        first_bootstrap,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        0,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: first_bootstrap,
        chunk_seq: 0,
        payload: Bytes::from_static(b"old"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: first_bootstrap,
        history_cursor: None,
    });
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    session.on_frame(attached(terminal_id.clone(), 10, 2));
    assert!(!session.render_visible());

    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        second_bootstrap,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        10,
        2,
        10,
    ));
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: second_bootstrap,
        chunk_seq: 0,
        payload: Bytes::from_static(b"new"),
    });
    let before: String = session.grid().cells[..10]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(before.starts_with("old"));

    let swapped = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id: second_bootstrap,
        history_cursor: None,
    });
    assert!(!swapped.render);
    assert!(!session.render_visible());
    let released = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(released.render);
    assert!(session.render_visible());
    let after: String = session.grid().cells[..10]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(after.starts_with("new"));
}

#[wasm_bindgen_test]
async fn attach_barrier_close_repaints_the_prior_visible_terminal_to_blank() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 10, 2);
    let terminal_id = TerminalId::local(6);
    let stream_id = stream(6);
    let bootstrap_id = bootstrap(7);
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
        payload: Bytes::from_static(b"visible"),
    });
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    assert!(
        session
            .on_frame(FrameKind::AttachReady { attach_id: 1 })
            .render
    );
    assert!(session.render_visible());

    session.on_frame(attached(terminal_id.clone(), 10, 2));
    assert!(!session.render_visible());
    let closed = session.on_frame(FrameKind::TerminalClosed {
        terminal_id,
        exit_status: None,
    });
    assert!(!closed.render);
    assert!(!session.render_visible());

    let released = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(released.render);
    assert!(session.render_visible());
    assert!(
        session
            .grid()
            .cells
            .iter()
            .all(|cell| cell.ch == ' ' || cell.ch == '\0'),
        "released Removed damage must clear the prior visible canvas",
    );
}

#[wasm_bindgen_test]
async fn hello_ok_rejects_version_drift_and_oversized_limits_before_payload() {
    let vt = Vt::load().await.expect("load engine");

    let mut wrong_version = hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    );
    let FrameKind::HelloOk { protocol_patch, .. } = &mut wrong_version else {
        unreachable!();
    };
    *protocol_patch = protocol_patch.saturating_add(1);
    let mut session = Session::new(&vt, 80, 24);
    assert!(session.on_frame(wrong_version).fatal.is_some());

    let mut synthesized_only = Session::new_synthesized_compat(&vt, 80, 24);
    let drift = synthesized_only.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    assert!(drift.fatal.is_some());
    assert!(
        drift.send.is_empty(),
        "drift must fail before ATTACH/payload"
    );
    assert!(synthesized_only.selected_profile().is_none());

    let oversized =
        BootstrapLimits::new(512 * 1024, 2 * 1024 * 1024).expect("within protocol hard limits");
    let mut session = Session::new(&vt, 80, 24);
    assert!(
        session
            .on_frame(hello_ok(BootstrapProfile::SynthesizedVtRaw, oversized))
            .fatal
            .is_some()
    );
}

#[wasm_bindgen_test]
async fn hello_advertises_exact_native_v2_and_explicit_synthesized_fallback() {
    let vt = Vt::load().await.expect("load engine");
    let capabilities = vt
        .incremental_capabilities()
        .expect("published wasm checkpoint capability");
    assert!(capabilities.supports_protocol_07(1024 * 1024));
    let mut identity_drift = capabilities.clone();
    identity_drift.codec_identity.push_str(".drift");
    assert!(!identity_drift.supports_protocol_07(1024 * 1024));
    let mut version_drift = capabilities.clone();
    version_drift.default_encode_version = 3;
    assert!(!version_drift.supports_protocol_07(1024 * 1024));
    let mut cross_build = capabilities.clone();
    cross_build.build_identity = "diagnostic-only-other-build".to_owned();
    assert!(
        cross_build.supports_protocol_07(1024 * 1024),
        "v2 is the immutable cross-build grammar"
    );

    let session = Session::new(&vt, 80, 24);
    let frames = session.handshake();
    let (hello, rest) = FrameKind::decode(&frames[0]).expect("decode hello");
    assert!(rest.is_empty());
    let FrameKind::Hello { client_caps, .. } = hello else {
        panic!("first handshake frame must be HELLO");
    };
    assert_eq!(client_caps.image_protocols, ImageProtocolSet::new());
    assert!(
        client_caps
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::NativeState)
    );
    assert!(
        client_caps
            .bootstrap
            .native_codecs
            .contains(EngineCodec::LibghosttyCheckpointV2)
    );
    assert_eq!(
        client_caps.bootstrap.native_features,
        EngineFeatureSet::required_native()
    );
    assert!(
        client_caps
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw)
    );
    assert!(
        client_caps
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtStateSync)
    );

    let compatibility = Session::new_synthesized_compat(&vt, 80, 24);
    assert!(
        !compatibility
            .advertised_capabilities()
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::NativeState)
    );
    assert!(
        compatibility
            .advertised_capabilities()
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw)
    );
}

#[wasm_bindgen_test]
async fn native_checkpoint_survives_arbitrary_frames_and_progressive_history() {
    let vt = Vt::load().await.expect("load engine");
    let bytes = checkpoint_fixture();
    let limit = BootstrapLimits::default().max_history_page_bytes() as usize;
    let ready_at = ready_offset(&vt, &bytes, limit);
    assert!(
        ready_at < bytes.len(),
        "fixture must include progressive history"
    );

    let terminal_id = TerminalId::local(70);
    let stream_id = stream(70);
    let bootstrap_id = bootstrap(70);
    let mut session = Session::new(&vt, 80, 24);
    let negotiated = session.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    assert!(negotiated.fatal.is_none());
    assert_eq!(session.selected_profile(), Some(native_profile()));
    assert!(
        session
            .on_frame(attached(terminal_id.clone(), 80, 24))
            .fatal
            .is_none()
    );
    assert!(
        session
            .on_frame(begin(
                terminal_id.clone(),
                stream_id,
                bootstrap_id,
                native_stream_profile(),
                80,
                24,
                0,
            ))
            .fatal
            .is_none()
    );

    let fragments = [1_usize, 2, 7, 31, 4096, 65_536, 262_144];
    let mut offset = 0;
    let mut fragment = 0;
    let mut chunk_seq = 0;
    while offset < ready_at {
        let end = ready_at.min(offset + fragments[fragment % fragments.len()]);
        let outcome = session.on_frame(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: Bytes::copy_from_slice(&bytes[offset..end]),
        });
        assert!(outcome.fatal.is_none());
        offset = end;
        fragment += 1;
        chunk_seq += 1;
    }
    assert!(
        !session.render_visible(),
        "protocol READY still gates publication"
    );

    let initial_cursor = Bytes::from_static(b"native-history-0");
    let ready = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: Some(initial_cursor.clone()),
    });
    assert!(ready.fatal.is_none());
    assert_eq!(ready.send.len(), 1);
    let published = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(published.fatal.is_none());
    assert!(published.render);
    assert!(session.render_visible());
    let cache_limits = phux_client_core::history::HistoryCacheConfig::default();
    assert_eq!(
        session.active_history_budget(),
        Some((cache_limits.max_bytes, cache_limits.max_materialized_rows))
    );

    offset = ready_at;
    fragment = 0;
    let mut cursor = initial_cursor;
    while offset < bytes.len() {
        let end = bytes
            .len()
            .min(offset + fragments[(fragment * 7 + 3) % fragments.len()]);
        let next_cursor =
            (end < bytes.len()).then(|| Bytes::from(format!("native-history-{}", fragment + 1)));
        let outcome = session.on_frame(FrameKind::HistoryPage {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            page_seq: 1,
            rows: 1,
            cursor: cursor.clone(),
            next_cursor: next_cursor.clone(),
            payload: Bytes::copy_from_slice(&bytes[offset..end]),
        });
        assert!(outcome.fatal.is_none());
        if let Some(next) = next_cursor {
            assert_eq!(outcome.send.len(), 1);
            cursor = next;
        } else {
            assert!(outcome.send.is_empty());
        }
        offset = end;
        fragment += 1;
    }

    let live = session.on_frame(FrameKind::TerminalOutput {
        terminal_id,
        stream_id,
        bootstrap_id,
        seq: 1,
        bytes: Bytes::from_static(b"\r\nnative-live"),
    });
    assert!(live.fatal.is_none());
    assert!(live.render);
    assert!(
        live.send.is_empty(),
        "NativeState raw output is never acked"
    );
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
    let terminal_id = TerminalId::local(71);
    let stream_id = stream(71);
    let bootstrap_id = bootstrap(71);
    let mut session = Session::new(&vt, 20, 3);
    assert!(
        session
            .on_frame(hello_ok(native_profile(), limits))
            .fatal
            .is_none()
    );
    session.on_frame(attached(terminal_id.clone(), 20, 3));
    session.on_frame(begin(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        native_stream_profile(),
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
    let mut session = Session::new(&vt, 10, 2);
    let terminal_id = TerminalId::local(5);
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

    let wrong = FrameKind::TerminalOutput {
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
