//! A newer 1.x satellite's unknown-value frame is skipped, not fatal: the link survives, and a request it answered settles `UNREADABLE_REPLY`.

use bytes::BytesMut;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::encode::Encoder;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, StateScope, TYPE_COMMAND_RESULT,
    TYPE_DIRECTORY_LISTING, TYPE_RESOURCE_CLOSED,
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

/// Relay `request` and return the request id the link frame carries.
fn relayed_request_id(session: &mut RelaySession, request: RelayRequest) -> u32 {
    let wire = session
        .handle_request_checked(request)
        .expect("the request reaches the link");
    match FrameKind::decode(&wire).unwrap().0 {
        FrameKind::Command { request_id, .. } | FrameKind::ListDirectory { request_id, .. } => {
            request_id
        }
        other => panic!("relayed {other:?}"),
    }
}

#[test]
fn an_unknown_value_inside_a_result_settles_it_unreadable() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut reply_rx) = tokio::sync::oneshot::channel();
    let request_id = relayed_request_id(
        &mut session,
        RelayRequest::Command {
            command: Command::GetState {
                scope: StateScope::Server,
            },
            reply,
            subscribe: None,
        },
    );

    // A COMMAND_RESULT carrying a resource id whose tag no build of this
    // line defines yet: this build's encoding, the id's tag patched. The
    // result decodes as an unreadable reply on its own.
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
    handle_satellite_frame(&mut session, &reply_frame).expect("the link survives");
    assert!(matches!(
        reply_rx.try_recv(),
        Ok(CommandResult::Error { code, .. }) if code == ErrorCode::UNREADABLE_REPLY
    ));
}

#[test]
fn an_undecodable_reply_settles_its_request_unreadable_and_keeps_the_link() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let (reply, mut reply_rx) = tokio::sync::oneshot::channel();
    let request_id = relayed_request_id(
        &mut session,
        RelayRequest::ListDirectory {
            path: "/srv".to_owned(),
            reply,
        },
    );
    // A DIRECTORY_LISTING whose `truncated` flag carries a value no build
    // of this line defines: the whole frame fails to decode.
    let reply_frame = framed(
        TYPE_DIRECTORY_LISTING,
        &[
            (1, &request_id.to_be_bytes()),
            (2, b"/srv"),
            (4, &0u32.to_be_bytes()),
            (5, &[2]),
        ],
    );
    assert!(
        FrameKind::decode(&reply_frame)
            .unwrap_err()
            .is_unsupported()
    );
    handle_satellite_frame(&mut session, &reply_frame).expect("the link survives");
    let refusal = reply_rx
        .try_recv()
        .expect("settled")
        .expect_err("not stated");
    assert!(refusal.message.contains("cannot read"), "{refusal:?}");
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
