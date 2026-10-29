//! [`FrameKind::encode`]'s dispatch: one arm per SPEC §7 catalog entry.

use crate::wire::encode::Encoder;
use crate::wire::field;
use crate::wire::frame::directory::{encode_directory_listing, encode_list_directory};
use crate::wire::frame::encode_actor_ref;
use crate::wire::frame::path::{encode_query, encode_results};

use super::{
    FrameKind, encode_event_stamp, generation_fields, u8_field, u64_field, write_i32_field,
};

impl FrameKind {
    /// Write the field-tagged payload of `self`; the type byte is already out.
    ///
    /// One arm per SPEC §7 catalog entry, each a single call into the
    /// `encode_*` helper that owns that frame's field order. The table stays
    /// in one place so it can be read against the decoder's dispatch.
    #[allow(
        clippy::too_many_lines,
        reason = "one call per arm over the whole SPEC §7 catalog; the arms belong in a single table that mirrors the decoder's"
    )]
    pub(super) fn encode_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            Self::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => Self::encode_hello(
                enc,
                client_name,
                *protocol_major,
                *protocol_minor,
                *protocol_patch,
                client_caps,
            ),
            Self::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => {
                Self::encode_hello_ok_version(
                    enc,
                    *protocol_major,
                    *protocol_minor,
                    *protocol_patch,
                );
                Self::encode_hello_ok_negotiation(
                    enc,
                    *server_caps,
                    server_id,
                    *selected_profile,
                    *bootstrap_limits,
                );
            }
            // `Ping` and `Pong` share a single-`u64` nonce field; merged to
            // satisfy `clippy::match_same_arms`.
            Self::Ping { nonce } | Self::Pong { nonce } => Self::encode_nonce(enc, *nonce),
            Self::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => Self::encode_terminal_output(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *seq,
                bytes,
            ),
            Self::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy,
            } => {
                Self::encode_attach(
                    enc,
                    *attach_id,
                    target,
                    viewport,
                    *request_scrollback,
                    *scrollback_limit_lines,
                );
                if let Some(policy) = role_policy {
                    u8_field(enc, field::attach::ROLE_POLICY, policy.to_u8());
                }
            }
            // `Detach` is a unit variant: type byte only, no fields.
            Self::Detach => {}
            Self::Detached { reason, message } => Self::encode_detached(enc, *reason, message),
            Self::InputKey { terminal_id, event } => {
                Self::encode_input_key(enc, terminal_id, event);
            }
            Self::InputMouse { terminal_id, event } => {
                Self::encode_input_mouse(enc, terminal_id, event);
            }
            Self::InputFocus { terminal_id, event } => {
                Self::encode_input_focus(enc, terminal_id, *event);
            }
            Self::InputPaste { terminal_id, event } => {
                Self::encode_input_paste(enc, terminal_id, event);
            }
            Self::InputTerminalReply { terminal_id, bytes } => {
                Self::encode_input_terminal_reply(enc, terminal_id, bytes);
            }
            Self::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => Self::encode_frame_ack(enc, terminal_id, *stream_id, *bootstrap_id, *seq),
            Self::ViewportResize { viewport } => Self::encode_viewport_resize(enc, viewport),
            Self::Attached {
                attach_id,
                snapshot,
                initial_client_id,
            } => Self::encode_attached(enc, *attach_id, snapshot, *initial_client_id),
            Self::AttachReady { attach_id } => Self::encode_attach_ready(enc, *attach_id),
            Self::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                Self::encode_bootstrap_begin_profile(enc, *profile, *cols, *rows, *base_seq);
            }
            Self::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => Self::encode_bootstrap_chunk(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *chunk_seq,
                payload,
            ),
            Self::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => Self::encode_bootstrap_ready(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                history_cursor.as_deref(),
            ),
            Self::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => Self::encode_history_request(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                cursor,
                *max_bytes,
                *max_rows,
            ),
            Self::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                Self::encode_history_page_body(
                    enc,
                    cursor,
                    next_cursor.as_deref(),
                    payload,
                    *page_seq,
                    *rows,
                );
            }
            Self::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => Self::encode_bootstrap_tombstone(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *reason,
                *last_valid_seq,
            ),
            Self::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => Self::encode_history_tombstone(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                cursor,
                *reason,
            ),
            Self::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                Self::encode_history_rejected_reason(
                    enc,
                    cursor,
                    *reason,
                    *required_bytes,
                    *required_rows,
                );
            }
            Self::Bell { terminal_id } => Self::encode_bell(enc, terminal_id),
            Self::Error {
                request_id,
                code,
                message,
            } => Self::encode_error(enc, *request_id, *code, message),
            // GET / DELETE share `{request_id, scope, key}`; merged to
            // satisfy `clippy::match_same_arms`. The wire bodies are
            // intentionally identical — the discriminating type byte is
            // emitted before this match arm runs.
            Self::GetMetadata {
                request_id,
                scope,
                key,
            }
            | Self::DeleteMetadata {
                request_id,
                scope,
                key,
            } => Self::encode_get_or_delete_metadata(enc, *request_id, scope, key),
            Self::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => Self::encode_set_metadata(enc, *request_id, scope, key, value),
            Self::ListMetadata { request_id, scope } => {
                Self::encode_list_metadata(enc, *request_id, scope);
            }
            Self::SubscribeMetadata { scope, key } => {
                Self::encode_subscribe_metadata(enc, scope, key);
            }
            Self::MetadataChanged {
                scope,
                key,
                value,
                actor,
            } => {
                Self::encode_metadata_changed(enc, scope, key, value.as_deref());
                if let Some(actor) = actor {
                    enc.write_field_with(field::metadata_changed::ACTOR, |e| {
                        encode_actor_ref(actor, e);
                    });
                }
            }
            Self::MetadataValue { request_id, value } => {
                Self::encode_metadata_value(enc, *request_id, value.as_deref());
            }
            Self::MetadataKeys { request_id, keys } => {
                Self::encode_metadata_keys(enc, *request_id, keys);
            }
            Self::ListDirectory {
                request_id,
                path,
                host,
            } => {
                encode_list_directory(enc, *request_id, path, host.as_ref());
            }
            Self::DirectoryListing { request_id, result } => {
                encode_directory_listing(enc, *request_id, result);
            }
            Self::PathQuery {
                request_id,
                root,
                query,
                recursive,
                host,
            } => {
                encode_query(enc, *request_id, root, query, *recursive, host.as_ref());
            }
            Self::PathResults { request_id, result } => {
                encode_results(enc, *request_id, result);
            }
            Self::SpawnResource {
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
            } => {
                Self::encode_spawn_terminal_request(enc, *request_id, *group);
                Self::encode_spawn_terminal_process(
                    enc,
                    command.as_deref(),
                    cwd.as_deref(),
                    env.as_deref(),
                    term.as_deref(),
                );
                Self::encode_spawn_terminal_placement(
                    enc,
                    satellite.as_ref(),
                    owner_terminal.as_ref(),
                    agent_session.as_deref(),
                    *initial_size,
                );
                if let Some(resource) = resource.as_deref() {
                    Self::encode_spawn_terminal_resource(enc, resource);
                }
            }
            Self::ResourceSpawned { request_id, result } => {
                Self::encode_terminal_spawned(enc, *request_id, result);
            }
            Self::MoveResource {
                request_id,
                terminal,
                owner_terminal,
            } => Self::encode_move_terminal(enc, *request_id, terminal, owner_terminal),
            Self::ResourceMoved { request_id, result } => {
                Self::encode_terminal_moved(enc, *request_id, result);
            }
            Self::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => {
                Self::encode_terminal_closed(enc, terminal_id, *exit_status, *reason);
                if let Some(signal) = signal {
                    write_i32_field(enc, field::terminal_closed::SIGNAL, *signal);
                }
            }
            Self::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                cell_px,
            } => Self::encode_terminal_resize(enc, terminal_id, *cols, *rows, *cell_px),
            Self::Command {
                request_id,
                command,
            } => Self::encode_command_frame(enc, *request_id, command),
            Self::CommandResult { request_id, result } => {
                Self::encode_command_result_frame(enc, *request_id, result);
            }
            Self::SubscribeEvents {
                terminal,
                after_seq,
            } => {
                Self::encode_subscribe_events(enc, terminal.as_ref());
                if let Some(after_seq) = after_seq {
                    u64_field(enc, field::subscribe_events::AFTER_SEQ, *after_seq);
                }
            }
            Self::Event {
                terminal,
                event,
                stamp,
            } => {
                Self::encode_event(enc, terminal.as_ref(), event);
                if let Some(stamp) = stamp {
                    encode_event_stamp(enc, stamp);
                }
            }
        }
    }
}
