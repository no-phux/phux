//! Inbound frame decoding and dispatch.

use super::{
    ControlError, ControlPlane, EngineEvent, Event, FrameKind, HistoryRejectionReason,
    HistoryUnavailableReason, WireRejection, WireTombstone,
};

const fn allowed_before_handshake(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::HelloOk { .. }
            | FrameKind::Error { .. }
            | FrameKind::Detached { .. }
            | FrameKind::Ping { .. }
            | FrameKind::Pong { .. }
    )
}

impl ControlPlane {
    // ----- frames in ------------------------------------------------

    /// Decode exactly one SPEC section 5 frame under the negotiated limits
    /// and feed it.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        let decoded = self.decode_limits().map_or_else(
            || FrameKind::decode(bytes),
            |limits| FrameKind::decode_with_limits(bytes, limits),
        );
        let (frame, tail) = decoded
            .map_err(|error| ControlError::Protocol(format!("invalid protocol frame: {error}")))?;
        if !tail.is_empty() {
            return Err(ControlError::Protocol(
                "protocol message contained trailing bytes".to_owned(),
            ));
        }
        self.feed(frame)
    }

    /// Apply one decoded inbound frame.
    pub fn feed(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        if !self.handshake_ready && !allowed_before_handshake(&frame) {
            return Err(ControlError::Protocol(
                "server frame arrived before HELLO_OK".to_owned(),
            ));
        }
        let Some(frame) = self.feed_session_frame(frame)? else {
            return Ok(());
        };
        self.feed_resource_frame(frame)
    }

    fn feed_session_frame(&mut self, frame: FrameKind) -> Result<Option<FrameKind>, ControlError> {
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => self.hello_ok(
                (protocol_major, protocol_minor, protocol_patch),
                &server_id,
                server_caps.features,
                server_caps.layers,
                selected_profile,
                bootstrap_limits,
            )?,
            FrameKind::Ping { nonce } => self.queue_frame(&FrameKind::Pong { nonce }),
            // The answer to a liveness probe: its arrival was the point.
            FrameKind::Pong { .. } => {}
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => self.attached(attach_id, &snapshot)?,
            FrameKind::AttachReady { attach_id } => self.attach_ready(attach_id)?,
            FrameKind::Error {
                request_id,
                code,
                message,
            } => self.server_error(request_id, code, message)?,
            FrameKind::Detached { reason, message } => self.detached(reason, &message)?,
            frame => return Ok(Some(frame)),
        }
        Ok(None)
    }

    fn feed_resource_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        match frame {
            FrameKind::Bell { terminal_id } => {
                self.push_event(Event::Bell { terminal_id });
                Ok(())
            }
            FrameKind::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => {
                self.attach_terminals.remove(&terminal_id);
                self.apply_engine(EngineEvent::Closed {
                    terminal_id: terminal_id.clone(),
                    exit_status,
                    signal,
                    reason,
                })?;
                if !self.close_pane(&terminal_id, exit_status, signal, reason) {
                    // A manual binding may own the admission correlation;
                    // still surface the authoritative close exactly once.
                    self.push_event(Event::TerminalClosed {
                        terminal_id,
                        exit_status,
                        signal,
                        reason,
                    });
                }
                Ok(())
            }
            FrameKind::ResourceSpawned { request_id, result } => {
                if !self.resource_spawned(request_id, &result) {
                    self.push_event(Event::Frame(Box::new(FrameKind::ResourceSpawned {
                        request_id,
                        result,
                    })));
                }
                Ok(())
            }
            FrameKind::CommandResult { request_id, result } => {
                self.command_result(request_id, result)
            }
            FrameKind::Event {
                terminal, event, ..
            } => self.agent_event(terminal, event),
            frame => self.feed_stream_frame(frame),
        }
    }

    pub(super) fn feed_stream_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        let event = match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => EngineEvent::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            },
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => EngineEvent::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: payload.to_vec(),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => EngineEvent::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: history_cursor.map(|cursor| cursor.to_vec()),
            },
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => EngineEvent::Tombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            },
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => EngineEvent::Output {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes: bytes.to_vec(),
            },
            frame => return self.feed_history_frame(frame),
        };
        self.apply_engine(event)
    }

    pub(super) fn feed_history_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        let event = match frame {
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => EngineEvent::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                rows,
                cursor: cursor.to_vec(),
                next_cursor: next_cursor.map(|cursor| cursor.to_vec()),
                payload: payload.to_vec(),
            },
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => EngineEvent::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor: cursor.to_vec(),
                reason: history_unavailable_reason(reason)?,
            },
            FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => EngineEvent::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor: cursor.to_vec(),
                reason: history_rejection_reason(reason)?,
                required_bytes,
                required_rows,
            },
            other => {
                self.push_event(Event::Frame(Box::new(other)));
                return Ok(());
            }
        };
        self.apply_engine(event)
    }
}

fn history_unavailable_reason(
    reason: WireTombstone,
) -> Result<HistoryUnavailableReason, ControlError> {
    Ok(match reason {
        WireTombstone::Stale => HistoryUnavailableReason::Stale,
        WireTombstone::Pruned => HistoryUnavailableReason::Pruned,
        WireTombstone::Reset => HistoryUnavailableReason::Reset,
        WireTombstone::Resize => HistoryUnavailableReason::Resize,
        WireTombstone::Expired => HistoryUnavailableReason::Expired,
        WireTombstone::Released => HistoryUnavailableReason::Released,
        WireTombstone::Limit => HistoryUnavailableReason::Limit,
        WireTombstone::CodecFailure => HistoryUnavailableReason::CodecFailure,
        _ => {
            return Err(ControlError::Protocol(
                "unsupported history tombstone reason".to_owned(),
            ));
        }
    })
}

fn history_rejection_reason(reason: WireRejection) -> Result<HistoryRejectionReason, ControlError> {
    Ok(match reason {
        WireRejection::ZeroLimit => HistoryRejectionReason::ZeroLimit,
        WireRejection::TooSmall => HistoryRejectionReason::TooSmall,
        WireRejection::Busy => HistoryRejectionReason::Busy,
        _ => {
            return Err(ControlError::Protocol(
                "unsupported history rejection reason".to_owned(),
            ));
        }
    })
}
