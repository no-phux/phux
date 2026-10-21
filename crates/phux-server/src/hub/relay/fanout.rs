//! Fanning stream frames out to proxy subscribers, live and during bootstrap.

use super::{
    DeliveryFailure, FanOutFrame, FanOutKind, FrameKind, Outbound, ProxySubscriber, RelaySession,
    SnapshotGate, VecDeque, mpsc, trace, warn,
};

impl RelaySession {
    /// Push `frame` to every proxy subscriber of terminal `id` via
    /// `try_send`.
    ///
    /// A `TERMINAL_SNAPSHOT` anchors ordering (L1 §9.1): a new attacher waits
    /// in [`SnapshotGate::AwaitingFirst`] and a refused snapshot is retained
    /// ([`SnapshotGate::Retained`]) and retried before any delta; a newer
    /// snapshot replaces it. `RESOURCE_CLOSED` and `BELL` bypass the gate
    /// ([`Self::fan_out_ungated`]).
    pub(super) fn fan_out(&mut self, id: u32, frame: &FrameKind) {
        let host = &self.host;
        let Some(subs) = self.subscribers.get_mut(&id) else {
            trace!(satellite = %host, terminal = id, "inbound stream frame with no proxy subscribers");
            return;
        };
        let detach_if_empty = subs.iter().any(Self::detach_upstream_on_removal);
        let is_begin = matches!(frame, FrameKind::BootstrapBegin { .. });
        let kind = if is_begin
            || matches!(
                frame,
                FrameKind::BootstrapChunk { .. }
                    | FrameKind::BootstrapReady { .. }
                    | FrameKind::BootstrapTombstone { .. }
            ) {
            FanOutKind::Bootstrap {
                open_after: matches!(frame, FrameKind::BootstrapReady { .. }),
                replace_legacy_begin: is_begin && !self.enforce_bootstrap_flow,
            }
        } else {
            FanOutKind::Content
        };
        let outbound = FanOutFrame {
            host,
            terminal: id,
            frame,
            kind,
        };
        let mut retained_bytes = self.retained_bytes;
        let mut retained_frames = self.retained_frames;
        subs.retain_mut(|sub| {
            Self::fan_out_to_subscriber(sub, &outbound, &mut retained_bytes, &mut retained_frames)
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

    pub(super) fn fan_out_to_subscriber(
        sub: &mut ProxySubscriber,
        outbound: &FanOutFrame<'_>,
        retained_bytes: &mut usize,
        retained_frames: &mut usize,
    ) -> bool {
        Self::observe_generation(sub, outbound.frame);
        if matches!(sub.gate, SnapshotGate::Retiring { .. }) {
            return Self::flush_pending_snapshot(sub).1;
        }
        if let FanOutKind::Bootstrap {
            open_after,
            replace_legacy_begin,
        } = outbound.kind
        {
            if replace_legacy_begin {
                sub.gate = SnapshotGate::AwaitingFirst;
            }
            let failure =
                Self::delivery_failure(sub, outbound.host, outbound.terminal, outbound.frame);
            return Self::push_bootstrap_frame(
                sub,
                outbound.frame.clone(),
                open_after,
                failure,
                retained_bytes,
                retained_frames,
            );
        }
        let awaiting_first = matches!(sub.gate, SnapshotGate::AwaitingFirst);
        let (may_send, alive) = Self::flush_pending_snapshot(sub);
        if !alive {
            return false;
        }
        if !may_send {
            if !awaiting_first {
                Self::retain_fan_out_frame(sub, outbound, retained_bytes, retained_frames);
            }
            return true;
        }
        Self::deliver_fan_out_frame(sub, outbound, retained_bytes, retained_frames)
    }

    pub(super) fn deliver_fan_out_frame(
        sub: &mut ProxySubscriber,
        outbound: &FanOutFrame<'_>,
        retained_bytes: &mut usize,
        retained_frames: &mut usize,
    ) -> bool {
        match sub.out_tx.try_send(Outbound::Frame(outbound.frame.clone())) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                Self::retain_fan_out_frame(sub, outbound, retained_bytes, retained_frames);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    pub(super) fn retain_fan_out_frame(
        sub: &mut ProxySubscriber,
        outbound: &FanOutFrame<'_>,
        retained_bytes: &mut usize,
        retained_frames: &mut usize,
    ) {
        let failure = Self::delivery_failure(sub, outbound.host, outbound.terminal, outbound.frame);
        Self::retain_ordered_frame(
            sub,
            outbound.frame.clone(),
            failure,
            retained_bytes,
            retained_frames,
        );
    }

    pub(super) fn push_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        open_after: bool,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        let gate = std::mem::replace(&mut sub.gate, SnapshotGate::AwaitingFirst);
        match gate {
            retiring @ SnapshotGate::Retiring { .. } => {
                sub.gate = retiring;
                true
            }
            SnapshotGate::Retained {
                frames,
                retained_bytes,
                open_after: prior_open_after,
            } => Self::retain_bootstrap_frame(
                sub,
                frame,
                frames,
                retained_bytes,
                prior_open_after || open_after,
                delivery_failure,
                connection_bytes,
                connection_frames,
            ),
            SnapshotGate::AwaitingFirst | SnapshotGate::Open => Self::deliver_bootstrap_frame(
                sub,
                frame,
                frame_bytes,
                open_after,
                delivery_failure,
                connection_bytes,
                connection_frames,
            ),
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "retained bootstrap state and both aggregate counters move together atomically"
    )]
    pub(super) fn retain_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        mut frames: VecDeque<FrameKind>,
        retained_bytes: usize,
        open_after: bool,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
        let frame_bytes = Self::retained_frame_bytes(&frame);
        if Self::retention_exceeded(
            frame_bytes,
            retained_bytes,
            frames.len(),
            *connection_bytes,
            *connection_frames,
        ) {
            *connection_bytes = connection_bytes.saturating_sub(retained_bytes);
            *connection_frames = connection_frames.saturating_sub(frames.len());
            warn!(client = ?sub.client, "relay subscriber exceeded retained delivery bounds; scheduling explicit failure");
            Self::begin_retirement(sub, delivery_failure);
            return true;
        }
        frames.push_back(frame);
        *connection_bytes += frame_bytes;
        *connection_frames += 1;
        sub.gate = SnapshotGate::Retained {
            frames,
            retained_bytes: retained_bytes.saturating_add(frame_bytes),
            open_after,
        };
        true
    }

    pub(super) fn deliver_bootstrap_frame(
        sub: &mut ProxySubscriber,
        frame: FrameKind,
        frame_bytes: usize,
        open_after: bool,
        delivery_failure: DeliveryFailure,
        connection_bytes: &mut usize,
        connection_frames: &mut usize,
    ) -> bool {
        match sub.out_tx.try_send(Outbound::Frame(frame.clone())) {
            Ok(()) => {
                sub.gate = if open_after {
                    SnapshotGate::Open
                } else {
                    SnapshotGate::AwaitingFirst
                };
                true
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                if Self::retention_exceeded(
                    frame_bytes,
                    0,
                    0,
                    *connection_bytes,
                    *connection_frames,
                ) {
                    Self::begin_retirement(sub, delivery_failure);
                    return true;
                }
                *connection_bytes += frame_bytes;
                *connection_frames += 1;
                sub.gate = SnapshotGate::Retained {
                    frames: VecDeque::from([frame]),
                    retained_bytes: frame_bytes,
                    open_after,
                };
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                sub.gate = SnapshotGate::Open;
                false
            }
        }
    }
}
