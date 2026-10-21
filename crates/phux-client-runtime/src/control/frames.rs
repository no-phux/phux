//! Inbound frame decoding and dispatch.

use bytes::Bytes;
use phux_protocol::wire::DecodeError;

use super::{AgentEvent, ControlError, ControlPlane, EngineEvent, Event, FrameKind};

mod classify;

use classify::{ClassifiedFrame, classify_engine_frame};

/// A decoded frame, `None` for a well-framed one carrying a value this build
/// does not define (logged and skipped: a newer 1.x server may send it), or
/// the protocol error malformed bytes are.
fn skip_unknown_value<T>(decoded: Result<T, DecodeError>) -> Result<Option<T>, ControlError> {
    match decoded {
        Ok(decoded) => Ok(Some(decoded)),
        Err(error) if error.is_unknown_value() => {
            tracing::warn!(%error, "skipping a server frame this build cannot read");
            Ok(None)
        }
        Err(error) => Err(ControlError::Protocol(format!(
            "invalid protocol frame: {error}"
        ))),
    }
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
    pub fn queue_inbound(&mut self, frame: impl Into<Bytes>) -> Result<(), ControlError> {
        self.queue_inbound_batch(vec![frame.into()])
    }

    /// Retain one transport read's frames whole.
    ///
    /// The driver stops reading while [`Self::has_inbound_room`] is false,
    /// so the ceilings are backpressure, not a failure: a batch read while
    /// there was room always lands, and the queue stays within the
    /// ceilings plus one read. Queueing past a full queue is a
    /// protocol-level failure: that caller ignored the backpressure.
    pub fn queue_inbound_batch(&mut self, frames: Vec<Bytes>) -> Result<(), ControlError> {
        if !self.has_inbound_room() {
            return Err(ControlError::Protocol(
                "inbound frame queue overflowed; the consumer stopped draining".to_owned(),
            ));
        }
        for frame in frames {
            self.inbound_bytes = self.inbound_bytes.saturating_add(frame.len());
            self.inbound.push(frame);
        }
        Ok(())
    }

    /// Whether the driver may read another batch for the consumer.
    #[must_use]
    pub const fn has_inbound_room(&self) -> bool {
        self.inbound.len() < super::MAX_QUEUED_INBOUND_FRAMES
            && self.inbound_bytes < super::MAX_QUEUED_INBOUND_BYTES
    }

    /// Drain the retained inbound frames.
    #[must_use]
    pub fn take_inbound(&mut self) -> Vec<Bytes> {
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
    /// and feed it. A well-framed frame carrying a value this build does not
    /// define (a newer 1.x server's frame type or enum value) is logged and
    /// skipped; malformed bytes are a protocol error.
    pub fn feed_bytes(&mut self, bytes: &[u8]) -> Result<(), ControlError> {
        self.decode_frame(bytes)?
            .map_or(Ok(()), |frame| self.feed(frame))
    }

    /// Decode a transport read's complete frames in order, applying each
    /// contiguous run of engine events as one projection batch.
    pub fn feed_bytes_batch(&mut self, frames: &[impl AsRef<[u8]>]) -> Result<(), ControlError> {
        let mut engine_events = Vec::new();
        let mut deferred_error = None;
        for bytes in frames {
            let frame = match self.decode_frame(bytes.as_ref()) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
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
        self.finish_batch(engine_events, deferred_error)
    }

    /// Feed already-decoded frames in order, applying each contiguous run
    /// of engine events as one projection batch: one owner-thread round trip
    /// and one publication per damaged terminal, however many frames the run
    /// carried. [`Self::feed_bytes_batch`] without the decode, for a binding
    /// that decodes on its own thread to run per-frame hooks first.
    pub fn feed_batch(
        &mut self,
        frames: impl IntoIterator<Item = FrameKind>,
    ) -> Result<(), ControlError> {
        let mut engine_events = Vec::new();
        let mut deferred_error = None;
        for frame in frames {
            self.queue_or_feed_frame(frame, &mut engine_events, &mut deferred_error)?;
        }
        self.finish_batch(engine_events, deferred_error)
    }

    fn finish_batch(
        &mut self,
        mut engine_events: Vec<EngineEvent>,
        mut deferred_error: Option<ControlError>,
    ) -> Result<(), ControlError> {
        Self::continue_after_nonfatal(
            &mut deferred_error,
            self.flush_engine_batch(&mut engine_events),
        )?;
        deferred_error.map_or(Ok(()), Err)
    }

    fn decode_frame(&self, bytes: &[u8]) -> Result<Option<FrameKind>, ControlError> {
        let decoded = self.decode_limits().map_or_else(
            || FrameKind::decode(bytes),
            |limits| FrameKind::decode_with_limits(bytes, limits),
        );
        let Some((frame, tail)) = skip_unknown_value(decoded)? else {
            return Ok(None);
        };
        if !tail.is_empty() {
            return Err(ControlError::Protocol(
                "protocol message contained trailing bytes".to_owned(),
            ));
        }
        Ok(Some(frame))
    }

    /// Decode a frame that is already a view of the socket buffer. Payload
    /// bytes alias that buffer instead of being copied.
    fn decode_shared_frame(&self, bytes: &Bytes) -> Result<Option<FrameKind>, ControlError> {
        let decoded = FrameKind::decode_shared(bytes, self.decode_limits());
        let Some((frame, tail)) = skip_unknown_value(decoded)? else {
            return Ok(None);
        };
        if !tail.is_empty() {
            return Err(ControlError::Protocol(
                "protocol message contained trailing bytes".to_owned(),
            ));
        }
        Ok(Some(frame))
    }

    /// [`Self::feed_bytes_batch`] for frames that are frozen socket views.
    pub fn feed_shared_batch(&mut self, frames: &[Bytes]) -> Result<(), ControlError> {
        let mut engine_events = Vec::new();
        let mut deferred_error = None;
        for bytes in frames {
            let frame = match self.decode_shared_frame(bytes) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
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
        self.finish_batch(engine_events, deferred_error)
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
        match classify_engine_frame(frame) {
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
            } => self.hello_ok(crate::handshake::HelloOk {
                protocol: (protocol_major, protocol_minor, protocol_patch),
                server_id,
                server_caps,
                profile: selected_profile,
                limits: bootstrap_limits,
            })?,
            FrameKind::Ping { nonce } => self.queue_frame(&FrameKind::Pong { nonce }),
            // The answer to a liveness probe: its arrival was the point.
            FrameKind::Pong { .. } => {}
            FrameKind::Attached {
                attach_id,
                snapshot,
                initial_client_id,
            } => {
                self.own_client_id = Some(initial_client_id);
                self.attached(attach_id, &snapshot)?;
            }
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
                self.fence_topology_read();
                self.attach_terminals.remove(&terminal_id);
                self.apply_engine(EngineEvent::Closed {
                    terminal_id: terminal_id.clone(),
                    exit_status,
                    signal,
                    reason,
                })?;
                if !self.close_pane(&terminal_id, exit_status, signal, reason)
                    && !self.retired_agents.contains(&terminal_id)
                {
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
            FrameKind::PathResults { request_id, result } => {
                if let Some(result) = self.resolve_path_results(request_id, result) {
                    self.push_event(Event::Frame(Box::new(FrameKind::PathResults {
                        request_id,
                        result,
                    })));
                }
                Ok(())
            }
            FrameKind::Event {
                terminal,
                event,
                stamp,
            } => {
                if super::organization::is_organization_event(&event)
                    || matches!(event, AgentEvent::JournalGap { .. })
                {
                    self.organization_event(stamp.as_ref().map(|stamp| stamp.seq), &event);
                }
                self.agent_event(terminal, event)
            }
            frame => self.feed_stream_frame(frame),
        }
    }

    pub(super) fn feed_stream_frame(&mut self, frame: FrameKind) -> Result<(), ControlError> {
        let Some(frame) = self.take_approval_frame(frame) else {
            return Ok(());
        };
        let Some(frame) = self.roster_metadata_frame(frame) else {
            return Ok(());
        };
        let Some(frame) = self.take_project_tag_frame(frame) else {
            return Ok(());
        };
        let Some(frame) = self.take_global_metadata_frame(frame) else {
            return Ok(());
        };
        match classify_engine_frame(frame) {
            ClassifiedFrame::Engine(event) => self.apply_engine(event),
            ClassifiedFrame::Other(frame) => {
                self.push_event(Event::Frame(Box::new(frame)));
                Ok(())
            }
        }
    }

    /// Subscribe to and read the one stored `phux.session.project/v1` tag.
    pub(super) fn watch_session_project(&mut self) {
        use phux_protocol::wire::frame::{SESSION_PROJECT_KEY, Scope};
        self.queue_frame(&FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: SESSION_PROJECT_KEY.to_owned(),
        });
        let request_id = self.next_request_id();
        self.project_tag_pending = Some(request_id);
        self.queue_frame(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Global,
            key: SESSION_PROJECT_KEY.to_owned(),
        });
    }

    /// Consume the project-tag GET. A live change is published and kept so
    /// other consumers still see the raw metadata frame.
    fn take_project_tag_frame(&mut self, frame: FrameKind) -> Option<FrameKind> {
        use phux_protocol::wire::frame::{SESSION_PROJECT_KEY, Scope};
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.project_tag_pending == Some(request_id) =>
            {
                self.project_tag_pending = None;
                self.publish_project_tag(value.as_deref());
                None
            }
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key,
                value,
                actor,
            } if key == SESSION_PROJECT_KEY => {
                self.publish_project_tag(value.as_deref());
                Some(FrameKind::MetadataChanged {
                    scope: Scope::Global,
                    key,
                    value,
                    actor,
                })
            }
            other => Some(other),
        }
    }

    fn publish_project_tag(&mut self, value: Option<&[u8]>) {
        let (name, project) =
            match value.and_then(phux_protocol::wire::frame::decode_session_project) {
                Some((name, project)) if !name.is_empty() && !project.is_empty() => {
                    (name.to_owned(), Some(project.to_owned()))
                }
                _ => (String::new(), None),
            };
        self.push_event(Event::SessionProject { name, project });
    }
}
