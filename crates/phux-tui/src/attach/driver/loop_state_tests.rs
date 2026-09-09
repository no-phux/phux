use super::*;
use phux_protocol::WindowId;
use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerFeatureSet};
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

fn initial_attached() -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        initial_client_id: ClientId::new(1),
        snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "test")])
            .with_windows(vec![WindowInfo::new(
                WindowId::new(1),
                SessionId::new(1),
                "test",
            )])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(1),
                WindowId::new(1),
                80,
                24,
            )]),
    }
}

async fn rename_from_prompt(state: &mut SessionLoop, conn: &mut Connection, out: &mut Vec<u8>) {
    state
        .overlays
        .push(Box::new(crate::render::overlay::PromptOverlay::new(
            "rename window",
            "rename-window",
            "name",
            "renamed",
            &state.settings.theme,
        )));
    let mut events = vec![InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    })];
    state
        .dispatch_input(conn, out, None, &mut events)
        .await
        .unwrap();
}

/// A FIFO marker bounds the observation without relying on an idle-time guess.
async fn assert_no_layout_write(client: &mut Connection, server: &mut Connection) {
    client
        .send(&FrameKind::GetMetadata {
            request_id: u32::MAX,
            scope: Scope::Global,
            key: "test.barrier".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match server.recv().await.unwrap() {
                FrameKind::GetMetadata {
                    request_id: u32::MAX,
                    ..
                } => break,
                FrameKind::SetMetadata { .. } => panic!("layout written before initial read"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn queued_rename_cannot_write_before_initial_metadata_is_processed() {
    for metadata in [Some(b"\xa1\x67version\x02".to_vec()), None] {
        let (a, b) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(a);
        let mut server = Connection::from_stream(b);
        let negotiated = NegotiatedBootstrap {
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features: ServerFeatureSet::new(),
        };
        let mut state = SessionLoop::new(
            negotiated,
            PredictiveConfig::disabled(),
            false,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let mut out = Vec::new();
        state
            .bootstrap(&mut client, &mut out, initial_attached(), None)
            .await
            .unwrap();
        let request_id = state.layout_get_request_id.unwrap();
        let before = state.workspace.clone();
        let unsupported = metadata.is_some();
        server
            .send(&FrameKind::MetadataValue {
                request_id,
                value: metadata,
            })
            .await
            .unwrap();
        // The reply is already queued on the socket, but stdin wins this turn.
        rename_from_prompt(&mut state, &mut client, &mut out).await;
        state.broadcast_layout(&mut client).await.unwrap();
        assert_no_layout_write(&mut client, &mut server).await;
        assert_eq!(state.workspace, before);
        assert!(!state.layout_read_complete);
        let reply = client.recv().await.unwrap();
        let result = state
            .apply_server_frame(
                &mut client,
                &mut out,
                None,
                reply,
                false,
                &mut RepaintAccumulator::default(),
            )
            .await;
        if unsupported {
            assert!(
                matches!(result, Err(AttachError::Protocol(message)) if message.contains("stored metadata was preserved"))
            );
            assert_eq!(state.workspace, before);
            assert!(!state.layout_read_complete);
            assert_no_layout_write(&mut client, &mut server).await;
        } else {
            result.unwrap();
            assert!(state.layout_read_complete);
            assert!(state.layout_get_request_id.is_none());
            rename_from_prompt(&mut state, &mut client, &mut out).await;
            let written = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if let FrameKind::SetMetadata { value, .. } = server.recv().await.unwrap() {
                        break Workspace::decode_cbor(&value).unwrap();
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(written.windows[0].name, "renamed");
        }
    }
}
