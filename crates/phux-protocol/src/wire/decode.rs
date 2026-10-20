//! Wire-frame decoder (`docs/spec/proto.md` §5, Appendix A). Bounds-checked;
//! never panics or reads past the borrowed slice on malformed input.

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
    TYPE_METADATA_KEYS, TYPE_METADATA_VALUE, TYPE_MOVE_RESOURCE, TYPE_PATH_QUERY,
    TYPE_PATH_RESULTS, TYPE_PING, TYPE_PONG, TYPE_RESIZE_TERMINAL, TYPE_RESOURCE_CLOSED,
    TYPE_RESOURCE_MOVED, TYPE_RESOURCE_OUTPUT, TYPE_RESOURCE_SPAWNED, TYPE_SET_METADATA,
    TYPE_SPAWN_RESOURCE, TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA, TYPE_VIEWPORT_RESIZE,
    TombstoneReason, decode_actor_ref, decode_agent_event, decode_attach_target,
    decode_bootstrap_codec, decode_bootstrap_id, decode_bootstrap_profile,
    decode_bootstrap_stream_profile, decode_command, decode_command_result,
    decode_directory_listing, decode_env, decode_focus_event, decode_idempotency_key,
    decode_key_event, decode_list_directory, decode_metadata_scope_key, decode_mouse_event,
    decode_move_result, decode_paste_event, decode_query, decode_results, decode_scope,
    decode_spawn_result, decode_stream_id, decode_string_list, decode_terminal_id,
    decode_viewport_info,
};
use super::info::{decode_client_id, decode_session_snapshot};
use crate::caps::{
    BootstrapCapabilities, BootstrapLimits, BootstrapProfileSet, EngineCodecSet, EngineFeatureSet,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES,
};
use crate::ids::{BootstrapId, GroupId, ResourceId, ResourceKind, StreamId};

/// Decode a positional sub-record from a TLV field's value with a fresh
/// [`Decoder`], so a malformed nested value cannot read past its field.
macro_rules! sub {
    ($value:expr, $body:expr) => {{
        let mut sub = Decoder::new($value);
        $body(&mut sub)?
    }};
}

mod bootstrap_frames;
mod caps;
mod handshake;
mod metadata_frames;
mod resource_frames;
mod session_frames;
mod spawn_rules;

use caps::{decode_client_capabilities, decode_server_capabilities, negotiated_bootstrap_limits};
use resource_frames::decode_flag;
use spawn_rules::{absorb_spawn_resource_field, bind_spawn_result, validate_spawn_for_kind};

/// A required field; absent is [`DecodeError::UnexpectedEof`].
fn req<T>(value: Option<T>) -> Result<T, DecodeError> {
    value.ok_or(DecodeError::UnexpectedEof)
}

/// A field value as an owned UTF-8 string.
pub(crate) fn utf8_value(value: &[u8]) -> Result<String, DecodeError> {
    core::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|_| DecodeError::InvalidUtf8)
}

/// Cursor-style decoder over a borrowed byte slice; `read_*` methods advance
/// and return [`DecodeError`] instead of panicking.
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
    /// When set, payload fields are views of this buffer instead of copies.
    shared: Option<bytes::Bytes>,
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
            shared: None,
        }
    }

    /// Decode `input` in place. Payload fields alias `input`.
    pub(crate) fn on_shared(input: &'a bytes::Bytes, limits: Option<BootstrapLimits>) -> Self {
        let (max_bootstrap_chunk_bytes, max_history_page_bytes) = limits.map_or(
            (MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES),
            |limits| (limits.max_chunk_bytes(), limits.max_history_page_bytes()),
        );
        Self {
            input: input.as_ref(),
            pos: 0,
            body_end: None,
            max_bootstrap_chunk_bytes,
            max_history_page_bytes,
            shared: Some(input.clone()),
        }
    }

    /// A payload field as a view of [`Self::on_shared`]'s buffer, or a copy
    /// when this decoder borrows a plain slice.
    pub(crate) fn share_exact(&self, value: &[u8]) -> bytes::Bytes {
        if value.is_empty() {
            return bytes::Bytes::new();
        }
        let Some(parent) = &self.shared else {
            return bytes::Bytes::copy_from_slice(value);
        };
        let base = parent.as_ptr() as usize;
        let start = value.as_ptr() as usize;
        let parent_end = base + parent.len();
        let value_end = start.saturating_add(value.len());
        if start >= base && value_end <= parent_end {
            let offset = start - base;
            return parent.slice(offset..offset + value.len());
        }
        bytes::Bytes::copy_from_slice(value)
    }

    /// Wrap `input` with the payload limits negotiated in `HELLO_OK`, checked
    /// before any payload is copied.
    #[must_use]
    pub const fn with_bootstrap_limits(input: &'a [u8], limits: BootstrapLimits) -> Self {
        Self {
            input,
            pos: 0,
            body_end: None,
            max_bootstrap_chunk_bytes: limits.max_chunk_bytes(),
            max_history_page_bytes: limits.max_history_page_bytes(),
            shared: None,
        }
    }

    /// Current read offset within the input.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Whether the cursor is at the end of the current frame body (or of the
    /// input outside a framed decode); an absent trailing field defaults.
    #[must_use]
    pub fn at_body_end(&self) -> bool {
        self.pos >= self.body_end.unwrap_or(self.input.len())
    }

    /// Remaining (unread) bytes.
    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        &self.input[self.pos..]
    }

    /// Bytes remaining before the frame-body boundary (or input end).
    ///
    /// Every list element takes at least one byte, so this caps
    /// pre-allocation and stops a tiny frame from reserving gigabytes.
    #[must_use]
    pub fn remaining_in_body(&self) -> usize {
        self.body_end
            .unwrap_or(self.input.len())
            .saturating_sub(self.pos)
    }

    /// A `Vec` whose reservation is at most the remaining body's bytes: an
    /// element count capped at one per byte would still reserve
    /// `size_of::<T>()` times the input. An over-declared `count` still
    /// fails with `UnexpectedEof` in the caller's read loop; a legitimate
    /// list of narrow-on-the-wire elements just grows as it decodes.
    #[must_use]
    pub(crate) fn bounded_capacity<T>(&self, count: usize) -> Vec<T> {
        let per_element = core::mem::size_of::<T>().max(1);
        Vec::with_capacity(count.min(self.remaining_in_body() / per_element))
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

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        self.take(N)?
            .try_into()
            .map_err(|_| DecodeError::UnexpectedEof)
    }

    /// Read one unsigned byte.
    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    /// Read a big-endian `u16`.
    pub fn read_u16_be(&mut self) -> Result<u16, DecodeError> {
        self.take_array().map(u16::from_be_bytes)
    }

    /// Read a big-endian `u32`.
    pub fn read_u32_be(&mut self) -> Result<u32, DecodeError> {
        self.take_array().map(u32::from_be_bytes)
    }

    /// Read a big-endian `u64`.
    pub fn read_u64_be(&mut self) -> Result<u64, DecodeError> {
        self.take_array().map(u64::from_be_bytes)
    }

    /// Read a big-endian two's-complement `i64`.
    pub fn read_i64_be(&mut self) -> Result<i64, DecodeError> {
        self.take_array().map(i64::from_be_bytes)
    }

    /// Read a big-endian IEEE-754 `f32`, bit for bit (NaNs preserved).
    pub fn read_f32_be(&mut self) -> Result<f32, DecodeError> {
        self.take_array().map(f32::from_be_bytes)
    }

    /// Read a big-endian IEEE-754 `f64`, bit for bit (NaNs preserved).
    pub fn read_f64_be(&mut self) -> Result<f64, DecodeError> {
        self.take_array().map(f64::from_be_bytes)
    }

    /// Read a presence byte (`0` = `None`, `1` = `Some`) and, when present, the
    /// value via `body`; any other byte is `UnknownEnumValue { field }`.
    pub(crate) fn read_option<T>(
        &mut self,
        field: &'static str,
        body: impl FnOnce(&mut Self) -> Result<T, DecodeError>,
    ) -> Result<Option<T>, DecodeError> {
        match self.read_u8()? {
            0 => Ok(None),
            1 => body(self).map(Some),
            other => Err(DecodeError::UnknownEnumValue {
                field,
                value: u32::from(other),
            }),
        }
    }

    /// Read a `u32`-length-prefixed byte slice; `LengthOverflow` past the
    /// protocol cap.
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

    /// Read an unsigned LEB128 varint; more than ten bytes is `LengthOverflow`.
    pub fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let mut result: u64 = 0;
        let mut shift: u32 = 0;
        loop {
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

    /// Read one body-level TLV field (`docs/spec/appendix-encoding.md` §1).
    ///
    /// `Ok(None)` at the end of the body. Every top-level field is
    /// length-delimited, so a caller skips an unknown id by ignoring its slice.
    pub fn read_field(&mut self) -> Result<Option<(u32, &'a [u8])>, DecodeError> {
        if self.at_body_end() {
            return Ok(None);
        }
        let field_id =
            u32::try_from(self.read_varint()?).map_err(|_| DecodeError::LengthOverflow)?;
        let _wire_type = self.read_u8()?; // always length-delimited here
        let len = self.read_varint()?;
        if len > u64::from(MAX_FRAME_LEN) {
            return Err(DecodeError::LengthOverflow);
        }
        let len_usize = usize::try_from(len).map_err(|_| DecodeError::LengthOverflow)?;
        let value = self.take(len_usize)?;
        Ok(Some((field_id, value)))
    }

    /// Read one complete frame; returns it and the unconsumed input tail.
    pub fn read_frame(&mut self) -> Result<(FrameKind, &'a [u8]), DecodeError> {
        // Length header: u32 big-endian, excludes itself, includes type byte.
        let length = self.read_u32_be()?;
        if !(1..=MAX_FRAME_LEN).contains(&length) {
            return Err(DecodeError::LengthOverflow);
        }
        let length_usize = usize::try_from(length).map_err(|_| DecodeError::LengthOverflow)?;

        let body_start = self.pos;
        let body_end = body_start
            .checked_add(length_usize)
            .ok_or(DecodeError::LengthOverflow)?;
        if body_end > self.input.len() {
            return Err(DecodeError::UnexpectedEof);
        }
        self.body_end = Some(body_end);

        let type_byte = self.read_u8()?;
        let frame = self.decode_body(type_byte)?;

        // Unconsumed trailing bytes are skipped (SPEC §6); reading past the
        // declared end is malformed.
        if self.pos > body_end {
            return Err(DecodeError::LengthOverflow);
        }
        self.pos = body_end;

        Ok((frame, self.remaining()))
    }
}

#[cfg(test)]
mod wire_schema;

#[cfg(test)]
mod path_capability_tests;
