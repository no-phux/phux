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
        .set_scrollback_max_bytes(Some(usize::MAX))
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
            CaptureEvent::Image { .. } => history.push((std::mem::take(&mut pending), 0)),
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

/// Upstream 1.0 restores image metadata at READY and pixels in later units.
#[test]
fn native_snapshot_restores_kitty_metadata_then_deferred_pixels() {
    let mut source = GhosttyTerminal::new(8, 3).unwrap();
    source
        .set_continuation_max_bytes(CONTINUATION_LIMIT)
        .unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.resize(8, 3, 8, 16).unwrap();
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,c=2,r=2,C=1;/wAA/w==\x1b\\");
    assert_eq!(kitty_pixels(&source, 42), Some(vec![255, 0, 0, 255]));
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
    assert_eq!(
        kitty_pixels(replica.terminal().unwrap(), 42),
        None,
        "pixels are deferred past READY"
    );
    let mut capture = capture.detach().unwrap();
    let mut pending = Vec::new();
    let mut pixel_units = 0;
    loop {
        let event = capture.next(&mut source, &mut bytes).unwrap();
        pending.extend_from_slice(&bytes[..event.written()]);
        let declared_rows = match event {
            CaptureEvent::Image { .. } => {
                pixel_units += 1;
                0
            }
            CaptureEvent::HistoryPage { rows, .. } => u32::try_from(rows).unwrap(),
            CaptureEvent::Finish { .. } => 0,
            CaptureEvent::Scan | CaptureEvent::Record { .. } => continue,
            other => panic!("unexpected capture event: {other:?}"),
        };
        let outcome = adapter
            .apply_history_page(&mut replica, &pending, declared_rows, &mut effects)
            .unwrap();
        pending.clear();
        if matches!(event, CaptureEvent::Finish { .. }) {
            assert_eq!(outcome.progress, BootstrapProgress::Finished);
            break;
        }
        assert_eq!(
            outcome.progress,
            BootstrapProgress::Ready,
            "zero-row image is not FINISH"
        );
    }
    assert!(pixel_units > 0);
    assert_eq!(
        kitty_pixels(replica.terminal().unwrap(), 42),
        Some(vec![255, 0, 0, 255])
    );
    // Import also enables the shared decoder for subsequent live output.
    adapter
        .apply_output(
            &mut replica,
            b"\x1b_Ga=T,f=32,s=1,v=1,i=43,c=1,r=1;/wAA/w==\x1b\\",
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        kitty_pixels(replica.terminal().unwrap(), 43),
        Some(vec![255, 0, 0, 255])
    );
}

fn kitty_pixels(terminal: &GhosttyTerminal<'_, '_>, id: u32) -> Option<Vec<u8>> {
    terminal
        .kitty_graphics()
        .unwrap()
        .expect("Kitty graphics enabled")
        .image(id)
        .expect("image metadata")
        .data()
        .unwrap()
        .map(<[u8]>::to_vec)
}
