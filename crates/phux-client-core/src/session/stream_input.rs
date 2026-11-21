//! Borrowed protocol stream frames as session-kernel inputs.

use phux_protocol::wire::frame::{FrameKind, HistoryRejectionReason, HistoryTombstoneReason};

use super::{
    CanonicalGeometry, HistoryRejectionReason as KernelHistoryRejectionReason,
    HistoryUnavailableReason, KernelInput,
};

impl<'a> KernelInput<'a> {
    /// Borrow a bootstrap, live output, or progressive history input from a
    /// decoded frame. The frame retains both the payload and cursor buffers.
    #[inline]
    #[must_use]
    pub fn from_stream_frame(frame: &'a FrameKind) -> Option<Self> {
        let input = match frame {
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => KernelInput::ResourceOutput {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                seq: *seq,
                payload: bytes,
            },
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => KernelInput::BootstrapBegin {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                profile: *profile,
                geometry: CanonicalGeometry {
                    cols: *cols,
                    rows: *rows,
                },
                base_seq: *base_seq,
            },
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => KernelInput::BootstrapChunk {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                chunk_seq: *chunk_seq,
                payload,
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => KernelInput::BootstrapReady {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                history_cursor: history_cursor.as_deref(),
            },
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => KernelInput::Tombstone {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                reason: *reason,
                last_valid_seq: *last_valid_seq,
            },
            _ => return Self::from_history_frame(frame),
        };
        Some(input)
    }

    fn from_history_frame(frame: &'a FrameKind) -> Option<Self> {
        let input = match frame {
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                rows,
                page_seq,
                cursor,
                next_cursor,
                payload,
            } => KernelInput::HistoryPage {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                rows: *rows,
                page_seq: *page_seq,
                payload,
                cursor,
                next_cursor: next_cursor.as_deref(),
            },
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => KernelInput::HistoryTombstone {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                cursor,
                reason: history_unavailable_reason(*reason),
            },
            FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => KernelInput::HistoryRejected {
                terminal_id,
                stream_id: *stream_id,
                bootstrap_id: *bootstrap_id,
                cursor,
                reason: history_rejection_reason(*reason),
                required_bytes: *required_bytes,
                required_rows: *required_rows,
            },
            _ => return None,
        };
        Some(input)
    }
}

/// The kernel's reason for a history tombstone. A reason this build does
/// not know (a newer server's) retires the cursor as `Released`: the server
/// let go of it, and the kernel tombstones history the same way.
const fn history_unavailable_reason(reason: HistoryTombstoneReason) -> HistoryUnavailableReason {
    match reason {
        HistoryTombstoneReason::Stale => HistoryUnavailableReason::Stale,
        HistoryTombstoneReason::Pruned => HistoryUnavailableReason::Pruned,
        HistoryTombstoneReason::Reset => HistoryUnavailableReason::Reset,
        HistoryTombstoneReason::Resize => HistoryUnavailableReason::Resize,
        HistoryTombstoneReason::Expired => HistoryUnavailableReason::Expired,
        HistoryTombstoneReason::Limit => HistoryUnavailableReason::Limit,
        HistoryTombstoneReason::CodecFailure => HistoryUnavailableReason::CodecFailure,
        // `Released`, and any reason a newer server added.
        _ => HistoryUnavailableReason::Released,
    }
}

/// The kernel's reason for a history rejection. A reason this build does not
/// know (a newer server's) reads as `ZeroLimit`: the fetch is cancelled
/// without a retry or a tombstone, so a later scroll may ask again.
const fn history_rejection_reason(reason: HistoryRejectionReason) -> KernelHistoryRejectionReason {
    match reason {
        HistoryRejectionReason::TooSmall => KernelHistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy => KernelHistoryRejectionReason::Busy,
        // `ZeroLimit`, and any reason a newer server added.
        _ => KernelHistoryRejectionReason::ZeroLimit,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HistoryRejectionReason, HistoryTombstoneReason, HistoryUnavailableReason,
        KernelHistoryRejectionReason, history_rejection_reason, history_unavailable_reason,
    };

    #[test]
    fn history_input_borrows_the_decoded_payload_and_cursors() {
        use phux_protocol::{BootstrapId, ResourceId, StreamId};

        let frame = super::FrameKind::HistoryPage {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("generation"),
            page_seq: 1,
            rows: 1,
            cursor: b"current".to_vec().into(),
            next_cursor: Some(b"next".to_vec().into()),
            payload: vec![b'x'; 4096].into(),
        };
        let super::FrameKind::HistoryPage {
            payload,
            cursor,
            next_cursor: Some(next),
            ..
        } = &frame
        else {
            panic!("history frame");
        };
        let Some(super::KernelInput::HistoryPage {
            payload: input_payload,
            cursor: input_cursor,
            next_cursor: Some(input_next),
            ..
        }) = super::KernelInput::from_stream_frame(&frame)
        else {
            panic!("history input");
        };
        assert_eq!(input_payload.as_ptr(), payload.as_ptr());
        assert_eq!(input_cursor.as_ptr(), cursor.as_ptr());
        assert_eq!(input_next.as_ptr(), next.as_ptr());
    }

    /// A newer server's history reason degrades instead of rejecting the
    /// route, which would end the attach.
    #[test]
    fn unknown_history_reasons_degrade_instead_of_failing() {
        assert_eq!(
            history_unavailable_reason(HistoryTombstoneReason::Unknown),
            HistoryUnavailableReason::Released
        );
        assert_eq!(
            history_rejection_reason(HistoryRejectionReason::Unknown),
            KernelHistoryRejectionReason::ZeroLimit
        );
    }
}
