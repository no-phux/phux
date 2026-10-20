//! Handling input, terminal-stream, metadata, and resource frames.

use super::{
    ConnectionEnding, ErrorCode, FrameDispatch, FrameFlow, FrameKind, FrameReader,
    HistoryPageRequest, InputStalled, SpawnRequest, TerminalInput, WireResourceId, debug,
    direction_invalid, dispatch_terminal_reply, handle_delete_metadata, handle_frame_ack,
    handle_get_metadata, handle_list_metadata, handle_move_terminal, handle_set_metadata,
    handle_subscribe_events, handle_subscribe_metadata, handle_terminal_resize,
    handle_viewport_resize, is_agent_session, route_client_input, serve_history_request,
};

impl<R: FrameReader> FrameDispatch<'_, R> {
    /// `INPUT_*`: route the event (see [`is_input_frame`]).
    pub(super) async fn input(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::InputKey { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Key(event))
                    .await;
            }
            FrameKind::InputMouse { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Mouse(event))
                    .await;
            }
            FrameKind::InputFocus { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Focus(event))
                    .await;
            }
            FrameKind::InputPaste { terminal_id, event } => {
                return self
                    .route_input(terminal_id, TerminalInput::Paste(event))
                    .await;
            }
            FrameKind::InputTerminalReply { terminal_id, bytes } => {
                let Some(selection) = self.negotiated.as_ref() else {
                    return FrameFlow::Continue(());
                };
                dispatch_terminal_reply(
                    self.client_id,
                    *selection,
                    &terminal_id,
                    bytes,
                    &self.plumbing.out_tx,
                )
                .await;
            }
            other => return direction_invalid(self.client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// Route one structured input event. Lane-bound input first takes a
    /// pane input credit (ADR-0144), and while the pane is saturated this
    /// connection reads no further frame: the event waits rather than being
    /// dropped, and the transport pushes back on the sender. A pane that
    /// stays saturated past the stall limit refuses the event with an
    /// uncorrelated `ERROR(RESOURCE_EXHAUSTED)`.
    pub(super) async fn route_input(
        self,
        terminal_id: WireResourceId,
        input: TerminalInput,
    ) -> FrameFlow {
        let lane = self.input_lane.filter(|_| terminal_id.is_local());
        let credit = match lane {
            Some(_) => {
                let acquired = tokio::select! {
                    biased;
                    () = self.token.cancelled() => {
                        return FrameFlow::Break(ConnectionEnding::Cancelled);
                    }
                    acquired = self.input_credits.acquire(self.state, &terminal_id) => acquired,
                };
                let Ok(credit) = acquired else {
                    crate::runtime::send_error(
                        &self.plumbing.out_tx,
                        ErrorCode::ResourceExhausted,
                        &InputStalled::message(&terminal_id),
                    )
                    .await;
                    return FrameFlow::Continue(());
                };
                credit
            }
            None => None,
        };
        route_client_input(self.state, lane, self.client_id, terminal_id, input, credit).await;
        FrameFlow::Continue(())
    }

    /// Frames about one attached Terminal's content stream (see
    /// [`is_stream_frame`]).
    pub(super) async fn stream_frame(self, frame: FrameKind) -> FrameFlow {
        match frame {
            FrameKind::ViewportResize { viewport } => {
                debug!(
                    client_id = ?self.client_id,
                    cols = viewport.cols,
                    rows = viewport.rows,
                    "VIEWPORT_RESIZE"
                );
                handle_viewport_resize(self.state, self.client_id, &viewport);
            }
            FrameKind::ResizeTerminal {
                terminal_id,
                cols,
                rows,
                cell_px,
            } => {
                handle_terminal_resize(
                    self.state,
                    self.client_id,
                    &terminal_id,
                    (cols, rows),
                    cell_px,
                );
            }
            // ADR-0103 §4: an agent-session stream is raw-profile with no
            // history beyond its bootstrap. Both refusals keep the connection.
            FrameKind::FrameAck {
                ref terminal_id, ..
            } if is_agent_session(self.state, terminal_id) => {
                crate::runtime::send_error(
                    &self.plumbing.out_tx,
                    ErrorCode::MalformedMessage,
                    "FRAME_ACK is not valid on an agent-session stream",
                )
                .await;
            }
            FrameKind::HistoryRequest {
                ref terminal_id, ..
            } if is_agent_session(self.state, terminal_id) => {
                crate::runtime::send_error(
                    &self.plumbing.out_tx,
                    ErrorCode::WrongResourceKind,
                    "an agent-session stream retains no history beyond its bootstrap",
                )
                .await;
            }
            FrameKind::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => {
                handle_frame_ack(
                    self.state,
                    self.client_id,
                    &terminal_id,
                    stream_id,
                    bootstrap_id,
                    seq,
                );
            }
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            FrameKind::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => {
                let Some(selection) = self.negotiated.as_ref() else {
                    return FrameFlow::Continue(());
                };
                // Terminal-content replies ride the bound stream when one
                // exists (proto.md §4.9); control otherwise.
                let history_tx = self.plumbing.sender_for(&terminal_id);
                serve_history_request(
                    self.state,
                    self.client_id,
                    &history_tx,
                    *selection,
                    HistoryPageRequest {
                        terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor,
                        max_bytes,
                        max_rows,
                    },
                )
                .await;
            }
            other => return direction_invalid(self.client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// L3 metadata frames (see [`is_metadata_frame`]).
    pub(super) async fn metadata(self, frame: FrameKind) -> FrameFlow {
        let (state, client_id, out_tx) = (self.state, self.client_id, &self.plumbing.out_tx);
        match frame {
            FrameKind::GetMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_get_metadata(state, client_id, request_id, &scope, &key, out_tx).await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } if crate::runtime::approvals::is_decision(&scope, &key) => {
                crate::runtime::approvals::decide(
                    state, client_id, request_id, &key, &value, out_tx,
                )
                .await;
            }
            FrameKind::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                handle_set_metadata(
                    state,
                    client_id,
                    request_id,
                    &scope,
                    &key,
                    value,
                    self.root_token,
                );
            }
            FrameKind::DeleteMetadata {
                request_id,
                scope,
                key,
            } => {
                handle_delete_metadata(state, client_id, request_id, &scope, &key);
            }
            FrameKind::ListMetadata { request_id, scope } => {
                handle_list_metadata(state, client_id, request_id, &scope, out_tx).await;
            }
            FrameKind::SubscribeMetadata { scope, key } => {
                handle_subscribe_metadata(state, client_id, scope, key, out_tx);
            }
            FrameKind::SubscribeEvents {
                terminal,
                after_seq,
            } => {
                handle_subscribe_events(state, client_id, terminal, after_seq, out_tx);
            }
            other => return direction_invalid(client_id, &other),
        }
        FrameFlow::Continue(())
    }

    /// Resource and filesystem frames; any other kind is not valid from a
    /// client and closes the connection.
    pub(super) async fn resource(self, frame: FrameKind) -> FrameFlow {
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
            FrameKind::SpawnResource {
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
            }
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
}
