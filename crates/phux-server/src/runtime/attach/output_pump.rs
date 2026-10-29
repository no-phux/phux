//! Per-pane output pump context: forwarding, control, and native capture.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// Why an output pump stopped. The caller decides what to release: a
/// `SPAWN_RESOURCE` pump owns its pane, an ATTACH pump shares it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PumpFault {
    /// The client's outbound mailbox closed. Nothing is left to serve.
    OutboundClosed,
    /// A `BOOTSTRAP_TOMBSTONE` could not be queued ahead of the replacement
    /// generation, so the client can never learn the old one is void.
    TombstoneNotQueued,
    /// The replacement capture, its publication, or the in-band gap resync
    /// failed: the published generation is unrecoverable.
    GenerationLost,
    /// The actor refused to activate the replacement publication after its
    /// bootstrap frames were already queued.
    PublicationNotActivated,
    /// The outbound mailbox closed mid-replay. The pump abandons the task
    /// without touching shared state.
    ReplayAbandoned,
    /// The pane's actor was gone when the pump asked it for a replacement:
    /// `RESOURCE_CLOSED` is the pane's end; the connection is fine.
    PaneGone,
}

/// Whether a broadcast control frame belongs to a pump's current generation,
/// and whether forwarding it ends that generation.
#[derive(Debug, Clone, Copy)]
struct ControlDisposition {
    /// The frame names this pump's terminal, stream, and generation.
    targets_pump: bool,
    /// Forwarding it voids the generation (a bootstrap, not a history,
    /// tombstone).
    ends_generation: bool,
}

/// The actor's full-grid resync payload: a control event that replaces the
/// published generation rather than extending it.
#[derive(Debug)]
pub(super) struct PaneResync {
    /// Post-reflow grid width the client mirror adopts.
    pub(super) cols: u16,
    /// Post-reflow grid height the client mirror adopts.
    pub(super) rows: u16,
    /// Synthesized grid replay for the replacement bootstrap.
    pub(super) bytes: bytes::Bytes,
    /// Why the prior generation cannot continue.
    pub(super) reason: crate::terminal_actor::ResyncReason,
    /// Actor-global raw sequence included by the replacement cut.
    pub(super) base_seq: u64,
}

/// Map the actor's resync cause onto its wire tombstone reason.
pub(super) const fn tombstone_reason_for(
    reason: crate::terminal_actor::ResyncReason,
) -> phux_protocol::wire::frame::TombstoneReason {
    match reason {
        crate::terminal_actor::ResyncReason::Resize => {
            phux_protocol::wire::frame::TombstoneReason::Resize
        }
        crate::terminal_actor::ResyncReason::OutboundGap
        | crate::terminal_actor::ResyncReason::Exit => {
            phux_protocol::wire::frame::TombstoneReason::OutboundGap
        }
    }
}

/// Everything one output pump needs for the life of its subscription: the
/// client it serves, the pane it reads, and the negotiated bootstrap shape.
pub(crate) struct OutputPumpContext {
    /// This client's outbound mailbox.
    pub(crate) out_tx: tokio::sync::mpsc::Sender<Outbound>,
    /// Where a lagged pump asks the actor for an in-band resync addressed to
    /// it ([`Self::resync_target`]).
    pub(crate) resize: ResizeSender,
    /// Wire identity of the pane being pumped.
    pub(crate) wire_terminal_id: phux_protocol::ids::ResourceId,
    /// Stream this pump publishes on.
    pub(crate) stream_id: StreamId,
    /// Generation the first published bootstrap carries.
    pub(crate) initial_bootstrap_id: BootstrapId,
    /// Owner of the ordered control frames this pump must honour.
    pub(crate) client_id: ClientId,
    /// Negotiated capabilities every payload is adapted to.
    pub(crate) client_caps: ClientCapabilities,
    /// Negotiated bootstrap stream profile.
    pub(crate) profile: BootstrapStreamProfile,
    /// Negotiated bootstrap bounds.
    pub(crate) limits: BootstrapLimits,
    /// How this pump names itself in the broadcast-lag warning.
    pub(crate) lag_label: &'static str,
    /// Skip a slow consumer to a fresh checkpoint once a chunk is older than
    /// [`pump::STALE_OUTPUT_BUDGET`].
    pub(crate) stale_skip: bool,
    /// Ends the pump between events (a ready event is still handled first).
    pub(crate) cancel: Option<CancellationToken>,
    /// Request a fresh final checkpoint on natural close.
    pub(crate) drain: Option<CancellationToken>,
    /// Actor capture remains available until final publication completes.
    pub(crate) snapshot: Option<tokio::sync::mpsc::Sender<SnapshotRequest>>,
    /// Kept at the last forwarded sequence, so a replacement generation can
    /// tombstone this one at the right point.
    pub(crate) last_seq: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    /// Terminal facet used for native checkpoint capture and publication.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(crate) terminal: crate::terminal_actor::TerminalHandle,
}

impl OutputPumpContext {
    /// Publish the generation's last forwarded sequence, when tracked.
    pub(super) fn publish_last_seq(&self, generation: &PumpGeneration) {
        if let Some(last_seq) = &self.last_seq {
            last_seq.store(
                generation.last_forwarded_seq(),
                std::sync::atomic::Ordering::Release,
            );
        }
    }

    /// How a gap resync names this pump, and the generation it replaces, to
    /// the actor.
    pub(super) const fn resync_target(&self, generation: &PumpGeneration) -> ResyncTarget {
        ResyncTarget {
            owner: self.client_id.0,
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
        }
    }

    /// Replace the published generation for a resync addressed to this pump,
    /// and skip one addressed to other pumps.
    pub(super) async fn apply_resync(
        &self,
        generation: &mut PumpGeneration,
        output_rx: &mut tokio::sync::broadcast::Receiver<PaneOutput>,
        audience: &ResyncAudience,
        resync: &PaneResync,
    ) -> ControlFlow<Option<PumpFault>> {
        if !generation.takes_resync(audience, self.resync_target(generation)) {
            return ControlFlow::Continue(());
        }
        self.republish_generation(generation, output_rx, resync)
            .await
    }

    /// Frame one output chunk for this client, adapted to its capabilities.
    fn output_frame(
        &self,
        generation: &PumpGeneration,
        seq: u64,
        bytes: &bytes::Bytes,
    ) -> FrameKind {
        FrameKind::ResourceOutput {
            terminal_id: self.wire_terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: generation.bootstrap_id(),
            seq,
            bytes: live_bytes_for_profile(bytes, self.client_caps, self.profile),
        }
    }

    /// Forward the backlog a publication captured behind its cut, skipping
    /// anything the published bootstrap already covered.
    pub(super) async fn forward_gated_replay(
        &self,
        generation: &mut PumpGeneration,
        replay: Vec<(u64, bytes::Bytes)>,
    ) -> Result<(), PumpFault> {
        for (seq, bytes) in replay {
            if !generation.forwards(seq) {
                continue;
            }
            let frame = self.output_frame(generation, seq, &bytes);
            if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
                return Err(PumpFault::ReplayAbandoned);
            }
            generation.note_forwarded(seq);
        }
        Ok(())
    }

    /// Forward one live PTY chunk, dropping anything a tombstone voided or the
    /// published bootstrap already covered. A full mailbox is a gap: fence and
    /// skip to a fresh screen rather than park off the broadcast.
    pub(super) async fn forward_live(
        &self,
        generation: &mut PumpGeneration,
        seq: u64,
        bytes: &bytes::Bytes,
    ) -> ControlFlow<Option<PumpFault>> {
        if !generation.forwards(seq) {
            return ControlFlow::Continue(());
        }
        let frame = self.output_frame(generation, seq, bytes);
        match pump::try_send_frame(&self.out_tx, frame) {
            pump::MailboxForward::Sent => {
                crate::perf::PUMP_FRAMES.incr();
                crate::perf::PUMP_BYTES.add_len(bytes.len());
                crate::perf::PUMP_FRAME_BYTES.record_len(bytes.len());
                generation.note_forwarded(seq);
                self.publish_last_seq(generation);
                ControlFlow::Continue(())
            }
            pump::MailboxForward::Closed => ControlFlow::Break(Some(PumpFault::OutboundClosed)),
            pump::MailboxForward::Full => {
                self.request_gap_resync(generation, GapCause::Backpressure)
                    .await
            }
        }
    }

    /// Does this ordered control frame name this pump's terminal, stream, and
    /// generation — and does forwarding it end that generation?
    fn classify_control(&self, frame: &FrameKind, bootstrap_id: BootstrapId) -> ControlDisposition {
        let (terminal_id, control_stream_id, control_bootstrap_id, ends_generation) = match frame {
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => (terminal_id, *stream_id, *bootstrap_id, true),
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => (terminal_id, *stream_id, *bootstrap_id, false),
            _ => {
                return ControlDisposition {
                    targets_pump: false,
                    ends_generation: false,
                };
            }
        };
        ControlDisposition {
            targets_pump: terminal_id == &self.wire_terminal_id
                && control_stream_id == self.stream_id
                && control_bootstrap_id == bootstrap_id,
            ends_generation,
        }
    }

    /// Forward an ordered control frame addressed to this pump's generation.
    pub(super) async fn forward_control(
        &self,
        generation: &mut PumpGeneration,
        owner: u64,
        frame: FrameKind,
    ) -> ControlFlow<Option<PumpFault>> {
        if owner != self.client_id.0 {
            return ControlFlow::Continue(());
        }
        let disposition = self.classify_control(&frame, generation.bootstrap_id());
        if !disposition.targets_pump {
            return ControlFlow::Continue(());
        }
        if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return ControlFlow::Break(Some(PumpFault::OutboundClosed));
        }
        if disposition.ends_generation {
            generation.retire();
        }
        ControlFlow::Continue(())
    }

    /// Ask the actor for the replacement native checkpoint. Only a refused
    /// capture loses the generation; a closed mailbox or dropped reply means
    /// the actor is already gone, which ends this pump without failing the
    /// connection. `None` means the actor invalidated the cut mid-capture (a
    /// reflow, a replacement child, or teardown): a resync or the pane's
    /// close follows, so the generation is not lost.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) async fn capture_native_checkpoint(
        &self,
        bootstrap_id: BootstrapId,
    ) -> Result<Option<crate::terminal_actor::NativeBootstrapReply>, PumpFault> {
        let captured = request_native_checkpoint(&self.terminal, |reply| {
            crate::terminal_actor::NativeBootstrapRequest::full_budget(
                self.client_id.0,
                self.wire_terminal_id.clone(),
                self.stream_id,
                bootstrap_id,
                self.limits,
                reply,
            )
        })
        .await;
        let reply = match captured {
            Ok(reply) => reply,
            Err(NativeRequestFailure::Unsent | NativeRequestFailure::Dropped) => {
                return Err(PumpFault::PaneGone);
            }
            Err(NativeRequestFailure::Refused(crate::native_state::NativeStateError::Resize)) => {
                debug!(
                    terminal_id = ?self.wire_terminal_id,
                    "native checkpoint invalidated mid-capture; awaiting the resync"
                );
                return Ok(None);
            }
            Err(NativeRequestFailure::Refused(error)) => {
                warn!(
                    terminal_id = ?self.wire_terminal_id,
                    %error,
                    "native checkpoint resync refused"
                );
                let _ = self
                    .out_tx
                    .send(Outbound::Frame(FrameKind::Error {
                        request_id: None,
                        code: ErrorCode::CodecUnavailable,
                        message: "native checkpoint resync failed".to_owned(),
                    }))
                    .await;
                return Err(PumpFault::GenerationLost);
            }
        };
        Ok(Some(reply))
    }
}
