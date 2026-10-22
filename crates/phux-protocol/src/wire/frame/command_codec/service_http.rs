//! The `SERVICE_HTTP_*` command bodies and their reply values: positional,
//! byte-bounded records (`crate::service_http`).

use crate::service_http::{
    MAX_SERVICE_HTTP_BODY_BYTES, MAX_SERVICE_HTTP_READ_BYTES, ServiceHttpChunk, ServiceHttpHead,
    ServiceHttpRequest, head_fits, read_fits, request_fits,
};
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::frame::codec::{decode_env, encode_env};
use crate::wire::frame::{
    COMMAND_TAG_SERVICE_HTTP_CLOSE, COMMAND_TAG_SERVICE_HTTP_OPEN, COMMAND_TAG_SERVICE_HTTP_READ,
    COMMAND_VALUE_TAG_SERVICE_HTTP_CHUNK, COMMAND_VALUE_TAG_SERVICE_HTTP_HEAD, Command,
};

pub(super) fn encode_open(request: &ServiceHttpRequest, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_TAG_SERVICE_HTTP_OPEN);
    enc.write_option(request.host.as_deref(), Encoder::write_str);
    enc.write_str(&request.service);
    enc.write_str(&request.method);
    enc.write_str(&request.target);
    encode_env(&request.headers, enc);
    enc.write_bytes(&request.body);
}

pub(super) fn encode_read(exchange: u64, max_bytes: u32, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_TAG_SERVICE_HTTP_READ);
    enc.write_u64_be(exchange);
    enc.write_u32_be(max_bytes);
}

pub(super) fn encode_close(exchange: u64, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_TAG_SERVICE_HTTP_CLOSE);
    enc.write_u64_be(exchange);
}

/// Decode a `SERVICE_HTTP_*` command; `None` (reading nothing) for any
/// other tag.
pub(super) fn decode_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let command = match tag {
        COMMAND_TAG_SERVICE_HTTP_OPEN => Command::ServiceHttpOpen {
            request: Box::new(decode_open(dec)?),
        },
        COMMAND_TAG_SERVICE_HTTP_READ => {
            let exchange = dec.read_u64_be()?;
            let max_bytes = dec.read_u32_be()?;
            bounded(read_fits(max_bytes))?;
            Command::ServiceHttpRead {
                exchange,
                max_bytes,
            }
        }
        COMMAND_TAG_SERVICE_HTTP_CLOSE => Command::ServiceHttpClose {
            exchange: dec.read_u64_be()?,
        },
        _ => return Ok(None),
    };
    Ok(Some(command))
}

fn decode_open(dec: &mut Decoder<'_>) -> Result<ServiceHttpRequest, DecodeError> {
    let request = ServiceHttpRequest {
        host: dec.read_option("ServiceHttpOpen.host tag", |d| Ok(d.read_str()?.to_owned()))?,
        service: dec.read_str()?.to_owned(),
        method: dec.read_str()?.to_owned(),
        target: dec.read_str()?.to_owned(),
        headers: decode_env(dec)?,
        body: read_bounded(dec, MAX_SERVICE_HTTP_BODY_BYTES)?,
    };
    bounded(request_fits(&request))?;
    Ok(request)
}

pub(super) fn encode_head(head: &ServiceHttpHead, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_VALUE_TAG_SERVICE_HTTP_HEAD);
    enc.write_u64_be(head.exchange);
    enc.write_u16_be(head.status);
    encode_env(&head.headers, enc);
}

pub(super) fn decode_head(dec: &mut Decoder<'_>) -> Result<ServiceHttpHead, DecodeError> {
    let head = ServiceHttpHead {
        exchange: dec.read_u64_be()?,
        status: dec.read_u16_be()?,
        headers: decode_env(dec)?,
    };
    bounded(head_fits(&head))?;
    Ok(head)
}

pub(super) fn encode_chunk(chunk: &ServiceHttpChunk, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_VALUE_TAG_SERVICE_HTTP_CHUNK);
    enc.write_bytes(&chunk.bytes);
    enc.write_u8(u8::from(chunk.end));
}

pub(super) fn decode_chunk(dec: &mut Decoder<'_>) -> Result<ServiceHttpChunk, DecodeError> {
    let chunk = ServiceHttpChunk {
        bytes: read_bounded(dec, MAX_SERVICE_HTTP_READ_BYTES)?,
        end: dec.read_bool("ServiceHttpChunk.end")?,
    };
    Ok(chunk)
}

/// A body or chunk, refused before it is copied when over `max`.
fn read_bounded(dec: &mut Decoder<'_>, max: usize) -> Result<Vec<u8>, DecodeError> {
    let bytes = dec.read_bytes()?;
    bounded(bytes.len() <= max)?;
    Ok(bytes.to_vec())
}

const fn bounded(fits: bool) -> Result<(), DecodeError> {
    if fits {
        Ok(())
    } else {
        Err(DecodeError::ServiceHttpLimitExceeded)
    }
}
