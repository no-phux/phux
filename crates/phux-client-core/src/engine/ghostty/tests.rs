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

/// The pinned native codec cannot be offered by an image-painting consumer.
#[test]
fn native_snapshot_ready_does_not_restore_kitty_image_storage() {
    let mut source = GhosttyTerminal::new(8, 3).unwrap();
    source
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.resize(8, 3, 8, 16).unwrap();
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=2,C=1;/wAA/w==\x1b\\");
    assert!(source.kitty_graphics().unwrap().image(42).is_some());
    let mut capture = source
        .capture_snapshot(CaptureOptions {
            max_record_bytes: 64 * 1024,
            max_pages: 100,
        })
        .unwrap();
    let mut bytes = vec![0; 64 * 1024];
    let mut ready = Vec::new();
    loop {
        let event = capture.next(&mut bytes).unwrap();
        ready.extend_from_slice(&bytes[..event.written()]);
        if matches!(event, CaptureEvent::Ready { .. }) {
            break;
        }
    }
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), CanonicalGeometry::new(8, 3).unwrap())
        .unwrap();
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &ready, &mut effects)
        .unwrap();
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .unwrap();
    assert!(
        replica
            .terminal()
            .unwrap()
            .kitty_graphics()
            .unwrap()
            .image(42)
            .is_none()
    );
    // Import still enables storage and the shared decoder for subsequent output.
    adapter
        .apply_output(
            &mut replica,
            b"\x1b_Ga=T,f=32,s=1,v=1,i=43,c=1,r=1;/wAA/w==\x1b\\",
            &mut effects,
        )
        .unwrap();
    assert!(
        replica
            .terminal()
            .unwrap()
            .kitty_graphics()
            .unwrap()
            .image(43)
            .is_some()
    );
}
