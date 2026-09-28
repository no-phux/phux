//! SPEC §5 length-prefix framing, the one implementation every transport uses.
//!
//! A frame is `length: u32 BE || type: u8 || payload`, with `length` in
//! <code>1..=[MAX_FRAME_LEN]</code>. Use [`frame_buffer`] for a stream
//! header, [`split_frame`] for a buffer of 0..n frames, and [`check_frame`]
//! for a datagram that must hold exactly one. Body decoding stays with
//! [`Decoder::read_frame`](super::decode::Decoder::read_frame).

use bytes::BytesMut;
use thiserror::Error;

use super::frame::{ErrorCode, FrameKind, MAX_FRAME_LEN};

/// Bytes in the wire length prefix: a big-endian `u32`, per
/// `docs/spec/proto.md` §5.
pub const LENGTH_PREFIX_LEN: usize = 4;

/// A frame header that violates `docs/spec/proto.md` §5; fatal to the
/// connection, since a length-prefixed stream cannot resynchronise.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FramingError {
    /// The declared `length` was `0` or exceeded [`MAX_FRAME_LEN`].
    #[error("frame length {length} is outside the permitted range 1..=16777216")]
    LengthOutOfRange {
        /// The declared `length` field, verbatim.
        length: u32,
    },

    /// A one-frame buffer was truncated or had trailing bytes.
    #[error("frame declares {expected} bytes on the wire but the buffer holds {actual}")]
    LengthMismatch {
        /// `LENGTH_PREFIX_LEN + length`, the size the header claims.
        expected: usize,
        /// The size the buffer actually has.
        actual: usize,
    },

    /// A one-frame buffer was shorter than the 4-byte length prefix.
    #[error("buffer holds {actual} bytes, too few for the 4-byte length prefix")]
    HeaderTruncated {
        /// The size the buffer actually has.
        actual: usize,
    },
}

impl FramingError {
    /// The text every peer role puts in its §5 `ERROR { FRAME_TOO_LARGE }`.
    #[must_use]
    pub fn wire_message(self) -> String {
        format!("frame violates SPEC §5 framing: {self}")
    }
}

impl From<FramingError> for std::io::Error {
    /// Malformed peer input: [`std::io::ErrorKind::InvalidData`].
    fn from(err: FramingError) -> Self {
        Self::new(std::io::ErrorKind::InvalidData, err)
    }
}

/// The uncorrelated `ERROR { FRAME_TOO_LARGE }` §5 obliges a peer to send
/// before closing a transport whose peer broke framing.
#[must_use]
pub fn frame_too_large_error(violation: FramingError) -> FrameKind {
    FrameKind::Error {
        request_id: None,
        code: ErrorCode::FrameTooLarge,
        message: violation.wire_message(),
    }
}

/// Validate a frame header's `length` and return the body length (type byte
/// plus payload).
#[inline]
pub fn decode_length(header: [u8; LENGTH_PREFIX_LEN]) -> Result<usize, FramingError> {
    let length = u32::from_be_bytes(header);
    if !(1..=MAX_FRAME_LEN).contains(&length) {
        return Err(FramingError::LengthOutOfRange { length });
    }
    Ok(length as usize) // <= 16 MiB fits usize everywhere, wasm32 included
}

/// Validate a header and allocate its whole frame: the header written, the
/// body zero-filled for one `read_exact`. Bounded by [`MAX_FRAME_LEN`].
pub fn frame_buffer(header: [u8; LENGTH_PREFIX_LEN]) -> Result<BytesMut, FramingError> {
    let body_len = decode_length(header)?;
    let total = LENGTH_PREFIX_LEN + body_len;
    let mut framed = BytesMut::with_capacity(total);
    framed.extend_from_slice(&header);
    framed.resize(total, 0);
    Ok(framed)
}

/// Peel one complete frame (prefix included) off the front of `buf`, or
/// `Ok(None)` with `buf` untouched until one has fully arrived.
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

/// Validate that `bytes` is exactly one whole frame and return its body
/// length; trailing bytes are malformed, not a batch.
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
    fn decode_length_enforces_the_spec_bounds() {
        assert_eq!(decode_length([0, 0, 0, 1]).unwrap(), 1);
        assert_eq!(
            decode_length(MAX_FRAME_LEN.to_be_bytes()).unwrap(),
            MAX_FRAME_LEN as usize
        );
        for length in [0, MAX_FRAME_LEN + 1, u32::MAX] {
            assert_eq!(
                decode_length(length.to_be_bytes()),
                Err(FramingError::LengthOutOfRange { length })
            );
        }
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
    fn check_frame_accepts_exactly_one_whole_frame() {
        assert_eq!(check_frame(&MIN_FRAME).unwrap(), 1);
        let mut trailing = MIN_FRAME.to_vec();
        trailing.push(0xff);
        let cases: [(&[u8], FramingError); 4] = [
            (
                &trailing,
                FramingError::LengthMismatch {
                    expected: 5,
                    actual: 6,
                },
            ),
            (
                &MIN_FRAME[..4],
                FramingError::LengthMismatch {
                    expected: 5,
                    actual: 4,
                },
            ),
            (&[0, 0, 0], FramingError::HeaderTruncated { actual: 3 }),
            (&[], FramingError::HeaderTruncated { actual: 0 }),
        ];
        for (bytes, want) in cases {
            assert_eq!(check_frame(bytes), Err(want));
        }
    }
}
