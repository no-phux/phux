use super::*;

/// A `RemoteClient` over a sans-IO runtime client attached to `panes`, each
/// bootstrapped with its bytes. No socket: frames are fed directly.
pub(super) fn remote_with(panes: &[(u32, Vec<u8>)]) -> Arc<RemoteClient> {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (COLS, ROWS),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features(ServerFeatureSet::new()),
            server_id: vec![0xAB; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("HELLO_OK");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decodes").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH queued");
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let resources = panes
        .iter()
        .map(|(id, _)| ResourceInfo::new(ResourceId::local(*id), window, COLS, ROWS))
        .collect();
    let snapshot = SessionSnapshot::new(session, window, ResourceId::local(panes[0].0))
        .with_sessions(vec![SessionInfo::new(session, "main")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(resources);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
    for (id, bytes) in panes {
        bootstrap(&client, ResourceId::local(*id), bytes);
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
    let _ = client.take_outbound();
    let remote = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    *remote.client.lock().unwrap() = Some(client);
    remote
}
