//! Wire-frame decoder. Bounds-checked; never panics on malformed input.
//!
//! Owned by phux-6yl.4. See `docs/spec/proto.md` §5 (framing) and Appendix A
//! (primitives). Every decode method returns `Result` and refuses to read
//! past the end of the borrowed slice.

use super::error::DecodeError;
use super::field;
use super::frame::Scope;
use super::frame::{
    CloseReason, DetachReason, ErrorCode, FrameKind, HistoryRejectionReason,
    HistoryTombstoneReason, MAX_FRAME_LEN, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_ROWS,
    MAX_INPUT_TERMINAL_REPLY_BYTES, MAX_RESOURCE_NATIVE_ID_BYTES, MAX_RESOURCE_PROVIDER_BYTES,
    SpawnResource, TYPE_ATTACH, TYPE_ATTACH_READY, TYPE_ATTACHED, TYPE_BELL, TYPE_BOOTSTRAP_BEGIN,
    TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY, TYPE_BOOTSTRAP_TOMBSTONE, TYPE_COMMAND,
    TYPE_COMMAND_RESULT, TYPE_DELETE_METADATA, TYPE_DETACH, TYPE_DETACHED, TYPE_DIRECTORY_LISTING,
    TYPE_ERROR, TYPE_EVENT, TYPE_FRAME_ACK, TYPE_FRAME_COMPRESSED, TYPE_GET_METADATA, TYPE_HELLO,
    TYPE_HELLO_OK, TYPE_HISTORY_PAGE, TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST,
    TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_FOCUS, TYPE_INPUT_KEY, TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE,
    TYPE_INPUT_TERMINAL_REPLY, TYPE_LIST_DIRECTORY, TYPE_LIST_METADATA, TYPE_METADATA_CHANGED,
    TYPE_METADATA_KEYS, TYPE_METADATA_VALUE, TYPE_MOVE_RESOURCE, TYPE_PING, TYPE_PONG,
    TYPE_RESIZE_TERMINAL, TYPE_RESOURCE_CLOSED, TYPE_RESOURCE_MOVED, TYPE_RESOURCE_OUTPUT,
    TYPE_RESOURCE_SPAWNED, TYPE_SET_METADATA, TYPE_SPAWN_RESOURCE, TYPE_SUBSCRIBE_EVENTS,
    TYPE_SUBSCRIBE_METADATA, TYPE_VIEWPORT_RESIZE, TombstoneReason, decode_agent_event,
    decode_attach_target, decode_bootstrap_codec, decode_bootstrap_id, decode_bootstrap_profile,
    decode_bootstrap_stream_profile, decode_command, decode_command_result,
    decode_directory_listing, decode_env, decode_focus_event, decode_key_event,
    decode_list_directory, decode_metadata_scope_key, decode_mouse_event, decode_move_result,
    decode_paste_event, decode_scope, decode_spawn_result, decode_stream_id, decode_string_list,
    decode_terminal_id, decode_viewport_info,
};
use super::info::{decode_client_id, decode_session_snapshot};
use crate::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfileSet, EngineCodecSet, EngineFeatureSet,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES,
};
use crate::ids::{BootstrapId, GroupId, ResourceId, ResourceKind, StreamId};
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
    /// End offset of the current frame body.
    body_end: Option<usize>,
    /// Connection-negotiated maximum `BOOTSTRAP_CHUNK.payload` bytes.
    max_bootstrap_chunk_bytes: u32,
    /// Connection-negotiated maximum `HISTORY_PAGE.payload` bytes.
    max_history_page_bytes: u32,
}

impl<'a> Decoder<'a> {
    /// Wrap `input` for primitive reads.
    #[must_use]
    pub const fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            pos: 0,
            body_end: None,
            max_bootstrap_chunk_bytes: MAX_BOOTSTRAP_CHUNK_BYTES,
            max_history_page_bytes: MAX_HISTORY_PAGE_BYTES,
        }
    }

    /// Wrap `input` with the payload limits negotiated in `HELLO_OK`.
    ///
    /// The decoder checks a payload's borrowed TLV slice length against these
    /// limits before copying it into owned [`bytes::Bytes`].
    #[must_use]
    pub const fn with_bootstrap_limits(input: &'a [u8], limits: BootstrapLimits) -> Self {
        Self {
            input,
            pos: 0,
            body_end: None,
            max_bootstrap_chunk_bytes: limits.max_chunk_bytes(),
            max_history_page_bytes: limits.max_history_page_bytes(),
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
    /// Three parts: the length/bounds prologue below, a one-line-per-frame
    /// dispatch over the SPEC §7 catalog in `decode_body`, and the
    /// trailing-field epilogue. Every catalog entry's body decoder is a
    /// private `decode_*` method further down this file, and those methods
    /// stay in catalog order so the dispatch still reads as the spec table.
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
        let frame = self.decode_body(type_byte)?;

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

    /// Decode one frame body, dispatching on its SPEC §7 type byte.
    ///
    /// Message bodies are field-tagged TLV (`docs/spec/appendix-encoding.md`):
    /// each top-level field is `field_id || wire_type || length-delimited
    /// value`, read by `read_field` which also skips an unrecognised
    /// `field_id` by its length (forward-compat). Each decoder below loops
    /// over the body's fields, collecting them by id, then assembles the
    /// variant applying documented defaults for absent optional/trailing
    /// fields. A missing *required* field surfaces as `UnexpectedEof` (the
    /// body ended before a field the message requires).
    fn decode_body(&mut self, type_byte: u8) -> Result<FrameKind, DecodeError> {
        match type_byte {
            TYPE_HELLO => self.decode_hello(),
            TYPE_HELLO_OK => self.decode_hello_ok(),
            TYPE_PING => self.decode_ping(),
            TYPE_PONG => self.decode_pong(),
            TYPE_RESOURCE_OUTPUT => self.decode_terminal_output(),
            TYPE_ATTACH => self.decode_attach(),
            TYPE_DETACH => self.decode_detach(),
            TYPE_INPUT_KEY => self.decode_input_key(),
            TYPE_INPUT_MOUSE => self.decode_input_mouse(),
            TYPE_INPUT_FOCUS => self.decode_input_focus(),
            TYPE_INPUT_PASTE => self.decode_input_paste(),
            TYPE_INPUT_TERMINAL_REPLY => self.decode_input_terminal_reply(),
            TYPE_FRAME_ACK => self.decode_frame_ack(),
            TYPE_VIEWPORT_RESIZE => self.decode_viewport_resize(),
            TYPE_ATTACHED => self.decode_attached(),
            TYPE_ATTACH_READY => self.decode_attach_ready(),
            TYPE_BOOTSTRAP_BEGIN => self.decode_bootstrap_begin(),
            TYPE_BOOTSTRAP_CHUNK => self.decode_bootstrap_chunk(),
            TYPE_BOOTSTRAP_READY => self.decode_bootstrap_ready(),
            TYPE_HISTORY_REQUEST => self.decode_history_request(),
            TYPE_HISTORY_PAGE => self.decode_history_page(),
            TYPE_BOOTSTRAP_TOMBSTONE => self.decode_bootstrap_tombstone(),
            TYPE_HISTORY_TOMBSTONE => self.decode_history_tombstone(),
            TYPE_HISTORY_REJECTED => self.decode_history_rejected(),
            TYPE_FRAME_COMPRESSED => self.decode_frame_compressed(),
            TYPE_DETACHED => self.decode_detached(),
            TYPE_BELL => self.decode_bell(),
            TYPE_ERROR => self.decode_error(),
            TYPE_GET_METADATA => self.decode_get_metadata(),
            TYPE_SET_METADATA => self.decode_set_metadata(),
            TYPE_DELETE_METADATA => self.decode_delete_metadata(),
            TYPE_LIST_METADATA => self.decode_list_metadata(),
            TYPE_SUBSCRIBE_METADATA => self.decode_subscribe_metadata(),
            TYPE_METADATA_CHANGED => self.decode_metadata_changed(),
            TYPE_METADATA_VALUE => self.decode_metadata_value(),
            TYPE_METADATA_KEYS => self.decode_metadata_keys(),
            TYPE_LIST_DIRECTORY => {
                let (request_id, path) = decode_list_directory(self)?;
                Ok(FrameKind::ListDirectory { request_id, path })
            }
            TYPE_DIRECTORY_LISTING => {
                let (request_id, result) = decode_directory_listing(self)?;
                Ok(FrameKind::DirectoryListing { request_id, result })
            }
            TYPE_SPAWN_RESOURCE => self.decode_spawn_terminal(),
            TYPE_RESOURCE_SPAWNED => self.decode_terminal_spawned(),
            TYPE_MOVE_RESOURCE => self.decode_move_terminal(),
            TYPE_RESOURCE_MOVED => self.decode_terminal_moved(),
            TYPE_RESOURCE_CLOSED => self.decode_terminal_closed(),
            TYPE_RESIZE_TERMINAL => self.decode_terminal_resize(),
            TYPE_COMMAND => self.decode_command(),
            TYPE_COMMAND_RESULT => self.decode_command_result(),
            TYPE_SUBSCRIBE_EVENTS => self.decode_subscribe_events(),
            TYPE_EVENT => self.decode_event(),
            other => Err(DecodeError::UnknownFrameKind {
                tag: u16::from(other),
            }),
        }
    }

    /// Decode a `HELLO` message body into [`FrameKind::Hello`].
    fn decode_hello(&mut self) -> Result<FrameKind, DecodeError> {
        let mut client_name: Option<String> = None;
        let mut protocol_major = None;
        let mut protocol_minor = None;
        let mut protocol_patch = None;
        let mut client_caps: Option<crate::caps::ClientCapabilities> = None;
        let mut compression: Option<crate::caps::CompressionSet> = None;
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
                    protocol_major = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::hello::PROTOCOL_MINOR => {
                    protocol_minor = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::hello::PROTOCOL_PATCH => {
                    protocol_patch = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::hello::CLIENT_CAPS => {
                    client_caps = Some(sub!(value, decode_client_capabilities));
                }
                field::hello::COMPRESSION => {
                    compression = Some(crate::caps::CompressionSet::from_bits(sub!(
                        value,
                        |d: &mut Decoder<'_>| d.read_u8()
                    )));
                }
                _ => {}
            }
        }
        let mut client_caps = client_caps.ok_or(DecodeError::UnexpectedEof)?;
        // The offer rides beside the frozen `CLIENT_CAPS` sub-record on the
        // wire (§6.2 fixes that record's byte order) but belongs with the
        // rest of the client's capabilities in the typed view, so fold it in.
        // Absent means the empty set, which `ClientCapabilities::new` already
        // installed.
        if let Some(compression) = compression {
            client_caps = client_caps.with_compression(compression);
        }
        Ok(FrameKind::Hello {
            client_name: client_name.ok_or(DecodeError::UnexpectedEof)?,
            protocol_major: protocol_major.ok_or(DecodeError::UnexpectedEof)?,
            protocol_minor: protocol_minor.ok_or(DecodeError::UnexpectedEof)?,
            protocol_patch: protocol_patch.ok_or(DecodeError::UnexpectedEof)?,
            client_caps,
        })
    }

    /// Decode a `HELLO_OK` message body into [`FrameKind::HelloOk`].
    fn decode_hello_ok(&mut self) -> Result<FrameKind, DecodeError> {
        let mut protocol_major = None;
        let mut protocol_minor = None;
        let mut protocol_patch = None;
        let mut server_caps = None;
        let mut server_id = None;
        let mut selected_profile = None;
        let mut max_chunk_bytes = None;
        let mut max_history_page_bytes = None;
        let mut compression: Option<crate::caps::Compression> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::hello_ok::PROTOCOL_MAJOR => {
                    protocol_major = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::hello_ok::PROTOCOL_MINOR => {
                    protocol_minor = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::hello_ok::PROTOCOL_PATCH => {
                    protocol_patch = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::hello_ok::SERVER_CAPS => {
                    server_caps = Some(sub!(value, decode_server_capabilities));
                }
                field::hello_ok::SERVER_ID => server_id = Some(value.to_vec()),
                field::hello_ok::SELECTED_PROFILE => {
                    selected_profile = Some(sub!(value, decode_bootstrap_profile));
                }
                field::hello_ok::MAX_CHUNK_BYTES => {
                    max_chunk_bytes = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                field::hello_ok::MAX_HISTORY_PAGE_BYTES => {
                    max_history_page_bytes =
                        Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                field::hello_ok::COMPRESSION => {
                    compression = Some(crate::caps::Compression::from_u8(sub!(
                        value,
                        |d: &mut Decoder<'_>| d.read_u8()
                    )));
                }
                _ => {}
            }
        }
        let bootstrap_limits =
            negotiated_bootstrap_limits(max_chunk_bytes, max_history_page_bytes)?;
        let mut server_caps = server_caps.ok_or(DecodeError::UnexpectedEof)?;
        // Same fold as HELLO's offer: additive top-level field on the wire,
        // one capability struct in the typed view.
        if let Some(compression) = compression {
            server_caps = server_caps.with_compression(compression);
        }
        Ok(FrameKind::HelloOk {
            protocol_major: protocol_major.ok_or(DecodeError::UnexpectedEof)?,
            protocol_minor: protocol_minor.ok_or(DecodeError::UnexpectedEof)?,
            protocol_patch: protocol_patch.ok_or(DecodeError::UnexpectedEof)?,
            server_caps,
            server_id: server_id.ok_or(DecodeError::UnexpectedEof)?,
            selected_profile: selected_profile.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_limits,
        })
    }

    /// Inflate a `FRAME_COMPRESSED` envelope and dispatch the frame inside it.
    ///
    /// The envelope is decode-invisible: the inner frame is reconstructed
    /// byte-for-byte and then run through the same [`Self::decode_body`]
    /// dispatch it would have taken uncompressed, carrying this decoder's
    /// negotiated bootstrap bounds with it so a wrapped `BOOTSTRAP_CHUNK` is
    /// still rejected above its limit. Callers therefore never learn whether a
    /// frame arrived compressed, which is the point.
    ///
    /// Two things are refused outright. A declared inflated length above
    /// [`MAX_FRAME_LEN`] is rejected **before** allocating, so an envelope
    /// cannot ask the receiver for more memory than a legal frame; and a
    /// nested envelope is rejected, so a hostile sender cannot drive
    /// unbounded recursion or amplification through repeated wrapping.
    /// The largest inflated frame body this connection will allocate for a
    /// `FRAME_COMPRESSED` envelope (`docs/spec/proto.md` §6.4).
    ///
    /// The envelope's `uncompressed_len` is a number the *sender* picked, and
    /// the receiver allocates it before inflating anything. Bounding it by the
    /// §5 frame cap alone would let a peer spend a few hundred bytes to make
    /// this side allocate 16 MiB, over and over. The bound is therefore the
    /// larger of the two payload limits **this connection actually
    /// negotiated**, plus one chunk of envelope allowance for the inner
    /// frame's own ids, cursors and length prefixes — because wrapping is only
    /// specified for the two payload-bearing frames whose sizes those limits
    /// govern. With the reference advertisements that is ~1 MiB rather than
    /// 16 MiB, and it shrinks further for a peer that negotiated smaller
    /// bounds, which is exactly the peer least able to absorb the allocation.
    ///
    /// It is a ceiling, not a shape check: a legal wrapped frame is always
    /// under it, so this rejects only senders outside the contract.
    fn max_compressed_frame_bytes(&self) -> u32 {
        /// Room for the inner frame's type byte, field ids, ids, and opaque
        /// cursors sitting alongside its payload. `HISTORY_PAGE` carries the
        /// most: two cursors at `MAX_HISTORY_CURSOR_BYTES` each, plus ids.
        const ENVELOPE_ALLOWANCE: u32 = 64 * 1024;

        self.max_bootstrap_chunk_bytes
            .max(self.max_history_page_bytes)
            .saturating_add(ENVELOPE_ALLOWANCE)
            .min(MAX_FRAME_LEN)
    }

    fn decode_frame_compressed(&mut self) -> Result<FrameKind, DecodeError> {
        let mut algorithm = None;
        let mut uncompressed_len = None;
        let mut payload: Option<&[u8]> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::frame_compressed::ALGORITHM => {
                    algorithm = Some(crate::caps::Compression::from_u8(sub!(
                        value,
                        |d: &mut Decoder<'_>| d.read_u8()
                    )));
                }
                field::frame_compressed::UNCOMPRESSED_LEN => {
                    uncompressed_len = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                field::frame_compressed::PAYLOAD => payload = Some(value),
                _ => {}
            }
        }
        if algorithm.ok_or(DecodeError::UnexpectedEof)? != crate::caps::Compression::Deflate {
            return Err(DecodeError::CompressedFrameInvalid);
        }
        let declared = uncompressed_len.ok_or(DecodeError::UnexpectedEof)?;
        if declared == 0 || declared > self.max_compressed_frame_bytes() {
            return Err(DecodeError::LengthOverflow);
        }
        let declared = usize::try_from(declared).map_err(|_| DecodeError::LengthOverflow)?;
        let payload = payload.ok_or(DecodeError::UnexpectedEof)?;
        let body = crate::wire::compress::inflate(payload, declared)?;

        let mut inner = Decoder::with_bootstrap_limits(
            &body,
            BootstrapLimits::new(self.max_bootstrap_chunk_bytes, self.max_history_page_bytes)
                .unwrap_or_default(),
        );
        // The inner buffer is exactly one frame body, so its end is the end of
        // the buffer. Setting it is what lets the inner variant decoders tell
        // an absent additive trailing field from the start of another frame.
        inner.body_end = Some(body.len());
        let type_byte = inner.read_u8()?;
        if type_byte == TYPE_FRAME_COMPRESSED {
            return Err(DecodeError::CompressedFrameInvalid);
        }
        inner.decode_body(type_byte)
    }

    /// Decode a `PING` message body into [`FrameKind::Ping`].
    fn decode_ping(&mut self) -> Result<FrameKind, DecodeError> {
        let mut nonce: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::ping::NONCE {
                nonce = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
            }
        }
        Ok(FrameKind::Ping {
            nonce: nonce.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `PONG` message body into [`FrameKind::Pong`].
    fn decode_pong(&mut self) -> Result<FrameKind, DecodeError> {
        let mut nonce: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::ping::NONCE {
                nonce = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
            }
        }
        Ok(FrameKind::Pong {
            nonce: nonce.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `RESOURCE_OUTPUT` message body into [`FrameKind::ResourceOutput`].
    fn decode_terminal_output(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut stream_id: Option<StreamId> = None;
        let mut bootstrap_id: Option<BootstrapId> = None;
        let mut seq: Option<u64> = None;
        let mut bytes: Option<bytes::Bytes> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_output::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::terminal_output::SEQ => {
                    seq = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
                }
                field::terminal_output::BYTES => {
                    bytes = Some(bytes::Bytes::copy_from_slice(value));
                }
                field::terminal_output::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::terminal_output::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                _ => {}
            }
        }
        Ok(FrameKind::ResourceOutput {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            seq: seq.ok_or(DecodeError::UnexpectedEof)?,
            bytes: bytes.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `ATTACH` message body into [`FrameKind::Attach`].
    fn decode_attach(&mut self) -> Result<FrameKind, DecodeError> {
        let mut target: Option<crate::wire::frame::AttachTarget> = None;
        let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
        let mut request_scrollback = false;
        let mut scrollback_limit_lines = 0u32;
        let mut attach_id = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::attach::TARGET => target = Some(sub!(value, decode_attach_target)),
                field::attach::VIEWPORT => {
                    viewport = Some(sub!(value, decode_viewport_info));
                }
                field::attach::REQUEST_SCROLLBACK => {
                    request_scrollback = sub!(value, |d: &mut Decoder<'_>| d.read_u8()) != 0;
                }
                field::attach::SCROLLBACK_LIMIT_LINES => {
                    scrollback_limit_lines = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                }
                field::attach::ATTACH_ID => {
                    attach_id = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                _ => {}
            }
        }
        Ok(FrameKind::Attach {
            attach_id: attach_id.ok_or(DecodeError::UnexpectedEof)?,
            target: target.ok_or(DecodeError::UnexpectedEof)?,
            viewport: viewport.ok_or(DecodeError::UnexpectedEof)?,
            request_scrollback,
            scrollback_limit_lines,
        })
    }

    /// Decode a `DETACH` message body into [`FrameKind::Detach`].
    fn decode_detach(&mut self) -> Result<FrameKind, DecodeError> {
        while self.read_field()?.is_some() {}
        Ok(FrameKind::Detach)
    }

    /// Decode an `INPUT_KEY` message body into [`FrameKind::InputKey`].
    fn decode_input_key(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
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
        Ok(FrameKind::InputKey {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            event: event.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `INPUT_MOUSE` message body into [`FrameKind::InputMouse`].
    fn decode_input_mouse(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
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
        Ok(FrameKind::InputMouse {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            event: event.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `INPUT_FOCUS` message body into [`FrameKind::InputFocus`].
    fn decode_input_focus(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
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
        Ok(FrameKind::InputFocus {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            event: event.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `INPUT_PASTE` message body into [`FrameKind::InputPaste`].
    fn decode_input_paste(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
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
        Ok(FrameKind::InputPaste {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            event: event.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `INPUT_TERMINAL_REPLY` message body into [`FrameKind::InputTerminalReply`].
    fn decode_input_terminal_reply(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut bytes: Option<bytes::Bytes> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::input_terminal_reply::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::input_terminal_reply::BYTES => {
                    if value.is_empty() || value.len() > MAX_INPUT_TERMINAL_REPLY_BYTES {
                        return Err(DecodeError::InputTerminalReplyLimitExceeded);
                    }
                    bytes = Some(bytes::Bytes::copy_from_slice(value));
                }
                _ => {}
            }
        }
        Ok(FrameKind::InputTerminalReply {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            bytes: bytes.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `FRAME_ACK` message body into [`FrameKind::FrameAck`].
    fn decode_frame_ack(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut stream_id: Option<StreamId> = None;
        let mut bootstrap_id: Option<BootstrapId> = None;
        let mut seq: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::frame_ack::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::frame_ack::SEQ => {
                    seq = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
                }
                field::frame_ack::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::frame_ack::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                _ => {}
            }
        }
        Ok(FrameKind::FrameAck {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            seq: seq.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `VIEWPORT_RESIZE` message body into [`FrameKind::ViewportResize`].
    fn decode_viewport_resize(&mut self) -> Result<FrameKind, DecodeError> {
        let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::viewport_resize::VIEWPORT {
                viewport = Some(sub!(value, decode_viewport_info));
            }
        }
        Ok(FrameKind::ViewportResize {
            viewport: viewport.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `ATTACHED` message body into [`FrameKind::Attached`].
    fn decode_attached(&mut self) -> Result<FrameKind, DecodeError> {
        let mut snapshot: Option<crate::wire::info::SessionSnapshot> = None;
        let mut initial_client_id: Option<crate::ids::ClientId> = None;
        let mut attach_id = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::attached::SNAPSHOT => {
                    snapshot = Some(sub!(value, decode_session_snapshot));
                }
                field::attached::INITIAL_CLIENT_ID => {
                    initial_client_id = Some(sub!(value, decode_client_id));
                }
                field::attached::ATTACH_ID => {
                    attach_id = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                _ => {}
            }
        }
        Ok(FrameKind::Attached {
            attach_id: attach_id.ok_or(DecodeError::UnexpectedEof)?,
            snapshot: snapshot.ok_or(DecodeError::UnexpectedEof)?,
            initial_client_id: initial_client_id.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `ATTACH_READY` message body into [`FrameKind::AttachReady`].
    fn decode_attach_ready(&mut self) -> Result<FrameKind, DecodeError> {
        let mut attach_id = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::attach_ready::ATTACH_ID {
                attach_id = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
            }
        }
        Ok(FrameKind::AttachReady {
            attach_id: attach_id.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `BOOTSTRAP_BEGIN` message body into [`FrameKind::BootstrapBegin`].
    fn decode_bootstrap_begin(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut codec = None;
        let mut cols = None;
        let mut rows = None;
        let mut output_mode = None;
        let mut base_seq = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::bootstrap_begin::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::bootstrap_begin::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::bootstrap_begin::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::bootstrap_begin::CODEC => {
                    codec = Some(sub!(value, decode_bootstrap_codec));
                }
                field::bootstrap_begin::COLS => {
                    cols = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::bootstrap_begin::ROWS => {
                    rows = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u16_be()));
                }
                field::bootstrap_begin::OUTPUT_MODE => {
                    output_mode = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u8()));
                }
                field::bootstrap_begin::BASE_SEQ => {
                    base_seq = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
                }
                _ => {}
            }
        }
        let profile = decode_bootstrap_stream_profile(
            codec.ok_or(DecodeError::UnexpectedEof)?,
            output_mode.ok_or(DecodeError::UnexpectedEof)?,
        )?;
        let (cols, rows) = checked_bootstrap_dimensions(profile, cols, rows)?;
        Ok(FrameKind::BootstrapBegin {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            profile,
            cols,
            rows,
            base_seq: base_seq.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `BOOTSTRAP_CHUNK` message body into [`FrameKind::BootstrapChunk`].
    fn decode_bootstrap_chunk(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut chunk_seq = None;
        let mut payload = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::bootstrap_chunk::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::bootstrap_chunk::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::bootstrap_chunk::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::bootstrap_chunk::CHUNK_SEQ => {
                    chunk_seq = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                field::bootstrap_chunk::PAYLOAD => {
                    if value.len() > self.max_bootstrap_chunk_bytes as usize {
                        return Err(DecodeError::BootstrapLimitExceeded);
                    }
                    payload = Some(bytes::Bytes::copy_from_slice(value));
                }
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            chunk_seq: chunk_seq.ok_or(DecodeError::UnexpectedEof)?,
            payload: payload.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `BOOTSTRAP_READY` message body into [`FrameKind::BootstrapReady`].
    fn decode_bootstrap_ready(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut history_cursor = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::bootstrap_ready::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::bootstrap_ready::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::bootstrap_ready::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::bootstrap_ready::HISTORY_CURSOR => {
                    history_cursor = Some(checked_history_cursor(value)?);
                }
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapReady {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            history_cursor,
        })
    }

    /// Decode a `HISTORY_REQUEST` message body into [`FrameKind::HistoryRequest`].
    fn decode_history_request(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut max_bytes = None;
        let mut max_rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::history_request::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::history_request::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::history_request::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::history_request::CURSOR => {
                    cursor = Some(checked_history_cursor(value)?);
                }
                field::history_request::MAX_BYTES => {
                    max_bytes = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                field::history_request::MAX_ROWS => {
                    max_rows = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                _ => {}
            }
        }
        let max_bytes = max_bytes.ok_or(DecodeError::UnexpectedEof)?;
        let max_rows = max_rows.ok_or(DecodeError::UnexpectedEof)?;
        Ok(FrameKind::HistoryRequest {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            cursor: cursor.ok_or(DecodeError::UnexpectedEof)?,
            max_bytes,
            max_rows,
        })
    }

    /// Decode a `HISTORY_PAGE` message body into [`FrameKind::HistoryPage`].
    ///
    /// Retain borrowed fields until every scalar and bound has been
    /// validated. A producer may place the opaque payload before a
    /// required scalar, so copying during the TLV scan would let a
    /// malformed frame allocate up to the negotiated page limit.
    fn decode_history_page(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut page_seq = None;
        let mut cursor = None;
        let mut next_cursor = None;
        let mut payload = None;
        let mut rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::history_page::TERMINAL_ID => terminal_id = Some(value),
                field::history_page::STREAM_ID => stream_id = Some(value),
                field::history_page::BOOTSTRAP_ID => bootstrap_id = Some(value),
                field::history_page::CURSOR => cursor = Some(value),
                field::history_page::NEXT_CURSOR => next_cursor = Some(value),
                field::history_page::PAYLOAD => payload = Some(value),
                field::history_page::PAGE_SEQ => page_seq = Some(value),
                field::history_page::ROWS => rows = Some(value),
                _ => {}
            }
        }

        let stream_id = sub!(
            stream_id.ok_or(DecodeError::UnexpectedEof)?,
            decode_stream_id
        );
        let bootstrap_id = sub!(
            bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            decode_bootstrap_id
        );
        let page_seq = checked_history_page_seq(page_seq)?;
        let rows = checked_history_page_rows(rows)?;

        let payload = payload.ok_or(DecodeError::UnexpectedEof)?;
        if payload.len() > self.max_history_page_bytes as usize {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        let cursor = cursor.ok_or(DecodeError::UnexpectedEof)?;
        if cursor.len() > MAX_HISTORY_CURSOR_BYTES {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        if next_cursor.is_some_and(|next| next.len() > MAX_HISTORY_CURSOR_BYTES) {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        let terminal_id = sub!(
            terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            decode_terminal_id
        );

        Ok(FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor: bytes::Bytes::copy_from_slice(cursor),
            next_cursor: next_cursor.map(bytes::Bytes::copy_from_slice),
            payload: bytes::Bytes::copy_from_slice(payload),
            rows,
        })
    }

    /// Decode a `BOOTSTRAP_TOMBSTONE` message body into [`FrameKind::BootstrapTombstone`].
    fn decode_bootstrap_tombstone(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut reason = None;
        let mut last_valid_seq = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::bootstrap_tombstone::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::bootstrap_tombstone::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::bootstrap_tombstone::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::bootstrap_tombstone::REASON => {
                    let value = sub!(value, |d: &mut Decoder<'_>| d.read_u8());
                    reason = Some(TombstoneReason::from_wire(value).ok_or_else(|| {
                        DecodeError::UnknownEnumValue {
                            field: "TombstoneReason",
                            value: u32::from(value),
                        }
                    })?);
                }
                field::bootstrap_tombstone::LAST_VALID_SEQ => {
                    last_valid_seq = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u64_be()));
                }
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapTombstone {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            reason: reason.ok_or(DecodeError::UnexpectedEof)?,
            last_valid_seq: last_valid_seq.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `HISTORY_TOMBSTONE` message body into [`FrameKind::HistoryTombstone`].
    fn decode_history_tombstone(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut reason = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::history_tombstone::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::history_tombstone::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::history_tombstone::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::history_tombstone::CURSOR => {
                    cursor = Some(checked_history_cursor(value)?);
                }
                field::history_tombstone::REASON => {
                    let value = sub!(value, |d: &mut Decoder<'_>| d.read_u8());
                    reason = Some(HistoryTombstoneReason::from_wire(value).ok_or_else(|| {
                        DecodeError::UnknownEnumValue {
                            field: "HistoryTombstoneReason",
                            value: u32::from(value),
                        }
                    })?);
                }
                _ => {}
            }
        }
        Ok(FrameKind::HistoryTombstone {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            cursor: cursor.ok_or(DecodeError::UnexpectedEof)?,
            reason: reason.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Validate a `HISTORY_REJECTED` body's required `required_bytes` retry
    /// hint against the limit negotiated for this connection.
    fn checked_history_required_bytes(
        &self,
        required_bytes: Option<u32>,
    ) -> Result<u32, DecodeError> {
        let required_bytes = required_bytes.ok_or(DecodeError::UnexpectedEof)?;
        if required_bytes == 0 || required_bytes > self.max_history_page_bytes {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        Ok(required_bytes)
    }

    /// Decode a `HISTORY_REJECTED` message body into [`FrameKind::HistoryRejected`].
    fn decode_history_rejected(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut reason = None;
        let mut required_bytes = None;
        let mut required_rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::history_rejected::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::history_rejected::STREAM_ID => {
                    stream_id = Some(sub!(value, decode_stream_id));
                }
                field::history_rejected::BOOTSTRAP_ID => {
                    bootstrap_id = Some(sub!(value, decode_bootstrap_id));
                }
                field::history_rejected::CURSOR => {
                    cursor = Some(checked_history_cursor(value)?);
                }
                field::history_rejected::REASON => {
                    let value = sub!(value, |d: &mut Decoder<'_>| d.read_u8());
                    reason = Some(HistoryRejectionReason::from_wire(value).ok_or_else(|| {
                        DecodeError::UnknownEnumValue {
                            field: "HistoryRejectionReason",
                            value: u32::from(value),
                        }
                    })?);
                }
                field::history_rejected::REQUIRED_BYTES => {
                    required_bytes = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                field::history_rejected::REQUIRED_ROWS => {
                    required_rows = Some(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
                }
                _ => {}
            }
        }
        let required_bytes = self.checked_history_required_bytes(required_bytes)?;
        let required_rows = checked_history_required_rows(required_rows)?;
        Ok(FrameKind::HistoryRejected {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            stream_id: stream_id.ok_or(DecodeError::UnexpectedEof)?,
            bootstrap_id: bootstrap_id.ok_or(DecodeError::UnexpectedEof)?,
            cursor: cursor.ok_or(DecodeError::UnexpectedEof)?,
            reason: reason.ok_or(DecodeError::UnexpectedEof)?,
            required_bytes,
            required_rows,
        })
    }

    /// Decode a `DETACHED` message body into [`FrameKind::Detached`].
    fn decode_detached(&mut self) -> Result<FrameKind, DecodeError> {
        let mut reason: Option<DetachReason> = None;
        let mut message: Option<String> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::detached::REASON => {
                    let raw = sub!(value, |d: &mut Decoder<'_>| d.read_u8());
                    // Deliberately tolerant: an unrecognised reason
                    // decodes as "unstated" rather than failing the
                    // frame. See `DetachReason::from_wire`.
                    reason = DetachReason::from_wire(raw);
                }
                field::detached::MESSAGE => {
                    message = Some(
                        core::str::from_utf8(value)
                            .map_err(|_| DecodeError::InvalidUtf8)?
                            .to_owned(),
                    );
                }
                _ => {}
            }
        }
        Ok(FrameKind::Detached {
            reason,
            // Absent message field = empty, per §7.2.
            message: message.unwrap_or_default(),
        })
    }

    /// Decode a `BELL` message body into [`FrameKind::Bell`].
    fn decode_bell(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::bell::TERMINAL_ID {
                terminal_id = Some(sub!(value, decode_terminal_id));
            }
        }
        Ok(FrameKind::Bell {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode an `ERROR` message body into [`FrameKind::Error`].
    fn decode_error(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::Error {
            request_id,
            code: code.ok_or(DecodeError::UnexpectedEof)?,
            message: message.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `GET_METADATA` message body into [`FrameKind::GetMetadata`].
    fn decode_get_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let (request_id, scope, key) = decode_metadata_scope_key(self)?;
        Ok(FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        })
    }

    /// Decode a `SET_METADATA` message body into [`FrameKind::SetMetadata`].
    fn decode_set_metadata(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::SetMetadata {
            request_id,
            scope: scope.ok_or(DecodeError::UnexpectedEof)?,
            key: key.ok_or(DecodeError::UnexpectedEof)?,
            value: value_bytes,
        })
    }

    /// Decode a `DELETE_METADATA` message body into [`FrameKind::DeleteMetadata`].
    fn decode_delete_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let (request_id, scope, key) = decode_metadata_scope_key(self)?;
        Ok(FrameKind::DeleteMetadata {
            request_id,
            scope,
            key,
        })
    }

    /// Decode a `LIST_METADATA` message body into [`FrameKind::ListMetadata`].
    fn decode_list_metadata(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::ListMetadata {
            request_id,
            scope: scope.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `SUBSCRIBE_METADATA` message body into [`FrameKind::SubscribeMetadata`].
    fn decode_subscribe_metadata(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::SubscribeMetadata {
            scope: scope.ok_or(DecodeError::UnexpectedEof)?,
            key: key.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `METADATA_CHANGED` message body into [`FrameKind::MetadataChanged`].
    fn decode_metadata_changed(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::MetadataChanged {
            scope: scope.ok_or(DecodeError::UnexpectedEof)?,
            key: key.ok_or(DecodeError::UnexpectedEof)?,
            value: value_bytes,
        })
    }

    /// Decode a `METADATA_VALUE` message body into [`FrameKind::MetadataValue`].
    fn decode_metadata_value(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::MetadataValue {
            request_id,
            value: value_bytes,
        })
    }

    /// Decode a `METADATA_KEYS` message body into [`FrameKind::MetadataKeys`].
    fn decode_metadata_keys(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::MetadataKeys { request_id, keys })
    }

    /// Decode a `SPAWN_RESOURCE` message body into [`FrameKind::SpawnResource`].
    fn decode_spawn_terminal(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut group = GroupId::new(0);
        let mut command: Option<Vec<String>> = None;
        let mut cwd: Option<String> = None;
        let mut env: Option<Vec<(String, String)>> = None;
        let mut term: Option<String> = None;
        let mut satellite: Option<crate::ids::SatelliteHost> = None;
        let mut owner_terminal: Option<crate::ids::ResourceId> = None;
        let mut agent_session: Option<Vec<u8>> = None;
        let mut initial_size: Option<(u16, u16)> = None;
        let mut kind = ResourceKind::Terminal;
        let mut parent: Option<ResourceId> = None;
        let mut provider: Option<String> = None;
        let mut native_id: Option<String> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::spawn_terminal::REQUEST_ID => {
                    request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                }
                field::spawn_terminal::GROUP => {
                    group = GroupId::new(sub!(value, |d: &mut Decoder<'_>| d.read_u32_be()));
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
                        core::str::from_utf8(value).map_err(|_| DecodeError::InvalidUtf8)?,
                    ));
                }
                field::spawn_terminal::OWNER_TERMINAL => {
                    owner_terminal = Some(sub!(value, decode_terminal_id));
                }
                field::spawn_terminal::AGENT_SESSION => {
                    agent_session = Some(value.to_vec());
                }
                field::spawn_terminal::INITIAL_SIZE => {
                    initial_size = Some(sub!(value, |d: &mut Decoder<'_>| {
                        let cols = d.read_u16_be()?;
                        let rows = d.read_u16_be()?;
                        Ok((cols, rows))
                    }));
                }
                field::spawn_terminal::KIND => {
                    kind = ResourceKind::from_wire(sub!(value, |d: &mut Decoder<'_>| d.read_u8()));
                }
                field::spawn_terminal::PARENT => {
                    parent = Some(sub!(value, decode_terminal_id));
                }
                field::spawn_terminal::PROVIDER => {
                    provider = Some(decode_agent_facet_str(value, MAX_RESOURCE_PROVIDER_BYTES)?);
                }
                field::spawn_terminal::NATIVE_ID => {
                    native_id = Some(decode_agent_facet_str(value, MAX_RESOURCE_NATIVE_ID_BYTES)?);
                }
                _ => {}
            }
        }
        let resource = SpawnResource {
            kind,
            parent,
            provider,
            native_id,
        };
        // A body carrying none of fields 11-14 is the plain Terminal spawn,
        // and so is one that spells the defaults out; both decode to `None`
        // so the value is canonical and re-encodes to the pre-kind bytes.
        let resource = (!resource.is_default()).then(|| Box::new(resource));
        let frame = FrameKind::SpawnResource {
            request_id,
            group,
            command,
            cwd,
            env,
            term,
            satellite,
            owner_terminal,
            agent_session,
            initial_size,
            resource,
        };
        validate_spawn_for_kind(&frame)?;
        Ok(frame)
    }

    /// Decode a `RESOURCE_SPAWNED` message body into [`FrameKind::ResourceSpawned`].
    fn decode_terminal_spawned(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::ResourceSpawned {
            request_id,
            result: result.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `MOVE_RESOURCE` message body into [`FrameKind::MoveResource`].
    fn decode_move_terminal(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut terminal: Option<ResourceId> = None;
        let mut owner_terminal: Option<ResourceId> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::move_terminal::REQUEST_ID => {
                    request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                }
                field::move_terminal::TERMINAL => {
                    terminal = Some(sub!(value, decode_terminal_id));
                }
                field::move_terminal::OWNER_TERMINAL => {
                    owner_terminal = Some(sub!(value, decode_terminal_id));
                }
                _ => {}
            }
        }
        Ok(FrameKind::MoveResource {
            request_id,
            terminal: terminal.ok_or(DecodeError::UnexpectedEof)?,
            owner_terminal: owner_terminal.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `RESOURCE_MOVED` message body into [`FrameKind::ResourceMoved`].
    fn decode_terminal_moved(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::MoveResult> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_moved::REQUEST_ID => {
                    request_id = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                }
                field::terminal_moved::RESULT => {
                    result = Some(sub!(value, decode_move_result));
                }
                _ => {}
            }
        }
        Ok(FrameKind::ResourceMoved {
            request_id,
            result: result.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `RESOURCE_CLOSED` message body into [`FrameKind::ResourceClosed`].
    fn decode_terminal_closed(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut exit_status: Option<i32> = None;
        let mut reason = CloseReason::Unknown;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_closed::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::terminal_closed::EXIT_STATUS => {
                    let bits = sub!(value, |d: &mut Decoder<'_>| d.read_u32_be());
                    exit_status = Some(i32::from_be_bytes(bits.to_be_bytes()));
                }
                field::terminal_closed::REASON => {
                    reason = CloseReason::from_wire(sub!(value, |d: &mut Decoder<'_>| d.read_u8()));
                }
                _ => {}
            }
        }
        Ok(FrameKind::ResourceClosed {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            exit_status,
            reason,
        })
    }

    /// Decode a `RESIZE_TERMINAL` message body into [`FrameKind::ResizeTerminal`].
    fn decode_terminal_resize(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
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
        Ok(FrameKind::ResizeTerminal {
            terminal_id: terminal_id.ok_or(DecodeError::UnexpectedEof)?,
            cols,
            rows,
        })
    }

    /// Decode a `COMMAND` message body into [`FrameKind::Command`].
    fn decode_command(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::Command {
            request_id,
            command: command.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `COMMAND_RESULT` message body into [`FrameKind::CommandResult`].
    fn decode_command_result(&mut self) -> Result<FrameKind, DecodeError> {
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
        Ok(FrameKind::CommandResult {
            request_id,
            result: result.ok_or(DecodeError::UnexpectedEof)?,
        })
    }

    /// Decode a `SUBSCRIBE_EVENTS` message body into [`FrameKind::SubscribeEvents`].
    fn decode_subscribe_events(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal: Option<ResourceId> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::subscribe_events::TERMINAL {
                terminal = Some(sub!(value, decode_terminal_id));
            }
        }
        Ok(FrameKind::SubscribeEvents { terminal })
    }

    /// Decode an `EVENT` message body into [`FrameKind::Event`].
    fn decode_event(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal: Option<ResourceId> = None;
        let mut event: Option<crate::wire::frame::AgentEvent> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::event::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                field::event::EVENT => event = Some(sub!(value, decode_agent_event)),
                _ => {}
            }
        }
        Ok(FrameKind::Event {
            terminal,
            event: event.ok_or(DecodeError::UnexpectedEof)?,
        })
    }
}

/// Decode one of `SPAWN_RESOURCE`'s agent-facet strings (`provider`,
/// `native_id`), refusing an empty value or one over `max_bytes` before the
/// bytes are copied out of the frame.
fn decode_agent_facet_str(value: &[u8], max_bytes: usize) -> Result<String, DecodeError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(DecodeError::AgentFacetLimitExceeded);
    }
    Ok(core::str::from_utf8(value)
        .map_err(|_| DecodeError::InvalidUtf8)?
        .to_owned())
}

/// Enforce `SPAWN_RESOURCE`'s per-kind field rules (`docs/spec/L1.md` §3.1).
///
/// A `Terminal` spawn carries none of the child-binding / agent-facet fields
/// (12-14). An `AgentSession` spawn requires `parent` (12) and `provider`
/// (13) and carries none of the process-shape fields `command` (3), `cwd`
/// (4), `env` (5), `term` (6), or `initial_size` (10), which describe a PTY
/// it does not have, nor `owner_terminal` (8), since no window places it.
/// An `Unknown` kind is passed through unvalidated so the server can answer
/// `SpawnError::UnsupportedKind` instead of the connection failing on a
/// malformed frame.
fn validate_spawn_for_kind(frame: &FrameKind) -> Result<(), DecodeError> {
    let FrameKind::SpawnResource {
        command,
        cwd,
        env,
        term,
        owner_terminal,
        initial_size,
        resource,
        ..
    } = frame
    else {
        return Ok(());
    };
    // `None` is the plain Terminal spawn, which carries nothing to check.
    let Some(resource) = resource.as_deref() else {
        return Ok(());
    };
    let SpawnResource {
        kind,
        parent,
        provider,
        native_id,
    } = resource;
    let rule = |field: u32, required: bool| DecodeError::InvalidSpawnForKind {
        kind: kind.as_wire(),
        field,
        required,
    };
    match kind {
        ResourceKind::Terminal => {
            for (field, present) in [
                (field::spawn_terminal::PARENT, parent.is_some()),
                (field::spawn_terminal::PROVIDER, provider.is_some()),
                (field::spawn_terminal::NATIVE_ID, native_id.is_some()),
            ] {
                if present {
                    return Err(rule(field, false));
                }
            }
        }
        ResourceKind::AgentSession => {
            for (field, present) in [
                (field::spawn_terminal::PARENT, parent.is_some()),
                (field::spawn_terminal::PROVIDER, provider.is_some()),
            ] {
                if !present {
                    return Err(rule(field, true));
                }
            }
            for (field, present) in [
                (field::spawn_terminal::COMMAND, command.is_some()),
                (field::spawn_terminal::CWD, cwd.is_some()),
                (field::spawn_terminal::ENV, env.is_some()),
                (field::spawn_terminal::TERM, term.is_some()),
                (
                    field::spawn_terminal::OWNER_TERMINAL,
                    owner_terminal.is_some(),
                ),
                (field::spawn_terminal::INITIAL_SIZE, initial_size.is_some()),
            ] {
                if present {
                    return Err(rule(field, false));
                }
            }
        }
        ResourceKind::Unknown { .. } => {}
    }
    Ok(())
}

/// Decode a `HELLO` frame's `client_caps` field value.
///
/// Positional, in wire order: color support, the layer / image / keyboard
/// protocol sets, the hyperlink flag, the output mode, the optional default
/// palette, then the bootstrap capability block.
fn decode_client_capabilities(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::ClientCapabilities, DecodeError> {
    let color_support = decode_color_support(d)?;
    let layers = crate::caps::LayerSet::from_wire(d.read_u8()?);
    let images = crate::caps::ImageProtocolSet::from_wire(d.read_u8()?);
    let keyboards = crate::caps::KeyboardProtocolSet::from_wire(d.read_u8()?);
    let hyperlinks = decode_hyperlinks_flag(d)?;
    let output_mode = decode_output_mode(d)?;
    let default_colors = decode_default_colors(d)?;
    let bootstrap = decode_bootstrap_capabilities(d)?;
    let mut caps = crate::caps::ClientCapabilities::new()
        .with_color_support(color_support)
        .with_layers(layers)
        .with_image_protocols(images)
        .with_kbd_protocols(keyboards)
        .with_hyperlinks(hyperlinks)
        .with_output_mode(output_mode)
        .with_bootstrap(bootstrap);
    if let Some(colors) = default_colors {
        caps = caps.with_default_colors(colors);
    }
    Ok(caps)
}

/// Decode the color-support byte of a client capability block.
fn decode_color_support(d: &mut Decoder<'_>) -> Result<crate::caps::ColorSupport, DecodeError> {
    let color_value = d.read_u8()?;
    crate::caps::ColorSupport::from_wire(color_value).ok_or_else(|| DecodeError::UnknownEnumValue {
        field: "ColorSupport",
        value: u32::from(color_value),
    })
}

/// Decode the hyperlink-support flag of a client capability block.
fn decode_hyperlinks_flag(d: &mut Decoder<'_>) -> Result<bool, DecodeError> {
    match d.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(DecodeError::UnknownEnumValue {
            field: "hyperlinks",
            value: u32::from(value),
        }),
    }
}

/// Decode the output-mode byte of a client capability block.
fn decode_output_mode(d: &mut Decoder<'_>) -> Result<crate::caps::OutputMode, DecodeError> {
    let output_mode_tag = d.read_u8()?;
    match output_mode_tag {
        0 => Ok(crate::caps::OutputMode::Raw),
        1 => Ok(crate::caps::OutputMode::StateSync),
        value => Err(DecodeError::UnknownEnumValue {
            field: "OutputMode",
            value: u32::from(value),
        }),
    }
}

/// Decode the presence-tagged default foreground/background palette of a
/// client capability block.
fn decode_default_colors(
    d: &mut Decoder<'_>,
) -> Result<Option<crate::caps::TerminalDefaultColors>, DecodeError> {
    let palette_tag = d.read_u8()?;
    match palette_tag {
        0 => Ok(None),
        1 => Ok(Some(crate::caps::TerminalDefaultColors {
            foreground: crate::caps::TerminalColor {
                r: d.read_u8()?,
                g: d.read_u8()?,
                b: d.read_u8()?,
            },
            background: crate::caps::TerminalColor {
                r: d.read_u8()?,
                g: d.read_u8()?,
                b: d.read_u8()?,
            },
        })),
        value => Err(DecodeError::UnknownEnumValue {
            field: "default_colors presence",
            value: u32::from(value),
        }),
    }
}

/// Decode the bootstrap capability block of a client capability field.
fn decode_bootstrap_capabilities(
    d: &mut Decoder<'_>,
) -> Result<BootstrapCapabilities, DecodeError> {
    let profiles = BootstrapProfileSet::from_wire(d.read_u8()?);
    let native_codecs = EngineCodecSet::from_wire(d.read_u64_be()?);
    let native_features = EngineFeatureSet::from_wire(d.read_u32_be()?);
    let max_chunk_bytes = d.read_u32_be()?;
    let max_history_page_bytes = d.read_u32_be()?;
    let limits = BootstrapLimits::new(max_chunk_bytes, max_history_page_bytes)
        .ok_or(DecodeError::BootstrapLimitExceeded)?;
    Ok(BootstrapCapabilities {
        profiles,
        native_codecs,
        native_features,
        limits,
    })
}

/// Decode a `HELLO_OK` frame's `server_caps` field value.
///
/// The feature set is an additive trailing field: a producer that ended the
/// capability block before it leaves the default (empty) set.
fn decode_server_capabilities(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::ServerCapabilities, DecodeError> {
    let mut caps = crate::caps::ServerCapabilities::new()
        .with_layers(crate::caps::LayerSet::from_wire(d.read_u8()?));
    if !d.at_body_end() {
        caps = caps.with_features(crate::caps::ServerFeatureSet::from_wire(d.read_u32_be()?));
    }
    Ok(caps)
}

/// Rebuild the payload limits a `HELLO_OK` body negotiated. Both bounds are
/// required fields, and a pair the limit type rejects is
/// [`DecodeError::BootstrapLimitExceeded`].
fn negotiated_bootstrap_limits(
    max_chunk_bytes: Option<u32>,
    max_history_page_bytes: Option<u32>,
) -> Result<BootstrapLimits, DecodeError> {
    BootstrapLimits::new(
        max_chunk_bytes.ok_or(DecodeError::UnexpectedEof)?,
        max_history_page_bytes.ok_or(DecodeError::UnexpectedEof)?,
    )
    .ok_or(DecodeError::BootstrapLimitExceeded)
}

/// Copy a history cursor field value, refusing one longer than
/// [`MAX_HISTORY_CURSOR_BYTES`].
fn checked_history_cursor(value: &[u8]) -> Result<bytes::Bytes, DecodeError> {
    if value.len() > MAX_HISTORY_CURSOR_BYTES {
        return Err(DecodeError::BootstrapLimitExceeded);
    }
    Ok(bytes::Bytes::copy_from_slice(value))
}

/// Validate a `BOOTSTRAP_BEGIN` body's required grid dimensions against its
/// stream profile. A Terminal stream (native or synthesized VT) replays into
/// a grid, so a zero column or row count is not a profile it can replay; an
/// `AgentEventsJsonlV1` stream has no grid and carries the `0 x 0` sentinel,
/// so a non-zero geometry there is equally malformed.
fn checked_bootstrap_dimensions(
    profile: crate::caps::BootstrapStreamProfile,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<(u16, u16), DecodeError> {
    let cols = cols.ok_or(DecodeError::UnexpectedEof)?;
    let rows = rows.ok_or(DecodeError::UnexpectedEof)?;
    let gridless = matches!(
        profile,
        crate::caps::BootstrapStreamProfile::AgentEventsJsonlV1
    );
    if gridless != (cols == 0 && rows == 0) {
        return Err(DecodeError::InvalidBootstrapProfile);
    }
    Ok((cols, rows))
}

/// Read a `HISTORY_PAGE` body's required `page_seq`, rejecting the reserved
/// zero sequence.
fn checked_history_page_seq(value: Option<&[u8]>) -> Result<u64, DecodeError> {
    let page_seq = sub!(
        value.ok_or(DecodeError::UnexpectedEof)?,
        |d: &mut Decoder<'_>| d.read_u64_be()
    );
    if page_seq == 0 {
        return Err(DecodeError::InvalidHistoryPageSequence);
    }
    Ok(page_seq)
}

/// Read a `HISTORY_PAGE` body's required `rows`, rejecting a page claiming
/// more than [`MAX_HISTORY_PAGE_ROWS`].
fn checked_history_page_rows(value: Option<&[u8]>) -> Result<u32, DecodeError> {
    let rows = sub!(
        value.ok_or(DecodeError::UnexpectedEof)?,
        |d: &mut Decoder<'_>| d.read_u32_be()
    );
    if rows > MAX_HISTORY_PAGE_ROWS {
        return Err(DecodeError::HistoryRowLimitExceeded);
    }
    Ok(rows)
}

/// Validate a `HISTORY_REJECTED` body's required `required_rows` retry hint
/// against [`MAX_HISTORY_PAGE_ROWS`].
fn checked_history_required_rows(required_rows: Option<u32>) -> Result<u32, DecodeError> {
    let required_rows = required_rows.ok_or(DecodeError::UnexpectedEof)?;
    if required_rows == 0 || required_rows > MAX_HISTORY_PAGE_ROWS {
        return Err(DecodeError::HistoryRowLimitExceeded);
    }
    Ok(required_rows)
}
