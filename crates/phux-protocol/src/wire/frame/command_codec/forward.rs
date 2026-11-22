//! The `FORWARD_*` command bodies and `FORWARD_READ`'s reply: positional,
//! byte-bounded records (`crate::forward`).

use crate::forward::{ForwardChunk, ForwardOp, MAX_FORWARD_CHUNK_BYTES, op_fits};
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::frame::command_codec::unknown_command;
use crate::wire::frame::{
    COMMAND_TAG_AGENT_OFFER, COMMAND_TAG_AGENT_REPLY, COMMAND_TAG_FORWARD_CLOSE,
    COMMAND_TAG_FORWARD_OPEN, COMMAND_TAG_FORWARD_READ, COMMAND_TAG_FORWARD_WRITE,
    COMMAND_VALUE_TAG_FORWARD_CHUNK,
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
        ForwardOp::AgentOffer => enc.write_u8(COMMAND_TAG_AGENT_OFFER),
        ForwardOp::AgentReply { id, message } => {
            enc.write_u8(COMMAND_TAG_AGENT_REPLY);
            enc.write_u64_be(*id);
            enc.write_bytes(message);
        }
    }
}

/// Decode a `FORWARD_*` command; any other tag is an unknown command.
pub(super) fn decode_op(tag: u8, dec: &mut Decoder<'_>) -> Result<ForwardOp, DecodeError> {
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
        COMMAND_TAG_AGENT_OFFER => ForwardOp::AgentOffer,
        COMMAND_TAG_AGENT_REPLY => ForwardOp::AgentReply {
            id: dec.read_u64_be()?,
            message: read_chunk_bytes(dec)?,
        },
        _ => return Err(unknown_command(tag)),
    };
    if op_fits(&op) {
        Ok(op)
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

/// An `agent_request` event body: the request's id, then the ssh-agent
/// message.
pub(super) fn encode_agent_request(id: u64, message: &[u8], enc: &mut Encoder<'_>) {
    enc.write_u64_be(id);
    enc.write_bytes(message);
}

pub(super) fn decode_agent_request(dec: &mut Decoder<'_>) -> Result<(u64, Vec<u8>), DecodeError> {
    Ok((dec.read_u64_be()?, read_chunk_bytes(dec)?))
}

/// A chunk or an ssh-agent message (the two bounds are equal), refused
/// before it is copied when over the bound.
fn read_chunk_bytes(dec: &mut Decoder<'_>) -> Result<Vec<u8>, DecodeError> {
    let bytes = dec.read_bytes()?;
    if bytes.len() > MAX_FORWARD_CHUNK_BYTES {
        return Err(DecodeError::ForwardLimitExceeded);
    }
    Ok(bytes.to_vec())
}
