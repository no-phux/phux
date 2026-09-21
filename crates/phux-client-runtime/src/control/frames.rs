//! Inbound frame decoding and dispatch.

use super::{
    ControlError, ControlPlane, EngineEvent, Event, FrameKind, HistoryRejectionReason,
    HistoryUnavailableReason, WireRejection, WireTombstone,
};

enum ClassifiedFrame {
    Engine(EngineEvent),
    Other(FrameKind),
}

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
    // ----- the queued inbound lane ----------------------------------

    /// Retain one frame for the consumer to feed itself, under
    /// [`InboundDelivery::Queued`](super::InboundDelivery::Queued).
    ///
    /// Overflowing either ceiling is a protocol-level failure, exactly as
    /// it was when a socket-owning embedder enforced the same bounds: a
    /// consumer that stopped draining cannot be allowed to grow the queue
    /// without limit.
    pub fn queue_inbound(&mut self, frame: Vec<u8>) -> Result<(), ControlError> {
        let bytes = self.inbound_bytes.saturating_add(frame.len());
        if self.inbound.len() >= super::MAX_QUEUED_INBOUND_FRAMES
            || bytes > super::MAX_QUEUED_INBOUND_BYTES
        {
            return Err(ControlError::Protocol(
                "inbound frame queue overflowed; the consumer stopped draining".to_owned(),
            ));
        }
        self.inbound_bytes = bytes;
        self.inbound.push(frame);
        Ok(())
    }

    /// Drain the retained inbound frames.
    #[must_use]
    pub fn take_inbound(&mut self) -> Vec<Vec<u8>> {
        self.inbound_bytes = 0;
        std::mem::take(&mut self.inbound)
    }

    /// Whether any retained inbound frame is waiting.
    #[must_use]
    pub const fn has_inbound(&self) -> bool {
        !self.inbound.is_empty()
    }

    // ----- frames in ------------------------------------------------

    /// Decode exactly one SPEC section 5 frame under the negotiated limits
    /// and feed it.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        let frame = self.decode_frame(bytes)?;
        self.feed(frame)
    }

    /// Decode a transport read's complete frames in order, applying each
    /// contiguous run of engine events as one projection batch.
    pub fn feed_bytes_batch(&mut self, frames: &[Vec<u8>]) -> Result<(), ControlError> {
        let mut engine_events = Vec::new();
        let mut deferred_error = None;
        for bytes in frames {
            let frame = match self.decode_frame(bytes) {
                Ok(frame) => frame,
                Err(error) => {
                    return self.error_after_engine_batch(
                        &mut engine_events,
                        &mut deferred_error,
                        error,
                    );
                }
            };
            self.queue_or_feed_frame(frame, &mut engine_events, &mut deferred_error)?;
        }
        Self::continue_after_nonfatal(
            &mut deferred_error,
            self.flush_engine_batch(&mut engine_events),
        )?;
        deferred_error.map_or(Ok(()), Err)
    }

    fn decode_frame(&self, bytes: &[u8]) -> Result<FrameKind, ControlError> {
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
        Ok(frame)
    }

    fn queue_or_feed_frame(
        &mut self,
        frame: FrameKind,
        engine_events: &mut Vec<EngineEvent>,
        deferred_error: &mut Option<ControlError>,
    ) -> Result<(), ControlError> {
        if !self.handshake_ready && !allowed_before_handshake(&frame) {
            let error = ControlError::Protocol("server frame arrived before HELLO_OK".to_owned());
            return self.error_after_engine_batch(engine_events, deferred_error, error);
        }
        let classified = match classify_engine_frame(frame) {
            Ok(classified) => classified,
            Err(error) => {
                return self.error_after_engine_batch(engine_events, deferred_error, error);
            }
        };
        match classified {
            ClassifiedFrame::Engine(event) => {
                engine_events.push(event);
                Ok(())
            }
            ClassifiedFrame::Other(frame) => {
                Self::continue_after_nonfatal(
                    deferred_error,
                    self.flush_engine_batch(engine_events),
                )?;
                Self::continue_after_nonfatal(deferred_error, self.feed(frame))
            }
        }
    }

    fn flush_engine_batch(
        &mut self,
        engine_events: &mut Vec<EngineEvent>,
    ) -> Result<(), ControlError> {
        self.apply_engine_events(std::mem::take(engine_events))
    }

    fn error_after_engine_batch(
        &mut self,
        engine_events: &mut Vec<EngineEvent>,
        deferred_error: &mut Option<ControlError>,
        error: ControlError,
    ) -> Result<(), ControlError> {
        if let Err(pending_error) =
            Self::continue_after_nonfatal(deferred_error, self.flush_engine_batch(engine_events))
        {
            return Err(ControlError::prefer(Some(pending_error), error));
        }
        Err(ControlError::prefer(deferred_error.take(), error))
    }

    fn continue_after_nonfatal(
        deferred_error: &mut Option<ControlError>,
        result: Result<(), ControlError>,
    ) -> Result<(), ControlError> {
        match result {
            Ok(()) => Ok(()),
            Err(error @ ControlError::InvalidState(_)) => {
                *deferred_error = Some(ControlError::prefer(deferred_error.take(), error));
                Ok(())
            }
            Err(error) => Err(ControlError::prefer(deferred_error.take(), error)),
        }
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
            FrameKind::DirectoryListing { request_id, result } => {
                if let Some(result) = self.resolve_directory_listing(request_id, result) {
                    // Preserve manually correlated extension traffic for
                    // projection shims such as the stable C ABI.
                    self.push_event(Event::Frame(Box::new(FrameKind::DirectoryListing {
                        request_id,
                        result,
                    })));
                }
                Ok(())
            }
            FrameKind::Event {
                terminal, event, ..
            } => self.agent_event(terminal, event),
            frame => self.feed_stream_frame(frame),
        }
    }

    pub(super) fn feed_stream_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        match classify_engine_frame(frame)? {
            ClassifiedFrame::Engine(event) => self.apply_engine(event),
            ClassifiedFrame::Other(frame) => {
                self.push_event(Event::Frame(Box::new(frame)));
                Ok(())
            }
        }
    }
}

fn classify_engine_frame(frame: FrameKind) -> Result<ClassifiedFrame, ControlError> {
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
        other => return classify_history_frame(other),
    };
    Ok(ClassifiedFrame::Engine(event))
}

fn classify_history_frame(frame: FrameKind) -> Result<ClassifiedFrame, ControlError> {
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
        other => return Ok(ClassifiedFrame::Other(other)),
    };
    Ok(ClassifiedFrame::Engine(event))
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
