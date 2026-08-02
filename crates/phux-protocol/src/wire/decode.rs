//! Wire-frame decoder. Bounds-checked; never panics on malformed input.
//!
//! Owned by phux-6yl.4. See `docs/spec/proto.md` §5 (framing) and Appendix A
//! (primitives). Every decode method returns `Result` and refuses to read
//! past the end of the borrowed slice.

use super::error::DecodeError;
use super::field;
use super::frame::Scope;
use super::frame::{
    ErrorCode, FrameKind, MAX_FRAME_LEN, TYPE_ATTACH, TYPE_ATTACHED, TYPE_BELL, TYPE_COMMAND,
    TYPE_COMMAND_RESULT, TYPE_DELETE_METADATA, TYPE_DETACH, TYPE_DETACHED, TYPE_ERROR, TYPE_EVENT,
    TYPE_FRAME_ACK, TYPE_GET_METADATA, TYPE_HELLO, TYPE_HELLO_OK, TYPE_INPUT_FOCUS, TYPE_INPUT_KEY,
    TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_LIST_METADATA, TYPE_METADATA_CHANGED,
    TYPE_METADATA_KEYS, TYPE_METADATA_VALUE, TYPE_PING, TYPE_PONG, TYPE_SET_METADATA,
    TYPE_SPAWN_TERMINAL, TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA, TYPE_TERMINAL_CLOSED,
    TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_RESIZE, TYPE_TERMINAL_SNAPSHOT, TYPE_TERMINAL_SPAWNED,
    TYPE_VIEWPORT_RESIZE, decode_agent_event, decode_attach_target, decode_command,
    decode_command_result, decode_env, decode_focus_event, decode_key_event,
    decode_metadata_scope_key, decode_mouse_event, decode_paste_event, decode_scope,
    decode_spawn_result, decode_string_list, decode_terminal_id, decode_viewport_info,
};
use super::info::{decode_client_id, decode_session_snapshot};
use crate::ids::{GroupId, TerminalId};
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;

/// Decode a sub-record / leaf from a TLV field's value via a positional
/// [`Decoder`] bounded by the field's bytes.
///
/// The field value's bytes are the positional encoding of one logical field;
/// running a fresh `Decoder` over just that slice means a malformed nested
/// value cannot read past its field (the slice end bounds it), and an
/// over-declared inner list errors on EOF rather than over-reserving.
macro_rules! sub {
    ($value:expr, $body:expr) => {{
        let mut sub = Decoder::new($value);
        $body(&mut sub)?
    }};
}

/// Cursor-style decoder over an immutable byte slice.
///
/// The decoder borrows its input; `read_*` methods advance an internal
/// position. None of them panic on truncated or otherwise malformed input;
/// they return [`DecodeError`] instead.
#[derive(Debug)]
pub struct Decoder<'a> {
    input: &'a [u8],
    pos: usize,
    /// End offset of the current frame body, set by [`Self::read_frame`]
    /// once the length header is parsed. Lets variant decoders distinguish
    /// "ran out of this frame's body" (a backward-compat trailing field is
    /// absent) from "ran out of the whole buffer", without conflating a
    /// following frame's bytes with this frame's optional tail. `None`
    /// outside a framed decode (the boundary is unknown), in which case
    /// [`Self::at_body_end`] falls back to the input end.
    body_end: Option<usize>,
}

impl<'a> Decoder<'a> {
    /// Wrap `input` for primitive reads.
    #[must_use]
    pub const fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            body_end: None,
        }
    }

    /// Current read offset within the input.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Whether the cursor is at (or past) the end of the current frame
    /// body. A variant decoder consults this to decide whether an additive
    /// trailing field is present: `true` means the producer encoded a body
    /// that ended before this field, so the field defaults.
    ///
    /// Outside a framed decode (`body_end` unset) this falls back to the
    /// end of the borrowed input.
    #[must_use]
    pub fn at_body_end(&self) -> bool {
        self.pos >= self.body_end.unwrap_or(self.input.len())
    }

    /// Remaining (unread) bytes.
    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        &self.input[self.pos..]
    }

    /// Count of bytes remaining before the current frame-body boundary (or
    /// the input end when decoding outside a framed context).
    ///
    /// Used to bound pre-allocation: a length-prefixed list cannot contain
    /// more elements than there are remaining bytes, because every element
    /// occupies at least one byte on the wire. Reserving capacity larger than
    /// this is always wasted — and lets an attacker drive an unbounded
    /// `Vec::with_capacity` from a tiny frame (a decode-path denial of
    /// service). Callers
    /// clamp their declared element count to this value before reserving;
    /// the read loop still errors with [`DecodeError::UnexpectedEof`] if the
    /// declared count overshoots the bytes actually present.
    #[must_use]
    pub fn remaining_in_body(&self) -> usize {
        self.body_end
            .unwrap_or(self.input.len())
            .saturating_sub(self.pos)
    }

    /// Reserve capacity for a length-prefixed collection without trusting the
    /// declared `count` past what the remaining frame bytes could justify.
    ///
    /// Returns a `Vec` whose capacity is `min(count, remaining_bytes)`. The
    /// caller's read loop runs `count` iterations and surfaces
    /// [`DecodeError::UnexpectedEof`] when the input runs out, so an
    /// over-declared `count` still errors cleanly — it just no longer
    /// pre-reserves gigabytes for elements that cannot possibly be present.
    #[must_use]
    pub(crate) fn bounded_capacity<T>(&self, count: usize) -> Vec<T> {
        Vec::with_capacity(count.min(self.remaining_in_body()))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::LengthOverflow)?;
        if end > self.input.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        let slice = &self.input[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Read one unsigned byte.
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    /// Read a `u16` in network (big-endian) byte order.
    pub fn read_u16_be(&mut self) -> Result<u16, DecodeError> {
        let slice = self.take(2)?;
        // SAFETY-free: slice length verified by `take`.
        let arr: [u8; 2] = slice.try_into().map_err(|_| DecodeError::UnexpectedEof)?;
        Ok(u16::from_be_bytes(arr))
    }

    /// Read a `u32` in network (big-endian) byte order.
    pub fn read_u32_be(&mut self) -> Result<u32, DecodeError> {
        let slice = self.take(4)?;
        let arr: [u8; 4] = slice.try_into().map_err(|_| DecodeError::UnexpectedEof)?;
        Ok(u32::from_be_bytes(arr))
    }

    /// Read a `u64` in network (big-endian) byte order.
    pub fn read_u64_be(&mut self) -> Result<u64, DecodeError> {
        let slice = self.take(8)?;
        let arr: [u8; 8] = slice.try_into().map_err(|_| DecodeError::UnexpectedEof)?;
        Ok(u64::from_be_bytes(arr))
    }

    /// Read an `i64` in network (big-endian) byte order.
    ///
    /// Two's-complement decoding; pairs with
    /// [`super::encode::Encoder::write_i64_be`]. Used by
    /// `SessionInfo::created_at_unix_secs`.
    pub fn read_i64_be(&mut self) -> Result<i64, DecodeError> {
        let slice = self.take(8)?;
        let arr: [u8; 8] = slice.try_into().map_err(|_| DecodeError::UnexpectedEof)?;
        Ok(i64::from_be_bytes(arr))
    }

    /// Read an IEEE-754 `f32` in network (big-endian) byte order.
    ///
    /// Bit-for-bit decoding via [`f32::from_be_bytes`] — preserves NaNs and
    /// signed zeros. Pairs with [`super::encode::Encoder::write_f32_be`].
    pub fn read_f32_be(&mut self) -> Result<f32, DecodeError> {
        let slice = self.take(4)?;
        let arr: [u8; 4] = slice.try_into().map_err(|_| DecodeError::UnexpectedEof)?;
        Ok(f32::from_be_bytes(arr))
    }

    /// Read an IEEE-754 `f64` in network (big-endian) byte order.
    ///
    /// Bit-for-bit decoding via [`f64::from_be_bytes`] — preserves NaNs and
    /// signed zeros. Pairs with [`super::encode::Encoder::write_f64_be`].
    pub fn read_f64_be(&mut self) -> Result<f64, DecodeError> {
        let slice = self.take(8)?;
        let arr: [u8; 8] = slice.try_into().map_err(|_| DecodeError::UnexpectedEof)?;
        Ok(f64::from_be_bytes(arr))
    }

    /// Read a length-prefixed byte slice.
    ///
    /// The length prefix is a big-endian `u32`. Returns `LengthOverflow` if
    /// the declared length exceeds the remaining input or the protocol cap.
    pub fn read_bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.read_u32_be()?;
        if len > MAX_FRAME_LEN {
            return Err(DecodeError::LengthOverflow);
        }
        let len_usize = usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)?;
        self.take(len_usize)
    }

    /// Read a length-prefixed UTF-8 string.
    pub fn read_str(&mut self) -> Result<&'a str, DecodeError> {
        let bytes = self.read_bytes()?;
        core::str::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)
    }

    /// Read an unsigned LEB128 varint (`docs/spec/appendix-encoding.md`,
    /// `wire_type` `VARINT`). Pairs with
    /// [`super::encode::Encoder::write_varint`].
    ///
    /// Refuses a varint longer than ten bytes (the maximum a `u64` needs) with
    /// [`DecodeError::LengthOverflow`], so a malformed continuation run cannot
    /// spin or overflow. Truncated input surfaces as
    /// [`DecodeError::UnexpectedEof`].
    pub fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            // A u64 needs at most ten 7-bit groups; reject anything longer.
            if shift >= 64 {
                return Err(DecodeError::LengthOverflow);
            }
            let byte = self.read_u8()?;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Read one TLV field at the message-body level
    /// (`docs/spec/appendix-encoding.md` §1).
    ///
    /// Returns `Ok(None)` when the cursor is at the end of the current frame
    /// body (no more fields). Otherwise reads `field_id: varint`,
    /// `wire_type: u8`, and the field's **length-delimited value**
    /// (`varint length || bytes`), returning `(field_id, value_slice)`. Every
    /// wire type phux emits at the top level is length-delimited, so this one
    /// primitive both reads a known field and *skips* an unknown one — a
    /// caller that does not recognise `field_id` simply discards the returned
    /// slice and loops, which is the forward-compat "skip unknown fields by
    /// length" rule.
    ///
    /// The returned slice is bounded by the field's declared length and by the
    /// remaining frame body, so a nested positional decoder run over it cannot
    /// read past the field — and an over-declared length errors with
    /// [`DecodeError::UnexpectedEof`] rather than bleeding into the next field.
    pub fn read_field(&mut self) -> Result<Option<(u32, &'a [u8])>, DecodeError> {
        if self.at_body_end() {
            return Ok(None);
        }
        let field_id =
            u32::try_from(self.read_varint()?).map_err(|_| DecodeError::LengthOverflow)?;
        // The wire_type byte is informational at the top level: every field
        // phux emits is length-delimited, so the value is always
        // `varint length || bytes` and an unknown field skips by that length.
        let _wire_type = self.read_u8()?;
        let len = self.read_varint()?;
        if len > u64::from(MAX_FRAME_LEN) {
            return Err(DecodeError::LengthOverflow);
        }
        let len_usize = usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)?;
        let value = self.take(len_usize)?;
        Ok(Some((field_id, value)))
    }

    /// Read a complete wire frame from the current position. Returns the
    /// decoded frame and the unconsumed tail of the underlying input.
    ///
    /// The body is one big `match` over the SPEC §7 catalog, intentionally —
    /// keeping the dispatch table in one place trades length for locality.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    pub fn read_frame(&mut self) -> Result<(FrameKind, &'a [u8]), DecodeError> {
        // Length header: u32 big-endian, excludes itself, includes type byte.
        let length = self.read_u32_be()?;
        if !(1..=MAX_FRAME_LEN).contains(&length) {
            return Err(DecodeError::LengthOverflow);
        }
        let length_usize = usize::try_from(length).map_err(|_| DecodeError::LengthOverflow)?;

        // Carve out the frame body so trailing fields can be ignored cleanly.
        let body_start = self.pos;
        let body_end = body_start
            .checked_add(length_usize)
            .ok_or(DecodeError::LengthOverflow)?;
        if body_end > self.input.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        // Record the body boundary so variant decoders can detect absent
        // additive trailing fields (e.g. GET_SCREEN's `cells`) without
        // mistaking a following frame's bytes for this frame's tail.
        self.body_end = Some(body_end);

        let type_byte = self.read_u8()?;
        // Message bodies are field-tagged TLV (`docs/spec/appendix-encoding.md`):
        // each top-level field is `field_id || wire_type || length-delimited
        // value`, read by `read_field` which also skips an unrecognised
        // `field_id` by its length (forward-compat). Each arm below loops over
        // the body's fields, collecting them by id, then assembles the variant
        // applying documented defaults for absent optional/trailing fields. A
        // missing *required* field surfaces as `UnexpectedEof` (the body ended
        // before a field the message requires).
        let frame = match type_byte {
            TYPE_HELLO => {
                let mut client_name: Option<String> = None;
                let mut protocol_major = 0u16;
                let mut protocol_minor = 0u16;
                let mut protocol_patch = 0u16;
                let mut client_caps = crate::caps::ClientCapabilities::default();
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::hello::CLIENT_NAME => {
                            client_name = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        field::hello::PROTOCOL_MAJOR => {
                            protocol_major = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::hello::PROTOCOL_MINOR => {
                            protocol_minor = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::hello::PROTOCOL_PATCH => {
                            protocol_patch = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::hello::CLIENT_CAPS => {
                            // Caps blob: a prefix of color_support, layers,
                            // image_protocols, kbd_protocols, hyperlinks,
                            // output_mode, then an optional palette-present
                            // byte + foreground/background RGB. A shorter blob
                            // (older peer) leaves trailing caps at defaults.
                            let mut d = Decoder::new(value);
                            if !d.at_body_end() {
                                let cs = crate::caps::ColorSupport::from_wire(d.read_u8()?)
                                    .unwrap_or_default();
                                client_caps = client_caps.with_color_support(cs);
                            }
                            if !d.at_body_end() {
                                client_caps = client_caps
                                    .with_layers(crate::caps::LayerSet::from_wire(d.read_u8()?));
                            }
                            if !d.at_body_end() {
                                client_caps = client_caps.with_image_protocols(
                                    crate::caps::ImageProtocolSet::from_wire(d.read_u8()?),
                                );
                            }
                            if !d.at_body_end() {
                                client_caps = client_caps.with_kbd_protocols(
                                    crate::caps::KeyboardProtocolSet::from_wire(d.read_u8()?),
                                );
                            }
                            if !d.at_body_end() {
                                client_caps = client_caps.with_hyperlinks(d.read_u8()? != 0);
                            }
                            if !d.at_body_end() {
                                client_caps = client_caps.with_output_mode(
                                    crate::caps::OutputMode::from_wire(d.read_u8()?),
                                );
                            }
                            if !d.at_body_end() && d.read_u8()? != 0 {
                                let foreground = crate::caps::TerminalColor {
                                    r: d.read_u8()?,
                                    g: d.read_u8()?,
                                    b: d.read_u8()?,
                                };
                                let background = crate::caps::TerminalColor {
                                    r: d.read_u8()?,
                                    g: d.read_u8()?,
                                    b: d.read_u8()?,
                                };
                                client_caps = client_caps.with_default_colors(
                                    crate::caps::TerminalDefaultColors {
                                        foreground,
                                        background,
                                    },
                                );
                            }
                        }
                        _ => {}
                    }
                }
                FrameKind::Hello {
                    client_name: client_name.ok_or(DecodeError::UnexpectedEof)?,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    client_caps,
                }
            }
            TYPE_HELLO_OK => {
                let mut protocol_major = 0u16;
                let mut protocol_minor = 0u16;
                let mut protocol_patch = 0u16;
                let mut server_caps = crate::caps::ServerCapabilities::default();
                let mut server_id: Vec<u8> = Vec::new();
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::hello_ok::PROTOCOL_MAJOR => {
                            protocol_major = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::hello_ok::PROTOCOL_MINOR => {
                            protocol_minor = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::hello_ok::PROTOCOL_PATCH => {
                            protocol_patch = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::hello_ok::SERVER_CAPS => {
                            let mut d = Decoder::new(value);
                            if !d.at_body_end() {
                                server_caps = server_caps
                                    .with_layers(crate::caps::LayerSet::from_wire(d.read_u8()?));
                            }
                            if !d.at_body_end() {
                                server_caps = server_caps.with_features(
                                    crate::caps::ServerFeatureSet::from_wire(d.read_u32_be()?),
                                );
                            }
                        }
                        field::hello_ok::SERVER_ID => server_id = value.to_vec(),
                        _ => {}
                    }
                }
                FrameKind::HelloOk {
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    server_caps,
                    server_id,
                }
            }
            TYPE_PING => {
                let mut nonce: Option<u64> = None;
                while let Some((id, value)) = self.read_field()? {
                    if id == field::ping::NONCE {
                        nonce = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
                    }
                }
                FrameKind::Ping {
                    nonce: nonce.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_PONG => {
                let mut nonce: Option<u64> = None;
                while let Some((id, value)) = self.read_field()? {
                    if id == field::ping::NONCE {
                        nonce = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
                    }
                }
                FrameKind::Pong {
                    nonce: nonce.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_TERMINAL_OUTPUT => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut seq = 0u64;
                let mut bytes = bytes::Bytes::new();
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::terminal_output::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::terminal_output::SEQ => {
                            seq = sub!(value, |d: &mut Decoder<'_>| d.read_u64_be());
                        }
                        field::terminal_output::BYTES => {
                            bytes = bytes::Bytes::copy_from_slice(value);
                        }
                        _ => {}
                    }
                }
                FrameKind::TerminalOutput {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    seq,
                    bytes,
                }
            }
            TYPE_ATTACH => {
                let mut target: Option<crate::wire::frame::AttachTarget> = None;
                let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
                let mut request_scrollback = false;
                let mut scrollback_limit_lines = 0u32;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::attach::TARGET => target = Some(sub!(value, decode_attach_target)),
                        field::attach::VIEWPORT => {
                            viewport = Some(sub!(value, decode_viewport_info));
                        }
                        field::attach::REQUEST_SCROLLBACK => {
                            request_scrollback =
                                sub!(value, |d: &mut Decoder<'_>| d.read_u8()) != 0;
                        }
                        field::attach::SCROLLBACK_LIMIT_LINES => {
                            scrollback_limit_lines =
                                sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        _ => {}
                    }
                }
                FrameKind::Attach {
                    target: target.ok_or(DecodeError::UnexpectedEof)?,
                    viewport: viewport.ok_or(DecodeError::UnexpectedEof)?,
                    request_scrollback,
                    scrollback_limit_lines,
                }
            }
            TYPE_DETACH => {
                while self.read_field()?.is_some() {}
                FrameKind::Detach
            }
            TYPE_INPUT_KEY => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut event: Option<KeyEvent> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::input_key::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::input_key::EVENT => event = Some(sub!(value, decode_key_event)),
                        _ => {}
                    }
                }
                FrameKind::InputKey {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    event: event.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_INPUT_MOUSE => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut event: Option<MouseEvent> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::input_mouse::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::input_mouse::EVENT => event = Some(sub!(value, decode_mouse_event)),
                        _ => {}
                    }
                }
                FrameKind::InputMouse {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    event: event.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_INPUT_FOCUS => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut event: Option<FocusEvent> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::input_focus::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::input_focus::EVENT => {
                            let tag = sub!(value, |d: &mut Decoder<'_>| d.read_u8());
                            event = Some(decode_focus_event(tag)?);
                        }
                        _ => {}
                    }
                }
                FrameKind::InputFocus {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    event: event.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_INPUT_PASTE => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut event: Option<crate::input::paste::PasteEvent> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::input_paste::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::input_paste::EVENT => event = Some(sub!(value, decode_paste_event)),
                        _ => {}
                    }
                }
                FrameKind::InputPaste {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    event: event.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_FRAME_ACK => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut seq = 0u64;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::frame_ack::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::frame_ack::SEQ => {
                            seq = sub!(value, |d: &mut Decoder<'_>| d.read_u64_be());
                        }
                        _ => {}
                    }
                }
                FrameKind::FrameAck {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    seq,
                }
            }
            TYPE_VIEWPORT_RESIZE => {
                let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
                while let Some((id, value)) = self.read_field()? {
                    if id == field::viewport_resize::VIEWPORT {
                        viewport = Some(sub!(value, decode_viewport_info));
                    }
                }
                FrameKind::ViewportResize {
                    viewport: viewport.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_ATTACHED => {
                let mut snapshot: Option<crate::wire::info::SessionSnapshot> = None;
                let mut initial_client_id: Option<crate::ids::ClientId> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::attached::SNAPSHOT => {
                            snapshot = Some(sub!(value, decode_session_snapshot));
                        }
                        field::attached::INITIAL_CLIENT_ID => {
                            initial_client_id = Some(sub!(value, decode_client_id));
                        }
                        _ => {}
                    }
                }
                FrameKind::Attached {
                    snapshot: snapshot.ok_or(DecodeError::UnexpectedEof)?,
                    initial_client_id: initial_client_id.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_TERMINAL_SNAPSHOT => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut cols = 0u16;
                let mut rows = 0u16;
                let mut vt_replay_bytes: Vec<u8> = Vec::new();
                let mut scrollback_bytes: Option<Vec<u8>> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::terminal_snapshot::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::terminal_snapshot::COLS => {
                            cols = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::terminal_snapshot::ROWS => {
                            rows = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::terminal_snapshot::VT_REPLAY_BYTES => {
                            vt_replay_bytes = value.to_vec();
                        }
                        field::terminal_snapshot::SCROLLBACK_BYTES => {
                            scrollback_bytes = Some(value.to_vec());
                        }
                        _ => {}
                    }
                }
                FrameKind::TerminalSnapshot {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    cols,
                    rows,
                    vt_replay_bytes,
                    scrollback_bytes,
                }
            }
            TYPE_DETACHED => {
                while self.read_field()?.is_some() {}
                FrameKind::Detached
            }
            TYPE_BELL => {
                let mut terminal_id: Option<TerminalId> = None;
                while let Some((id, value)) = self.read_field()? {
                    if id == field::bell::TERMINAL_ID {
                        terminal_id = Some(sub!(value, decode_terminal_id));
                    }
                }
                FrameKind::Bell {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_ERROR => {
                let mut request_id: Option<u32> = None;
                let mut code: Option<ErrorCode> = None;
                let mut message: Option<String> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::error::REQUEST_ID => {
                            request_id = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                        }
                        field::error::CODE => {
                            let raw = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                            code = Some(ErrorCode::from_wire(raw).ok_or_else(|| {
                                DecodeError::UnknownEnumValue {
                                    field: "ErrorCode",
                                    value: u32::from(raw),
                                }
                            })?);
                        }
                        field::error::MESSAGE => {
                            message = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        _ => {}
                    }
                }
                FrameKind::Error {
                    request_id,
                    code: code.ok_or(DecodeError::UnexpectedEof)?,
                    message: message.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_GET_METADATA => {
                let (request_id, scope, key) = decode_metadata_scope_key(self)?;
                FrameKind::GetMetadata {
                    request_id,
                    scope,
                    key,
                }
            }
            TYPE_SET_METADATA => {
                let mut request_id = 0u32;
                let mut scope: Option<Scope> = None;
                let mut key: Option<String> = None;
                let mut value_bytes: Vec<u8> = Vec::new();
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::set_metadata::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::set_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                        field::set_metadata::KEY => {
                            key = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        field::set_metadata::VALUE => value_bytes = value.to_vec(),
                        _ => {}
                    }
                }
                FrameKind::SetMetadata {
                    request_id,
                    scope: scope.ok_or(DecodeError::UnexpectedEof)?,
                    key: key.ok_or(DecodeError::UnexpectedEof)?,
                    value: value_bytes,
                }
            }
            TYPE_DELETE_METADATA => {
                let (request_id, scope, key) = decode_metadata_scope_key(self)?;
                FrameKind::DeleteMetadata {
                    request_id,
                    scope,
                    key,
                }
            }
            TYPE_LIST_METADATA => {
                let mut request_id = 0u32;
                let mut scope: Option<Scope> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::list_metadata::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::list_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                        _ => {}
                    }
                }
                FrameKind::ListMetadata {
                    request_id,
                    scope: scope.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_SUBSCRIBE_METADATA => {
                let mut scope: Option<Scope> = None;
                let mut key: Option<String> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::subscribe_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                        field::subscribe_metadata::KEY => {
                            key = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        _ => {}
                    }
                }
                FrameKind::SubscribeMetadata {
                    scope: scope.ok_or(DecodeError::UnexpectedEof)?,
                    key: key.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_METADATA_CHANGED => {
                let mut scope: Option<Scope> = None;
                let mut key: Option<String> = None;
                let mut value_bytes: Option<Vec<u8>> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::metadata_changed::SCOPE => scope = Some(sub!(value, decode_scope)),
                        field::metadata_changed::KEY => {
                            key = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        field::metadata_changed::VALUE => value_bytes = Some(value.to_vec()),
                        _ => {}
                    }
                }
                FrameKind::MetadataChanged {
                    scope: scope.ok_or(DecodeError::UnexpectedEof)?,
                    key: key.ok_or(DecodeError::UnexpectedEof)?,
                    value: value_bytes,
                }
            }
            TYPE_METADATA_VALUE => {
                let mut request_id = 0u32;
                let mut value_bytes: Option<Vec<u8>> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::metadata_value::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::metadata_value::VALUE => value_bytes = Some(value.to_vec()),
                        _ => {}
                    }
                }
                FrameKind::MetadataValue {
                    request_id,
                    value: value_bytes,
                }
            }
            TYPE_METADATA_KEYS => {
                let mut request_id = 0u32;
                let mut keys: Vec<String> = Vec::new();
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::metadata_keys::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::metadata_keys::KEYS => {
                            let mut d = Decoder::new(value);
                            let count = d.read_u32_be()?;
                            let count_usize =
                                usize::try_from(count).map_err(|_| DecodeError::LengthOverflow)?;
                            let mut out = d.bounded_capacity(count_usize);
                            for _ in 0..count_usize {
                                out.push(d.read_str()?.to_owned());
                            }
                            keys = out;
                        }
                        _ => {}
                    }
                }
                FrameKind::MetadataKeys { request_id, keys }
            }
            TYPE_SPAWN_TERMINAL => {
                let mut request_id = 0u32;
                let mut group = GroupId::new(0);
                let mut command: Option<Vec<String>> = None;
                let mut cwd: Option<String> = None;
                let mut env: Option<Vec<(String, String)>> = None;
                let mut term: Option<String> = None;
                let mut satellite: Option<crate::ids::SatelliteHost> = None;
                let mut owner_terminal: Option<crate::ids::TerminalId> = None;
                let mut agent_session: Option<Vec<u8>> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::spawn_terminal::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::spawn_terminal::GROUP => {
                            group =
                                GroupId::new(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                        }
                        field::spawn_terminal::COMMAND => {
                            command = Some(sub!(value, decode_string_list));
                        }
                        field::spawn_terminal::CWD => {
                            cwd = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        field::spawn_terminal::ENV => env = Some(sub!(value, decode_env)),
                        field::spawn_terminal::TERM => {
                            term = Some(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?
                                    .to_owned(),
                            );
                        }
                        field::spawn_terminal::SATELLITE => {
                            satellite = Some(crate::ids::SatelliteHost::new(
                                core::str::from_utf8(value)
                                    .map_err(|_| DecodeError::InvalidUtf8)?,
                            ));
                        }
                        field::spawn_terminal::OWNER_TERMINAL => {
                            owner_terminal = Some(sub!(value, decode_terminal_id));
                        }
                        field::spawn_terminal::AGENT_SESSION => {
                            agent_session = Some(value.to_vec());
                        }
                        _ => {}
                    }
                }
                FrameKind::SpawnTerminal {
                    request_id,
                    group,
                    command,
                    cwd,
                    env,
                    term,
                    satellite,
                    owner_terminal,
                    agent_session,
                }
            }
            TYPE_TERMINAL_SPAWNED => {
                let mut request_id = 0u32;
                let mut result: Option<crate::wire::frame::SpawnResult> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::terminal_spawned::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::terminal_spawned::RESULT => {
                            result = Some(sub!(value, decode_spawn_result));
                        }
                        _ => {}
                    }
                }
                FrameKind::TerminalSpawned {
                    request_id,
                    result: result.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_TERMINAL_CLOSED => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut exit_status: Option<i32> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::terminal_closed::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::terminal_closed::EXIT_STATUS => {
                            let bits = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                            exit_status = Some(i32::from_be_bytes(bits.to_be_bytes()));
                        }
                        _ => {}
                    }
                }
                FrameKind::TerminalClosed {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    exit_status,
                }
            }
            TYPE_TERMINAL_RESIZE => {
                let mut terminal_id: Option<TerminalId> = None;
                let mut cols = 0u16;
                let mut rows = 0u16;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::terminal_resize::TERMINAL_ID => {
                            terminal_id = Some(sub!(value, decode_terminal_id));
                        }
                        field::terminal_resize::COLS => {
                            cols = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        field::terminal_resize::ROWS => {
                            rows = sub!(value, |d: &mut Decoder<'_>| d.read_u16_be());
                        }
                        _ => {}
                    }
                }
                FrameKind::TerminalResize {
                    terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
                    cols,
                    rows,
                }
            }
            TYPE_COMMAND => {
                let mut request_id = 0u32;
                let mut command: Option<crate::wire::frame::Command> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::command::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::command::COMMAND => command = Some(sub!(value, decode_command)),
                        _ => {}
                    }
                }
                FrameKind::Command {
                    request_id,
                    command: command.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_COMMAND_RESULT => {
                let mut request_id = 0u32;
                let mut result: Option<crate::wire::frame::CommandResult> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::command_result::REQUEST_ID => {
                            request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                        }
                        field::command_result::RESULT => {
                            result = Some(sub!(value, decode_command_result));
                        }
                        _ => {}
                    }
                }
                FrameKind::CommandResult {
                    request_id,
                    result: result.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            TYPE_SUBSCRIBE_EVENTS => {
                let mut terminal: Option<TerminalId> = None;
                while let Some((id, value)) = self.read_field()? {
                    if id == field::subscribe_events::TERMINAL {
                        terminal = Some(sub!(value, decode_terminal_id));
                    }
                }
                FrameKind::SubscribeEvents { terminal }
            }
            TYPE_EVENT => {
                let mut terminal: Option<TerminalId> = None;
                let mut event: Option<crate::wire::frame::AgentEvent> = None;
                while let Some((id, value)) = self.read_field()? {
                    match id {
                        field::event::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                        field::event::EVENT => event = Some(sub!(value, decode_agent_event)),
                        _ => {}
                    }
                }
                FrameKind::Event {
                    terminal,
                    event: event.ok_or(DecodeError::UnexpectedEof)?,
                }
            }
            other => {
                return Err(DecodeError::UnknownFrameKind {
                    tag: u16::from(other),
                });
            }
        };

        // Trailing fields the decoder didn't consume MUST be skipped per
        // SPEC §6 ("skip them by length"). Advance to the declared end.
        if self.pos > body_end {
            // The frame body claimed N bytes but the variant read more —
            // means the encoder produced a longer body than the length
            // header advertised. Treat as malformed.
            return Err(DecodeError::LengthOverflow);
        }
        self.pos = body_end;

        Ok((frame, self.remaining()))
    }
}
