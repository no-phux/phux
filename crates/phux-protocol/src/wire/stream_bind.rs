//! `STREAM_BIND` header for QUIC multi-stream (`docs/spec/proto.md` §4.2,
//! ADR-0113).
//!
//! Transport establishment, not a phux frame: the first bytes a client writes
//! on a newly opened Terminal stream, binding that QUIC stream to one
//! `(terminal_id, stream_id)` pair. Layout on the wire:
//!
//! ```text
//! len: u32 BE || terminal_id || stream_id: u64 BE
//! ```
//!
//! where `terminal_id` is the canonical `ResourceId` encoding (tag byte +
//! `u32`, or tag + length-prefixed host + `u32` for satellite ids) and `len`
//! counts everything after it. Like the bearer preamble, the header takes no
//! frame discriminant; the server answers with that generation's
//! `BOOTSTRAP_BEGIN` on the same stream, or refuses with an uncorrelated
//! `ERROR` on control and a stream reset.

use bytes::BytesMut;

use super::decode::Decoder;
use super::encode::Encoder;
use super::error::DecodeError;
use super::frame::{decode_terminal_id, encode_terminal_id};
use crate::ids::{ResourceId, StreamId};

/// Upper bound on a `STREAM_BIND` header body, in bytes.
///
/// A satellite id carries at most a 255-byte host; the body is one tag byte,
/// one `u32` id, and one `u64` stream id around it. Anything larger is
/// hostile, not a Terminal name, and is refused before allocation — the same
/// posture as the bearer preamble's length cap.
pub const MAX_STREAM_BIND_BYTES: usize = 512;

/// A decoded `STREAM_BIND` header: the Terminal the stream carries and the
/// app-level stream generation it opens under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamBind {
    /// The Terminal whose §4 frames ride this QUIC stream.
    pub terminal_id: ResourceId,
    /// The app-level `StreamId` this stream opens under. Never the QUIC
    /// stream id, which encodes initiator/type bits and is unstable across
    /// reconnects.
    pub stream_id: StreamId,
}

/// Encode a `STREAM_BIND` header (length prefix included) into `buf`.
pub fn encode(bind: &StreamBind, buf: &mut BytesMut) {
    let mut body = BytesMut::new();
    {
        let mut enc = Encoder::new(&mut body);
        encode_terminal_id(&bind.terminal_id, &mut enc);
        enc.write_u64_be(bind.stream_id.get());
    }
    debug_assert!(body.len() <= MAX_STREAM_BIND_BYTES);
    let mut enc = Encoder::new(buf);
    enc.write_u32_be(body.len() as u32);
    // Raw append, not `write_bytes`: that helper length-prefixes its input
    // as a leaf primitive, which would double-frame the header.
    buf.extend_from_slice(&body);
}

/// Decode one `STREAM_BIND` header from the front of `input`.
///
/// Returns the bind plus the number of bytes consumed (length prefix and
/// body). A declared length that disagrees with the available input, exceeds
/// [`MAX_STREAM_BIND_BYTES`], or leaves trailing bytes after the
/// `terminal_id || stream_id` pair is refused: a bind is fixed-shape, so
/// trailing bytes are corruption, not extension.
pub fn decode(input: &[u8]) -> Result<(StreamBind, usize), DecodeError> {
    let mut dec = Decoder::new(input);
    let len = dec.read_u32_be()? as usize;
    if len == 0 || len > MAX_STREAM_BIND_BYTES {
        return Err(DecodeError::LengthOverflow);
    }
    let body = dec.remaining();
    if body.len() < len {
        return Err(DecodeError::UnexpectedEof);
    }
    let mut body_dec = Decoder::new(&body[..len]);
    let terminal_id = decode_terminal_id(&mut body_dec)?;
    let stream_id = StreamId::new(body_dec.read_u64_be()?).ok_or(DecodeError::InvalidStreamId)?;
    if !body_dec.at_body_end() {
        return Err(DecodeError::LengthOverflow);
    }
    Ok((
        StreamBind {
            terminal_id,
            stream_id,
        },
        4 + len,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(bind: &StreamBind) {
        let mut buf = BytesMut::new();
        encode(bind, &mut buf);
        let (back, used) = decode(&buf).expect("bind round-trips");
        assert_eq!(&back, bind);
        assert_eq!(used, buf.len());
    }

    #[test]
    fn local_bind_roundtrips() {
        roundtrip(&StreamBind {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).expect("nonzero"),
        });
    }

    #[test]
    fn satellite_bind_roundtrips() {
        roundtrip(&StreamBind {
            terminal_id: ResourceId::satellite("devbox", 12),
            stream_id: StreamId::new(u64::MAX).expect("nonzero"),
        });
    }

    #[test]
    fn oversized_length_refused_before_allocation() {
        let mut buf = BytesMut::new();
        Encoder::new(&mut buf).write_u32_be((MAX_STREAM_BIND_BYTES + 1) as u32);
        assert!(matches!(
            decode(&buf),
            Err(DecodeError::LengthOverflow)
        ));
    }

    #[test]
    fn truncated_body_is_eof_not_a_bind() {
        let mut buf = BytesMut::new();
        encode(
            &StreamBind {
                terminal_id: ResourceId::local(3),
                stream_id: StreamId::new(9).expect("nonzero"),
            },
            &mut buf,
        );
        buf.truncate(buf.len() - 1);
        assert!(matches!(
            decode(&buf),
            Err(DecodeError::UnexpectedEof)
        ));
    }

    #[test]
    fn trailing_bytes_are_corruption() {
        let mut buf = BytesMut::new();
        encode(
            &StreamBind {
                terminal_id: ResourceId::local(3),
                stream_id: StreamId::new(9).expect("nonzero"),
            },
            &mut buf,
        );
        // Grow the declared length by one and append one trailing byte: the
        // body no longer ends exactly at the stream id.
        let len = u32::from_be_bytes(buf[..4].try_into().expect("len")) + 1;
        buf[..4].copy_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&[0]);
        assert!(matches!(
            decode(&buf),
            Err(DecodeError::LengthOverflow)
        ));
    }

    #[test]
    fn zero_stream_id_refused() {
        // tag LOCAL + id + eight zero bytes, framed honestly.
        let mut body = BytesMut::new();
        {
            let mut enc = Encoder::new(&mut body);
            encode_terminal_id(&ResourceId::local(1), &mut enc);
            enc.write_u64_be(0);
        }
        let mut buf = BytesMut::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.write_u32_be(body.len() as u32);
        }
        // Raw append (see `encode`): `write_bytes` would add a second prefix.
        buf.extend_from_slice(&body);
        assert!(matches!(
            decode(&buf),
            Err(DecodeError::InvalidStreamId)
        ));
    }
}
