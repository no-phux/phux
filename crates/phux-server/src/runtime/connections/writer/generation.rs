use crate::state::Outbound;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::FrameKind;
use std::collections::HashMap;

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
struct OutboundGeneration {
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    retired: bool,
}

impl OutboundGeneration {
    fn same_identity(
        self,
        stream_id: phux_protocol::ids::StreamId,
        bootstrap_id: phux_protocol::ids::BootstrapId,
    ) -> bool {
        self.stream_id == stream_id && self.bootstrap_id == bootstrap_id
    }
}

#[derive(Clone, Copy, Debug)]
enum OutboundTerminalState {
    Generation(OutboundGeneration),
    Closed,
}

#[derive(Debug, Default)]
pub(super) struct OutboundGenerationFence {
    terminals: HashMap<WireResourceId, OutboundTerminalState>,
    diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl OutboundGenerationFence {
    pub(super) fn new(diagnostics: Option<crate::stream_diagnostics::StreamTracker>) -> Self {
        Self {
            diagnostics,
            ..Self::default()
        }
    }

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

    fn admit_begin(
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

    fn admit_tombstone(
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

    fn admit_data(
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

    fn current(&self, terminal_id: &WireResourceId) -> Option<OutboundGeneration> {
        match self.terminals.get(terminal_id) {
            Some(OutboundTerminalState::Generation(generation)) => Some(*generation),
            Some(OutboundTerminalState::Closed) | None => None,
        }
    }

    fn is_closed(&self, terminal_id: &WireResourceId) -> bool {
        matches!(
            self.terminals.get(terminal_id),
            Some(OutboundTerminalState::Closed)
        )
    }
}

/// The generation identity carried by a server-to-client data frame. BEGIN
/// and the bootstrap tombstone are transitions [`OutboundGenerationFence::admits`]
/// handles itself.
const fn generation_of_outbound_frame(
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

#[cfg(test)]
mod outbound_generation_fence_tests {
    use bytes::Bytes;
    use phux_protocol::caps::BootstrapStreamProfile;
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    use phux_protocol::wire::frame::{FrameKind, HistoryTombstoneReason, TombstoneReason};

    use super::{Outbound, OutboundGenerationFence};

    fn terminal() -> ResourceId {
        ResourceId::local(1)
    }

    fn stream() -> StreamId {
        StreamId::new(2).expect("stream id")
    }

    fn bootstrap(raw: u64) -> BootstrapId {
        BootstrapId::new(raw).expect("bootstrap id")
    }

    fn begin(bootstrap_id: BootstrapId) -> Outbound {
        Outbound::Frame(FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        })
    }

    fn tombstone(bootstrap_id: BootstrapId, reason: TombstoneReason) -> Outbound {
        Outbound::Frame(FrameKind::BootstrapTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id,
            reason,
            last_valid_seq: 1,
        })
    }

    fn output(bootstrap_id: BootstrapId) -> Outbound {
        Outbound::Frame(FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id,
            seq: 1,
            bytes: Bytes::from_static(b"output"),
        })
    }

    #[test]
    fn tombstone_fences_late_history_and_output_before_replacement() {
        use crate::stream_diagnostics::{
            ResyncReason, StreamContext, StreamDiagnostics, StreamLane,
        };
        let diagnostics = StreamDiagnostics::new();
        let registration = diagnostics.register(StreamContext {
            connection_id: 1,
            stream_id: 4,
            lane: StreamLane::Terminal,
        });
        let initial = bootstrap(1);
        let replacement = bootstrap(2);
        let mut fence = OutboundGenerationFence {
            diagnostics: Some(registration.tracker()),
            ..OutboundGenerationFence::default()
        };
        assert!(fence.admits(&begin(initial)));
        assert!(fence.admits(&output(initial)));
        assert!(fence.admits(&tombstone(initial, TombstoneReason::Resize)));

        assert!(!fence.admits(&Outbound::Frame(FrameKind::HistoryPage {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: initial,
            page_seq: 1,
            cursor: Bytes::from_static(b"old"),
            next_cursor: None,
            payload: Bytes::from_static(b"late history"),
            rows: 1,
        })));
        assert!(!fence.admits(&Outbound::Frame(FrameKind::HistoryTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: initial,
            cursor: Bytes::from_static(b"old"),
            reason: HistoryTombstoneReason::Stale,
        },)));
        assert!(!fence.admits(&output(initial)));
        assert!(!fence.admits(&begin(initial)));

        assert!(fence.admits(&tombstone(initial, TombstoneReason::OutboundGap)));
        assert_eq!(
            diagnostics.snapshot().streams[0].resync_reason,
            Some(ResyncReason::Resize)
        );

        assert!(fence.admits(&begin(replacement)));
        assert!(fence.admits(&output(replacement)));
        assert!(!fence.admits(&output(initial)));
        assert!(!fence.admits(&tombstone(initial, TombstoneReason::OutboundGap)));
        assert_eq!(
            diagnostics.snapshot().streams[0].resync_reason,
            Some(ResyncReason::Resize)
        );
    }
}
