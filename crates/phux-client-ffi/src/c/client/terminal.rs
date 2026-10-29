#[cfg(test)]
use super::{Arc, GridFrame, ResourceKind};
use super::{
    BootstrapId, BridgeError, Client, EngineDocumentPoint, FrameKind, Lane, MouseMode,
    PhuxDocumentAnchor, PhuxDocumentPoint, PhuxSearchResult, ReplicaKey, ResourceId, Scroll,
    SelectionGestureEvent, SelectionGestureResult, StreamId, engine_bridge,
};

impl Client {
    pub(crate) fn active_attach_contains(&self, id: &ResourceId) -> bool {
        self.control().active_attach_contains(id)
    }

    pub(crate) fn input_ready(&self, id: &ResourceId) -> bool {
        self.control()
            .engine()
            .is_some_and(|engine| engine.input_ready(id))
    }

    pub(crate) fn terminal_closed(&self, id: &ResourceId) -> bool {
        self.control()
            .engine()
            .is_some_and(|engine| engine.is_closed(id))
    }

    #[cfg(test)]
    pub(crate) fn projection(&self, id: &ResourceId) -> Option<Arc<GridFrame>> {
        self.control().publication().acquire(id)
    }

    pub(crate) fn input_eligibility(
        &self,
        id: &ResourceId,
    ) -> phux_client_core::session::InputEligibility {
        self.control().engine().map_or(
            phux_client_core::session::InputEligibility::Ineligible(
                phux_client_core::session::InputBlockReason::UnknownTerminal,
            ),
            |engine| engine.input_eligibility(id),
        )
    }

    #[cfg(test)]
    pub(crate) fn resource_kind(&self, id: &ResourceId) -> Option<ResourceKind> {
        if self.is_agent_stream(id) {
            return Some(ResourceKind::AgentSession);
        }
        let info = self.control().engine()?.replica_info(id).ok()?;
        Some(match info.profile {
            phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1
            | phux_protocol::BootstrapStreamProfile::AgentEventsJsonlResumeV1 { .. } => {
                ResourceKind::AgentSession
            }
            _ => ResourceKind::Terminal,
        })
    }

    pub(crate) fn has_projection(&self, id: &ResourceId) -> bool {
        self.control()
            .engine()
            .is_some_and(|engine| engine.has_projection(id))
    }

    pub(crate) fn release_terminal(&self, id: &ResourceId) -> bool {
        self.control().release_terminal(id)
    }

    pub(crate) fn terminal_key(&self, id: &ResourceId) -> Result<ReplicaKey, BridgeError> {
        let info = self.engine()?.replica_info(id).map_err(engine_bridge)?;
        let profile = info.profile;
        Ok(ReplicaKey {
            terminal_id: id.clone(),
            stream_id: StreamId::new(info.stream_id)
                .ok_or_else(|| BridgeError::state("runtime published a zero stream id"))?,
            bootstrap_id: BootstrapId::new(info.bootstrap_id)
                .ok_or_else(|| BridgeError::state("runtime published a zero bootstrap id"))?,
            profile,
        })
    }

    pub(crate) fn mouse_tracking(&self, id: &ResourceId) -> Result<bool, BridgeError> {
        Ok(self.engine()?.mouse_mode(id).map_err(engine_bridge)? != MouseMode::None)
    }

    pub(crate) fn bracketed_paste(&self, id: &ResourceId) -> Result<bool, BridgeError> {
        self.engine()?.bracketed_paste(id).map_err(engine_bridge)
    }

    pub(crate) fn mouse_mode(&self, id: &ResourceId) -> Result<MouseMode, BridgeError> {
        self.engine()?.mouse_mode(id).map_err(engine_bridge)
    }

    pub(crate) fn track_anchor(
        &self,
        id: &ResourceId,
        point: PhuxDocumentPoint,
    ) -> Result<PhuxDocumentAnchor, BridgeError> {
        self.ensure_attached()?;
        if point.reserved != 0 {
            return Err(BridgeError::invalid(
                "document point reserved field must be zero",
            ));
        }
        let handle = self
            .engine()?
            .track_anchor(
                id,
                EngineDocumentPoint {
                    space: point.space,
                    column: point.column,
                    row: point.row,
                },
            )
            .map_err(engine_bridge)?;
        Ok(PhuxDocumentAnchor { opaque_id: handle })
    }

    pub(crate) fn release_anchor(
        &self,
        id: &ResourceId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .release_anchor(id, anchor.opaque_id)
            .map_err(engine_bridge)
    }

    pub(crate) fn clear_presentation(
        &self,
        id: &ResourceId,
        stream_id: u64,
        bootstrap_id: u64,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .clear_presentation(id, stream_id, bootstrap_id)
            .map_err(engine_bridge)
    }

    pub(crate) fn pin_viewport(
        &mut self,
        id: &ResourceId,
        anchor: PhuxDocumentAnchor,
    ) -> Result<(), BridgeError> {
        self.control()
            .pin_viewport(id, anchor.opaque_id)
            .map_err(engine_bridge)?;
        self.process_runtime_events()?;
        Ok(())
    }

    pub(crate) fn follow_live(&mut self, id: &ResourceId) -> Result<(), BridgeError> {
        self.control().follow_live(id).map_err(engine_bridge)?;
        self.process_runtime_events()?;
        Ok(())
    }

    pub(crate) fn set_selection(
        &self,
        id: &ResourceId,
        start: PhuxDocumentAnchor,
        end: PhuxDocumentAnchor,
        rectangle: bool,
    ) -> Result<(), BridgeError> {
        self.engine()?
            .set_selection(id, start.opaque_id, end.opaque_id, rectangle)
            .map_err(engine_bridge)
    }

    pub(crate) fn clear_selection(&self, id: &ResourceId) -> Result<(), BridgeError> {
        self.engine()?.clear_selection(id).map_err(engine_bridge)
    }

    pub(crate) fn selection_text(&mut self, id: &ResourceId) -> Result<(), BridgeError> {
        self.selection_buf = self.engine()?.selection_text(id).map_err(engine_bridge)?;
        Ok(())
    }

    pub(crate) fn search(
        &mut self,
        id: &ResourceId,
        query: &[u8],
        case_sensitive: bool,
    ) -> Result<(), BridgeError> {
        let query = std::str::from_utf8(query)
            .map_err(|_| BridgeError::invalid("search query is not UTF-8"))?;
        self.search_results = self
            .engine()?
            .search(id, query.to_owned(), case_sensitive)
            .map_err(engine_bridge)?
            .into_iter()
            .map(|result| PhuxSearchResult {
                start: PhuxDocumentAnchor {
                    opaque_id: result.start,
                },
                end: PhuxDocumentAnchor {
                    opaque_id: result.end,
                },
            })
            .collect();
        Ok(())
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "keeps the mutation adapter uniform and explicitly idempotent"
    )]
    pub(crate) fn release_search_results(&mut self) -> Result<(), BridgeError> {
        let results = std::mem::take(&mut self.search_results);
        let ids: Vec<ResourceId> = self.control().publication().terminals();
        let mut first = None;
        for result in results {
            for anchor in [result.start, result.end] {
                let released = ids.iter().any(|id| {
                    self.engine()
                        .and_then(|engine| {
                            engine
                                .release_anchor(id, anchor.opaque_id)
                                .map_err(engine_bridge)
                        })
                        .is_ok()
                });
                if !released && first.is_none() {
                    first = Some(BridgeError::state("document anchor is stale or unknown"));
                }
            }
        }
        // Result handles are transient C borrows. A result may already have
        // been invalidated by another viewport mutation; releasing the set is
        // intentionally idempotent, as the pre-runtime shim was.
        let _ = first;
        Ok(())
    }

    pub(crate) fn scroll(
        &mut self,
        id: &ResourceId,
        kind: u32,
        value: i64,
    ) -> Result<(), BridgeError> {
        let scroll = match kind {
            0 => Scroll::Top,
            1 => Scroll::Bottom,
            2 => Scroll::Delta(value),
            3 => Scroll::Row(
                u64::try_from(value)
                    .map_err(|_| BridgeError::invalid("scroll row must be non-negative"))?,
            ),
            4 => Scroll::Prompt(value),
            _ => return Err(BridgeError::invalid("unknown viewport scroll kind")),
        };
        self.control().scroll(id, scroll).map_err(engine_bridge)?;
        self.process_runtime_events()?;
        Ok(())
    }

    pub(crate) fn selection_gesture(
        &self,
        id: &ResourceId,
        event: SelectionGestureEvent,
    ) -> Result<SelectionGestureResult, BridgeError> {
        self.engine()?
            .selection_gesture(id, event)
            .map_err(engine_bridge)
    }

    pub(crate) fn process_send(
        &mut self,
        send: phux_client_core::session::KernelSend,
    ) -> Result<(), BridgeError> {
        use phux_client_core::session::KernelSend;
        let send = match send {
            KernelSend::SubscribeEvents {
                terminal,
                after_seq,
            } => KernelSend::SubscribeEvents {
                terminal,
                after_seq: after_seq
                    .or(self.event_after_seq)
                    .filter(|_| self.event_journal),
            },
            send => send,
        };
        if self.operations.fence_send(&send) {
            return Ok(());
        }
        let frame = crate::c::operations::kernel_send_frame(send)?;
        self.queue_frame(&frame)
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves the fallible binding queue seam used by all extension modules"
    )]
    pub(crate) fn queue_frame(&mut self, frame: &FrameKind) -> Result<(), BridgeError> {
        self.control().queue_frame(frame);
        self.drain_outbound();
        Ok(())
    }

    /// Drain the frames a connected driver read since the last poll.
    pub(crate) fn take_inbound(&self) -> (u64, Vec<bytes::Bytes>) {
        self.runtime.take_inbound()
    }

    /// Whether the runtime's driver owns this client's socket.
    pub(crate) fn is_connected_lane(&self) -> bool {
        self.runtime.lane() == Lane::Connected
    }

    pub(crate) fn drain_outbound(&mut self) {
        // The connected lane's driver writes queued frames itself, and the
        // control guard has already woken it. Draining here would steal
        // them off the socket.
        if self.is_connected_lane() {
            return;
        }
        let outbound = self.control().take_outbound();
        self.outgoing.extend(outbound);
    }
}
