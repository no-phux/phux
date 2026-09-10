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

// ---- phux-c2td.3: held federation notices --------------------------------

fn host_rows() -> Vec<phux_protocol::wire::info::HostInventory> {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::HostInventory;

    vec![
        HostInventory::unreachable(
            SatelliteHost::new("down"),
            "satellite down is unreachable: link is down",
        ),
        HostInventory::reachable(SatelliteHost::new("edge"), Vec::new()),
    ]
}

/// The reply drops only the notices its unreachable rows explain; a link
/// drop for a satellite it still lists as reachable surfaces.
#[test]
fn held_notices_surface_unless_the_inventory_explains_them() {
    let held = vec![
        // Explained: the row carries exactly this diagnostic.
        "satellite down is unreachable: link is down".to_owned(),
        // Explained: same host, a different reason.
        "satellite down is unreachable: dial refused".to_owned(),
        // Not explained: the inventory lists `edge` as reachable.
        "satellite edge is unreachable: link dropped".to_owned(),
        // Not explained: a different host whose name merely extends `down`.
        "satellite downstairs is unreachable: gone".to_owned(),
    ];
    assert_eq!(
        unexplained_unreachable_notices(held, &host_rows()),
        vec![
            "satellite edge is unreachable: link dropped".to_owned(),
            "satellite downstairs is unreachable: gone".to_owned(),
        ]
    );
}

/// An inventory with no unreachable rows explains nothing, and a refusal
/// (no inventory at all) surfaces everything held.
#[test]
fn held_notices_all_surface_when_nothing_explains_them() {
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::HostInventory;

    let rows = vec![HostInventory::reachable(
        SatelliteHost::new("down"),
        Vec::new(),
    )];
    let held = vec!["satellite down is unreachable: link is down".to_owned()];
    assert_eq!(unexplained_unreachable_notices(held.clone(), &rows), held);
    assert_eq!(unexplained_unreachable_notices(held.clone(), &[]), held);
}

/// A request is overdue only past the deadline, and never when none is in
/// flight.
#[test]
fn host_inventory_overdue_only_past_the_deadline() {
    let now = std::time::Instant::now();
    assert!(!host_inventory_overdue(None, now));
    assert!(!host_inventory_overdue(Some(now), now));
    let sent = now.checked_sub(HOST_INVENTORY_DEADLINE + std::time::Duration::from_secs(1));
    assert!(sent.is_some_and(|sent| host_inventory_overdue(Some(sent), now)));
}

/// Held notices surface in the same wording the frame handler uses for a
/// live degradation notice.
#[test]
fn federation_notices_use_the_degraded_wording() {
    let notices = federation_notices(vec!["satellite edge is unreachable: x".to_owned()]);
    assert_eq!(notices.len(), 1);
    assert_eq!(
        notices[0].text,
        "federation degraded: satellite edge is unreachable: x"
    );
}
