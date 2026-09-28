//! Golden-file coverage against `docs/assets/pi-live-fleet.cast`, a real
//! asciicast v3 recording made by asciinema itself. Only a foreign file can
//! catch a codec that is self-consistently wrong about v3 intervals.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use std::io::BufReader;
use std::path::PathBuf;
use std::time::Duration;

use phux_record::cast::{CastEvent, CastHeader, CastVersion, CastWriter, EventCode, read_cast};

fn read_fixture() -> (CastHeader, Vec<CastEvent>) {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/assets/pi-live-fleet.cast");
    let file = std::fs::File::open(path).expect("fixture is committed at docs/assets/");
    read_cast(BufReader::new(file)).expect("fixture parses as asciicast v3")
}

#[test]
fn golden_v3_cast_parses_with_the_expected_shape() {
    let (header, events) = read_fixture();
    assert_eq!((header.cols, header.rows), (140, 40));
    assert_eq!(header.idle_time_limit, Some(2.0));
    assert_eq!(events.len(), 68);
    let outputs = events
        .iter()
        .filter(|event| event.code == EventCode::Output)
        .count();
    assert_eq!(outputs, 67);

    // v3 intervals summed into an absolute timeline; drift here means floats
    // are accumulating somewhere.
    assert_eq!(events.first().expect("non-empty").time_ms, 117);
    let last = events.last().expect("non-empty");
    assert_eq!(
        (last.time_ms, last.code, last.data.as_str()),
        (258_425, EventCode::Exit, "0")
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].time_ms <= pair[1].time_ms)
    );
}

#[test]
fn golden_v3_round_trips_through_both_writers_unchanged() {
    let (header, events) = read_fixture();
    for version in [CastVersion::V2, CastVersion::V3] {
        let mut writer = CastWriter::new(Vec::new(), &header, version).expect("writer opens");
        for event in &events {
            let at = Duration::from_millis(event.time_ms);
            match event.code {
                EventCode::Output => writer.output(at, event.data.as_bytes()),
                EventCode::Exit => writer.exit(at, event.data.parse().expect("int status")),
                other => panic!("fixture unexpectedly contains a {other:?} event"),
            }
            .expect("event writes");
        }
        let bytes = writer.finish().expect("writer finishes");
        let (reread_header, reread) = read_cast(bytes.as_slice()).expect("output re-reads");
        assert_eq!(reread_header, header, "{version:?}");
        assert_eq!(reread, events, "{version:?}");
    }
}

/// The whole pipeline on 140x40 of real output: the only place stage-boundary
/// mistakes (a band outliving a resize, a palette missing a colour, pass
/// counts disagreeing) show up. One test so the ~2 s renders run once each.
#[cfg(feature = "render")]
#[test]
fn golden_cast_renders_gif_and_apng_and_frame_counts_agree() {
    use phux_record::render::{OutputFormat, RenderOptions, render_cast};

    let (header, events) = read_fixture();
    let render = |format| {
        let mut sink: Vec<u8> = Vec::new();
        let stats = render_cast(
            &header,
            &events,
            &mut sink,
            format,
            &RenderOptions::default(),
        )
        .expect("the fixture renders end to end");
        assert!(stats.frames > 0 && !stats.truncated, "{stats:?}");
        assert_eq!(stats.bytes, sink.len() as u64);
        // Four minutes of session under a few KiB means dropped frames.
        assert!(sink.len() > 4096, "suspiciously small: {}", sink.len());
        (sink, stats)
    };

    let (gif, gif_stats) = render(OutputFormat::Gif);
    assert_eq!(gif.get(..6), Some(&b"GIF89a"[..]));
    assert_eq!(gif.last(), Some(&0x3b), "missing trailer");
    assert!(
        gif.windows(11).any(|window| window == b"NETSCAPE2.0"),
        "a GIF without the loop extension plays once"
    );
    assert_eq!(gif_stats.colors, 2, "an unstyled recording is fg and bg");

    let (apng, apng_stats) = render(OutputFormat::Apng);
    assert!(apng.windows(4).any(|window| window == b"acTL"));
    assert!(apng.windows(4).any(|window| window == b"IEND"));
    assert_eq!(gif_stats.frames, apng_stats.frames);
    assert_eq!(gif_stats.duration_ms, apng_stats.duration_ms);
}
