#![allow(clippy::expect_used, reason = "tests")]

use super::*;

fn keys(specs: &[&str]) -> Vec<String> {
    specs.iter().map(|s| (*s).to_owned()).collect()
}

fn record(name: &str, kind: Option<&str>) -> AgentRecord {
    AgentRecord {
        name: name.to_owned(),
        kind: kind.map(str::to_owned),
        ..AgentRecord::default()
    }
}

/// Named keys, chords, and literal text pass; near-miss chord shapes
/// refuse the whole batch, naming the argument's position.
#[test]
fn key_specs_pass_or_refuse_the_whole_batch() {
    assert_eq!(
        validate_key_specs(&keys(&["Enter", "C-c", "M-x", "yes please", "Up"])),
        Ok(())
    );
    let err = validate_key_specs(&keys(&["approve", "Enter", "C-cc"]))
        .expect_err("`C-cc` must not be accepted as literal text");
    assert_eq!((err.index, err.spec.as_str()), (2, "C-cc"));
    for spec in ["C-", "M-", "c-esc", "C-\u{e9}", ""] {
        assert!(
            validate_key_specs(&keys(&[spec])).is_err(),
            "'{spec}' must be refused rather than typed literally"
        );
    }
}

/// A literal run not followed by `Enter` types one event per character, so
/// a long argument crosses the event cap and is refused whole.
#[test]
fn a_batch_past_the_event_cap_is_refused_whole_and_never_split() {
    let long = "x".repeat(phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS + 1);
    let events = phux_client::send_keys::events_for(&[long]);
    assert!(
        events.len() > phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS,
        "a literal run types one event per character: got {}",
        events.len()
    );
    match validate_batch(&events) {
        Err(Refusal::TooLarge {
            unit,
            limit,
            wire: true,
            ..
        }) => {
            assert_eq!(unit, "events");
            assert_eq!(limit, phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS);
        }
        other => panic!("an over-cap batch must be refused: {other:?}"),
    }

    // A literal run before `Enter` collapses to one paste plus one key.
    let submitted = phux_client::send_keys::events_for(&keys(&["yes please", "Enter"]));
    assert_eq!(submitted.len(), 2);
    assert_eq!(validate_batch(&submitted), Ok(()));
}

/// Names and kinds compare trimmed and case-insensitively; a different
/// occupant is a mismatch naming both.
#[test]
fn names_and_kinds_compare_trimmed_and_case_insensitively() {
    assert_eq!(
        identity_mismatch(&record("reviewer", Some("claude")), None, None),
        None
    );
    let mismatch = identity_mismatch(&record("builder", Some("codex")), Some("reviewer"), None)
        .expect("a different name must be refused");
    assert!(
        mismatch.contains("builder") && mismatch.contains("reviewer"),
        "{mismatch}"
    );
    assert_eq!(
        identity_mismatch(
            &record("Reviewer", Some("Claude")),
            Some(" reviewer "),
            None
        ),
        None
    );
    assert_eq!(
        identity_mismatch(&record("reviewer", Some("claude")), None, Some("CLAUDE")),
        None
    );
    assert!(
        identity_mismatch(&record("reviewer", None), None, Some("claude")).is_some(),
        "a record with no kind cannot satisfy --expect-kind"
    );
}
