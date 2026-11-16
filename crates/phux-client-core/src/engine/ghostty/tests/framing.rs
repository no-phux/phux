//! Reader-local framing must preserve stock snapshots and preapply rejection.

use super::*;
use phux_protocol::snapshot_framing::{HistoryUnit, history_unit};

fn image_source() -> GhosttyTerminal<'static, 'static> {
    let mut source = GhosttyTerminal::new(200, 3).unwrap();
    source.set_scrollback_max_bytes(Some(usize::MAX)).unwrap();
    source.set_scrollback_max_lines(Some(5000)).unwrap();
    phux_vt::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,q=2;/wAA/w==\x1b\\");
    source.vt_write(&b"history\r\n".repeat(1200));
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=8,q=2;/wAA/w==\x1b\\");
    source
}

fn publish(capture: &EncodedSnapshot) -> (GhosttyAdapter, GhosttyReplica, EngineEffectBuffer) {
    let prefix_end = capture.prefix_records.last().unwrap().end;
    published(native_profile(), &capture.bytes[..prefix_end])
}

fn placement_offset(unit: &[u8]) -> usize {
    let mut offset = 0;
    loop {
        let tag = u16::from_le_bytes(unit[offset..offset + 2].try_into().unwrap());
        if tag == 10 {
            return offset;
        }
        let len = u32::from_le_bytes(unit[offset + 2..offset + 6].try_into().unwrap()) as usize;
        offset += 10 + len;
    }
}

#[test]
fn corrupt_placement_and_unknown_suffix_are_rejected_before_page_application() {
    for unknown in [false, true] {
        let capture = EncodedSnapshot::capture(&mut image_source(), capture_limits()).unwrap();
        let (mut adapter, mut replica, mut effects) = publish(&capture);
        let mut rejected = false;
        for record in &capture.history {
            let unit = &capture.bytes[record.range.clone()];
            if matches!(
                history_unit(unit),
                Some(HistoryUnit::Page {
                    placements: true,
                    ..
                })
            ) {
                let mut corrupt = unit.to_vec();
                if unknown {
                    // A valid optional record whose payload is then corrupted.
                    let optional = [0, 64, 1, 0, 0, 0, 232, 82, 106, 118, b'y'];
                    corrupt.splice(placement_offset(unit)..placement_offset(unit), optional);
                } else {
                    *corrupt.last_mut().unwrap() ^= 1;
                }
                let rows_before = replica.terminal().unwrap().scrollback_rows().unwrap();
                assert!(
                    adapter
                        .apply_history_page(
                            &mut replica,
                            &corrupt,
                            u32::try_from(record.rows).unwrap(),
                            &mut effects
                        )
                        .is_err()
                );
                assert_eq!(
                    replica.terminal().unwrap().scrollback_rows().unwrap(),
                    rows_before
                );
                assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0);
                assert!(matches!(
                    adapter.apply_history_page(
                        &mut replica,
                        unit,
                        u32::try_from(record.rows).unwrap(),
                        &mut effects
                    ),
                    Err(GhosttyEngineError::DecoderFailed)
                ));
                adapter
                    .apply_output(&mut replica, b"LIVE", &mut effects)
                    .unwrap();
                rejected = true;
                break;
            }
            adapter
                .apply_history_page(
                    &mut replica,
                    unit,
                    u32::try_from(record.rows).unwrap(),
                    &mut effects,
                )
                .unwrap();
        }
        assert!(rejected, "fixture must include real history placements");
    }
}

#[test]
fn stock_pages_with_and_without_placements_finish_without_transport_lookahead() {
    let mut source = image_source();
    let expected_rows = source.scrollback_rows().unwrap();
    let capture = EncodedSnapshot::capture(&mut source, capture_limits()).unwrap();
    let (mut adapter, mut replica, mut effects) = publish(&capture);
    let mut bare = false;
    let mut placed = false;
    for record in &capture.history {
        let unit = &capture.bytes[record.range.clone()];
        match history_unit(unit).unwrap() {
            HistoryUnit::Page {
                placements: true, ..
            } => placed = true,
            HistoryUnit::Page {
                placements: false, ..
            } => bare = true,
            _ => {}
        }
        adapter
            .apply_history_page(
                &mut replica,
                unit,
                u32::try_from(record.rows).unwrap(),
                &mut effects,
            )
            .unwrap();
        assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0);
    }
    assert!(bare && placed);
    assert_eq!(
        replica.terminal().unwrap().scrollback_rows().unwrap(),
        expected_rows
    );
    assert_eq!(
        kitty_pixels(replica.terminal().unwrap(), 7),
        Some(vec![255, 0, 0, 255])
    );
    assert_eq!(
        kitty_pixels(replica.terminal().unwrap(), 8),
        Some(vec![255, 0, 0, 255])
    );
}

#[test]
fn primary_page_suffix_uses_history_key_not_active_alternate_screen() {
    let mut source = image_source();
    source.vt_write(b"\x1b[?1049hAlternate");
    let capture = EncodedSnapshot::capture(&mut source, capture_limits()).unwrap();
    let (mut adapter, mut replica, mut effects) = publish(&capture);
    let mut bare = false;
    for record in &capture.history {
        let unit = &capture.bytes[record.range.clone()];
        bare |= matches!(
            history_unit(unit),
            Some(HistoryUnit::Page {
                placements: false,
                ..
            })
        );
        adapter
            .apply_history_page(
                &mut replica,
                unit,
                u32::try_from(record.rows).unwrap(),
                &mut effects,
            )
            .unwrap();
    }
    assert!(bare);
    assert_eq!(
        replica.terminal().unwrap().active_screen().unwrap(),
        libghostty_vt::screen::Screen::Alternate
    );
    adapter
        .apply_output(&mut replica, b"\x1b[?1049l", &mut effects)
        .unwrap();
    source.vt_write(b"\x1b[?1049l");
    assert_eq!(
        replica.terminal().unwrap().scrollback_rows().unwrap(),
        source.scrollback_rows().unwrap()
    );
}
