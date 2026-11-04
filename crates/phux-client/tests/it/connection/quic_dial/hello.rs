use super::*;

#[allow(
    clippy::panic,
    reason = "the fixture must reject a non-HELLO first frame"
)]
pub(super) async fn accept_hello_with_caps(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    server_caps: ServerCapabilities,
) {
    let FrameKind::Hello { client_caps, .. } = read_frame(recv).await else {
        panic!("expected HELLO");
    };
    if server_caps.features.contains(ServerFeature::QuicStreams) {
        assert!(
            client_caps.quic_streams,
            "QUIC_STREAMS selection requires an explicit client offer"
        );
    }
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(&client_caps, &BootstrapCapabilities::new())
            .expect("fixture profiles intersect");
    write_frame(
        send,
        &FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps,
            server_id: Vec::new(),
            selected_profile: Some(selected_profile),
            bootstrap_limits,
        },
    )
    .await;
}
