//! Retention budgets, delivery failures, and generation retirement.

use super::{
    AgentEvent, BootstrapId, DeliveryFailure, ErrorCode, FrameKind, HistoryTombstoneReason,
    MAX_RELAY_CONNECTION_RETAINED_BYTES, MAX_RELAY_CONNECTION_RETAINED_FRAMES,
    MAX_RELAY_SUBSCRIBER_RETAINED_BYTES, MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES, Outbound,
    ProxySubscriber, RELAY_RETIREMENT_TIMEOUT, RETAINED_FRAME_OVERHEAD, RelaySession, ResourceId,
    SatelliteHost, SnapshotGate, StreamId, TombstoneReason, VecDeque, mpsc, trace,
};

impl RelaySession {
    pub(super) const fn retention_exceeded(
        frame_bytes: usize,
        retained_bytes: usize,
        retained_frames: usize,
        connection_bytes: usize,
        connection_frames: usize,
    ) -> bool {
        retained_bytes.saturating_add(frame_bytes) > MAX_RELAY_SUBSCRIBER_RETAINED_BYTES
            || retained_frames.saturating_add(1) > MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES
            || connection_bytes.saturating_add(frame_bytes) > MAX_RELAY_CONNECTION_RETAINED_BYTES
            || connection_frames.saturating_add(1) > MAX_RELAY_CONNECTION_RETAINED_FRAMES
    }

    pub(super) fn retained_frame_bytes(frame: &FrameKind) -> usize {
        let dynamic = match frame {
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } => Self::resource_id_bytes(terminal_id).saturating_add(bytes.len()),
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } => Self::resource_id_bytes(terminal_id).saturating_add(payload.len()),
            FrameKind::BootstrapReady {
                terminal_id,
                history_cursor,
                ..
            } => Self::resource_id_bytes(terminal_id)
                .saturating_add(history_cursor.as_ref().map_or(0, bytes::Bytes::len)),
            FrameKind::HistoryPage {
                terminal_id,
                cursor,
                next_cursor,
                payload,
                ..
            } => Self::resource_id_bytes(terminal_id)
                .saturating_add(cursor.len())
                .saturating_add(next_cursor.as_ref().map_or(0, bytes::Bytes::len))
                .saturating_add(payload.len()),
            FrameKind::HistoryTombstone {
                terminal_id,
                cursor,
                ..
            }
            | FrameKind::HistoryRejected {
                terminal_id,
                cursor,
                ..
            } => Self::resource_id_bytes(terminal_id).saturating_add(cursor.len()),
            FrameKind::Event {
                terminal, event, ..
            } => terminal
                .as_ref()
                .map_or(0, Self::resource_id_bytes)
                .saturating_add(Self::agent_event_bytes(event)),
            FrameKind::BootstrapBegin { terminal_id, .. }
            | FrameKind::BootstrapTombstone { terminal_id, .. }
            | FrameKind::ResourceClosed { terminal_id, .. }
            | FrameKind::Bell { terminal_id } => Self::resource_id_bytes(terminal_id),
            _ => 0,
        };
        dynamic.saturating_add(RETAINED_FRAME_OVERHEAD)
    }

    pub(super) fn resource_id_bytes(id: &ResourceId) -> usize {
        match id {
            ResourceId::Local { .. } => 0,
            ResourceId::Satellite { host, .. } => host.as_str().len(),
        }
    }

    pub(super) fn agent_event_bytes(event: &AgentEvent) -> usize {
        match event {
            AgentEvent::TitleChanged { title } => title.capacity(),
            AgentEvent::ResourceSpawned { parent, .. } => {
                parent.as_ref().map_or(0, Self::resource_id_bytes)
            }
            AgentEvent::Asked {
                id,
                question,
                suggestions,
                ..
            } => id
                .capacity()
                .saturating_add(question.capacity())
                .saturating_add(
                    suggestions
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                )
                .saturating_add(
                    suggestions
                        .iter()
                        .map(String::capacity)
                        .fold(0usize, usize::saturating_add),
                ),
            AgentEvent::CwdChanged { cwd } => cwd.capacity(),
            AgentEvent::Unknown { body, .. } => body.capacity(),
            _ => 0,
        }
    }

    pub(super) fn delivery_failure(
        sub: &ProxySubscriber,
        host: &SatelliteHost,
        terminal: u32,
        frame: &FrameKind,
    ) -> DeliveryFailure {
        if let FrameKind::HistoryPage {
            stream_id,
            bootstrap_id,
            cursor,
            ..
        } = frame
        {
            return DeliveryFailure {
                frame: FrameKind::HistoryTombstone {
                    terminal_id: ResourceId::satellite(host.clone(), terminal),
                    stream_id: *stream_id,
                    bootstrap_id: *bootstrap_id,
                    cursor: cursor.clone(),
                    reason: HistoryTombstoneReason::Limit,
                },
                detach_upstream: true,
            };
        }
        let identity = match frame {
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                seq,
                ..
            } => Some((*stream_id, *bootstrap_id, seq.saturating_sub(1))),
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } => Some((*stream_id, *bootstrap_id, *base_seq)),
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            } => Some((
                *stream_id,
                *bootstrap_id,
                Self::last_valid_seq(sub, *stream_id, *bootstrap_id),
            )),
            _ => sub.current_generation,
        };
        let failure = identity.map_or_else(
            || FrameKind::Error {
                request_id: None,
                code: ErrorCode::ResourceExhausted,
                message: format!(
                    "satellite {host} terminal {terminal} relay delivery exceeded bounded downstream capacity; subscription closed"
                ),
            },
            |(stream_id, bootstrap_id, last_valid_seq)| FrameKind::BootstrapTombstone {
                terminal_id: ResourceId::satellite(host.clone(), terminal),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::OutboundGap,
                last_valid_seq,
            },
        );
        DeliveryFailure {
            frame: failure,
            detach_upstream: !matches!(frame, FrameKind::ResourceClosed { .. }),
        }
    }

    pub(super) fn begin_retirement(sub: &mut ProxySubscriber, failure: DeliveryFailure) {
        sub.gate = SnapshotGate::Retiring {
            failure: failure.frame,
            deadline: std::time::Instant::now() + RELAY_RETIREMENT_TIMEOUT,
            detach_upstream: failure.detach_upstream,
        };
    }

    pub(super) fn observe_generation(sub: &mut ProxySubscriber, frame: &FrameKind) {
        let generation = match frame {
            FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                base_seq,
                ..
            } => Some((*stream_id, *bootstrap_id, *base_seq)),
            FrameKind::ResourceOutput {
                stream_id,
                bootstrap_id,
                seq,
                ..
            }
            | FrameKind::BootstrapTombstone {
                stream_id,
                bootstrap_id,
                last_valid_seq: seq,
                ..
            } => Some((*stream_id, *bootstrap_id, *seq)),
            FrameKind::BootstrapChunk {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::BootstrapReady {
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryPage {
                stream_id,
                bootstrap_id,
                ..
            } => Some((
                *stream_id,
                *bootstrap_id,
                Self::last_valid_seq(sub, *stream_id, *bootstrap_id),
            )),
            _ => None,
        };
        if let Some(generation) = generation {
            sub.current_generation = Some(generation);
        }
    }

    pub(super) fn last_valid_seq(
        sub: &ProxySubscriber,
        stream: StreamId,
        bootstrap: BootstrapId,
    ) -> u64 {
        sub.current_generation.map_or(0, |current| {
            if current.0 == stream && current.1 == bootstrap {
                current.2
            } else {
                0
            }
        })
    }

    pub(super) const fn detach_upstream_on_removal(sub: &ProxySubscriber) -> bool {
        match &sub.gate {
            SnapshotGate::Retiring {
                detach_upstream, ..
            } => *detach_upstream,
            _ => !sub.retire_after_flush,
        }
    }

    pub(super) fn retain_ordered_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        let (mut frames, retained_bytes, open_after) = match gate {
            SnapshotGate::Retained {
                frames,
                retained_bytes,
                open_after,
            } => (frames, retained_bytes, open_after),
            SnapshotGate::Open => (VecDeque::new(), 0, true),
            SnapshotGate::AwaitingFirst => (VecDeque::new(), 0, false),
            SnapshotGate::Retiring {
                failure,
                deadline,
                detach_upstream,
            } => {
                if matches!(&frame, FrameKind::ResourceClosed { .. }) {
                    // The upstream close supersedes a queued delivery-gap
                    // fence (and its redundant detach).
                    sub.gate = SnapshotGate::Retiring {
                        failure: frame,
                        deadline,
                        detach_upstream: false,
                    };
                } else {
                    sub.gate = SnapshotGate::Retiring {
                        failure,
                        deadline,
                        detach_upstream,
                    };
                }
                return;
            }
        };
        let next_bytes = retained_bytes.saturating_add(frame_bytes);
        let next_frames = frames.len().saturating_add(1);
        if next_bytes > MAX_RELAY_SUBSCRIBER_RETAINED_BYTES
            || next_frames > MAX_RELAY_SUBSCRIBER_RETAINED_FRAMES
            || connection_bytes.saturating_add(frame_bytes) > MAX_RELAY_CONNECTION_RETAINED_BYTES
            || connection_frames.saturating_add(1) > MAX_RELAY_CONNECTION_RETAINED_FRAMES
        {
            *connection_bytes = connection_bytes.saturating_sub(retained_bytes);
            *connection_frames = connection_frames.saturating_sub(frames.len());
            Self::begin_retirement(sub, delivery_failure);
            return;
        }
        frames.push_back(frame);
        *connection_bytes = connection_bytes.saturating_add(frame_bytes);
        *connection_frames = connection_frames.saturating_add(1);
        sub.gate = SnapshotGate::Retained {
            frames,
            retained_bytes: next_bytes,
            open_after,
        };
    }

    /// Best-effort delivery past the snapshot gate for frames the snapshot
    /// does not capture (`RESOURCE_CLOSED`, `BELL`). A refusal reaps the
    /// subscriber.
    pub(super) fn fan_out_ungated(&mut self, id: u32, frame: &FrameKind) {
        let host = &self.host;
        let Some(subs) = self.subscribers.get_mut(&id) else {
            trace!(satellite = %self.host, terminal = id, "ungated frame with no proxy subscribers");
            return;
        };
        let detach_if_empty = subs.iter().any(Self::detach_upstream_on_removal);
        let mut retained_bytes = self.retained_bytes;
        let mut retained_frames = self.retained_frames;
        subs.retain_mut(|sub| {
            Self::observe_generation(sub, frame);
            if matches!(
                sub.gate,
                SnapshotGate::Retained { .. } | SnapshotGate::Retiring { .. }
            ) {
                let failure = Self::delivery_failure(sub, host, id, frame);
                Self::retain_ordered_frame(
                    sub,
                    frame.clone(),
                    failure,
                    &mut retained_bytes,
                    &mut retained_frames,
                );
                return true;
            }
            match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
                Ok(()) => !sub.retire_after_flush,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let failure = Self::delivery_failure(sub, host, id, frame);
                    Self::retain_ordered_frame(
                        sub,
                        frame.clone(),
                        failure,
                        &mut retained_bytes,
                        &mut retained_frames,
                    );
                    true
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
        self.retained_bytes = retained_bytes;
        self.retained_frames = retained_frames;
        if subs.is_empty() {
            self.subscribers.remove(&id);
            if detach_if_empty {
                self.orphaned_by_delivery.insert(id);
            }
        }
        self.recalculate_retained_totals();
    }
}
