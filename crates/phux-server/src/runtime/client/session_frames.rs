//! Handling HELLO, ATTACH, DETACH, and COMMAND frames, history requests, and the frame-kind predicates.

use super::{
    AttachIdVerdict, BootstrapProfile, ClientId, Command, CommandDispatch, CommandDispatchOutcome,
    ConnectionClose, ConnectionEnding, DetachReason, ErrorCode, FrameDispatch, FrameFlow,
    FrameKind, FrameReader, HelloRequest, NegotiatedConnection, Outbound, SharedState,
    WireResourceId, classify_attach_id, debug, detach_on_request, handle_attach, mpsc,
    negotiate_hello, oneshot, warn,
};

/// The `HISTORY_REQUEST` frame's payload.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) struct HistoryPageRequest {
    pub(super) terminal_id: WireResourceId,
    pub(super) stream_id: phux_protocol::ids::StreamId,
    pub(super) bootstrap_id: phux_protocol::ids::BootstrapId,
    pub(super) cursor: bytes::Bytes,
    pub(super) max_bytes: u32,
    pub(super) max_rows: u32,
}

#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
/// The Terminal facet a `HISTORY_REQUEST` pages against: `None` for an
/// unknown terminal or a resource of another kind.
pub(super) fn history_terminal(
    state: &SharedState,
    terminal_id: &WireResourceId,
) -> Option<crate::terminal_actor::TerminalHandle> {
    let handle = state.with(|server| {
        server
            .terminal_from_wire(terminal_id)
            .and_then(|pane| server.resource_handle(pane).cloned())
    });
    let Some(handle) = handle else {
        warn!(?terminal_id, "HISTORY_REQUEST for an unknown terminal");
        return None;
    };
    match handle.terminal() {
        Ok(terminal) => Some(terminal.clone()),
        Err(error) => {
            warn!(?terminal_id, %error, "HISTORY_REQUEST for a non-Terminal resource");
            None
        }
    }
}

/// Answer one `HISTORY_REQUEST` from the pane's replica. SPEC L1 §4.5: every
/// failure tombstones the cursor rather than sending an uncorrelated `ERROR`.
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(super) async fn serve_history_request(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &mpsc::Sender<Outbound>,
    selection: NegotiatedConnection,
    request: HistoryPageRequest,
) {
    use phux_protocol::wire::frame::HistoryTombstoneReason;

    let HistoryPageRequest {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor,
        max_bytes,
        max_rows,
    } = request;
    let tombstone = |reason| {
        Outbound::Frame(FrameKind::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            reason,
        })
    };
    if !matches!(
        selection.profile,
        BootstrapProfile::NativeState {
            codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
            ..
        }
    ) {
        warn!(
            ?terminal_id,
            "HISTORY_REQUEST requires negotiated native snapshot v1"
        );
        let _ = out_tx
            .send(tombstone(HistoryTombstoneReason::CodecFailure))
            .await;
        return;
    }
    let Some(terminal) = history_terminal(state, &terminal_id) else {
        let _ = out_tx
            .send(tombstone(HistoryTombstoneReason::Released))
            .await;
        return;
    };
    let Ok(permit) = out_tx.clone().reserve_owned().await else {
        return;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    if terminal
        .native_history
        .send(crate::terminal_actor::NativeHistoryRequest {
            permit,
            owner: client_id.0,
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            max_bytes,
            max_rows,
            limits: selection.limits,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return;
    }
    let Ok(reply) = reply_rx.await else {
        return;
    };
    match reply.result {
        Ok(frame) => {
            reply.permit.send(Outbound::Frame(frame));
        }
        Err(error) => {
            // Mirrors the actor's own in-band mapping.
            let reason = match error {
                crate::native_state::NativeStateError::OutOfMemory
                | crate::native_state::NativeStateError::OutOfSpace { .. }
                | crate::native_state::NativeStateError::LimitExceeded => {
                    HistoryTombstoneReason::Limit
                }
                _ => HistoryTombstoneReason::CodecFailure,
            };
            warn!(
                %error,
                ?terminal_id,
                "native history request failed; tombstoning the cursor"
            );
            reply.permit.send(tombstone(reason));
        }
    }
}

impl<R: FrameReader> FrameDispatch<'_, R> {
    /// HELLO: negotiate the connection, then apply what it selected.
    pub(super) async fn hello(self, hello: HelloRequest) -> FrameFlow {
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
    pub(super) async fn attach(self, request: AttachFrame) -> FrameFlow {
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
        debug!(
            client_id = ?self.client_id,
            attach_id,
            profile = ?selection.profile,
            chunk_limit = selection.limits.max_chunk_bytes(),
            history_page_limit = selection.limits.max_history_page_bytes(),
            "ATTACH with immutable bootstrap selection",
        );
        let attach_started = std::time::Instant::now();
        // QUIC multi-stream: content streams start at STREAM_BIND.
        let defer_subscription = selection.quic_streams();
        handle_attach(
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
            selection.profile,
            selection.limits,
            self.root_token,
            &mut self.plumbing.output_pumps,
            self.token,
            defer_subscription,
        )
        .await;
        crate::perf::ATTACH_HANDLE.record_elapsed(attach_started);
        FrameFlow::Continue(())
    }

    /// Session DETACH: detach, and end every Terminal stream too.
    pub(super) async fn detach(self) {
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
    pub(super) async fn command(self, request_id: u32, command: Command) -> FrameFlow {
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
}

/// A frame kind a client may not send in the negotiated phase: close.
pub(super) fn direction_invalid(client_id: ClientId, other: &FrameKind) -> FrameFlow {
    warn!(?client_id, kind = ?other, "direction-invalid client frame; closing");
    FrameFlow::Break(ConnectionEnding::Violation(ConnectionClose {
        attached_reason: Some("direction-invalid frame"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::InvalidCommand,
        message: format!("frame is not valid from a client in the negotiated phase: {other:?}"),
    }))
}

/// The fields of one `ATTACH` frame.
pub(super) struct AttachFrame {
    pub(super) attach_id: u32,
    pub(super) target: phux_protocol::wire::frame::AttachTarget,
    pub(super) viewport: phux_protocol::wire::frame::ViewportInfo,
    pub(super) request_scrollback: bool,
    pub(super) scrollback_limit_lines: u32,
    pub(super) role_policy: Option<phux_protocol::wire::frame::RolePolicy>,
}

/// `INPUT_*` frames: routed to the Terminal's input path.
pub(super) const fn is_input_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::InputKey { .. }
            | FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputPaste { .. }
            | FrameKind::InputTerminalReply { .. }
    )
}

/// Frames about an attached Terminal's size or content stream.
pub(super) const fn is_stream_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::ViewportResize { .. }
            | FrameKind::ResizeTerminal { .. }
            | FrameKind::FrameAck { .. }
            | FrameKind::HistoryRequest { .. }
    )
}

/// L3 metadata and event-subscription frames.
pub(super) const fn is_metadata_frame(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::GetMetadata { .. }
            | FrameKind::SetMetadata { .. }
            | FrameKind::DeleteMetadata { .. }
            | FrameKind::ListMetadata { .. }
            | FrameKind::SubscribeMetadata { .. }
            | FrameKind::SubscribeEvents { .. }
    )
}
