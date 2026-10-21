//! Correlating a frame that did not decode: its request id, and what a client reads in its place.

use super::{
    DecodeError, Decoder, ErrorCode, FrameKind, TYPE_COMMAND, TYPE_COMMAND_RESULT,
    TYPE_DELETE_METADATA, TYPE_DIRECTORY_LISTING, TYPE_ERROR, TYPE_GET_METADATA,
    TYPE_LIST_DIRECTORY, TYPE_LIST_METADATA, TYPE_METADATA_KEYS, TYPE_METADATA_VALUE,
    TYPE_MOVE_RESOURCE, TYPE_PATH_QUERY, TYPE_PATH_RESULTS, TYPE_RESOURCE_MOVED,
    TYPE_RESOURCE_SPAWNED, TYPE_SET_METADATA, TYPE_SPAWN_RESOURCE,
};

/// Whether frames of `type_byte` carry their correlating `request_id` as
/// body field 1 (`u32`). Every request and reply frame does.
const fn carries_request_id(type_byte: u8) -> bool {
    matches!(
        type_byte,
        TYPE_ERROR
            | TYPE_GET_METADATA
            | TYPE_SET_METADATA
            | TYPE_DELETE_METADATA
            | TYPE_LIST_METADATA
            | TYPE_METADATA_VALUE
            | TYPE_METADATA_KEYS
            | TYPE_LIST_DIRECTORY
            | TYPE_DIRECTORY_LISTING
            | TYPE_PATH_QUERY
            | TYPE_PATH_RESULTS
            | TYPE_SPAWN_RESOURCE
            | TYPE_RESOURCE_SPAWNED
            | TYPE_MOVE_RESOURCE
            | TYPE_RESOURCE_MOVED
            | TYPE_COMMAND
            | TYPE_COMMAND_RESULT
    )
}

/// The correlating `request_id` of one framed message (length prefix
/// included) whose body did not decode, so its refusal can still answer the
/// request. Walks only the top-level TLV fields, never the values.
///
/// `None` when the frame type carries no request id, or when the framing
/// itself is broken. A request frame without the field reads as `0`, as its
/// decoder defaults it; an `ERROR` without one is uncorrelated (`None`).
#[must_use]
pub fn frame_request_id(framed: &[u8]) -> Option<u32> {
    let mut dec = Decoder::new(framed);
    let length = usize::try_from(dec.read_u32_be().ok()?).ok()?;
    let body_end = length.checked_add(super::super::LENGTH_PREFIX_LEN)?;
    if length == 0 || body_end > framed.len() {
        return None;
    }
    dec.body_end = Some(body_end);
    let type_byte = dec.read_u8().ok()?;
    if !carries_request_id(type_byte) {
        return None;
    }
    let mut request_id = (type_byte != TYPE_ERROR).then_some(0);
    while let Ok(Some((id, value))) = dec.read_field() {
        if id == 1 {
            request_id = Decoder::new(value).read_u32_be().ok();
        }
    }
    request_id
}

/// What a client reads in place of a server frame that failed to decode.
///
/// - A correlated reply (`COMMAND_RESULT`, `RESOURCE_SPAWNED`,
///   `RESOURCE_MOVED`, `METADATA_VALUE`, `METADATA_KEYS`,
///   `DIRECTORY_LISTING`, `PATH_RESULTS`) carrying a value this build does
///   not know is `Ok(Some(ERROR))` for its `request_id` with
///   [`ErrorCode::UNREADABLE_REPLY`]: the request settles as answered with
///   the outcome not stated, instead of waiting forever.
/// - Any other frame carrying such a value (an unknown frame type, or an
///   uncorrelated frame) is `Ok(None)`: skip it, with a log, and keep the
///   connection.
/// - Malformed bytes are `Err(error)`, as before.
///
/// A server sends a new frame type or enum value only to a client whose
/// `HELLO` version or capabilities show it knows it, so this is a fallback,
/// never how a 1.x pair is meant to converse; most state-bearing enums in
/// server frames already decode an `Unknown` variant instead.
///
/// # Errors
///
/// `error` itself when it is not [`DecodeError::is_unsupported`].
pub fn newer_server_fallback(
    framed: &[u8],
    error: DecodeError,
) -> Result<Option<FrameKind>, DecodeError> {
    if !error.is_unsupported() {
        return Err(error);
    }
    let reply = framed
        .get(super::super::LENGTH_PREFIX_LEN)
        .is_some_and(|&type_byte| is_correlated_reply(type_byte));
    Ok(reply
        .then(|| frame_request_id(framed))
        .flatten()
        .map(|request_id| FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::UNREADABLE_REPLY,
            message: format!("this build cannot read the server's reply: {error}"),
        }))
}

/// Whether `type_byte` is a server-to-client reply that settles a request.
const fn is_correlated_reply(type_byte: u8) -> bool {
    matches!(
        type_byte,
        TYPE_METADATA_VALUE
            | TYPE_METADATA_KEYS
            | TYPE_DIRECTORY_LISTING
            | TYPE_PATH_RESULTS
            | TYPE_RESOURCE_SPAWNED
            | TYPE_RESOURCE_MOVED
            | TYPE_COMMAND_RESULT
    )
}

#[cfg(test)]
mod request_id_tests {
    use super::super::TYPE_INPUT_KEY;
    use super::*;

    fn framed(type_byte: u8, fields: &[(u32, &[u8])]) -> Vec<u8> {
        let mut body = bytes::BytesMut::new();
        let mut enc = crate::wire::encode::Encoder::new(&mut body);
        enc.write_u8(type_byte);
        for (id, value) in fields {
            enc.write_field(*id, value);
        }
        let mut out = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn a_request_frame_yields_its_request_id_whatever_its_field_order() {
        let frame = framed(TYPE_GET_METADATA, &[(2, &[0x7F]), (1, &9u32.to_be_bytes())]);
        assert_eq!(frame_request_id(&frame), Some(9));
        assert_eq!(
            frame_request_id(&framed(TYPE_COMMAND, &[(2, &[0x7F])])),
            Some(0)
        );
    }

    #[test]
    fn frames_without_a_request_id_and_broken_framing_yield_none() {
        assert_eq!(
            frame_request_id(&framed(TYPE_INPUT_KEY, &[(1, &[0; 4])])),
            None
        );
        assert_eq!(frame_request_id(&framed(TYPE_ERROR, &[(2, &[0, 1])])), None);
        let mut truncated = framed(TYPE_COMMAND, &[(1, &5u32.to_be_bytes())]);
        truncated.pop();
        assert_eq!(frame_request_id(&truncated), None);
    }
}
