use super::*;
use crate::{Histogram, Unit};

static TRACED: Histogram = Histogram::new();
static TABLE: &[Metric] = &[Metric::histogram("test.traced", Unit::Micros, &TRACED)];

/// Captures are process-global; one test drives the whole lifecycle so a
/// parallel test cannot disarm another's capture.
#[test]
fn capture_names_spans_from_tables_and_respects_the_budget() {
    TRACED.record_duration(Duration::from_millis(1));
    assert!(stop().is_none(), "nothing armed yet");
    assert!(start(4));
    assert!(!start(4), "a second start while armed is refused");
    {
        let _t = TRACED.timer();
        std::thread::sleep(Duration::from_millis(2));
    }
    {
        let _s = span("test.explicit");
    }
    instant("test.mark");
    counter("test.rss", 42.0);
    TRACED.record_duration(Duration::from_micros(5));
    let capture = stop().unwrap_or_else(|| panic!("armed capture"));
    assert_eq!(capture.event_count(), 4);
    assert_eq!(capture.dropped(), 1);
    TRACED.record_duration(Duration::from_millis(1));
    assert!(stop().is_none(), "disarmed after stop");

    let doc: Value = serde_json::from_str(&capture.to_chrome_json("phux test", &[TABLE]))
        .unwrap_or_else(|e| panic!("{e}"));
    let events = doc["traceEvents"]
        .as_array()
        .unwrap_or_else(|| panic!("events"));
    let span = events
        .iter()
        .find(|e| e["name"] == "test.traced")
        .unwrap_or_else(|| panic!("named histogram span in {events:?}"));
    assert_eq!(span["ph"], "X");
    assert_eq!(span["cat"], "test");
    assert!(span["dur"].as_f64().unwrap_or_default() >= 2000.0);
    assert!(
        events
            .iter()
            .any(|e| e["name"] == "test.explicit" && e["ph"] == "X")
    );
    assert!(
        events
            .iter()
            .any(|e| e["name"] == "test.mark" && e["ph"] == "i")
    );
    assert!(
        events
            .iter()
            .any(|e| e["name"] == "test.rss" && e["args"]["value"] == 42.0)
    );
    assert!(
        events
            .iter()
            .any(|e| e["name"] == "process_name" && e["args"]["name"] == "phux test")
    );
    assert_eq!(doc["otherData"]["dropped"], 1);

    let merged: Value = serde_json::from_str(&merge_chrome_json([
        capture.to_chrome_json("a", &[TABLE]).as_str(),
        "not json",
        capture.to_chrome_json("b", &[TABLE]).as_str(),
    ]))
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        merged["traceEvents"].as_array().map(Vec::len),
        Some(events.len() * 2)
    );
    assert_eq!(
        merged["otherData"]["processes"].as_array().map(Vec::len),
        Some(2)
    );

    // A deadline for one capture does not end the next.
    assert!(start(4));
    let armed_generation = generation();
    assert!(stop_generation(armed_generation + 1).is_none());
    assert!(armed());
    assert!(stop_generation(armed_generation).is_some());
    assert!(!armed());
    stale_guards_do_not_cross_captures();
    admitted_writers_do_not_cross_captures();
}

fn stale_guards_do_not_cross_captures() {
    // A guard from a stopped capture must not leak into the next one.
    assert!(start(4));
    let stale_span = super::span("test.stale");
    assert!(stop().is_some());
    assert!(start(4));
    drop(stale_span);
    let capture = stop().unwrap_or_else(|| panic!("armed capture"));
    assert_eq!(capture.event_count(), 0);
}

fn admitted_writers_do_not_cross_captures() {
    // Pause an admitted writer across stop/restart. A zero-budget next
    // capture must receive neither its event nor its drop accounting.
    assert!(start(4));
    let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let admission = STATE.load(Acquire);
        admitted_tx.send(()).unwrap_or_else(|e| panic!("{e}"));
        resume_rx.recv().unwrap_or_else(|e| panic!("{e}"));
        push_generation(Key::Named("test.delayed"), Kind::Instant, admission);
    });
    admitted_rx.recv().unwrap_or_else(|e| panic!("{e}"));
    assert!(stop().is_some());
    assert!(start(0));
    resume_tx.send(()).unwrap_or_else(|e| panic!("{e}"));
    writer.join().unwrap_or_else(|_| panic!("writer"));
    let capture = stop().unwrap_or_else(|| panic!("armed capture"));
    assert_eq!(capture.event_count(), 0);
    assert_eq!(capture.dropped(), 0);
}

#[test]
fn retrospective_span_keeps_its_end_before_the_clock_epoch() {
    let capture = Capture {
        anchor_ns: 0,
        anchor_unix_us: 1_000_000,
        duration: Duration::from_millis(5),
        dropped: 0,
        threads: vec![ThreadEvents {
            tid: 1,
            name: "test".to_owned(),
            events: vec![Event {
                key: Key::Named("test.retrospective"),
                kind: Kind::Span { dur_ns: 20_000_000 },
                end_ns: 5_000_000,
            }],
        }],
    };
    let doc: Value = serde_json::from_str(&capture.to_chrome_json("test", &[]))
        .unwrap_or_else(|e| panic!("{e}"));
    let event = &doc["traceEvents"][2];
    assert_eq!(event["ts"].as_f64(), Some(985_000.0));
    assert_eq!(event["dur"].as_f64(), Some(20_000.0));
}

#[test]
fn trace_files_with_the_same_timestamp_never_overwrite() {
    use std::io::Write as _;
    let dir = std::env::temp_dir().join(format!("phux-trace-exclusive-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let (first, mut a) =
        create_trace_file_at(&dir, "server", 123).unwrap_or_else(|e| panic!("{e}"));
    a.write_all(b"first").unwrap_or_else(|e| panic!("{e}"));
    let (second, mut b) =
        create_trace_file_at(&dir, "server", 123).unwrap_or_else(|e| panic!("{e}"));
    b.write_all(b"second").unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(first, second);
    assert_eq!(std::fs::read(&first).unwrap_or_default(), b"first");
    assert_eq!(std::fs::read(&second).unwrap_or_default(), b"second");
    let _ = std::fs::remove_dir_all(dir);
}
