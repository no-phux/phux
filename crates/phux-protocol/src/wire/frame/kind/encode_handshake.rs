//! Payload writers for the handshake, attach, input, and ack frames.

use crate::caps::{ClientCapabilities, Compression, ServerCapabilities};
use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::{
    encode_attach_target, encode_bootstrap_profile, encode_focus_event, encode_key_event,
    encode_mouse_event, encode_paste_event, encode_viewport_info,
};
use crate::wire::info::{encode_client_id, encode_session_snapshot};

use super::{FrameKind, id_field};

impl FrameKind {
    /// Write a connection frame's payload: handshake, attach, input, ack.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per connection frame, each its field order"
    )]
    pub(super) fn encode_connection_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            Self::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => {
                // Strings ride as raw UTF-8: the TLV header already
                // delimits them.
                enc.write_field(field::hello::CLIENT_NAME, client_name.as_bytes());
                enc.write_u16_field(field::hello::PROTOCOL_MAJOR, *protocol_major);
                enc.write_u16_field(field::hello::PROTOCOL_MINOR, *protocol_minor);
                enc.write_u16_field(field::hello::PROTOCOL_PATCH, *protocol_patch);
                encode_hello_caps(enc, client_caps);
            }
            Self::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => {
                enc.write_u16_field(field::hello_ok::PROTOCOL_MAJOR, *protocol_major);
                enc.write_u16_field(field::hello_ok::PROTOCOL_MINOR, *protocol_minor);
                enc.write_u16_field(field::hello_ok::PROTOCOL_PATCH, *protocol_patch);
                encode_server_caps(enc, *server_caps);
                enc.write_field(field::hello_ok::SERVER_ID, server_id);
                // Absent when the client offered no Terminal profile.
                if let Some(profile) = selected_profile {
                    enc.write_field_with(field::hello_ok::SELECTED_PROFILE, |e| {
                        encode_bootstrap_profile(*profile, e);
                    });
                }
                let (chunk, page) = (
                    bootstrap_limits.max_chunk_bytes(),
                    bootstrap_limits.max_history_page_bytes(),
                );
                enc.write_u32_field(field::hello_ok::MAX_CHUNK_BYTES, chunk);
                enc.write_u32_field(field::hello_ok::MAX_HISTORY_PAGE_BYTES, page);
                // Omitted when nothing was selected, so an uncompressed
                // connection's HELLO_OK is unchanged.
                if server_caps.compression != Compression::None {
                    let compression = server_caps.compression.as_u8();
                    enc.write_u8_field(field::hello_ok::COMPRESSION, compression);
                }
            }
            Self::Ping { nonce } | Self::Pong { nonce } => {
                enc.write_u64_field(field::ping::NONCE, *nonce);
            }
            Self::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                id_field(enc, field::terminal_output::TERMINAL_ID, terminal_id);
                enc.write_u64_field(field::terminal_output::SEQ, *seq);
                enc.write_field(field::terminal_output::BYTES, bytes);
                enc.write_u64_field(field::terminal_output::STREAM_ID, stream_id.get());
                enc.write_u64_field(field::terminal_output::BOOTSTRAP_ID, bootstrap_id.get());
            }
            Self::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy,
            } => {
                enc.write_field_with(field::attach::TARGET, |e| encode_attach_target(target, e));
                enc.write_field_with(field::attach::VIEWPORT, |e| {
                    encode_viewport_info(viewport, e);
                });
                let scrollback = u8::from(*request_scrollback);
                enc.write_u8_field(field::attach::REQUEST_SCROLLBACK, scrollback);
                let limit = *scrollback_limit_lines;
                enc.write_u32_field(field::attach::SCROLLBACK_LIMIT_LINES, limit);
                enc.write_u32_field(field::attach::ATTACH_ID, *attach_id);
                if let Some(policy) = role_policy {
                    enc.write_u8_field(field::attach::ROLE_POLICY, policy.to_u8());
                }
            }
            // A unit variant: the type byte only.
            Self::Detach => {}
            // An unstated reason and an empty message encode as nothing, so
            // a clean detach keeps the bytes every 0.7.0 peer emits.
            Self::Detached { reason, message } => {
                if let Some(reason) = reason {
                    enc.write_u8_field(field::detached::REASON, reason.as_wire());
                }
                if !message.is_empty() {
                    enc.write_field(field::detached::MESSAGE, message.as_bytes());
                }
            }
            Self::InputKey { terminal_id, event } => {
                id_field(enc, field::input_key::TERMINAL_ID, terminal_id);
                enc.write_field_with(field::input_key::EVENT, |e| encode_key_event(event, e));
            }
            Self::InputMouse { terminal_id, event } => {
                id_field(enc, field::input_mouse::TERMINAL_ID, terminal_id);
                enc.write_field_with(field::input_mouse::EVENT, |e| encode_mouse_event(event, e));
            }
            Self::InputFocus { terminal_id, event } => {
                id_field(enc, field::input_focus::TERMINAL_ID, terminal_id);
                enc.write_u8_field(field::input_focus::EVENT, encode_focus_event(*event));
            }
            Self::InputPaste { terminal_id, event } => {
                id_field(enc, field::input_paste::TERMINAL_ID, terminal_id);
                enc.write_field_with(field::input_paste::EVENT, |e| encode_paste_event(event, e));
            }
            Self::InputTerminalReply { terminal_id, bytes } => {
                id_field(enc, field::input_terminal_reply::TERMINAL_ID, terminal_id);
                enc.write_field(field::input_terminal_reply::BYTES, bytes);
            }
            Self::ClipboardReply {
                terminal_id,
                selection,
                text,
            } => {
                id_field(enc, field::clipboard_reply::TERMINAL_ID, terminal_id);
                enc.write_u8_field(field::clipboard_reply::SELECTION, *selection);
                enc.write_field(field::clipboard_reply::TEXT, text.0.as_bytes());
            }
            Self::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => {
                id_field(enc, field::frame_ack::TERMINAL_ID, terminal_id);
                enc.write_u64_field(field::frame_ack::SEQ, *seq);
                enc.write_u64_field(field::frame_ack::STREAM_ID, stream_id.get());
                enc.write_u64_field(field::frame_ack::BOOTSTRAP_ID, bootstrap_id.get());
            }
            Self::ViewportResize { viewport } => {
                enc.write_field_with(field::viewport_resize::VIEWPORT, |e| {
                    encode_viewport_info(viewport, e);
                });
            }
            Self::Attached {
                attach_id,
                snapshot,
                initial_client_id,
            } => {
                enc.write_field_with(field::attached::SNAPSHOT, |e| {
                    encode_session_snapshot(snapshot, e);
                });
                enc.write_field_with(field::attached::INITIAL_CLIENT_ID, |e| {
                    encode_client_id(*initial_client_id, e);
                });
                enc.write_u32_field(field::attached::ATTACH_ID, *attach_id);
            }
            Self::AttachReady { attach_id } => {
                enc.write_u32_field(field::attach_ready::ATTACH_ID, *attach_id);
            }
            _ => unreachable!("encode_payload routes only connection frames here"),
        }
    }
}

/// Write `HELLO`'s capability fields, from field 5 on.
fn encode_hello_caps(enc: &mut Encoder<'_>, caps: &ClientCapabilities) {
    // A positional sub-record whose order protocol 0.7 fixes: legacy render
    // caps, palette presence/value, profile set, exact native codec
    // set/features, then receive bounds.
    enc.write_field_with(field::hello::CLIENT_CAPS, |e| {
        e.write_u8(caps.color_support.as_wire());
        e.write_u8(caps.layers.as_wire());
        e.write_u8(caps.image_protocols.as_wire());
        e.write_u8(caps.kbd_protocols.as_wire());
        e.write_u8(u8::from(caps.hyperlinks));
        e.write_u8(caps.output_mode.as_wire());
        e.write_option(caps.default_colors, |e, colors| {
            let (fg, bg) = (colors.foreground, colors.background);
            e.write_raw(&[fg.r, fg.g, fg.b, bg.r, bg.g, bg.b]);
        });
        e.write_u8(caps.bootstrap.profiles.as_wire());
        e.write_u64_be(caps.bootstrap.native_codecs.as_wire());
        e.write_u32_be(caps.bootstrap.native_features.as_wire());
        e.write_u32_be(caps.bootstrap.limits.max_chunk_bytes());
        e.write_u32_be(caps.bootstrap.limits.max_history_page_bytes());
    });
    // Omitted when the client accepts nothing compressed, as every local
    // consumer does, so a UDS HELLO keeps its protocol 0.8 bytes.
    if !caps.compression.is_empty() {
        enc.write_u8_field(field::hello::COMPRESSION, caps.compression.bits());
    }
    // Stamped by `phux stdio-bridge` on the HELLO it relays.
    if let Some(origin) = caps.ssh_origin {
        enc.write_field_with(field::hello::SSH_ORIGIN, |e| {
            crate::wire::ssh_origin::encode_ssh_origin(&origin, e);
        });
    }
    if caps.quic_streams {
        enc.write_u8_field(field::hello::QUIC_STREAMS, 1);
    }
    // Always explicit, so a server never guesses kinds from the default.
    enc.write_field_with(field::hello::RESOURCE_KINDS, |e| {
        e.write_u64_be(caps.resource_kinds.as_wire());
    });
    if let Some(platform) = caps.platform {
        enc.write_field(field::hello::PLATFORM, platform.as_str().as_bytes());
    }
    if caps.credential_approvals {
        enc.write_u8_field(field::hello::CREDENTIAL_APPROVALS, 1);
    }
}

/// Write `HELLO_OK.server_caps`: layers, then the feature words only as far
/// as one is non-empty.
fn encode_server_caps(enc: &mut Encoder<'_>, caps: ServerCapabilities) {
    enc.write_field_with(field::hello_ok::SERVER_CAPS, |e| {
        e.write_u8(caps.layers.as_wire());
        if !caps.features.is_empty() || !caps.features_ext.is_empty() {
            e.write_u32_be(caps.features.as_wire());
            if !caps.features_ext.is_empty() {
                e.write_u32_be(caps.features_ext.as_wire());
            }
        }
    });
}
