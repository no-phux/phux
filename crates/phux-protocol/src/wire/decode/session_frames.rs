//! The detach and terminal-reply frame bodies. The other attach, input,
//! viewport, and error frames are generated from their `field` modules.

use super::{
    DecodeError, Decoder, DetachReason, FrameKind, MAX_INPUT_TERMINAL_REPLY_BYTES, ResourceId,
    decode_terminal_id, field, req, utf8_value,
};

impl Decoder<'_> {
    pub(super) fn decode_detach(&mut self) -> Result<FrameKind, DecodeError> {
        while self.read_field()?.is_some() {}
        Ok(FrameKind::Detach)
    }

    pub(super) fn decode_input_terminal_reply(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut bytes: Option<bytes::Bytes> = None;
        for_each_field!(self, value, {
            field::input_terminal_reply::TERMINAL_ID => {
                terminal_id = Some(sub!(value, decode_terminal_id));
            }
            field::input_terminal_reply::BYTES => {
                if value.is_empty() || value.len() > MAX_INPUT_TERMINAL_REPLY_BYTES {
                    return Err(DecodeError::InputTerminalReplyLimitExceeded);
                }
                bytes = Some(bytes::Bytes::copy_from_slice(value));
            }
        });
        Ok(FrameKind::InputTerminalReply {
            terminal_id: req(terminal_id)?,
            bytes: req(bytes)?,
        })
    }

    pub(super) fn decode_detached(&mut self) -> Result<FrameKind, DecodeError> {
        let mut reason: Option<DetachReason> = None;
        let mut message: Option<String> = None;
        for_each_field!(self, value, {
            field::detached::REASON => {
                let raw = sub!(value, Decoder::read_u8);
                // An unknown reason decodes as unstated, never an error.
                reason = DetachReason::from_wire(raw);
            }
            field::detached::MESSAGE => message = Some(utf8_value(value)?),
        });
        Ok(FrameKind::Detached {
            reason,
            // Absent message field = empty, per §7.2.
            message: message.unwrap_or_default(),
        })
    }
}
