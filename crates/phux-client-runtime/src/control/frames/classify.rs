//! Classifying inbound frames: replica (engine) events, and the history
//! reasons they carry.

use super::super::{
    EngineEvent, FrameKind, HistoryRejectionReason, HistoryUnavailableReason, WireRejection,
    WireTombstone,
};

pub(super) enum ClassifiedFrame {
    Engine(EngineEvent),
    Other(FrameKind),
}

pub(super) fn classify_engine_frame(frame: FrameKind) -> ClassifiedFrame {
    let event = match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => EngineEvent::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => EngineEvent::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => EngineEvent::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: history_cursor.map(|cursor| cursor.to_vec()),
        },
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => EngineEvent::Tombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        },
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => EngineEvent::Output {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        },
        other => return classify_history_frame(other),
    };
    ClassifiedFrame::Engine(event)
}

fn classify_history_frame(frame: FrameKind) -> ClassifiedFrame {
    let event = match frame {
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => EngineEvent::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            rows,
            cursor: cursor.to_vec(),
            next_cursor: next_cursor.map(|cursor| cursor.to_vec()),
            payload,
        },
        FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => EngineEvent::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor: cursor.to_vec(),
            reason: history_unavailable_reason(reason),
        },
        FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => EngineEvent::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor: cursor.to_vec(),
            reason: history_rejection_reason(reason),
            required_bytes,
            required_rows,
        },
        other => return ClassifiedFrame::Other(other),
    };
    ClassifiedFrame::Engine(event)
}

/// The replica's reason for a history tombstone. A reason this build does not
/// know (a newer server's) retires the cursor as `Released`: the server let
/// go of it, and the replica tombstones history the same way.
const fn history_unavailable_reason(reason: WireTombstone) -> HistoryUnavailableReason {
    match reason {
        WireTombstone::Stale => HistoryUnavailableReason::Stale,
        WireTombstone::Pruned => HistoryUnavailableReason::Pruned,
        WireTombstone::Reset => HistoryUnavailableReason::Reset,
        WireTombstone::Resize => HistoryUnavailableReason::Resize,
        WireTombstone::Expired => HistoryUnavailableReason::Expired,
        WireTombstone::Limit => HistoryUnavailableReason::Limit,
        WireTombstone::CodecFailure => HistoryUnavailableReason::CodecFailure,
        // `Released`, and any reason a newer server added.
        _ => HistoryUnavailableReason::Released,
    }
}

/// The replica's reason for a history rejection. A reason this build does
/// not know (a newer server's) reads as `ZeroLimit`: the fetch is cancelled
/// without a retry or a tombstone, so a later scroll may ask again.
const fn history_rejection_reason(reason: WireRejection) -> HistoryRejectionReason {
    match reason {
        WireRejection::TooSmall => HistoryRejectionReason::TooSmall,
        WireRejection::Busy => HistoryRejectionReason::Busy,
        // `ZeroLimit`, and any reason a newer server added.
        _ => HistoryRejectionReason::ZeroLimit,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClassifiedFrame, EngineEvent, FrameKind, HistoryRejectionReason, HistoryUnavailableReason,
        WireRejection, WireTombstone, classify_engine_frame, history_rejection_reason,
        history_unavailable_reason,
    };
    use phux_protocol::ResourceId;
    use phux_protocol::ids::{BootstrapId, StreamId};

    /// The decoded payload moves into the engine event: an output flood, a
    /// bootstrap, or a history page is not copied a second time on its way
    /// to the replica.
    #[test]
    fn engine_payloads_share_the_decoded_buffer() {
        let payload = bytes::Bytes::from(vec![b'x'; 4096]);
        let terminal_id = ResourceId::Local { id: 7 };
        let stream_id = StreamId::new(1).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("generation");
        let frames = [
            FrameKind::ResourceOutput {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                seq: 1,
                bytes: payload.clone(),
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: payload.clone(),
            },
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq: 1,
                cursor: bytes::Bytes::from_static(b"c"),
                next_cursor: None,
                payload: payload.clone(),
                rows: 1,
            },
        ];
        for frame in frames {
            let ClassifiedFrame::Engine(event) = classify_engine_frame(frame) else {
                panic!("a stream frame classifies as an engine event");
            };
            let shared = match &event {
                EngineEvent::Output { bytes, .. } => bytes,
                EngineEvent::BootstrapChunk { payload, .. }
                | EngineEvent::HistoryPage { payload, .. } => payload,
                other => panic!("unexpected event {other:?}"),
            };
            assert_eq!(shared.as_ptr(), payload.as_ptr(), "{event:?} copied");
        }
    }

    /// A newer server's history reason degrades instead of failing the
    /// connection: an unknown tombstone retires the cursor, and an unknown
    /// rejection cancels the fetch without a retry.
    #[test]
    fn unknown_history_reasons_degrade_instead_of_failing() {
        assert_eq!(
            history_unavailable_reason(WireTombstone::Unknown),
            HistoryUnavailableReason::Released
        );
        assert_eq!(
            history_unavailable_reason(WireTombstone::Pruned),
            HistoryUnavailableReason::Pruned
        );
        assert_eq!(
            history_rejection_reason(WireRejection::Unknown),
            HistoryRejectionReason::ZeroLimit
        );
        assert_eq!(
            history_rejection_reason(WireRejection::Busy),
            HistoryRejectionReason::Busy
        );
    }
}
