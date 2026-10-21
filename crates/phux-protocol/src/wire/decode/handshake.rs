//! Frame-body dispatch by type byte, and the handshake, compression, liveness, and output frames.

use super::{
    BootstrapId, BootstrapLimits, DecodeError, Decoder, FrameKind, MAX_FRAME_LEN, ResourceId,
    StreamId, TYPE_ATTACH, TYPE_ATTACH_READY, TYPE_ATTACHED, TYPE_BELL, TYPE_BOOTSTRAP_BEGIN,
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
    decode_bootstrap_id, decode_bootstrap_profile, decode_client_capabilities,
    decode_directory_listing, decode_focus_event, decode_key_event, decode_list_directory,
    decode_mouse_event, decode_paste_event, decode_query, decode_results,
    decode_server_capabilities, decode_stream_id, decode_terminal_id, field,
    negotiated_bootstrap_limits, req, utf8_value,
};

impl Decoder<'_> {
    /// Decode one frame body by its SPEC §7 type byte. Each decoder collects
    /// fields by id, defaults absent optional ones, and reports a missing
    /// required one as `UnexpectedEof`.
    pub(super) fn decode_body(&mut self, type_byte: u8) -> Result<FrameKind, DecodeError> {
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

    pub(super) fn decode_hello(&mut self) -> Result<FrameKind, DecodeError> {
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
                f::SSH_ORIGIN => ssh_origin = crate::wire::ssh_origin::decode_ssh_origin(value),
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

    pub(super) fn decode_hello_ok(&mut self) -> Result<FrameKind, DecodeError> {
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
    pub(super) fn max_compressed_frame_bytes(&self) -> u32 {
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
    pub(super) fn decode_frame_compressed(&mut self) -> Result<FrameKind, DecodeError> {
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
    pub(super) fn decode_nonce(&mut self) -> Result<u64, DecodeError> {
        let mut nonce: Option<u64> = None;
        while let Some((id, value)) = self.read_field()? {
            if id == field::ping::NONCE {
                nonce = Some(sub!(value, Decoder::read_u64_be));
            }
        }
        req(nonce)
    }

    pub(super) fn decode_terminal_output(&mut self) -> Result<FrameKind, DecodeError> {
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
                f::BYTES => bytes = Some(self.share_exact(value)),
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
}
