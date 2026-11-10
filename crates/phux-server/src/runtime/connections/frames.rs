mod metadata;

use super::commands::{CommandDispatch, CommandDispatchOutcome};
use super::dispatch::{ConnectionEnding, FrameDispatch, FrameFlow};
use super::hello::{HelloRequest, negotiate_hello};
use super::plumbing::{
    AttachIdVerdict, ConnectionClose, classify_attach_id, detach_on_request, reply_pong,
};
use crate::runtime::apps::ResourceFrame;
use crate::runtime::apps::spawn::SpawnRequest;
use crate::runtime::apps::terminal::attach::handle_move_terminal;
use crate::runtime::apps::terminal::commands::handle_viewport_resize;
use crate::runtime::apps::terminal::terminal_streams::{
    AttachFrame, direction_invalid, is_input_frame, is_metadata_frame, is_stream_frame,
};
use crate::runtime::session_attach::handle_attach;
use crate::transport::FrameReader;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{Command, DetachReason, ErrorCode, FrameKind};
use tracing::debug;

impl<R: FrameReader> FrameDispatch<'_, R> {
    /// Dispatch one admitted frame: one arm per client-to-server kind.
    pub(in crate::runtime) async fn dispatch(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => {
                let hello = HelloRequest {
                    client_name,
                    protocol_major,
                    protocol_minor,
                    protocol_patch,
                    client_caps,
                };
                return self.hello(hello).await;
            }
            FrameKind::Ping { nonce } => {
                reply_pong(&self.plumbing.out_tx, self.client_id, nonce).await;
            }
            FrameKind::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy,
            } => {
                let request = AttachFrame {
                    attach_id,
                    target,
                    viewport,
                    request_scrollback,
                    scrollback_limit_lines,
                    role_policy,
                };
                return self.attach(request).await;
            }
            FrameKind::Detach => self.detach().await,
            FrameKind::Command {
                request_id,
                command,
            } => return self.command(request_id, command).await,
            frame if is_input_frame(&frame) || is_stream_frame(&frame) => {
                return self.resource_frame(frame).await;
            }
            frame if is_metadata_frame(&frame) => return self.metadata(frame).await,
            frame => return self.resource(frame).await,
        }
        FrameFlow::Continue(())
    }

    /// HELLO: negotiate the connection, then apply what it selected.
    async fn hello(self, hello: HelloRequest) -> FrameFlow {
        if let Err(close) = negotiate_hello(
            self.state,
            self.client_id,
            &self.plumbing.out_tx,
            hello,
            self.negotiated,
            self.transport,
            self.route_supports_quic_streams,
        )
        .await
        {
            return FrameFlow::Break(ConnectionEnding::Violation(close));
        }
        if let Some(selection) = self.negotiated.as_ref() {
            self.plumbing.set_compression(selection.compression);
            // The client learns the bit from HELLO_OK, so no Terminal
            // stream can have opened before this.
            if selection.quic_streams() {
                *self.stream_events = self.reader.take_stream_events();
            }
        }
        FrameFlow::Continue(())
    }

    /// ATTACH: vet the attach id, then attach under the negotiated
    /// bootstrap selection.
    async fn attach(self, request: AttachFrame) -> FrameFlow {
        let attach_id = request.attach_id;
        match classify_attach_id(attach_id, self.used_attach_ids, self.client_id) {
            AttachIdVerdict::Fresh => {}
            AttachIdVerdict::Reused => {
                crate::runtime::send_error(
                    &self.plumbing.out_tx,
                    ErrorCode::MalformedMessage,
                    &format!("ATTACH attach_id {attach_id} was already used on this connection"),
                )
                .await;
                return FrameFlow::Continue(());
            }
            AttachIdVerdict::Reserved => {
                return FrameFlow::Break(ConnectionEnding::Violation(ConnectionClose {
                    attached_reason: Some("zero ATTACH id"),
                    detach_reason: DetachReason::ProtocolError,
                    code: ErrorCode::MalformedMessage,
                    message: "ATTACH attach_id must be nonzero".to_owned(),
                }));
            }
        }
        let Some(selection) = self.negotiated.as_ref() else {
            return FrameFlow::Continue(());
        };
        // SESSION ATTACH preflights actual membership: only a session with
        // Terminals requires a Terminal profile.
        let profile = selection.profile;
        debug!(
            client_id = ?self.client_id,
            attach_id,
            ?profile,
            chunk_limit = selection.limits.max_chunk_bytes(),
            history_page_limit = selection.limits.max_history_page_bytes(),
            "ATTACH with immutable bootstrap selection",
        );
        let attach_started = std::time::Instant::now();
        // QUIC multi-stream: content streams start at STREAM_BIND.
        let defer_subscription = selection.quic_streams();
        let published = handle_attach(
            self.state,
            self.client_id,
            attach_id,
            request.target,
            request.viewport,
            request.request_scrollback,
            request.scrollback_limit_lines,
            request.role_policy,
            &self.plumbing.out_tx,
            selection.client_caps,
            profile,
            selection.limits,
            self.root_token,
            &mut self.plumbing.output_pumps,
            self.token,
            defer_subscription,
        )
        .await;
        if published && defer_subscription {
            // A successful aggregate replacement fences its prior pumps. End
            // their exact bindings too, so a fresh STREAM_BIND cannot race an
            // old stream-end event into unsubscribing the new generation.
            self.plumbing.drop_all_stream_bindings().await;
        }
        crate::perf::ATTACH_HANDLE.record_elapsed(attach_started);
        FrameFlow::Continue(())
    }

    /// Session DETACH: detach, and end every Terminal stream too.
    async fn detach(self) {
        detach_on_request(
            self.state,
            self.client_id,
            &self.plumbing.out_tx,
            &mut self.plumbing.output_pumps,
        )
        .await;
        self.plumbing.drop_all_stream_bindings().await;
    }

    /// COMMAND: run it on this connection's dispatch; a command that
    /// detached a Terminal drops that Terminal's stream binding too.
    async fn command(self, request_id: u32, command: Command) -> FrameFlow {
        let Some(selection) = *self.negotiated else {
            return FrameFlow::Continue(());
        };
        let command_outcome = (CommandDispatch {
            state: self.state,
            client_id: self.client_id,
            out_tx: &self.plumbing.out_tx,
            input_lane: self.input_lane,
            token: self.token,
            root_token: self.root_token,
            selection,
            command_tasks: self.command_tasks,
            input_receipts: self.input_receipts,
            input_receipt_slots: self.input_receipt_slots,
            input_credits: self.input_credits,
            held_commands: self.held_commands,
            service_http: self.service_http,
        })
        .run(request_id, command)
        .await;
        match command_outcome {
            CommandDispatchOutcome::Completed(Some(terminal_id)) => {
                // Already unsubscribed; drop the stream binding too.
                self.plumbing.drop_stream_binding(&terminal_id).await;
            }
            CommandDispatchOutcome::Completed(None) => {}
            CommandDispatchOutcome::Cancelled => {
                return FrameFlow::Break(ConnectionEnding::Cancelled);
            }
        }
        FrameFlow::Continue(())
    }

    /// A frame about one resource's stream or input goes to the app serving
    /// that resource's kind (see [`is_stream_frame`] and [`is_input_frame`]).
    /// `VIEWPORT_RESIZE` names no resource: it is the connection's size vote
    /// for every Terminal it is attached to.
    async fn resource_frame(self, frame: FrameKind) -> FrameFlow {
        if let FrameKind::ViewportResize { viewport } = frame {
            debug!(
                client_id = ?self.client_id,
                cols = viewport.cols,
                rows = viewport.rows,
                "VIEWPORT_RESIZE"
            );
            handle_viewport_resize(self.state, self.client_id, &viewport);
            return FrameFlow::Continue(());
        }
        let ctx = ResourceFrame {
            state: self.state,
            client_id: self.client_id,
            token: self.token,
            input_lane: self.input_lane,
            plumbing: self.plumbing,
            negotiated: *self.negotiated,
            input_credits: self.input_credits,
        };
        super::resource_frames::serve(ctx, frame).await
    }

    /// Resource and filesystem frames; any other kind is not valid from a
    /// client and closes the connection.
    async fn resource(self, frame: FrameKind) -> FrameFlow {
        let (state, client_id) = (self.state, self.client_id);
        match frame {
            FrameKind::ListDirectory {
                request_id,
                path,
                host,
            } => {
                crate::runtime::directory::handle_list_directory(
                    state,
                    client_id,
                    crate::runtime::directory::ListRequest {
                        request_id,
                        path,
                        host,
                    },
                    &self.plumbing.out_tx,
                );
            }
            FrameKind::PathQuery {
                request_id,
                root,
                query,
                recursive,
                host,
            } => {
                crate::runtime::path_search::handle_path_query(
                    state,
                    client_id,
                    crate::runtime::path_search::PathRequest {
                        request_id,
                        root,
                        query,
                        recursive,
                        host,
                    },
                    &self.plumbing.out_tx,
                );
            }
            frame @ FrameKind::SpawnResource { .. } => return self.spawn_resource(frame).await,
            FrameKind::MoveResource {
                request_id,
                terminal,
                owner_terminal,
            } => {
                handle_move_terminal(
                    state,
                    client_id,
                    request_id,
                    terminal,
                    owner_terminal,
                    &self.plumbing.out_tx,
                )
                .await;
            }
            other => return direction_invalid(client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// `SPAWN_RESOURCE`, with the kinds the connection's `HELLO` decodes.
    async fn spawn_resource(self, frame: FrameKind) -> FrameFlow {
        let (state, client_id) = (self.state, self.client_id);
        let FrameKind::SpawnResource {
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
        } = frame
        else {
            return direction_invalid(self.client_id, &frame);
        };
        let Some(selection) = self.negotiated.as_ref() else {
            return FrameFlow::Continue(());
        };
        crate::runtime::idempotent_create::handle_spawn_resource(
            state,
            client_id,
            request_id,
            SpawnRequest {
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
                decodes: selection.client_caps.resource_kinds,
            },
            &self.plumbing.out_tx,
            selection.profile,
            selection.limits,
            self.root_token,
            self.token,
            &mut self.plumbing.output_pumps,
            // QUIC multi-stream: content starts at STREAM_BIND (L1 §4.9).
            selection.quic_streams(),
        )
        .await;
        FrameFlow::Continue(())
    }
}

/// The resource a stream or input frame is about; `None` for one that
/// names no resource.
pub(super) const fn frame_resource(frame: &FrameKind) -> Option<&WireResourceId> {
    match frame {
        FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::InputTerminalReply { terminal_id, .. }
        | FrameKind::ResizeTerminal { terminal_id, .. }
        | FrameKind::FrameAck { terminal_id, .. }
        | FrameKind::HistoryRequest { terminal_id, .. } => Some(terminal_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
