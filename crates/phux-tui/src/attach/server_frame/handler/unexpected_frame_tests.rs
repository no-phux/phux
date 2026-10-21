//! A server frame the dispatcher has no arm for is ignored, so a newer server
//! cannot tear the attach down with an unsolicited reply; a client-direction
//! frame, or a second `HELLO_OK`, from the server is still a protocol fault.

use phux_protocol::caps::{BootstrapLimits, BootstrapProfile, ServerCapabilities};
use phux_protocol::wire::frame::FrameKind;

use super::{AttachError, unexpected_frame_outcome};

#[test]
fn unhandled_server_frames_are_ignored() {
    for frame in [
        FrameKind::MetadataKeys {
            request_id: 77,
            keys: vec!["x".to_owned()],
        },
        FrameKind::Pong { nonce: 3 },
    ] {
        let outcome = unexpected_frame_outcome(&frame).expect("ignored, not fatal");
        assert!(!outcome.exit);
    }
}

#[test]
fn client_frames_and_a_second_hello_ok_stay_fatal() {
    let hello_ok = FrameKind::HelloOk {
        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new(),
        server_id: Vec::new(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    };
    for frame in [FrameKind::Detach, hello_ok] {
        let error = unexpected_frame_outcome(&frame).expect_err("fatal");
        assert!(
            matches!(&error, AttachError::Protocol(message) if message.contains("not valid from a server")),
            "{error:?}"
        );
    }
}
