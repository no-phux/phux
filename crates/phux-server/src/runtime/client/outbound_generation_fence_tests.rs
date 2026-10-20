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
    use crate::stream_diagnostics::{ResyncReason, StreamContext, StreamDiagnostics, StreamLane};
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
