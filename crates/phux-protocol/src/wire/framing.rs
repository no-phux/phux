//! SPEC §5 length-prefix framing — the single owning implementation.
//!
//! `docs/spec/proto.md` §5 is normative: every message on the wire is
//!
//! ```text
//! +-------------------------+
//! | length: u32 big-endian  |   number of bytes that follow
//! +-------------------------+
//! | type:   u8              |
//! +-------------------------+
//! | payload: length-1 bytes |
//! +-------------------------+
//! ```
//!
//! so a frame occupies exactly <code>[LENGTH_PREFIX_LEN] + length</code>
//! bytes, and `length` MUST lie in <code>1..=[MAX_FRAME_LEN]</code>.
//!
//! Every transport phux speaks — UDS, QUIC, WebTransport, WebSocket, and the
//! satellite ssh link — re-derives that rule from the same three primitives
//! here, so the rule has one implementation rather than one per transport:
//!
//! | Shape of the input | Use |
//! |---|---|
//! | A 4-byte header read off a byte stream | [`frame_buffer`] (or [`decode_length`]) |
//! | A growing buffer that may hold 0..n frames | [`split_frame`] |
//! | A datagram that must hold exactly one frame | [`check_frame`] |
//!
//! These primitives validate *framing* only. Decoding the body into a
//! [`FrameKind`] stays with
//! [`Decoder::read_frame`](super::decode::Decoder::read_frame), which
//! re-checks the same bound because it is also reachable on buffers that never
//! passed through a transport reader.

use bytes::BytesMut;
use thiserror::Error;

use super::frame::{ErrorCode, FrameKind, MAX_FRAME_LEN};

/// Bytes in the wire length prefix: a big-endian `u32`, per
/// `docs/spec/proto.md` §5.
pub const LENGTH_PREFIX_LEN: usize = 4;

/// A frame header that violates `docs/spec/proto.md` §5.
///
/// Framing errors are unrecoverable for the connection: §5 requires the peer
/// to send `ERROR { code: FRAME_TOO_LARGE }` and close the transport, because
/// there is no resynchronisation point in a pure length-prefixed stream.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FramingError {
    /// The declared `length` was `0` (no room for the type byte) or exceeded
    /// [`MAX_FRAME_LEN`].
    #[error("frame length {length} is outside the permitted range 1..=16777216")]
    LengthOutOfRange {
        /// The declared `length` field, verbatim.
        length: u32,
    },

    /// A buffer that must hold exactly one frame did not: its size disagreed
    /// with `LENGTH_PREFIX_LEN + length`. Covers both a truncated frame and
    /// trailing bytes past the frame's end — §5 defines no second framing
    /// layer, so neither is representable.
    #[error("frame declares {expected} bytes on the wire but the buffer holds {actual}")]
    LengthMismatch {
        /// `LENGTH_PREFIX_LEN + length`, the size the header claims.
        expected: usize,
        /// The size the buffer actually has.
        actual: usize,
    },

    /// A buffer that must hold a complete frame was shorter than the 4-byte
    /// length prefix, so no `length` could even be read.
    #[error("buffer holds {actual} bytes, too few for the 4-byte length prefix")]
    HeaderTruncated {
        /// The size the buffer actually has.
        actual: usize,
    },
}

impl FramingError {
    /// The peer-visible text §5 obliges the receiving peer to put in its
    /// `ERROR { code: FRAME_TOO_LARGE }` before closing.
    ///
    /// One definition of the wording, next to the type that describes the
    /// violation: every phux peer role owes the *same* message for the same
    /// §5 obligation — the server's per-client loop and the hub's satellite
    /// links both send it — and only the transport differs. Prefer
    /// [`frame_too_large_error`] where a whole frame is wanted; this is for
    /// callers that already hold the code and need only the text.
    #[must_use]
    pub fn wire_message(self) -> String {
        format!("frame violates SPEC §5 framing: {self}")
    }
}

impl From<FramingError> for std::io::Error {
    /// Framing violations are malformed peer input, not local faults, so they
    /// surface as [`std::io::ErrorKind::InvalidData`] with the framing error
    /// retained as the source.
    fn from(err: FramingError) -> Self {
        Self::new(std::io::ErrorKind::InvalidData, err)
    }
}

/// Build the `ERROR { code: FRAME_TOO_LARGE }` frame §5 obliges a peer to send
/// before closing a transport whose peer broke framing.
///
/// The frame is the shared part of that obligation; how it reaches the wire is
/// not (the server enqueues it on a client mailbox, the hub encodes it straight
/// onto the condemned link). Constructing it here keeps one wording, one code,
/// and one `request_id` decision — a framing violation is never
/// COMMAND-correlated, so the id is always `None`.
#[must_use]
pub fn frame_too_large_error(violation: FramingError) -> FrameKind {
    FrameKind::Error {
        request_id: None,
        code: ErrorCode::FrameTooLarge,
        message: violation.wire_message(),
    }
}

/// Validate a frame header's `length` field and widen it to `usize`.
///
/// The returned value is the *body* length — the type byte plus payload,
/// excluding the prefix itself. A frame therefore occupies
/// [`LENGTH_PREFIX_LEN`] plus the returned value bytes on the wire.
///
/// This is the primitive for a reader that has just pulled four bytes off a
/// byte stream and needs to know how many more to expect.
#[inline]
pub fn decode_length(header: [u8; LENGTH_PREFIX_LEN]) -> Result<usize, FramingError> {
    let length = u32::from_be_bytes(header);
    if !(1..=MAX_FRAME_LEN).contains(&length) {
        return Err(FramingError::LengthOutOfRange { length });
    }
    // `MAX_FRAME_LEN` is 16 MiB, so this fits usize on every target phux
    // builds for, including wasm32.
    Ok(length as usize)
}

/// Validate a frame header and allocate the exact buffer its frame needs.
///
/// The returned buffer is `LENGTH_PREFIX_LEN + length` bytes long: the header
/// is already written at the front and the body is zero-filled, so a stream
/// reader fills it with a single
/// `read_exact(&mut buf[LENGTH_PREFIX_LEN..])` and hands the whole buffer —
/// prefix included — to [`FrameKind::decode`](super::frame::FrameKind::decode).
///
/// The allocation is bounded by [`MAX_FRAME_LEN`] because the header is
/// validated first, so a peer cannot induce an unbounded reserve.
pub fn frame_buffer(header: [u8; LENGTH_PREFIX_LEN]) -> Result<BytesMut, FramingError> {
    let body_len = decode_length(header)?;
    let total = LENGTH_PREFIX_LEN + body_len;
    let mut framed = BytesMut::with_capacity(total);
    framed.extend_from_slice(&header);
    framed.resize(total, 0);
    Ok(framed)
}

/// Peel one complete frame off the front of `buf`, prefix included.
///
/// Returns `Ok(None)` when `buf` does not yet hold a whole frame — either the
/// length prefix has not arrived or the body is still in flight — leaving
/// `buf` untouched so the caller can retry after the next read. On success the
/// frame's bytes are removed from the front of `buf` and any trailing partial
/// frame stays behind.
///
/// This is the primitive for a transport that delivers an arbitrary byte run
/// per read and may over-read into the next frame.
pub fn split_frame(buf: &mut BytesMut) -> Result<Option<BytesMut>, FramingError> {
    if buf.len() < LENGTH_PREFIX_LEN {
        return Ok(None);
    }
    let mut header = [0_u8; LENGTH_PREFIX_LEN];
    header.copy_from_slice(&buf[..LENGTH_PREFIX_LEN]);
    let total = LENGTH_PREFIX_LEN + decode_length(header)?;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some(buf.split_to(total)))
}

/// Validate that `bytes` is exactly one whole frame, and return its body
/// length.
///
/// Rejects a short buffer, a `length` out of range, and — the case a
/// per-transport check is most likely to miss — a buffer whose size disagrees
/// with the `length` it declares. §5 defines no second framing layer, so a
/// datagram carrying a frame plus trailing bytes is malformed rather than a
/// batch, and the trailing bytes must not be silently dropped.
///
/// This is the primitive for a message-oriented transport (WebSocket binary
/// messages) and for asserting a locally encoded frame is emittable.
pub fn check_frame(bytes: &[u8]) -> Result<usize, FramingError> {
    let Some(header) = bytes.first_chunk::<LENGTH_PREFIX_LEN>() else {
        return Err(FramingError::HeaderTruncated {
            actual: bytes.len(),
        });
    };
    let body_len = decode_length(*header)?;
    let expected = LENGTH_PREFIX_LEN + body_len;
    if bytes.len() == expected {
        Ok(body_len)
    } else {
        Err(FramingError::LengthMismatch {
            expected,
            actual: bytes.len(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    /// A one-byte body is the smallest legal frame: just the type byte.
    const MIN_FRAME: [u8; 5] = [0, 0, 0, 1, 0x01];

    /// Both peer roles that owe the §5 goodbye build it here, so this pins the
    /// shape once for all of them: code `FRAME_TOO_LARGE`, no `request_id`, and
    /// a message that names the offending length.
    #[test]
    fn frame_too_large_error_carries_the_code_and_names_the_violation() {
        let violation = FramingError::LengthOutOfRange { length: 0 };
        let frame = frame_too_large_error(violation);
        assert!(
            matches!(
                &frame,
                FrameKind::Error {
                    request_id: None,
                    code: ErrorCode::FrameTooLarge,
                    message,
                } if *message == violation.wire_message() && message.contains('0')
            ),
            "expected a FRAME_TOO_LARGE error naming the violation, got {frame:?}",
        );
    }

    #[test]
    fn decode_length_accepts_the_spec_bounds() {
        assert_eq!(decode_length([0, 0, 0, 1]).unwrap(), 1);
        assert_eq!(
            decode_length(MAX_FRAME_LEN.to_be_bytes()).unwrap(),
            MAX_FRAME_LEN as usize
        );
    }

    #[test]
    fn decode_length_rejects_zero_and_oversize() {
        assert_eq!(
            decode_length([0, 0, 0, 0]),
            Err(FramingError::LengthOutOfRange { length: 0 })
        );
        let over = MAX_FRAME_LEN + 1;
        assert_eq!(
            decode_length(over.to_be_bytes()),
            Err(FramingError::LengthOutOfRange { length: over })
        );
        assert_eq!(
            decode_length([0xff, 0xff, 0xff, 0xff]),
            Err(FramingError::LengthOutOfRange { length: u32::MAX })
        );
    }

    #[test]
    fn frame_buffer_is_sized_for_prefix_plus_body() {
        let buf = frame_buffer([0, 0, 0, 7]).unwrap();
        assert_eq!(buf.len(), LENGTH_PREFIX_LEN + 7);
        assert_eq!(&buf[..LENGTH_PREFIX_LEN], &[0, 0, 0, 7]);
        assert!(buf[LENGTH_PREFIX_LEN..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn split_frame_peels_frames_in_order_and_keeps_the_tail() {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&MIN_FRAME);
        buf.extend_from_slice(&[0, 0, 0, 2, 0x02, 0xaa]);
        buf.extend_from_slice(&[0, 0, 0]); // partial third header

        let first = split_frame(&mut buf).unwrap().expect("first frame");
        assert_eq!(&first[..], &MIN_FRAME);
        let second = split_frame(&mut buf).unwrap().expect("second frame");
        assert_eq!(&second[..], &[0, 0, 0, 2, 0x02, 0xaa]);
        assert!(split_frame(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 3, "partial header is retained for the next read");
    }

    #[test]
    fn split_frame_holds_a_partial_body_without_consuming_it() {
        let mut buf = BytesMut::from(&[0, 0, 0, 4, 0x01, 0xaa][..]);
        assert!(split_frame(&mut buf).unwrap().is_none());
        assert_eq!(buf.len(), 6, "nothing is consumed until the frame is whole");
        buf.extend_from_slice(&[0xbb, 0xcc]);
        assert_eq!(split_frame(&mut buf).unwrap().expect("whole").len(), 8);
        assert!(buf.is_empty());
    }

    #[test]
    fn split_frame_rejects_a_bad_length_rather_than_waiting_forever() {
        let mut buf = BytesMut::from(&[0, 0, 0, 0][..]);
        assert_eq!(
            split_frame(&mut buf),
            Err(FramingError::LengthOutOfRange { length: 0 })
        );
    }

    #[test]
    fn check_frame_accepts_exactly_one_frame() {
        assert_eq!(check_frame(&MIN_FRAME).unwrap(), 1);
    }

    #[test]
    fn check_frame_rejects_trailing_bytes_and_truncation() {
        let mut trailing = MIN_FRAME.to_vec();
        trailing.push(0xff);
        assert_eq!(
            check_frame(&trailing),
            Err(FramingError::LengthMismatch {
                expected: 5,
                actual: 6
            })
        );
        assert_eq!(
            check_frame(&MIN_FRAME[..4]),
            Err(FramingError::LengthMismatch {
                expected: 5,
                actual: 4
            })
        );
    }

    #[test]
    fn check_frame_rejects_a_buffer_too_short_to_hold_a_header() {
        assert_eq!(
            check_frame(&[0, 0, 0]),
            Err(FramingError::HeaderTruncated { actual: 3 })
        );
        assert_eq!(
            check_frame(&[]),
            Err(FramingError::HeaderTruncated { actual: 0 })
        );
    }

    #[test]
    fn framing_errors_surface_as_invalid_data() {
        let err = std::io::Error::from(FramingError::LengthOutOfRange { length: 0 });
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
