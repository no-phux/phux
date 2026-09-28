//! `STREAM_BIND` header for QUIC multi-stream (`docs/spec/proto.md` §4.2,
//! ADR-0115).
//!
//! Not a phux frame: the first bytes on a new Terminal stream, binding it to
//! one `(terminal_id, stream_id)` pair as
//! `len: u32 BE || terminal_id || stream_id: u64 BE`.

use bytes::BytesMut;

use super::decode::Decoder;
use super::encode::Encoder;
use super::error::DecodeError;
use super::frame::{decode_terminal_id, encode_terminal_id};
use crate::ids::{ResourceId, StreamId};

/// Upper bound on a `STREAM_BIND` body (a 255-byte host plus fixed fields);
/// anything larger is refused before allocation.
pub const MAX_STREAM_BIND_BYTES: usize = 512;

/// A decoded `STREAM_BIND` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamBind {
    /// The Terminal whose §4 frames ride this QUIC stream.
    pub terminal_id: ResourceId,
    /// The app-level `StreamId` (never the QUIC stream id).
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
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut enc = Encoder::new(buf);
    enc.write_u32_be(len);
    // Raw append: `write_bytes` would add a second length prefix.
    buf.extend_from_slice(&body);
}

/// Decode one `STREAM_BIND` header from the front of `input`, returning it
/// and the bytes consumed. The shape is fixed, so trailing body bytes are
/// corruption.
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
    fn local_and_satellite_binds_roundtrip() {
        roundtrip(&StreamBind {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).expect("nonzero"),
        });
        roundtrip(&StreamBind {
            terminal_id: ResourceId::satellite("devbox", 12),
            stream_id: StreamId::new(u64::MAX).expect("nonzero"),
        });
    }

    #[test]
    fn oversized_length_refused_before_allocation() {
        let mut buf = BytesMut::new();
        {
            let len = u32::try_from(MAX_STREAM_BIND_BYTES + 1).expect("test bound fits");
            Encoder::new(&mut buf).write_u32_be(len);
        }
        assert!(matches!(decode(&buf), Err(DecodeError::LengthOverflow)));
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
        assert!(matches!(decode(&buf), Err(DecodeError::UnexpectedEof)));
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
        assert!(matches!(decode(&buf), Err(DecodeError::LengthOverflow)));
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
            let len = u32::try_from(body.len()).expect("bind body bounded by construction");
            enc.write_u32_be(len);
        }
        // Raw append (see `encode`): `write_bytes` would add a second prefix.
        buf.extend_from_slice(&body);
        assert!(matches!(decode(&buf), Err(DecodeError::InvalidStreamId)));
    }
}
