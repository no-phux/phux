//! Wire frames as session-kernel inputs, and the history reasons they carry.

use phux_client_core::engine::CanonicalGeometry;
use phux_client_core::session::{
    HistoryRejectionReason as KernelHistoryRejectionReason, HistoryUnavailableReason, KernelInput,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, HistoryRejectionReason, HistoryTombstoneReason};

/// Translate a wire frame into its kernel input; `None` when not modeled.
pub(super) fn kernel_input_for<'a>(
    frame: &'a FrameKind,
    terminals: &'a [ResourceId],
) -> Option<KernelInput<'a>> {
    attach_stream_input(frame, terminals)
        .or_else(|| content_stream_input(frame))
        .or_else(|| history_stream_input(frame))
}

/// The attach barrier and bootstrap-transcript frames.
fn attach_stream_input<'a>(
    frame: &'a FrameKind,
    terminals: &'a [ResourceId],
) -> Option<KernelInput<'a>> {
    match frame {
        FrameKind::Attached { attach_id, .. } => Some(KernelInput::AttachStarted {
            attach_id: *attach_id,
            terminals,
        }),
        FrameKind::AttachReady { attach_id } => Some(KernelInput::AttachReady {
            attach_id: *attach_id,
        }),
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => Some(KernelInput::BootstrapBegin {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            profile: *profile,
            geometry: CanonicalGeometry {
                cols: *cols,
                rows: *rows,
            },
            base_seq: *base_seq,
        }),
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => Some(KernelInput::BootstrapChunk {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            chunk_seq: *chunk_seq,
            payload,
        }),
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => Some(KernelInput::BootstrapReady {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            history_cursor: history_cursor.as_deref(),
        }),
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => Some(KernelInput::Tombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            reason: *reason,
            last_valid_seq: *last_valid_seq,
        }),
        _ => None,
    }
}

/// The live-content frames: applied VT bytes and the pane's permanent close.
fn content_stream_input(frame: &FrameKind) -> Option<KernelInput<'_>> {
    match frame {
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => Some(KernelInput::ResourceOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        }),
        FrameKind::ResourceClosed {
            terminal_id,
            exit_status,
            reason,
            signal,
        } => Some(KernelInput::ResourceClosed {
            terminal_id,
            exit_status: *exit_status,
            signal: *signal,
            reason: *reason,
        }),
        _ => None,
    }
}

/// The scrollback-page frames. A reason a newer server added degrades
/// rather than rejecting the route.
fn history_stream_input(frame: &FrameKind) -> Option<KernelInput<'_>> {
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

/// The kernel's reason for a history tombstone. A reason this build does
/// not know (a newer server's) retires the cursor as `Released`: the server
/// let go of it, and the kernel tombstones history the same way.
pub(super) const fn history_unavailable_reason(
    reason: HistoryTombstoneReason,
) -> HistoryUnavailableReason {
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
pub(super) const fn history_rejection_reason(
    reason: HistoryRejectionReason,
) -> KernelHistoryRejectionReason {
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

    /// A newer server's history reason degrades instead of rejecting the
    /// route, which would end the attach.
    #[test]
    fn unknown_history_reasons_degrade_instead_of_failing() {
        assert_eq!(
            history_unavailable_reason(HistoryTombstoneReason::Unknown),
            HistoryUnavailableReason::Released
        );
        assert_eq!(
            history_unavailable_reason(HistoryTombstoneReason::Stale),
            HistoryUnavailableReason::Stale
        );
        assert_eq!(
            history_rejection_reason(HistoryRejectionReason::Unknown),
            KernelHistoryRejectionReason::ZeroLimit
        );
        assert_eq!(
            history_rejection_reason(HistoryRejectionReason::TooSmall),
            KernelHistoryRejectionReason::TooSmall
        );
    }
}
