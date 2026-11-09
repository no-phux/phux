//! Adapter tests: shared snapshot fixtures and helpers.

mod native;
mod projection;
mod synthesized;

use super::document::{NeedleScan, history_point, history_row_wrapped_at, scan_history_for_needle};
use super::projection::ProjectedCell;
use super::snapshot::{EncodedSnapshot, SnapshotCaptureLimits};
use super::*;

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
    let capture = EncodedSnapshot::capture(&mut source, capture_limits()).expect("capture");
    let ready_end = capture.prefix_records.last().unwrap().end;
    let ready = capture.bytes[..ready_end].to_vec();
    let mut history = Vec::new();
    let mut finish = Vec::new();
    for unit in &capture.history {
        let bytes = capture.bytes[unit.range.clone()].to_vec();
        if unit.finish {
            finish = bytes;
        } else {
            history.push((bytes, u32::try_from(unit.rows).unwrap()));
        }
    }
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

fn capture_limits() -> SnapshotCaptureLimits {
    SnapshotCaptureLimits {
        max_bytes: 64 * 1024 * 1024,
        max_record_bytes: 64 * 1024,
        max_records: 10_000,
        max_rows: 10_000,
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
    let capture = EncodedSnapshot::capture(&mut source, capture_limits()).unwrap();
    let ready = &capture.bytes[..capture.prefix_records.last().unwrap().end];
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), CanonicalGeometry::new(8, 3).unwrap())
        .unwrap();
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, ready, &mut effects)
        .unwrap();
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .unwrap();
    assert_eq!(
        kitty_pixels(replica.terminal().unwrap(), 42),
        None,
        "pixels are deferred past READY"
    );
    let mut pixel_units = 0;
    for unit in &capture.history {
        if unit.rows == 0 && !unit.finish {
            pixel_units += 1;
        }
        let outcome = adapter
            .apply_history_page(
                &mut replica,
                &capture.bytes[unit.range.clone()],
                u32::try_from(unit.rows).unwrap(),
                &mut effects,
            )
            .unwrap();
        if unit.finish {
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
