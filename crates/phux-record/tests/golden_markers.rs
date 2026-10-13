//! Golden marker timing. The numbers are the contract for `--speed-marked 2`:
//! a marker pair compresses the span it brackets, and the cast round-trips
//! the `m` events before that remap.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::time::Duration;

use phux_record::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode, read_cast};
use phux_record::compress_marked;

fn event(time_ms: u64, code: EventCode, data: &str) -> CastEvent {
    CastEvent {
        time_ms,
        code,
        data: data.to_owned(),
    }
}

fn times(events: &[CastEvent]) -> Vec<u64> {
    events.iter().map(|event| event.time_ms).collect()
}

/// The demo timeline: one second of real time, a four-second marked span,
/// then three more seconds. At 2x the span becomes two seconds and the
/// tail shifts from 8s to 6s.
#[test]
fn golden_marker_pair_at_2x_lands_on_these_timestamps() {
    let events = vec![
        event(0, EventCode::Output, "open"),
        event(1_000, EventCode::Marker, "demo"),
        event(3_000, EventCode::Output, "middle"),
        event(5_000, EventCode::Marker, "demo"),
        event(8_000, EventCode::Output, "close"),
    ];
    let got = compress_marked(&events, 2.0);
    assert_eq!(times(&got), vec![0, 1_000, 2_000, 3_000, 6_000]);
    assert_eq!(
        got.iter()
            .filter(|event| event.code == EventCode::Marker)
            .map(|event| event.data.as_str())
            .collect::<Vec<_>>(),
        ["demo", "demo"]
    );
}

/// Markers survive a v2 write/read before the renderer compresses them, so
/// a cast recorded live and a cast re-read from disk share one timeline.
#[test]
fn golden_markers_round_trip_through_the_cast_before_compression() {
    let header = CastHeader {
        cols: 80,
        rows: 24,
        ..CastHeader::default()
    };
    let events = vec![
        event(0, EventCode::Output, "a"),
        event(2_000, EventCode::Marker, "label"),
        event(6_000, EventCode::Output, "b"),
    ];
    let mut writer = CastWriter::new(Vec::new(), &header, CastVersion::V2).expect("writer");
    for event in &events {
        let at = Duration::from_millis(event.time_ms);
        match event.code {
            EventCode::Output => writer.output(at, event.data.as_bytes()),
            EventCode::Marker => writer.marker(at, &event.data),
            other => panic!("unexpected {other:?}"),
        }
        .expect("write");
    }
    let bytes = writer.finish().expect("finish");
    let (_header, reread) = read_cast(bytes.as_slice()).expect("read");
    assert_eq!(reread, events);
    // Unclosed marker: 2s..6s is 4s, which 2x shortens to 2s, so 6s -> 4s.
    assert_eq!(times(&compress_marked(&reread, 2.0)), vec![0, 2_000, 4_000]);
}
