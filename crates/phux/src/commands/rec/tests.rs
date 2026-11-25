use super::capture::{composite_cells, into_timeline};
use super::output::{RecOutcome, human_bytes, parse_resize, rec_json};
use super::{cast_version, checked_speed, idle_limit, mark::mark_json};
use phux_client::record::HeadlessRecording;
use phux_record::cast::{CastEvent, CastVersion, EventCode};
use phux_record::render::OutputFormat;

/// `phux rec --json` pins `schema_version` 1 plus the documented result
/// fields.
#[test]
fn rec_json_pins_the_contract_shape() {
    let outcome = RecOutcome {
        path: "demo.gif".into(),
        format: OutputFormat::Gif,
        bytes: 188_742,
        duration_ms: 42_130,
        frames: 211,
        cols: 120,
        rows: 34,
        truncated: false,
    };
    let doc = rec_json(&outcome);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["path"], "demo.gif");
    assert_eq!(doc["format"], "gif");
    assert_eq!(doc["bytes"], 188_742);
    assert_eq!(doc["duration_ms"], 42_130);
    assert_eq!(doc["frames"], 211);
    assert_eq!(doc["cols"], 120);
    assert_eq!(doc["rows"], 34);
    assert_eq!(doc["truncated"], false);
    assert_eq!(doc.as_object().map(serde_json::Map::len), Some(9));
}

/// A capture with `cols`x`rows` and one output event.
fn recording(cols: u16, rows: u16) -> HeadlessRecording {
    HeadlessRecording {
        cols,
        rows,
        started_unix: 1_700_000_000,
        events: vec![CastEvent {
            time_ms: 0,
            code: EventCode::Output,
            data: "hi".to_owned(),
        }],
    }
}

#[test]
fn zero_dimensions_are_refused_rather_than_written() {
    // A zero grid means the priming TERMINAL_SNAPSHOT never landed, so
    // the capture is missing the screen it opens on. A 0x0 artifact that
    // exits 0 is worse than a named failure.
    for dims in [(0, 24), (80, 0), (0, 0)] {
        let refused = into_timeline(recording(dims.0, dims.1), None);
        assert!(
            refused
                .as_ref()
                .is_err_and(|message| message.contains("opening snapshot")),
            "{dims:?} should be refused with a diagnosable message, got {refused:?}"
        );
    }
}

#[test]
fn idle_limit_zero_disables_the_clamp() {
    assert_eq!(idle_limit(2.0), Some(2.0));
    assert_eq!(idle_limit(0.0), None);
    assert_eq!(idle_limit(-1.0), None);
    assert_eq!(idle_limit(f64::NAN), None);
}

#[test]
fn cast_version_defaults_to_v2() {
    assert_eq!(cast_version(2), CastVersion::V2);
    assert_eq!(cast_version(3), CastVersion::V3);
    // Out of clap's range, so unreachable in practice — but the fallback
    // must be the interoperable version, never the newer one.
    assert_eq!(cast_version(9), CastVersion::V2);
}

#[test]
fn resize_payload_parses_colsxrows() {
    assert_eq!(parse_resize("100x30"), Some((100, 30)));
    assert_eq!(parse_resize(" 80x24 "), Some((80, 24)));
    assert_eq!(parse_resize("80"), None);
    assert_eq!(parse_resize("axb"), None);
}

#[test]
fn human_bytes_switches_unit_at_the_binary_prefixes() {
    assert_eq!(human_bytes(512), "512 B");
    assert_eq!(human_bytes(1024), "1.0 KiB");
    assert_eq!(human_bytes(184_320), "180.0 KiB");
    assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MiB");
}

#[test]
fn mark_json_pins_label_and_recording_count() {
    let doc = mark_json("intro", 2);
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["label"], "intro");
    assert_eq!(doc["recordings"], 2);
    assert_eq!(doc.as_object().map(serde_json::Map::len), Some(3));
}

#[test]
fn speed_marked_accepts_an_integer_factor_above_one() {
    assert_eq!(checked_speed(None, false).expect("absent"), None);
    assert_eq!(checked_speed(Some(2.0), false).expect("2x"), Some(2.0));
    assert_eq!(checked_speed(Some(100.0), false).expect("cap"), Some(100.0));
    assert!(checked_speed(Some(1.0), false).is_err());
    assert!(checked_speed(Some(0.5), false).is_err());
    assert!(checked_speed(Some(101.0), false).is_err());
    assert!(checked_speed(Some(f64::NAN), false).is_err());
}

#[test]
fn composite_size_defaults_to_1080p_cells() {
    assert_eq!(composite_cells(None, false).expect("default"), (240, 67));
    assert_eq!(
        composite_cells(Some("1920x1080"), false).expect("explicit"),
        (240, 67)
    );
    assert!(composite_cells(Some("0x1080"), false).is_err());
    assert!(composite_cells(Some("wide"), false).is_err());
}

#[test]
fn cast_reexport_rejects_unknown_exit_instead_of_fabricating_zero() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (header, _) = into_timeline(recording(80, 24), None).expect("header");
    for version in [CastVersion::V2, CastVersion::V3] {
        for status in ["unknown", "", "2147483648", "{\"status\":null}"] {
            let path = dir.path().join("invalid.cast");
            let events = [CastEvent {
                time_ms: 12,
                code: EventCode::Exit,
                data: status.into(),
            }];
            let error = super::write_cast_file(&path, &header, &events, version)
                .expect_err("unsupported status");
            assert!(error.to_string().contains("unsupported exit event"));
        }
    }
}

#[test]
fn cast_reexport_keeps_normal_exit_status_and_never_writes_input() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (header, _) = into_timeline(recording(80, 24), None).expect("header");
    for status in [0, 7, -1] {
        let path = dir.path().join("exit.cast");
        let events = [
            CastEvent {
                time_ms: 1,
                code: EventCode::Input,
                data: "password-must-not-appear".into(),
            },
            CastEvent {
                time_ms: 2,
                code: EventCode::Exit,
                data: status.to_string(),
            },
        ];
        super::write_cast_file(&path, &header, &events, CastVersion::V3).expect("write");
        let (_, actual) = super::read_cast_file(&path).expect("read");
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].code, EventCode::Exit);
        assert_eq!(actual[0].data, status.to_string());
        assert!(
            !std::fs::read_to_string(&path)
                .expect("file")
                .contains("password")
        );
    }
}
