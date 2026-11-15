//! Attach, input, viewport, detach, and error frame bodies.

use super::{
    BootstrapId, DecodeError, Decoder, DetachReason, ErrorCode, FrameKind,
    MAX_INPUT_TERMINAL_REPLY_BYTES, ResourceId, StreamId, decode_attach_target,
    decode_bootstrap_id, decode_client_id, decode_session_snapshot, decode_stream_id,
    decode_terminal_id, decode_viewport_info, field, req, utf8_value,
};

impl Decoder<'_> {
    pub(super) fn decode_attach(&mut self) -> Result<FrameKind, DecodeError> {
        use field::attach as f;
        let mut target: Option<crate::wire::frame::AttachTarget> = None;
        let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
        let mut request_scrollback = false;
        let mut scrollback_limit_lines = 0u32;
        let mut attach_id = None;
        let mut role_policy = None;
        for_each_field!(self, value, {
            f::ROLE_POLICY => {
                role_policy = Some(crate::wire::frame::RolePolicy::from_wire(sub!(
                    value,
                    Decoder::read_u8
                ))?);
            }
            f::TARGET => target = Some(sub!(value, decode_attach_target)),
            f::VIEWPORT => viewport = Some(sub!(value, decode_viewport_info)),
            f::REQUEST_SCROLLBACK => {
                request_scrollback = sub!(value, |d: &mut Decoder<'_>| d
                    .read_bool("Attach.request_scrollback"));
            }
            f::SCROLLBACK_LIMIT_LINES => scrollback_limit_lines = sub!(value, Decoder::read_u32_be),
            f::ATTACH_ID => attach_id = Some(sub!(value, Decoder::read_u32_be)),
        });
        Ok(FrameKind::Attach {
            attach_id: req(attach_id)?,
            target: req(target)?,
            viewport: req(viewport)?,
            request_scrollback,
            scrollback_limit_lines,
            role_policy,
        })
    }

    pub(super) fn decode_detach(&mut self) -> Result<FrameKind, DecodeError> {
        while self.read_field()?.is_some() {}
        Ok(FrameKind::Detach)
    }

    /// Decode the shared `INPUT_KEY` / `_MOUSE` / `_FOCUS` / `_PASTE` body:
    /// `TERMINAL_ID` (1) and a positional `EVENT` (2).
    pub(super) fn decode_input_event<E>(
        &mut self,
        decode_event: impl Fn(&mut Decoder<'_>) -> Result<E, DecodeError>,
    ) -> Result<(ResourceId, E), DecodeError> {
        let mut terminal_id = None;
        let mut event = None;
        for_each_field!(self, value, {
            field::input_key::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
            field::input_key::EVENT => event = Some(sub!(value, decode_event)),
        });
        Ok((req(terminal_id)?, req(event)?))
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

    pub(super) fn decode_clipboard_reply(&mut self) -> Result<FrameKind, DecodeError> {
        use field::clipboard_reply as f;
        let mut terminal_id: Option<ResourceId> = None;
        let mut selection: Option<u8> = None;
        let mut text: Option<String> = None;
        for_each_field!(self, value, {
            f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
            f::SELECTION => selection = Some(sub!(value, Decoder::read_u8)),
            f::TEXT => {
                if value.len() > crate::wire::frame::MAX_CLIPBOARD_TEXT {
                    return Err(DecodeError::LengthOverflow);
                }
                text = Some(utf8_value(value)?);
            }
        });
        Ok(FrameKind::ClipboardReply {
            terminal_id: req(terminal_id)?,
            selection: req(selection)?,
            text: crate::input::paste::ClipboardData(req(text)?),
        })
    }

    pub(super) fn decode_frame_ack(&mut self) -> Result<FrameKind, DecodeError> {
        use field::frame_ack as f;
        let mut terminal_id: Option<ResourceId> = None;
        let mut stream_id: Option<StreamId> = None;
        let mut bootstrap_id: Option<BootstrapId> = None;
        let mut seq: Option<u64> = None;
        for_each_field!(self, value, {
            f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
            f::SEQ => seq = Some(sub!(value, Decoder::read_u64_be)),
            f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
            f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
        });
        Ok(FrameKind::FrameAck {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            seq: req(seq)?,
        })
    }

    pub(super) fn decode_viewport_resize(&mut self) -> Result<FrameKind, DecodeError> {
        let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
        for_each_field!(self, value, {
            field::viewport_resize::VIEWPORT => viewport = Some(sub!(value, decode_viewport_info)),
        });
        Ok(FrameKind::ViewportResize {
            viewport: req(viewport)?,
        })
    }

    pub(super) fn decode_attached(&mut self) -> Result<FrameKind, DecodeError> {
        let mut snapshot: Option<crate::wire::info::SessionSnapshot> = None;
        let mut initial_client_id: Option<crate::ids::ClientId> = None;
        let mut attach_id = None;
        for_each_field!(self, value, {
            field::attached::SNAPSHOT => snapshot = Some(sub!(value, decode_session_snapshot)),
            field::attached::INITIAL_CLIENT_ID => {
                initial_client_id = Some(sub!(value, decode_client_id));
            }
            field::attached::ATTACH_ID => attach_id = Some(sub!(value, Decoder::read_u32_be)),
        });
        Ok(FrameKind::Attached {
            attach_id: req(attach_id)?,
            snapshot: req(snapshot)?,
            initial_client_id: req(initial_client_id)?,
        })
    }

    pub(super) fn decode_attach_ready(&mut self) -> Result<FrameKind, DecodeError> {
        let mut attach_id = None;
        for_each_field!(self, value, {
            field::attach_ready::ATTACH_ID => attach_id = Some(sub!(value, Decoder::read_u32_be)),
        });
        Ok(FrameKind::AttachReady {
            attach_id: req(attach_id)?,
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

    pub(super) fn decode_bell(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        for_each_field!(self, value, {
            field::bell::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
        });
        Ok(FrameKind::Bell {
            terminal_id: req(terminal_id)?,
        })
    }

    pub(super) fn decode_error(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id: Option<u32> = None;
        let mut code: Option<ErrorCode> = None;
        let mut message: Option<String> = None;
        for_each_field!(self, value, {
            field::error::REQUEST_ID => request_id = Some(sub!(value, Decoder::read_u32_be)),
            field::error::CODE => {
                code = Some(ErrorCode::from_wire_or_unknown(sub!(
                    value,
                    Decoder::read_u16_be
                )));
            }
            field::error::MESSAGE => message = Some(utf8_value(value)?),
        });
        Ok(FrameKind::Error {
            request_id,
            code: req(code)?,
            message: req(message)?,
        })
    }
}
