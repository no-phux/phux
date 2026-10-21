//! A newer 1.x satellite's unknown-value frame is skipped, not fatal: the link survives, and a request it answered settles `Unsupported`.

use bytes::BytesMut;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::encode::Encoder;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, StateScope, TYPE_COMMAND_RESULT,
    TYPE_RESOURCE_CLOSED,
};

use crate::hub::link::session::handle_satellite_frame;
use crate::hub::relay::{RelayRequest, RelaySession};

use super::*;

/// A framed message of `type_byte` with these TLV fields, in order.
fn framed(type_byte: u8, fields: &[(u32, &[u8])]) -> Vec<u8> {
    let mut body = BytesMut::new();
    let mut enc = Encoder::new(&mut body);
    enc.write_u8(type_byte);
    for (id, value) in fields {
        enc.write_field(*id, value);
    }
    let mut out = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out
}

#[test]
fn an_unknown_value_in_a_reply_settles_its_request_unsupported_and_keeps_the_link() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut reply_rx) = tokio::sync::oneshot::channel();
    let wire = session
        .handle_request_checked(RelayRequest::Command {
            command: Command::GetState {
                scope: StateScope::Server,
            },
            reply,
            subscribe: None,
        })
        .expect("GET_STATE reaches the link");
    let FrameKind::Command { request_id, .. } = FrameKind::decode(&wire).unwrap().0 else {
        panic!("relayed a non-COMMAND");
    };

    // A COMMAND_RESULT carrying a resource id whose tag no build of this
    // line defines yet: this build's encoding, the id's tag patched.
    let mut result = Vec::new();
    let encoded = FrameKind::CommandResult {
        request_id,
        result: CommandResult::OkWith(CommandValue::ResourceId(ResourceId::local(1))),
    };
    let mut buf = BytesMut::new();
    encoded.encode(&mut buf);
    let mut dec = phux_protocol::wire::decode::Decoder::new(&buf[5..]);
    while let Some((id, value)) = dec.read_field().unwrap() {
        if id == 2 {
            result = value.to_vec();
        }
    }
    assert_eq!(result[2], 0, "OK_WITH, RESOURCE_ID, then the Local tag");
    result[2] = 0x7E;
    let reply_frame = framed(
        TYPE_COMMAND_RESULT,
        &[(1, &request_id.to_be_bytes()), (2, &result)],
    );
    assert!(
        FrameKind::decode(&reply_frame)
            .unwrap_err()
            .is_unsupported()
    );
    handle_satellite_frame(&mut session, &reply_frame).expect("the link survives");
    assert!(matches!(
        reply_rx.try_recv(),
        Ok(CommandResult::Error {
            code: ErrorCode::Unsupported,
            ..
        })
    ));
}

#[test]
fn an_uncorrelated_unknown_value_frame_is_skipped_and_malformed_bytes_still_end_the_link() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    // RESOURCE_CLOSED naming a `ResourceId` tag this build does not know.
    let newer = framed(TYPE_RESOURCE_CLOSED, &[(1, &[0x7E, 0, 0, 0, 1])]);
    handle_satellite_frame(&mut session, &newer).expect("skipped, not fatal");
    let unknown_type = framed(0x7E, &[(1, b"future")]);
    handle_satellite_frame(&mut session, &unknown_type).expect("skipped, not fatal");

    let truncated = framed(TYPE_RESOURCE_CLOSED, &[(1, &[0x00, 0, 0])]);
    assert!(handle_satellite_frame(&mut session, &truncated).is_err());
}
