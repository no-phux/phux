use super::commands::Undispatched;
use super::plumbing::ConnectionClose;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::decode::frame_request_id;
use phux_protocol::wire::frame::{CommandResult, DetachReason, ErrorCode, FrameKind, TYPE_COMMAND};
use tracing::{debug, warn};

// Refusing a well-framed client frame that is newer than this build, in place, without closing.
//
// Protocol 1.x (`docs/protocol.md` "Compatibility"): after HELLO, a frame
// whose type, tag, enum value, presence byte, or JSON operation or field
// this build does not know is answered `Unsupported` and the connection
// keeps reading. Nothing in it is applied. Structurally malformed bytes
// (truncation, bad UTF-8, oversize counts) still close.
//
// The refusal precedes the frame guard: it reads no server state, so it
// tells a scoped grant only which values this build does not know (what
// `HELLO_OK`'s feature bits already say) and echoes the request id the
// client sent.

/// A frame that did not decode. After HELLO, one a newer 1.x client may
/// send is refused in place; anything else is malformed and closes the
/// connection.
pub(super) fn undecodable(err: &DecodeError, framed: &[u8], negotiated: bool) -> Undispatched {
    if let Some(refusal) = negotiated
        .then(|| unsupported_refusal(err, framed))
        .flatten()
    {
        debug!(error = %err, "client sent a frame this server does not support");
        return Undispatched::Refuse(Box::new(refusal));
    }
    warn!(error = ?err, "client sent undecodable frame; closing");
    Undispatched::Close(ConnectionClose {
        attached_reason: Some("undecodable frame"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::MalformedMessage,
        message: format!("could not decode client frame: {err:?}"),
    })
}

/// The `Unsupported` answer to a well-framed message newer than this build,
/// correlated where the frame carries a request id: a `COMMAND_RESULT` for
/// a `COMMAND`, a correlated `ERROR` for another request frame, and an
/// uncorrelated `ERROR` otherwise. `None` for a malformed frame.
fn unsupported_refusal(err: &DecodeError, framed: &[u8]) -> Option<FrameKind> {
    if !err.is_unsupported() {
        return None;
    }
    let message = match *err {
        DecodeError::UnknownFrameKind { tag } => {
            // An unknown type has no known body, so no request id either.
            return Some(FrameKind::Error {
                request_id: None,
                code: ErrorCode::Unsupported,
                message: format!("unsupported frame type 0x{tag:02x}"),
            });
        }
        DecodeError::UnsupportedCommand { field, value, .. } => {
            format!("unsupported command: unknown {field} value {value}")
        }
        _ => format!("unsupported: {err}"),
    };
    let request_id = match *err {
        DecodeError::UnsupportedCommand { request_id, .. } => Some(request_id),
        _ => frame_request_id(framed),
    };
    let is_command = framed.get(phux_protocol::wire::LENGTH_PREFIX_LEN) == Some(&TYPE_COMMAND);
    Some(match request_id {
        Some(request_id) if is_command => FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::Unsupported,
                message,
            },
        },
        request_id => FrameKind::Error {
            request_id,
            code: ErrorCode::Unsupported,
            message,
        },
    })
}

#[cfg(test)]
mod unsupported_tests {
    use bytes::BytesMut;
    use phux_protocol::wire::encode::Encoder;
    use phux_protocol::wire::frame::{TYPE_GET_METADATA, TYPE_INPUT_KEY};

    use super::*;

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

    fn refusal(framed: &[u8]) -> Option<FrameKind> {
        let err = FrameKind::decode(framed).unwrap_err();
        unsupported_refusal(&err, framed)
    }

    #[test]
    fn an_unknown_scope_in_a_request_frame_is_refused_with_its_request_id() {
        let frame = framed(
            TYPE_GET_METADATA,
            &[(1, &44u32.to_be_bytes()), (2, &[0x7E]), (3, b"k")],
        );
        assert!(matches!(
            refusal(&frame),
            Some(FrameKind::Error {
                request_id: Some(44),
                code: ErrorCode::Unsupported,
                ..
            })
        ));
    }

    #[test]
    fn an_unknown_key_in_an_uncorrelated_frame_is_refused_uncorrelated() {
        let mut event = vec![0, 0, 0, 0];
        event.extend_from_slice(&0x7FFF_FFFF_u32.to_be_bytes());
        let frame = framed(TYPE_INPUT_KEY, &[(1, &[0, 0, 0, 0, 1]), (2, &event)]);
        assert!(matches!(
            refusal(&frame),
            Some(FrameKind::Error {
                request_id: None,
                code: ErrorCode::Unsupported,
                ..
            })
        ));
    }

    #[test]
    fn malformed_bytes_get_no_refusal() {
        let frame = framed(TYPE_GET_METADATA, &[(1, &44u32.to_be_bytes()), (2, &[0])]);
        assert_eq!(refusal(&frame), None, "a truncated scope is malformed");
    }
}
