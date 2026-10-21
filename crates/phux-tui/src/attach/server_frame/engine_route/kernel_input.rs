//! Wire frames as session-kernel inputs, and the history reasons they carry.

use phux_client_core::engine::CanonicalGeometry;
use phux_client_core::session::{
    HistoryRejectionReason as KernelHistoryRejectionReason, HistoryUnavailableReason, KernelInput,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, HistoryRejectionReason, HistoryTombstoneReason};

/// Translate a wire frame into its kernel input. `Ok(None)` ⇒ not modeled;
/// `Err` ⇒ an unknown history reason, reported as a rejected route.
pub(super) fn kernel_input_for<'a>(
    frame: &'a FrameKind,
    terminals: &'a [ResourceId],
) -> Result<Option<KernelInput<'a>>, &'static str> {
    if let Some(input) = attach_stream_input(frame, terminals) {
        return Ok(Some(input));
    }
    if let Some(input) = content_stream_input(frame) {
        return Ok(Some(input));
    }
    history_stream_input(frame)
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

/// The scrollback-page frames; an unrecognised reason is a rejected route.
fn history_stream_input(frame: &FrameKind) -> Result<Option<KernelInput<'_>>, &'static str> {
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
            reason: history_unavailable_reason(*reason)
                .ok_or("unsupported history tombstone reason")?,
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
            reason: history_rejection_reason(*reason)
                .ok_or("unsupported history rejection reason")?,
            required_bytes: *required_bytes,
            required_rows: *required_rows,
        },
        _ => return Ok(None),
    };
    Ok(Some(input))
}

pub(super) const fn history_unavailable_reason(
    reason: HistoryTombstoneReason,
) -> Option<HistoryUnavailableReason> {
    Some(match reason {
        HistoryTombstoneReason::Stale => HistoryUnavailableReason::Stale,
        HistoryTombstoneReason::Pruned => HistoryUnavailableReason::Pruned,
        HistoryTombstoneReason::Reset => HistoryUnavailableReason::Reset,
        HistoryTombstoneReason::Resize => HistoryUnavailableReason::Resize,
        HistoryTombstoneReason::Expired => HistoryUnavailableReason::Expired,
        HistoryTombstoneReason::Released => HistoryUnavailableReason::Released,
        HistoryTombstoneReason::Limit => HistoryUnavailableReason::Limit,
        HistoryTombstoneReason::CodecFailure => HistoryUnavailableReason::CodecFailure,
        _ => return None,
    })
}

pub(super) const fn history_rejection_reason(
    reason: HistoryRejectionReason,
) -> Option<KernelHistoryRejectionReason> {
    Some(match reason {
        HistoryRejectionReason::ZeroLimit => KernelHistoryRejectionReason::ZeroLimit,
        HistoryRejectionReason::TooSmall => KernelHistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy => KernelHistoryRejectionReason::Busy,
        _ => return None,
    })
}
