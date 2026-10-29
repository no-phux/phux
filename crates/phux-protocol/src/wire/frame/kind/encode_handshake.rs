//! Payload writers for the handshake, attach, input, and ack frames.

use crate::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, Compression, ServerCapabilities,
};
use crate::ids::{BootstrapId, ClientId, ResourceId, StreamId};
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::PasteEvent;
use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::{
    AttachTarget, DetachReason, ViewportInfo, encode_attach_target, encode_bootstrap_profile,
    encode_focus_event, encode_key_event, encode_mouse_event, encode_paste_event,
    encode_viewport_info,
};
use crate::wire::info::{SessionSnapshot, encode_client_id, encode_session_snapshot};

use super::{FrameKind, id_field, u8_field, u16_field, u32_field, u64_field};

impl FrameKind {
    /// Write the `HELLO` payload (`docs/protocol.md#handshake`).
    pub(super) fn encode_hello(
        enc: &mut Encoder<'_>,
        client_name: &str,
        protocol_major: u16,
        protocol_minor: u16,
        protocol_patch: u16,
        client_caps: &ClientCapabilities,
    ) {
        // String fields ride as raw UTF-8 bytes — the field is already
        // length-delimited by the TLV header, so no inner length prefix.
        enc.write_field(field::hello::CLIENT_NAME, client_name.as_bytes());
        u16_field(enc, field::hello::PROTOCOL_MAJOR, protocol_major);
        u16_field(enc, field::hello::PROTOCOL_MINOR, protocol_minor);
        u16_field(enc, field::hello::PROTOCOL_PATCH, protocol_patch);
        // ClientCapabilities remains a positional sub-record inside its
        // top-level TLV field. Protocol 0.7 fixes the complete order:
        // legacy render caps, palette presence/value, profile set,
        // exact native codec set/features, then receive bounds.
        enc.write_field_with(field::hello::CLIENT_CAPS, |e| {
            e.write_u8(client_caps.color_support.as_wire());
            e.write_u8(client_caps.layers.as_wire());
            e.write_u8(client_caps.image_protocols.as_wire());
            e.write_u8(client_caps.kbd_protocols.as_wire());
            e.write_u8(u8::from(client_caps.hyperlinks));
            e.write_u8(client_caps.output_mode.as_wire());
            if let Some(colors) = client_caps.default_colors {
                e.write_u8(1);
                e.write_u8(colors.foreground.r);
                e.write_u8(colors.foreground.g);
                e.write_u8(colors.foreground.b);
                e.write_u8(colors.background.r);
                e.write_u8(colors.background.g);
                e.write_u8(colors.background.b);
            } else {
                e.write_u8(0);
            }
            e.write_u8(client_caps.bootstrap.profiles.as_wire());
            e.write_u64_be(client_caps.bootstrap.native_codecs.as_wire());
            e.write_u32_be(client_caps.bootstrap.native_features.as_wire());
            e.write_u32_be(client_caps.bootstrap.limits.max_chunk_bytes());
            e.write_u32_be(client_caps.bootstrap.limits.max_history_page_bytes());
        });
        // Additive top-level field, omitted when the client accepts nothing
        // compressed — which is every local consumer, so a UDS HELLO stays
        // byte-identical to what protocol 0.8 has always emitted.
        if !client_caps.compression.is_empty() {
            u8_field(
                enc,
                field::hello::COMPRESSION,
                client_caps.compression.bits(),
            );
        }
        // Additive top-level field that `phux stdio-bridge` stamps on the
        // HELLO it relays. Ordinary clients leave it absent.
        if let Some(origin) = client_caps.ssh_origin {
            enc.write_field_with(field::hello::SSH_ORIGIN, |e| {
                crate::wire::ssh_origin::encode_ssh_origin(&origin, e);
            });
        }
        if client_caps.quic_streams {
            u8_field(enc, field::hello::QUIC_STREAMS, 1);
        }
    }

    /// Write the exact protocol version `HELLO_OK` admits the peer at.
    pub(super) fn encode_hello_ok_version(
        enc: &mut Encoder<'_>,
        protocol_major: u16,
        protocol_minor: u16,
        protocol_patch: u16,
    ) {
        u16_field(enc, field::hello_ok::PROTOCOL_MAJOR, protocol_major);
        u16_field(enc, field::hello_ok::PROTOCOL_MINOR, protocol_minor);
        u16_field(enc, field::hello_ok::PROTOCOL_PATCH, protocol_patch);
    }

    /// Write the terms `HELLO_OK` negotiates: caps, identity, profile, bounds.
    pub(super) fn encode_hello_ok_negotiation(
        enc: &mut Encoder<'_>,
        server_caps: ServerCapabilities,
        server_id: &[u8],
        selected_profile: BootstrapProfile,
        bootstrap_limits: BootstrapLimits,
    ) {
        enc.write_field_with(field::hello_ok::SERVER_CAPS, |e| {
            e.write_u8(server_caps.layers.as_wire());
            if !server_caps.features.is_empty() || !server_caps.features_ext.is_empty() {
                e.write_u32_be(server_caps.features.as_wire());
                if !server_caps.features_ext.is_empty() {
                    e.write_u32_be(server_caps.features_ext.as_wire());
                }
            }
        });
        // server_id is opaque bytes; the field is already
        // length-delimited so the raw bytes are the value.
        enc.write_field(field::hello_ok::SERVER_ID, server_id);
        enc.write_field_with(field::hello_ok::SELECTED_PROFILE, |e| {
            encode_bootstrap_profile(selected_profile, e);
        });
        u32_field(
            enc,
            field::hello_ok::MAX_CHUNK_BYTES,
            bootstrap_limits.max_chunk_bytes(),
        );
        u32_field(
            enc,
            field::hello_ok::MAX_HISTORY_PAGE_BYTES,
            bootstrap_limits.max_history_page_bytes(),
        );
        // Same discipline as HELLO's offer field: omitted when nothing was
        // selected, so an uncompressed connection's HELLO_OK is unchanged.
        if server_caps.compression != Compression::None {
            u8_field(
                enc,
                field::hello_ok::COMPRESSION,
                server_caps.compression.as_u8(),
            );
        }
    }

    /// Write the single nonce field shared by `PING` and `PONG`.
    pub(super) fn encode_nonce(enc: &mut Encoder<'_>, nonce: u64) {
        u64_field(enc, field::ping::NONCE, nonce);
    }

    /// Write the `RESOURCE_OUTPUT` payload: VT bytes bound to a generation.
    pub(super) fn encode_terminal_output(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        bytes: &[u8],
    ) {
        id_field(enc, field::terminal_output::TERMINAL_ID, terminal_id);
        u64_field(enc, field::terminal_output::SEQ, seq);
        enc.write_field(field::terminal_output::BYTES, bytes);
        u64_field(enc, field::terminal_output::STREAM_ID, stream_id.get());
        u64_field(
            enc,
            field::terminal_output::BOOTSTRAP_ID,
            bootstrap_id.get(),
        );
    }

    /// Write the `ATTACH` payload.
    pub(super) fn encode_attach(
        enc: &mut Encoder<'_>,
        attach_id: u32,
        target: &AttachTarget,
        viewport: &ViewportInfo,
        request_scrollback: bool,
        scrollback_limit_lines: u32,
    ) {
        enc.write_field_with(field::attach::TARGET, |e| encode_attach_target(target, e));
        enc.write_field_with(field::attach::VIEWPORT, |e| {
            encode_viewport_info(viewport, e);
        });
        u8_field(
            enc,
            field::attach::REQUEST_SCROLLBACK,
            u8::from(request_scrollback),
        );
        u32_field(
            enc,
            field::attach::SCROLLBACK_LIMIT_LINES,
            scrollback_limit_lines,
        );
        u32_field(enc, field::attach::ATTACH_ID, attach_id);
    }

    /// Write the `DETACHED` payload.
    ///
    /// Both fields are optional-absent (field.rs allocation discipline): an
    /// unstated reason and an empty message encode as nothing at all, which
    /// keeps the common acknowledge-a-clean-detach frame byte-identical to
    /// what every 0.7.0 peer already emits.
    pub(super) fn encode_detached(
        enc: &mut Encoder<'_>,
        reason: Option<DetachReason>,
        message: &str,
    ) {
        if let Some(reason) = reason {
            u8_field(enc, field::detached::REASON, reason.as_wire());
        }
        if !message.is_empty() {
            enc.write_field(field::detached::MESSAGE, message.as_bytes());
        }
    }

    /// Write the `INPUT_KEY` payload.
    pub(super) fn encode_input_key(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        event: &KeyEvent,
    ) {
        id_field(enc, field::input_key::TERMINAL_ID, terminal_id);
        enc.write_field_with(field::input_key::EVENT, |e| encode_key_event(event, e));
    }

    /// Write the `INPUT_MOUSE` payload.
    pub(super) fn encode_input_mouse(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        event: &MouseEvent,
    ) {
        id_field(enc, field::input_mouse::TERMINAL_ID, terminal_id);
        enc.write_field_with(field::input_mouse::EVENT, |e| encode_mouse_event(event, e));
    }

    /// Write the `INPUT_FOCUS` payload.
    pub(super) fn encode_input_focus(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        event: FocusEvent,
    ) {
        id_field(enc, field::input_focus::TERMINAL_ID, terminal_id);
        u8_field(enc, field::input_focus::EVENT, encode_focus_event(event));
    }

    /// Write the `INPUT_PASTE` payload.
    pub(super) fn encode_input_paste(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        event: &PasteEvent,
    ) {
        id_field(enc, field::input_paste::TERMINAL_ID, terminal_id);
        enc.write_field_with(field::input_paste::EVENT, |e| encode_paste_event(event, e));
    }

    /// Write the `INPUT_TERMINAL_REPLY` payload.
    pub(super) fn encode_input_terminal_reply(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        bytes: &[u8],
    ) {
        id_field(enc, field::input_terminal_reply::TERMINAL_ID, terminal_id);
        enc.write_field(field::input_terminal_reply::BYTES, bytes);
    }

    /// Write the `FRAME_ACK` payload.
    pub(super) fn encode_frame_ack(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
    ) {
        id_field(enc, field::frame_ack::TERMINAL_ID, terminal_id);
        u64_field(enc, field::frame_ack::SEQ, seq);
        u64_field(enc, field::frame_ack::STREAM_ID, stream_id.get());
        u64_field(enc, field::frame_ack::BOOTSTRAP_ID, bootstrap_id.get());
    }

    /// Write the `VIEWPORT_RESIZE` payload.
    pub(super) fn encode_viewport_resize(enc: &mut Encoder<'_>, viewport: &ViewportInfo) {
        enc.write_field_with(field::viewport_resize::VIEWPORT, |e| {
            encode_viewport_info(viewport, e);
        });
    }

    /// Write the `ATTACHED` payload.
    pub(super) fn encode_attached(
        enc: &mut Encoder<'_>,
        attach_id: u32,
        snapshot: &SessionSnapshot,
        initial_client_id: ClientId,
    ) {
        enc.write_field_with(field::attached::SNAPSHOT, |e| {
            encode_session_snapshot(snapshot, e);
        });
        enc.write_field_with(field::attached::INITIAL_CLIENT_ID, |e| {
            encode_client_id(initial_client_id, e);
        });
        u32_field(enc, field::attached::ATTACH_ID, attach_id);
    }

    /// Write the `ATTACH_READY` payload.
    pub(super) fn encode_attach_ready(enc: &mut Encoder<'_>, attach_id: u32) {
        u32_field(enc, field::attach_ready::ATTACH_ID, attach_id);
    }
}
