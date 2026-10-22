//! Adapter tests: shared snapshot fixtures and helpers.

mod native;
mod projection;
mod synthesized;

use super::document::{NeedleScan, history_point, history_row_wrapped_at, scan_history_for_needle};
use super::projection::ProjectedCell;
use super::*;
use libghostty_vt::snapshot::{CaptureEvent, CaptureOptions};

fn geometry() -> CanonicalGeometry {
    CanonicalGeometry::new(80, 4).expect("valid geometry")
}

fn native_profile() -> BootstrapStreamProfile {
    BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttySnapshotV1,
    }
}

struct CapturedSnapshot {
    ready: Vec<u8>,
    history: Vec<(Vec<u8>, u32)>,
    finish: Vec<u8>,
}

fn capture_snapshot() -> CapturedSnapshot {
    let mut source = GhosttyTerminal::new(200, 3).expect("source terminal");
    source
        .set_scrollback_max_lines(Some(5_000))
        .expect("scrollback");
    source
        .set_scrollback_max_bytes(None)
        .expect("unlimited scrollback bytes");
    source
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .expect("continuation");
    for line in 0..3_000 {
        source.vt_write(format!("line {line}\r\n").as_bytes());
    }
    source.vt_write(b"\x1b]2;checkpoint-title\x07");
    let mut capture = source
        .capture_snapshot(CaptureOptions {
            max_record_bytes: 64 * 1024,
            max_pages: 1_000,
        })
        .expect("capture");
    let mut buffer = vec![0; 64 * 1024];
    let mut ready = Vec::new();
    loop {
        let event = capture.next(&mut buffer).expect("READY record");
        ready.extend_from_slice(&buffer[..event.written()]);
        if matches!(event, CaptureEvent::Ready { .. }) {
            break;
        }
    }
    let mut capture = capture.detach().expect("detached history");
    let mut pending = Vec::new();
    let mut history = Vec::new();
    let finish = loop {
        let event = capture
            .next(&mut source, &mut buffer)
            .expect("history record");
        pending.extend_from_slice(&buffer[..event.written()]);
        match event {
            CaptureEvent::HistoryPage { rows, .. } => history.push((
                std::mem::take(&mut pending),
                u32::try_from(rows).expect("page rows"),
            )),
            CaptureEvent::Finish { .. } => break std::mem::take(&mut pending),
            CaptureEvent::Scan | CaptureEvent::Record { .. } => {}
            CaptureEvent::Invalidated(reason) => panic!("capture invalidated: {reason:?}"),
            CaptureEvent::Ready { .. } => unreachable!(),
        }
    };
    assert!(
        history.len() >= 2,
        "fixture must exercise progressive history"
    );
    CapturedSnapshot {
        ready,
        history,
        finish,
    }
}

fn apply_history(
    adapter: &mut GhosttyAdapter,
    replica: &mut GhosttyReplica,
    captured: &CapturedSnapshot,
    effects: &mut EngineEffectBuffer,
) {
    for (page, rows) in &captured.history {
        let outcome = adapter
            .apply_history_page(replica, page, *rows, effects)
            .expect("history page");
        assert_eq!(outcome.progress, BootstrapProgress::Ready);
        assert_eq!(
            adapter.bootstrap_staging_bytes(replica),
            0,
            "decoder must release each complete history unit"
        );
    }
    assert_eq!(
        adapter
            .apply_history_page(replica, &captured.finish, 0, effects)
            .expect("authenticated FINISH")
            .progress,
        BootstrapProgress::Finished
    );
}

fn native_adapter() -> GhosttyAdapter {
    GhosttyAdapter::new(BootstrapLimits::default())
}
