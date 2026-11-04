use super::*;

#[test]
fn duplicate_hello_ok_is_fatal_in_attached_phase() {
    let mut h = H::on(ArrangementDoc::single(tid(1)), &[&tid(1)]);
    let error = h
        .try_send(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile: Some(phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: phux_protocol::caps::BootstrapLimits::default(),
        })
        .expect_err("post-negotiation HELLO_OK must terminate the client");
    assert!(matches!(
        error,
        AttachError::Protocol(message) if message.contains("not valid from a server")
    ));
}
