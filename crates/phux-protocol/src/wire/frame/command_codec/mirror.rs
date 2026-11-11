//! The `MIRROR` command body (an optional satellite host, then one
//! [`MirrorOp`] as a tag byte and its fields) and `MIRROR_PROVIDE`'s (the
//! target list).

use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::frame::{
    COMMAND_TAG_MIRROR, COMMAND_TAG_MIRROR_PROVIDE, Command, MAX_MIRROR_TARGET, MAX_MIRROR_TARGETS,
    MirrorOp,
};

const OP_TARGETS: u8 = 0;
const OP_START: u8 = 1;
const OP_STOP: u8 = 2;

pub(super) fn encode_provide(targets: &str, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_TAG_MIRROR_PROVIDE);
    enc.write_str(targets);
}

pub(super) fn encode(host: Option<&str>, op: &MirrorOp, enc: &mut Encoder<'_>) {
    enc.write_u8(COMMAND_TAG_MIRROR);
    enc.write_option(host, Encoder::write_str);
    match op {
        MirrorOp::Targets => enc.write_u8(OP_TARGETS),
        MirrorOp::Start { target, jpeg } => {
            enc.write_u8(OP_START);
            enc.write_str(target);
            enc.write_u8(u8::from(*jpeg));
        }
        MirrorOp::Stop { target } => {
            enc.write_u8(OP_STOP);
            enc.write_str(target);
        }
    }
}

/// Decode a `MIRROR` command; `None` (reading nothing) for any other tag.
pub(super) fn decode_command(
    tag: u8,
    dec: &mut Decoder<'_>,
) -> Result<Option<Command>, DecodeError> {
    let bounded = |d: &mut Decoder<'_>, max: usize| -> Result<String, DecodeError> {
        let text = d.read_str()?;
        if text.len() > max {
            return Err(DecodeError::LengthOverflow);
        }
        Ok(text.to_owned())
    };
    if tag == COMMAND_TAG_MIRROR_PROVIDE {
        let targets = bounded(dec, MAX_MIRROR_TARGETS)?;
        return Ok(Some(Command::MirrorProvide { targets }));
    }
    if tag != COMMAND_TAG_MIRROR {
        return Ok(None);
    }
    let host = dec.read_option("Mirror.host tag", |d| Ok(d.read_str()?.to_owned()))?;
    let op = match dec.read_u8()? {
        OP_TARGETS => MirrorOp::Targets,
        OP_START => MirrorOp::Start {
            target: bounded(dec, MAX_MIRROR_TARGET)?,
            jpeg: dec.read_bool("Mirror.jpeg")?,
        },
        OP_STOP => MirrorOp::Stop {
            target: bounded(dec, MAX_MIRROR_TARGET)?,
        },
        other => return Err(DecodeError::unknown_enum("MirrorOp", other)),
    };
    Ok(Some(Command::Mirror { host, op }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_round_trips() {
        let start = MirrorOp::Start {
            target: "firefox:Docs".to_owned(),
            jpeg: true,
        };
        let stop = MirrorOp::Stop {
            target: "display".to_owned(),
        };
        let commands = [
            Command::Mirror {
                host: None,
                op: MirrorOp::Targets,
            },
            Command::Mirror {
                host: Some("box".to_owned()),
                op: start,
            },
            Command::Mirror {
                host: None,
                op: stop,
            },
            Command::MirrorProvide {
                targets: "[]".to_owned(),
            },
        ];
        for command in commands {
            let frame = crate::wire::frame::FrameKind::Command {
                request_id: 7,
                command,
            };
            let mut buf = bytes::BytesMut::new();
            frame.encode(&mut buf);
            let (decoded, tail) = crate::wire::frame::FrameKind::decode(&buf).expect("decode");
            assert_eq!(decoded, frame);
            assert!(tail.is_empty());
        }
    }
}
