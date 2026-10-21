//! The `CommandResult` and `CommandValue` codecs.

use super::{
    COMMAND_RESULT_TAG_ERROR, COMMAND_RESULT_TAG_OK, COMMAND_RESULT_TAG_OK_WITH,
    COMMAND_VALUE_TAG_BYTES, COMMAND_VALUE_TAG_FILE_CHUNK, COMMAND_VALUE_TAG_FILE_UPLOAD,
    COMMAND_VALUE_TAG_GROUP_ID, COMMAND_VALUE_TAG_JSON, COMMAND_VALUE_TAG_RESOURCE_ID,
    COMMAND_VALUE_TAG_STATE, CommandResult, CommandValue, DecodeError, Decoder, Encoder, ErrorCode,
    FileChunk, FileUploadAck, GroupId, IdempotencyKey, MAX_FILE_UPLOAD_CHUNK,
    checkout_decode_error, decode_optional_sha256, decode_session_snapshot, decode_terminal_id,
    encode_session_snapshot, encode_terminal_id,
};

pub(in crate::wire) fn encode_command_result(result: &CommandResult, enc: &mut Encoder<'_>) {
    match result {
        CommandResult::Ok => enc.write_u8(COMMAND_RESULT_TAG_OK),
        CommandResult::OkWith(value) => {
            enc.write_u8(COMMAND_RESULT_TAG_OK_WITH);
            encode_command_value(value, enc);
        }
        CommandResult::Error { code, message } => {
            enc.write_u8(COMMAND_RESULT_TAG_ERROR);
            enc.write_u16_be(code.as_wire());
            enc.write_str(message);
        }
        CommandResult::Refused(refusal) => {
            enc.write_u8(crate::wire::frame::COMMAND_RESULT_TAG_REFUSED);
            crate::wire::frame::org::encode_refusal(refusal, enc);
        }
        CommandResult::Accepted { operation } => {
            enc.write_u8(crate::wire::frame::COMMAND_RESULT_TAG_ACCEPTED);
            for byte in operation.as_bytes() {
                enc.write_u8(*byte);
            }
        }
    }
}

pub(in crate::wire) fn decode_command_result(
    dec: &mut Decoder<'_>,
) -> Result<CommandResult, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        COMMAND_RESULT_TAG_OK => Ok(CommandResult::Ok),
        // The value ends its field, so an unknown value tag, or an unknown
        // enum anywhere inside a known value (an arrangement node, a split
        // direction, an entry target), settles the request instead of
        // failing the frame.
        COMMAND_RESULT_TAG_OK_WITH => match decode_command_value(dec) {
            Err(DecodeError::UnknownEnumValue { field, value }) => Ok(unknown_result(field, value)),
            value => value.map(CommandResult::OkWith),
        },
        COMMAND_RESULT_TAG_ERROR => {
            let code = ErrorCode::from_wire_or_unknown(dec.read_u16_be()?);
            let message = dec.read_str()?.to_owned();
            Ok(CommandResult::Error { code, message })
        }
        crate::wire::frame::COMMAND_RESULT_TAG_REFUSED => Ok(CommandResult::Refused(
            crate::wire::frame::org::decode_refusal(dec)?,
        )),
        crate::wire::frame::COMMAND_RESULT_TAG_ACCEPTED => {
            let mut bytes = [0; 16];
            for byte in &mut bytes {
                *byte = dec.read_u8()?;
            }
            let operation = IdempotencyKey::new(bytes).ok_or(DecodeError::InvalidIdempotencyKey)?;
            Ok(CommandResult::Accepted { operation })
        }
        other => Ok(unknown_result("CommandResult", u32::from(other))),
    }
}

/// The result of a reply whose result tag, value tag, or nested enum value
/// this build does not know: a newer peer's shape. The request settles as an
/// [`ErrorCode::UNREADABLE_REPLY`] error naming the field, never as
/// `Unsupported`: the server answered, so the request may have taken effect.
fn unknown_result(what: &str, tag: u32) -> CommandResult {
    CommandResult::Error {
        code: ErrorCode::UNREADABLE_REPLY,
        message: format!("reply carried an unknown {what} tag {tag:#04x}"),
    }
}

pub(super) fn encode_command_value(value: &CommandValue, enc: &mut Encoder<'_>) {
    match value {
        CommandValue::ResourceId(id) => {
            enc.write_u8(COMMAND_VALUE_TAG_RESOURCE_ID);
            encode_terminal_id(id, enc);
        }
        CommandValue::GroupId(id) => {
            enc.write_u8(COMMAND_VALUE_TAG_GROUP_ID);
            enc.write_u32_be(id.get());
        }
        CommandValue::State(snapshot) => {
            enc.write_u8(COMMAND_VALUE_TAG_STATE);
            encode_session_snapshot(snapshot, enc);
        }
        CommandValue::Json(s) => {
            enc.write_u8(COMMAND_VALUE_TAG_JSON);
            enc.write_str(s);
        }
        CommandValue::Bytes(b) => {
            enc.write_u8(COMMAND_VALUE_TAG_BYTES);
            enc.write_bytes(b);
        }
        CommandValue::FileUpload(ack) => {
            enc.write_u8(COMMAND_VALUE_TAG_FILE_UPLOAD);
            enc.write_u64_be(ack.next_offset);
            enc.write_option(ack.path.as_deref(), Encoder::write_str);
        }
        CommandValue::FileChunk(chunk) => {
            enc.write_u8(COMMAND_VALUE_TAG_FILE_CHUNK);
            enc.write_u64_be(chunk.size);
            enc.write_bytes(&chunk.data);
            enc.write_option(chunk.sha256.as_ref(), |e, digest| {
                for byte in digest {
                    e.write_u8(*byte);
                }
            });
        }
        CommandValue::OrganizationPage(page) => {
            enc.write_u8(crate::wire::frame::COMMAND_VALUE_TAG_ORGANIZATION_PAGE);
            crate::wire::frame::org::encode_organization_page(page, enc);
        }
        CommandValue::Arrangement(value) => {
            enc.write_u8(crate::wire::frame::COMMAND_VALUE_TAG_ARRANGEMENT);
            crate::wire::frame::org::encode_arrangement_value(value, enc);
        }
        CommandValue::Committed(committed) => {
            enc.write_u8(crate::wire::frame::COMMAND_VALUE_TAG_COMMITTED);
            crate::wire::frame::org::encode_committed(committed, enc);
        }
        CommandValue::Delegation(delegation) => {
            enc.write_u8(crate::wire::frame::COMMAND_VALUE_TAG_DELEGATION);
            crate::wire::frame::org::encode_delegation(delegation, enc);
        }
        CommandValue::Checkout(answer) => {
            enc.write_u8(crate::wire::frame::COMMAND_VALUE_TAG_CHECKOUT);
            enc.write_bytes(&crate::checkout::encode_answer(answer));
        }
    }
}

pub(super) fn decode_command_value(dec: &mut Decoder<'_>) -> Result<CommandValue, DecodeError> {
    let tag = dec.read_u8()?;
    match tag {
        COMMAND_VALUE_TAG_RESOURCE_ID => Ok(CommandValue::ResourceId(decode_terminal_id(dec)?)),
        COMMAND_VALUE_TAG_GROUP_ID => Ok(CommandValue::GroupId(GroupId::new(dec.read_u32_be()?))),
        COMMAND_VALUE_TAG_STATE => Ok(CommandValue::State(decode_session_snapshot(dec)?)),
        COMMAND_VALUE_TAG_JSON => Ok(CommandValue::Json(dec.read_str()?.to_owned())),
        COMMAND_VALUE_TAG_BYTES => Ok(CommandValue::Bytes(dec.read_bytes()?.to_vec())),
        COMMAND_VALUE_TAG_FILE_UPLOAD => {
            let next_offset = dec.read_u64_be()?;
            let path = if dec.read_u8()? == 0 {
                None
            } else {
                Some(dec.read_str()?.to_owned())
            };
            Ok(CommandValue::FileUpload(FileUploadAck {
                next_offset,
                path,
            }))
        }
        COMMAND_VALUE_TAG_FILE_CHUNK => {
            let size = dec.read_u64_be()?;
            let data = dec.read_bytes()?;
            if data.len() > MAX_FILE_UPLOAD_CHUNK {
                return Err(DecodeError::FileUploadLimitExceeded);
            }
            let data = data.to_vec();
            let sha256 = decode_optional_sha256(dec)?;
            Ok(CommandValue::FileChunk(FileChunk { size, data, sha256 }))
        }
        crate::wire::frame::COMMAND_VALUE_TAG_ORGANIZATION_PAGE => {
            Ok(CommandValue::OrganizationPage(Box::new(
                crate::wire::frame::org::decode_organization_page(dec)?,
            )))
        }
        crate::wire::frame::COMMAND_VALUE_TAG_ARRANGEMENT => Ok(CommandValue::Arrangement(
            Box::new(crate::wire::frame::org::decode_arrangement_value(dec)?),
        )),
        crate::wire::frame::COMMAND_VALUE_TAG_COMMITTED => Ok(CommandValue::Committed(Box::new(
            crate::wire::frame::org::decode_committed(dec)?,
        ))),
        crate::wire::frame::COMMAND_VALUE_TAG_DELEGATION => Ok(CommandValue::Delegation(Box::new(
            crate::wire::frame::org::decode_delegation(dec)?,
        ))),
        crate::wire::frame::COMMAND_VALUE_TAG_CHECKOUT => Ok(CommandValue::Checkout(Box::new(
            crate::checkout::decode_answer(dec.read_bytes()?).map_err(checkout_decode_error)?,
        ))),
        other => Err(DecodeError::unknown_enum("CommandValue", other)),
    }
}

// `Option<i32>`: presence byte, then the two's-complement bits as a `u32`.
pub(in crate::wire) fn encode_optional_i32(value: Option<i32>, enc: &mut Encoder<'_>) {
    enc.write_option(value, |e, n| {
        e.write_u32_be(u32::from_be_bytes(n.to_be_bytes()));
    });
}

pub(in crate::wire) fn decode_optional_i32(
    dec: &mut Decoder<'_>,
) -> Result<Option<i32>, DecodeError> {
    dec.read_option("Option<i32> tag", |d| {
        Ok(i32::from_be_bytes(d.read_u32_be()?.to_be_bytes()))
    })
}
