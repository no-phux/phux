//! The `OPEN_URL` and `OPEN_URL_REPLY` bodies (`crate::open_url`).

use crate::ids::ResourceId;
use crate::open_url::OpenUrlOp;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::frame::command_codec::unknown_command;
use crate::wire::frame::{COMMAND_TAG_OPEN_URL, COMMAND_TAG_OPEN_URL_REPLY, Command};

pub(super) fn encode(terminal_id: &ResourceId, op: &OpenUrlOp, enc: &mut Encoder<'_>) {
    enc.write_u8(match op {
        OpenUrlOp::Request { .. } => COMMAND_TAG_OPEN_URL,
        OpenUrlOp::Reply { .. } => COMMAND_TAG_OPEN_URL_REPLY,
    });
    super::encode_terminal_id(terminal_id, enc);
    match op {
        OpenUrlOp::Request { url } => enc.write_str(url),
        OpenUrlOp::Reply { id, opened } => {
            enc.write_u64_be(*id);
            enc.write_u8(u8::from(*opened));
        }
    }
}

pub(super) fn decode(tag: u8, dec: &mut Decoder<'_>) -> Result<Command, DecodeError> {
    let terminal_id = super::decode_terminal_id(dec)?;
    let op = match tag {
        COMMAND_TAG_OPEN_URL => OpenUrlOp::Request {
            url: dec.read_str()?.to_owned(),
        },
        COMMAND_TAG_OPEN_URL_REPLY => OpenUrlOp::Reply {
            id: dec.read_u64_be()?,
            opened: dec.read_bool("OpenUrlReply.opened")?,
        },
        _ => return Err(unknown_command(tag)),
    };
    Ok(Command::OpenUrl { terminal_id, op })
}
