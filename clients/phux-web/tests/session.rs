//! Protocol-0.7 web session transcripts against the real wasm engine.

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ResourceKind;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, EngineCodec, EngineFeatureSet,
    ImageProtocolSet, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{
    BootstrapId, ClientId, ResourceId, SatelliteHost, SessionId, StreamId, WindowId,
};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::{FrameKind, PathKind, PathResults, PathRow, PathStatus};
use phux_protocol::wire::info::{AgentFacet, ResourceInfo, SessionSnapshot};
use phux_vt_web::Vt;
use phux_web::Session;
use wasm_bindgen_test::wasm_bindgen_test;

fn stream(raw: u64) -> StreamId {
    StreamId::new(raw).expect("non-zero stream")
}

fn path_hello(supported: bool) -> FrameKind {
    let mut frame = hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    );
    if let FrameKind::HelloOk { server_caps, .. } = &mut frame {
        *server_caps = if supported {
            ServerCapabilities::new()
                .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery]))
        } else {
            ServerCapabilities::new()
        };
    }
    frame
}

fn path_reply(request_id: u32, path: &str) -> FrameKind {
    FrameKind::PathResults {
        request_id,
        result: Ok(PathResults {
            root: "/work".to_owned(),
            parent: Some("/".to_owned()),
            rows: vec![PathRow {
                path: path.to_owned(),
                kind: PathKind::File,
            }],
            status: PathStatus::Complete,
        }),
    }
}

async fn path_session(supported: bool, second_pane: bool) -> Session {
    let panes = if second_pane {
        vec![ResourceId::local(101), ResourceId::local(102)]
    } else {
        vec![ResourceId::local(101)]
    };
    path_session_with_panes(supported, panes).await
}

async fn path_session_with_panes(supported: bool, panes: Vec<ResourceId>) -> Session {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 20, 3);
    session.on_frame(path_hello(supported));
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), panes[0].clone())
        .with_resources(
            panes
                .iter()
                .cloned()
                .map(|id| ResourceInfo::new(id, WindowId::new(1), 20, 3))
                .collect(),
        );
    session.on_frame(FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    });
    for (i, id) in panes.iter().enumerate() {
        let stream_id = stream(i as u64 + 1);
        let bootstrap_id = bootstrap(i as u64 + 1);
        session.on_frame(begin(
            id.clone(),
            stream_id,
            bootstrap_id,
            phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
            20,
            3,
            0,
        ));
        session.on_frame(FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
    }
    session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    session
}

#[wasm_bindgen_test]
async fn satellite_pane_queries_its_own_host_not_the_hub() {
    let mut session = path_session_with_panes(
        true,
        vec![ResourceId::satellite(SatelliteHost::new("build"), 101)],
    )
    .await;
    let request = session.path_query_frame("~", "src", true).expect("query");
    let (frame, _) = FrameKind::decode(&request).expect("decode");
    let FrameKind::PathQuery { host, .. } = frame else {
        panic!("not a query")
    };
    assert_eq!(host, Some(SatelliteHost::new("build")));
}

#[wasm_bindgen_test]
async fn path_query_negotiation_and_stale_replies() {
    let mut old = path_session(false, false).await;
    assert!(!old.path_query_supported());
    assert!(old.path_query_frame("~", "src", true).is_none());
    let mut session = path_session(true, false).await;
    let first = session
        .path_query_frame("~", "src", true)
        .expect("negotiated query");
    let (first, _) = FrameKind::decode(&first).unwrap();
    let FrameKind::PathQuery {
        request_id: old_id,
        host: None,
        recursive: true,
        ..
    } = first
    else {
        panic!("wrong query")
    };
    let second = session.path_query_frame("/work", "", false).unwrap();
    let (second, _) = FrameKind::decode(&second).unwrap();
    let FrameKind::PathQuery {
        request_id: new_id,
        host: None,
        recursive: false,
        ..
    } = second
    else {
        panic!("wrong browse")
    };
    assert_ne!(old_id, new_id);
    session.on_frame(path_reply(old_id, "/work/stale"));
    assert!(session.path_pending());
    assert!(session.path_results().is_none());
    session.on_frame(path_reply(new_id, "/work/new"));
    assert_eq!(session.path_results().unwrap().rows[0].path, "/work/new");
    session.cancel_path_query();
    session.on_frame(path_reply(new_id, "/work/new"));
    assert!(session.path_results().is_none());
}

#[wasm_bindgen_test]
async fn path_selection_keeps_pane_and_emits_only_editable_paste() {
    let mut session = path_session(true, false).await;
    let query = session.path_query_frame("/work", "a", true).unwrap();
    let (FrameKind::PathQuery { request_id, .. }, _) = FrameKind::decode(&query).unwrap() else {
        panic!("query")
    };
    session.on_frame(path_reply(request_id, "/work/a b'$(echo nope).txt"));
    let paste = session.paste_path_row(0).expect("eligible paste");
    let (frame, rest) = FrameKind::decode(&paste).unwrap();
    assert!(rest.is_empty());
    let FrameKind::InputPaste { terminal_id, event } = frame else {
        panic!("paste only, never Enter")
    };
    assert_eq!(terminal_id, ResourceId::local(101));
    assert_eq!(event.data, b"'/work/a b'\\''$(echo nope).txt'");
    assert_eq!(
        event.trust,
        phux_protocol::input::paste::PasteTrust::Untrusted
    );
    assert!(
        session.key_frame(key()).is_some(),
        "selection retains input lease"
    );
    assert!(
        session.paste_path_row(0).is_none(),
        "selection cannot replay"
    );
    let query = session.path_query_frame("/work", "a", true).unwrap();
    let (FrameKind::PathQuery { request_id, .. }, _) = FrameKind::decode(&query).unwrap() else {
        panic!("query")
    };
    session.on_frame(path_reply(request_id, "/work/unsafe\ncommand"));
    assert!(
        session.paste_path_row(0).is_none(),
        "control characters cannot become terminal input"
    );
}

#[wasm_bindgen_test]
async fn focus_switch_discards_query_and_cannot_paste_into_next_pane() {
    let mut session = path_session(true, true).await;
    let query = session.path_query_frame("/work", "a", true).unwrap();
    let (FrameKind::PathQuery { request_id, .. }, _) = FrameKind::decode(&query).unwrap() else {
        panic!("query")
    };
    session.on_frame(FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(101),
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
        signal: None,
    });
    session.on_frame(path_reply(request_id, "/work/a"));
    assert!(session.path_results().is_none());
    assert!(session.paste_path_row(0).is_none());
    assert!(
        session.key_frame(key()).is_some(),
        "next pane remains usable"
    );
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

#[wasm_bindgen_test]
async fn hello_ok_without_aggregate_ready_is_not_connection_ready() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    let outcome = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert!(outcome.fatal.is_none());
    assert_eq!(outcome.send.len(), 2, "HELLO_OK subscribes before attach");
    assert!(matches!(
        decode_one(&outcome.send[0]),
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None
        }
    ));
    assert!(!session.is_attach_ready(), "HELLO_OK alone is not usable");
}

fn attached(terminal_id: ResourceId, cols: u16, rows: u16) -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_resources(vec![ResourceInfo::new(
                terminal_id,
                WindowId::new(1),
                cols,
                rows,
            )]),
        initial_client_id: ClientId::new(1),
    }
}

fn begin(
    terminal_id: ResourceId,
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

#[wasm_bindgen_test]
async fn raw_transcript_waits_for_dual_and_global_ready_without_ack() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 20, 3);
    let terminal_id = ResourceId::local(1);
    let stream_id = stream(1);
    let bootstrap_id = bootstrap(1);

    let hello = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert!(hello.fatal.is_none());
    assert_eq!(hello.send.len(), 2);
    let (attach, _) = FrameKind::decode(&hello.send[1]).expect("decode attach");
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

    let output = session.on_frame(FrameKind::ResourceOutput {
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
    assert!(session.is_attach_ready());
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
    let terminal_id = ResourceId::local(2);
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

    let output = session.on_frame(FrameKind::ResourceOutput {
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
    let terminal_id = ResourceId::local(3);
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
    let terminal_id = ResourceId::local(4);
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
    let terminal_id = ResourceId::local(6);
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
    let closed = session.on_frame(FrameKind::ResourceClosed {
        terminal_id,
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
        signal: None,
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
async fn hello_advertises_synthesized_until_wasm_speaks_official_snapshot() {
    let vt = Vt::load().await.expect("load engine");
    // Vendored ghostty-vt.wasm still speaks the fork incremental ABI.
    // Advertising native would select GHOSTSNP the WASM engine cannot decode.
    assert!(
        vt.incremental_capabilities().is_none(),
        "wasm must not publish native checkpoint until rebuilt against official GHOSTSNP"
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
        !client_caps
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::NativeState)
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
async fn session_rejects_native_hello_ok_until_wasm_speaks_official_snapshot() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    let negotiated = session.on_frame(hello_ok(native_profile(), BootstrapLimits::default()));
    assert!(negotiated.fatal.is_some());
    assert!(
        negotiated.send.is_empty(),
        "unadvertised native must fail before ATTACH/payload"
    );
    assert!(session.selected_profile().is_none());
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
    let mut session = Session::new(&vt, 20, 3);
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
    let mut session = Session::new(&vt, 10, 2);
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

/// A focused session holding one terminal pane and one `AgentSession`
/// resource bound to it, the way a RESOURCE_KINDS server reports them.
fn attached_with_agent(
    terminal_id: ResourceId,
    agent_id: ResourceId,
    cols: u16,
    rows: u16,
) -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_resources(vec![
                ResourceInfo::new(terminal_id.clone(), WindowId::new(1), cols, rows),
                ResourceInfo::new(agent_id, WindowId::new(0), 0, 0)
                    .with_kind(ResourceKind::AgentSession)
                    .with_parent(Some(terminal_id))
                    .with_agent(Some(AgentFacet::new("claude", "idle"))),
            ]),
        initial_client_id: ClientId::new(1),
    }
}

fn agent_record(seq: u64, kind: &str, data: &str) -> Bytes {
    Bytes::from(format!(
        "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
        seq * 10
    ))
}

fn badge_summary(session: &Session) -> Vec<(String, String)> {
    session
        .agent_badges()
        .into_iter()
        .map(|badge| (badge.provider, badge.state))
        .collect()
}

#[wasm_bindgen_test]
async fn agent_sessions_become_badges_and_never_panes() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 10, 2);
    let terminal_id = ResourceId::local(8);
    let agent_id = ResourceId::local(9);
    let stream_id = stream(8);
    let bootstrap_id = bootstrap(8);
    let agent_stream = stream(9);
    let agent_bootstrap = bootstrap(9);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));

    // The snapshot facet seeds the badge; the agent is not a pane.
    let attached = session.on_frame(attached_with_agent(
        terminal_id.clone(),
        agent_id.clone(),
        10,
        2,
    ));
    assert!(attached.fatal.is_none());
    assert!(attached.badges);
    agent_attach(&attached, 9);
    assert!(session.on_frame(spawned_agent(9, 8)).send.is_empty());
    assert_eq!(
        badge_summary(&session),
        vec![("claude".to_owned(), "idle".to_owned())]
    );

    // The terminal alone completes the attach; the agent never bootstrapped.
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
        payload: Bytes::from_static(b"pane"),
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
    let key_frame = session.key_frame(key()).expect("terminal accepts input");
    let (decoded, _) = FrameKind::decode(&key_frame).expect("decode key frame");
    assert!(
        matches!(decoded, FrameKind::InputKey { terminal_id: ref id, .. } if *id == terminal_id),
        "input goes to the terminal, never the agent session"
    );

    // The agent stream publishes its retained backlog, then live records,
    // and each update refreshes the badge.
    let began = session.on_frame(begin(
        agent_id.clone(),
        agent_stream,
        agent_bootstrap,
        phux_protocol::caps::BootstrapStreamProfile::AgentEventsJsonlV1,
        0,
        0,
        1,
    ));
    assert!(began.fatal.is_none());
    assert!(!began.render, "an agent stream never paints");
    session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: agent_id.clone(),
        stream_id: agent_stream,
        bootstrap_id: agent_bootstrap,
        chunk_seq: 0,
        payload: agent_record(1, "prompt", r#"{"length":4}"#),
    });
    let ready = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: agent_id.clone(),
        stream_id: agent_stream,
        bootstrap_id: agent_bootstrap,
        history_cursor: None,
    });
    assert!(ready.badges);
    assert!(!ready.render);
    assert_eq!(
        badge_summary(&session),
        vec![("claude".to_owned(), "working".to_owned())]
    );
    let live = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: agent_id.clone(),
        stream_id: agent_stream,
        bootstrap_id: agent_bootstrap,
        seq: 2,
        bytes: agent_record(2, "ask", r#"{"question":"?"}"#),
    });
    assert!(live.badges);
    assert_eq!(
        badge_summary(&session),
        vec![("claude".to_owned(), "blocked".to_owned())]
    );

    // A malformed record retires the agent generation but never the terminal.
    let broken = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: agent_id.clone(),
        stream_id: agent_stream,
        bootstrap_id: agent_bootstrap,
        seq: 3,
        bytes: Bytes::from_static(b"{not json\n"),
    });
    assert!(broken.fatal.is_none());
    assert!(!session.is_failed());
    let output = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq: 1,
        bytes: Bytes::from_static(b"!"),
    });
    assert!(output.render);
    let grid = session.grid();
    let row0: String = grid.cells[..usize::from(grid.cols)]
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(row0.starts_with("pane!"), "row 0 = {row0:?}");

    // Closing the agent retracts its badge and leaves the pane alone.
    let closed = session.on_frame(FrameKind::ResourceClosed {
        terminal_id: agent_id,
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
        signal: None,
    });
    assert!(closed.badges);
    assert!(closed.fatal.is_none());
    assert!(session.agent_badges().is_empty());
    assert!(session.render_visible());
    assert!(session.key_frame(key()).is_some());
}

fn spawned_agent(id: u32, parent: u32) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(id)),
        event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
            kind: ResourceKind::AgentSession,
            parent: Some(ResourceId::local(parent)),
        },
        stamp: None,
    }
}

fn closed_agent(id: u32) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(id)),
        event: phux_protocol::wire::frame::AgentEvent::ResourceClosed { exit_status: None },
        stamp: None,
    }
}

fn agent_requests(outcome: &phux_web::Outcome) -> Vec<ResourceId> {
    outcome
        .send
        .iter()
        .filter_map(|bytes| match decode_one(bytes) {
            FrameKind::Command {
                command: phux_protocol::wire::frame::Command::AttachResource { terminal_id, .. },
                ..
            } => Some(terminal_id),
            _ => None,
        })
        .collect()
}

#[wasm_bindgen_test]
async fn pre_snapshot_agent_announcements_survive_parent_admission() {
    let vt = Vt::load().await.expect("engine");
    let mut session = Session::new(&vt, 20, 3);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    for (id, parent) in [(201, 101), (203, 101), (301, 999)] {
        assert!(session.on_frame(spawned_agent(id, parent)).send.is_empty());
    }
    // Both closure forms can overtake the captured snapshot, including a
    // child already in that snapshot whose spawn event preceded subscription.
    session.on_frame(closed_agent(202));
    session.on_frame(FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(203),
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Exited,
        signal: None,
    });
    let admitted = session.on_frame(attached_with_agent(
        ResourceId::local(101),
        ResourceId::local(202),
        20,
        3,
    ));
    assert!(admitted.fatal.is_none());
    assert_eq!(agent_requests(&admitted), vec![ResourceId::local(201)]);
    assert_eq!(session.agent_badges().len(), 1);
    for (id, parent) in [(201, 101), (202, 101), (203, 101), (301, 999)] {
        assert!(session.on_frame(spawned_agent(id, parent)).send.is_empty());
    }
    assert!(!session.is_failed());
}

#[wasm_bindgen_test]
async fn pending_split_agent_announcements_wait_for_parent_commit() {
    for ack_first in [false, true] {
        let mut session = path_session(false, false).await;
        let spawn = requested_spawn(&mut session, "vertical");
        // Child events may even beat the reply naming the split's new parent.
        session.on_frame(spawned_agent(301, 202));
        session.on_frame(spawned_agent(302, 202));
        session.on_frame(spawned_agent(401, 999));
        session.on_frame(closed_agent(302));
        let attach = announce_split(&mut session, spawn, ResourceId::local(202));
        let ack = FrameKind::CommandResult {
            request_id: attach,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        };
        let committed = if ack_first {
            assert!(agent_requests(&session.on_frame(ack)).is_empty());
            assert!(session.agent_badges().is_empty());
            bootstrap_split(&mut session, ResourceId::local(202), 2, b"split")
        } else {
            assert!(
                agent_requests(&bootstrap_split(
                    &mut session,
                    ResourceId::local(202),
                    2,
                    b"split"
                ))
                .is_empty()
            );
            assert!(session.agent_badges().is_empty());
            session.on_frame(ack)
        };
        assert!(committed.fatal.is_none());
        assert_eq!(agent_requests(&committed), vec![ResourceId::local(301)]);
        assert_eq!(session.agent_badges().len(), 1);
        assert!(session.on_frame(spawned_agent(301, 202)).send.is_empty());
        assert!(session.on_frame(spawned_agent(302, 202)).send.is_empty());
        assert!(session.on_frame(spawned_agent(401, 999)).send.is_empty());
        assert!(session.key_frame(key()).is_some());
    }
}

#[wasm_bindgen_test]
async fn failed_split_discards_unadmitted_agent_announcements() {
    let mut session = path_session(false, false).await;
    let spawn = requested_spawn(&mut session, "vertical");
    session.on_frame(spawned_agent(301, 202));
    session.on_frame(FrameKind::CommandResult {
        request_id: spawn,
        result: phux_protocol::wire::frame::CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
            message: "split refused".into(),
        },
    });
    let spawn = requested_spawn(&mut session, "vertical");
    let attach = announce_split(&mut session, spawn, ResourceId::local(202));
    bootstrap_split(&mut session, ResourceId::local(202), 2, b"new split");
    let committed = session.on_frame(FrameKind::CommandResult {
        request_id: attach,
        result: phux_protocol::wire::frame::CommandResult::Ok,
    });
    assert!(agent_requests(&committed).is_empty());
    assert!(session.agent_badges().is_empty());
}

#[wasm_bindgen_test]
async fn closing_split_parent_discards_its_pending_agent_discovery() {
    let mut session = path_session(false, false).await;
    let spawn = requested_spawn(&mut session, "vertical");
    announce_split(&mut session, spawn, ResourceId::local(202));
    session.on_frame(spawned_agent(301, 202));
    let closed = session.on_frame(FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(202),
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Exited,
        signal: None,
    });
    assert!(closed.fatal.is_none());
    assert!(agent_requests(&closed).is_empty());
    assert!(!session.pane_pending());
    assert!(session.on_frame(spawned_agent(301, 202)).send.is_empty());
    assert!(session.agent_badges().is_empty());
    assert!(session.key_frame(key()).is_some());
}

#[wasm_bindgen_test]
async fn agent_discovery_admission_is_bounded_and_duplicate_safe() {
    let vt = Vt::load().await.expect("engine");
    let mut session = Session::new(&vt, 20, 3);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    for _ in 0..300 {
        assert!(session.on_frame(spawned_agent(2000, 999)).fatal.is_none());
    }
    for id in 2001..2256 {
        assert!(session.on_frame(spawned_agent(id, 999)).fatal.is_none());
    }
    let overflow = session.on_frame(spawned_agent(2256, 999));
    assert!(
        overflow
            .fatal
            .as_deref()
            .unwrap()
            .contains("discovery admission buffer exhausted")
    );
    assert!(session.is_failed());
    assert!(session.agent_badges().is_empty());
}

fn agent_attach(outcome: &phux_web::Outcome, expected: u32) -> u32 {
    assert!(outcome.fatal.is_none());
    assert!(outcome.badges);
    assert_eq!(outcome.send.len(), 1);
    match decode_one(&outcome.send[0]) {
        FrameKind::Command {
            request_id,
            command: phux_protocol::wire::frame::Command::AttachResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, ResourceId::local(expected));
            request_id
        }
        other => panic!("expected child attach, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn live_agent_discovery_attaches_once_and_closure_never_resurrects() {
    let mut session = path_session(false, false).await;
    let attached = session.on_frame(spawned_agent(201, 101));
    let request_id = agent_attach(&attached, 201);
    assert_eq!(session.agent_badges().len(), 1);
    assert!(session.on_frame(spawned_agent(201, 101)).send.is_empty());
    assert!(session.on_frame(spawned_agent(202, 999)).send.is_empty());
    assert!(session.on_frame(spawned_agent(203, 201)).send.is_empty());

    let closed = session.on_frame(FrameKind::Event {
        terminal: Some(ResourceId::local(201)),
        event: phux_protocol::wire::frame::AgentEvent::ResourceClosed { exit_status: None },
        stamp: None,
    });
    assert!(closed.badges);
    assert!(session.agent_badges().is_empty());
    // A queued bootstrap and reply after closure are harmless to the parent.
    let late = session.on_frame(begin(
        ResourceId::local(201),
        stream(201),
        bootstrap(201),
        phux_protocol::caps::BootstrapStreamProfile::AgentEventsJsonlV1,
        0,
        0,
        0,
    ));
    assert!(late.fatal.is_none());
    let reply = session.on_frame(FrameKind::CommandResult {
        request_id,
        result: phux_protocol::wire::frame::CommandResult::Ok,
    });
    assert!(reply.fatal.is_none());
    assert!(session.on_frame(spawned_agent(201, 101)).send.is_empty());
    assert!(session.agent_badges().is_empty());
    assert!(session.key_frame(key()).is_some());
    assert!(!session.is_failed());
}

#[wasm_bindgen_test]
async fn refused_agent_attach_retracts_badge_without_failing_terminal() {
    let mut session = path_session(false, false).await;
    let request_id = agent_attach(&session.on_frame(spawned_agent(201, 101)), 201);
    let refused = session.on_frame(FrameKind::CommandResult {
        request_id,
        result: phux_protocol::wire::frame::CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
            message: "child already closed".to_owned(),
        },
    });
    assert!(refused.fatal.is_none());
    assert!(session.agent_badges().is_empty());
    assert!(session.key_frame(key()).is_some());
    assert!(!session.pane_pending());
}

fn decode_one(frame: &[u8]) -> FrameKind {
    let (decoded, rest) = FrameKind::decode(frame).expect("decodable client frame");
    assert!(rest.is_empty());
    decoded
}

/// The viewport's size in pixels, as the ATTACH or VIEWPORT_RESIZE in
/// `frame` reports it.
fn reported_pixels(frame: &[u8]) -> (Option<u16>, Option<u16>) {
    match decode_one(frame) {
        FrameKind::Attach { viewport, .. } | FrameKind::ViewportResize { viewport } => {
            (viewport.pixel_w, viewport.pixel_h)
        }
        other => panic!("not a viewport report: {other:?}"),
    }
}

/// The server sizes its mouse encoder's cells from the viewport's pixel
/// size (`pixel / cells`), so the client reports the cell grid it draws and
/// sends pointer positions in: without it, the server divides by whatever
/// cell size another client reported, or its 8x16 default.
#[wasm_bindgen_test]
async fn the_viewport_reports_the_cell_grid_in_pixels() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    session.set_cell_size(9, 18);
    let attach = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    assert_eq!(
        reported_pixels(&attach.send[1]),
        (Some(80 * 9), Some(24 * 18))
    );
    let resize = session.resize_frame(100, 30).expect("VIEWPORT_RESIZE");
    assert_eq!(reported_pixels(&resize), (Some(100 * 9), Some(30 * 18)));

    // A grid too wide for the wire's u16 pixels reports no pixel size
    // rather than a wrapped one.
    let huge = session.resize_frame(8_000, 30).expect("VIEWPORT_RESIZE");
    assert_eq!(reported_pixels(&huge), (None, None));
}

#[wasm_bindgen_test]
async fn resize_rides_the_attach_before_hello_ok_and_viewport_resize_after() {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 80, 24);
    assert!(
        session.resize_frame(100, 30).is_none(),
        "no stateful frame before HELLO_OK"
    );
    let attach = session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    let FrameKind::Attach { viewport, .. } = decode_one(&attach.send[1]) else {
        panic!("HELLO_OK is answered by ATTACH");
    };
    assert_eq!((viewport.cols, viewport.rows), (100, 30));

    let frame = session.resize_frame(120, 40).expect("VIEWPORT_RESIZE");
    let FrameKind::ViewportResize { viewport } = decode_one(&frame) else {
        panic!("resize encodes VIEWPORT_RESIZE");
    };
    assert_eq!((viewport.cols, viewport.rows), (120, 40));
    assert!(
        session.resize_frame(120, 40).is_none(),
        "an unchanged size sends nothing"
    );
    let clamped = session.resize_frame(0, 0).expect("clamped resize");
    let FrameKind::ViewportResize { viewport } = decode_one(&clamped) else {
        panic!("resize encodes VIEWPORT_RESIZE");
    };
    assert_eq!((viewport.cols, viewport.rows), (1, 1));

    session.fail_protocol("closed");
    assert!(
        session.resize_frame(90, 20).is_none(),
        "a failed session is inert"
    );
}

#[wasm_bindgen_test]
async fn live_bells_ring_bootstrap_bells_do_not_and_mouse_input_is_structured() {
    use phux_protocol::input::InputEvent;
    use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};

    let vt = Vt::load().await.expect("load engine");
    let mut session = Session::new(&vt, 20, 3);
    let terminal_id = ResourceId::local(1);
    let (stream_id, bootstrap_id) = (stream(1), bootstrap(1));
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
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
    let replayed = session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(b"old\x07"),
    });
    assert!(!replayed.bell, "replayed state never rings");
    session.on_frame(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    let ready = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert!(!ready.bell, "a bootstrap bell does not ring at publication");

    let output = |seq, bytes: &'static [u8]| FrameKind::ResourceOutput {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        seq,
        bytes: Bytes::from_static(bytes),
    };
    assert!(
        !session.on_frame(output(1, b"\x1b]0;title\x07")).bell,
        "an OSC terminator is not a bell"
    );
    assert!(
        session.on_frame(output(2, b"ding\x07")).bell,
        "a live BEL rings"
    );
    assert!(!session.on_frame(output(3, b"more")).bell);

    assert!(!session.terminal().expect("published").mouse_tracking());
    session.on_frame(output(4, b"\x1b[?1000h"));
    assert!(session.terminal().expect("published").mouse_tracking());

    let press = MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: 32.0,
        y: 16.0,
    };
    let frame = session
        .input_frame(InputEvent::Mouse(press))
        .expect("mouse input is eligible");
    assert!(matches!(
        decode_one(&frame),
        FrameKind::InputMouse { event, .. } if event == press
    ));
}

fn requested_spawn(session: &mut Session, axis: &str) -> u32 {
    match decode_one(&session.split_pane_frame(axis).expect("split request")) {
        FrameKind::SpawnResource { request_id, .. } => request_id,
        other => panic!("expected spawn, got {other:?}"),
    }
}

fn announce_split(session: &mut Session, request_id: u32, id: ResourceId) -> u32 {
    let outcome = session.on_frame(FrameKind::ResourceSpawned {
        request_id,
        result: phux_protocol::wire::frame::SpawnResult::Ok(id.clone()),
    });
    match decode_one(&outcome.send[0]) {
        FrameKind::Command {
            request_id,
            command: phux_protocol::wire::frame::Command::AttachResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, id);
            request_id
        }
        other => panic!("expected attach, got {other:?}"),
    }
}

fn bootstrap_split(
    session: &mut Session,
    id: ResourceId,
    serial: u64,
    text: &'static [u8],
) -> phux_web::Outcome {
    let stream_id = stream(serial);
    let bootstrap_id = bootstrap(serial);
    let began = session.on_frame(begin(
        id.clone(),
        stream_id,
        bootstrap_id,
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        9,
        3,
        0,
    ));
    assert!(began.fatal.is_none());
    let chunk = session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: id.clone(),
        stream_id,
        bootstrap_id,
        chunk_seq: 0,
        payload: Bytes::from_static(text),
    });
    assert!(chunk.fatal.is_none());
    let ready = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: id,
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    assert!(ready.fatal.is_none());
    ready
}

#[wasm_bindgen_test]
async fn split_waits_for_bootstrap_and_ack_then_isolates_content_and_input() {
    let mut session = path_session(false, false).await;
    let first = ResourceId::local(101);
    let second = ResourceId::local(202);
    session.on_frame(FrameKind::ResourceOutput {
        terminal_id: first.clone(),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        seq: 1,
        bytes: Bytes::from_static(b"original"),
    });
    let spawn = requested_spawn(&mut session, "vertical");
    let attach = announce_split(&mut session, spawn, second.clone());
    assert_eq!(session.pane_rects().len(), 1);
    bootstrap_split(&mut session, second.clone(), 2, b"second");
    assert!(session.pane_pending());
    assert_eq!(session.focused_pane(), Some(first.clone()));
    let committed = session.on_frame(FrameKind::CommandResult {
        request_id: attach,
        result: phux_protocol::wire::frame::CommandResult::Ok,
    });
    assert!(committed.panes);
    assert!(!session.pane_pending());
    assert_eq!(session.pane_rects().len(), 2);
    assert_eq!(session.focused_pane(), Some(second.clone()));
    let rects = session.pane_rects();
    assert_eq!((rects[0].1.x, rects[0].1.cols), (0, 9));
    assert_eq!((rects[1].1.x, rects[1].1.cols), (10, 10));
    let first_text: String = session
        .pane_terminal(&first)
        .unwrap()
        .grid()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect();
    let second_text: String = session
        .pane_terminal(&second)
        .unwrap()
        .grid()
        .cells
        .iter()
        .map(|cell| cell.ch)
        .collect();
    assert!(first_text.starts_with("original"));
    assert!(second_text.starts_with("second"));
    assert!(matches!(decode_one(&session.key_frame(key()).unwrap()),
        FrameKind::InputKey { terminal_id, .. } if terminal_id == second));
    session.focus_next_pane().unwrap();
    assert!(matches!(decode_one(&session.key_frame(key()).unwrap()),
        FrameKind::InputKey { terminal_id, .. } if terminal_id == first));
    let output = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: second.clone(),
        stream_id: stream(2),
        bootstrap_id: bootstrap(2),
        seq: 1,
        bytes: Bytes::from_static(b"!"),
    });
    assert!(output.render, "unfocused output still repaints");
    assert!(
        session
            .pane_terminal(&second)
            .unwrap()
            .grid()
            .cells
            .iter()
            .any(|cell| cell.ch == '!')
    );
    assert!(
        !session
            .pane_terminal(&first)
            .unwrap()
            .grid()
            .cells
            .iter()
            .any(|cell| cell.ch == '!')
    );
}

#[wasm_bindgen_test]
async fn refused_split_and_attach_leave_the_original_usable() {
    use phux_protocol::wire::frame::{Command, CommandResult, SpawnError, SpawnResult};
    let mut session = path_session(false, false).await;
    let original = ResourceId::local(101);
    let request_id = requested_spawn(&mut session, "vertical");
    let refused = session.on_frame(FrameKind::ResourceSpawned {
        request_id,
        result: SpawnResult::Err(SpawnError::SpawnFailed("quota reached".to_owned())),
    });
    assert!(refused.panes);
    assert!(session.pane_error().unwrap().contains("quota reached"));
    assert!(!session.pane_pending());
    assert_eq!(session.focused_pane(), Some(original.clone()));
    let request_id = requested_spawn(&mut session, "vertical");
    let spawned = ResourceId::local(202);
    let request_id = announce_split(&mut session, request_id, spawned.clone());
    let refused = session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
            message: "attach refused".to_owned(),
        },
    });
    assert!(matches!(decode_one(&refused.send[0]), FrameKind::Command {
        command: Command::KillResource { terminal_id, .. }, ..
    } if terminal_id == spawned));
    assert_eq!(session.pane_rects().len(), 1);
    assert!(!session.is_failed());
    assert!(matches!(decode_one(&session.key_frame(key()).unwrap()),
        FrameKind::InputKey { terminal_id, .. } if terminal_id == original));
}

#[wasm_bindgen_test]
async fn closing_a_pane_waits_for_resource_closed_and_restores_sibling_size() {
    use phux_protocol::wire::frame::{CloseReason, Command, CommandResult};
    let mut session = path_session(false, false).await;
    assert!(
        session
            .close_pane_frame()
            .unwrap_err()
            .contains("Release Session")
    );
    let original = ResourceId::local(101);
    let second = ResourceId::local(202);
    let spawn = requested_spawn(&mut session, "vertical");
    let attach = announce_split(&mut session, spawn, second.clone());
    bootstrap_split(&mut session, second.clone(), 2, b"independent");
    session.on_frame(FrameKind::CommandResult {
        request_id: attach,
        result: CommandResult::Ok,
    });
    let resized = session.resize_panes(60, 15);
    let sizes: Vec<_> = resized
        .iter()
        .map(|frame| match decode_one(frame) {
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                ..
            } => (terminal_id, cols, rows),
            other => panic!("expected addressed resize, got {other:?}"),
        })
        .collect();
    assert_eq!(
        sizes,
        vec![(original.clone(), 29, 15), (second.clone(), 30, 15)]
    );
    let request_id = match decode_one(&session.close_pane_frame().unwrap()) {
        FrameKind::Command {
            request_id,
            command: Command::KillResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, second);
            request_id
        }
        other => panic!("expected kill, got {other:?}"),
    };
    session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Ok,
    });
    assert_eq!(session.pane_rects().len(), 2);
    assert!(session.pane_pending());
    let closed = session.on_frame(FrameKind::ResourceClosed {
        terminal_id: second,
        exit_status: Some(0),
        reason: CloseReason::Unknown,
        signal: None,
    });
    assert!(!session.pane_pending());
    assert_eq!(session.focused_pane(), Some(original.clone()));
    assert_eq!(session.pane_rects()[0].1.cols, 60);
    assert!(closed.send.iter().any(|frame| matches!(decode_one(frame),
        FrameKind::ResizeTerminal { terminal_id, cols: 60, rows: 15, .. } if terminal_id == original)));
    assert!(session.close_pane_frame().is_err());
    assert!(session.key_frame(key()).is_some());
}

#[wasm_bindgen_test]
async fn panes_stop_at_four_and_survive_a_refused_close() {
    use phux_protocol::wire::frame::CommandResult;
    let mut session = path_session(false, false).await;
    session.resize_panes(120, 40);
    for serial in 2..=4 {
        let spawn = requested_spawn(
            &mut session,
            if serial == 3 {
                "horizontal"
            } else {
                "vertical"
            },
        );
        let id = ResourceId::local(200 + serial as u32);
        let attach = announce_split(&mut session, spawn, id.clone());
        bootstrap_split(&mut session, id, serial, b"pane");
        session.on_frame(FrameKind::CommandResult {
            request_id: attach,
            result: CommandResult::Ok,
        });
    }
    assert_eq!(session.pane_rects().len(), 4);
    assert!(
        session
            .split_pane_frame("vertical")
            .unwrap_err()
            .contains("Four")
    );
    let focused = session.focused_pane();
    let request_id = match decode_one(&session.close_pane_frame().unwrap()) {
        FrameKind::Command { request_id, .. } => request_id,
        other => panic!("{other:?}"),
    };
    session.on_frame(FrameKind::CommandResult {
        request_id,
        result: CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
            message: "not permitted".to_owned(),
        },
    });
    assert_eq!(session.pane_rects().len(), 4);
    assert_eq!(session.focused_pane(), focused);
    assert!(!session.pane_pending());
    assert!(session.key_frame(key()).is_some());
}

#[wasm_bindgen_test]
async fn pane_resize_restores_size_before_earlier_bootstrap_arrives() {
    let mut session = path_session(false, false).await;
    let sent = session.resize_panes(40, 3);
    assert!(matches!(
        decode_one(&sent[0]),
        FrameKind::ResizeTerminal {
            cols: 40,
            rows: 3,
            ..
        }
    ));
    let restored = session.resize_panes(20, 3);
    assert_eq!(
        restored.len(),
        1,
        "the outstanding 40-column request must be superseded"
    );
    assert!(matches!(
        decode_one(&restored[0]),
        FrameKind::ResizeTerminal {
            cols: 20,
            rows: 3,
            ..
        }
    ));
}

#[wasm_bindgen_test]
async fn failed_split_quarantines_queued_frames_without_weakening_other_streams() {
    let mut session = path_session(false, false).await;
    let spawned = ResourceId::local(202);
    let request = requested_spawn(&mut session, "vertical");
    announce_split(&mut session, request, spawned.clone());
    session.on_frame(begin(
        spawned.clone(),
        stream(2),
        bootstrap(2),
        phux_protocol::caps::BootstrapStreamProfile::SynthesizedVtRaw,
        9,
        3,
        0,
    ));
    let refused = session.on_frame(FrameKind::BootstrapChunk {
        terminal_id: spawned.clone(),
        stream_id: stream(2),
        bootstrap_id: bootstrap(2),
        chunk_seq: 0,
        payload: Bytes::from(vec![
            b'x';
            session.bootstrap_limits().unwrap().max_chunk_bytes()
                as usize
                + 1
        ]),
    });
    assert!(refused.fatal.is_none());
    assert!(session.pane_error().is_some());
    let late = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: spawned.clone(),
        stream_id: stream(2),
        bootstrap_id: bootstrap(2),
        history_cursor: None,
    });
    assert!(
        late.fatal.is_none(),
        "a failed split must not close its healthy sibling"
    );
    session.on_frame(FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(101),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        seq: 1,
        bytes: Bytes::from_static(b"healthy"),
    });
    let text: String = session.grid().cells.iter().map(|cell| cell.ch).collect();
    assert!(text.starts_with("healthy"));
    assert!(session.key_frame(key()).is_some());
    let closed = session.on_frame(FrameKind::ResourceClosed {
        terminal_id: spawned,
        exit_status: None,
        signal: None,
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
    });
    assert!(closed.fatal.is_none());
    let unrelated = session.on_frame(FrameKind::BootstrapReady {
        terminal_id: ResourceId::local(999),
        stream_id: stream(9),
        bootstrap_id: bootstrap(9),
        history_cursor: None,
    });
    assert!(
        unrelated.fatal.is_some(),
        "unknown streams remain a protocol fault"
    );
}
