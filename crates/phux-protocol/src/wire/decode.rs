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

    /// Decode one frame body by its SPEC §7 type byte. Each decoder collects
    /// fields by id, defaults absent optional ones, and reports a missing
    /// required one as `UnexpectedEof`.
    fn decode_body(&mut self, type_byte: u8) -> Result<FrameKind, DecodeError> {
        match type_byte {
            TYPE_HELLO => self.decode_hello(),
            TYPE_HELLO_OK => self.decode_hello_ok(),
            TYPE_PING => Ok(FrameKind::Ping {
                nonce: self.decode_nonce()?,
            }),
            TYPE_PONG => Ok(FrameKind::Pong {
                nonce: self.decode_nonce()?,
            }),
            TYPE_RESOURCE_OUTPUT => self.decode_terminal_output(),
            TYPE_ATTACH => self.decode_attach(),
            TYPE_DETACH => self.decode_detach(),
            TYPE_INPUT_KEY => {
                let (terminal_id, event) = self.decode_input_event(decode_key_event)?;
                Ok(FrameKind::InputKey { terminal_id, event })
            }
            TYPE_INPUT_MOUSE => {
                let (terminal_id, event) = self.decode_input_event(decode_mouse_event)?;
                Ok(FrameKind::InputMouse { terminal_id, event })
            }
            TYPE_INPUT_FOCUS => {
                let (terminal_id, event) =
                    self.decode_input_event(|d| decode_focus_event(d.read_u8()?))?;
                Ok(FrameKind::InputFocus { terminal_id, event })
            }
            TYPE_INPUT_PASTE => {
                let (terminal_id, event) = self.decode_input_event(decode_paste_event)?;
                Ok(FrameKind::InputPaste { terminal_id, event })
            }
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
                let (request_id, path, host) = decode_list_directory(self)?;
                Ok(FrameKind::ListDirectory {
                    request_id,
                    path,
                    host,
                })
            }
            TYPE_DIRECTORY_LISTING => {
                let (request_id, result) = decode_directory_listing(self)?;
                Ok(FrameKind::DirectoryListing { request_id, result })
            }
            TYPE_PATH_QUERY => {
                let (request_id, root, query, recursive, host) = decode_query(self)?;
                Ok(FrameKind::PathQuery {
                    request_id,
                    root,
                    query,
                    recursive,
                    host,
                })
            }
            TYPE_PATH_RESULTS => {
                let (request_id, result) = decode_results(self)?;
                Ok(FrameKind::PathResults { request_id, result })
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

    fn decode_hello(&mut self) -> Result<FrameKind, DecodeError> {
        use field::hello as f;
        let mut client_name: Option<String> = None;
        let mut protocol_major = None;
        let mut protocol_minor = None;
        let mut protocol_patch = None;
        let mut client_caps: Option<crate::caps::ClientCapabilities> = None;
        let mut compression: Option<crate::caps::CompressionSet> = None;
        let mut ssh_origin = None;
        let mut quic_streams = false;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::CLIENT_NAME => client_name = Some(utf8_value(value)?),
                f::PROTOCOL_MAJOR => protocol_major = Some(sub!(value, Decoder::read_u16_be)),
                f::PROTOCOL_MINOR => protocol_minor = Some(sub!(value, Decoder::read_u16_be)),
                f::PROTOCOL_PATCH => protocol_patch = Some(sub!(value, Decoder::read_u16_be)),
                f::CLIENT_CAPS => client_caps = Some(sub!(value, decode_client_capabilities)),
                f::COMPRESSION => {
                    compression = Some(crate::caps::CompressionSet::from_bits(sub!(
                        value,
                        Decoder::read_u8
                    )));
                }
                f::SSH_ORIGIN => ssh_origin = super::ssh_origin::decode_ssh_origin(value),
                f::QUIC_STREAMS => quic_streams = sub!(value, Decoder::read_u8) != 0,
                _ => {}
            }
        }
        let mut client_caps = req(client_caps)?;
        if let Some(origin) = ssh_origin {
            client_caps = client_caps.with_ssh_origin(origin);
        }
        // Top-level on the wire (§6.2 freezes `CLIENT_CAPS`), folded into the
        // typed capabilities; absent keeps the empty set.
        if let Some(compression) = compression {
            client_caps = client_caps.with_compression(compression);
        }
        client_caps = client_caps.with_quic_streams(quic_streams);
        Ok(FrameKind::Hello {
            client_name: req(client_name)?,
            protocol_major: req(protocol_major)?,
            protocol_minor: req(protocol_minor)?,
            protocol_patch: req(protocol_patch)?,
            client_caps,
        })
    }

    fn decode_hello_ok(&mut self) -> Result<FrameKind, DecodeError> {
        use field::hello_ok as f;
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
                f::PROTOCOL_MAJOR => protocol_major = Some(sub!(value, Decoder::read_u16_be)),
                f::PROTOCOL_MINOR => protocol_minor = Some(sub!(value, Decoder::read_u16_be)),
                f::PROTOCOL_PATCH => protocol_patch = Some(sub!(value, Decoder::read_u16_be)),
                f::SERVER_CAPS => server_caps = Some(sub!(value, decode_server_capabilities)),
                f::SERVER_ID => server_id = Some(value.to_vec()),
                f::SELECTED_PROFILE => {
                    selected_profile = Some(sub!(value, decode_bootstrap_profile));
                }
                f::MAX_CHUNK_BYTES => max_chunk_bytes = Some(sub!(value, Decoder::read_u32_be)),
                f::MAX_HISTORY_PAGE_BYTES => {
                    max_history_page_bytes = Some(sub!(value, Decoder::read_u32_be));
                }
                f::COMPRESSION => {
                    compression = Some(crate::caps::Compression::from_u8(sub!(
                        value,
                        Decoder::read_u8
                    )));
                }
                _ => {}
            }
        }
        let bootstrap_limits =
            negotiated_bootstrap_limits(max_chunk_bytes, max_history_page_bytes)?;
        let mut server_caps = req(server_caps)?;
        if let Some(compression) = compression {
            server_caps = server_caps.with_compression(compression);
        }
        Ok(FrameKind::HelloOk {
            protocol_major: req(protocol_major)?,
            protocol_minor: req(protocol_minor)?,
            protocol_patch: req(protocol_patch)?,
            server_caps,
            server_id: req(server_id)?,
            selected_profile: req(selected_profile)?,
            bootstrap_limits,
        })
    }

    /// Largest inflated body a `FRAME_COMPRESSED` envelope may declare
    /// (`docs/spec/proto.md` §6.4).
    ///
    /// The sender picks `uncompressed_len` and we allocate it before
    /// inflating, so it is bounded by this connection's negotiated payload
    /// limits plus envelope room rather than the 16 MiB frame cap.
    fn max_compressed_frame_bytes(&self) -> u32 {
        /// Room for the inner frame's ids and cursors beside its payload.
        const ENVELOPE_ALLOWANCE: u32 = 64 * 1024;

        self.max_bootstrap_chunk_bytes
            .max(self.max_history_page_bytes)
            .saturating_add(ENVELOPE_ALLOWANCE)
            .min(MAX_FRAME_LEN)
    }

    /// Inflate a `FRAME_COMPRESSED` envelope and decode the frame inside with
    /// the same negotiated limits. Oversized declarations are refused before
    /// allocating and nested envelopes are refused outright.
    fn decode_frame_compressed(&mut self) -> Result<FrameKind, DecodeError> {
        let mut algorithm = None;
        let mut uncompressed_len = None;
        let mut payload: Option<&[u8]> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::frame_compressed::ALGORITHM => {
                    algorithm = Some(crate::caps::Compression::from_u8(sub!(
                        value,
                        Decoder::read_u8
                    )));
                }
                field::frame_compressed::UNCOMPRESSED_LEN => {
                    uncompressed_len = Some(sub!(value, Decoder::read_u32_be));
                }
                field::frame_compressed::PAYLOAD => payload = Some(value),
                _ => {}
            }
        }
        if req(algorithm)? != crate::caps::Compression::Deflate {
            return Err(DecodeError::CompressedFrameInvalid);
        }
        let declared = req(uncompressed_len)?;
        if declared == 0 || declared > self.max_compressed_frame_bytes() {
            return Err(DecodeError::LengthOverflow);
        }
        let declared = usize::try_from(declared).map_err(|_| DecodeError::LengthOverflow)?;
        let payload = req(payload)?;
        let body = crate::wire::compress::inflate(payload, declared)?;

        let mut inner = Decoder::with_bootstrap_limits(
            &body,
            BootstrapLimits::new(self.max_bootstrap_chunk_bytes, self.max_history_page_bytes)
                .unwrap_or_default(),
        );
        inner.body_end = Some(body.len());
        let type_byte = inner.read_u8()?;
        if type_byte == TYPE_FRAME_COMPRESSED {
            return Err(DecodeError::CompressedFrameInvalid);
        }
        inner.decode_body(type_byte)
    }

    /// Decode the shared `PING` / `PONG` body: its required nonce.
    fn decode_nonce(&mut self) -> Result<u64, DecodeError> {
        let mut nonce: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::ping::NONCE {
                nonce = Some(sub!(value, Decoder::read_u64_be));
            }
        }
        req(nonce)
    }

    fn decode_terminal_output(&mut self) -> Result<FrameKind, DecodeError> {
        use field::terminal_output as f;
        let mut terminal_id: Option<ResourceId> = None;
        let mut stream_id: Option<StreamId> = None;
        let mut bootstrap_id: Option<BootstrapId> = None;
        let mut seq: Option<u64> = None;
        let mut bytes: Option<bytes::Bytes> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::SEQ => seq = Some(sub!(value, Decoder::read_u64_be)),
                f::BYTES => bytes = Some(bytes::Bytes::copy_from_slice(value)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                _ => {}
            }
        }
        Ok(FrameKind::ResourceOutput {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            seq: req(seq)?,
            bytes: req(bytes)?,
        })
    }

    fn decode_attach(&mut self) -> Result<FrameKind, DecodeError> {
        use field::attach as f;
        let mut target: Option<crate::wire::frame::AttachTarget> = None;
        let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
        let mut request_scrollback = false;
        let mut scrollback_limit_lines = 0u32;
        let mut attach_id = None;
        let mut role_policy = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::ROLE_POLICY => {
                    role_policy = Some(crate::wire::frame::RolePolicy::from_u8(sub!(
                        value,
                        Decoder::read_u8
                    )));
                }
                f::TARGET => target = Some(sub!(value, decode_attach_target)),
                f::VIEWPORT => viewport = Some(sub!(value, decode_viewport_info)),
                f::REQUEST_SCROLLBACK => request_scrollback = sub!(value, Decoder::read_u8) != 0,
                f::SCROLLBACK_LIMIT_LINES => {
                    scrollback_limit_lines = sub!(value, Decoder::read_u32_be);
                }
                f::ATTACH_ID => attach_id = Some(sub!(value, Decoder::read_u32_be)),
                _ => {}
            }
        }
        Ok(FrameKind::Attach {
            attach_id: req(attach_id)?,
            target: req(target)?,
            viewport: req(viewport)?,
            request_scrollback,
            scrollback_limit_lines,
            role_policy,
        })
    }

    fn decode_detach(&mut self) -> Result<FrameKind, DecodeError> {
        while self.read_field()?.is_some() {}
        Ok(FrameKind::Detach)
    }

    /// Decode the shared `INPUT_KEY` / `_MOUSE` / `_FOCUS` / `_PASTE` body:
    /// `TERMINAL_ID` (1) and a positional `EVENT` (2).
    fn decode_input_event<E>(
        &mut self,
        decode_event: impl Fn(&mut Decoder<'_>) -> Result<E, DecodeError>,
    ) -> Result<(ResourceId, E), DecodeError> {
        let mut terminal_id = None;
        let mut event = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::input_key::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::input_key::EVENT => event = Some(sub!(value, decode_event)),
                _ => {}
            }
        }
        Ok((req(terminal_id)?, req(event)?))
    }

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
            terminal_id: req(terminal_id)?,
            bytes: req(bytes)?,
        })
    }

    fn decode_frame_ack(&mut self) -> Result<FrameKind, DecodeError> {
        use field::frame_ack as f;
        let mut terminal_id: Option<ResourceId> = None;
        let mut stream_id: Option<StreamId> = None;
        let mut bootstrap_id: Option<BootstrapId> = None;
        let mut seq: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::SEQ => seq = Some(sub!(value, Decoder::read_u64_be)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                _ => {}
            }
        }
        Ok(FrameKind::FrameAck {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            seq: req(seq)?,
        })
    }

    fn decode_viewport_resize(&mut self) -> Result<FrameKind, DecodeError> {
        let mut viewport: Option<crate::wire::frame::ViewportInfo> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::viewport_resize::VIEWPORT {
                viewport = Some(sub!(value, decode_viewport_info));
            }
        }
        Ok(FrameKind::ViewportResize {
            viewport: req(viewport)?,
        })
    }

    fn decode_attached(&mut self) -> Result<FrameKind, DecodeError> {
        let mut snapshot: Option<crate::wire::info::SessionSnapshot> = None;
        let mut initial_client_id: Option<crate::ids::ClientId> = None;
        let mut attach_id = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::attached::SNAPSHOT => snapshot = Some(sub!(value, decode_session_snapshot)),
                field::attached::INITIAL_CLIENT_ID => {
                    initial_client_id = Some(sub!(value, decode_client_id));
                }
                field::attached::ATTACH_ID => attach_id = Some(sub!(value, Decoder::read_u32_be)),
                _ => {}
            }
        }
        Ok(FrameKind::Attached {
            attach_id: req(attach_id)?,
            snapshot: req(snapshot)?,
            initial_client_id: req(initial_client_id)?,
        })
    }

    fn decode_attach_ready(&mut self) -> Result<FrameKind, DecodeError> {
        let mut attach_id = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::attach_ready::ATTACH_ID {
                attach_id = Some(sub!(value, Decoder::read_u32_be));
            }
        }
        Ok(FrameKind::AttachReady {
            attach_id: req(attach_id)?,
        })
    }

    fn decode_bootstrap_begin(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_begin as f;
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
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CODEC => codec = Some(sub!(value, decode_bootstrap_codec)),
                f::COLS => cols = Some(sub!(value, Decoder::read_u16_be)),
                f::ROWS => rows = Some(sub!(value, Decoder::read_u16_be)),
                f::OUTPUT_MODE => output_mode = Some(sub!(value, Decoder::read_u8)),
                f::BASE_SEQ => base_seq = Some(sub!(value, Decoder::read_u64_be)),
                _ => {}
            }
        }
        let profile = decode_bootstrap_stream_profile(req(codec)?, req(output_mode)?)?;
        let (cols, rows) = checked_bootstrap_dimensions(profile, cols, rows)?;
        Ok(FrameKind::BootstrapBegin {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            profile,
            cols,
            rows,
            base_seq: req(base_seq)?,
        })
    }

    fn decode_bootstrap_chunk(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_chunk as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut chunk_seq = None;
        let mut payload = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CHUNK_SEQ => chunk_seq = Some(sub!(value, Decoder::read_u32_be)),
                f::PAYLOAD => {
                    if value.len() > self.max_bootstrap_chunk_bytes as usize {
                        return Err(DecodeError::BootstrapLimitExceeded);
                    }
                    payload = Some(bytes::Bytes::copy_from_slice(value));
                }
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapChunk {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            chunk_seq: req(chunk_seq)?,
            payload: req(payload)?,
        })
    }

    fn decode_bootstrap_ready(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_ready as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut history_cursor = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::HISTORY_CURSOR => history_cursor = Some(checked_history_cursor(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapReady {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            history_cursor,
        })
    }

    fn decode_history_request(&mut self) -> Result<FrameKind, DecodeError> {
        use field::history_request as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut max_bytes = None;
        let mut max_rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CURSOR => cursor = Some(checked_history_cursor(value)?),
                f::MAX_BYTES => max_bytes = Some(sub!(value, Decoder::read_u32_be)),
                f::MAX_ROWS => max_rows = Some(sub!(value, Decoder::read_u32_be)),
                _ => {}
            }
        }
        let max_bytes = req(max_bytes)?;
        let max_rows = req(max_rows)?;
        Ok(FrameKind::HistoryRequest {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            cursor: req(cursor)?,
            max_bytes,
            max_rows,
        })
    }

    /// Borrow every field until all scalars validate, so a malformed frame
    /// cannot make us copy a payload first.
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

        let stream_id = sub!(req(stream_id)?, decode_stream_id);
        let bootstrap_id = sub!(req(bootstrap_id)?, decode_bootstrap_id);
        let page_seq = checked_history_page_seq(page_seq)?;
        let rows = checked_history_page_rows(rows)?;

        let payload = req(payload)?;
        if payload.len() > self.max_history_page_bytes as usize {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        let cursor = req(cursor)?;
        if cursor.len() > MAX_HISTORY_CURSOR_BYTES {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        if next_cursor.is_some_and(|next| next.len() > MAX_HISTORY_CURSOR_BYTES) {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        let terminal_id = sub!(req(terminal_id)?, decode_terminal_id);

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

    fn decode_bootstrap_tombstone(&mut self) -> Result<FrameKind, DecodeError> {
        use field::bootstrap_tombstone as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut reason = None;
        let mut last_valid_seq = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::REASON => {
                    let value = sub!(value, Decoder::read_u8);
                    reason = Some(
                        TombstoneReason::from_wire(value)
                            .ok_or_else(|| DecodeError::unknown_enum("TombstoneReason", value))?,
                    );
                }
                f::LAST_VALID_SEQ => last_valid_seq = Some(sub!(value, Decoder::read_u64_be)),
                _ => {}
            }
        }
        Ok(FrameKind::BootstrapTombstone {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            reason: req(reason)?,
            last_valid_seq: req(last_valid_seq)?,
        })
    }

    fn decode_history_tombstone(&mut self) -> Result<FrameKind, DecodeError> {
        use field::history_tombstone as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut reason = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CURSOR => cursor = Some(checked_history_cursor(value)?),
                f::REASON => {
                    let value = sub!(value, Decoder::read_u8);
                    reason = Some(HistoryTombstoneReason::from_wire(value).ok_or_else(|| {
                        DecodeError::unknown_enum("HistoryTombstoneReason", value)
                    })?);
                }
                _ => {}
            }
        }
        Ok(FrameKind::HistoryTombstone {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            cursor: req(cursor)?,
            reason: req(reason)?,
        })
    }

    /// Validate a `HISTORY_REJECTED` body's required `required_bytes` retry
    /// hint against the limit negotiated for this connection.
    fn checked_history_required_bytes(
        &self,
        required_bytes: Option<u32>,
    ) -> Result<u32, DecodeError> {
        let required_bytes = req(required_bytes)?;
        if required_bytes == 0 || required_bytes > self.max_history_page_bytes {
            return Err(DecodeError::BootstrapLimitExceeded);
        }
        Ok(required_bytes)
    }

    fn decode_history_rejected(&mut self) -> Result<FrameKind, DecodeError> {
        use field::history_rejected as f;
        let mut terminal_id = None;
        let mut stream_id = None;
        let mut bootstrap_id = None;
        let mut cursor = None;
        let mut reason = None;
        let mut required_bytes = None;
        let mut required_rows = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::STREAM_ID => stream_id = Some(sub!(value, decode_stream_id)),
                f::BOOTSTRAP_ID => bootstrap_id = Some(sub!(value, decode_bootstrap_id)),
                f::CURSOR => cursor = Some(checked_history_cursor(value)?),
                f::REASON => {
                    let value = sub!(value, Decoder::read_u8);
                    reason = Some(HistoryRejectionReason::from_wire(value).ok_or_else(|| {
                        DecodeError::unknown_enum("HistoryRejectionReason", value)
                    })?);
                }
                f::REQUIRED_BYTES => required_bytes = Some(sub!(value, Decoder::read_u32_be)),
                f::REQUIRED_ROWS => required_rows = Some(sub!(value, Decoder::read_u32_be)),
                _ => {}
            }
        }
        let required_bytes = self.checked_history_required_bytes(required_bytes)?;
        let required_rows = checked_history_required_rows(required_rows)?;
        Ok(FrameKind::HistoryRejected {
            terminal_id: req(terminal_id)?,
            stream_id: req(stream_id)?,
            bootstrap_id: req(bootstrap_id)?,
            cursor: req(cursor)?,
            reason: req(reason)?,
            required_bytes,
            required_rows,
        })
    }

    fn decode_detached(&mut self) -> Result<FrameKind, DecodeError> {
        let mut reason: Option<DetachReason> = None;
        let mut message: Option<String> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::detached::REASON => {
                    let raw = sub!(value, Decoder::read_u8);
                    // An unknown reason decodes as unstated, never an error.
                    reason = DetachReason::from_wire(raw);
                }
                field::detached::MESSAGE => message = Some(utf8_value(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::Detached {
            reason,
            // Absent message field = empty, per §7.2.
            message: message.unwrap_or_default(),
        })
    }

    fn decode_bell(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::bell::TERMINAL_ID {
                terminal_id = Some(sub!(value, decode_terminal_id));
            }
        }
        Ok(FrameKind::Bell {
            terminal_id: req(terminal_id)?,
        })
    }

    fn decode_error(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id: Option<u32> = None;
        let mut code: Option<ErrorCode> = None;
        let mut message: Option<String> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::error::REQUEST_ID => request_id = Some(sub!(value, Decoder::read_u32_be)),
                field::error::CODE => {
                    let raw = sub!(value, Decoder::read_u16_be);
                    code = Some(
                        ErrorCode::from_wire(raw)
                            .ok_or_else(|| DecodeError::unknown_enum("ErrorCode", raw))?,
                    );
                }
                field::error::MESSAGE => message = Some(utf8_value(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::Error {
            request_id,
            code: req(code)?,
            message: req(message)?,
        })
    }

    fn decode_get_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let (request_id, scope, key) = decode_metadata_scope_key(self)?;
        Ok(FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        })
    }

    fn decode_set_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut scope: Option<Scope> = None;
        let mut key: Option<String> = None;
        let mut value_bytes: Vec<u8> = Vec::new();
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::set_metadata::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::set_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                field::set_metadata::KEY => key = Some(utf8_value(value)?),
                field::set_metadata::VALUE => value_bytes = value.to_vec(),
                _ => {}
            }
        }
        Ok(FrameKind::SetMetadata {
            request_id,
            scope: req(scope)?,
            key: req(key)?,
            value: value_bytes,
        })
    }

    fn decode_delete_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let (request_id, scope, key) = decode_metadata_scope_key(self)?;
        Ok(FrameKind::DeleteMetadata {
            request_id,
            scope,
            key,
        })
    }

    fn decode_list_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut scope: Option<Scope> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::list_metadata::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::list_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                _ => {}
            }
        }
        Ok(FrameKind::ListMetadata {
            request_id,
            scope: req(scope)?,
        })
    }

    fn decode_subscribe_metadata(&mut self) -> Result<FrameKind, DecodeError> {
        let mut scope: Option<Scope> = None;
        let mut key: Option<String> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::subscribe_metadata::SCOPE => scope = Some(sub!(value, decode_scope)),
                field::subscribe_metadata::KEY => key = Some(utf8_value(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::SubscribeMetadata {
            scope: req(scope)?,
            key: req(key)?,
        })
    }

    fn decode_metadata_changed(&mut self) -> Result<FrameKind, DecodeError> {
        let mut scope: Option<Scope> = None;
        let mut key: Option<String> = None;
        let mut value_bytes: Option<Vec<u8>> = None;
        let mut actor = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::metadata_changed::SCOPE => scope = Some(sub!(value, decode_scope)),
                field::metadata_changed::KEY => key = Some(utf8_value(value)?),
                field::metadata_changed::VALUE => value_bytes = Some(value.to_vec()),
                field::metadata_changed::ACTOR => actor = Some(sub!(value, decode_actor_ref)),
                _ => {}
            }
        }
        Ok(FrameKind::MetadataChanged {
            scope: req(scope)?,
            key: req(key)?,
            value: value_bytes,
            actor,
        })
    }

    fn decode_metadata_value(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut value_bytes: Option<Vec<u8>> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::metadata_value::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::metadata_value::VALUE => value_bytes = Some(value.to_vec()),
                _ => {}
            }
        }
        Ok(FrameKind::MetadataValue {
            request_id,
            value: value_bytes,
        })
    }

    fn decode_metadata_keys(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut keys: Vec<String> = Vec::new();
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::metadata_keys::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
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

    fn decode_spawn_terminal(&mut self) -> Result<FrameKind, DecodeError> {
        use field::spawn_terminal as f;
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
        let mut resource = SpawnResource::default();
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                f::GROUP => group = GroupId::new(sub!(value, Decoder::read_u32_be)),
                f::COMMAND => command = Some(sub!(value, decode_string_list)),
                f::CWD => cwd = Some(utf8_value(value)?),
                f::ENV => env = Some(sub!(value, decode_env)),
                f::TERM => term = Some(utf8_value(value)?),
                f::SATELLITE => {
                    satellite = Some(crate::ids::SatelliteHost::new(
                        core::str::from_utf8(value).map_err(|_| DecodeError::InvalidUtf8)?,
                    ));
                }
                f::OWNER_TERMINAL => owner_terminal = Some(sub!(value, decode_terminal_id)),
                f::AGENT_SESSION => agent_session = Some(value.to_vec()),
                f::INITIAL_SIZE => {
                    initial_size = Some(sub!(value, |d: &mut Decoder<'_>| {
                        let cols = d.read_u16_be()?;
                        let rows = d.read_u16_be()?;
                        Ok((cols, rows))
                    }));
                }
                other => absorb_spawn_resource_field(&mut resource, other, value)?,
            }
        }
        // Default resource fields decode to `None`: canonical, pre-kind bytes.
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

    fn decode_terminal_spawned(&mut self) -> Result<FrameKind, DecodeError> {
        use field::terminal_spawned as f;
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::SpawnResult> = None;
        let mut instance: Option<crate::ids::ServerInstance> = None;
        let mut replayed = false;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                f::RESULT => result = Some(sub!(value, decode_spawn_result)),
                f::INSTANCE => {
                    instance = Some(sub!(value, crate::wire::frame::decode_server_instance));
                }
                f::REPLAYED => {
                    replayed = sub!(value, |d: &mut Decoder<'_>| decode_flag(d, "replayed"));
                }
                _ => {}
            }
        }
        let result = req(result)?;
        Ok(FrameKind::ResourceSpawned {
            request_id,
            result: bind_spawn_result(result, instance, replayed),
        })
    }

    fn decode_move_terminal(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut terminal: Option<ResourceId> = None;
        let mut owner_terminal: Option<ResourceId> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::move_terminal::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::move_terminal::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                field::move_terminal::OWNER_TERMINAL => {
                    owner_terminal = Some(sub!(value, decode_terminal_id));
                }
                _ => {}
            }
        }
        Ok(FrameKind::MoveResource {
            request_id,
            terminal: req(terminal)?,
            owner_terminal: req(owner_terminal)?,
        })
    }

    fn decode_terminal_moved(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::MoveResult> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_moved::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::terminal_moved::RESULT => result = Some(sub!(value, decode_move_result)),
                _ => {}
            }
        }
        Ok(FrameKind::ResourceMoved {
            request_id,
            result: req(result)?,
        })
    }

    fn decode_terminal_closed(&mut self) -> Result<FrameKind, DecodeError> {
        use field::terminal_closed as f;
        let mut terminal_id: Option<ResourceId> = None;
        let mut exit_status: Option<i32> = None;
        let mut reason = CloseReason::Unknown;
        let mut signal: Option<i32> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL_ID => terminal_id = Some(sub!(value, decode_terminal_id)),
                f::EXIT_STATUS => exit_status = Some(read_i32_value(value)?),
                f::REASON => reason = CloseReason::from_wire(sub!(value, Decoder::read_u8)),
                f::SIGNAL => signal = Some(read_i32_value(value)?),
                _ => {}
            }
        }
        Ok(FrameKind::ResourceClosed {
            terminal_id: req(terminal_id)?,
            exit_status,
            reason,
            signal,
        })
    }

    fn decode_terminal_resize(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal_id: Option<ResourceId> = None;
        let mut cols = 0u16;
        let mut rows = 0u16;
        let mut cell_w: Option<u16> = None;
        let mut cell_h: Option<u16> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::terminal_resize::TERMINAL_ID => {
                    terminal_id = Some(sub!(value, decode_terminal_id));
                }
                field::terminal_resize::COLS => cols = sub!(value, Decoder::read_u16_be),
                field::terminal_resize::ROWS => rows = sub!(value, Decoder::read_u16_be),
                field::terminal_resize::CELL_WIDTH_PX => {
                    cell_w = Some(sub!(value, Decoder::read_u16_be));
                }
                field::terminal_resize::CELL_HEIGHT_PX => {
                    cell_h = Some(sub!(value, Decoder::read_u16_be));
                }
                _ => {}
            }
        }
        Ok(FrameKind::ResizeTerminal {
            terminal_id: req(terminal_id)?,
            cols,
            rows,
            // Both axes or neither (L1 §3.1): a half pair is no cell size.
            cell_px: cell_w.zip(cell_h),
        })
    }

    fn decode_command(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut command: Option<crate::wire::frame::Command> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::command::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::command::COMMAND => command = Some(sub!(value, decode_command)),
                _ => {}
            }
        }
        Ok(FrameKind::Command {
            request_id,
            command: req(command)?,
        })
    }

    fn decode_command_result(&mut self) -> Result<FrameKind, DecodeError> {
        let mut request_id = 0u32;
        let mut result: Option<crate::wire::frame::CommandResult> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::command_result::REQUEST_ID => request_id = sub!(value, Decoder::read_u32_be),
                field::command_result::RESULT => result = Some(sub!(value, decode_command_result)),
                _ => {}
            }
        }
        Ok(FrameKind::CommandResult {
            request_id,
            result: req(result)?,
        })
    }

    fn decode_subscribe_events(&mut self) -> Result<FrameKind, DecodeError> {
        use field::subscribe_events as f;
        let mut terminal: Option<ResourceId> = None;
        let mut after_seq: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            match id {
                f::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                f::AFTER_SEQ => after_seq = Some(sub!(value, Decoder::read_u64_be)),
                _ => {}
            }
        }
        Ok(FrameKind::SubscribeEvents {
            terminal,
            after_seq,
        })
    }

    fn decode_event(&mut self) -> Result<FrameKind, DecodeError> {
        let mut terminal: Option<ResourceId> = None;
        let mut event: Option<crate::wire::frame::AgentEvent> = None;
        let mut stamp = StampParts::default();
        while let Some((id, value)) = self.read_field()? {
            match id {
                field::event::TERMINAL => terminal = Some(sub!(value, decode_terminal_id)),
                field::event::EVENT => event = Some(sub!(value, decode_agent_event)),
                other => stamp.absorb(other, value)?,
            }
        }
        Ok(FrameKind::Event {
            terminal,
            event: req(event)?,
            stamp: stamp.finish(),
        })
    }
}

/// The `EVENT` journal-stamp fields (3-6, ADR-0123); the stamp exists iff
/// `seq` was present.
#[derive(Default)]
struct StampParts {
    seq: Option<u64>,
    ts_ms: u64,
    actor: Option<crate::wire::frame::ActorRef>,
    operation_id: Option<crate::ids::IdempotencyKey>,
}

impl StampParts {
    /// Take one `EVENT` field if it is a stamp field; ignore any other id.
    fn absorb(&mut self, id: u32, value: &[u8]) -> Result<(), DecodeError> {
        match id {
            field::event::SEQ => self.seq = Some(sub!(value, Decoder::read_u64_be)),
            field::event::TS_MS => self.ts_ms = sub!(value, Decoder::read_u64_be),
            field::event::ACTOR => self.actor = Some(sub!(value, decode_actor_ref)),
            field::event::OPERATION_ID => self.operation_id = Some(decode_idempotency_key(value)?),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Option<Box<crate::wire::frame::EventStamp>> {
        let seq = self.seq?;
        Some(Box::new(
            crate::wire::frame::EventStamp::new(seq, self.ts_ms)
                .with_actor(self.actor)
                .with_operation_id(self.operation_id),
        ))
    }
}

/// Read a field value that is an `i32` carried as its two's-complement `u32`.
fn read_i32_value(value: &[u8]) -> Result<i32, DecodeError> {
    let bits = Decoder::new(value).read_u32_be()?;
    Ok(i32::from_be_bytes(bits.to_be_bytes()))
}

/// Read a one-byte flag: `0` or `1`; anything else is malformed.
fn decode_flag(dec: &mut Decoder<'_>, field: &'static str) -> Result<bool, DecodeError> {
    match dec.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(DecodeError::UnknownEnumValue {
            field,
            value: u32::from(other),
        }),
    }
}

/// Take one of `SPAWN_RESOURCE`'s resource fields (11-17); ignore other ids.
fn absorb_spawn_resource_field(
    resource: &mut SpawnResource,
    id: u32,
    value: &[u8],
) -> Result<(), DecodeError> {
    use field::spawn_terminal as f;
    match id {
        f::KIND => resource.kind = ResourceKind::from_wire(sub!(value, Decoder::read_u8)),
        f::PARENT => resource.parent = Some(sub!(value, decode_terminal_id)),
        f::PROVIDER => {
            resource.provider = Some(decode_agent_facet_str(value, MAX_RESOURCE_PROVIDER_BYTES)?);
        }
        f::NATIVE_ID => {
            resource.native_id = Some(decode_agent_facet_str(value, MAX_RESOURCE_NATIVE_ID_BYTES)?);
        }
        f::BIND_INSTANCE => resource.bind_instance = sub!(value, decode_bind_instance),
        f::RETAIN_SECS => resource.retain_secs = Some(sub!(value, Decoder::read_u32_be)),
        f::IDEMPOTENCY_KEY => resource.idempotency_key = Some(decode_idempotency_key(value)?),
        _ => {}
    }
    Ok(())
}

/// Read `SPAWN_RESOURCE.bind_instance` (field 15): `0` or `1`.
fn decode_bind_instance(dec: &mut Decoder<'_>) -> Result<bool, DecodeError> {
    decode_flag(dec, "SpawnResource.bind_instance")
}

/// Fold `RESOURCE_SPAWNED.instance` and `replayed` into the typed result;
/// both are dropped beside a refusal.
fn bind_spawn_result(
    result: crate::wire::frame::SpawnResult,
    instance: Option<crate::ids::ServerInstance>,
    replayed: bool,
) -> crate::wire::frame::SpawnResult {
    use crate::wire::frame::SpawnResult;
    match (result, instance) {
        (SpawnResult::Ok(id), instance) if replayed => SpawnResult::Replayed { id, instance },
        (SpawnResult::Ok(id), Some(instance)) => SpawnResult::OkBound { id, instance },
        (result, _) => result,
    }
}

/// Decode an agent-facet string, refusing empty or over-`max_bytes` values
/// before copying.
fn decode_agent_facet_str(value: &[u8], max_bytes: usize) -> Result<String, DecodeError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(DecodeError::AgentFacetLimitExceeded);
    }
    utf8_value(value)
}

/// Enforce `SPAWN_RESOURCE`'s per-kind field rules (`docs/spec/L1.md` §3.1).
///
/// An `Unknown` kind passes unvalidated so the server can answer
/// `UnsupportedKind` rather than fail the connection.
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
    let Some(resource) = resource.as_deref() else {
        return Ok(());
    };
    let SpawnResource {
        kind,
        parent,
        provider,
        native_id,
        retain_secs,
        ..
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
                (field::spawn_terminal::RETAIN_SECS, retain_secs.is_some()),
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

/// Decode a `HELLO` frame's positional `client_caps` field value.
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
    crate::caps::ColorSupport::from_wire(color_value)
        .ok_or_else(|| DecodeError::unknown_enum("ColorSupport", color_value))
}

/// Decode the hyperlink-support flag of a client capability block.
fn decode_hyperlinks_flag(d: &mut Decoder<'_>) -> Result<bool, DecodeError> {
    match d.read_u8()? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(DecodeError::unknown_enum("hyperlinks", value)),
    }
}

/// Decode the output-mode byte of a client capability block.
fn decode_output_mode(d: &mut Decoder<'_>) -> Result<crate::caps::OutputMode, DecodeError> {
    let output_mode_tag = d.read_u8()?;
    match output_mode_tag {
        0 => Ok(crate::caps::OutputMode::Raw),
        1 => Ok(crate::caps::OutputMode::StateSync),
        value => Err(DecodeError::unknown_enum("OutputMode", value)),
    }
}

/// Decode the presence-tagged default palette of a client capability block.
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
        value => Err(DecodeError::unknown_enum("default_colors presence", value)),
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
/// The feature word is trailing-optional, and bytes after it are ignored:
/// ADR-0137 reserves them for `features_ext`.
fn decode_server_capabilities(
    d: &mut Decoder<'_>,
) -> Result<crate::caps::ServerCapabilities, DecodeError> {
    let mut caps = crate::caps::ServerCapabilities::new()
        .with_layers(crate::caps::LayerSet::from_wire(d.read_u8()?));
    if !d.at_body_end() {
        caps = caps.with_features(crate::caps::ServerFeatureSet::from_wire(d.read_u32_be()?));
    }
    if !d.at_body_end() {
        caps = caps.with_features_ext(crate::caps::ServerFeatureExtSet::from_wire(
            d.read_u32_be()?,
        ));
    }
    Ok(caps)
}

/// Rebuild the two required payload limits a `HELLO_OK` negotiated.
fn negotiated_bootstrap_limits(
    max_chunk_bytes: Option<u32>,
    max_history_page_bytes: Option<u32>,
) -> Result<BootstrapLimits, DecodeError> {
    BootstrapLimits::new(req(max_chunk_bytes)?, req(max_history_page_bytes)?)
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

/// Validate `BOOTSTRAP_BEGIN` dimensions: a Terminal stream needs a non-zero
/// grid, and a gridless `AgentEventsJsonlV1` stream must carry `0 x 0`.
fn checked_bootstrap_dimensions(
    profile: crate::caps::BootstrapStreamProfile,
    cols: Option<u16>,
    rows: Option<u16>,
) -> Result<(u16, u16), DecodeError> {
    let cols = req(cols)?;
    let rows = req(rows)?;
    let gridless = matches!(
        profile,
        crate::caps::BootstrapStreamProfile::AgentEventsJsonlV1
    );
    if gridless != (cols == 0 && rows == 0) {
        return Err(DecodeError::InvalidBootstrapProfile);
    }
    Ok((cols, rows))
}

/// Read a required non-zero `HISTORY_PAGE.page_seq`.
fn checked_history_page_seq(value: Option<&[u8]>) -> Result<u64, DecodeError> {
    let page_seq = sub!(req(value)?, Decoder::read_u64_be);
    if page_seq == 0 {
        return Err(DecodeError::InvalidHistoryPageSequence);
    }
    Ok(page_seq)
}

/// Read a required `HISTORY_PAGE.rows`, at most [`MAX_HISTORY_PAGE_ROWS`].
fn checked_history_page_rows(value: Option<&[u8]>) -> Result<u32, DecodeError> {
    let rows = sub!(req(value)?, Decoder::read_u32_be);
    if rows > MAX_HISTORY_PAGE_ROWS {
        return Err(DecodeError::HistoryRowLimitExceeded);
    }
    Ok(rows)
}

/// Validate a required `HISTORY_REJECTED.required_rows` retry hint.
fn checked_history_required_rows(required_rows: Option<u32>) -> Result<u32, DecodeError> {
    let required_rows = req(required_rows)?;
    if required_rows == 0 || required_rows > MAX_HISTORY_PAGE_ROWS {
        return Err(DecodeError::HistoryRowLimitExceeded);
    }
    Ok(required_rows)
}

#[cfg(test)]
mod path_capability_tests {
    use super::*;
    use crate::caps::{ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet};

    #[test]
    fn second_word_preserves_first_and_ignores_unknown_bits() {
        let legacy =
            decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0])).expect("old caps");
        assert_eq!(legacy.features, ServerFeatureSet::new());
        assert_eq!(legacy.features_ext, ServerFeatureExtSet::new());
        let extended =
            decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0x80, 0, 0, 1]))
                .expect("extended caps");
        assert_eq!(extended.features, ServerFeatureSet::new());
        assert!(extended.features_ext.contains(ServerFeatureExt::PathQuery));
        assert_eq!(extended.features_ext.as_wire(), 1);
        assert!(decode_server_capabilities(&mut Decoder::new(&[1, 0, 0, 0, 0, 0])).is_err());
    }
}
