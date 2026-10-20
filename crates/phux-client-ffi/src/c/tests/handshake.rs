//! `HELLO_OK` acceptance and the first attach correlation.

use super::*;

fn hello_ok_patch(patch: u16, selected_profile: phux_protocol::BootstrapProfile) -> FrameKind {
    let mut frame = hello_ok(ServerCapabilities::new(), selected_profile);
    if let FrameKind::HelloOk { protocol_patch, .. } = &mut frame {
        *protocol_patch = patch;
    }
    frame
}

#[test]
fn hello_ok_accepts_matching_patch_and_native_features() {
    let client = new_client();
    let offered = unsafe { (*client).inner.control().options().client_caps() };
    let profile = if offered
        .bootstrap
        .profiles
        .contains(phux_protocol::BootstrapProfileKind::NativeState)
    {
        phux_protocol::BootstrapProfile::NativeState {
            codec: phux_protocol::EngineCodec::LibghosttySnapshotV1,
            features: phux_protocol::EngineFeatureSet::required_native(),
        }
    } else {
        phux_protocol::BootstrapProfile::SynthesizedVtRaw
    };
    negotiate_with(client, ServerCapabilities::new(), profile);
    assert!(unsafe { (*client).inner.protocol_ready });
    unsafe { phux_client_free(client) };
}

#[test]
fn hello_ok_refuses_mismatched_patch_and_incomplete_native_profiles() {
    let incomplete = phux_protocol::BootstrapProfile::NativeState {
        codec: phux_protocol::EngineCodec::LibghosttySnapshotV1,
        features: phux_protocol::EngineFeatureSet::with(&[
            phux_protocol::EngineFeature::Continuation,
        ]),
    };
    for frame in [
        hello_ok_patch(
            PROTOCOL_VERSION.patch.wrapping_add(1),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        ),
        hello_ok_patch(PROTOCOL_VERSION.patch, incomplete),
    ] {
        let client = new_client();
        assert_eq!(feed(client, &frame), PhuxClientResult::ProtocolError);
        assert!(!unsafe { (*client).inner.protocol_ready });
        unsafe { phux_client_free(client) };
    }
}

#[test]
fn attach_ready_must_match_the_queued_attach_id() {
    let client = attaching(&[], 7);
    let mut encoded = bytes::BytesMut::new();
    FrameKind::AttachReady { attach_id: 8 }.encode(&mut encoded);
    assert_eq!(
        unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) },
        PhuxClientResult::ProtocolError
    );
    assert!(!unsafe { (*client).inner.attached });
    assert!(unsafe { (*client).inner.attach_queued });
    unsafe { phux_client_free(client) };
}

#[test]
fn borrowed_getter_clears_output_before_no_value() {
    let client = new_client();
    let mut frame = PhuxBytes {
        data: ptr::dangling(),
        len: usize::MAX,
    };
    assert_eq!(
        unsafe { phux_client_outgoing_get(client, 0, &raw mut frame) },
        PhuxClientResult::NoValue
    );
    assert!(frame.data.is_null());
    assert_eq!(frame.len, 0);
    unsafe { phux_client_free(client) };
}
