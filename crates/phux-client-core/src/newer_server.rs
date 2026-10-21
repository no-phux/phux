//! How every client reads a server frame that did not decode.
//!
//! [`newer_server_fallback`] settles an unreadable correlated reply and skips
//! any other frame carrying a newer 1.x server's value. A skip is wrong for
//! the frames an attach waits on: an `ATTACHED` or `ATTACH_READY` this build
//! cannot read would leave the attach pending forever, so those fail
//! visibly instead.

use phux_protocol::wire::DecodeError;
use phux_protocol::wire::LENGTH_PREFIX_LEN;
use phux_protocol::wire::decode::newer_server_fallback;
use phux_protocol::wire::frame::{FrameKind, TYPE_ATTACH_READY, TYPE_ATTACHED};

/// What a client reads in place of `framed`, which failed with `error`.
///
/// `Ok(Some(ERROR))` settles a correlated reply, `Ok(None)` skips a newer
/// server's frame, and `Err` ends the connection, for malformed bytes and
/// for an attach frame this build cannot read.
///
/// # Errors
///
/// `error` when the frame is malformed or is an unreadable `ATTACHED` or
/// `ATTACH_READY`.
pub fn read_undecodable_frame(
    framed: &[u8],
    error: DecodeError,
) -> Result<Option<FrameKind>, DecodeError> {
    let attach_frame = framed
        .get(LENGTH_PREFIX_LEN)
        .is_some_and(|&type_byte| matches!(type_byte, TYPE_ATTACHED | TYPE_ATTACH_READY));
    if attach_frame {
        return Err(error);
    }
    newer_server_fallback(framed, error)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use super::*;

    fn framed(type_byte: u8, body: &[u8]) -> Vec<u8> {
        let len = u32::try_from(body.len() + 1).unwrap();
        let mut frame = len.to_be_bytes().to_vec();
        frame.push(type_byte);
        frame.extend_from_slice(body);
        frame
    }

    /// An attach frame with a newer value fails the attach rather than
    /// hanging it; any other newer frame is still skipped.
    #[test]
    fn an_unreadable_attach_frame_fails_instead_of_being_skipped() {
        let unknown = || DecodeError::UnknownEnumValue {
            field: "ResourceId",
            value: 0xFF,
        };
        for type_byte in [TYPE_ATTACHED, TYPE_ATTACH_READY] {
            assert!(read_undecodable_frame(&framed(type_byte, &[]), unknown()).is_err());
        }
        assert_eq!(
            read_undecodable_frame(&framed(0x6E, &[]), unknown()),
            Ok(None)
        );
    }
}
