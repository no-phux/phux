use super::*;

pub(super) fn handshake_capabilities(client: &Client, server_id: u8, supports_resume: bool) -> u32 {
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features(ServerFeatureSet::new())
                .with_features_ext(if supports_resume {
                    ServerFeatureExtSet::with(&[ServerFeatureExt::AgentStreamResume])
                } else {
                    ServerFeatureExtSet::new()
                }),
            server_id: vec![server_id; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("HELLO_OK");
    outgoing(client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH queued")
}
