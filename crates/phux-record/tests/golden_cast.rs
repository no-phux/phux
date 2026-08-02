//! Golden-file coverage for the asciicast codec.
//!
//! The fixture is `docs/assets/pi-live-fleet.cast`, a real asciicast **v3**
//! recording already committed to the repo (it backs
//! `docs/pi-live-fleet-proof.md`). Testing against a file produced by
//! asciinema itself — rather than by our own writer — is the only way to
//! catch a codec that is self-consistently wrong: v3's relative intervals in
//! particular are exactly the sort of thing a round-trip-only test would
//! happily confirm in the wrong direction.

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
#[cfg(feature = "render")]
use phux_record::render::{OutputFormat, RenderOptions, RenderStats, render_cast};

/// Absolute path to the committed fixture.
fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/assets/pi-live-fleet.cast")
}

fn read_fixture() -> (CastHeader, Vec<CastEvent>) {
    let file = std::fs::File::open(fixture()).expect("fixture is committed at docs/assets/");
    read_cast(BufReader::new(file)).expect("fixture parses as asciicast v3")
}

#[test]
fn golden_v3_cast_parses_with_the_expected_shape() {
    let (header, events) = read_fixture();

    assert_eq!(header.cols, 140, "v3 dims come from term.cols");
    assert_eq!(header.rows, 40, "v3 dims come from term.rows");
    assert_eq!(header.idle_time_limit, Some(2.0));

    assert_eq!(events.len(), 68);
    let outputs = events
        .iter()
        .filter(|event| event.code == EventCode::Output)
        .count();
    let exits = events
        .iter()
        .filter(|event| event.code == EventCode::Exit)
        .count();
    assert_eq!(outputs, 67);
    assert_eq!(exits, 1);

    // v3 intervals summed into an absolute timeline. If this drifts, the
    // reader is accumulating floats somewhere it should be accumulating
    // integers.
    let last = events.last().expect("non-empty");
    assert_eq!(last.time_ms, 258_425);
    assert_eq!(last.code, EventCode::Exit);
    assert_eq!(last.data, "0");
}

#[test]
fn golden_v3_timeline_is_monotonic_and_starts_where_the_fixture_does() {
    let (_, events) = read_fixture();
    let first = events.first().expect("non-empty");
    assert_eq!(first.time_ms, 117, "first interval is 0.117 s");
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].time_ms <= pair[1].time_ms),
        "absolute times must never go backwards"
    );
}

#[test]
fn golden_v3_round_trips_through_the_v2_writer_unchanged() {
    let (header, events) = read_fixture();

    let mut writer =
        CastWriter::new(Vec::new(), &header, CastVersion::V2).expect("v2 writer opens");
    for event in &events {
        let at = Duration::from_millis(event.time_ms);
        match event.code {
            EventCode::Output => writer
                .output(at, event.data.as_bytes())
                .expect("output event"),
            EventCode::Exit => {
                let status: i32 = event.data.parse().expect("fixture exit status is an int");
                writer.exit(at, status).expect("exit event");
            }
            other => panic!("fixture unexpectedly contains a {other:?} event"),
        }
    }
    let bytes = writer.finish().expect("v2 writer finishes");

    let (v2_header, v2_events) = read_cast(bytes.as_slice()).expect("v2 output re-reads");

    assert_eq!(v2_header.cols, header.cols);
    assert_eq!(v2_header.rows, header.rows);
    assert_eq!(v2_header.idle_time_limit, header.idle_time_limit);
    assert_eq!(v2_header.title, header.title);
    assert_eq!(v2_header.command, header.command);
    assert_eq!(v2_header.env, header.env);

    assert_eq!(v2_events.len(), events.len());
    for (original, round_tripped) in events.iter().zip(&v2_events) {
        assert_eq!(
            round_tripped.time_ms, original.time_ms,
            "absolute time drift"
        );
        assert_eq!(round_tripped.code, original.code);
        assert_eq!(round_tripped.data, original.data);
    }
}

#[test]
fn golden_v3_round_trips_through_the_v3_writer_unchanged() {
    let (header, events) = read_fixture();

    let mut writer =
        CastWriter::new(Vec::new(), &header, CastVersion::V3).expect("v3 writer opens");
    for event in &events {
        let at = Duration::from_millis(event.time_ms);
        match event.code {
            EventCode::Output => writer
                .output(at, event.data.as_bytes())
                .expect("output event"),
            EventCode::Exit => {
                let status: i32 = event.data.parse().expect("fixture exit status is an int");
                writer.exit(at, status).expect("exit event");
            }
            other => panic!("fixture unexpectedly contains a {other:?} event"),
        }
    }
    let bytes = writer.finish().expect("v3 writer finishes");
    let (_, v3_events) = read_cast(bytes.as_slice()).expect("v3 output re-reads");

    assert_eq!(v3_events.len(), events.len());
    for (original, round_tripped) in events.iter().zip(&v3_events) {
        assert_eq!(round_tripped.time_ms, original.time_ms);
        assert_eq!(round_tripped.data, original.data);
    }
}

#[test]
fn golden_v2_header_is_flat_and_v3_header_nests_term() {
    let (header, _) = read_fixture();

    let v2 = CastWriter::new(Vec::new(), &header, CastVersion::V2)
        .expect("v2 writer opens")
        .finish()
        .expect("v2 writer finishes");
    let v2_head = String::from_utf8(v2).expect("utf-8");
    assert!(v2_head.contains("\"width\":140"), "{v2_head}");
    assert!(v2_head.contains("\"height\":40"), "{v2_head}");

    let v3 = CastWriter::new(Vec::new(), &header, CastVersion::V3)
        .expect("v3 writer opens")
        .finish()
        .expect("v3 writer finishes");
    let v3_head = String::from_utf8(v3).expect("utf-8");
    assert!(
        v3_head.contains("\"term\":{\"cols\":140,\"rows\":40}"),
        "{v3_head}"
    );
}

// ---------------------------------------------------------------------------
// Whole-pipeline coverage: the same real recording, rendered.
//
// The unit tests exercise the replayer, the rasterizer, and the two containers
// in isolation against hand-built inputs. These drive all of it end to end on
// 140x40 of genuine terminal output — 67 output events across four minutes of
// a real session — which is the only place a stage-boundary mistake (a dirty
// band that outlives a resize, a palette that misses a color the rasterizer
// emits, a frame count pass 2 disagrees with) actually shows up.
//
// The fixture is deliberately plain: it carries no SGR sequences at all, so
// the palette is exactly two entries. That is the *lossless* path, and it is
// what nearly every real recording of a shell session looks like.

/// 8 MiB, the CLI's default `--max-bytes`.
#[cfg(feature = "render")]
const CAP: u64 = 8 * 1024 * 1024;

/// Render the fixture once per format and cache the artifact: the GIF
/// test, the APNG test, and the cross-container comparison all consume
/// the same two renders instead of re-rendering 140x40 x 4 minutes from
/// scratch per test (the renders dominate this binary's wall time when
/// the tests share a process, e.g. under `cargo test`).
#[cfg(feature = "render")]
fn render_fixture(format: OutputFormat) -> &'static (Vec<u8>, RenderStats) {
    use std::sync::OnceLock;
    static GIF: OnceLock<(Vec<u8>, RenderStats)> = OnceLock::new();
    static APNG: OnceLock<(Vec<u8>, RenderStats)> = OnceLock::new();
    let cell = match format {
        OutputFormat::Gif => &GIF,
        OutputFormat::Apng => &APNG,
        OutputFormat::Cast => panic!("these tests only render image containers"),
    };
    cell.get_or_init(|| {
        let (header, events) = read_fixture();
        let mut sink: Vec<u8> = Vec::new();
        let opts = RenderOptions {
            max_bytes: CAP,
            ..RenderOptions::default()
        };
        let stats = render_cast(&header, &events, &mut sink, format, &opts)
            .expect("the fixture renders end to end");
        (sink, stats)
    })
}

// One test covers both containers plus their agreement. Three separate
// tests would each re-render under nextest's process-per-test model
// (the OnceLock cache in `render_fixture` cannot cross processes), turning
// two ~2s renders into four.
#[cfg(feature = "render")]
#[test]
fn golden_cast_renders_gif_and_apng_and_frame_counts_agree() {
    let (bytes, stats) = render_fixture(OutputFormat::Gif);

    assert_eq!(bytes.get(..6), Some(&b"GIF89a"[..]), "missing signature");
    assert_eq!(bytes.last(), Some(&0x3b), "missing trailer");
    assert!(
        bytes.windows(11).any(|window| window == b"NETSCAPE2.0"),
        "a GIF without the loop extension plays once and stops"
    );
    assert!(stats.frames > 0, "no frames emitted");
    assert!(!stats.truncated, "8 MiB should be plenty for the fixture");
    assert_eq!(stats.bytes, bytes.len() as u64);
    // The fixture is four minutes of a real session; anything under a few
    // kilobytes means frames are being silently dropped.
    assert!(
        bytes.len() > 4096,
        "suspiciously small GIF: {}",
        bytes.len()
    );
    assert_eq!(
        stats.colors, 2,
        "an unstyled recording resolves to exactly foreground and background"
    );

    let gif_stats = stats;
    let (bytes, stats) = render_fixture(OutputFormat::Apng);

    assert_eq!(
        bytes.get(..8),
        Some(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..]),
        "missing signature"
    );
    assert!(
        bytes.windows(4).any(|window| window == b"acTL"),
        "an APNG without acTL decodes as a still image"
    );
    assert!(
        bytes.windows(4).any(|window| window == b"IEND"),
        "container was not closed"
    );
    assert!(stats.frames > 0, "no frames emitted");
    assert!(!stats.truncated);
    assert_eq!(stats.bytes, bytes.len() as u64);
    assert!(
        bytes.len() > 4096,
        "suspiciously small APNG: {}",
        bytes.len()
    );

    // The two backends share the sampling walk, so a divergence here means
    // something format-specific leaked into the driver.
    assert_eq!(gif_stats.frames, stats.frames);
    assert_eq!(gif_stats.duration_ms, stats.duration_ms);
}
