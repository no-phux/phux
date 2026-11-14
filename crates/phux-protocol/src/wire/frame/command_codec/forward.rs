//! The `FORWARD_*` command bodies and `FORWARD_READ`'s reply: positional,
//! byte-bounded records (`crate::forward`).

use crate::forward::{ForwardChunk, ForwardOp, MAX_FORWARD_CHUNK_BYTES, op_fits};
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::frame::{
    COMMAND_TAG_FORWARD_CLOSE, COMMAND_TAG_FORWARD_OPEN, COMMAND_TAG_FORWARD_READ,
    COMMAND_TAG_FORWARD_WRITE, COMMAND_VALUE_TAG_FORWARD_CHUNK,
};

pub(super) fn encode_op(op: &ForwardOp, enc: &mut Encoder<'_>) {
    match op {
        ForwardOp::Open { addr, port } => {
            enc.write_u8(COMMAND_TAG_FORWARD_OPEN);
            enc.write_str(addr);
            enc.write_u16_be(*port);
        }
        ForwardOp::Read { id, max_bytes } => {
            enc.write_u8(COMMAND_TAG_FORWARD_READ);
            enc.write_u64_be(*id);
            enc.write_u32_be(*max_bytes);
        }
        ForwardOp::Write { id, bytes, fin } => {
            enc.write_u8(COMMAND_TAG_FORWARD_WRITE);
            enc.write_u64_be(*id);
            enc.write_bytes(bytes);
            enc.write_u8(u8::from(*fin));
        }
        ForwardOp::Close { id } => {
            enc.write_u8(COMMAND_TAG_FORWARD_CLOSE);
            enc.write_u64_be(*id);
        }
    }
}

/// Decode a `FORWARD_*` command; `None` (reading nothing) for any other tag.
pub(super) fn decode_op(tag: u8, dec: &mut Decoder<'_>) -> Result<Option<ForwardOp>, DecodeError> {
    let op = match tag {
        COMMAND_TAG_FORWARD_OPEN => ForwardOp::Open {
            addr: dec.read_str()?.to_owned(),
            port: dec.read_u16_be()?,
        },
        COMMAND_TAG_FORWARD_READ => ForwardOp::Read {
            id: dec.read_u64_be()?,
            max_bytes: dec.read_u32_be()?,
        },
        COMMAND_TAG_FORWARD_WRITE => ForwardOp::Write {
            id: dec.read_u64_be()?,
            bytes: read_chunk_bytes(dec)?,
            fin: dec.read_bool("ForwardWrite.fin")?,
        },
        COMMAND_TAG_FORWARD_CLOSE => ForwardOp::Close {
            id: dec.read_u64_be()?,
        },
        _ => return Ok(None),
    };
    if op_fits(&op) {
        Ok(Some(op))
    } else {
        Err(DecodeError::ForwardLimitExceeded)
    }
}

pub(super) fn encode_chunk(chunk: &ForwardChunk, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_VALUE_TAG_FORWARD_CHUNK);
    enc.write_bytes(&chunk.bytes);
    enc.write_u8(u8::from(chunk.eof));
}

pub(super) fn decode_chunk(dec: &mut Decoder<'_>) -> Result<ForwardChunk, DecodeError> {
    Ok(ForwardChunk {
        bytes: read_chunk_bytes(dec)?,
        eof: dec.read_bool("ForwardChunk.eof")?,
    })
}

/// A chunk, refused before it is copied when over the bound.
fn read_chunk_bytes(dec: &mut Decoder<'_>) -> Result<Vec<u8>, DecodeError> {
    let bytes = dec.read_bytes()?;
    if bytes.len() > MAX_FORWARD_CHUNK_BYTES {
        return Err(DecodeError::ForwardLimitExceeded);
    }
    Ok(bytes.to_vec())
}
