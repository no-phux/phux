//! Markers round-trip through a cast file, so `--speed-marked` compresses the
//! same timeline whether the cast was recorded live or re-read from disk.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::time::Duration;

use phux_record::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode, read_cast};

fn event(time_ms: u64, code: EventCode, data: &str) -> CastEvent {
    CastEvent {
        time_ms,
        code,
        data: data.to_owned(),
    }
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
}
