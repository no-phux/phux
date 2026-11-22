use super::*;

pub(super) async fn handshake(stream: &mut UnixStream, broker: &Broker) {
    let Some(FrameKind::Hello { client_caps, .. }) = read_frame(stream).await else {
        panic!("HELLO required")
    };
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new()).unwrap();
    send(
        stream,
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new().with_features_ext(
                if broker.advertises_native.load(Ordering::Acquire) {
                    ServerFeatureExtSet::with(&[ServerFeatureExt::NativeAgent])
                } else {
                    ServerFeatureExtSet::new()
                },
            ),
            server_id: vec![],
            selected_profile: Some(selected_profile),
            bootstrap_limits,
        },
    )
    .await;
}
