//! Outbound generation fencing and write batching.

use super::{BytesMut, Compression, FrameKind, HashMap, Outbound, WireResourceId};

/// Upper bound on queued outbound messages coalesced into one transport
/// write, so a saturated mailbox cannot starve the close-control arm.
pub(super) const MAX_WRITE_COALESCE: usize = 32;

/// The one generation a client-side terminal may currently receive.
///
/// Producers enqueue from different tasks, and a reserved permit guarantees
/// capacity, not order, so a history reply can land behind the tombstone that
/// retired it. The writer is the last common ordering point and drops any
/// generation-bound frame that no longer names the live generation.
#[derive(Clone, Copy, Debug)]
pub(super) struct OutboundGeneration {
    pub(super) stream_id: phux_protocol::ids::StreamId,
    pub(super) bootstrap_id: phux_protocol::ids::BootstrapId,
    pub(super) retired: bool,
}

impl OutboundGeneration {
    pub(super) fn same_identity(
        self,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        self.stream_id == stream_id && self.bootstrap_id == bootstrap_id
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum OutboundTerminalState {
    Generation(OutboundGeneration),
    Closed,
}

#[derive(Debug, Default)]
pub(super) struct OutboundGenerationFence {
    pub(super) terminals: HashMap<WireResourceId, OutboundTerminalState>,
    pub(super) diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl OutboundGenerationFence {
    /// Admit one mailbox item into the wire stream, applying generation
    /// transitions before the next queued item is considered.
    pub(super) fn admits(&mut self, message: &Outbound) -> bool {
        let Outbound::Frame(frame) = message else {
            return true;
        };
        match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => self.admit_begin(terminal_id, *stream_id, *bootstrap_id),
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                ..
            } => {
                let already_retired = self
                    .current(terminal_id)
                    .is_some_and(|current| current.retired);
                let admitted = self.admit_tombstone(terminal_id, *stream_id, *bootstrap_id);
                if admitted
                    && !already_retired
                    && let Some(tracker) = &self.diagnostics
                {
                    tracker.record_tombstone(*reason);
                }
                admitted
            }
            FrameKind::ResourceClosed { terminal_id, .. } => {
                self.terminals
                    .insert(terminal_id.clone(), OutboundTerminalState::Closed);
                true
            }
            _ => generation_of_outbound_frame(frame).is_none_or(
                |(terminal_id, stream_id, bootstrap_id)| {
                    self.admit_data(terminal_id, stream_id, bootstrap_id)
                },
            ),
        }
    }

    pub(super) fn admit_begin(
        &mut self,
        terminal_id: &WireResourceId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        if self.is_closed(terminal_id)
            || self.current(terminal_id).is_some_and(|current| {
                current.retired && current.same_identity(stream_id, bootstrap_id)
            })
        {
            return false;
        }
        self.terminals.insert(
            terminal_id.clone(),
            OutboundTerminalState::Generation(OutboundGeneration {
                stream_id,
                bootstrap_id,
                retired: false,
            }),
        );
        true
    }

    pub(super) fn admit_tombstone(
        &mut self,
        terminal_id: &WireResourceId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        if self.is_closed(terminal_id)
            || self
                .current(terminal_id)
                .is_some_and(|current| !current.same_identity(stream_id, bootstrap_id))
        {
            return false;
        }
        self.terminals.insert(
            terminal_id.clone(),
            OutboundTerminalState::Generation(OutboundGeneration {
                stream_id,
                bootstrap_id,
                retired: true,
            }),
        );
        true
    }

    pub(super) fn admit_data(
        &self,
        terminal_id: &WireResourceId,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        !self.is_closed(terminal_id)
            && self.current(terminal_id).is_none_or(|current| {
                !current.retired && current.same_identity(stream_id, bootstrap_id)
            })
    }

    pub(super) fn current(&self, terminal_id: &WireResourceId) -> Option<OutboundGeneration> {
        match self.terminals.get(terminal_id) {
            Some(OutboundTerminalState::Generation(generation)) => Some(*generation),
            Some(OutboundTerminalState::Closed) | None => None,
        }
    }

    pub(super) fn is_closed(&self, terminal_id: &WireResourceId) -> bool {
        matches!(
            self.terminals.get(terminal_id),
            Some(OutboundTerminalState::Closed)
        )
    }
}

/// The generation identity carried by a server-to-client data frame. BEGIN
/// and the bootstrap tombstone are transitions [`OutboundGenerationFence::admits`]
/// handles itself.
pub(super) const fn generation_of_outbound_frame(
    frame: &FrameKind,
) -> Option<(
    &WireResourceId,
    phux_protocol::ids::StreamId,
    phux_protocol::ids::BootstrapId,
)> {
    match frame {
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        }
        | FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            ..
        } => Some((terminal_id, *stream_id, *bootstrap_id)),
        _ => None,
    }
}

/// Encode one outbound message onto the end of `batch`, recording its frame
/// boundary in `ends`. Returns the message of an [`Outbound::TerminalError`],
/// after which the caller must stop draining.
pub(super) fn encode_into_batch(
    message: Outbound,
    compression: Compression,
    scratch: &mut BytesMut,
    batch: &mut BytesMut,
    ends: &mut Vec<usize>,
) -> Option<String> {
    let (frame, terminal_message) = match message {
        Outbound::Frame(frame) => (frame, None),
        Outbound::TerminalError {
            request_id,
            code,
            message,
        } => (
            FrameKind::Error {
                request_id,
                code,
                message: message.clone(),
            },
            Some(message),
        ),
    };
    let encode_started = std::time::Instant::now();
    frame.encode_compressed(compress_policy(&frame, compression), scratch, batch);
    crate::perf::WIRE_ENCODE.record_elapsed(encode_started);
    ends.push(batch.len());
    terminal_message
}

/// Which frames are worth wrapping (proto.md §6.4): bootstrap and history
/// payloads only. Those are large and gate first paint; live
/// `RESOURCE_OUTPUT` measured slower when compressed.
pub(super) const fn compress_policy(frame: &FrameKind, negotiated: Compression) -> Compression {
    match frame {
        FrameKind::BootstrapChunk { .. } | FrameKind::HistoryPage { .. } => negotiated,
        _ => Compression::None,
    }
}

/// Absorb up to [`MAX_WRITE_COALESCE`] further messages that are *already*
/// queued into the same batch. Stops on an empty or closed mailbox, and on a
/// terminal-error sentinel (returned so the caller can finish the shutdown
/// handshake).
pub(super) fn drain_ready_into_batch(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    generation_fence: &mut OutboundGenerationFence,
    compression: Compression,
    scratch: &mut BytesMut,
    batch: &mut BytesMut,
    ends: &mut Vec<usize>,
) -> Option<String> {
    for _ in 1..MAX_WRITE_COALESCE {
        let Ok(next) = rx.try_recv() else {
            return None;
        };
        if !generation_fence.admits(&next) {
            continue;
        }
        if let Some(message) = encode_into_batch(next, compression, scratch, batch, ends) {
            return Some(message);
        }
    }
    None
}
